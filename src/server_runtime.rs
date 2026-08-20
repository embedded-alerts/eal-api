use std::{env, sync::Arc};

use anyhow::Context;
use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, FromRequestParts, Path, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{
        HeaderMap, HeaderName, HeaderValue, Method, StatusCode,
        header::{AUTHORIZATION, CONTENT_TYPE, ORIGIN},
        request::Parts,
    },
    response::IntoResponse,
    routing::{get, post},
};
use eal_api::{
    alert_store,
    alerts::{AlertRule, CreateAlertRule},
    auth::{AuthConfig, AuthContext, ROLES_HEADER, SUBJECT_HEADER},
    error::HttpError,
    indexing, migrations, store, tenant, worker_auth,
};
use eal_interfaces::{
    CreateSourcePolicy, EmbeddingSearchRequest, EmbeddingSearchResponse, MatchCandidate,
    PageIngestRequest, PageRevision, SourcePolicy,
};
use futures_util::{SinkExt, StreamExt};
use sea_orm::{Database, DatabaseConnection};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    trace::TraceLayer,
};
use tracing::{info, warn};
use url::Url;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppEnvironment {
    Development,
    Test,
    Production,
}

impl AppEnvironment {
    fn parse(value: Option<&str>) -> Result<Self, String> {
        let normalized = value
            .unwrap_or("development")
            .trim()
            .to_ascii_lowercase();
        match normalized.as_str() {
            "" | "dev" | "development" => Ok(Self::Development),
            "test" => Ok(Self::Test),
            "prod" | "production" => Ok(Self::Production),
            other => Err(format!("unsupported APP_ENV value: {other}")),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Development => "development",
            Self::Test => "test",
            Self::Production => "production",
        }
    }
}

fn validate_startup_policy(
    environment: AppEnvironment,
    database_connected: bool,
    schema_ready: bool,
    verified_jwt_enabled: bool,
    ingest_auth_configured: bool,
) -> Result<(), &'static str> {
    if environment == AppEnvironment::Production && !database_connected {
        return Err("production startup requires durable PostgreSQL storage");
    }
    if environment == AppEnvironment::Production && !schema_ready {
        return Err("production startup requires the durable Embedded Alerts schema");
    }
    if environment == AppEnvironment::Production && !verified_jwt_enabled {
        return Err("production startup requires verified JWT authentication");
    }
    if environment == AppEnvironment::Production && !ingest_auth_configured {
        return Err("production startup requires authenticated crawler ingestion");
    }
    Ok(())
}

#[derive(Clone)]
struct AppState {
    environment: AppEnvironment,
    db: Option<DatabaseConnection>,
    schema_ready: bool,
    ingest_auth_configured: bool,
    events: broadcast::Sender<TenantEvent>,
    auth: Arc<AuthConfig>,
    allowed_origins: Arc<Vec<HeaderValue>>,
    supabase_url: Option<String>,
}

#[derive(Debug, Clone)]
struct TenantEvent {
    tenant_id: Uuid,
    audience: EventAudience,
    payload: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EventAudience {
    TenantAdministrators,
    SubjectOrTenantAdministrators(String),
}

struct Authenticated(AuthContext);
struct AuthorizedWebSocketOrigin;

impl FromRequestParts<AppState> for Authenticated {
    type Rejection = HttpError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        state.auth.authenticate(&parts.headers).map(Self)
    }
}

impl FromRequestParts<AppState> for AuthorizedWebSocketOrigin {
    type Rejection = HttpError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        authorize_websocket_origin(&parts.headers, state.allowed_origins.as_ref())?;
        Ok(Self)
    }
}

#[derive(Debug, Deserialize)]
struct EvaluateMatchesRequest {
    alert_rule_id: Uuid,
    threshold: f64,
    search: EmbeddingSearchRequest,
}

#[derive(Debug, Serialize)]
struct Health {
    service: &'static str,
    status: &'static str,
    environment: &'static str,
    alert_storage_mode: &'static str,
    indexing_storage_mode: &'static str,
    production_ready: bool,
    database_connected: bool,
    supabase_configured: bool,
    tenant_context: &'static str,
    websocket_authorization: &'static str,
    schema_ready: bool,
    ingest_auth_configured: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let app_env = env::var("APP_ENV").ok();
    let environment = AppEnvironment::parse(app_env.as_deref()).map_err(anyhow::Error::msg)?;
    let allow_insecure_tenant_header = env_flag("ALLOW_INSECURE_TENANT_HEADER");
    let auth = Arc::new(
        AuthConfig::from_env(
            environment == AppEnvironment::Production,
            allow_insecure_tenant_header,
        )
        .map_err(anyhow::Error::msg)?,
    );

    let db = match env::var("DATABASE_URL") {
        Ok(url) if !url.trim().is_empty() => {
            Some(Database::connect(url).await.context("connect database")?)
        }
        _ => None,
    };
    if env_flag("MIGRATE_ON_STARTUP") {
        let database = db
            .as_ref()
            .context("MIGRATE_ON_STARTUP requires DATABASE_URL")?;
        migrations::migrate_all(database)
            .await
            .context("apply Embedded Alerts migrations")?;
    }
    let schema_ready = match db.as_ref() {
        Some(database) => migrations::schema_ready(database)
            .await
            .context("verify durable Embedded Alerts schema")?,
        None => false,
    };
    let ingest_auth_configured = worker_auth::is_configured();
    validate_startup_policy(
        environment,
        db.is_some(),
        schema_ready,
        auth.is_jwt(),
        ingest_auth_configured,
    )
    .map_err(anyhow::Error::msg)?;

    let allowed_origins = Arc::new(configured_origins(environment)?);
    let (events, _) = broadcast::channel(512);
    let state = AppState {
        environment,
        db,
        schema_ready,
        ingest_auth_configured,
        events,
        auth,
        allowed_origins,
        supabase_url: env::var("SUPABASE_URL").ok(),
    };

    if !state.auth.is_jwt()
        || state.db.is_none()
        || !state.schema_ready
        || !state.ingest_auth_configured
    {
        warn!(
            environment = state.environment.as_str(),
            database_connected = state.db.is_some(),
            schema_ready = state.schema_ready,
            ingest_auth_configured = state.ingest_auth_configured,
            auth_mode = state.auth.mode_name(),
            "Embedded Alerts is running in a non-production development posture"
        );
    } else {
        info!(
            environment = state.environment.as_str(),
            auth_mode = state.auth.mode_name(),
            "durable alert-rule storage and verified JWT authorization are enabled"
        );
    }

    let app = build_router(state);
    let host = env::var("HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port = env::var("PORT").unwrap_or_else(|_| "8080".into());
    let listener = tokio::net::TcpListener::bind(format!("{host}:{port}")).await?;
    info!(address = %listener.local_addr()?, "Embedded Alerts API listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

fn build_router(state: AppState) -> Router {
    let cors = cors_layer(state.allowed_origins.as_ref());
    Router::new()
        .route("/healthz", get(health))
        .route("/v1/alerts", get(list_records).post(create_record))
        .route("/v1/alerts/{id}", get(get_record))
        .route("/v1/sources", get(list_sources).post(create_source))
        .route("/v1/sources/{source_id}", get(get_source))
        .route("/v1/sources/{source_id}/pages", post(ingest_page))
        .route("/v1/embeddings/search", post(search_embeddings))
        .route("/v1/matches/evaluate", post(evaluate_matches))
        .route("/v1/ws", get(ws_upgrade))
        .layer(DefaultBodyLimit::max(6 * 1024 * 1024))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn health(State(state): State<AppState>) -> Json<Health> {
    let production_ready = state.db.is_some()
        && state.schema_ready
        && state.auth.is_jwt()
        && state.ingest_auth_configured;
    Json(Health {
        service: "eal-api",
        status: if production_ready { "ok" } else { "degraded" },
        environment: state.environment.as_str(),
        alert_storage_mode: if state.db.is_some() && state.schema_ready {
            "postgresql_immutable_revisions"
        } else {
            "unavailable"
        },
        indexing_storage_mode: if state.db.is_some() && state.schema_ready {
            "postgresql_pgvector"
        } else {
            "unavailable"
        },
        production_ready,
        database_connected: state.db.is_some(),
        supabase_configured: state.supabase_url.is_some(),
        tenant_context: state.auth.mode_name(),
        websocket_authorization: "same_verified_identity_plus_explicit_origin",
        schema_ready: state.schema_ready,
        ingest_auth_configured: state.ingest_auth_configured,
    })
}

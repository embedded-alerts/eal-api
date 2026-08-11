mod semantic;

use std::{collections::HashMap, env, sync::Arc};

use anyhow::Context;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use futures_util::{SinkExt, StreamExt};
use sea_orm::{Database, DatabaseConnection};
use semantic::{
    CreateSourceDomain, IngestPageRequest, SemanticError, SemanticSearchRequest,
    SemanticService,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing::{info, warn};
use uuid::Uuid;

const TENANT_HEADER: &str = "x-eal-tenant-id";
const OPENAPI_DOCUMENT: &str = include_str!("../openapi/eal-api.json");

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

fn validate_startup_policy(environment: AppEnvironment) -> Result<(), &'static str> {
    if environment == AppEnvironment::Production {
        return Err(
            "production startup blocked: alert rules and semantic index state are still process-local; complete DEN-3459 and the durable DEN-3461 repository before deployment",
        );
    }
    Ok(())
}

#[derive(Clone)]
struct AppState {
    environment: AppEnvironment,
    db: Option<DatabaseConnection>,
    records: Arc<RwLock<HashMap<Uuid, AlertRule>>>,
    events: broadcast::Sender<String>,
    supabase_url: Option<String>,
    semantic: SemanticService,
    default_tenant_id: Uuid,
    openapi: Arc<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AlertRule {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub name: String,
    pub query_text: String,
    pub embedding_model: String,
    pub similarity_threshold: f32,
    pub source_filters: Vec<String>,
    pub delivery_channels: Vec<String>,
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
struct CreateAlertRule {
    pub name: String,
    pub query_text: String,
    pub embedding_model: String,
    pub similarity_threshold: f32,
    pub source_filters: Vec<String>,
    pub delivery_channels: Vec<String>,
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
struct Health {
    service: &'static str,
    status: &'static str,
    environment: &'static str,
    storage_mode: &'static str,
    semantic_index_mode: &'static str,
    production_ready: bool,
    database_connected: bool,
    supabase_configured: bool,
    embedding_mode: &'static str,
    embedding_model: String,
}

#[derive(Debug, Serialize)]
struct ApiErrorBody {
    error: ApiErrorDetail,
}

#[derive(Debug, Serialize)]
struct ApiErrorDetail {
    code: String,
    message: String,
}

struct ApiResponseError(SemanticError);

impl From<SemanticError> for ApiResponseError {
    fn from(error: SemanticError) -> Self {
        Self(error)
    }
}

impl IntoResponse for ApiResponseError {
    fn into_response(self) -> Response {
        let status = self.0.status_code();
        (
            status,
            Json(ApiErrorBody {
                error: ApiErrorDetail {
                    code: self.0.code().to_owned(),
                    message: self.0.message().to_owned(),
                },
            }),
        )
            .into_response()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let app_env = env::var("APP_ENV").ok();
    let environment = AppEnvironment::parse(app_env.as_deref()).map_err(anyhow::Error::msg)?;
    validate_startup_policy(environment).map_err(anyhow::Error::msg)?;

    let db = match env::var("DATABASE_URL") {
        Ok(url) if !url.trim().is_empty() => {
            Some(Database::connect(url).await.context("connect database")?)
        }
        _ => None,
    };
    let semantic = SemanticService::from_env().map_err(anyhow::Error::msg)?;
    let default_tenant_id = env::var("DEFAULT_TENANT_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| Uuid::parse_str(value.trim()))
        .transpose()
        .context("parse DEFAULT_TENANT_ID")?
        .unwrap_or_else(|| Uuid::from_u128(1));
    let openapi: serde_json::Value =
        serde_json::from_str(OPENAPI_DOCUMENT).context("parse bundled OpenAPI document")?;

    let (events, _) = broadcast::channel(512);
    let state = AppState {
        environment,
        db,
        records: Arc::new(RwLock::new(HashMap::new())),
        events,
        supabase_url: env::var("SUPABASE_URL").ok(),
        semantic,
        default_tenant_id,
        openapi: Arc::new(openapi),
    };

    warn!(
        environment = state.environment.as_str(),
        database_connected = state.db.is_some(),
        embedding_mode = state.semantic.embedding_mode(),
        "Embedded Alerts is using process-local rule and semantic index state; production startup is disabled"
    );

    let app = Router::new()
        .route("/healthz", get(health))
        .route("/openapi.json", get(openapi))
        .route("/v1/alerts", get(list_records).post(create_record))
        .route("/v1/alerts/{id}", get(get_record))
        .route("/v1/sources", get(list_sources).post(create_source))
        .route("/v1/sources/{id}/scan", post(scan_source))
        .route("/v1/sources/{id}/ingest", post(ingest_page))
        .route("/v1/pages", get(list_pages))
        .route("/v1/embeddings/search", post(search_embeddings))
        .route("/v1/matches", get(list_matches))
        .route("/v1/ws", get(ws_upgrade))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let host = env::var("HOST").unwrap_or_else(|_| "0.0.0.0".into());
    let port = env::var("PORT").unwrap_or_else(|_| "8080".into());
    let listener = tokio::net::TcpListener::bind(format!("{host}:{port}")).await?;
    info!(address = %listener.local_addr()?, "Embedded Alerts API listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn health(State(state): State<AppState>) -> Json<Health> {
    Json(Health {
        service: "eal-api",
        status: "degraded",
        environment: state.environment.as_str(),
        storage_mode: "process_local_memory",
        semantic_index_mode: "allowlisted_public_pages",
        production_ready: false,
        database_connected: state.db.is_some(),
        supabase_configured: state.supabase_url.is_some(),
        embedding_mode: state.semantic.embedding_mode(),
        embedding_model: state.semantic.embedding_model().fingerprint(),
    })
}

async fn openapi(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json((*state.openapi).clone())
}


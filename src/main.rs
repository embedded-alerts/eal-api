mod error;
mod indexing;
mod store;
mod tenant;
mod worker_auth;

use std::{collections::HashMap, env, sync::Arc};

use anyhow::Context;
use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Path, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{
        HeaderMap, HeaderName, HeaderValue, Method, StatusCode,
        header::{AUTHORIZATION, CONTENT_TYPE},
    },
    response::IntoResponse,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use eal_interfaces::{
    CreateSourcePolicy, EmbeddingSearchRequest, EmbeddingSearchResponse, MatchCandidate,
    PageIngestRequest, PageRevision, SourcePolicy,
};
use futures_util::{SinkExt, StreamExt};
use sea_orm::{Database, DatabaseConnection};
use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, broadcast};
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    trace::TraceLayer,
};
use tracing::{info, warn};
use uuid::Uuid;

use crate::{error::HttpError, tenant::TenantId};

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
    allow_insecure_tenant_header: bool,
) -> Result<(), &'static str> {
    if environment == AppEnvironment::Production {
        return Err(
            "production startup blocked: alert rules are still stored only in process memory; implement the durable tenant repository in DEN-3459 before deployment",
        );
    }
    if !allow_insecure_tenant_header {
        return Err(
            "startup blocked: X-Eal-Tenant-Id is not authentication; set ALLOW_INSECURE_TENANT_HEADER=true only in an isolated development or test environment",
        );
    }
    Ok(())
}

#[derive(Clone)]
struct AppState {
    environment: AppEnvironment,
    db: Option<DatabaseConnection>,
    records: Arc<RwLock<HashMap<Uuid, AlertRule>>>,
    events: broadcast::Sender<TenantEvent>,
    supabase_url: Option<String>,
}

#[derive(Debug, Clone)]
struct TenantEvent {
    tenant_id: Uuid,
    payload: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AlertRule {
    pub id: Uuid,
    pub tenant_id: Uuid,
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
    validate_startup_policy(environment, allow_insecure_tenant_header)
        .map_err(anyhow::Error::msg)?;

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
        store::migrate(database)
            .await
            .context("apply domain-scoped indexing migration")?;
    }

    let (events, _) = broadcast::channel(512);
    let state = AppState {
        environment,
        db,
        records: Arc::new(RwLock::new(HashMap::new())),
        events,
        supabase_url: env::var("SUPABASE_URL").ok(),
    };

    warn!(
        environment = state.environment.as_str(),
        database_connected = state.db.is_some(),
        insecure_tenant_header_enabled = allow_insecure_tenant_header,
        "alert rules remain process-local; production startup is disabled"
    );

    let app = Router::new()
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
        .layer(cors_layer()?)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let host = env::var("HOST").unwrap_or_else(|_| "127.0.0.1".into());
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
        alert_storage_mode: "process_local_memory",
        indexing_storage_mode: if state.db.is_some() {
            "postgresql_pgvector"
        } else {
            "unavailable"
        },
        production_ready: false,
        database_connected: state.db.is_some(),
        supabase_configured: state.supabase_url.is_some(),
        tenant_context: "explicit_insecure_header_until_shared_auth",
    })
}

async fn list_records(
    TenantId(tenant_id): TenantId,
    State(state): State<AppState>,
) -> Json<Vec<AlertRule>> {
    Json(
        state
            .records
            .read()
            .await
            .values()
            .filter(|record| record.tenant_id == tenant_id)
            .cloned()
            .collect(),
    )
}

async fn get_record(
    TenantId(tenant_id): TenantId,
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    match state
        .records
        .read()
        .await
        .get(&id)
        .filter(|record| record.tenant_id == tenant_id)
        .cloned()
    {
        Some(record) => (StatusCode::OK, Json(record)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn create_record(
    TenantId(tenant_id): TenantId,
    State(state): State<AppState>,
    Json(input): Json<CreateAlertRule>,
) -> Result<(StatusCode, Json<AlertRule>), HttpError> {
    if input.name.trim().is_empty() || input.name.len() > 256 {
        return Err(HttpError::validation(
            "name must contain between 1 and 256 bytes",
        ));
    }
    if input.query_text.trim().is_empty() {
        return Err(HttpError::validation("query_text must not be empty"));
    }
    if !(0.0..=1.0).contains(&input.similarity_threshold) {
        return Err(HttpError::validation(
            "similarity_threshold must be between 0 and 1",
        ));
    }

    let now = Utc::now();
    let record = AlertRule {
        id: Uuid::new_v4(),
        tenant_id,
        created_at: now,
        updated_at: now,
        name: input.name,
        query_text: input.query_text,
        embedding_model: input.embedding_model,
        similarity_threshold: input.similarity_threshold,
        source_filters: input.source_filters,
        delivery_channels: input.delivery_channels,
        enabled: input.enabled,
    };
    state
        .records
        .write()
        .await
        .insert(record.id, record.clone());
    publish_event(&state, tenant_id, "alert_rule.created", &record)?;
    Ok((StatusCode::CREATED, Json(record)))
}

async fn list_sources(
    TenantId(tenant_id): TenantId,
    State(state): State<AppState>,
) -> Result<Json<Vec<SourcePolicy>>, HttpError> {
    let database = require_database(&state)?;
    Ok(Json(store::list_sources(database, tenant_id).await?))
}

async fn get_source(
    TenantId(tenant_id): TenantId,
    Path(source_id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<SourcePolicy>, HttpError> {
    let database = require_database(&state)?;
    store::get_source(database, tenant_id, source_id)
        .await?
        .map(Json)
        .ok_or_else(|| HttpError::not_found("source policy was not found"))
}

async fn create_source(
    TenantId(tenant_id): TenantId,
    State(state): State<AppState>,
    Json(input): Json<CreateSourcePolicy>,
) -> Result<(StatusCode, Json<SourcePolicy>), HttpError> {
    input.validate().map_err(HttpError::validation)?;
    let canonical_root =
        indexing::canonicalize_source_root(&input).map_err(HttpError::validation)?;
    let database = require_database(&state)?;
    let source = store::create_source(database, tenant_id, &input, &canonical_root).await?;
    publish_event(&state, tenant_id, "source.created", &source)?;
    Ok((StatusCode::CREATED, Json(source)))
}

async fn ingest_page(
    TenantId(tenant_id): TenantId,
    Path(source_id): Path<Uuid>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<PageIngestRequest>,
) -> Result<(StatusCode, Json<PageRevision>), HttpError> {
    worker_auth::authorize(&headers)?;
    input.validate().map_err(HttpError::validation)?;
    if input.source_id != source_id {
        return Err(HttpError::bad_request(
            "body source_id must match the source_id route parameter",
        ));
    }

    let database = require_database(&state)?;
    let source = store::get_source(database, tenant_id, source_id)
        .await?
        .ok_or_else(|| HttpError::not_found("source policy was not found"))?;
    if !source.enabled {
        return Err(HttpError::validation("source policy is disabled"));
    }
    let canonical_original =
        indexing::canonicalize_for_source(&source, &input.url).map_err(HttpError::validation)?;
    let canonical_final = indexing::canonicalize_for_source(&source, &input.final_url)
        .map_err(HttpError::validation)?;
    let revision = store::ingest_page(
        database,
        tenant_id,
        source_id,
        &input,
        &canonical_original,
        &canonical_final,
    )
    .await?;
    let status = if revision.changed {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    publish_event(&state, tenant_id, "page.revision_indexed", &revision)?;
    Ok((status, Json(revision)))
}

async fn search_embeddings(
    TenantId(tenant_id): TenantId,
    State(state): State<AppState>,
    Json(input): Json<EmbeddingSearchRequest>,
) -> Result<Json<EmbeddingSearchResponse>, HttpError> {
    input.validate().map_err(HttpError::validation)?;
    let database = require_database(&state)?;
    Ok(Json(
        store::search_embeddings(database, tenant_id, &input)
            .await?
            .response,
    ))
}

async fn evaluate_matches(
    TenantId(tenant_id): TenantId,
    State(state): State<AppState>,
    Json(input): Json<EvaluateMatchesRequest>,
) -> Result<Json<Vec<MatchCandidate>>, HttpError> {
    if !(0.0..=1.0).contains(&input.threshold) {
        return Err(HttpError::validation(
            "threshold must be between 0 and 1",
        ));
    }
    input.search.validate().map_err(HttpError::validation)?;
    let database = require_database(&state)?;
    let candidates = store::evaluate_matches(
        database,
        tenant_id,
        input.alert_rule_id,
        input.threshold,
        input.search,
    )
    .await?;
    for candidate in &candidates {
        publish_event(&state, tenant_id, "match.candidate", candidate)?;
    }
    Ok(Json(candidates))
}

async fn ws_upgrade(
    TenantId(tenant_id): TenantId,
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| websocket(socket, state, tenant_id))
}

async fn websocket(socket: WebSocket, state: AppState, tenant_id: Uuid) {
    let (mut sender, mut receiver) = socket.split();
    let mut events = state.events.subscribe();
    let send_task = tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            if event.tenant_id != tenant_id {
                continue;
            }
            if sender
                .send(Message::Text(event.payload.into()))
                .await
                .is_err()
            {
                break;
            }
        }
    });
    let receive_task = tokio::spawn(async move {
        while let Some(Ok(message)) = receiver.next().await {
            if matches!(message, Message::Close(_)) {
                break;
            }
        }
    });
    tokio::select! { _ = send_task => {}, _ = receive_task => {} }
}

fn publish_event<T: Serialize>(
    state: &AppState,
    tenant_id: Uuid,
    event_type: &'static str,
    data: &T,
) -> Result<(), HttpError> {
    let payload = serde_json::to_string(&serde_json::json!({
        "event_type": event_type,
        "tenant_id": tenant_id,
        "occurred_at": Utc::now(),
        "data": data,
    }))?;
    let _ = state.events.send(TenantEvent { tenant_id, payload });
    Ok(())
}

fn require_database(state: &AppState) -> Result<&DatabaseConnection, HttpError> {
    state.db.as_ref().ok_or_else(HttpError::database_required)
}

fn cors_layer() -> anyhow::Result<CorsLayer> {
    let raw = env::var("CORS_ALLOWED_ORIGINS").unwrap_or_else(|_| {
        [
            "http://localhost:8081",
            "http://localhost:8082",
            "http://localhost:8083",
        ]
        .join(",")
    });
    let origins: Vec<HeaderValue> = raw
        .split(',')
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
        .map(str::parse)
        .collect::<Result<_, _>>()?;
    anyhow::ensure!(
        !origins.is_empty(),
        "CORS_ALLOWED_ORIGINS must contain at least one explicit origin"
    );

    Ok(CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([
            AUTHORIZATION,
            CONTENT_TYPE,
            HeaderName::from_static(tenant::TENANT_HEADER),
        ]))
}

fn env_flag(name: &str) -> bool {
    env::var(name).ok().is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes"
        )
    })
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_development() {
        assert_eq!(
            AppEnvironment::parse(None),
            Ok(AppEnvironment::Development)
        );
        assert_eq!(
            AppEnvironment::parse(Some("  ")),
            Ok(AppEnvironment::Development)
        );
    }

    #[test]
    fn parses_supported_environment_aliases() {
        assert_eq!(
            AppEnvironment::parse(Some("DEV")),
            Ok(AppEnvironment::Development)
        );
        assert_eq!(
            AppEnvironment::parse(Some("test")),
            Ok(AppEnvironment::Test)
        );
        assert_eq!(
            AppEnvironment::parse(Some("Prod")),
            Ok(AppEnvironment::Production)
        );
    }

    #[test]
    fn rejects_unknown_environment() {
        let error = AppEnvironment::parse(Some("staging"))
            .expect_err("unknown environments must fail closed");
        assert!(error.contains("unsupported APP_ENV"));
    }

    #[test]
    fn blocks_production_while_rule_storage_is_process_local() {
        let error = validate_startup_policy(AppEnvironment::Production, true)
            .expect_err("production must remain blocked");
        assert!(error.contains("process memory"));
    }

    #[test]
    fn requires_explicit_insecure_tenant_header_opt_in() {
        let error = validate_startup_policy(AppEnvironment::Development, false)
            .expect_err("development tenant-header compatibility must be explicit");
        assert!(error.contains("ALLOW_INSECURE_TENANT_HEADER"));
    }

    #[test]
    fn permits_explicit_development_and_test_scaffolds() {
        assert!(validate_startup_policy(AppEnvironment::Development, true).is_ok());
        assert!(validate_startup_policy(AppEnvironment::Test, true).is_ok());
    }
}

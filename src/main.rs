use std::{collections::HashMap, env, sync::Arc};

use anyhow::Context;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use chrono::{DateTime, Utc};
use futures_util::{SinkExt, StreamExt};
use sea_orm::{Database, DatabaseConnection};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing::{info, warn};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppEnvironment {
    Development,
    Test,
    Production,
}

impl AppEnvironment {
    fn parse(value: Option<&str>) -> Result<Self, String> {
        let normalized = value.unwrap_or("development").trim().to_ascii_lowercase();
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
            "production startup blocked: alert rules are still stored only in process memory; implement the durable tenant repository in DEN-3459 before deployment",
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
    production_ready: bool,
    database_connected: bool,
    supabase_configured: bool,
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
        "Embedded Alerts is using process-local alert rule state; production startup is disabled"
    );

    let app = Router::new()
        .route("/healthz", get(health))
        .route("/v1/alerts", get(list_records).post(create_record))
        .route("/v1/alerts/{id}", get(get_record))
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
        production_ready: false,
        database_connected: state.db.is_some(),
        supabase_configured: state.supabase_url.is_some(),
    })
}

async fn list_records(State(state): State<AppState>) -> Json<Vec<AlertRule>> {
    Json(state.records.read().await.values().cloned().collect())
}

async fn get_record(Path(id): Path<Uuid>, State(state): State<AppState>) -> impl IntoResponse {
    match state.records.read().await.get(&id).cloned() {
        Some(record) => (StatusCode::OK, Json(record)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn create_record(
    State(state): State<AppState>,
    Json(input): Json<CreateAlertRule>,
) -> impl IntoResponse {
    let now = Utc::now();
    let record = AlertRule {
        id: Uuid::new_v4(),
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
    let _ = state
        .events
        .send(serde_json::to_string(&record).unwrap_or_default());
    (StatusCode::CREATED, Json(record))
}

async fn ws_upgrade(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| websocket(socket, state))
}

async fn websocket(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();
    let mut events = state.events.subscribe();
    let send_task = tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            if sender.send(Message::Text(event.into())).await.is_err() {
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
        let error = validate_startup_policy(AppEnvironment::Production)
            .expect_err("production must remain blocked");
        assert!(error.contains("process memory"));
    }

    #[test]
    fn permits_development_and_test_for_scaffold_work() {
        assert!(validate_startup_policy(AppEnvironment::Development).is_ok());
        assert!(validate_startup_policy(AppEnvironment::Test).is_ok());
    }
}

use std::{collections::HashMap, env, sync::Arc};

use anyhow::Context;
use axum::{
    extract::{Path, State, ws::{Message, WebSocket, WebSocketUpgrade}},
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
use tracing::info;
use uuid::Uuid;

#[derive(Clone)]
struct AppState {
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
    database_configured: bool,
    supabase_configured: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let db = match env::var("DATABASE_URL") {
        Ok(url) if !url.trim().is_empty() => Some(Database::connect(url).await.context("connect database")?),
        _ => None,
    };
    let (events, _) = broadcast::channel(512);
    let state = AppState {
        db,
        records: Arc::new(RwLock::new(HashMap::new())),
        events,
        supabase_url: env::var("SUPABASE_URL").ok(),
    };

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
    axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()).await?;
    Ok(())
}

async fn health(State(state): State<AppState>) -> Json<Health> {
    Json(Health {
        service: "eal-api",
        status: "ok",
        database_configured: state.db.is_some(),
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
    state.records.write().await.insert(record.id, record.clone());
    let _ = state.events.send(serde_json::to_string(&record).unwrap_or_default());
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
            if sender.send(Message::Text(event.into())).await.is_err() { break; }
        }
    });
    let receive_task = tokio::spawn(async move {
        while let Some(Ok(message)) = receiver.next().await {
            if matches!(message, Message::Close(_)) { break; }
        }
    });
    tokio::select! { _ = send_task => {}, _ = receive_task => {} }
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

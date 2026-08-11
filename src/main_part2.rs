async fn list_sources(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<Vec<semantic::SourceDomain>>, ApiResponseError> {
    let tenant_id = tenant_id(&headers, &state)?;
    Ok(Json(state.semantic.list_sources(tenant_id).await))
}

async fn create_source(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(input): Json<CreateSourceDomain>,
) -> Result<impl IntoResponse, ApiResponseError> {
    let tenant_id = tenant_id(&headers, &state)?;
    let source = state.semantic.register_source(tenant_id, input).await?;
    Ok((StatusCode::CREATED, Json(source)))
}

async fn scan_source(
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<semantic::ScanReport>, ApiResponseError> {
    let tenant_id = tenant_id(&headers, &state)?;
    Ok(Json(state.semantic.scan_source(tenant_id, id).await?))
}

async fn ingest_page(
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    Json(input): Json<IngestPageRequest>,
) -> Result<impl IntoResponse, ApiResponseError> {
    let tenant_id = tenant_id(&headers, &state)?;
    let outcome = state
        .semantic
        .ingest_supplied_page(tenant_id, id, input)
        .await?;
    let status = match outcome.disposition {
        semantic::IngestDisposition::Created | semantic::IngestDisposition::Updated => {
            StatusCode::CREATED
        }
        semantic::IngestDisposition::Unchanged => StatusCode::OK,
    };
    Ok((status, Json(outcome)))
}

async fn list_pages(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<Vec<semantic::PageIndexRecord>>, ApiResponseError> {
    let tenant_id = tenant_id(&headers, &state)?;
    Ok(Json(state.semantic.list_pages(tenant_id).await))
}

async fn search_embeddings(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(input): Json<SemanticSearchRequest>,
) -> Result<Json<semantic::SemanticSearchResponse>, ApiResponseError> {
    let tenant_id = tenant_id(&headers, &state)?;
    Ok(Json(state.semantic.search(tenant_id, input).await?))
}

async fn list_matches(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<Vec<semantic::MatchCandidate>>, ApiResponseError> {
    let tenant_id = tenant_id(&headers, &state)?;
    Ok(Json(state.semantic.list_matches(tenant_id).await))
}

fn tenant_id(headers: &HeaderMap, state: &AppState) -> Result<Uuid, ApiResponseError> {
    match headers.get(TENANT_HEADER) {
        Some(value) => {
            let value = value.to_str().map_err(|_| {
                ApiResponseError(SemanticError::invalid(
                    "tenant_header",
                    "x-eal-tenant-id must be valid UTF-8",
                ))
            })?;
            Uuid::parse_str(value.trim()).map_err(|error| {
                ApiResponseError(SemanticError::invalid(
                    "tenant_header",
                    format!("x-eal-tenant-id must be a UUID: {error}"),
                ))
            })
        }
        None => Ok(state.default_tenant_id),
    }
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


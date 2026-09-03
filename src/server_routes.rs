async fn list_records(
    Authenticated(auth): Authenticated,
    State(state): State<AppState>,
) -> Result<Json<Vec<AlertRule>>, HttpError> {
    let database = require_database(&state)?;
    Ok(Json(
        alert_store::list_alert_rules(
            database,
            auth.tenant_id,
            &auth.subject,
            auth.is_tenant_admin(),
        )
        .await?,
    ))
}

async fn get_record(
    Authenticated(auth): Authenticated,
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<AlertRule>, HttpError> {
    let database = require_database(&state)?;
    alert_store::get_alert_rule(
        database,
        auth.tenant_id,
        id,
        &auth.subject,
        auth.is_tenant_admin(),
    )
    .await?
    .map(Json)
    .ok_or_else(|| HttpError::not_found("alert rule was not found"))
}

async fn create_record(
    Authenticated(auth): Authenticated,
    State(state): State<AppState>,
    Json(input): Json<CreateAlertRule>,
) -> Result<(StatusCode, Json<AlertRule>), HttpError> {
    let input = input.normalized().map_err(HttpError::validation)?;
    let database = require_database(&state)?;
    let record =
        alert_store::create_alert_rule(database, auth.tenant_id, &auth.subject, &input).await?;
    publish_event(
        &state,
        auth.tenant_id,
        EventAudience::SubjectOrTenantAdministrators(auth.subject),
        "alert_rule.created",
        &record,
    )?;
    Ok((StatusCode::CREATED, Json(record)))
}

async fn list_sources(
    Authenticated(auth): Authenticated,
    State(state): State<AppState>,
) -> Result<Json<Vec<SourcePolicy>>, HttpError> {
    let database = require_database(&state)?;
    Ok(Json(store::list_sources(database, auth.tenant_id).await?))
}

async fn get_source(
    Authenticated(auth): Authenticated,
    Path(source_id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<SourcePolicy>, HttpError> {
    let database = require_database(&state)?;
    store::get_source(database, auth.tenant_id, source_id)
        .await?
        .map(Json)
        .ok_or_else(|| HttpError::not_found("source policy was not found"))
}

async fn create_source(
    Authenticated(auth): Authenticated,
    State(state): State<AppState>,
    Json(input): Json<CreateSourcePolicy>,
) -> Result<(StatusCode, Json<SourcePolicy>), HttpError> {
    auth.require_tenant_admin()?;
    input.validate().map_err(HttpError::validation)?;
    let canonical_root =
        indexing::canonicalize_source_root(&input).map_err(HttpError::validation)?;
    let database = require_database(&state)?;
    let source = store::create_source(database, auth.tenant_id, &input, &canonical_root).await?;
    publish_event(
        &state,
        auth.tenant_id,
        EventAudience::TenantAdministrators,
        "source.created",
        &source,
    )?;
    Ok((StatusCode::CREATED, Json(source)))
}

async fn ingest_page(
    Path(source_id): Path<Uuid>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<PageIngestRequest>,
) -> Result<(StatusCode, Json<PageRevision>), HttpError> {
    worker_auth::authorize(&headers)?;
    let tenant_id = tenant::tenant_id_from_headers(&headers)?;
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
    publish_event(
        &state,
        tenant_id,
        EventAudience::TenantAdministrators,
        "page.revision_indexed",
        &revision,
    )?;
    Ok((status, Json(revision)))
}

async fn search_embeddings(
    Authenticated(auth): Authenticated,
    State(state): State<AppState>,
    Json(input): Json<EmbeddingSearchRequest>,
) -> Result<Json<EmbeddingSearchResponse>, HttpError> {
    input.validate().map_err(HttpError::validation)?;
    let database = require_database(&state)?;
    Ok(Json(
        store::search_embeddings(database, auth.tenant_id, &input)
            .await?
            .response,
    ))
}

fn require_enabled_alert_rule(enabled: bool) -> Result<(), HttpError> {
    if enabled {
        Ok(())
    } else {
        Err(HttpError::validation("alert rule is disabled"))
    }
}

async fn evaluate_matches(
    Authenticated(auth): Authenticated,
    State(state): State<AppState>,
    Json(input): Json<EvaluateMatchesRequest>,
) -> Result<Json<Vec<MatchCandidate>>, HttpError> {
    if !input.threshold.is_finite() || !(0.0..=1.0).contains(&input.threshold) {
        return Err(HttpError::validation(
            "threshold must be finite and between 0 and 1",
        ));
    }
    input.search.validate().map_err(HttpError::validation)?;
    let database = require_database(&state)?;
    let rule = alert_store::require_alert_rule(
        database,
        auth.tenant_id,
        input.alert_rule_id,
        &auth.subject,
        auth.is_tenant_admin(),
    )
    .await?;
    require_enabled_alert_rule(rule.enabled)?;
    if input.search.embedding.model != rule.embedding_model {
        return Err(HttpError::validation(
            "search embedding model must match the active alert-rule revision",
        ));
    }
    let threshold = input
        .threshold
        .max(f64::from(rule.similarity_threshold));
    let candidates = store::evaluate_matches(
        database,
        auth.tenant_id,
        input.alert_rule_id,
        rule.revision_id,
        threshold,
        input.search,
    )
    .await?;
    for candidate in &candidates {
        publish_event(
            &state,
            auth.tenant_id,
            EventAudience::SubjectOrTenantAdministrators(rule.owner_subject.clone()),
            "match.candidate",
            candidate,
        )?;
    }
    Ok(Json(candidates))
}

async fn ws_upgrade(
    Authenticated(auth): Authenticated,
    _origin: AuthorizedWebSocketOrigin,
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| websocket(socket, state, auth))
}

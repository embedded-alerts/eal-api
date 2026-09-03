async fn websocket(socket: WebSocket, state: AppState, auth: AuthContext) {
    let (mut sender, mut receiver) = socket.split();
    let mut events = state.events.subscribe();
    let send_auth = auth.clone();
    let mut send_task = tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    if !event_visible_to(&event, &send_auth) {
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
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(skipped, "WebSocket subscriber lagged behind tenant events");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    let mut receive_task = tokio::spawn(async move {
        while let Some(Ok(message)) = receiver.next().await {
            if matches!(message, Message::Close(_)) {
                break;
            }
        }
    });
    tokio::select! {
        _ = &mut send_task => receive_task.abort(),
        _ = &mut receive_task => send_task.abort(),
    }
}

fn event_visible_to(event: &TenantEvent, auth: &AuthContext) -> bool {
    if event.tenant_id != auth.tenant_id {
        return false;
    }
    match &event.audience {
        EventAudience::TenantAdministrators => auth.is_tenant_admin(),
        EventAudience::SubjectOrTenantAdministrators(subject) => {
            auth.can_access_owner(subject)
        }
    }
}

fn publish_event<T: Serialize>(
    state: &AppState,
    tenant_id: Uuid,
    audience: EventAudience,
    event_type: &'static str,
    data: &T,
) -> Result<(), HttpError> {
    let payload = serde_json::to_string(&serde_json::json!({
        "event_type": event_type,
        "tenant_id": tenant_id,
        "occurred_at": chrono::Utc::now(),
        "data": data,
    }))?;
    let _ = state.events.send(TenantEvent {
        tenant_id,
        audience,
        payload,
    });
    Ok(())
}

fn require_database(state: &AppState) -> Result<&DatabaseConnection, HttpError> {
    if !state.schema_ready {
        return Err(HttpError::database_required());
    }
    state.db.as_ref().ok_or_else(HttpError::database_required)
}

fn configured_origins(environment: AppEnvironment) -> anyhow::Result<Vec<HeaderValue>> {
    let raw = match env::var("CORS_ALLOWED_ORIGINS") {
        Ok(value) if !value.trim().is_empty() => value,
        _ if environment != AppEnvironment::Production => [
            "http://localhost:8081",
            "http://localhost:8082",
            "http://localhost:8083",
        ]
        .join(","),
        _ => anyhow::bail!(
            "CORS_ALLOWED_ORIGINS must contain explicit browser origins in production"
        ),
    };
    parse_origins(&raw)
}

fn parse_origins(raw: &str) -> anyhow::Result<Vec<HeaderValue>> {
    let mut origins = Vec::new();
    for raw_origin in raw.split(',').map(str::trim).filter(|value| !value.is_empty()) {
        let url = Url::parse(raw_origin).context("parse CORS origin")?;
        anyhow::ensure!(
            matches!(url.scheme(), "http" | "https"),
            "CORS origins must use http or https"
        );
        anyhow::ensure!(url.host().is_some(), "CORS origins must contain a host");
        anyhow::ensure!(
            url.username().is_empty() && url.password().is_none(),
            "CORS origins must not contain credentials"
        );
        anyhow::ensure!(
            url.path() == "/" && url.query().is_none() && url.fragment().is_none(),
            "CORS origins must not contain paths, queries, or fragments"
        );
        let origin = url.origin().ascii_serialization();
        anyhow::ensure!(origin != "null", "opaque CORS origins are not allowed");
        origins.push(origin.parse::<HeaderValue>()?);
    }
    origins.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    origins.dedup();
    anyhow::ensure!(
        !origins.is_empty(),
        "CORS_ALLOWED_ORIGINS must contain at least one explicit origin"
    );
    Ok(origins)
}

fn cors_layer(origins: &[HeaderValue]) -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins.to_vec()))
        .allow_credentials(true)
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([
            AUTHORIZATION,
            CONTENT_TYPE,
            HeaderName::from_static(tenant::TENANT_HEADER),
            HeaderName::from_static(SUBJECT_HEADER),
            HeaderName::from_static(ROLES_HEADER),
        ])
}

fn authorize_websocket_origin(
    headers: &HeaderMap,
    allowed_origins: &[HeaderValue],
) -> Result<(), HttpError> {
    let origin = headers
        .get(ORIGIN)
        .ok_or_else(|| HttpError::forbidden("WebSocket requests require an Origin header"))?;
    if allowed_origins.iter().any(|allowed| allowed == origin) {
        Ok(())
    } else {
        Err(HttpError::forbidden(
            "WebSocket origin is not in CORS_ALLOWED_ORIGINS",
        ))
    }
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

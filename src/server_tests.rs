#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use axum::{body::Body, http::Request};
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use hmac::{Hmac, Mac};
    use serde_json::json;
    use sha2::Sha256;
    use tower::ServiceExt;

    use super::*;

    const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";

    fn jwt_config() -> AuthConfig {
        AuthConfig::jwt(
            SECRET.to_vec(),
            "https://auth.example.test".into(),
            "eal-api".into(),
            "tenant_id".into(),
            "role".into(),
            BTreeSet::from([
                "authenticated".into(),
                "member".into(),
                "admin".into(),
                "owner".into(),
            ]),
            BTreeSet::from(["admin".into(), "owner".into()]),
            30,
            "eal_access_token".into(),
        )
        .unwrap()
    }

    fn token(tenant_id: Uuid, subject: &str, role: &str) -> String {
        type HmacSha256 = Hmac<Sha256>;
        let protected = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
        let claims = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({
                "iss": "https://auth.example.test",
                "aud": "eal-api",
                "sub": subject,
                "exp": chrono::Utc::now().timestamp() + 300,
                "iat": chrono::Utc::now().timestamp() - 1,
                "tenant_id": tenant_id,
                "role": role
            }))
            .unwrap(),
        );
        let signing_input = format!("{protected}.{claims}");
        let mut mac = HmacSha256::new_from_slice(SECRET).unwrap();
        mac.update(signing_input.as_bytes());
        let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
        format!("{signing_input}.{signature}")
    }

    fn state(auth: AuthConfig) -> AppState {
        let (events, _) = broadcast::channel(32);
        AppState {
            environment: AppEnvironment::Test,
            db: None,
            schema_ready: false,
            ingest_auth_configured: false,
            events,
            auth: Arc::new(auth),
            allowed_origins: Arc::new(parse_origins("https://app.example.test").unwrap()),
            supabase_url: None,
        }
    }

    fn websocket_request(token: &str, origin: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder()
            .uri("/v1/ws")
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .header("connection", "upgrade")
            .header("upgrade", "websocket")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==");
        if let Some(origin) = origin {
            builder = builder.header(ORIGIN, origin);
        }
        builder.body(Body::empty()).unwrap()
    }

    #[test]
    fn startup_requires_database_schema_jwt_and_crawler_auth_only_in_production() {
        assert!(
            validate_startup_policy(
                AppEnvironment::Development,
                false,
                false,
                false,
                false,
            )
            .is_ok()
        );
        assert!(
            validate_startup_policy(
                AppEnvironment::Production,
                false,
                false,
                true,
                true,
            )
            .is_err()
        );
        assert!(
            validate_startup_policy(
                AppEnvironment::Production,
                true,
                false,
                true,
                true,
            )
            .is_err()
        );
        assert!(
            validate_startup_policy(
                AppEnvironment::Production,
                true,
                true,
                false,
                true,
            )
            .is_err()
        );
        assert!(
            validate_startup_policy(
                AppEnvironment::Production,
                true,
                true,
                true,
                false,
            )
            .is_err()
        );
        assert!(
            validate_startup_policy(
                AppEnvironment::Production,
                true,
                true,
                true,
                true,
            )
            .is_ok()
        );
    }

    #[test]
    fn disabled_alert_rules_fail_closed_before_match_evaluation() {
        assert!(require_enabled_alert_rule(true).is_ok());
        assert!(require_enabled_alert_rule(false).is_err());
    }

    fn auth_context(tenant_id: Uuid, subject: &str, role: &str) -> AuthContext {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!(
                "Bearer {}",
                token(tenant_id, subject, role)
            ))
            .unwrap(),
        );
        jwt_config().authenticate(&headers).unwrap()
    }

    #[test]
    fn event_visibility_enforces_tenant_subject_and_admin_roles() {
        let tenant_id = Uuid::new_v4();
        let other_tenant = Uuid::new_v4();
        let member = auth_context(tenant_id, "member-a", "member");
        let admin = auth_context(tenant_id, "admin-a", "admin");
        let owned = TenantEvent {
            tenant_id,
            audience: EventAudience::SubjectOrTenantAdministrators("member-a".into()),
            payload: "{}".into(),
        };
        assert!(event_visible_to(&owned, &member));
        assert!(event_visible_to(&owned, &admin));

        let admin_only = TenantEvent {
            tenant_id,
            audience: EventAudience::TenantAdministrators,
            payload: "{}".into(),
        };
        assert!(!event_visible_to(&admin_only, &member));
        assert!(event_visible_to(&admin_only, &admin));

        let cross_tenant = TenantEvent {
            tenant_id: other_tenant,
            audience: EventAudience::SubjectOrTenantAdministrators("member-a".into()),
            payload: "{}".into(),
        };
        assert!(!event_visible_to(&cross_tenant, &member));
        assert!(!event_visible_to(&cross_tenant, &admin));
    }

    #[test]
    fn origin_configuration_is_explicit_and_path_free() {
        let origins = parse_origins(
            "https://app.example.test,http://localhost:8080,https://app.example.test",
        )
        .unwrap();
        assert_eq!(origins.len(), 2);
        assert!(parse_origins("*").is_err());
        assert!(parse_origins("https://app.example.test/path").is_err());
        assert!(parse_origins("https://user@app.example.test").is_err());
    }

    #[tokio::test]
    async fn http_routes_require_verified_identity_before_storage() {
        let tenant_id = Uuid::new_v4();
        let app = build_router(state(jwt_config()));
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/alerts")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/alerts")
                    .header(
                        AUTHORIZATION,
                        format!("Bearer {}", token(tenant_id, "member-a", "member")),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn websocket_upgrade_requires_jwt_and_allowed_origin() {
        let tenant_id = Uuid::new_v4();
        let token = token(tenant_id, "member-a", "member");
        let app = build_router(state(jwt_config()));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/ws")
                    .header("connection", "upgrade")
                    .header("upgrade", "websocket")
                    .header("sec-websocket-version", "13")
                    .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
                    .header(ORIGIN, "https://app.example.test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(websocket_request(&token, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let response = app
            .clone()
            .oneshot(websocket_request(&token, Some("https://evil.example.test")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let request = websocket_request(&token, Some("https://app.example.test"));
        let (parts, _) = request.into_parts();
        jwt_config().authenticate(&parts.headers).unwrap();
        authorize_websocket_origin(
            &parts.headers,
            parse_origins("https://app.example.test").unwrap().as_ref(),
        )
        .unwrap();
    }
}

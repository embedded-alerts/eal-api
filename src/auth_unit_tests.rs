#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use serde_json::json;

    const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";

    fn config() -> AuthConfig {
        AuthConfig::jwt(
            SECRET.to_vec(),
            "https://auth.example.test".into(),
            "eal-api".into(),
            "app_metadata.tenant_id".into(),
            "roles".into(),
            parse_role_set(DEFAULT_ALLOWED_ROLES).unwrap(),
            parse_role_set(DEFAULT_ADMIN_ROLES).unwrap(),
            0,
            DEFAULT_COOKIE_NAME.into(),
        )
        .unwrap()
    }

    fn token(claims: Value) -> String {
        let protected = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
        let claims = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        let signing_input = format!("{protected}.{claims}");
        let mut mac = HmacSha256::new_from_slice(SECRET).unwrap();
        mac.update(signing_input.as_bytes());
        let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
        format!("{signing_input}.{signature}")
    }

    fn claims(tenant_id: Uuid, role: &str) -> Value {
        json!({
            "iss": "https://auth.example.test",
            "aud": ["other", "eal-api"],
            "sub": "user-123",
            "exp": 2_000_000_000_i64,
            "iat": 1_900_000_000_i64,
            "app_metadata": {"tenant_id": tenant_id},
            "roles": [role]
        })
    }

    #[test]
    fn verifies_signed_tenant_and_role_claims() {
        let tenant_id = Uuid::new_v4();
        let token = token(claims(tenant_id, "admin"));
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        let context = config().authenticate_at(&headers, 1_950_000_000).unwrap();
        assert_eq!(context.tenant_id, tenant_id);
        assert_eq!(context.subject, "user-123");
        assert!(context.is_tenant_admin());
    }

    #[test]
    fn accepts_cookie_tokens_for_browser_websockets() {
        let tenant_id = Uuid::new_v4();
        let token = token(claims(tenant_id, "member"));
        let mut headers = HeaderMap::new();
        headers.insert(
            COOKIE,
            HeaderValue::from_str(&format!("other=1; {DEFAULT_COOKIE_NAME}={token}")).unwrap(),
        );
        let context = config().authenticate_at(&headers, 1_950_000_000).unwrap();
        assert_eq!(context.tenant_id, tenant_id);
        assert!(!context.is_tenant_admin());
    }

    #[test]
    fn rejects_expired_wrong_audience_and_modified_tokens() {
        let tenant_id = Uuid::new_v4();
        let mut expired = claims(tenant_id, "member");
        expired["exp"] = json!(100_i64);
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", token(expired))).unwrap(),
        );
        assert!(config().authenticate_at(&headers, 200).is_err());

        let mut wrong_audience = claims(tenant_id, "member");
        wrong_audience["aud"] = json!("another-service");
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", token(wrong_audience))).unwrap(),
        );
        assert!(config().authenticate_at(&headers, 1_950_000_000).is_err());

        let mut modified = token(claims(tenant_id, "member"));
        modified.push('x');
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {modified}")).unwrap(),
        );
        assert!(config().authenticate_at(&headers, 1_950_000_000).is_err());
    }

    #[test]
    fn rejects_disallowed_roles_and_conflicting_credentials() {
        let tenant_id = Uuid::new_v4();
        let disallowed_token = token(claims(tenant_id, "service_role"));
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {disallowed_token}")).unwrap(),
        );
        assert!(config().authenticate_at(&headers, 1_950_000_000).is_err());

        let member = token(claims(tenant_id, "member"));
        let admin = token(claims(tenant_id, "admin"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {member}")).unwrap(),
        );
        headers.insert(
            COOKIE,
            HeaderValue::from_str(&format!("{DEFAULT_COOKIE_NAME}={admin}")).unwrap(),
        );
        assert!(config().authenticate_at(&headers, 1_950_000_000).is_err());
    }

    #[test]
    fn insecure_mode_requires_explicit_subject_and_roles() {
        let tenant_id = Uuid::new_v4();
        let mut headers = HeaderMap::new();
        headers.insert(
            tenant::TENANT_HEADER,
            HeaderValue::from_str(&tenant_id.to_string()).unwrap(),
        );
        assert!(
            AuthConfig::insecure_headers()
                .authenticate(&headers)
                .is_err()
        );
        headers.insert(SUBJECT_HEADER, HeaderValue::from_static("developer"));
        headers.insert(ROLES_HEADER, HeaderValue::from_static("owner"));
        let context = AuthConfig::insecure_headers()
            .authenticate(&headers)
            .unwrap();
        assert_eq!(context.tenant_id, tenant_id);
        assert!(context.is_tenant_admin());
    }
}

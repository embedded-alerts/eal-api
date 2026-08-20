use std::{collections::BTreeSet, env, sync::Arc};

use axum::http::{
    HeaderMap,
    header::{AUTHORIZATION, COOKIE},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use uuid::Uuid;

use crate::{error::HttpError, tenant};

type HmacSha256 = Hmac<Sha256>;

pub const SUBJECT_HEADER: &str = "x-eal-subject-id";
pub const ROLES_HEADER: &str = "x-eal-roles";

const DEFAULT_COOKIE_NAME: &str = "eal_access_token";
const DEFAULT_TENANT_CLAIM: &str = "tenant_id";
const DEFAULT_ROLE_CLAIM: &str = "role";
const DEFAULT_ALLOWED_ROLES: &str = "authenticated,member,admin,owner";
const DEFAULT_ADMIN_ROLES: &str = "admin,owner";
const DEFAULT_LEEWAY_SECONDS: i64 = 30;
const MIN_SECRET_BYTES: usize = 32;
const MAX_SECRET_BYTES: usize = 4096;
const MAX_TOKEN_BYTES: usize = 16 * 1024;
const MAX_HEADER_BYTES: usize = 4 * 1024;
const MAX_CLAIMS_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct AuthConfig {
    mode: AuthMode,
    cookie_name: Arc<str>,
}

#[derive(Clone)]
enum AuthMode {
    Jwt(JwtVerifier),
    InsecureHeaders,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthContext {
    pub tenant_id: Uuid,
    pub subject: String,
    pub roles: BTreeSet<String>,
    tenant_admin: bool,
}

impl AuthContext {
    pub fn is_tenant_admin(&self) -> bool {
        self.tenant_admin
    }

    pub fn can_access_owner(&self, owner_subject: &str) -> bool {
        self.tenant_admin || self.subject == owner_subject
    }

    pub fn require_tenant_admin(&self) -> Result<(), HttpError> {
        if self.tenant_admin {
            Ok(())
        } else {
            Err(HttpError::forbidden(
                "this operation requires a tenant administrator role",
            ))
        }
    }
}

impl AuthConfig {
    pub fn from_env(production: bool, allow_insecure_tenant_header: bool) -> Result<Self, String> {
        let secret = env::var("EAL_JWT_HS256_SECRET")
            .ok()
            .filter(|value| !value.trim().is_empty());
        if let Some(secret) = secret {
            let issuer = required_env("EAL_JWT_ISSUER")?;
            let audience = required_env("EAL_JWT_AUDIENCE")?;
            let tenant_claim = env_string("EAL_JWT_TENANT_CLAIM", DEFAULT_TENANT_CLAIM);
            let role_claim = env_string("EAL_JWT_ROLE_CLAIM", DEFAULT_ROLE_CLAIM);
            let allowed_roles =
                parse_role_set(&env_string("EAL_JWT_ALLOWED_ROLES", DEFAULT_ALLOWED_ROLES))?;
            let admin_roles = parse_role_set(&env_string(
                "EAL_JWT_TENANT_ADMIN_ROLES",
                DEFAULT_ADMIN_ROLES,
            ))?;
            let leeway_seconds = env::var("EAL_JWT_LEEWAY_SECONDS")
                .ok()
                .map(|value| value.trim().parse::<i64>())
                .transpose()
                .map_err(|_| "EAL_JWT_LEEWAY_SECONDS must be an integer".to_owned())?
                .unwrap_or(DEFAULT_LEEWAY_SECONDS);
            let cookie_name = env_string("EAL_JWT_COOKIE_NAME", DEFAULT_COOKIE_NAME);
            return Self::jwt(
                secret.into_bytes(),
                issuer,
                audience,
                tenant_claim,
                role_claim,
                allowed_roles,
                admin_roles,
                leeway_seconds,
                cookie_name,
            );
        }

        if production {
            return Err(
                "production authentication requires EAL_JWT_HS256_SECRET, EAL_JWT_ISSUER, and EAL_JWT_AUDIENCE"
                    .into(),
            );
        }
        if allow_insecure_tenant_header {
            return Ok(Self::insecure_headers());
        }
        Err(
            "authentication is not configured; set JWT verification variables or explicitly enable the isolated development header scaffold"
                .into(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn jwt(
        secret: Vec<u8>,
        issuer: String,
        audience: String,
        tenant_claim: String,
        role_claim: String,
        allowed_roles: BTreeSet<String>,
        admin_roles: BTreeSet<String>,
        leeway_seconds: i64,
        cookie_name: String,
    ) -> Result<Self, String> {
        if !(MIN_SECRET_BYTES..=MAX_SECRET_BYTES).contains(&secret.len()) {
            return Err(format!(
                "EAL_JWT_HS256_SECRET must contain between {MIN_SECRET_BYTES} and {MAX_SECRET_BYTES} bytes"
            ));
        }
        let issuer = issuer.trim().to_owned();
        let audience = audience.trim().to_owned();
        if issuer.is_empty() || issuer.len() > 512 {
            return Err("EAL_JWT_ISSUER must contain between 1 and 512 bytes".into());
        }
        if audience.is_empty() || audience.len() > 512 {
            return Err("EAL_JWT_AUDIENCE must contain between 1 and 512 bytes".into());
        }
        let tenant_claim_path = parse_claim_path(&tenant_claim)?;
        let role_claim_path = parse_claim_path(&role_claim)?;
        if allowed_roles.is_empty() {
            return Err("EAL_JWT_ALLOWED_ROLES must contain at least one role".into());
        }
        if !admin_roles.is_subset(&allowed_roles) {
            return Err(
                "EAL_JWT_TENANT_ADMIN_ROLES must be a subset of EAL_JWT_ALLOWED_ROLES".into(),
            );
        }
        if !(0..=300).contains(&leeway_seconds) {
            return Err("EAL_JWT_LEEWAY_SECONDS must be between 0 and 300".into());
        }
        validate_cookie_name(&cookie_name)?;

        Ok(Self {
            mode: AuthMode::Jwt(JwtVerifier {
                secret: Arc::from(secret),
                issuer: issuer.into(),
                audience: audience.into(),
                tenant_claim_path,
                role_claim_path,
                allowed_roles: Arc::new(allowed_roles),
                admin_roles: Arc::new(admin_roles),
                leeway_seconds,
            }),
            cookie_name: cookie_name.into(),
        })
    }

    pub fn insecure_headers() -> Self {
        Self {
            mode: AuthMode::InsecureHeaders,
            cookie_name: DEFAULT_COOKIE_NAME.into(),
        }
    }

    pub fn is_jwt(&self) -> bool {
        matches!(&self.mode, AuthMode::Jwt(_))
    }

    pub fn mode_name(&self) -> &'static str {
        match &self.mode {
            AuthMode::Jwt(_) => "verified_jwt",
            AuthMode::InsecureHeaders => "explicit_insecure_headers",
        }
    }

    pub fn authenticate(&self, headers: &HeaderMap) -> Result<AuthContext, HttpError> {
        self.authenticate_at(headers, chrono::Utc::now().timestamp())
    }

    fn authenticate_at(&self, headers: &HeaderMap, now: i64) -> Result<AuthContext, HttpError> {
        match &self.mode {
            AuthMode::Jwt(verifier) => {
                let token = extract_token(headers, &self.cookie_name)?;
                verifier
                    .verify(&token, now)
                    .map_err(|_| HttpError::unauthorized("invalid bearer token"))
            }
            AuthMode::InsecureHeaders => insecure_context(headers),
        }
    }
}

#[derive(Clone)]
struct JwtVerifier {
    secret: Arc<[u8]>,
    issuer: Arc<str>,
    audience: Arc<str>,
    tenant_claim_path: Arc<[String]>,
    role_claim_path: Arc<[String]>,
    allowed_roles: Arc<BTreeSet<String>>,
    admin_roles: Arc<BTreeSet<String>>,
    leeway_seconds: i64,
}

impl JwtVerifier {
    fn verify(&self, token: &str, now: i64) -> Result<AuthContext, ()> {
        if token.len() > MAX_TOKEN_BYTES || token.bytes().any(|byte| byte.is_ascii_whitespace()) {
            return Err(());
        }
        let mut segments = token.split('.');
        let protected = segments
            .next()
            .filter(|value| !value.is_empty())
            .ok_or(())?;
        let claims = segments
            .next()
            .filter(|value| !value.is_empty())
            .ok_or(())?;
        let signature = segments
            .next()
            .filter(|value| !value.is_empty())
            .ok_or(())?;
        if segments.next().is_some() {
            return Err(());
        }

        let protected_bytes = URL_SAFE_NO_PAD.decode(protected).map_err(|_| ())?;
        if protected_bytes.len() > MAX_HEADER_BYTES {
            return Err(());
        }
        let protected_json: Value = serde_json::from_slice(&protected_bytes).map_err(|_| ())?;
        let protected_object = protected_json.as_object().ok_or(())?;
        if protected_object.get("alg").and_then(Value::as_str) != Some("HS256") {
            return Err(());
        }
        if protected_object
            .get("typ")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.eq_ignore_ascii_case("JWT"))
        {
            return Err(());
        }
        if protected_object.get("crit").is_some() {
            return Err(());
        }

        let signature = URL_SAFE_NO_PAD.decode(signature).map_err(|_| ())?;
        if signature.len() != 32 {
            return Err(());
        }
        let mut mac = HmacSha256::new_from_slice(self.secret.as_ref()).map_err(|_| ())?;
        mac.update(format!("{protected}.{claims}").as_bytes());
        mac.verify_slice(&signature).map_err(|_| ())?;

        let claims_bytes = URL_SAFE_NO_PAD.decode(claims).map_err(|_| ())?;
        if claims_bytes.len() > MAX_CLAIMS_BYTES {
            return Err(());
        }
        let claims: Value = serde_json::from_slice(&claims_bytes).map_err(|_| ())?;
        let claims_object = claims.as_object().ok_or(())?;

        if claims_object.get("iss").and_then(Value::as_str) != Some(self.issuer.as_ref()) {
            return Err(());
        }
        if !audience_matches(claims_object.get("aud").ok_or(())?, self.audience.as_ref()) {
            return Err(());
        }
        let subject = claims_object
            .get("sub")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty() && value.len() <= 256)
            .ok_or(())?;
        let expiration = integer_claim(claims_object.get("exp").ok_or(())?)?;
        if expiration <= now.saturating_sub(self.leeway_seconds) {
            return Err(());
        }
        if let Some(not_before) = claims_object.get("nbf") {
            if integer_claim(not_before)? > now.saturating_add(self.leeway_seconds) {
                return Err(());
            }
        }
        if let Some(issued_at) = claims_object.get("iat") {
            if integer_claim(issued_at)? > now.saturating_add(self.leeway_seconds) {
                return Err(());
            }
        }

        let tenant = claim_at_path(&claims, &self.tenant_claim_path)
            .and_then(Value::as_str)
            .ok_or(())?;
        let tenant_id = Uuid::parse_str(tenant).map_err(|_| ())?;
        let roles = parse_roles(claim_at_path(&claims, &self.role_claim_path).ok_or(())?)?;
        if roles.is_disjoint(self.allowed_roles.as_ref()) {
            return Err(());
        }
        let tenant_admin = !roles.is_disjoint(self.admin_roles.as_ref());

        Ok(AuthContext {
            tenant_id,
            subject: subject.to_owned(),
            roles,
            tenant_admin,
        })
    }
}

fn insecure_context(headers: &HeaderMap) -> Result<AuthContext, HttpError> {
    let tenant_id = tenant::tenant_id_from_headers(headers)?;
    let subject = headers
        .get(SUBJECT_HEADER)
        .ok_or_else(|| HttpError::unauthorized("missing X-Eal-Subject-Id header"))?
        .to_str()
        .map_err(|_| HttpError::unauthorized("invalid X-Eal-Subject-Id header"))?
        .trim();
    if subject.is_empty() || subject.len() > 256 {
        return Err(HttpError::unauthorized(
            "X-Eal-Subject-Id must contain between 1 and 256 bytes",
        ));
    }
    let roles = headers
        .get(ROLES_HEADER)
        .ok_or_else(|| HttpError::unauthorized("missing X-Eal-Roles header"))?
        .to_str()
        .map_err(|_| HttpError::unauthorized("invalid X-Eal-Roles header"))?;
    let roles = parse_role_set(roles).map_err(HttpError::unauthorized)?;
    if roles.is_empty() {
        return Err(HttpError::unauthorized(
            "X-Eal-Roles must contain at least one role",
        ));
    }
    let tenant_admin = roles.contains("admin") || roles.contains("owner");
    Ok(AuthContext {
        tenant_id,
        subject: subject.to_owned(),
        roles,
        tenant_admin,
    })
}

fn extract_token(headers: &HeaderMap, cookie_name: &str) -> Result<String, HttpError> {
    let authorization_values: Vec<_> = headers.get_all(AUTHORIZATION).iter().collect();
    if authorization_values.len() > 1 {
        return Err(HttpError::unauthorized(
            "multiple Authorization headers are not allowed",
        ));
    }
    let authorization = authorization_values
        .first()
        .map(|value| {
            let value = value
                .to_str()
                .map_err(|_| HttpError::unauthorized("invalid Authorization header"))?;
            let mut parts = value.split_ascii_whitespace();
            let scheme = parts.next().unwrap_or_default();
            let token = parts.next().unwrap_or_default();
            if !scheme.eq_ignore_ascii_case("Bearer") || token.is_empty() || parts.next().is_some()
            {
                return Err(HttpError::unauthorized(
                    "Authorization must use a single Bearer token",
                ));
            }
            Ok(token.to_owned())
        })
        .transpose()?;
    let cookie = cookie_token(headers, cookie_name)?;

    match (authorization, cookie) {
        (Some(authorization), Some(cookie)) if authorization != cookie => Err(
            HttpError::unauthorized("conflicting JWT credentials were supplied"),
        ),
        (Some(token), _) | (_, Some(token)) => Ok(token),
        (None, None) => Err(HttpError::unauthorized("missing bearer token")),
    }
}

fn cookie_token(headers: &HeaderMap, cookie_name: &str) -> Result<Option<String>, HttpError> {
    let mut found = None;
    for header in headers.get_all(COOKIE).iter() {
        let header = header
            .to_str()
            .map_err(|_| HttpError::unauthorized("invalid Cookie header"))?;
        for pair in header.split(';') {
            let Some((name, value)) = pair.trim().split_once('=') else {
                continue;
            };
            if name != cookie_name {
                continue;
            }
            if value.is_empty() {
                return Err(HttpError::unauthorized("JWT cookie must not be empty"));
            }
            if found.as_deref().is_some_and(|existing| existing != value) {
                return Err(HttpError::unauthorized(
                    "conflicting JWT cookies were supplied",
                ));
            }
            found = Some(value.to_owned());
        }
    }
    Ok(found)
}

fn audience_matches(value: &Value, expected: &str) -> bool {
    match value {
        Value::String(value) => value == expected,
        Value::Array(values) => values.iter().any(|value| value.as_str() == Some(expected)),
        _ => false,
    }
}

fn integer_claim(value: &Value) -> Result<i64, ()> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .ok_or(())
}

fn claim_at_path<'a>(value: &'a Value, path: &[String]) -> Option<&'a Value> {
    path.iter()
        .try_fold(value, |current, part| current.get(part.as_str()))
}

fn parse_roles(value: &Value) -> Result<BTreeSet<String>, ()> {
    let mut roles = BTreeSet::new();
    match value {
        Value::String(value) => {
            for role in value.split(',') {
                insert_role(&mut roles, role).map_err(|_| ())?;
            }
        }
        Value::Array(values) => {
            for value in values {
                insert_role(&mut roles, value.as_str().ok_or(())?).map_err(|_| ())?;
            }
        }
        _ => return Err(()),
    }
    if roles.is_empty() { Err(()) } else { Ok(roles) }
}

fn parse_role_set(value: &str) -> Result<BTreeSet<String>, String> {
    let mut roles = BTreeSet::new();
    for role in value.split(',') {
        insert_role(&mut roles, role)?;
    }
    Ok(roles)
}

fn insert_role(roles: &mut BTreeSet<String>, role: &str) -> Result<(), String> {
    let role = role.trim().to_ascii_lowercase();
    if role.is_empty() {
        return Ok(());
    }
    if role.len() > 80
        || !role
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b':' | b'.'))
    {
        return Err("roles must use 1 to 80 ASCII identifier characters".into());
    }
    roles.insert(role);
    Ok(())
}

fn parse_claim_path(value: &str) -> Result<Arc<[String]>, String> {
    let parts: Vec<String> = value.split('.').map(str::trim).map(str::to_owned).collect();
    if parts.is_empty()
        || parts.len() > 8
        || parts.iter().any(|part| {
            part.is_empty()
                || part.len() > 80
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
    {
        return Err("JWT claim paths must contain 1 to 8 identifier segments".into());
    }
    Ok(parts.into())
}

fn validate_cookie_name(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 120
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err("EAL_JWT_COOKIE_NAME must be an ASCII cookie identifier".into());
    }
    Ok(())
}

fn required_env(name: &str) -> Result<String, String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{name} is required when JWT authentication is enabled"))
}

fn env_string(name: &str, default: &str) -> String {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_owned())
}

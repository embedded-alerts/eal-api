use axum::{
    extract::FromRequestParts,
    http::{HeaderMap, request::Parts},
};
use uuid::Uuid;

use crate::error::HttpError;

pub const TENANT_HEADER: &str = "x-eal-tenant-id";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TenantId(pub Uuid);

pub fn tenant_id_from_headers(headers: &HeaderMap) -> Result<Uuid, HttpError> {
    let value = headers
        .get(TENANT_HEADER)
        .ok_or_else(|| HttpError::unauthorized("missing X-Eal-Tenant-Id header"))?;
    let value = value
        .to_str()
        .map_err(|_| HttpError::unauthorized("invalid X-Eal-Tenant-Id header"))?;
    Uuid::parse_str(value)
        .map_err(|_| HttpError::unauthorized("X-Eal-Tenant-Id must be a UUID"))
}

impl<S> FromRequestParts<S> for TenantId
where
    S: Send + Sync,
{
    type Rejection = HttpError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        tenant_id_from_headers(&parts.headers).map(Self)
    }
}

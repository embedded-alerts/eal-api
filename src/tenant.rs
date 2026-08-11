use axum::{extract::FromRequestParts, http::request::Parts};
use uuid::Uuid;

use crate::error::HttpError;

pub const TENANT_HEADER: &str = "x-eal-tenant-id";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TenantId(pub Uuid);

impl<S> FromRequestParts<S> for TenantId
where
    S: Send + Sync,
{
    type Rejection = HttpError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        let value = parts
            .headers
            .get(TENANT_HEADER)
            .ok_or_else(|| HttpError::unauthorized("missing X-Eal-Tenant-Id header"))?;
        let value = value
            .to_str()
            .map_err(|_| HttpError::unauthorized("invalid X-Eal-Tenant-Id header"))?;
        let tenant_id = Uuid::parse_str(value)
            .map_err(|_| HttpError::unauthorized("X-Eal-Tenant-Id must be a UUID"))?;
        Ok(Self(tenant_id))
    }
}

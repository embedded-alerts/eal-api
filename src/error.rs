use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

#[derive(Debug)]
pub struct HttpError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

impl HttpError {
    pub fn validation(error: impl std::fmt::Display) -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation_error",
            error.to_string(),
        )
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", message.into())
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", message.into())
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message.into())
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "conflict", message.into())
    }

    pub fn database_required() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "database_required",
            "this route requires durable PostgreSQL storage",
        )
    }

    pub fn ingest_auth_required() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "ingest_auth_required",
            "page ingestion is disabled until crawler authentication is configured",
        )
    }

    pub fn internal(error: impl std::fmt::Display) -> Self {
        tracing::error!(error = %error, "request failed");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "the request could not be completed",
        )
    }

    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                code: self.code,
                message: self.message,
            }),
        )
            .into_response()
    }
}

impl From<sea_orm::DbErr> for HttpError {
    fn from(error: sea_orm::DbErr) -> Self {
        Self::internal(error)
    }
}

impl From<serde_json::Error> for HttpError {
    fn from(error: serde_json::Error) -> Self {
        Self::internal(error)
    }
}

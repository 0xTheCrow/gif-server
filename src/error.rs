use axum::{http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::json;
use thiserror::Error;

#[allow(dead_code)]
#[derive(Debug, Error)]
pub enum AppError {
    #[error("not found")]
    NotFound,
    #[error("unauthorized")]
    Unauthorized,
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("internal error: {0}")]
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            AppError::NotFound =>
                (StatusCode::NOT_FOUND, "not_found", "not found".to_string()),
            AppError::Unauthorized =>
                (StatusCode::UNAUTHORIZED, "unauthorized", "unauthorized".to_string()),
            AppError::BadRequest(msg) =>
                (StatusCode::BAD_REQUEST, "bad_request", format!("bad request: {}", msg)),
            AppError::Conflict(msg) =>
                (StatusCode::CONFLICT, "conflict", format!("conflict: {}", msg)),
            AppError::Database(e) => {
                tracing::error!("database error: {}", e);
                (StatusCode::INTERNAL_SERVER_ERROR, "internal_error", "an internal error occurred".to_string())
            }
            AppError::Io(e) => {
                tracing::error!("io error: {}", e);
                (StatusCode::INTERNAL_SERVER_ERROR, "internal_error", "an internal error occurred".to_string())
            }
            AppError::Internal(msg) => {
                tracing::error!("internal error: {}", msg);
                (StatusCode::INTERNAL_SERVER_ERROR, "internal_error", "an internal error occurred".to_string())
            }
        };

        (status, Json(json!({ "error": code, "message": message }))).into_response()
    }
}

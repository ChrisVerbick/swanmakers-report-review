//! One error type for every handler: a status, a JSON body, and a log line for the unexpected.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

#[derive(Debug)]
pub enum AppError {
    BadRequest(String),
    /// The body says what kind of login would help (`{"error": "login", "mode": …}`).
    Unauthorized(Value),
    Forbidden(String),
    NotFound(String),
    Conflict(String),
    TooMany,
    Internal(String),
}

pub type Result<T> = std::result::Result<T, AppError>;

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::BadRequest(m) | AppError::Forbidden(m) | AppError::NotFound(m) | AppError::Conflict(m) | AppError::Internal(m) => {
                f.write_str(m)
            }
            AppError::Unauthorized(v) => write!(f, "{v}"),
            AppError::TooMany => f.write_str("too many requests"),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, json!({"error": m})),
            AppError::Unauthorized(v) => (StatusCode::UNAUTHORIZED, v),
            AppError::Forbidden(m) => (StatusCode::FORBIDDEN, json!({"error": m})),
            AppError::NotFound(m) => (StatusCode::NOT_FOUND, json!({"error": m})),
            AppError::Conflict(m) => (StatusCode::CONFLICT, json!({"error": m})),
            AppError::TooMany => (StatusCode::TOO_MANY_REQUESTS, json!({"error": "too many requests; try again in a few minutes"})),
            AppError::Internal(m) => {
                tracing::error!("{m}");
                (StatusCode::INTERNAL_SERVER_ERROR, json!({"error": "internal error"}))
            }
        };
        (status, Json(body)).into_response()
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        AppError::Internal(format!("database: {e}"))
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::Internal(format!("file: {e}"))
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::Internal(format!("json: {e}"))
    }
}

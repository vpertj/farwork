//! One error type for every handler. The body shape follows FastAPI's
//! `HTTPException` (`{"detail": ...}`) so existing clients parse it unchanged;
//! unimplemented endpoints additionally carry `error: "not_implemented"`.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub detail: serde_json::Value,
    pub not_implemented: bool,
}

impl ApiError {
    pub fn new(status: StatusCode, detail: impl Into<serde_json::Value>) -> Self {
        Self {
            status,
            detail: detail.into(),
            not_implemented: false,
        }
    }

    pub fn bad_request(detail: impl Into<serde_json::Value>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, detail)
    }

    pub fn unauthorized(detail: impl Into<serde_json::Value>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, detail)
    }

    pub fn forbidden(detail: impl Into<serde_json::Value>) -> Self {
        Self::new(StatusCode::FORBIDDEN, detail)
    }

    pub fn not_found(detail: impl Into<serde_json::Value>) -> Self {
        Self::new(StatusCode::NOT_FOUND, detail)
    }

    pub fn conflict(detail: impl Into<serde_json::Value>) -> Self {
        Self::new(StatusCode::CONFLICT, detail)
    }

    pub fn unprocessable(detail: impl Into<serde_json::Value>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, detail)
    }

    pub fn internal(detail: impl Into<serde_json::Value>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, detail)
    }

    pub fn bad_gateway(detail: impl Into<serde_json::Value>) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, detail)
    }

    pub fn service_unavailable(detail: impl Into<serde_json::Value>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, detail)
    }

    pub fn gateway_timeout(detail: impl Into<serde_json::Value>) -> Self {
        Self::new(StatusCode::GATEWAY_TIMEOUT, detail)
    }

    /// 501 with an explicit machine-readable reason. Used for every capability
    /// this build does not implement yet — never a fake success.
    pub fn not_implemented(detail: impl Into<serde_json::Value>) -> Self {
        Self {
            status: StatusCode::NOT_IMPLEMENTED,
            detail: detail.into(),
            not_implemented: true,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = if self.not_implemented {
            json!({
                "error": "not_implemented",
                "detail": self.detail,
                "serverTime": crate::timeutil::utc_now(),
            })
        } else {
            json!({ "detail": self.detail })
        };
        (self.status, Json(body)).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(exc: sqlx::Error) -> Self {
        tracing::error!(error = %exc, "database error");
        ApiError::internal("internal database error")
    }
}

impl From<sqlx::migrate::MigrateError> for ApiError {
    fn from(exc: sqlx::migrate::MigrateError) -> Self {
        tracing::error!(error = %exc, "migration error");
        ApiError::internal("database migration failed")
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

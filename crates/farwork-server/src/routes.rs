use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::Json;
use farwork_protocol::{DevicesResponse, HealthResponse};

use crate::state::AppState;

/// Extract the token from an `Authorization: Bearer <token>` header.
pub(crate) fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_string())
}

pub(crate) fn require(state: &AppState, token: Option<&str>) -> Result<(), StatusCode> {
    if state.authorize(token) {
        Ok(())
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

pub async fn health(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<HealthResponse>, StatusCode> {
    require(&state, bearer_token(&headers).as_deref())?;
    Ok(Json(HealthResponse::ok()))
}

pub async fn devices(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<DevicesResponse>, StatusCode> {
    require(&state, bearer_token(&headers).as_deref())?;
    Ok(Json(DevicesResponse {
        devices: state.devices.snapshot(),
    }))
}

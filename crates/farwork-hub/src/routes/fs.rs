//! File browsing relay: `POST /connectors/{id}/fs/list` and
//! `POST /connectors/{id}/fs/readText`.
//!
//! The hub never touches the device's filesystem. It mints a requestId, pushes
//! `fs.request` down that connector's socket and answers the browser with the
//! matching `fs.response` — or with 504 when the device stays silent for ten
//! seconds, because a hung connector must not hang the browser with it.

use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult};
use crate::ingress::{self, FsEntry, FsErrorBody, IngressFsResponse};
use crate::routes::extract::AuthedUser;
use crate::state::AppState;
use crate::timeutil;

/// How long the hub waits for `fs.response` before answering 504.
pub const FS_RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/connectors/{connector_id}/fs/list", post(list))
        .route("/connectors/{connector_id}/fs/readText", post(read_text))
}

#[derive(Debug, Deserialize)]
pub struct FsRequest {
    #[serde(default)]
    pub path: String,
}

async fn list(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Path(connector_id): Path<String>,
    Json(payload): Json<FsRequest>,
) -> ApiResult<Json<Value>> {
    let requested = payload.path;
    let reply = relay(&state, &user_id, &connector_id, "list", &requested).await?;
    if !reply.ok {
        return Err(error_response(reply.error));
    }
    let Some(entries) = reply.entries else {
        return Err(malformed("a list request"));
    };
    let path = reply.path.unwrap_or(requested);
    let entries: Vec<Value> = entries.iter().map(FsEntry::view).collect();
    Ok(Json(json!({
        "path": path,
        "entries": entries,
        "serverTime": timeutil::utc_now(),
    })))
}

async fn read_text(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Path(connector_id): Path<String>,
    Json(payload): Json<FsRequest>,
) -> ApiResult<Json<Value>> {
    let requested = payload.path;
    let reply = relay(&state, &user_id, &connector_id, "readText", &requested).await?;
    if !reply.ok {
        return Err(error_response(reply.error));
    }
    let (Some(content), Some(size_bytes)) = (reply.content, reply.size_bytes) else {
        return Err(malformed("a readText request"));
    };
    Ok(Json(json!({
        "path": reply.path.unwrap_or(requested),
        "content": content,
        "sizeBytes": size_bytes,
        // The connector never truncates: an oversized file is refused with
        // too_large, so this reads "the body is whole" and nothing else.
        "truncated": reply.truncated.unwrap_or(false),
        "serverTime": timeutil::utc_now(),
    })))
}

/// Send one `fs.request` and wait for the reply that carries its requestId.
async fn relay(
    state: &AppState,
    user_id: &str,
    connector_id: &str,
    op: &str,
    raw_path: &str,
) -> ApiResult<IngressFsResponse> {
    debug_assert!(ingress::is_known_fs_op(op));
    let connector = crate::routes::connectors::find_connector(state.pool(), connector_id)
        .await?
        .filter(|row| row.user_id == user_id && !row.revoked)
        .ok_or_else(|| {
            // 404 rather than 403: a connector belonging to somebody else must not
            // be distinguishable from one that does not exist.
            ApiError::not_found(json!({
                "code": "connector_not_found",
                "message": "connector not found",
            }))
        })?;

    let path = raw_path.trim();
    if path.is_empty() || !std::path::Path::new(path).is_absolute() {
        return Err(ApiError::unprocessable(json!({
            "code": "invalid_path",
            "message": format!("path must be an absolute path, got {raw_path:?}"),
        })));
    }

    let handle = state.connectors().get(&connector.id).ok_or_else(offline)?;
    let request_id = crate::ids::request_id();
    // Registered before the frame leaves: a fast connector could answer before
    // the hub got back here.
    let receiver = state.fs_relay().register(&connector.id, &request_id);
    if !handle.send(&ingress::fs_request_frame(&request_id, op, path)) {
        state.fs_relay().abandon(&request_id);
        return Err(offline());
    }

    match tokio::time::timeout(FS_RESPONSE_TIMEOUT, receiver).await {
        Ok(Ok(reply)) => Ok(reply),
        // The waiting sender was dropped: the socket ended while the request was
        // in flight (see run_connector_socket).
        Ok(Err(_)) => Err(ApiError::service_unavailable(json!({
            "code": "connector_offline",
            "message": "the connector connection closed before answering",
        }))),
        Err(_) => {
            // Release the entry, or a late reply would keep it for the process's
            // lifetime.
            state.fs_relay().abandon(&request_id);
            Err(ApiError::gateway_timeout(json!({
                "code": "fs_timeout",
                "message": format!(
                    "the connector did not answer {op:?} within {}s",
                    FS_RESPONSE_TIMEOUT.as_secs()
                ),
            })))
        }
    }
}

fn offline() -> ApiError {
    ApiError::service_unavailable(json!({
        "code": "connector_offline",
        "message": "connector is not connected; the file request was not delivered",
    }))
}

fn malformed(what: &str) -> ApiError {
    ApiError::bad_gateway(json!({
        "code": "io_error",
        "message": format!("connector answered {what} without a usable body"),
    }))
}

/// The connector's own failure, mapped onto HTTP. The original code and message
/// survive: the browser shows the device's reason, not a hub paraphrase.
fn error_response(error: Option<FsErrorBody>) -> ApiError {
    let Some(error) = error else {
        return malformed("a failure");
    };
    let status = match error.code.as_str() {
        "not_found" => StatusCode::NOT_FOUND,
        "permission_denied" => StatusCode::FORBIDDEN,
        "too_large" => StatusCode::PAYLOAD_TOO_LARGE,
        // io_error, and anything a future connector invents: the device failed,
        // and a gateway error says so without inventing a meaning.
        _ => StatusCode::BAD_GATEWAY,
    };
    ApiError::new(
        status,
        json!({ "code": error.code, "message": error.message }),
    )
}

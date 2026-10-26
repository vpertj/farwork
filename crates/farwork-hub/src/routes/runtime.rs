//! Session runtime control: capabilities, catalogs, commands, steer, selections.
//!
//! Every answer here is the agent's own `session.capabilities` report, cached per
//! session (see `runtime_caps`). Nothing is derived: a hub that guessed "codex
//! probably supports interrupts" would hand the browser controls that do nothing,
//! and a fabricated model list would be worse than an empty one.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult};
use crate::ingress;
use crate::routes::extract::AuthedUser;
use crate::routes::sessions::owned_session;
use crate::runtime_caps::{CapabilityFlags, Catalog, CatalogItem, CommandEntry};
use crate::state::AppState;
use crate::timeutil;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/sessions/{session_id}/runtime/catalogs/{catalog}",
            get(session_runtime_catalog),
        )
        .route(
            "/sessions/{session_id}/runtime/commands",
            get(session_runtime_commands),
        )
        .route("/sessions/{session_id}/runtime/steer", post(steer_session))
        .route(
            "/sessions/{session_id}/runtime/selections",
            patch(patch_selections),
        )
}

// ------------------------------------------------------------- capabilities

/// What the runtime reported, or the all-false default when it has reported
/// nothing yet. The connector id is included so the browser can tell which
/// device the answer came from, exactly like `runtime/state` does.
pub fn capabilities_view(state: &AppState, session_id: &str, connector_id: &str) -> Value {
    let snapshot = state.runtime_caps().get(session_id);
    let (capabilities, revision, updated_at) = match &snapshot {
        Some(snapshot) => (
            snapshot.capabilities.view(),
            snapshot.revision,
            Value::String(snapshot.updated_at.clone()),
        ),
        None => (CapabilityFlags::default().view(), 0, Value::Null),
    };
    json!({
        "connectorId": connector_id,
        "capabilitySet": capabilities,
        "revision": revision,
        "reportedAt": updated_at,
        "serverTime": timeutil::utc_now(),
    })
}

// ----------------------------------------------------------------- catalogs

async fn session_runtime_catalog(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Path((session_id, catalog)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    owned_session(&state, &user_id, &session_id).await?;
    let Some(catalog) = Catalog::parse(&catalog) else {
        // Not an empty list: "unknown catalog" and "this runtime offers none"
        // are different answers and the caller acts on them differently.
        return Err(ApiError::unprocessable(json!({
            "code": "unknown_catalog",
            "message": "catalog must be one of [\"model\", \"permission\"]",
        })));
    };
    let snapshot = state.runtime_caps().get(&session_id);
    let (items, revision) = match &snapshot {
        Some(snapshot) => (
            catalog
                .items(&snapshot.catalogs)
                .iter()
                .map(CatalogItem::view)
                .collect::<Vec<_>>(),
            snapshot.revision,
        ),
        None => (Vec::new(), 0),
    };
    Ok(Json(json!({
        "catalog": catalog.key(),
        "items": items,
        "revision": revision,
        "serverTime": timeutil::utc_now(),
    })))
}

async fn session_runtime_commands(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Path(session_id): Path<String>,
) -> ApiResult<Json<Value>> {
    owned_session(&state, &user_id, &session_id).await?;
    let commands = state
        .runtime_caps()
        .get(&session_id)
        .map(|snapshot| {
            snapshot
                .commands
                .iter()
                .map(CommandEntry::view)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(Json(json!({
        "commands": commands,
        "serverTime": timeutil::utc_now(),
    })))
}

// -------------------------------------------------------------------- steer

#[derive(Debug, Deserialize)]
pub struct SteerRequest {
    #[serde(default)]
    pub content: String,
}

/// `POST /sessions/{id}/runtime/steer` — hand the running turn one more
/// instruction.
///
/// 202 means "delivered to the device", not "the runtime obeyed": the agent
/// acknowledges by writing the instruction into the timeline, which is what the
/// browser actually watches.
async fn steer_session(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Path(session_id): Path<String>,
    Json(payload): Json<SteerRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let session = owned_session(&state, &user_id, &session_id).await?;
    if session.archived {
        return Err(ApiError::conflict("session is archived"));
    }
    if !session.takeover {
        return Err(ApiError::conflict(
            "session is read-only until takeover is enabled",
        ));
    }
    let content = payload.content.trim();
    if content.is_empty() {
        return Err(ApiError::unprocessable(json!({
            "code": "empty_content",
            "message": "content must not be empty",
        })));
    }

    // No stream first: "there is nothing running to steer" is the actionable
    // answer when both that and "this runtime cannot steer" are true.
    let stream_id = ingress::active_stream(&state, &session_id)
        .await?
        .ok_or_else(|| {
            ApiError::conflict(json!({
                "code": "session_not_running",
                "message": "no active runtime stream for this session",
            }))
        })?;

    // Only a *reported* false is a refusal. A session whose capabilities have not
    // arrived yet is dispatched optimistically: refusing on silence would make a
    // slow first frame look like a missing feature.
    if let Some(snapshot) = state.runtime_caps().get(&session_id) {
        if !snapshot.capabilities.steer {
            return Err(ApiError::not_implemented(json!({
                "code": "steer_unsupported",
                "message": "the runtime reported capabilities.steer=false; the instruction was not delivered",
                "streamId": stream_id,
            })));
        }
    }

    let handle = state
        .connectors()
        .get(&session.connector_id)
        .ok_or_else(connector_offline)?;
    if !handle.send(&ingress::session_steer_frame(&stream_id, content)) {
        return Err(ApiError::conflict(json!({
            "code": "connector_offline",
            "message": "connector socket closed; the instruction was not delivered",
        })));
    }
    tracing::info!(session_id = %session_id, %stream_id, "session.steer dispatched");
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "delivered": true,
            "streamId": stream_id,
            "serverTime": timeutil::utc_now(),
        })),
    ))
}

// --------------------------------------------------------------- selections

#[derive(Debug, Deserialize)]
pub struct SelectionsPatchRequest {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default, rename = "permissionMode")]
    pub permission_mode: Option<String>,
}

fn non_empty(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// `PATCH /sessions/{id}/runtime/selections` — change the model and/or the
/// permission mode of a running session.
///
/// 200 only after the frame reached the connector: a 200 for an undelivered
/// selection would show the browser a choice the runtime never saw.
async fn patch_selections(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Path(session_id): Path<String>,
    Json(payload): Json<SelectionsPatchRequest>,
) -> ApiResult<Json<Value>> {
    let session = owned_session(&state, &user_id, &session_id).await?;
    if session.archived {
        return Err(ApiError::conflict("session is archived"));
    }
    if !session.takeover {
        return Err(ApiError::conflict(
            "session is read-only until takeover is enabled",
        ));
    }
    let model = non_empty(payload.model.as_deref());
    let permission_mode = non_empty(payload.permission_mode.as_deref());
    if model.is_none() && permission_mode.is_none() {
        return Err(ApiError::unprocessable(json!({
            "code": "no_selection",
            "message": "at least one of model, permissionMode is required",
        })));
    }

    let stream_id = ingress::active_stream(&state, &session_id)
        .await?
        .ok_or_else(|| {
            ApiError::conflict(json!({
                "code": "session_not_running",
                "message": "no active runtime stream for this session",
            }))
        })?;

    if let Some(snapshot) = state.runtime_caps().get(&session_id) {
        if !snapshot.capabilities.selections {
            return Err(ApiError::not_implemented(json!({
                "code": "selections_unsupported",
                "message": "the runtime reported capabilities.selections=false; the selection was not delivered",
                "streamId": stream_id,
            })));
        }
    }

    let handle = state
        .connectors()
        .get(&session.connector_id)
        .ok_or_else(connector_offline)?;
    if !handle.send(&ingress::session_selections_frame(
        &stream_id,
        model.as_deref(),
        permission_mode.as_deref(),
    )) {
        return Err(ApiError::conflict(json!({
            "code": "connector_offline",
            "message": "connector socket closed; the selection was not delivered",
        })));
    }

    let selections = record_selections(
        &state,
        &session_id,
        model.as_deref(),
        permission_mode.as_deref(),
    )
    .await?;
    tracing::info!(session_id = %session_id, %stream_id, "session.selections dispatched");
    Ok(Json(json!({
        "selections": selections,
        "serverTime": timeutil::utc_now(),
    })))
}

/// Merge the accepted selection into the session's runtime state, so `GET
/// runtime/state` and this response cannot disagree.
async fn record_selections(
    state: &AppState,
    session_id: &str,
    model: Option<&str>,
    permission_mode: Option<&str>,
) -> ApiResult<Value> {
    let mut selections = json!({});
    if let Some(row) = crate::routes::sessions::runtime_state_row(state.pool(), session_id).await? {
        let parsed =
            serde_json::from_str::<Value>(&row.selections_json).unwrap_or_else(|_| json!({}));
        if parsed.is_object() {
            selections = parsed;
        }
        if let Some(map) = selections.as_object_mut() {
            if let Some(model) = model {
                map.insert(String::from("model"), Value::String(model.to_string()));
            }
            if let Some(permission_mode) = permission_mode {
                map.insert(
                    String::from("permissionMode"),
                    Value::String(permission_mode.to_string()),
                );
            }
        }
        sqlx::query(
            "update session_runtime_states set selections_json = $2, updated_at = $3 where session_id = $1",
        )
        .bind(session_id)
        .bind(selections.to_string())
        .bind(timeutil::utc_now())
        .execute(state.pool())
        .await?;
    }
    Ok(selections)
}

fn connector_offline() -> ApiError {
    ApiError::conflict(json!({
        "code": "connector_offline",
        "message": "connector is not connected; the request was not delivered",
    }))
}

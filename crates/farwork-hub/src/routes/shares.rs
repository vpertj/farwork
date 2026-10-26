//! Session and message sharing.
//!
//! A share is an **immutable snapshot, not a live view**: the timeline (or one
//! item) is copied once under the session's write lock, stored in
//! `shares.snapshot_json`, and the public endpoint reads nothing but that row.
//! Changing the original session afterwards cannot change a published share.
//! That is the feature's core semantic, and it is also a privacy guarantee:
//! everything a stranger can see is exactly what the owner published.
//!
//! ## Why the public read is mounted separately
//!
//! Every other `/api/v2` handler resolves its caller through
//! [crate::routes::extract::AuthedUser]. [public_router] registers
//! `GET /public/shares/{share_id}` with **no** extractor, so a logged-out
//! visitor holding only the link can read it. Its body is assembled from a key
//! whitelist (see [shared_item]): `connectorId`, `workspacePath`, `cwd`,
//! `sessionId`, `source`, tokens and emails never enter the snapshot, so they
//! cannot leak out of it.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sqlx::PgPool;

use crate::error::{ApiError, ApiResult};
use crate::ids;
use crate::models::SessionRow;
use crate::routes::extract::AuthedUser;
use crate::routes::sessions::owned_session;
use crate::state::AppState;
use crate::timeutil;

/// Stored snapshot shape version. Shares are immutable and never rewritten, so
/// this only changes if a future build must read the old rows differently.
const SNAPSHOT_VERSION: i64 = 1;

/// Only what the public read needs. Ownership and scope columns stay in the
/// table; the unauthenticated handler must not be able to read them at all.
const SHARE_COLUMNS: &str = "id, scope, snapshot_json, created_at";

/// Owner-facing routes; both resolve the caller and require ownership.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/sessions/{session_id}/shares", post(create_share))
        .route(
            "/sessions/{session_id}/shares/{share_id}",
            delete(delete_share),
        )
}

/// The unauthenticated surface. Nothing here may take an [AuthedUser] — a public
/// link is the only credential a visitor has.
pub fn public_router() -> Router<AppState> {
    Router::new().route("/public/shares/{share_id}", get(public_share))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Session,
    Message,
}

impl Scope {
    fn parse(raw: &str) -> ApiResult<Self> {
        match raw {
            "session" => Ok(Self::Session),
            "message" => Ok(Self::Message),
            other => Err(ApiError::unprocessable(format!(
                "unknown share scope {other:?}"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Message => "message",
        }
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct ShareRow {
    id: String,
    scope: String,
    snapshot_json: String,
    created_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShareCreateRequest {
    scope: String,
    /// `messageId` per the contract; the snake_case spelling is accepted too
    /// because the upstream Python client sends it that way.
    #[serde(default, alias = "message_id")]
    message_id: Option<String>,
    /// Attachment whitelist. Stored with the share so a future share-attachment
    /// route can only ever serve ids the owner named here; this build has no
    /// such route, so an empty list is all that is served today.
    #[serde(default, alias = "allowed_file_ids")]
    allowed_file_ids: Vec<String>,
}

// ---------------------------------------------------------------- handlers

async fn create_share(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Path(session_id): Path<String>,
    Json(payload): Json<ShareCreateRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    // Ownership is checked against the session, so someone else's session id is
    // a 404 rather than a 403: a caller must not learn that it exists.
    let session = owned_session(&state, &user_id, &session_id).await?;
    let scope = Scope::parse(payload.scope.trim())?;
    let message_id = match scope {
        Scope::Session => None,
        Scope::Message => Some(
            payload
                .message_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    ApiError::unprocessable("messageId is required when scope is \"message\"")
                })?
                .to_string(),
        ),
    };

    let (event_cursor, items) = capture_items(&state, &session, message_id.as_deref()).await?;
    if items.is_empty() {
        // An empty share would be a link to nothing; refuse instead of
        // publishing a blank page that looks like a bug on the visitor's side.
        return Err(ApiError::unprocessable("there is no content to share"));
    }

    let snapshot = json!({
        "version": SNAPSHOT_VERSION,
        "sessionTitle": session.title.clone().unwrap_or_default(),
        "eventCursor": event_cursor,
        "items": items,
    });
    let allowed_file_ids_json = serde_json::to_string(&payload.allowed_file_ids)
        .map_err(|error| ApiError::internal(format!("cannot encode allowed file ids: {error}")))?;

    let share_id = ids::share_id();
    let now = timeutil::utc_now();
    sqlx::query(
        "insert into shares
            (id, user_id, session_id, scope, message_id, snapshot_json,
             allowed_file_ids_json, created_at)
         values ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(&share_id)
    .bind(&user_id)
    .bind(&session.id)
    .bind(scope.as_str())
    .bind(message_id.as_deref())
    .bind(&snapshot)
    .bind(&allowed_file_ids_json)
    .bind(&now)
    .execute(state.pool())
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "share": {
                "id": share_id,
                "scope": scope.as_str(),
                // The path that actually serves this payload, so a client that
                // prefixes its own origin gets something it can fetch. Clients
                // that render a page derive their own route from `id`; pointing
                // this at one would hand out a link this server does not serve.
                "urlPath": format!("/api/v2/public/shares/{share_id}"),
                "createdAt": now,
            },
            "serverTime": timeutil::utc_now(),
        })),
    ))
}

/// No [AuthedUser]: the whole point is that a visitor with only the link can read
/// it. The response is built exclusively from the stored snapshot, so the live
/// session (and any user, connector or workspace metadata) is unreachable here.
async fn public_share(
    State(state): State<AppState>,
    Path(share_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let row = live_share(state.pool(), &share_id)
        .await?
        .ok_or_else(|| ApiError::not_found("share not found"))?;
    let snapshot: Value = serde_json::from_str(&row.snapshot_json).map_err(|error| {
        tracing::error!(share_id = %row.id, error = %error, "corrupt share snapshot");
        ApiError::internal("invalid share snapshot")
    })?;
    Ok(Json(json!({
        "share": {
            "id": row.id,
            "scope": row.scope,
            "sessionTitle": snapshot
                .get("sessionTitle")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            "createdAt": row.created_at,
            "timeline": {
                "items": snapshot
                    .get("items")
                    .cloned()
                    .unwrap_or_else(|| json!([])),
            },
        },
        "serverTime": timeutil::utc_now(),
    })))
}

async fn delete_share(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Path((session_id, share_id)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    // Same 404-not-403 rule as creation: share ids are not probeable through
    // someone else's session namespace.
    owned_session(&state, &user_id, &session_id).await?;
    let now = timeutil::utc_now();
    let result = sqlx::query(
        "update shares
            set deleted_at = $1
          where id = $2 and session_id = $3 and user_id = $4 and deleted_at is null",
    )
    .bind(&now)
    .bind(&share_id)
    .bind(&session_id)
    .bind(&user_id)
    .execute(state.pool())
    .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("share not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------- snapshot

/// Copy the timeline — or the single requested item — into share items.
///
/// [crate::timeline::TimelineHub::snapshot] holds the same per-session engine
/// mutex that `write_message`/`apply_runtime_item` hold across "apply + persist",
/// so the copy is always a committed head: a share can never capture an item
/// that was staged but not yet written to `timeline_items`.
async fn capture_items(
    state: &AppState,
    session: &SessionRow,
    message_id: Option<&str>,
) -> ApiResult<(String, Vec<Value>)> {
    let (cursor, wire_items) = state
        .timelines()
        .snapshot(state.pool(), &session.id)
        .await?;
    let mut items: Vec<Value> = wire_items.iter().map(shared_item).collect();
    if let Some(message_id) = message_id {
        // The snapshot is per session, so "not in this timeline" is exactly
        // "does not exist or belongs to another session" — one 404 covers both.
        items.retain(|item| item.get("id").and_then(Value::as_str) == Some(message_id));
        if items.is_empty() {
            return Err(ApiError::not_found("timeline item not found"));
        }
    }
    Ok((cursor.to_wire(), items))
}

/// The only per-item fields a public share exposes.
///
/// A whitelist, not a strip-list: the live projection also carries
/// `sessionId`, `source`, `updatedSeq`, `revision` and `contentHash`, and any
/// field added to it later must be opted in here instead of leaking by default.
fn shared_item(item: &Value) -> Value {
    let mut out = Map::new();
    for key in ["id", "type", "status"] {
        out.insert(
            String::from(key),
            item.get(key).cloned().unwrap_or(Value::Null),
        );
    }
    // `role` is `skip_serializing_if = none` on the live projection; a share item
    // always carries the key so a public reader never has to guess.
    out.insert(
        String::from("role"),
        item.get("role").cloned().unwrap_or(Value::Null),
    );
    out.insert(
        String::from("content"),
        item.get("content").cloned().unwrap_or_else(|| json!({})),
    );
    out.insert(
        String::from("orderSeq"),
        item.get("orderSeq").cloned().unwrap_or(Value::Null),
    );
    // `timeline_items.item_time` stores exactly the committed `updatedAt`
    // (`timeline::persist_item`), and the ingress `itemTime` is deliberately
    // unused, so this is the truthful per-item time.
    out.insert(
        String::from("itemTime"),
        item.get("updatedAt").cloned().unwrap_or(Value::Null),
    );
    Value::Object(out)
}

async fn live_share(pool: &PgPool, share_id: &str) -> ApiResult<Option<ShareRow>> {
    Ok(sqlx::query_as::<_, ShareRow>(&format!(
        "select {SHARE_COLUMNS} from shares where id = $1 and deleted_at is null"
    ))
    .bind(share_id)
    .fetch_optional(pool)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live_item() -> Value {
        json!({
            "id": "itm_1",
            "sessionId": "sess_secret",
            "type": "message",
            "status": "done",
            "role": "assistant",
            "orderSeq": 3,
            "updatedSeq": 7,
            "revision": 2,
            "contentHash": "sha256:deadbeef",
            "content": { "text": "hello" },
            "createdAt": "2026-01-01T00:00:00.000000Z",
            "updatedAt": "2026-01-01T00:00:01.000000Z",
            "source": {
                "runtime": "codex",
                "sessionId": "sess_secret",
                "itemId": "itm_1"
            },
        })
    }

    #[test]
    fn a_shared_item_carries_only_the_public_whitelist() {
        let mut shared = shared_item(&live_item());
        let map = shared.as_object_mut().expect("object");
        let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["content", "id", "itemTime", "orderSeq", "role", "status", "type"],
            "the public projection must not grow a field by accident"
        );
        assert_eq!(shared["orderSeq"], 3);
        assert_eq!(shared["itemTime"], "2026-01-01T00:00:01.000000Z");
        assert_eq!(shared["content"]["text"], "hello");
        let text = shared.to_string();
        for forbidden in [
            "sessionId",
            "source",
            "runtime",
            "contentHash",
            "updatedSeq",
        ] {
            assert!(
                !text.contains(forbidden),
                "share item leaked {forbidden}: {text}"
            );
        }
    }

    #[test]
    fn an_item_without_a_role_still_carries_the_key() {
        let mut item = live_item();
        item.as_object_mut().expect("object").remove("role");
        assert_eq!(shared_item(&item)["role"], Value::Null);
    }

    #[test]
    fn scope_parsing_is_a_closed_set() {
        assert_eq!(Scope::parse("session").expect("session"), Scope::Session);
        assert_eq!(Scope::parse("message").expect("message"), Scope::Message);
        let error = Scope::parse("everything").expect_err("unknown scope");
        assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);
    }
}

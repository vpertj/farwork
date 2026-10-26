//! Runtime approvals relayed from the connector.
//!
//! The connector owns the agent's approval prompt; the hub owns durability and
//! fan-out. A notice lives in \`session_notices\` from \`notice.upsert\` until the
//! connector reports \`notice.resolved\`, so a browser that reloads mid-approval
//! still sees the pending prompt instead of losing the agent's turn.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;

use farwork_timeline::{EventEnvelope, EventType};

use crate::error::{ApiError, ApiResult};
use crate::ingress;
use crate::state::AppState;
use crate::timeline::wire_event;
use crate::timeutil;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/sessions/{session_id}/runtime/notices", get(list_notices))
        // The browser contract calls this one \`/sessions/{id}/notices/{id}/respond\`;
        // the runtime namespace spelling is kept as an alias because it is what the
        // 501 table advertised until this build implemented it.
        .route(
            "/sessions/{session_id}/notices/{notice_id}/respond",
            post(respond_notice),
        )
        .route(
            "/sessions/{session_id}/runtime/notices/{notice_id}/respond",
            post(respond_notice),
        )
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct NoticeRow {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub title: String,
    pub body_json: String,
    pub options_json: String,
    pub decision: Option<String>,
    pub interaction_type: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl NoticeRow {
    /// \`RuntimeNoticeView\`. \`decision\` is additive: the contract's notice shape
    /// has no field that says *how* a notice closed, and \`status\` alone would force
    /// the browser to re-fetch the whole list to tell approve from reject.
    pub fn view(&self) -> Value {
        json!({
            "id": self.id,
            "kind": self.kind,
            "status": self.status,
            "title": self.title,
            "body": parse_json(&self.body_json, Value::Null),
            // Passthrough, not a re-typing: rows written before the object form
            // still hold strings, and the browser reads both.
            "options": parse_json(&self.options_json, json!([])),
            "interactionType": self.interaction_type,
            "decision": self.decision,
            "createdAt": self.created_at,
            "updatedAt": self.updated_at,
        })
    }

    /// The action ids this notice offered. Legacy string rows parse as the old
    /// shape, so an answer can be checked against either generation.
    pub fn option_ids(&self) -> Vec<String> {
        serde_json::from_str::<Vec<ingress::NoticeOption>>(&self.options_json)
            .map(|options| {
                options
                    .into_iter()
                    .map(|option| option.id().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn parse_json(raw: &str, fallback: Value) -> Value {
    serde_json::from_str(raw).unwrap_or(fallback)
}

const NOTICE_COLUMNS: &str = "id, kind, status, title, body_json, options_json, decision,
     interaction_type, created_at, updated_at";

pub async fn fetch_notices(pool: &PgPool, session_id: &str) -> ApiResult<Vec<NoticeRow>> {
    Ok(sqlx::query_as::<_, NoticeRow>(&format!(
        "select {NOTICE_COLUMNS} from session_notices
          where session_id = $1
          order by created_at, id"
    ))
    .bind(session_id)
    .fetch_all(pool)
    .await?)
}

pub async fn fetch_notice(
    pool: &PgPool,
    session_id: &str,
    notice_id: &str,
) -> ApiResult<Option<NoticeRow>> {
    Ok(sqlx::query_as::<_, NoticeRow>(&format!(
        "select {NOTICE_COLUMNS} from session_notices where session_id = $1 and id = $2"
    ))
    .bind(session_id)
    .bind(notice_id)
    .fetch_optional(pool)
    .await?)
}

/// Persist a notice reported by the connector. Re-reporting the same id updates
/// the row: a connector is allowed to refine a title or options while the prompt
/// is still open.
pub async fn store_notice(
    state: &AppState,
    session_id: &str,
    notice: &crate::ingress::IngressNotice,
) -> ApiResult<NoticeRow> {
    if notice.id.trim().is_empty() {
        return Err(ApiError::unprocessable("notice.id must not be empty"));
    }
    let now = timeutil::utc_now();
    let status = notice
        .status
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("open");
    let title = notice.title.as_deref().unwrap_or_default();
    let body = serde_json::to_string(&notice.body.clone().unwrap_or(Value::Null))
        .map_err(|error| ApiError::internal(error.to_string()))?;
    // Stored in canonical object form: a legacy string option is upgraded here, so
    // the browser sees one shape regardless of which agent generation wrote it.
    let options = serde_json::to_string(&notice.options_wire())
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let interaction_type = notice
        .interaction_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    sqlx::query(
        "insert into session_notices
            (session_id, id, kind, status, title, body_json, options_json, decision,
             interaction_type, created_at, updated_at)
         values ($1, $2, $3, $4, $5, $6, $7, null, $8, $9, $9)
         on conflict (session_id, id) do update
            set kind = excluded.kind,
                status = excluded.status,
                title = excluded.title,
                body_json = excluded.body_json,
                options_json = excluded.options_json,
                interaction_type = excluded.interaction_type,
                updated_at = excluded.updated_at",
    )
    .bind(session_id)
    .bind(&notice.id)
    .bind(&notice.kind)
    .bind(status)
    .bind(title)
    .bind(&body)
    .bind(&options)
    .bind(interaction_type)
    .bind(&now)
    .execute(state.pool())
    .await?;
    fetch_notice(state.pool(), session_id, &notice.id)
        .await?
        .ok_or_else(|| ApiError::internal("notice vanished after write"))
}

/// Close a notice with the decision the connector actually applied. Scoped to the
/// connector that owns the session, because \`notice.resolved\` carries no session.
pub async fn resolve_notice(
    state: &AppState,
    connector_id: &str,
    notice_id: &str,
    decision: Option<&str>,
    action_id: Option<&str>,
) -> ApiResult<Option<(String, NoticeRow)>> {
    use sqlx::Row as _;
    // The row is read before the write: it holds the actions this notice actually
    // offered, which is what an actionId has to be checked against.
    let row = sqlx::query(
        "select n.session_id from session_notices n
           join sessions s on s.id = n.session_id
          where n.id = $2 and s.connector_id = $1",
    )
    .bind(connector_id)
    .bind(notice_id)
    .fetch_optional(state.pool())
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let session_id: String = row.get("session_id");
    let notice = fetch_notice(state.pool(), &session_id, notice_id)
        .await?
        .ok_or_else(|| ApiError::internal("notice vanished before resolve"))?;

    let recorded: Option<String> = match (decision, action_id) {
        (Some(decision), _) => {
            if !ingress::is_known_decision(decision) {
                return Err(ApiError::unprocessable(format!(
                    "decision must be one of {:?}",
                    ingress::DECISIONS
                )));
            }
            Some(decision.to_string())
        }
        (None, Some(action_id)) => {
            if !notice.option_ids().iter().any(|id| id == action_id) {
                return Err(ApiError::unprocessable(json!({
                    "code": "unknown_action",
                    "message": format!(
                        "actionId {action_id:?} is not offered by this notice; expected one of {:?}",
                        notice.option_ids()
                    ),
                })));
            }
            Some(action_id.to_string())
        }
        // Closed by the runtime or by another client: recorded as closed with no
        // answer, rather than inventing someone else's decision.
        (None, None) => None,
    };

    let updated = sqlx::query(
        "update session_notices
            set status = 'resolved', decision = $3, updated_at = $4
          where id = $2 and session_id = $1 and status = 'open'
      returning session_id",
    )
    .bind(&session_id)
    .bind(notice_id)
    .bind(recorded.as_deref())
    .bind(timeutil::utc_now())
    .fetch_optional(state.pool())
    .await?;
    if updated.is_none() {
        // Already resolved: never overwrite a real answer with a later one.
        return Ok(None);
    }
    let notice = fetch_notice(state.pool(), &session_id, notice_id)
        .await?
        .ok_or_else(|| ApiError::internal("notice vanished after resolve"))?;
    Ok(Some((session_id, notice)))
}

/// One \`runtime.notice.updated\` delta for the session stream.
pub async fn publish_notice_updated(
    state: &AppState,
    user_id: &str,
    session_id: &str,
    notice: &Value,
) -> ApiResult<()> {
    let event = event_at_head(
        state,
        session_id,
        EventType::RuntimeNoticeUpdated,
        json!({ "notice": notice }),
    )
    .await?;
    state
        .hub()
        .publish_json(user_id, Some(session_id), event.to_string());
    Ok(())
}

/// One \`runtime.notice.snapshot\` for the session stream: the whole list at the
/// current head, used whenever the set changed in a way a delta cannot express.
pub async fn publish_notice_snapshot(
    state: &AppState,
    user_id: &str,
    session_id: &str,
) -> ApiResult<()> {
    let notices: Vec<Value> = fetch_notices(state.pool(), session_id)
        .await?
        .iter()
        .map(NoticeRow::view)
        .collect();
    let event = event_at_head(
        state,
        session_id,
        EventType::RuntimeNoticeSnapshot,
        json!({ "notices": notices }),
    )
    .await?;
    state
        .hub()
        .publish_json(user_id, Some(session_id), event.to_string());
    Ok(())
}

async fn event_at_head(
    state: &AppState,
    session_id: &str,
    event_type: EventType,
    payload: Value,
) -> ApiResult<Value> {
    let head = state.timelines().head(state.pool(), session_id).await?;
    EventEnvelope::new(
        session_id,
        head.sequence(),
        event_type,
        payload,
        timeutil::utc_now(),
    )
    .map(|event| wire_event(&event))
    .map_err(|error| ApiError::internal(error.to_string()))
}

async fn list_notices(
    State(state): State<AppState>,
    crate::routes::extract::AuthedUser(user_id): crate::routes::extract::AuthedUser,
    Path(session_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let session = crate::routes::sessions::find_session(state.pool(), &session_id)
        .await?
        .filter(|row| row.user_id == user_id)
        .ok_or_else(|| ApiError::not_found("session not found"))?;
    let notices: Vec<Value> = fetch_notices(state.pool(), &session.id)
        .await?
        .iter()
        .map(NoticeRow::view)
        .collect();
    Ok(Json(json!({
        "notices": notices,
        "serverTime": timeutil::utc_now(),
    })))
}

#[derive(Debug, Deserialize)]
pub struct NoticeRespondRequest {
    /// The closed vocabulary (`approve` / `approve_for_session` / `reject`).
    #[serde(default)]
    pub decision: Option<String>,
    /// The runtime's own button id, which is how a DSH approval is answered.
    #[serde(default, rename = "actionId")]
    pub action_id: Option<String>,
    /// A question notice's structured answer; relayed verbatim as `inputData`.
    /// `inputData` is the field the browser sends, `input` is the agent-side
    /// spelling of the same value: both are accepted so neither side has to be
    /// renamed for the other.
    #[serde(default, alias = "inputData")]
    pub input: Option<Value>,
}

async fn respond_notice(
    State(state): State<AppState>,
    crate::routes::extract::AuthedUser(user_id): crate::routes::extract::AuthedUser,
    Path((session_id, notice_id)): Path<(String, String)>,
    Json(payload): Json<NoticeRespondRequest>,
) -> ApiResult<Json<Value>> {
    let session = crate::routes::sessions::find_session(state.pool(), &session_id)
        .await?
        .filter(|row| row.user_id == user_id)
        .ok_or_else(|| ApiError::not_found("session not found"))?;
    if session.archived {
        return Err(ApiError::conflict("session is archived"));
    }
    if !session.takeover {
        return Err(ApiError::conflict(
            "session is read-only until takeover is enabled",
        ));
    }
    let decision = payload
        .decision
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let action_id = payload
        .action_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    // The browser sends `actionId` and mirrors it into `decision` (so a hub that
    // only knows the old vocabulary still understands the answer). Agreement is
    // therefore the normal case, and only a *disagreement* is ambiguous. A pair
    // whose value is not a known decision is an action, not an error.
    let (decision, action_id) = match (decision, action_id) {
        (None, None) => {
            return Err(ApiError::unprocessable(json!({
                "code": "empty_answer",
                "message": "decision or actionId is required",
            })))
        }
        (Some(decision), Some(action_id)) if decision != action_id => {
            return Err(ApiError::unprocessable(json!({
                "code": "ambiguous_answer",
                "message": format!(
                    "decision {decision:?} and actionId {action_id:?} disagree; send one answer"
                ),
            })))
        }
        (Some(value), Some(_)) if ingress::is_known_decision(value) => (Some(value), None),
        (Some(value), Some(_)) => (None, Some(value)),
        (Some(decision), None) => {
            if !ingress::is_known_decision(decision) {
                return Err(ApiError::unprocessable(format!(
                    "decision must be one of {:?}",
                    ingress::DECISIONS
                )));
            }
            (Some(decision), None)
        }
        (None, Some(action_id)) => (None, Some(action_id)),
    };

    let notice = fetch_notice(state.pool(), &session_id, &notice_id)
        .await?
        .ok_or_else(|| {
            ApiError::conflict(json!({
                "code": "notice_not_found",
                "message": "unknown notice for this session",
            }))
        })?;
    if notice.status != "open" {
        return Err(ApiError::conflict(json!({
            "code": "notice_already_resolved",
            "message": format!("notice is already {}", notice.status),
        })));
    }

    // An actionId has to be one this notice actually offered: a stale browser tab
    // must not smuggle in a button the runtime never published, and a typo has to
    // be a 422 rather than silently dropping the agent's prompt.
    if let Some(action_id) = action_id {
        let offered = notice.option_ids();
        if !offered.iter().any(|id| id == action_id) {
            return Err(ApiError::unprocessable(json!({
                "code": "unknown_action",
                "message": format!(
                    "actionId {action_id:?} is not offered by this notice; expected one of {offered:?}"
                ),
            })));
        }
    }

    let frame =
        ingress::notice_respond_frame(&notice.id, decision, action_id, payload.input.as_ref());
    let delivered = state
        .connectors()
        .get(&session.connector_id)
        .map(|handle| handle.send(&frame))
        .unwrap_or(false);
    if !delivered {
        // The decision was not delivered, so the notice stays open: reporting
        // success here would strand the agent on a prompt nobody will answer.
        return Err(ApiError::conflict(json!({
            "code": "connector_offline",
            "message": "connector is not connected; the decision was not delivered",
        })));
    }

    Ok(Json(json!({
        "ok": true,
        // Still open on the hub: the connector confirms with `"notice.resolved"`.
        "notice": notice.view(),
        "serverTime": timeutil::utc_now(),
    })))
}

//! \`/sessions/*\` — the session aggregate: inventory, snapshot, timeline,
//! revision catch-up, runtime state/capabilities and message intake.
//!
//! Timeline semantics come from `farwork-timeline`; `crate::timeline` is only the
//! persistence/wire glue around it.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;

use farwork_timeline::{Cursor, Recovery};

use crate::error::{ApiError, ApiResult};
use crate::ids;
use crate::models::{RuntimeStateRow, SessionRow};
use crate::routes::extract::AuthedUser;
use crate::state::AppState;
use crate::timeline::WriteOutcome;
use crate::timeutil;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/sessions", get(list_sessions).post(create_session))
        .route("/sessions/list", get(list_session_inventory))
        .route("/sessions/create-and-start", post(create_and_start_session))
        .route(
            "/sessions/{session_id}/meta",
            get(get_session_meta).patch(patch_session_meta),
        )
        .route("/sessions/{session_id}/timeline", get(session_timeline))
        .route("/sessions/{session_id}/snapshot", get(session_snapshot))
        .route("/sessions/{session_id}/events", get(session_events))
        .route(
            "/sessions/{session_id}/runtime/state",
            get(session_runtime_state),
        )
        .route(
            "/sessions/{session_id}/runtime/capabilities",
            get(session_runtime_capabilities),
        )
        .route(
            "/sessions/{session_id}/runtime/messages",
            post(send_message),
        )
        .route(
            "/sessions/{session_id}/runtime/interrupt",
            post(interrupt_session),
        )
}

/// Runtime names advertised by upstream protocol 1.0.
const RUNTIMES: [&str; 5] = ["codex", "claude", "opencode", "acp", "dsh"];

/// Comma-separated extra runtime names appended to the allowlist. Exists so a
/// real hub binary can be smoke-tested end to end against a runtime that is not
/// part of the shipped protocol set (e.g. `--runtime echo`). Dev/test only.
pub const DEV_RUNTIMES_ENV: &str = "FARWORK_HUB_DEV_RUNTIMES";

pub const SESSION_COLUMNS: &str =
    "id, user_id, connector_id, project_id, runtime, runtime_id, origin,
            external_session_id, title, title_source, cwd, status, takeover, pinned, pinned_at,
            archived, archived_at, source_state, last_read_seq, latest_turn_end_seq,
            last_synced_at, source_observed_at, last_activity_at, sort_at, seq,
            seq_allocated_high, updated_seq, created_at, updated_at";

const RUNTIME_STATE_COLUMNS: &str = "session_id, runtime, external_session_id, status,
            selections_json, status_reason, error_json, metadata_json, updated_seq,
            created_at, updated_at";

fn parse_runtime_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

fn dev_extra_runtimes() -> Vec<String> {
    parse_runtime_list(&std::env::var(DEV_RUNTIMES_ENV).unwrap_or_default())
}

/// The effective allowlist: the protocol set plus whatever dev-only extras are
/// live. Parameterized so tests never have to touch process env.
fn allowed_runtimes(extra: &[String]) -> Vec<String> {
    let mut allowed: Vec<String> = RUNTIMES.iter().map(|name| (*name).to_string()).collect();
    for name in extra {
        if !allowed.iter().any(|known| known == name) {
            allowed.push(name.clone());
        }
    }
    allowed
}

fn runtime_allowed(runtime: &str, extra: &[String]) -> bool {
    allowed_runtimes(extra).iter().any(|name| name == runtime)
}

fn validate_runtime(runtime: &str) -> ApiResult<()> {
    if runtime_allowed(runtime, &dev_extra_runtimes()) {
        Ok(())
    } else {
        Err(ApiError::unprocessable(format!(
            "unknown runtime {runtime:?}"
        )))
    }
}

/// One WARN at startup when the dev escape hatch is armed, so an operator can
/// never enable extra runtimes silently.
pub fn warn_if_dev_runtimes_enabled() {
    let extra = dev_extra_runtimes();
    if !extra.is_empty() {
        tracing::warn!(
            "dev-only runtimes enabled: {} (never set {DEV_RUNTIMES_ENV} in production)",
            extra.join(",")
        );
    }
}

// ---------------------------------------------------------------- cursor

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCursor {
    pub pinned: bool,
    pub sort_at: String,
    pub updated_seq: i64,
    pub id: String,
}

/// Opaque cursor: base64url of the sort key tuple. Only ever produced here.
pub fn encode_cursor(row: &SessionRow) -> String {
    let cursor = SessionCursor {
        pinned: row.pinned,
        sort_at: row.sort_at.clone(),
        updated_seq: row.updated_seq,
        id: row.id.clone(),
    };
    let raw = serde_json::to_vec(&cursor).unwrap_or_default();
    crate::auth::b64url_encode(&raw)
}

pub fn decode_cursor(raw: &str) -> ApiResult<SessionCursor> {
    let bytes =
        crate::auth::b64url_decode(raw).ok_or_else(|| ApiError::bad_request("invalid cursor"))?;
    serde_json::from_slice::<SessionCursor>(&bytes)
        .map_err(|_| ApiError::bad_request("invalid cursor"))
}

// ------------------------------------------------------------- lookups

pub async fn find_session(
    pool: &PgPool,
    session_id: &str,
) -> Result<Option<SessionRow>, sqlx::Error> {
    sqlx::query_as::<_, SessionRow>(&format!(
        "select {SESSION_COLUMNS} from sessions where id = $1"
    ))
    .bind(session_id)
    .fetch_optional(pool)
    .await
}

/// Session lookup that also enforces ownership: someone else's session id is a
/// 404, so a caller cannot probe for sessions it does not own.
pub(crate) async fn owned_session(
    state: &AppState,
    user_id: &str,
    session_id: &str,
) -> ApiResult<SessionRow> {
    find_session(state.pool(), session_id)
        .await?
        .filter(|row| row.user_id == user_id)
        .ok_or_else(|| ApiError::not_found("session not found"))
}

/// Connector presence is read from the connector rows. A connector with no live
/// ingress socket is \`offline\` because the ingress wrote that; nothing here invents a
/// status.
pub async fn connector_status_map(
    state: &AppState,
    user_id: &str,
) -> ApiResult<HashMap<String, String>> {
    use sqlx::Row as _;
    let rows = sqlx::query("select id, status from connectors where user_id = $1")
        .bind(user_id)
        .fetch_all(state.pool())
        .await?;
    Ok(rows
        .into_iter()
        .map(|row| (row.get::<String, _>("id"), row.get::<String, _>("status")))
        .collect())
}

fn session_view(row: &SessionRow, statuses: &HashMap<String, String>) -> Value {
    let status = statuses
        .get(&row.connector_id)
        .map(|value| value.as_str())
        .unwrap_or("offline");
    row.view(status)
}

/// Shared page reader for \`GET /sessions\` and \`GET /projects/{id}/sessions\`.
pub async fn list_session_rows(
    state: &AppState,
    user_id: &str,
    archived: bool,
    limit: i64,
    cursor: Option<SessionCursor>,
    project_id: Option<&str>,
) -> ApiResult<(Vec<Value>, bool, Option<String>)> {
    let limit = limit.clamp(1, 100);
    let cursor = cursor.unwrap_or(SessionCursor {
        pinned: false,
        sort_at: String::new(),
        updated_seq: 0,
        id: String::new(),
    });
    let has_cursor = !cursor.sort_at.is_empty();

    let statement = format!(
        "select {SESSION_COLUMNS} from sessions
          where user_id = $1
            and archived = $2
            and ($3::text is null or project_id = $3)
            and (
                $4::boolean = false
                or (pinned, sort_at, updated_seq, id) < ($5, $6, $7, $8)
            )
          order by pinned desc, sort_at desc, updated_seq desc, id desc
          limit $9"
    );
    let rows = sqlx::query_as::<_, SessionRow>(&statement)
        .bind(user_id)
        .bind(archived)
        .bind(project_id)
        .bind(has_cursor)
        .bind(cursor.pinned)
        .bind(&cursor.sort_at)
        .bind(cursor.updated_seq)
        .bind(&cursor.id)
        .bind(limit + 1)
        .fetch_all(state.pool())
        .await?;

    let statuses = connector_status_map(state, user_id).await?;
    let has_more = rows.len() as i64 > limit;
    let page: Vec<SessionRow> = rows.into_iter().take(limit as usize).collect();
    let next_cursor = if has_more {
        page.last().map(encode_cursor)
    } else {
        None
    };
    let views = page
        .iter()
        .map(|row| session_view(row, &statuses))
        .collect();
    Ok((views, has_more, next_cursor))
}

#[derive(Debug, Deserialize)]
pub struct SessionsQuery {
    #[serde(default)]
    pub archived: Option<bool>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SessionCreateRequest {
    #[serde(default, rename = "connectorId")]
    pub connector_id: String,
    #[serde(default, rename = "projectId")]
    pub project_id: String,
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default, rename = "runtimeId")]
    pub runtime_id: Option<String>,
    #[serde(default, rename = "externalSessionId")]
    pub external_session_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SessionCreateAndStartRequest {
    #[serde(default, rename = "connectorId")]
    pub connector_id: String,
    #[serde(default, rename = "projectId")]
    pub project_id: String,
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default, rename = "runtimeId")]
    pub runtime_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub content: String,
    #[serde(default, rename = "clientMessageId")]
    pub client_message_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SessionPatchRequest {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub pinned: Option<bool>,
    #[serde(default)]
    pub archived: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct TimelineQuery {
    #[serde(default, rename = "afterSeq")]
    pub after_seq: Option<i64>,
    #[serde(default, rename = "beforeOrderSeq")]
    pub before_order_seq: Option<i64>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    #[serde(default)]
    pub after: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MessageCreateRequest {
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub attachments: Option<Vec<Value>>,
    #[serde(default, rename = "clientMessageId")]
    pub client_message_id: Option<String>,
}

// --------------------------------------------------------- create paths

struct Binding {
    connector_id: String,
    project_id: String,
    cwd: String,
}

/// Validate the connector/project/workspace triple the way upstream
/// \`_validate_project_binding\` does: both must be owned, must agree on the
/// connector, and a supplied cwd must resolve to the project workspace.
async fn resolve_binding(
    state: &AppState,
    user_id: &str,
    connector_id: &str,
    project_id: &str,
    cwd: Option<&str>,
) -> ApiResult<Binding> {
    let connector = crate::routes::connectors::find_connector(state.pool(), connector_id)
        .await?
        .filter(|row| row.user_id == user_id && !row.revoked)
        .ok_or_else(|| ApiError::not_found("connector not found"))?;
    let project = crate::routes::projects::find_project(state.pool(), project_id)
        .await?
        .filter(|row| row.user_id == user_id)
        .ok_or_else(|| ApiError::not_found("project not found"))?;
    if project.connector_id != connector.id {
        return Err(ApiError::unprocessable(
            "project connector and workspace must match the session",
        ));
    }
    let resolved_cwd = match cwd.map(str::trim).filter(|value| !value.is_empty()) {
        Some(cwd) => {
            if crate::routes::projects::workspace_key(cwd) != project.workspace_key {
                return Err(ApiError::unprocessable(
                    "project connector and workspace must match the session",
                ));
            }
            project.workspace_path.clone()
        }
        None => project.workspace_path.clone(),
    };
    Ok(Binding {
        connector_id: connector.id,
        project_id: project.id,
        cwd: resolved_cwd,
    })
}

struct NewSession<'a> {
    connector_id: &'a str,
    project_id: &'a str,
    runtime: &'a str,
    runtime_id: &'a str,
    external_session_id: Option<&'a str>,
    title: Option<&'a str>,
    cwd: &'a str,
    takeover: bool,
}

async fn insert_session(
    state: &AppState,
    user_id: &str,
    params: NewSession<'_>,
) -> ApiResult<SessionRow> {
    let now = timeutil::utc_now();
    let session_id = ids::session_id();
    let title = params
        .title
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());

    let mut tx = state.pool().begin().await?;
    let session = sqlx::query_as::<_, SessionRow>(&format!(
        "insert into sessions
            (id, user_id, connector_id, project_id, runtime, runtime_id, origin,
             external_session_id, title, title_source, cwd, status, takeover, sort_at,
             seq, seq_allocated_high, updated_seq, last_activity_at, created_at, updated_at)
         values ($1, $2, $3, $4, $5, $6, 'platform', $7, $8, $9, $10, 'idle', $11, $12,
                 0, 0, 0, $12, $12, $12)
         returning {SESSION_COLUMNS}"
    ))
    .bind(&session_id)
    .bind(user_id)
    .bind(params.connector_id)
    .bind(params.project_id)
    .bind(params.runtime)
    .bind(params.runtime_id)
    .bind(params.external_session_id)
    .bind(title)
    .bind(title.map(|_| "client"))
    .bind(params.cwd)
    .bind(params.takeover)
    .bind(&now)
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query(
        "insert into session_runtime_states
            (session_id, runtime, external_session_id, status, selections_json,
             metadata_json, updated_seq, created_at, updated_at)
         values ($1, $2, $3, 'idle', '{}', '{}', 0, $4, $4)",
    )
    .bind(&session.id)
    .bind(params.runtime)
    .bind(params.external_session_id)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(session)
}

async fn create_session(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Json(payload): Json<SessionCreateRequest>,
) -> ApiResult<Json<Value>> {
    let external_session_id = payload
        .external_session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ApiError::unprocessable("new sessions must use /sessions/create-and-start")
        })?;
    let runtime = payload
        .runtime
        .clone()
        .unwrap_or_else(|| String::from("codex"));
    validate_runtime(&runtime)?;
    let binding = resolve_binding(
        &state,
        &user_id,
        &payload.connector_id,
        &payload.project_id,
        payload.cwd.as_deref(),
    )
    .await?;
    let runtime_id = payload
        .runtime_id
        .clone()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| runtime.clone());

    // Imported sessions are read-only until a takeover is granted; the hub
    // cannot grant one yet, so takeover stays false exactly like upstream.
    let session = insert_session(
        &state,
        &user_id,
        NewSession {
            connector_id: &binding.connector_id,
            project_id: &binding.project_id,
            runtime: &runtime,
            runtime_id: &runtime_id,
            external_session_id: Some(external_session_id),
            title: payload.title.as_deref(),
            cwd: &binding.cwd,
            takeover: false,
        },
    )
    .await?;

    crate::routes::projects::publish_dashboard_change(&state, &user_id);
    let statuses = connector_status_map(&state, &user_id).await?;
    Ok(Json(json!({
        "session": session_view(&session, &statuses),
        "connectorResult": null,
    })))
}

async fn create_and_start_session(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Json(payload): Json<SessionCreateAndStartRequest>,
) -> ApiResult<Json<Value>> {
    if payload.content.trim().is_empty() {
        return Err(ApiError::unprocessable("content must not be empty"));
    }
    let runtime = payload
        .runtime
        .clone()
        .unwrap_or_else(|| String::from("codex"));
    validate_runtime(&runtime)?;
    let binding = resolve_binding(
        &state,
        &user_id,
        &payload.connector_id,
        &payload.project_id,
        payload.cwd.as_deref(),
    )
    .await?;
    let runtime_id = payload
        .runtime_id
        .clone()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| runtime.clone());

    let session = insert_session(
        &state,
        &user_id,
        NewSession {
            connector_id: &binding.connector_id,
            project_id: &binding.project_id,
            runtime: &runtime,
            runtime_id: &runtime_id,
            external_session_id: None,
            title: payload.title.as_deref(),
            cwd: &binding.cwd,
            // Platform-created sessions are writable, like upstream.
            takeover: true,
        },
    )
    .await?;

    let written = state
        .timelines()
        .write_message(
            state.pool(),
            &session,
            payload.content.trim(),
            payload.client_message_id.as_deref(),
        )
        .await?;
    let item = match &written {
        WriteOutcome::Committed { item, .. } | WriteOutcome::Unchanged { item } => item.clone(),
    };

    // The first user message reaches the agent through `session.start`, not
    // `session.message`: the runtime has no stream to address yet.
    let stream_id =
        dispatch_session_start(&state, &session, &binding.cwd, payload.content.trim()).await;
    let dispatched = stream_id.is_some();

    crate::routes::projects::publish_dashboard_change(&state, &user_id);
    // Re-read: the message commit advanced the session's seq, and the response
    // must not report the pre-write projection.
    let session = find_session(state.pool(), &session.id)
        .await?
        .unwrap_or(session);
    let statuses = connector_status_map(&state, &user_id).await?;
    Ok(Json(json!({
        "session": session_view(&session, &statuses),
        // Never report a start that did not happen: the session and its first
        // message are durable either way, but only a delivered `session.start`
        // counts as a dispatched runtime.
        "connectorResult": match &stream_id {
            Some(stream_id) => json!({
                "ok": true,
                "streamId": stream_id,
                "sessionId": session.id,
            }),
            None => json!({
                "ok": false,
                "code": "connector_offline",
                "message": "the connector is not connected; the platform session and its \
    first message are durable, runtime dispatch is pending",
                "sessionId": session.id,
            }),
        },
        "runtimeDispatched": dispatched,
        "eventCursor": Cursor::new(item.updated_seq)
            .map_err(|error| ApiError::internal(error.to_string()))?
            .to_wire(),
    })))
}

/// Push `session.start` to the session's connector and remember the stream it
/// drives. `None` means nothing was dispatched — no connector is connected, or the
/// socket closed mid-send — and the caller must say so instead of faking a start.
async fn dispatch_session_start(
    state: &AppState,
    session: &SessionRow,
    cwd: &str,
    prompt: &str,
) -> Option<String> {
    let handle = state.connectors().get(&session.connector_id)?;
    let stream_id = ids::stream_id();
    if let Err(error) = crate::ingress::open_stream(state, session, &stream_id).await {
        tracing::warn!(
            session_id = %session.id,
            detail = ?error.detail,
            "cannot record connector stream"
        );
        return None;
    }
    let frame =
        crate::ingress::session_start_frame(&stream_id, &session.id, &session.runtime, cwd, prompt);
    if handle.send(&frame) {
        Some(stream_id)
    } else {
        let _ = crate::ingress::close_stream(state, &stream_id).await;
        None
    }
}

// ------------------------------------------------------------- handlers

async fn list_sessions(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Query(query): Query<SessionsQuery>,
) -> ApiResult<Json<Value>> {
    let archived = query.archived.unwrap_or(false);
    let limit = query.limit.unwrap_or(100).clamp(1, 100);
    let cursor = query.cursor.as_deref().map(decode_cursor).transpose()?;
    let (sessions, has_more, next_cursor) =
        list_session_rows(&state, &user_id, archived, limit, cursor, None).await?;
    Ok(Json(json!({
        "sessions": sessions,
        "hasMore": has_more,
        "nextCursor": next_cursor,
        "serverTime": timeutil::utc_now(),
    })))
}

async fn list_session_inventory(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Query(query): Query<SessionsQuery>,
) -> ApiResult<Json<Value>> {
    // Upstream \`/sessions/list\` is the unpaginated dashboard inventory; the P0
    // hub caps the row count so one account cannot pull an unbounded response.
    let limit = query.limit.unwrap_or(100).clamp(1, 100);
    let statuses = connector_status_map(&state, &user_id).await?;
    let rows = sqlx::query_as::<_, SessionRow>(&format!(
        "select {SESSION_COLUMNS} from sessions
          where user_id = $1
          order by pinned desc, sort_at desc, updated_seq desc, id desc
          limit $2"
    ))
    .bind(&user_id)
    .bind(limit)
    .fetch_all(state.pool())
    .await?;
    Ok(Json(json!({
        "sessions": rows
            .iter()
            .map(|row| session_view(row, &statuses))
            .collect::<Vec<_>>(),
        "serverTime": timeutil::utc_now(),
    })))
}

async fn get_session_meta(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Path(session_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let session = owned_session(&state, &user_id, &session_id).await?;
    let statuses = connector_status_map(&state, &user_id).await?;
    Ok(Json(json!({
        "session": session_view(&session, &statuses),
        "serverTime": timeutil::utc_now(),
    })))
}

async fn patch_session_meta(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Path(session_id): Path<String>,
    Json(payload): Json<SessionPatchRequest>,
) -> ApiResult<Json<Value>> {
    let session = owned_session(&state, &user_id, &session_id).await?;
    if payload.title.is_none() && payload.pinned.is_none() && payload.archived.is_none() {
        return Err(ApiError::unprocessable(
            "at least one session field is required",
        ));
    }
    if let Some(title) = payload.title.as_deref() {
        let trimmed = title.trim();
        if trimmed.is_empty() || trimmed.chars().count() > 255 {
            return Err(ApiError::unprocessable("title must be 1-255 characters"));
        }
    }

    let now = timeutil::utc_now();
    let archived_at = match payload.archived {
        Some(true) => Some(now.clone()),
        Some(false) => None,
        None => session.archived_at.clone(),
    };
    let pinned_at = match payload.pinned {
        Some(true) => session.pinned_at.clone().or_else(|| Some(now.clone())),
        Some(false) => None,
        None => session.pinned_at.clone(),
    };
    sqlx::query(
        "update sessions
            set title = coalesce($2, title),
                pinned = coalesce($3, pinned),
                pinned_at = case when $3 is null then pinned_at else $4 end,
                archived = coalesce($5, archived),
                archived_at = case when $5 is null then archived_at else $6 end,
                updated_at = $7
          where id = $1",
    )
    .bind(&session_id)
    .bind(payload.title.as_deref().map(str::trim))
    .bind(payload.pinned)
    .bind(pinned_at.as_deref())
    .bind(payload.archived)
    .bind(archived_at.as_deref())
    .bind(&now)
    .execute(state.pool())
    .await?;

    let updated = owned_session(&state, &user_id, &session_id).await?;
    let statuses = connector_status_map(&state, &user_id).await?;
    let view = session_view(&updated, &statuses);
    let event = state
        .timelines()
        .meta_event(state.pool(), &session_id, &view)
        .await?;
    state
        .hub()
        .publish_json(&user_id, Some(&session_id), event.to_string());
    crate::routes::projects::publish_dashboard_change(&state, &user_id);
    Ok(Json(json!({
        "session": view,
        "serverTime": timeutil::utc_now(),
    })))
}

async fn session_timeline(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Path(session_id): Path<String>,
    Query(query): Query<TimelineQuery>,
) -> ApiResult<Json<Value>> {
    owned_session(&state, &user_id, &session_id).await?;
    let mode = query.mode.unwrap_or_else(|| String::from("latest"));
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let hub = state.timelines();
    let (cursor, items, has_more) = match mode.as_str() {
        "latest" => hub.items_latest(state.pool(), &session_id, limit).await?,
        "changes" => {
            hub.items_since(
                state.pool(),
                &session_id,
                query.after_seq.unwrap_or(0),
                limit,
            )
            .await?
        }
        "history" => {
            let before = query.before_order_seq.ok_or_else(|| {
                ApiError::unprocessable("beforeOrderSeq is required for history mode")
            })?;
            hub.items_before_order_seq(state.pool(), &session_id, before, limit)
                .await?
        }
        _ => return Err(ApiError::unprocessable("invalid timeline mode")),
    };
    Ok(Json(json!({
        "sessionId": session_id,
        "items": items,
        "nextSeq": cursor.sequence(),
        "hasMore": has_more,
        "serverTime": timeutil::utc_now(),
    })))
}

async fn session_snapshot(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Path(session_id): Path<String>,
    Query(query): Query<TimelineQuery>,
) -> ApiResult<Json<Value>> {
    let session = owned_session(&state, &user_id, &session_id).await?;
    let statuses = connector_status_map(&state, &user_id).await?;
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let (cursor, items, has_more) = state
        .timelines()
        .items_latest(state.pool(), &session_id, limit)
        .await?;
    let state_row = runtime_state_row(state.pool(), &session_id).await?;
    let notices: Vec<Value> = crate::routes::notices::fetch_notices(state.pool(), &session_id)
        .await?
        .iter()
        .map(crate::routes::notices::NoticeRow::view)
        .collect();
    let next_seq = cursor.sequence();
    Ok(Json(json!({
        "session": session_view(&session, &statuses),
        "state": state_row.map(|row| row.view()),
        "timeline": {
            "items": items,
            "nextSeq": next_seq,
            "hasMore": has_more,
        },
        // Approvals/notices/catalogs are runtime-owned; with no connector
        // ingress the truthful answer is empty rather than fabricated content.
        "approvals": [],
        "notices": notices,
        "effectiveCapabilities": { "revision": 0, "capabilities": [] },
        "runtimeCapabilities": { "revision": 0, "capabilities": [] },
        "catalogs": {},
        "eventCursor": cursor.to_wire(),
        "serverTime": timeutil::utc_now(),
    })))
}

pub async fn runtime_state_row(
    pool: &PgPool,
    session_id: &str,
) -> ApiResult<Option<RuntimeStateRow>> {
    Ok(sqlx::query_as::<_, RuntimeStateRow>(&format!(
        "select {RUNTIME_STATE_COLUMNS} from session_runtime_states where session_id = $1"
    ))
    .bind(session_id)
    .fetch_optional(pool)
    .await?)
}

async fn session_events(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Path(session_id): Path<String>,
    Query(query): Query<EventsQuery>,
) -> ApiResult<Json<Value>> {
    owned_session(&state, &user_id, &session_id).await?;
    let after = query.after.unwrap_or_else(|| String::from("seq:0"));
    // An unparseable cursor cannot be reconciled incrementally either: answer
    // with the same explicit refetch signal instead of a 422 the client would
    // have to special-case.
    let cursor = match Cursor::parse(&after) {
        Ok(cursor) => cursor,
        Err(_) => {
            let head = state.timelines().head(state.pool(), &session_id).await?;
            return Ok(Json(json!({
                "events": [],
                "nextCursor": head.to_wire(),
                "snapshotRequired": true,
                "serverTime": timeutil::utc_now(),
            })));
        }
    };

    // The domain crate decides whether a hole-free suffix exists; when it does
    // not we answer `snapshotRequired` instead of an empty "you are current".
    let recovery = state
        .timelines()
        .recover(state.pool(), &session_id, cursor)
        .await?;
    let (events, next_cursor, snapshot_required) = match recovery {
        Recovery::Events { events, next } => (
            events
                .iter()
                .map(crate::timeline::wire_event)
                .collect::<Vec<_>>(),
            next.to_wire(),
            false,
        ),
        Recovery::RefetchRequired { reason, next } => {
            tracing::debug!(
                session_id = %session_id,
                reason = reason.code(),
                "timeline recovery requires a snapshot"
            );
            (Vec::new(), next.to_wire(), true)
        }
    };
    Ok(Json(json!({
        "events": events,
        "nextCursor": next_cursor,
        "snapshotRequired": snapshot_required,
        "serverTime": timeutil::utc_now(),
    })))
}

async fn session_runtime_state(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Path(session_id): Path<String>,
) -> ApiResult<Json<Value>> {
    owned_session(&state, &user_id, &session_id).await?;
    let state_row = runtime_state_row(state.pool(), &session_id)
        .await?
        .ok_or_else(|| ApiError::not_found("session runtime state not found"))?;
    Ok(Json(json!({
        "state": state_row.view(),
        "serverTime": timeutil::utc_now(),
    })))
}

async fn session_runtime_capabilities(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Path(session_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let session = owned_session(&state, &user_id, &session_id).await?;
    // The agent's own report, cached while its stream is alive. A session that
    // never reported (or whose runtime is gone) reads all-false: the truth is
    // "nothing is supported", not "everything is".
    Ok(Json(crate::routes::runtime::capabilities_view(
        &state,
        &session_id,
        &session.connector_id,
    )))
}

async fn send_message(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Path(session_id): Path<String>,
    Json(payload): Json<MessageCreateRequest>,
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
    let content = payload.content.trim();
    if content.is_empty() {
        return Err(ApiError::unprocessable("content must not be empty"));
    }
    if let Some(attachments) = payload.attachments.as_ref() {
        if !attachments.is_empty() {
            // Attachment storage is not part of P0; refusing beats silently
            // dropping the user's files.
            return Err(ApiError::not_implemented(
                "attachments are not implemented in this build",
            ));
        }
    }

    let client_message_id = payload
        .client_message_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let written = state
        .timelines()
        .write_message(state.pool(), &session, content, client_message_id)
        .await?;

    let (item, duplicate, event) = match written {
        WriteOutcome::Committed { item, event } => (item, false, Some(event)),
        WriteOutcome::Unchanged { item } => (item, true, None),
    };
    if let Some(event) = event {
        state
            .hub()
            .publish_json(&user_id, Some(&session_id), event.to_string());
        crate::routes::projects::publish_dashboard_change(&state, &user_id);
    }
    // Dispatch after the write committed, and never for a duplicate: replaying a
    // `clientMessageId` must not run the same turn on the agent twice.
    let mut dispatched = false;
    if !duplicate {
        match crate::ingress::active_stream(&state, &session_id).await? {
            Some(stream_id) => {
                if let Some(handle) = state.connectors().get(&session.connector_id) {
                    let frame = crate::ingress::session_message_frame(
                        &stream_id,
                        content,
                        client_message_id,
                    );
                    dispatched = handle.send(&frame);
                    if !dispatched {
                        tracing::warn!(
                            session_id = %session_id,
                            "connector socket closed during session.message dispatch"
                        );
                    }
                }
            }
            None => tracing::debug!(
                session_id = %session_id,
                "no runtime stream for this session; message stored without dispatch"
            ),
        }
    }

    let cursor =
        Cursor::new(item.updated_seq).map_err(|error| ApiError::internal(error.to_string()))?;
    let source =
        crate::timeline::item_source(&session.runtime, &session_id, &item.id, client_message_id);
    Ok(Json(json!({
        "ok": true,
        "result": {
            "item": crate::timeline::wire_item(&item, &source),
            "eventCursor": cursor.to_wire(),
            "clientMessageId": client_message_id,
            "duplicate": duplicate,
            "runtimeDispatched": dispatched,
        },
    })))
}

/// `POST /sessions/{id}/runtime/interrupt` — stop the turn the connector is running.
///
/// Refuses with 409 rather than answering 200 when there is nothing to interrupt:
/// a client that got 200 would believe the agent stopped.
async fn interrupt_session(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Path(session_id): Path<String>,
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
    let stream_id = crate::ingress::active_stream(&state, &session_id)
        .await?
        .ok_or_else(|| {
            ApiError::conflict(json!({
                "code": "session_not_running",
                "message": "no active runtime stream for this session",
            }))
        })?;
    let handle = state
        .connectors()
        .get(&session.connector_id)
        .ok_or_else(|| {
            ApiError::conflict(json!({
                "code": "connector_offline",
                "message": "connector is not connected; the interrupt was not delivered",
            }))
        })?;
    if !handle.send(&crate::ingress::session_interrupt_frame(&stream_id)) {
        return Err(ApiError::conflict(json!({
            "code": "connector_offline",
            "message": "connector socket closed; the interrupt was not delivered",
        })));
    }
    Ok(Json(json!({
        "ok": true,
        "sessionId": session_id,
        "streamId": stream_id,
        "serverTime": timeutil::utc_now(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_round_trips_through_base64() {
        let row = SessionRow {
            id: String::from("sess_1"),
            user_id: String::from("usr_1"),
            connector_id: String::from("conn_1"),
            project_id: None,
            runtime: String::from("codex"),
            runtime_id: String::from("codex"),
            origin: String::from("platform"),
            external_session_id: None,
            title: None,
            title_source: None,
            cwd: None,
            status: String::from("idle"),
            takeover: true,
            pinned: false,
            pinned_at: None,
            archived: false,
            archived_at: None,
            source_state: String::from("visible"),
            last_read_seq: 0,
            latest_turn_end_seq: 0,
            last_synced_at: None,
            source_observed_at: None,
            last_activity_at: None,
            sort_at: String::from("2025-01-01T00:00:00.000000Z"),
            seq: 0,
            seq_allocated_high: 0,
            updated_seq: 0,
            created_at: String::from("2025-01-01T00:00:00.000000Z"),
            updated_at: String::from("2025-01-01T00:00:00.000000Z"),
        };
        let encoded = encode_cursor(&row);
        let decoded = decode_cursor(&encoded).expect("decodes");
        assert_eq!(decoded.id, "sess_1");
        assert_eq!(decoded.sort_at, row.sort_at);
        assert!(decode_cursor("not-a-cursor").is_err());
    }

    #[test]
    fn runtime_validation_matches_protocol_names() {
        let none: Vec<String> = Vec::new();
        assert!(runtime_allowed("codex", &none));
        assert!(runtime_allowed("dsh", &none));
        assert!(!runtime_allowed("gpt", &none));
    }

    #[test]
    fn default_allowed_runtimes_exclude_echo() {
        let allowed = allowed_runtimes(&[]);
        assert_eq!(allowed, RUNTIMES.map(str::to_string).to_vec());
        assert!(!allowed.contains(&String::from("echo")));
        assert!(!runtime_allowed("echo", &[]));
    }

    #[test]
    fn dev_runtimes_are_appended_to_the_allowlist() {
        let extra = vec![String::from("echo")];
        let allowed = allowed_runtimes(&extra);
        assert!(allowed.contains(&String::from("echo")));
        assert!(runtime_allowed("echo", &extra));
        assert_eq!(allowed.len(), RUNTIMES.len() + 1);
        // A dev extra must never shrink the protocol set or duplicate a name.
        for name in RUNTIMES {
            assert!(allowed.contains(&String::from(name)));
        }
        let idempotent = allowed_runtimes(&[String::from("codex"), String::from("echo")]);
        assert_eq!(idempotent.len(), RUNTIMES.len() + 1);
    }

    #[test]
    fn dev_runtime_env_list_is_trimmed_and_skips_blanks() {
        assert_eq!(
            parse_runtime_list(" echo , codex ,, "),
            vec![String::from("echo"), String::from("codex")]
        );
        assert!(parse_runtime_list("").is_empty());
    }
}

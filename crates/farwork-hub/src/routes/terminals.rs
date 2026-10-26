//! Device terminals: `POST /connectors/{id}/terminals` and
//! `GET /connectors/{id}/terminals/{terminalId}/stream`.
//!
//! The hub never owns a PTY. It mints `terminalId`, records the request, and from
//! then on relays two sockets: the connector ingress (one per device) and one
//! browser socket per terminal. The relay is two-way and does not rewrite `data` —
//! PTY bytes are base64 in both directions, and a hub that decoded and re-encoded
//! them would be a second, independently buggy implementation of the contract.
//!
//! Failure is always reported: an offline connector refuses the request (503) or
//! the stream (503 at the handshake), and a terminal that cannot be resumed is
//! announced with `terminal.error` instead of a silent close.

use std::collections::HashMap;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::time::{Duration, Instant};

use crate::error::{ApiError, ApiResult};
use crate::ingress::{self, ConnectionHandle};
use crate::routes::extract::AuthedUser;
use crate::state::AppState;
use crate::timeutil;

/// Terminal geometry used when the caller omits cols/rows.
pub const DEFAULT_COLS: u16 = 80;
pub const DEFAULT_ROWS: u16 = 24;

/// Same cadence as the other hub sockets: a dead browser must be noticed.
const KEEPALIVE_SECONDS: u64 = 15;

pub const TERMINAL_COLUMNS: &str =
    "id, connector_id, user_id, cols, rows, cwd, created_at, closed_at, error_code, error_message";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/connectors/{connector_id}/terminals", post(create))
        .route(
            "/connectors/{connector_id}/terminals/{terminal_id}/stream",
            get(stream),
        )
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TerminalRow {
    pub id: String,
    pub connector_id: String,
    pub user_id: String,
    pub cols: i32,
    pub rows: i32,
    pub cwd: Option<String>,
    pub created_at: String,
    pub closed_at: Option<String>,
    /// Set when the agent refused to open the terminal: what a browser attaching
    /// after the refusal is told.
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

impl TerminalRow {
    pub fn is_closed(&self) -> bool {
        self.closed_at.is_some()
    }

    /// The `terminal` object: exactly the five fields the contract names.
    pub fn view(&self) -> Value {
        json!({
            "id": self.id,
            "connectorId": self.connector_id,
            "cols": self.cols,
            "rows": self.rows,
            "createdAt": self.created_at,
        })
    }
}

async fn create(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Path(connector_id): Path<String>,
    Json(payload): Json<CreateTerminal>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let connector = crate::routes::connectors::find_connector(state.pool(), &connector_id)
        .await?
        .filter(|row| row.user_id == user_id && !row.revoked)
        .ok_or_else(|| {
            // 404 rather than 403: somebody else's connector must not be
            // distinguishable from one that does not exist.
            ApiError::not_found(json!({
                "code": "connector_not_found",
                "message": "connector not found",
            }))
        })?;

    let cols = payload.cols.unwrap_or(DEFAULT_COLS);
    let rows = payload.rows.unwrap_or(DEFAULT_ROWS);
    if !ingress::is_valid_terminal_size(cols, rows) {
        return Err(ApiError::unprocessable(json!({
            "code": "invalid_size",
            "message": format!(
                "cols/rows must be {}..={}, got {cols}x{rows}",
                ingress::MIN_TERMINAL_DIMENSION,
                ingress::MAX_TERMINAL_DIMENSION
            ),
        })));
    }
    let cwd = normalize_cwd(payload.cwd.as_deref())?;

    let handle = state.connectors().get(&connector.id).ok_or_else(offline)?;

    let terminal_id = crate::ids::terminal_id();
    insert_terminal(
        &state,
        &terminal_id,
        &connector.id,
        &user_id,
        cols,
        rows,
        cwd.as_deref(),
    )
    .await?;
    // Registered before the frame leaves: a fast agent can answer
    // terminal.opened before this handler gets back here.
    state.term_relay().register(&terminal_id);
    if !handle.send(&ingress::terminal_open_frame(
        &terminal_id,
        cols,
        rows,
        cwd.as_deref(),
        None,
    )) {
        state.term_relay().close(&terminal_id);
        // Recorded, not just closed: a browser that attaches anyway is told the
        // device is gone instead of "this terminal never existed".
        fail_terminal(
            &state,
            &terminal_id,
            "connector_offline",
            "the connector socket closed before the terminal request was sent",
        )
        .await?;
        return Err(offline());
    }

    let stored = find_terminal(&state, &terminal_id)
        .await?
        .ok_or_else(|| ApiError::internal("the terminal was not persisted"))?;
    tracing::info!(
        %terminal_id,
        connector_id = %connector.id,
        cols,
        rows,
        cwd = cwd.as_deref().unwrap_or("-"),
        // Dispatched, not proven: the agent may still refuse the cwd, and that
        // refusal arrives as terminal.error on the stream.
        "terminal requested"
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "terminal": stored.view(),
            "serverTime": timeutil::utc_now(),
        })),
    ))
}

#[derive(Debug, Deserialize)]
pub struct CreateTerminal {
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub cols: Option<u16>,
    #[serde(default)]
    pub rows: Option<u16>,
}

/// An absent or blank cwd means "the agent's --workdir". The hub cannot check the
/// sandbox itself: the root is the agent's --workdir, which it never reports. The
/// agent resolves this path and refuses it with permission_denied on the stream.
fn normalize_cwd(raw: Option<&str>) -> ApiResult<Option<String>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if !std::path::Path::new(trimmed).is_absolute() {
        return Err(ApiError::unprocessable(json!({
            "code": "invalid_path",
            "message": format!("cwd must be an absolute path, got {raw:?}"),
        })));
    }
    Ok(Some(trimmed.to_string()))
}

fn offline() -> ApiError {
    ApiError::service_unavailable(json!({
        "code": "connector_offline",
        "message": "connector is not connected; the terminal was not requested",
    }))
}

fn offline_stream() -> ApiError {
    ApiError::service_unavailable(json!({
        "code": "connector_offline",
        "message": "connector is not connected; the terminal stream cannot attach",
    }))
}

/// The browser asked to attach without a usable ticket.
fn unauthorized(detail: &str) -> Response {
    (StatusCode::UNAUTHORIZED, Json(json!({ "detail": detail }))).into_response()
}

async fn stream(
    State(state): State<AppState>,
    Path((connector_id, terminal_id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let Some(ticket_value) = query
        .get("ticket")
        .map(String::as_str)
        .filter(|value| !value.is_empty())
    else {
        return unauthorized("missing ticket");
    };
    // The ticket is single-use and its scope is this exact terminalId, so a
    // ticket minted for terminal A cannot be spent on terminal B.
    let ticket = match crate::routes::ws::consume_ticket(&state, ticket_value, &terminal_id).await {
        Ok(Some(ticket)) => ticket,
        Ok(None) => return unauthorized("invalid ticket"),
        Err(error) => return error.into_response(),
    };

    // Offline is checked before the row: "the device is gone" is a better answer
    // than "unknown terminal" for a terminal that existed a moment ago, and it is
    // refused at the HTTP layer. Establishing the socket and then saying "offline"
    // would look like a live terminal that simply produces no output.
    if state.connectors().get(&connector_id).is_none() {
        return offline_stream().into_response();
    }

    let owned = find_terminal(&state, &terminal_id)
        .await
        .ok()
        .flatten()
        .filter(|row| row.user_id == ticket.user_id && row.connector_id == connector_id);
    let Some(row) = owned else {
        // The handshake is allowed so the browser gets a reason rather than a
        // bare socket failure; the reason is the contract's own error frame.
        return upgrade.on_upgrade(move |mut socket| async move {
            let _ = send_error(&mut socket, "unknown terminal", "terminal_not_found").await;
            let _ = socket.send(Message::Close(None)).await;
        });
    };
    if row.is_closed() {
        // The refusal reason outlives the moment it was reported: the agent usually
        // answers before the browser has finished minting its ticket, so replaying
        // the stored reason is the difference between "permission_denied" and a
        // terminal that appears to have vanished.
        let refusal = match (&row.error_code, &row.error_message) {
            (Some(code), message) => terminal_error(message.clone().unwrap_or_default(), code),
            _ => terminal_error("this terminal is already closed", "terminal_not_found"),
        };
        return upgrade.on_upgrade(move |mut socket| async move {
            let _ = send_frame(&mut socket, &refusal).await;
            let _ = socket.send(Message::Close(None)).await;
        });
    }

    let state_clone = state.clone();
    upgrade.on_upgrade(
        move |socket| async move { run_terminal_socket(state_clone, socket, row).await },
    )
}

async fn run_terminal_socket(state: AppState, mut socket: WebSocket, row: TerminalRow) {
    let terminal_id = row.id.clone();
    let Some(subscription) = state.term_relay().subscribe(&terminal_id) else {
        // The row exists but no live channel does: the hub restarted, or the
        // terminal was retired. Either way this id cannot be resumed.
        let _ = send_error(
            &mut socket,
            "this terminal is no longer open",
            "terminal_not_found",
        )
        .await;
        close_terminal(&state, &terminal_id).await.ok();
        let _ = socket.send(Message::Close(None)).await;
        return;
    };
    let mut frames = subscription.frames;
    if subscription.already_opened
        && !send_frame(&mut socket, &json!({ "type": "terminal.opened" })).await
    {
        return;
    }
    let Some(handle) = state.connectors().get(&row.connector_id) else {
        let _ = send_error(
            &mut socket,
            "the connector disconnected",
            "connector_offline",
        )
        .await;
        close_terminal(&state, &terminal_id).await.ok();
        let _ = socket.send(Message::Close(None)).await;
        return;
    };

    let mut ticker = tokio::time::interval_at(
        Instant::now() + Duration::from_secs(KEEPALIVE_SECONDS),
        Duration::from_secs(KEEPALIVE_SECONDS),
    );
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                // The same keepalive every other hub socket sends: a browser that
                // already handles /sessions/{id}/ws needs no new vocabulary.
                let keepalive = json!({ "type": "keepalive", "serverTime": timeutil::utc_now() });
                if !send_frame(&mut socket, &keepalive).await {
                    break;
                }
            }
            relayed = frames.recv() => {
                match relayed {
                    Ok(frame) => {
                        if !send_frame(&mut socket, &frame).await {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        // A PTY byte stream cannot be resumed mid-way: skipping
                        // frames would corrupt the screen silently. Say so and
                        // let the browser start a fresh terminal.
                        let message = format!(
                            "terminal stream lagged by {skipped} frames; reopen the terminal"
                        );
                        let _ = send_error(&mut socket, message, "stream_lagged").await;
                        break;
                    }
                    // The agent reported exit/error and the relay retired the id.
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            incoming = socket.next() => {
                match incoming {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                    Some(Ok(Message::Text(text))) => {
                        if !handle_browser_frame(&state, &handle, &terminal_id, text.as_str(), &mut socket).await {
                            break;
                        }
                    }
                    // Ping/Pong are plumbing; binary is not part of the contract.
                    Some(Ok(_)) => continue,
                }
            }
        }
    }

    // Whoever left first, the PTY must not outlive them: an orphaned shell with no
    // reader is a process leak on the device. A still-registered channel means the
    // agent has not reported the end yet, so it is told to close.
    let still_open = state.term_relay().close(&terminal_id);
    if still_open && !handle.send(&ingress::terminal_close_frame(&terminal_id)) {
        tracing::debug!(%terminal_id, "cannot tell the connector to close the terminal");
    }
    if let Err(error) = close_terminal(&state, &terminal_id).await {
        tracing::warn!(%terminal_id, detail = ?error.detail, "cannot mark the terminal closed");
    }
    let _ = socket.send(Message::Close(None)).await;
}

/// `true` keeps the socket open.
async fn handle_browser_frame(
    state: &AppState,
    handle: &ConnectionHandle,
    terminal_id: &str,
    text: &str,
    socket: &mut WebSocket,
) -> bool {
    let value: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(error) => {
            return send_error(socket, format!("malformed frame: {error}"), "invalid_frame").await
        }
    };
    match value.get("type").and_then(Value::as_str) {
        Some("terminal.input") => match value.get("data").and_then(Value::as_str) {
            Some(data) => {
                // Verbatim: the hub neither re-encodes nor re-chunks PTY input,
                // whatever the browser produced is what the PTY receives. It is
                // decoded once only to refuse garbage here, where the browser can
                // still be told, instead of on the device where nothing answers.
                if let Err(error) = ingress_decode(data) {
                    return send_error(
                        socket,
                        format!("terminal.input data is not standard base64: {error}"),
                        "invalid_frame",
                    )
                    .await;
                }
                if !handle.send(&ingress::terminal_data_frame(terminal_id, data)) {
                    return send_error(
                        socket,
                        "the connector is not connected",
                        "connector_offline",
                    )
                    .await;
                }
                true
            }
            None => {
                send_error(
                    socket,
                    "terminal.input needs a base64 data field",
                    "invalid_frame",
                )
                .await
            }
        },
        Some("terminal.resize") => {
            let cols = dimension(&value, "cols");
            let rows = dimension(&value, "rows");
            let (Some(cols), Some(rows)) = (cols, rows) else {
                return send_error(
                    socket,
                    "terminal.resize needs cols and rows",
                    "invalid_frame",
                )
                .await;
            };
            if !ingress::is_valid_terminal_size(cols, rows) {
                return send_error(
                    socket,
                    format!(
                        "cols/rows must be {}..={}, got {cols}x{rows}",
                        ingress::MIN_TERMINAL_DIMENSION,
                        ingress::MAX_TERMINAL_DIMENSION
                    ),
                    "invalid_size",
                )
                .await;
            }
            if !handle.send(&ingress::terminal_resize_frame(terminal_id, cols, rows)) {
                return send_error(
                    socket,
                    "the connector is not connected",
                    "connector_offline",
                )
                .await;
            }
            true
        }
        Some("terminal.close") => {
            handle.send(&ingress::terminal_close_frame(terminal_id));
            if let Err(error) = close_terminal(state, terminal_id).await {
                tracing::warn!(%terminal_id, detail = ?error.detail, "cannot mark the terminal closed");
            }
            // Stay attached: the agent still owes a terminal.exit, and the browser
            // must see it to tell a clean end from a lost device.
            true
        }
        Some(other) => {
            send_error(
                socket,
                format!("unknown browser frame {other:?}"),
                "invalid_frame",
            )
            .await
        }
        None => send_error(socket, "frame has no type", "invalid_frame").await,
    }
}

/// Standard alphabet with padding, per the contract. Only checked, never
/// rewritten: the byte string the browser sent is the byte string that is relayed.
fn ingress_decode(data: &str) -> Result<Vec<u8>, base64::DecodeError> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(data.as_bytes())
}

fn dimension(value: &Value, key: &str) -> Option<u16> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|raw| u16::try_from(raw).ok())
}

/// The browser-facing error frame. `code` is a closed set: the browser acts on it
/// (reopen, give up, retry), so an invented code would be unactionable.
pub fn terminal_error(message: impl Into<String>, code: &str) -> Value {
    json!({
        "type": "terminal.error",
        "message": message.into(),
        "code": code,
    })
}

async fn send_frame(socket: &mut WebSocket, frame: &Value) -> bool {
    socket
        .send(Message::Text(frame.to_string().into()))
        .await
        .is_ok()
}

async fn send_error(socket: &mut WebSocket, message: impl Into<String>, code: &str) -> bool {
    send_frame(socket, &terminal_error(message, code)).await
}

// ------------------------------------------------------------------- storage

pub async fn find_terminal(state: &AppState, terminal_id: &str) -> ApiResult<Option<TerminalRow>> {
    let sql = format!("select {TERMINAL_COLUMNS} from terminals where id = $1");
    Ok(sqlx::query_as::<_, TerminalRow>(&sql)
        .bind(terminal_id)
        .fetch_optional(state.pool())
        .await?)
}

async fn insert_terminal(
    state: &AppState,
    terminal_id: &str,
    connector_id: &str,
    user_id: &str,
    cols: u16,
    rows: u16,
    cwd: Option<&str>,
) -> ApiResult<()> {
    sqlx::query(
        "insert into terminals (id, connector_id, user_id, cols, rows, cwd, created_at)
         values ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(terminal_id)
    .bind(connector_id)
    .bind(user_id)
    .bind(i32::from(cols))
    .bind(i32::from(rows))
    .bind(cwd)
    .bind(timeutil::utc_now())
    .execute(state.pool())
    .await?;
    Ok(())
}

/// Idempotent: a terminal can be closed by the browser, by the agent or by a
/// disconnect, and all three may race. The first write wins.
pub async fn close_terminal(state: &AppState, terminal_id: &str) -> ApiResult<bool> {
    let result =
        sqlx::query("update terminals set closed_at = $2 where id = $1 and closed_at is null")
            .bind(terminal_id)
            .bind(timeutil::utc_now())
            .execute(state.pool())
            .await?;
    Ok(result.rows_affected() > 0)
}

/// Close a terminal the agent refused, keeping the reason for whoever attaches
/// next. The first refusal wins: a row that already explains itself is not
/// rewritten by a later frame.
pub async fn fail_terminal(
    state: &AppState,
    terminal_id: &str,
    code: &str,
    message: &str,
) -> ApiResult<bool> {
    let result = sqlx::query(
        "update terminals
            set closed_at = $2, error_code = $3, error_message = $4
          where id = $1 and closed_at is null",
    )
    .bind(terminal_id)
    .bind(timeutil::utc_now())
    .bind(code)
    .bind(message)
    .execute(state.pool())
    .await?;
    Ok(result.rows_affected() > 0)
}

/// A connector's socket is gone, so every PTY it was streaming died with it.
/// Each open terminal is told so on its browser socket and then marked closed.
pub async fn abandon_connector_terminals(state: &AppState, connector_id: &str) -> ApiResult<usize> {
    use sqlx::Row as _;
    let rows =
        sqlx::query("select id from terminals where connector_id = $1 and closed_at is null")
            .bind(connector_id)
            .fetch_all(state.pool())
            .await?;
    let ids: Vec<String> = rows.iter().map(|row| row.get::<String, _>("id")).collect();
    for terminal_id in &ids {
        state.term_relay().publish(
            terminal_id,
            &terminal_error(
                "the connector disconnected; the terminal is gone",
                "connector_offline",
            ),
        );
        state.term_relay().close(terminal_id);
        close_terminal(state, terminal_id).await?;
    }
    Ok(ids.len())
}

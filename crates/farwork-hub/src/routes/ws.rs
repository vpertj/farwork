//! WebSocket surface: one-shot tickets, the session delta stream and the
//! dashboard stream.
//!
//! Contract: upstream \`api/client_ws.py\`, \`api/sessions.py\` (\`session_ws\`) and
//! \`api/dashboard_stream.py\`.

use std::collections::HashMap;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::time::Instant;

use crate::auth::sha256_hex;
use crate::error::{ApiError, ApiResult};
use crate::ids;
use crate::routes::extract::AuthedUser;
use crate::state::AppState;
use crate::timeutil;

pub const TICKET_TTL_SECONDS: i64 = 60;
const KEEPALIVE_SECONDS: u64 = 15;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/ws-ticket", post(create_ws_ticket))
        .route("/dashboard/ws", get(dashboard_ws))
        .route("/sessions/{session_id}/ws", get(session_ws))
}

#[derive(Debug, Deserialize)]
pub struct WsTicketScope {
    #[serde(default, rename = "sessionId")]
    pub session_id: Option<String>,
    /// A device terminal stream. The scope string stored on the ticket is the
    /// terminalId itself, so the ticket can only be spent on that terminal.
    #[serde(default, rename = "terminalId")]
    pub terminal_id: Option<String>,
    #[serde(default)]
    pub dashboard: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct WsTicketRequest {
    #[serde(default, rename = "clientId")]
    pub client_id: String,
    #[serde(default)]
    pub scope: Option<WsTicketScope>,
}

pub(crate) struct ConsumedTicket {
    pub(crate) user_id: String,
    pub(crate) client_id: String,
}

async fn issue_ticket(
    state: &AppState,
    user_id: &str,
    client_id: &str,
    scope: &str,
) -> ApiResult<(String, String)> {
    // Token shape mirrors upstream: \`wst_\` + 32 random bytes, stored hashed.
    let token = format!("wst_{}", ids::token_urlsafe(32));
    let expires_epoch = timeutil::unix_now() + TICKET_TTL_SECONDS;
    sqlx::query(
        "insert into ws_tickets (ticket_hash, user_id, client_id, scope, expires_at_epoch, created_at)
         values ($1, $2, $3, $4, $5, $6)",
    )
    .bind(sha256_hex(token.as_bytes()))
    .bind(user_id)
    .bind(client_id)
    .bind(scope)
    .bind(expires_epoch)
    .bind(timeutil::utc_now())
    .execute(state.pool())
    .await?;
    Ok((token, timeutil::iso_from_epoch(expires_epoch)))
}

/// Single use: the row is deleted on read, so a replay cannot be distinguished
/// from a stale ticket by design.
pub(crate) async fn consume_ticket(
    state: &AppState,
    token: &str,
    expected_scope: &str,
) -> ApiResult<Option<ConsumedTicket>> {
    use sqlx::Row as _;
    let row = sqlx::query(
        "delete from ws_tickets
          where ticket_hash = $1 and expires_at_epoch >= $2
      returning user_id, client_id, scope",
    )
    .bind(sha256_hex(token.as_bytes()))
    .bind(timeutil::unix_now())
    .fetch_optional(state.pool())
    .await?;
    Ok(row.and_then(|row| {
        let scope: String = row.get("scope");
        if scope != expected_scope {
            return None;
        }
        Some(ConsumedTicket {
            user_id: row.get("user_id"),
            client_id: row.get("client_id"),
        })
    }))
}

async fn create_ws_ticket(
    State(state): State<AppState>,
    AuthedUser(user_id): AuthedUser,
    Json(payload): Json<WsTicketRequest>,
) -> ApiResult<Json<Value>> {
    let client_id = payload.client_id.trim().to_string();
    if client_id.is_empty() {
        return Err(ApiError::unprocessable("clientId is required"));
    }
    let scope = payload
        .scope
        .ok_or_else(|| ApiError::unprocessable("exactly one WebSocket ticket scope is required"))?;
    let session_id = scope
        .session_id
        .as_deref()
        .filter(|value| !value.is_empty());
    let terminal_id = scope
        .terminal_id
        .as_deref()
        .filter(|value| !value.is_empty());
    let dashboard = scope.dashboard.unwrap_or(false);
    let chosen = [session_id.is_some(), terminal_id.is_some(), dashboard]
        .iter()
        .filter(|chosen| **chosen)
        .count();
    if chosen != 1 {
        return Err(ApiError::unprocessable(
            "exactly one WebSocket ticket scope is required",
        ));
    }
    let resolved = match (session_id, terminal_id) {
        (Some(session_id), None) => {
            crate::routes::sessions::find_session(state.pool(), session_id)
                .await?
                .filter(|row| row.user_id == user_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            session_id.to_string()
        }
        (None, Some(terminal_id)) => {
            // Creating the terminal is not enough: a ticket may only be minted
            // for a terminal this user owns, and its scope binds that exact id.
            crate::routes::terminals::find_terminal(&state, terminal_id)
                .await?
                .filter(|row| row.user_id == user_id)
                .ok_or_else(|| ApiError::not_found("terminal not found"))?;
            terminal_id.to_string()
        }
        _ => String::from("dashboard"),
    };
    let (ticket, expires_at) = issue_ticket(&state, &user_id, &client_id, &resolved).await?;
    Ok(Json(json!({
        "ticket": ticket,
        "expiresAt": expires_at,
        "serverTime": timeutil::utc_now(),
    })))
}

fn ticket_from_query(query: &HashMap<String, String>) -> Option<&str> {
    query
        .get("ticket")
        .map(String::as_str)
        .filter(|value| !value.is_empty())
}

async fn session_ws(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let Some(ticket_value) = ticket_from_query(&query) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"detail": "missing ticket"})),
        )
            .into_response();
    };
    let ticket = match consume_ticket(&state, ticket_value, &session_id).await {
        Ok(Some(ticket)) => ticket,
        Ok(None) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"detail": "invalid ticket"})),
            )
                .into_response()
        }
        Err(error) => return error.into_response(),
    };
    let owned = crate::routes::sessions::find_session(state.pool(), &session_id)
        .await
        .ok()
        .flatten()
        .filter(|row| row.user_id == ticket.user_id);
    if owned.is_none() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"detail": "session not found"})),
        )
            .into_response();
    }

    let hub_receiver = state.hub().subscribe();
    let state_clone = state.clone();
    upgrade.on_upgrade(move |socket| async move {
        run_session_socket(
            state_clone,
            socket,
            session_id,
            ticket.client_id,
            hub_receiver,
        )
        .await;
    })
}

async fn run_session_socket(
    state: AppState,
    mut socket: WebSocket,
    session_id: String,
    client_id: String,
    mut hub_receiver: tokio::sync::broadcast::Receiver<crate::state::HubEvent>,
) {
    let subscribed = match state
        .timelines()
        .subscribed_event(state.pool(), &session_id, &client_id)
        .await
    {
        Ok(event) => event,
        Err(_) => return,
    };
    if socket
        .send(Message::Text(subscribed.to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    let mut ticker = tokio::time::interval_at(
        Instant::now() + Duration::from_secs(KEEPALIVE_SECONDS),
        Duration::from_secs(KEEPALIVE_SECONDS),
    );
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let keepalive = json!({"type": "keepalive", "serverTime": timeutil::utc_now()});
                if socket.send(Message::Text(keepalive.to_string().into())).await.is_err() {
                    break;
                }
            }
            event = hub_receiver.recv() => {
                match event {
                    Ok(event) => {
                        if event.session_id.as_deref() != Some(session_id.as_str()) {
                            continue;
                        }
                        if socket.send(Message::Text(event.json.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        // Dropped deltas cannot be replayed incrementally: tell
                        // the client to re-read the snapshot instead of patching.
                        let refetch = match state
                            .timelines()
                            .refetch_event(
                                state.pool(),
                                &session_id,
                                farwork_timeline::RefetchReason::Requested,
                            )
                            .await
                        {
                            Ok(event) => event,
                            Err(_) => break,
                        };
                        if socket.send(Message::Text(refetch.to_string().into())).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            incoming = socket.next() => {
                match incoming {
                    None => break,
                    Some(Err(_)) => break,
                    // Inbound frames are reserved for future client actions;
                    // ignoring them keeps the lease alive.
                    Some(Ok(_)) => continue,
                }
            }
        }
    }
}

async fn dashboard_ws(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let Some(ticket_value) = ticket_from_query(&query) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"detail": "missing ticket"})),
        )
            .into_response();
    };
    let ticket = match consume_ticket(&state, ticket_value, "dashboard").await {
        Ok(Some(ticket)) => ticket,
        Ok(None) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"detail": "invalid ticket"})),
            )
                .into_response()
        }
        Err(error) => return error.into_response(),
    };
    let hub_receiver = state.hub().subscribe();
    let state_clone = state.clone();
    upgrade.on_upgrade(move |socket| async move {
        run_dashboard_socket(state_clone, socket, ticket.user_id, hub_receiver).await;
    })
}

pub async fn dashboard_snapshot(state: &AppState, user_id: &str) -> ApiResult<Value> {
    use sqlx::Row as _;
    let connectors = crate::routes::connectors::connector_views(state, user_id).await?;
    let projects = crate::routes::projects::project_views(state, user_id, None).await?;
    let statuses = crate::routes::sessions::connector_status_map(state, user_id).await?;
    let rows = sqlx::query(&format!(
        "select {} from sessions where user_id = $1
          order by pinned desc, sort_at desc, updated_seq desc, id desc limit 100",
        crate::routes::sessions::SESSION_COLUMNS
    ))
    .bind(user_id)
    .fetch_all(state.pool())
    .await?;
    let sessions: Vec<Value> = rows
        .iter()
        .map(|row| {
            let session = crate::models::SessionRow {
                id: row.get("id"),
                user_id: row.get("user_id"),
                connector_id: row.get("connector_id"),
                project_id: row.get("project_id"),
                runtime: row.get("runtime"),
                runtime_id: row.get("runtime_id"),
                origin: row.get("origin"),
                external_session_id: row.get("external_session_id"),
                title: row.get("title"),
                title_source: row.get("title_source"),
                cwd: row.get("cwd"),
                status: row.get("status"),
                takeover: row.get("takeover"),
                pinned: row.get("pinned"),
                pinned_at: row.get("pinned_at"),
                archived: row.get("archived"),
                archived_at: row.get("archived_at"),
                source_state: row.get("source_state"),
                last_read_seq: row.get("last_read_seq"),
                latest_turn_end_seq: row.get("latest_turn_end_seq"),
                last_synced_at: row.get("last_synced_at"),
                source_observed_at: row.get("source_observed_at"),
                last_activity_at: row.get("last_activity_at"),
                sort_at: row.get("sort_at"),
                seq: row.get("seq"),
                seq_allocated_high: row.get("seq_allocated_high"),
                updated_seq: row.get("updated_seq"),
                created_at: row.get("created_at"),
                updated_at: row.get("updated_at"),
            };
            let status = statuses
                .get(&session.connector_id)
                .map(|value| value.as_str())
                .unwrap_or("offline");
            session.view(status)
        })
        .collect();
    Ok(json!({
        "type": "dashboard.snapshot",
        "connectors": connectors,
        "projects": projects,
        "sessions": sessions,
        // Runtime lifecycle belongs to the connector ingress, which this build
        // does not have; an empty list is the honest answer.
        "runtimes": [],
        "sessionPages": {
            "active": { "hasMore": false, "nextCursor": null },
            "archived": { "hasMore": false, "nextCursor": null },
        },
        "serverTime": timeutil::utc_now(),
    }))
}

async fn send_snapshot(socket: &mut WebSocket, snapshot: &Value) -> Result<(), axum::Error> {
    socket
        .send(Message::Text(snapshot.to_string().into()))
        .await
}

async fn run_dashboard_socket(
    state: AppState,
    mut socket: WebSocket,
    user_id: String,
    mut hub_receiver: tokio::sync::broadcast::Receiver<crate::state::HubEvent>,
) {
    match dashboard_snapshot(&state, &user_id).await {
        Ok(snapshot) => {
            if send_snapshot(&mut socket, &snapshot).await.is_err() {
                return;
            }
        }
        Err(_) => return,
    }

    let mut ticker = tokio::time::interval_at(
        Instant::now() + Duration::from_secs(KEEPALIVE_SECONDS),
        Duration::from_secs(KEEPALIVE_SECONDS),
    );
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let keepalive = json!({"type": "keepalive", "serverTime": timeutil::utc_now()});
                if socket.send(Message::Text(keepalive.to_string().into())).await.is_err() {
                    break;
                }
            }
            event = hub_receiver.recv() => {
                match event {
                    Ok(event) => {
                        if event.user_id != user_id {
                            continue;
                        }
                        // Collapse bursts: drain queued invalidations before
                        // rebuilding, so one rebuild covers a whole write batch.
                        while let Ok(next) = hub_receiver.try_recv() {
                            if next.user_id != user_id {
                                continue;
                            }
                        }
                        match dashboard_snapshot(&state, &user_id).await {
                            Ok(snapshot) => {
                                if send_snapshot(&mut socket, &snapshot).await.is_err() {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        if let Ok(snapshot) = dashboard_snapshot(&state, &user_id).await {
                            if send_snapshot(&mut socket, &snapshot).await.is_err() {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            incoming = socket.next() => {
                match incoming {
                    None => break,
                    Some(Err(_)) => break,
                    Some(Ok(_)) => continue,
                }
            }
        }
    }
}

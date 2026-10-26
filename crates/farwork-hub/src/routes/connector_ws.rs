//! \`GET /api/v2/connector/ws?token=...\` — the connector ingress.
//!
//! Connectors dial out, so this socket is the only path by which a connector can
//! be reached. It carries Ingress Protocol v1 (\`docs/architecture.md\`): the
//! connector says \`hello\`, the hub answers \`welcome\`, and from then on the hub
//! pushes \`session.*\`/\`notice.respond\` down while the connector reports
//! \`item.upsert\`, \`session.state\`, \`notice.upsert\` and friends up.
//!
//! Two rules shape the loop below:
//!
//! * A frame that cannot be routed (unknown \`streamId\`, a stream owned by another
//!   connector) is answered with \`error\` and dropped — never guessed at.
//! * Presence is only cleared by the connection that still owns the route, so a
//!   reconnect cannot be marked offline by the socket it replaced.

use std::collections::HashMap;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::time::Instant;

use farwork_timeline::{EventEnvelope, EventType};

use crate::auth::sha256_hex;
use crate::error::ApiError;
use crate::ingress::{self, ConnectionHandle, ConnectorFrame};
use crate::models::{ConnectorRow, SessionRow};
use crate::state::AppState;
use crate::timeline::wire_event;
use crate::timeutil;

/// How long a freshly upgraded socket may stay silent before the hub gives up on
/// it. A connector that never says \`hello\` must not hold a slot open.
const HELLO_TIMEOUT_SECS: u64 = 10;

pub fn router() -> Router<AppState> {
    Router::new().route("/connector/ws", get(connector_ws))
}

async fn connector_ws(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let Some(token) = query
        .get("token")
        .map(String::as_str)
        .filter(|value| !value.is_empty())
    else {
        return rejection("missing connector token");
    };
    // The plaintext token is only ever seen here; the database holds its digest.
    let connector = match crate::routes::connectors::find_connector_by_token(
        state.pool(),
        &sha256_hex(token.as_bytes()),
    )
    .await
    {
        Ok(Some(connector)) => connector,
        Ok(None) => return rejection("invalid connector token"),
        Err(error) => {
            tracing::error!(error = %error, "connector token lookup failed");
            return ApiError::internal("internal database error").into_response();
        }
    };

    let state_clone = state.clone();
    upgrade.on_upgrade(move |socket| run_connector_socket(state_clone, socket, connector))
}

/// No upgrade on an auth failure: the handshake is refused at the HTTP layer.
fn rejection(detail: &str) -> Response {
    (StatusCode::UNAUTHORIZED, Json(json!({ "detail": detail }))).into_response()
}

async fn run_connector_socket(state: AppState, mut socket: WebSocket, connector: ConnectorRow) {
    let agent = match await_hello(&mut socket).await {
        Ok(agent) => agent,
        Err(message) => {
            tracing::warn!(connector_id = %connector.id, reason = %message, "connector handshake rejected");
            let _ = send_frame(&mut socket, &ingress::error_frame(message, None)).await;
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
    };
    tracing::info!(
        connector_id = %connector.id,
        agent = %agent.name,
        platform = agent.platform.as_deref().unwrap_or("unknown"),
        version = agent.version.as_deref().unwrap_or("unknown"),
        "connector connected"
    );

    let (sender, mut outbound) = tokio::sync::mpsc::unbounded_channel::<String>();
    let connection_id = crate::ids::random_hex(8);
    let handle = ConnectionHandle::new(
        connector.id.clone(),
        connector.user_id.clone(),
        connection_id.clone(),
        sender,
    );
    // A reconnecting connector takes over the route; the socket it replaced is
    // told why instead of silently going dark.
    if let Some(previous) = state.connectors().register(handle) {
        previous.send(&ingress::error_frame(
            "connection replaced by a newer connector connection",
            None,
        ));
    }

    // Presence is recorded before \`welcome\` so a connector that has seen the
    // welcome can rely on being routable.
    if let Err(error) = ingress::set_presence(
        &state,
        &connector.id,
        &connector.user_id,
        true,
        agent.platform.as_deref(),
    )
    .await
    {
        tracing::warn!(connector_id = %connector.id, detail = ?error.detail, "cannot record presence");
    }

    if send_frame(&mut socket, &ingress::welcome_frame(crate::VERSION)).await {
        let deadline = Duration::from_secs(ingress::HEARTBEAT_TIMEOUT_SECS);
        let mut expires = Instant::now() + deadline;
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(expires) => {
                    tracing::warn!(connector_id = %connector.id, "connector heartbeat timeout");
                    let _ = send_frame(&mut socket, &ingress::error_frame("heartbeat timeout", None)).await;
                    break;
                }
                queued = outbound.recv() => {
                    let Some(text) = queued else { break };
                    if socket.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                incoming = socket.next() => {
                    match incoming {
                        None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                        Some(Ok(Message::Text(text))) => {
                            expires = Instant::now() + deadline;
                            if handle_frame(&state, &connector, &mut socket, text.as_str()).await {
                                break;
                            }
                        }
                        // Ping/Pong are protocol plumbing; binary is not part of v1.
                        Some(Ok(_)) => continue,
                    }
                }
            }
        }
    }

    // No reply can arrive for these any more: releasing them here turns a
    // connector disconnect into an immediate error instead of a 10s timeout per
    // waiting browser request.
    let abandoned = state.fs_relay().abandon_connector(&connector.id);
    if abandoned > 0 {
        tracing::warn!(
            connector_id = %connector.id,
            requests = abandoned,
            "connector disconnected with file requests in flight"
        );
    }

    // The connector's runtimes died with this socket, so their streams are closed
    // here whether or not a newer connection already took over the route.
    match ingress::abandon_connector_streams(&state, &connector.id).await {
        Ok(session_ids) => {
            for session_id in &session_ids {
                if let Err(error) =
                    publish_runtime_state(&state, &connector.user_id, session_id).await
                {
                    tracing::warn!(
                        session_id = %session_id,
                        reason = %error,
                        "cannot broadcast an abandoned stream"
                    );
                }
            }
            if !session_ids.is_empty() {
                crate::routes::projects::publish_dashboard_change(&state, &connector.user_id);
            }
        }
        Err(error) => {
            tracing::warn!(
                connector_id = %connector.id,
                detail = ?error.detail,
                "cannot close abandoned streams"
            );
        }
    }

    // Only the connection that still owns the route may clear presence.
    if state.connectors().unregister(&connector.id, &connection_id) {
        if let Err(error) =
            ingress::set_presence(&state, &connector.id, &connector.user_id, false, None).await
        {
            tracing::warn!(connector_id = %connector.id, detail = ?error.detail, "cannot clear presence");
        }
        tracing::info!(connector_id = %connector.id, "connector disconnected");
    }
}

async fn await_hello(socket: &mut WebSocket) -> Result<ingress::AgentInfo, String> {
    let timeout = Duration::from_secs(HELLO_TIMEOUT_SECS);
    loop {
        match tokio::time::timeout(timeout, socket.next()).await {
            Err(_) => return Err(String::from("handshake timeout: no hello frame")),
            Ok(None) => return Err(String::from("socket closed before hello")),
            Ok(Some(Err(error))) => return Err(format!("socket error before hello: {error}")),
            Ok(Some(Ok(Message::Text(text)))) => {
                return match serde_json::from_str::<ConnectorFrame>(text.as_str()) {
                    Ok(ConnectorFrame::Hello { agent }) => Ok(agent),
                    Ok(_) => Err(String::from("the first frame must be hello")),
                    Err(error) => Err(format!("malformed hello frame: {error}")),
                };
            }
            Ok(Some(Ok(Message::Close(_)))) => return Err(String::from("closed before hello")),
            Ok(Some(Ok(_))) => continue,
        }
    }
}

/// \`true\` means the socket must close.
async fn handle_frame(
    state: &AppState,
    connector: &ConnectorRow,
    socket: &mut WebSocket,
    text: &str,
) -> bool {
    let frame = match serde_json::from_str::<ConnectorFrame>(text) {
        Ok(frame) => frame,
        Err(error) => {
            let _ = send_frame(
                socket,
                &ingress::error_frame(format!("malformed frame: {error}"), None),
            )
            .await;
            return false;
        }
    };
    let stream_id = frame.stream_id().map(str::to_string);
    match apply_frame(state, connector, frame).await {
        Ok(()) => false,
        Err(message) => {
            let _ = send_frame(socket, &ingress::error_frame(message, stream_id.as_deref())).await;
            false
        }
    }
}

async fn apply_frame(
    state: &AppState,
    connector: &ConnectorRow,
    frame: ConnectorFrame,
) -> Result<(), String> {
    match frame {
        // A second hello is harmless; the socket is already routed and welcomed.
        ConnectorFrame::Hello { agent } => {
            tracing::debug!(connector_id = %connector.id, agent = %agent.name, "repeat hello ignored");
            Ok(())
        }
        ConnectorFrame::Heartbeat { ts } => {
            if ts.is_none() {
                tracing::debug!(connector_id = %connector.id, "heartbeat without ts");
            }
            ingress::touch_last_seen(state, &connector.id)
                .await
                .map_err(err)?;
            Ok(())
        }
        ConnectorFrame::SessionStarted {
            stream_id,
            external_session_id,
        } => {
            let session = session_for_stream(state, &connector.id, &stream_id).await?;
            if let Some(external) = external_session_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                let now = timeutil::utc_now();
                sqlx::query(
                    "update sessions set external_session_id = $2, updated_at = $3 where id = $1",
                )
                .bind(&session.id)
                .bind(external)
                .bind(&now)
                .execute(state.pool())
                .await
                .map_err(|error| error.to_string())?;
                sqlx::query(
                    "update session_runtime_states
                        set external_session_id = $2, updated_at = $3
                      where session_id = $1",
                )
                .bind(&session.id)
                .bind(external)
                .bind(&now)
                .execute(state.pool())
                .await
                .map_err(|error| error.to_string())?;
            }
            publish_session_meta(state, &connector.user_id, &session.id).await?;
            crate::routes::projects::publish_dashboard_change(state, &connector.user_id);
            Ok(())
        }
        ConnectorFrame::SessionState {
            stream_id,
            state: reported,
        } => {
            if !ingress::is_known_state(&reported) {
                return Err(format!(
                    "unknown session state {reported:?}; expected one of {:?}",
                    ingress::RUNTIME_STATES
                ));
            }
            let session = session_for_stream(state, &connector.id, &stream_id).await?;
            let now = timeutil::utc_now();
            sqlx::query(
                "update session_runtime_states set status = $2, updated_at = $3 where session_id = $1",
            )
            .bind(&session.id)
            .bind(&reported)
            .bind(&now)
            .execute(state.pool())
            .await
            .map_err(|error| error.to_string())?;
            sqlx::query("update sessions set status = $2, updated_at = $3 where id = $1")
                .bind(&session.id)
                .bind(&reported)
                .bind(&now)
                .execute(state.pool())
                .await
                .map_err(|error| error.to_string())?;
            publish_runtime_state(state, &connector.user_id, &session.id).await?;
            crate::routes::projects::publish_dashboard_change(state, &connector.user_id);
            Ok(())
        }
        ConnectorFrame::SessionExit { stream_id, code } => {
            let session = session_for_stream(state, &connector.id, &stream_id).await?;
            // A stream is single-use: once it exits, \`session.message\` has nothing
            // to address until a new turn is started.
            ingress::close_stream(state, &stream_id)
                .await
                .map_err(err)?;
            let final_status = if code.unwrap_or(0) == 0 {
                "idle"
            } else {
                "error"
            };
            let now = timeutil::utc_now();
            sqlx::query(
                "update session_runtime_states set status = $2, updated_at = $3 where session_id = $1",
            )
            .bind(&session.id)
            .bind(final_status)
            .bind(&now)
            .execute(state.pool())
            .await
            .map_err(|error| error.to_string())?;
            sqlx::query("update sessions set status = $2, updated_at = $3 where id = $1")
                .bind(&session.id)
                .bind(final_status)
                .bind(&now)
                .execute(state.pool())
                .await
                .map_err(|error| error.to_string())?;
            publish_runtime_state(state, &connector.user_id, &session.id).await?;
            crate::routes::projects::publish_dashboard_change(state, &connector.user_id);
            Ok(())
        }
        ConnectorFrame::ItemUpsert { stream_id, item } => {
            let session = session_for_stream(state, &connector.id, &stream_id).await?;
            let input = item.to_input()?;
            let item_id = input.id.clone();
            let source =
                crate::timeline::item_source(&session.runtime, &session.id, &item_id, None);
            let (committed, event) = state
                .timelines()
                .apply_runtime_item(state.pool(), &session, input, source)
                .await
                .map_err(err)?;
            match event {
                // \`None\` is the dedupe case: the runtime re-reported identical state,
                // so nothing was committed and nothing is broadcast.
                None => {
                    tracing::debug!(
                        session_id = %session.id,
                        item_id = %committed.id,
                        "item re-reported unchanged"
                    );
                }
                Some(event) => {
                    state.hub().publish_json(
                        &connector.user_id,
                        Some(&session.id),
                        event.to_string(),
                    );
                    crate::routes::projects::publish_dashboard_change(state, &connector.user_id);
                }
            }
            Ok(())
        }
        ConnectorFrame::NoticeUpsert { stream_id, notice } => {
            let session = session_for_stream(state, &connector.id, &stream_id).await?;
            let stored = crate::routes::notices::store_notice(state, &session.id, &notice)
                .await
                .map_err(err)?;
            crate::routes::notices::publish_notice_updated(
                state,
                &connector.user_id,
                &session.id,
                &stored.view(),
            )
            .await
            .map_err(err)?;
            Ok(())
        }
        ConnectorFrame::NoticeResolved {
            stream_id: _,
            notice_id,
            decision,
        } => {
            if !ingress::is_known_decision(&decision) {
                return Err(format!(
                    "unknown decision {decision:?}; expected one of {:?}",
                    ingress::DECISIONS
                ));
            }
            // The contract's \`notice.resolved\` carries no streamId, so the notice id
            // is resolved against the sessions this connector owns.
            let resolved =
                crate::routes::notices::resolve_notice(state, &connector.id, &notice_id, &decision)
                    .await
                    .map_err(err)?;
            let Some((session_id, notice)) = resolved else {
                return Err(format!("unknown notice {notice_id:?} for this connector"));
            };
            crate::routes::notices::publish_notice_updated(
                state,
                &connector.user_id,
                &session_id,
                &notice.view(),
            )
            .await
            .map_err(err)?;
            // The decision also leaves the notice list in a state a delta cannot
            // describe (\`options\` stops being actionable), so the list is re-sent.
            crate::routes::notices::publish_notice_snapshot(state, &connector.user_id, &session_id)
                .await
                .map_err(err)?;
            Ok(())
        }
        // A file-browsing reply is paired by requestId, not routed by stream, so
        // it is handed straight to the handler waiting for it.
        ConnectorFrame::FsResponse(reply) => {
            match state.fs_relay().deliver(&connector.id, reply) {
                crate::fsrelay::Delivery::Delivered => {}
                delivery => tracing::warn!(
                    connector_id = %connector.id,
                    delivery = ?delivery,
                    "dropped an fs.response that nothing was waiting for"
                ),
            }
            Ok(())
        }
        ConnectorFrame::Error { message, stream_id } => {
            tracing::warn!(
                connector_id = %connector.id,
                stream_id = stream_id.as_deref().unwrap_or("-"),
                message = %message,
                "connector reported an error"
            );
            Ok(())
        }
    }
}

/// Resolve \`streamId\` to the session it drives, refusing a stream that belongs to
/// another connector.
async fn session_for_stream(
    state: &AppState,
    connector_id: &str,
    stream_id: &str,
) -> Result<SessionRow, String> {
    let Some((session_id, owner)) = ingress::stream_owner(state, stream_id).await.map_err(err)?
    else {
        return Err(format!("unknown streamId {stream_id:?}"));
    };
    if owner != connector_id {
        return Err(format!(
            "streamId {stream_id:?} does not belong to this connector"
        ));
    }
    crate::routes::sessions::find_session(state.pool(), &session_id)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("unknown session {session_id:?}"))
}

async fn publish_session_meta(
    state: &AppState,
    user_id: &str,
    session_id: &str,
) -> Result<(), String> {
    let session = crate::routes::sessions::find_session(state.pool(), session_id)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("unknown session {session_id:?}"))?;
    let statuses = crate::routes::sessions::connector_status_map(state, user_id)
        .await
        .map_err(err)?;
    let status = statuses
        .get(&session.connector_id)
        .map(String::as_str)
        .unwrap_or("offline");
    let view = session.view(status);
    let event = state
        .timelines()
        .meta_event(state.pool(), session_id, &view)
        .await
        .map_err(err)?;
    state
        .hub()
        .publish_json(user_id, Some(session_id), event.to_string());
    Ok(())
}

async fn publish_runtime_state(
    state: &AppState,
    user_id: &str,
    session_id: &str,
) -> Result<(), String> {
    let Some(row) = crate::routes::sessions::runtime_state_row(state.pool(), session_id)
        .await
        .map_err(err)?
    else {
        return Ok(());
    };
    let head = state
        .timelines()
        .head(state.pool(), session_id)
        .await
        .map_err(err)?;
    let event = EventEnvelope::new(
        session_id,
        head.sequence(),
        EventType::RuntimeStateUpdated,
        json!({ "state": row.view() }),
        timeutil::utc_now(),
    )
    .map_err(|error| error.to_string())?;
    state
        .hub()
        .publish_json(user_id, Some(session_id), wire_event(&event).to_string());
    Ok(())
}

fn err(error: ApiError) -> String {
    error.detail.to_string()
}

async fn send_frame(socket: &mut WebSocket, frame: &Value) -> bool {
    socket
        .send(Message::Text(frame.to_string().into()))
        .await
        .is_ok()
}

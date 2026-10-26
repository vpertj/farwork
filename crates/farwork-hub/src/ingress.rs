//! Connector ingress: the routable connection table, presence writes and the
//! hub half of Ingress Protocol v1.
//!
//! The hub never dials a connector. Connectors open
//! \`GET /api/v2/connector/ws?token=...\` and the hub pushes commands down that
//! socket; the socket loop itself lives in [crate::routes::connector_ws].

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use farwork_timeline::{ItemInput, ItemStatus, Role, TimelineItemKind};

use crate::error::ApiResult;
use crate::models::SessionRow;
use crate::state::AppState;
use crate::timeutil;

/// Heartbeat cadence announced in \`welcome\`.
pub const HEARTBEAT_SECS: u64 = 15;

/// A connector that misses three heartbeat intervals is gone. Without this a
/// half-open TCP connection would keep a phantom \`online\` forever.
pub const HEARTBEAT_TIMEOUT_SECS: u64 = HEARTBEAT_SECS * 3;

/// Approval decisions the hub accepts on either side of the round trip.
pub const DECISIONS: [&str; 3] = ["approve", "approve_for_session", "reject"];

/// Session runtime states a connector may report.
pub const RUNTIME_STATES: [&str; 4] = ["running", "idle", "waiting_approval", "error"];

pub fn is_known_state(state: &str) -> bool {
    RUNTIME_STATES.contains(&state)
}

pub fn is_known_decision(decision: &str) -> bool {
    DECISIONS.contains(&decision)
}

// ------------------------------------------------------- routable connections

/// One live connector socket, as the rest of the hub sees it: a way to push a
/// frame. Cloning a handle lets an HTTP handler dispatch without touching the
/// registry lock.
#[derive(Clone)]
pub struct ConnectionHandle {
    pub connector_id: String,
    pub user_id: String,
    /// Identifies *this* socket. A reconnect takes over the route while the old
    /// socket is still winding down; the old one must not clear the new one's
    /// presence.
    pub connection_id: String,
    sender: mpsc::UnboundedSender<String>,
}

impl ConnectionHandle {
    /// The socket loop is the only creator: it owns the receiving end that actually
    /// writes frames.
    pub fn new(
        connector_id: impl Into<String>,
        user_id: impl Into<String>,
        connection_id: impl Into<String>,
        sender: mpsc::UnboundedSender<String>,
    ) -> Self {
        Self {
            connector_id: connector_id.into(),
            user_id: user_id.into(),
            connection_id: connection_id.into(),
            sender,
        }
    }

    /// \`false\` means the socket is already gone — the frame was not delivered.
    pub fn send(&self, frame: &Value) -> bool {
        self.sender.send(frame.to_string()).is_ok()
    }
}

/// \`connector_id -> live socket\`. In-process on purpose: a socket is not
/// shareable across processes, so this cannot be a database table.
#[derive(Default)]
pub struct ConnectorRegistry {
    connections: Mutex<HashMap<String, ConnectionHandle>>,
}

impl ConnectorRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, ConnectionHandle>> {
        match self.connections.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Install \`handle\` as the route for its connector and hand back whatever it
    /// replaced, so the caller can tell the old socket it lost the race.
    pub fn register(&self, handle: ConnectionHandle) -> Option<ConnectionHandle> {
        self.lock().insert(handle.connector_id.clone(), handle)
    }

    /// Remove the route only if it still points at \`connection_id\`.
    pub fn unregister(&self, connector_id: &str, connection_id: &str) -> bool {
        let mut connections = self.lock();
        let is_current = connections
            .get(connector_id)
            .map(|handle| handle.connection_id == connection_id)
            .unwrap_or(false);
        if is_current {
            connections.remove(connector_id);
        }
        is_current
    }

    pub fn get(&self, connector_id: &str) -> Option<ConnectionHandle> {
        self.lock().get(connector_id).cloned()
    }

    pub fn online(&self, connector_id: &str) -> bool {
        self.lock().contains_key(connector_id)
    }

    pub fn online_connectors(&self) -> Vec<String> {
        self.lock().keys().cloned().collect()
    }
}

// ------------------------------------------------------------------- presence

/// Persist presence and tell every dashboard. \`online\` also refreshes
/// \`last_seen_at\`, which is what the connector list shows as liveness.
pub async fn set_presence(
    state: &AppState,
    connector_id: &str,
    user_id: &str,
    online: bool,
    device_os: Option<&str>,
) -> ApiResult<()> {
    let now = timeutil::utc_now();
    sqlx::query(
        "update connectors
            set status = $2,
                last_seen_at = $3,
                device_os = coalesce($4, device_os),
                updated_at = $3
          where id = $1",
    )
    .bind(connector_id)
    .bind(if online { "online" } else { "offline" })
    .bind(&now)
    .bind(device_os)
    .execute(state.pool())
    .await?;
    crate::routes::projects::publish_dashboard_change(state, user_id);
    Ok(())
}

/// Heartbeat: liveness only. Deliberately not a dashboard invalidation — a
/// rebuild every 15s per connector would be pure noise.
pub async fn touch_last_seen(state: &AppState, connector_id: &str) -> ApiResult<()> {
    sqlx::query("update connectors set last_seen_at = $2 where id = $1")
        .bind(connector_id)
        .bind(timeutil::utc_now())
        .execute(state.pool())
        .await?;
    Ok(())
}

// --------------------------------------------------------------- stream table

/// Record a hub-minted \`streamId\` against the session it drives.
pub async fn open_stream(state: &AppState, session: &SessionRow, stream_id: &str) -> ApiResult<()> {
    sqlx::query(
        "insert into session_streams (stream_id, session_id, connector_id, created_at)
         values ($1, $2, $3, $4)",
    )
    .bind(stream_id)
    .bind(&session.id)
    .bind(&session.connector_id)
    .bind(timeutil::utc_now())
    .execute(state.pool())
    .await?;
    Ok(())
}

pub async fn close_stream(state: &AppState, stream_id: &str) -> ApiResult<()> {
    sqlx::query(
        "update session_streams set closed_at = $2 where stream_id = $1 and closed_at is null",
    )
    .bind(stream_id)
    .bind(timeutil::utc_now())
    .execute(state.pool())
    .await?;
    Ok(())
}

/// Close every stream a connector still owns and mark those sessions as errored.
///
/// A connector's runtimes die with its process: leaving the streams open would
/// let the next `session.message` be dispatched into a stream nobody is reading,
/// which the browser would then be told was delivered.
pub async fn abandon_connector_streams(
    state: &AppState,
    connector_id: &str,
) -> ApiResult<Vec<String>> {
    use sqlx::Row as _;
    let now = timeutil::utc_now();
    let mut tx = state.pool().begin().await?;
    let rows = sqlx::query(
        "update session_streams set closed_at = $2
          where connector_id = $1 and closed_at is null
      returning session_id",
    )
    .bind(connector_id)
    .bind(&now)
    .fetch_all(&mut *tx)
    .await?;
    let session_ids: Vec<String> = rows
        .iter()
        .map(|row| row.get::<String, _>("session_id"))
        .collect();
    for session_id in &session_ids {
        sqlx::query(
            "update session_runtime_states
                set status = 'error', status_reason = $2, updated_at = $3
              where session_id = $1",
        )
        .bind(session_id)
        .bind("connector disconnected")
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        sqlx::query("update sessions set status = 'error', updated_at = $2 where id = $1")
            .bind(session_id)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(session_ids)
}

/// The stream a session is currently driven by, if any.
pub async fn active_stream(state: &AppState, session_id: &str) -> ApiResult<Option<String>> {
    use sqlx::Row as _;
    let row = sqlx::query(
        "select stream_id from session_streams
          where session_id = $1 and closed_at is null
          order by created_at desc
          limit 1",
    )
    .bind(session_id)
    .fetch_optional(state.pool())
    .await?;
    Ok(row.map(|row| row.get::<String, _>("stream_id")))
}

/// \`streamId -> (session_id, connector_id)\` for an inbound session-scoped frame.
pub async fn stream_owner(
    state: &AppState,
    stream_id: &str,
) -> ApiResult<Option<(String, String)>> {
    use sqlx::Row as _;
    let row =
        sqlx::query("select session_id, connector_id from session_streams where stream_id = $1")
            .bind(stream_id)
            .fetch_optional(state.pool())
            .await?;
    Ok(row.map(|row| {
        (
            row.get::<String, _>("session_id"),
            row.get::<String, _>("connector_id"),
        )
    }))
}

// -------------------------------------------------------------------- frames
//
// Hub -> connector. Field names are the contract's, verbatim.

pub fn welcome_frame(server_version: &str) -> Value {
    json!({
        "type": "welcome",
        "serverVersion": server_version,
        "heartbeatSecs": HEARTBEAT_SECS,
    })
}

pub fn error_frame(message: impl Into<String>, stream_id: Option<&str>) -> Value {
    let mut frame = json!({ "type": "error", "message": message.into() });
    if let Some(stream_id) = stream_id {
        if let Some(map) = frame.as_object_mut() {
            map.insert(
                String::from("streamId"),
                Value::String(stream_id.to_string()),
            );
        }
    }
    frame
}

pub fn session_start_frame(
    stream_id: &str,
    session_id: &str,
    runtime: &str,
    cwd: &str,
    prompt: &str,
) -> Value {
    json!({
        "type": "session.start",
        "streamId": stream_id,
        "sessionId": session_id,
        "runtime": runtime,
        "cwd": cwd,
        "prompt": prompt,
    })
}

pub fn session_message_frame(
    stream_id: &str,
    text: &str,
    client_message_id: Option<&str>,
) -> Value {
    json!({
        "type": "session.message",
        "streamId": stream_id,
        "text": text,
        "clientMessageId": client_message_id,
    })
}

pub fn session_interrupt_frame(stream_id: &str) -> Value {
    json!({ "type": "session.interrupt", "streamId": stream_id })
}

pub fn notice_respond_frame(notice_id: &str, decision: &str) -> Value {
    json!({
        "type": "notice.respond",
        "noticeId": notice_id,
        "decision": decision,
    })
}

// ----------------------------------------------------------- inbound frames
//
// Connector -> hub. An unparseable frame is answered with \`error\`; it is never
// guessed at, because a half-understood rewrite of a session would be worse than
// a dropped frame.

#[derive(Debug, Deserialize)]
pub struct AgentInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ConnectorFrame {
    #[serde(rename = "hello")]
    Hello { agent: AgentInfo },
    #[serde(rename = "heartbeat")]
    Heartbeat {
        #[serde(default)]
        ts: Option<String>,
    },
    #[serde(rename = "session.started")]
    SessionStarted {
        #[serde(rename = "streamId")]
        stream_id: String,
        #[serde(default, rename = "externalSessionId")]
        external_session_id: Option<String>,
    },
    #[serde(rename = "session.state")]
    SessionState {
        #[serde(rename = "streamId")]
        stream_id: String,
        state: String,
    },
    #[serde(rename = "session.exit")]
    SessionExit {
        #[serde(rename = "streamId")]
        stream_id: String,
        #[serde(default)]
        code: Option<i64>,
    },
    #[serde(rename = "item.upsert")]
    ItemUpsert {
        #[serde(rename = "streamId")]
        stream_id: String,
        item: IngressItem,
    },
    #[serde(rename = "notice.upsert")]
    NoticeUpsert {
        #[serde(rename = "streamId")]
        stream_id: String,
        notice: IngressNotice,
    },
    #[serde(rename = "notice.resolved")]
    NoticeResolved {
        #[serde(default, rename = "streamId")]
        stream_id: Option<String>,
        #[serde(rename = "noticeId")]
        notice_id: String,
        decision: String,
    },
    #[serde(rename = "error")]
    Error {
        message: String,
        #[serde(default, rename = "streamId")]
        stream_id: Option<String>,
    },
}

impl ConnectorFrame {
    /// The stream this frame belongs to, when it has one.
    pub fn stream_id(&self) -> Option<&str> {
        match self {
            Self::SessionStarted { stream_id, .. }
            | Self::SessionState { stream_id, .. }
            | Self::SessionExit { stream_id, .. }
            | Self::ItemUpsert { stream_id, .. }
            | Self::NoticeUpsert { stream_id, .. } => Some(stream_id),
            Self::NoticeResolved { stream_id, .. } | Self::Error { stream_id, .. } => {
                stream_id.as_deref()
            }
            Self::Hello { .. } | Self::Heartbeat { .. } => None,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct IngressItem {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub status: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Value,
    /// The agent's own clock. Ordering is the hub's \`orderSeq\`, so this is
    /// accepted for completeness but never used to sort or to stamp rows.
    #[serde(default, rename = "itemTime")]
    pub item_time: Option<String>,
}

impl IngressItem {
    /// Parse the wire item into a domain input. Unknown enum spellings are a
    /// protocol break and are reported, not coerced.
    pub fn to_input(&self) -> Result<ItemInput, String> {
        if self.id.trim().is_empty() {
            return Err(String::from("item.id must not be empty"));
        }
        let kind: TimelineItemKind = self
            .kind
            .parse()
            .map_err(|_| format!("unknown item type {:?}", self.kind))?;
        let status: ItemStatus = self
            .status
            .parse()
            .map_err(|_| format!("unknown item status {:?}", self.status))?;
        let mut input =
            ItemInput::new(self.id.clone(), kind, self.content.clone()).with_status(status);
        if let Some(role) = self
            .role
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty())
        {
            let role: Role = role
                .parse()
                .map_err(|_| format!("unknown item role {role:?}"))?;
            input = input.with_role(role);
        }
        Ok(input)
    }
}

#[derive(Debug, Deserialize)]
pub struct IngressNotice {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<Value>,
    #[serde(default)]
    pub options: Option<Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle(
        connector_id: &str,
        connection_id: &str,
    ) -> (ConnectionHandle, mpsc::UnboundedReceiver<String>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (
            ConnectionHandle::new(connector_id, "usr_1", connection_id, sender),
            receiver,
        )
    }

    #[test]
    fn reconnect_replaces_the_route_and_keeps_the_old_sender_alive() {
        let registry = ConnectorRegistry::new();
        let (first, mut first_rx) = handle("conn_1", "c1");
        assert!(registry.register(first).is_none());
        let (second, _second_rx) = handle("conn_1", "c2");
        let replaced = registry.register(second).expect("replaces");
        assert_eq!(replaced.connection_id, "c1");
        assert!(replaced.send(&error_frame("superseded", None)));
        assert!(first_rx.try_recv().is_ok());
        assert_eq!(registry.get("conn_1").expect("current").connection_id, "c2");
    }

    #[test]
    fn a_replaced_connection_cannot_clear_the_new_presence() {
        let registry = ConnectorRegistry::new();
        let (first, _rx) = handle("conn_1", "c1");
        registry.register(first);
        let (second, _rx2) = handle("conn_1", "c2");
        registry.register(second);
        assert!(!registry.unregister("conn_1", "c1"));
        assert!(registry.online("conn_1"));
        assert!(registry.unregister("conn_1", "c2"));
        assert!(!registry.online("conn_1"));
    }

    #[test]
    fn hub_frames_match_the_contract_field_names() {
        let frame = session_start_frame("str_1", "sess_1", "codex", "/tmp", "hi");
        assert_eq!(frame["type"], "session.start");
        assert_eq!(frame["streamId"], "str_1");
        assert_eq!(frame["sessionId"], "sess_1");
        assert_eq!(frame["runtime"], "codex");
        assert_eq!(frame["cwd"], "/tmp");
        assert_eq!(frame["prompt"], "hi");

        let frame = session_message_frame("str_1", "go", Some("cm_1"));
        assert_eq!(frame["type"], "session.message");
        assert_eq!(frame["clientMessageId"], "cm_1");

        let frame = notice_respond_frame("ntc_1", "approve");
        assert_eq!(frame["type"], "notice.respond");
        assert_eq!(frame["noticeId"], "ntc_1");
        assert_eq!(frame["decision"], "approve");

        let frame = welcome_frame("0.1.0");
        assert_eq!(frame["type"], "welcome");
        assert_eq!(frame["serverVersion"], "0.1.0");
        assert_eq!(frame["heartbeatSecs"], 15);

        let frame = error_frame("boom", Some("str_1"));
        assert_eq!(frame["type"], "error");
        assert_eq!(frame["message"], "boom");
        assert_eq!(frame["streamId"], "str_1");
        assert!(error_frame("boom", None).get("streamId").is_none());
    }

    #[test]
    fn inbound_frames_parse_by_type_discriminator() {
        let frame: ConnectorFrame = serde_json::from_str(
            r#"{"type":"item.upsert","streamId":"str_1","item":{"id":"itm_1","type":"message","status":"running","role":"assistant","content":{"text":"hi"},"itemTime":"2025-01-01T00:00:00Z"}}"#,
        )
        .expect("parses");
        match frame {
            ConnectorFrame::ItemUpsert { stream_id, item } => {
                assert_eq!(stream_id, "str_1");
                let input = item.to_input().expect("valid");
                assert_eq!(input.id, "itm_1");
                assert!(input.role.is_some());
            }
            other => panic!("wrong variant: {other:?}"),
        }

        let frame: ConnectorFrame = serde_json::from_str(
            r#"{"type":"notice.resolved","noticeId":"ntc_1","decision":"reject"}"#,
        )
        .expect("parses");
        assert_eq!(frame.stream_id(), None);

        let frame: ConnectorFrame = serde_json::from_str(
            r#"{"type":"hello","agent":{"name":"mac","platform":"macos","version":"0.1.0"}}"#,
        )
        .expect("parses");
        match frame {
            ConnectorFrame::Hello { agent } => {
                assert_eq!(agent.name, "mac");
                assert_eq!(agent.platform.as_deref(), Some("macos"));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn an_unknown_item_enum_is_refused_not_coerced() {
        let item = IngressItem {
            id: String::from("itm_1"),
            kind: String::from("carrier_pigeon"),
            status: String::from("done"),
            role: None,
            content: Value::Null,
            item_time: None,
        };
        assert!(item.to_input().is_err());

        let item = IngressItem {
            id: String::from("itm_1"),
            kind: String::from("message"),
            status: String::from("half_done"),
            role: None,
            content: Value::Null,
            item_time: None,
        };
        assert!(item.to_input().is_err());

        let item = IngressItem {
            id: String::from("itm_1"),
            kind: String::from("message"),
            status: String::from("waiting_approval"),
            role: Some(String::from("tool")),
            content: json!({"text": "hi"}),
            item_time: None,
        };
        let input = item.to_input().expect("valid");
        assert_eq!(input.status, ItemStatus::WaitingApproval);
    }

    #[test]
    fn decisions_and_states_are_closed_sets() {
        assert!(is_known_decision("approve_for_session"));
        assert!(!is_known_decision("maybe"));
        assert!(is_known_state("waiting_approval"));
        assert!(!is_known_state("thinking"));
    }
}

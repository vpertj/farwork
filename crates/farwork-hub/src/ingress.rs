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

/// File-browsing operations the hub may ask a connector for.
pub const FS_OPS: [&str; 2] = ["list", "readText"];

/// Terminal dimensions the hub accepts, in both directions. The agent refuses
/// the same range: the hub refuses first so the browser gets a 422 instead of a
/// stream that dies on its first frame.
pub const MIN_TERMINAL_DIMENSION: u16 = 1;
pub const MAX_TERMINAL_DIMENSION: u16 = 2000;

pub fn is_valid_terminal_size(cols: u16, rows: u16) -> bool {
    (MIN_TERMINAL_DIMENSION..=MAX_TERMINAL_DIMENSION).contains(&cols)
        && (MIN_TERMINAL_DIMENSION..=MAX_TERMINAL_DIMENSION).contains(&rows)
}

/// Failure codes an `fs.response` may carry. `routes::fs` maps each one to an
/// HTTP status; an unlisted code is reported as 502 rather than guessed at.
pub const FS_ERROR_CODES: [&str; 4] = ["not_found", "permission_denied", "io_error", "too_large"];

pub fn is_known_fs_op(op: &str) -> bool {
    FS_OPS.contains(&op)
}

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
    let mut frame = json!({
        "type": "session.message",
        "streamId": stream_id,
        "text": text,
    });
    // The agent types this field as a string, so an absent id must omit the key
    // rather than serialize null: null fails the connector's decode and the whole
    // turn is dropped on the floor.
    if let Some(client_message_id) = client_message_id {
        if let Some(map) = frame.as_object_mut() {
            map.insert(
                String::from("clientMessageId"),
                Value::String(client_message_id.to_string()),
            );
        }
    }
    frame
}

pub fn session_interrupt_frame(stream_id: &str) -> Value {
    json!({ "type": "session.interrupt", "streamId": stream_id })
}

/// `fs.request`. The hub mints `requestId`; the connector must answer with that
/// exact id, which is what lets many file requests share the one socket.
pub fn fs_request_frame(request_id: &str, op: &str, path: &str) -> Value {
    json!({
        "type": "fs.request",
        "requestId": request_id,
        "op": op,
        "path": path,
    })
}

/// `terminal.open`. `cwd` and `shell` are omitted when absent — never
/// serialized as null — because the agent reads an absent key as "use the
/// workdir / $SHELL".
pub fn terminal_open_frame(
    terminal_id: &str,
    cols: u16,
    rows: u16,
    cwd: Option<&str>,
    shell: Option<&str>,
) -> Value {
    let mut frame = json!({
        "type": "terminal.open",
        "terminalId": terminal_id,
        "cols": cols,
        "rows": rows,
    });
    let map = frame.as_object_mut().expect("object literal");
    if let Some(cwd) = cwd {
        map.insert(String::from("cwd"), Value::String(cwd.to_string()));
    }
    if let Some(shell) = shell {
        map.insert(String::from("shell"), Value::String(shell.to_string()));
    }
    frame
}

/// `terminal.data`. `data` is forwarded verbatim in both directions: the hub
/// never decodes or re-encodes a PTY byte.
pub fn terminal_data_frame(terminal_id: &str, data: &str) -> Value {
    json!({
        "type": "terminal.data",
        "terminalId": terminal_id,
        "data": data,
    })
}

pub fn terminal_resize_frame(terminal_id: &str, cols: u16, rows: u16) -> Value {
    json!({
        "type": "terminal.resize",
        "terminalId": terminal_id,
        "cols": cols,
        "rows": rows,
    })
}

pub fn terminal_close_frame(terminal_id: &str) -> Value {
    json!({ "type": "terminal.close", "terminalId": terminal_id })
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
    #[serde(rename = "fs.response")]
    FsResponse(IngressFsResponse),
    #[serde(rename = "terminal.opened")]
    TerminalOpened {
        #[serde(rename = "terminalId")]
        terminal_id: String,
    },
    #[serde(rename = "terminal.data")]
    TerminalData {
        #[serde(rename = "terminalId")]
        terminal_id: String,
        /// Standard base64 with padding, relayed byte-for-byte.
        data: String,
    },
    #[serde(rename = "terminal.exit")]
    TerminalExit {
        #[serde(rename = "terminalId")]
        terminal_id: String,
        /// Absent or null both mean "no exit code" (signal death, unknown).
        #[serde(default)]
        code: Option<i64>,
    },
    /// The terminal never came up: the agent refused the cwd, or the shell could
    /// not be spawned. Relayed to the browser as `terminal.error`.
    #[serde(rename = "terminal.error")]
    TerminalError {
        #[serde(rename = "terminalId")]
        terminal_id: String,
        #[serde(default)]
        message: String,
        #[serde(default)]
        code: Option<String>,
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
            // `fs.response` is paired by requestId, not routed by stream.
            Self::FsResponse(_) | Self::Hello { .. } | Self::Heartbeat { .. } => None,
            // Terminal frames are routed by terminalId, which is not a stream.
            Self::TerminalOpened { .. }
            | Self::TerminalData { .. }
            | Self::TerminalExit { .. }
            | Self::TerminalError { .. } => None,
        }
    }

    /// The terminal this frame belongs to, when it has one.
    pub fn terminal_id(&self) -> Option<&str> {
        match self {
            Self::TerminalOpened { terminal_id }
            | Self::TerminalData { terminal_id, .. }
            | Self::TerminalExit { terminal_id, .. }
            | Self::TerminalError { terminal_id, .. } => Some(terminal_id),
            _ => None,
        }
    }
}

/// One directory entry, exactly as the connector reports it.
#[derive(Debug, Clone, Deserialize)]
pub struct FsEntry {
    pub name: String,
    pub path: String,
    /// `file` or `dir`. Required: it is the field the browser navigates on, so a
    /// reply without it is malformed rather than a file by default.
    pub kind: String,
    // Lenient on purpose: a missing size is a display detail, not a reason to
    // fail a request the device already answered.
    #[serde(default, rename = "sizeBytes")]
    pub size_bytes: u64,
    #[serde(default, rename = "modifiedAt")]
    pub modified_at: String,
}

impl FsEntry {
    pub fn view(&self) -> Value {
        json!({
            "name": self.name,
            "path": self.path,
            "kind": self.kind,
            "sizeBytes": self.size_bytes,
            "modifiedAt": self.modified_at,
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct FsErrorBody {
    pub code: String,
    pub message: String,
}

/// A connector's answer to one `fs.request`. All three shapes (list, readText,
/// failure) are one struct because they share `type: "fs.response"`.
#[derive(Debug, Clone, Deserialize)]
pub struct IngressFsResponse {
    #[serde(rename = "requestId")]
    pub request_id: String,
    pub ok: bool,
    #[serde(default)]
    pub op: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub entries: Option<Vec<FsEntry>>,
    #[serde(default, rename = "sizeBytes")]
    pub size_bytes: Option<u64>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub truncated: Option<bool>,
    #[serde(default)]
    pub error: Option<FsErrorBody>,
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
    fn session_message_frame_omits_an_absent_client_message_id() {
        let frame = session_message_frame("str_1", "hi", None);
        assert_eq!(frame["type"], "session.message");
        assert_eq!(frame["streamId"], "str_1");
        assert_eq!(frame["text"], "hi");
        // A missing id must be absent, never null: the agent parses this field as
        // a string and drops the whole turn when it sees null.
        assert!(frame.get("clientMessageId").is_none());
        let encoded = serde_json::to_string(&frame).expect("serializes");
        assert!(!encoded.contains("clientMessageId"));
        assert_eq!(
            frame.as_object().expect("object").keys().count(),
            3,
            "expected exactly type/streamId/text, got {encoded}"
        );

        let with_id = session_message_frame("str_1", "hi", Some("cm_x"));
        assert_eq!(with_id["clientMessageId"], "cm_x");
    }

    #[test]
    fn fs_request_frame_carries_the_contract_fields() {
        let frame = fs_request_frame("req_1", "readText", "/srv/farwork/a.txt");
        assert_eq!(frame["type"], "fs.request");
        assert_eq!(frame["requestId"], "req_1");
        assert_eq!(frame["op"], "readText");
        assert_eq!(frame["path"], "/srv/farwork/a.txt");
        assert_eq!(
            frame.as_object().expect("object").keys().count(),
            4,
            "expected exactly type/requestId/op/path: {frame}"
        );
        assert!(is_known_fs_op("list") && is_known_fs_op("readText"));
        assert!(!is_known_fs_op("delete"));
    }

    #[test]
    fn fs_responses_parse_in_all_three_shapes() {
        let list: ConnectorFrame = serde_json::from_str(
            r#"{"type":"fs.response","requestId":"req_1","ok":true,"op":"list","path":"/w","entries":[{"name":"a.txt","path":"/w/a.txt","kind":"file","sizeBytes":12,"modifiedAt":"2026-01-01T00:00:00Z"}]}"#,
        )
        .expect("parses");
        match list {
            ConnectorFrame::FsResponse(reply) => {
                assert_eq!(reply.request_id, "req_1");
                assert!(reply.ok);
                assert_eq!(reply.op.as_deref(), Some("list"));
                let entries = reply.entries.expect("entries");
                assert_eq!(entries[0].name, "a.txt");
                assert_eq!(entries[0].kind, "file");
                assert_eq!(entries[0].size_bytes, 12);
                assert_eq!(entries[0].modified_at, "2026-01-01T00:00:00Z");
                assert_eq!(entries[0].view()["sizeBytes"], 12);
            }
            other => panic!("wrong variant: {other:?}"),
        }

        let read: ConnectorFrame = serde_json::from_str(
            r#"{"type":"fs.response","requestId":"req_2","ok":true,"op":"readText","path":"/w/a.txt","content":"hi","sizeBytes":2,"truncated":false}"#,
        )
        .expect("parses");
        match read {
            ConnectorFrame::FsResponse(reply) => {
                assert_eq!(reply.content.as_deref(), Some("hi"));
                assert_eq!(reply.size_bytes, Some(2));
                assert_eq!(reply.truncated, Some(false));
                assert!(reply.entries.is_none());
            }
            other => panic!("wrong variant: {other:?}"),
        }

        let failure: ConnectorFrame = serde_json::from_str(
            r#"{"type":"fs.response","requestId":"req_3","ok":false,"error":{"code":"permission_denied","message":"outside the workdir"}}"#,
        )
        .expect("parses");
        match failure {
            ConnectorFrame::FsResponse(reply) => {
                assert!(!reply.ok);
                let error = reply.error.expect("error");
                assert_eq!(error.code, "permission_denied");
                assert_eq!(error.message, "outside the workdir");
            }
            other => panic!("wrong variant: {other:?}"),
        }
        assert_eq!(FS_ERROR_CODES.len(), 4);
    }

    #[test]
    fn an_fs_response_has_no_stream_to_route_on() {
        let frame: ConnectorFrame =
            serde_json::from_str(r#"{"type":"fs.response","requestId":"req_1","ok":false}"#)
                .expect("parses");
        assert_eq!(frame.stream_id(), None);
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
    fn terminal_open_frame_carries_only_the_contract_fields() {
        let frame = terminal_open_frame("t1", 80, 24, None, None);
        assert_eq!(frame["type"], "terminal.open");
        assert_eq!(frame["terminalId"], "t1");
        assert_eq!(frame["cols"], 80);
        assert_eq!(frame["rows"], 24);
        // Absent stays absent: the agent reads a missing cwd as "the workdir" and
        // a missing shell as "$SHELL". Null would be a third spelling.
        assert!(frame.get("cwd").is_none());
        assert!(frame.get("shell").is_none());
        assert_eq!(
            frame.as_object().expect("object").keys().count(),
            4,
            "expected exactly type/terminalId/cols/rows: {frame}"
        );

        let with_options = terminal_open_frame("t1", 100, 30, Some("/w/sub"), Some("/bin/bash"));
        assert_eq!(with_options["cwd"], "/w/sub");
        assert_eq!(with_options["shell"], "/bin/bash");
    }

    #[test]
    fn terminal_data_resize_and_close_frames_are_verbatim() {
        let data = terminal_data_frame("t1", "AAEC/w==");
        assert_eq!(data["type"], "terminal.data");
        assert_eq!(data["terminalId"], "t1");
        assert_eq!(
            data["data"], "AAEC/w==",
            "base64 is relayed, never re-encoded"
        );
        assert_eq!(data.as_object().expect("object").keys().count(), 3);

        let resize = terminal_resize_frame("t1", 100, 30);
        assert_eq!(resize["type"], "terminal.resize");
        assert_eq!(resize["terminalId"], "t1");
        assert_eq!(resize["cols"], 100);
        assert_eq!(resize["rows"], 30);

        let close = terminal_close_frame("t1");
        assert_eq!(close["type"], "terminal.close");
        assert_eq!(close["terminalId"], "t1");
        assert_eq!(close.as_object().expect("object").keys().count(), 2);
    }

    #[test]
    fn inbound_terminal_frames_route_by_terminal_id_not_stream() {
        let opened: ConnectorFrame =
            serde_json::from_str(r#"{"type":"terminal.opened","terminalId":"t1"}"#)
                .expect("parses");
        assert_eq!(opened.terminal_id(), Some("t1"));
        assert_eq!(opened.stream_id(), None);

        let data: ConnectorFrame =
            serde_json::from_str(r#"{"type":"terminal.data","terminalId":"t1","data":"AAEC/w=="}"#)
                .expect("parses");
        assert_eq!(data.terminal_id(), Some("t1"));
        assert_eq!(data.stream_id(), None);

        let exit: ConnectorFrame =
            serde_json::from_str(r#"{"type":"terminal.exit","terminalId":"t1","code":7}"#)
                .expect("parses");
        match exit {
            ConnectorFrame::TerminalExit { terminal_id, code } => {
                assert_eq!(terminal_id, "t1");
                assert_eq!(code, Some(7));
            }
            other => panic!("wrong variant: {other:?}"),
        }

        // A signal death reports null, and a shorter agent may omit the key: both
        // must read as "no exit code", not as a broken frame.
        for text in [
            r#"{"type":"terminal.exit","terminalId":"t1","code":null}"#,
            r#"{"type":"terminal.exit","terminalId":"t1"}"#,
        ] {
            match serde_json::from_str::<ConnectorFrame>(text).expect("parses") {
                ConnectorFrame::TerminalExit { code, .. } => assert_eq!(code, None, "{text}"),
                other => panic!("wrong variant: {other:?}"),
            }
        }

        let refused: ConnectorFrame = serde_json::from_str(
            r#"{"type":"terminal.error","terminalId":"t1","message":"/etc is outside the agent workdir /w","code":"permission_denied"}"#,
        )
        .expect("parses");
        match refused {
            ConnectorFrame::TerminalError {
                terminal_id,
                code,
                message,
            } => {
                assert_eq!(terminal_id, "t1");
                assert_eq!(code.as_deref(), Some("permission_denied"));
                assert!(message.contains("outside the agent workdir"));
            }
            other => panic!("wrong variant: {other:?}"),
        }

        // A code-less refusal is still a refusal the browser has to hear about.
        match serde_json::from_str::<ConnectorFrame>(
            r#"{"type":"terminal.error","terminalId":"t1"}"#,
        )
        .expect("parses")
        {
            ConnectorFrame::TerminalError { code, .. } => assert_eq!(code, None),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn terminal_sizes_are_bounded_in_both_directions() {
        assert!(is_valid_terminal_size(80, 24));
        assert!(is_valid_terminal_size(
            MIN_TERMINAL_DIMENSION,
            MAX_TERMINAL_DIMENSION
        ));
        assert!(!is_valid_terminal_size(0, 24));
        assert!(!is_valid_terminal_size(80, 0));
        assert!(!is_valid_terminal_size(MAX_TERMINAL_DIMENSION + 1, 24));
        assert!(!is_valid_terminal_size(80, MAX_TERMINAL_DIMENSION + 1));
    }

    #[test]
    fn decisions_and_states_are_closed_sets() {
        assert!(is_known_decision("approve_for_session"));
        assert!(!is_known_decision("maybe"));
        assert!(is_known_state("waiting_approval"));
        assert!(!is_known_state("thinking"));
    }
}

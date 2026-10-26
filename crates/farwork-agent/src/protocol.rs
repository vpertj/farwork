//! Ingress Protocol v1: the exact JSON text frames exchanged over
//! GET /api/v2/connector/ws?token=...
//!
//! Field names are camelCase on the wire and every frame is discriminated by
//! 'type'. Unknown frames must be dropped without closing the socket, so parsing
//! deliberately distinguishes "unknown type" from "known type, bad payload".

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Timeline item type, as understood by farwork-timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemType {
    Message,
    Tool,
    Artifact,
    Marker,
    System,
}

/// Timeline item status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    Pending,
    Running,
    WaitingApproval,
    Done,
    Failed,
    Cancelled,
    Interrupted,
}

/// Timeline item author role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    System,
    Tool,
}

/// Runtime state reported for a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Running,
    Idle,
    WaitingApproval,
    Error,
}

/// Human approval decision. Both directions use the same vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Approve,
    ApproveForSession,
    Reject,
}

/// Notice kind. 'permissions' is part of the contract, but this agent answers
/// those codex requests with a JSON-RPC error instead of fabricating a grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    CommandExecution,
    FileChange,
    Permissions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub name: String,
    pub platform: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemEnvelope {
    pub id: String,
    #[serde(rename = "type")]
    pub item_type: ItemType,
    pub status: ItemStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    pub content: Value,
    #[serde(rename = "itemTime")]
    pub item_time: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoticeEnvelope {
    pub id: String,
    pub kind: NoticeKind,
    pub status: String,
    pub title: String,
    pub body: Value,
    pub options: Vec<String>,
}

impl NoticeEnvelope {
    /// The only status an agent ever opens a notice with; resolution is
    /// reported through notice.resolved, not by mutating this envelope.
    pub const OPEN: &'static str = "open";
}

/// `fs.request.op`. Only these two exist: a third spelling is a broken frame,
/// not a future-proof extension, and the agent must refuse it rather than guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FsOp {
    List,
    ReadText,
}

/// Directory entry kind. The contract has exactly two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FsEntryKind {
    File,
    Dir,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsEntry {
    pub name: String,
    pub path: String,
    pub kind: FsEntryKind,
    #[serde(rename = "sizeBytes")]
    pub size_bytes: u64,
    #[serde(rename = "modifiedAt")]
    pub modified_at: String,
}

/// Failure codes the hub maps onto HTTP statuses. Closed set: an unmapped code
/// would become a 502 with no explanation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FsErrorCode {
    NotFound,
    PermissionDenied,
    IoError,
    TooLarge,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsErrorBody {
    pub code: FsErrorCode,
    pub message: String,
}

/// `fs.response` payload. The frame is a union on the wire (list reply, readText
/// reply, or failure), so it is one struct with absent halves rather than an
/// enum: all three carry `type: "fs.response"`, which a tagged enum cannot
/// express twice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsResponse {
    #[serde(rename = "requestId")]
    pub request_id: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<FsOp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entries: Option<Vec<FsEntry>>,
    #[serde(default, rename = "sizeBytes", skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<FsErrorBody>,
}

impl FsResponse {
    pub fn list(
        request_id: impl Into<String>,
        path: impl Into<String>,
        entries: Vec<FsEntry>,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            ok: true,
            op: Some(FsOp::List),
            path: Some(path.into()),
            entries: Some(entries),
            size_bytes: None,
            content: None,
            truncated: None,
            error: None,
        }
    }

    pub fn read_text(
        request_id: impl Into<String>,
        path: impl Into<String>,
        content: impl Into<String>,
        size_bytes: u64,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            ok: true,
            op: Some(FsOp::ReadText),
            path: Some(path.into()),
            entries: None,
            size_bytes: Some(size_bytes),
            content: Some(content.into()),
            // Never true: an oversized file is refused with too_large rather than
            // returned short and passed off as whole.
            truncated: Some(false),
            error: None,
        }
    }

    pub fn failure(
        request_id: impl Into<String>,
        code: FsErrorCode,
        message: impl Into<String>,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            ok: false,
            op: None,
            path: None,
            entries: None,
            size_bytes: None,
            content: None,
            truncated: None,
            error: Some(FsErrorBody {
                code,
                message: message.into(),
            }),
        }
    }
}

/// What the runtime reports it can do `right now` (Ingress Protocol v1.3).
///
/// Every flag is `default`: a field the runtime does not mention reads as "not
/// supported", never as "supported". A hub that filled in the missing ones would
/// turn a silent absence into a control that does nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityFlags {
    #[serde(default)]
    pub steer: bool,
    #[serde(default)]
    pub interrupt: bool,
    #[serde(default)]
    pub selections: bool,
    #[serde(default)]
    pub commands: bool,
}

/// One entry of a model or permission catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogItem {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl CatalogItem {
    pub fn new(id: impl Into<String>, name: impl Into<String>, description: Option<&str>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.map(str::to_string),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CatalogSet {
    #[serde(default)]
    pub models: Vec<CatalogItem>,
    #[serde(default)]
    pub permissions: Vec<CatalogItem>,
}

/// One runtime command the bridge can execute.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandEntry {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl CommandEntry {
    pub fn new(name: impl Into<String>, description: Option<&str>) -> Self {
        Self {
            name: name.into(),
            description: description.map(str::to_string),
        }
    }
}

/// Connector -> hub frames.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientFrame {
    #[serde(rename = "hello")]
    Hello { agent: AgentInfo },
    #[serde(rename = "heartbeat")]
    Heartbeat { ts: String },
    #[serde(rename = "session.started")]
    SessionStarted {
        #[serde(rename = "streamId")]
        stream_id: String,
        #[serde(rename = "externalSessionId")]
        external_session_id: String,
    },
    #[serde(rename = "session.state")]
    SessionStateFrame {
        #[serde(rename = "streamId")]
        stream_id: String,
        state: SessionState,
    },
    #[serde(rename = "session.exit")]
    SessionExit {
        #[serde(rename = "streamId")]
        stream_id: String,
        code: i32,
    },
    #[serde(rename = "session.capabilities")]
    SessionCapabilities {
        #[serde(rename = "streamId")]
        stream_id: String,
        capabilities: CapabilityFlags,
        catalogs: CatalogSet,
        commands: Vec<CommandEntry>,
    },
    #[serde(rename = "item.upsert")]
    ItemUpsert {
        #[serde(rename = "streamId")]
        stream_id: String,
        item: ItemEnvelope,
    },
    #[serde(rename = "notice.upsert")]
    NoticeUpsert {
        #[serde(rename = "streamId")]
        stream_id: String,
        notice: NoticeEnvelope,
    },
    #[serde(rename = "notice.resolved")]
    NoticeResolved {
        #[serde(rename = "noticeId")]
        notice_id: String,
        decision: Decision,
    },
    #[serde(rename = "fs.response")]
    FsResponse(FsResponse),
    #[serde(rename = "terminal.opened")]
    TerminalOpened {
        #[serde(rename = "terminalId")]
        terminal_id: String,
    },
    #[serde(rename = "terminal.data")]
    TerminalData {
        #[serde(rename = "terminalId")]
        terminal_id: String,
        /// Standard base64 with padding. Bytes, never text: a UTF-8
        /// assumption here corrupts every frame that is not ASCII.
        data: String,
    },
    #[serde(rename = "terminal.exit")]
    TerminalExit {
        #[serde(rename = "terminalId")]
        terminal_id: String,
        /// null when the shell died from a signal or the code is unknown.
        /// The key is always present: the contract says code may be null, and
        /// omitting it would be a third spelling for the same fact.
        code: Option<i32>,
    },
    /// The terminal never came up (sandbox refusal, spawn failure). The
    /// browser side of the contract has terminal.error; the agent needs the
    /// same frame so a refused terminal.open is not indistinguishable from
    /// silence.
    #[serde(rename = "terminal.error")]
    TerminalError {
        #[serde(rename = "terminalId")]
        terminal_id: String,
        message: String,
        code: String,
    },
    #[serde(rename = "error")]
    Error {
        message: String,
        #[serde(rename = "streamId", skip_serializing_if = "Option::is_none")]
        stream_id: Option<String>,
    },
}

/// Hub -> connector frames.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerFrame {
    #[serde(rename = "welcome")]
    Welcome {
        #[serde(rename = "serverVersion")]
        server_version: String,
        #[serde(rename = "heartbeatSecs")]
        heartbeat_secs: u64,
    },
    #[serde(rename = "session.start")]
    SessionStart {
        #[serde(rename = "streamId")]
        stream_id: String,
        #[serde(rename = "sessionId")]
        session_id: String,
        runtime: String,
        cwd: String,
        prompt: String,
    },
    #[serde(rename = "session.message")]
    SessionMessage {
        #[serde(rename = "streamId")]
        stream_id: String,
        text: String,
        // Absent or null both mean "the caller did not supply one": the hub omits
        // the key in that case, and dropping the turn over a missing idempotency
        // token would lose a message the hub already recorded.
        #[serde(
            default,
            rename = "clientMessageId",
            skip_serializing_if = "Option::is_none"
        )]
        client_message_id: Option<String>,
    },
    #[serde(rename = "session.interrupt")]
    SessionInterrupt {
        #[serde(rename = "streamId")]
        stream_id: String,
    },
    /// One more instruction for the turn that is already running.
    #[serde(rename = "session.steer")]
    SessionSteer {
        #[serde(rename = "streamId")]
        stream_id: String,
        // Tolerated as absent so a steer without text is dropped with a warning
        // here rather than taking the whole frame decode down with it: the
        // connection must survive a peer's malformed frame.
        #[serde(default)]
        text: String,
    },
    /// Change the model and/or the permission mode of a running session. Absent
    /// keys mean "keep the current value" and are never sent as null.
    #[serde(rename = "session.selections")]
    SessionSelections {
        #[serde(rename = "streamId")]
        stream_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(
            default,
            rename = "permissionMode",
            skip_serializing_if = "Option::is_none"
        )]
        permission_mode: Option<String>,
    },
    #[serde(rename = "notice.respond")]
    NoticeRespond {
        #[serde(rename = "noticeId")]
        notice_id: String,
        decision: Decision,
    },
    #[serde(rename = "fs.request")]
    FsRequest {
        #[serde(rename = "requestId")]
        request_id: String,
        op: FsOp,
        path: String,
    },
    #[serde(rename = "terminal.open")]
    TerminalOpen {
        #[serde(rename = "terminalId")]
        terminal_id: String,
        cols: u16,
        rows: u16,
        /// Absent means "the agent --workdir". Present means an absolute
        /// path that must resolve inside that sandbox.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        /// Per-terminal override; absent falls back to --shell, then $SHELL.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shell: Option<String>,
    },
    #[serde(rename = "terminal.data")]
    TerminalData {
        #[serde(rename = "terminalId")]
        terminal_id: String,
        data: String,
    },
    #[serde(rename = "terminal.resize")]
    TerminalResize {
        #[serde(rename = "terminalId")]
        terminal_id: String,
        cols: u16,
        rows: u16,
    },
    #[serde(rename = "terminal.close")]
    TerminalClose {
        #[serde(rename = "terminalId")]
        terminal_id: String,
    },
    #[serde(rename = "error")]
    Error {
        message: String,
        #[serde(rename = "streamId", skip_serializing_if = "Option::is_none")]
        stream_id: Option<String>,
    },
}

impl ClientFrame {
    pub fn encode(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

/// Outcome of decoding one inbound hub frame.
#[derive(Debug, Clone, PartialEq)]
pub enum ParsedServerFrame {
    Frame(ServerFrame),
    /// Unknown discriminator: drop the frame, keep the connection.
    UnknownType(String),
    /// Known discriminator with a payload that does not match the contract.
    Malformed {
        ty: Option<String>,
        error: String,
    },
}

/// Decode one hub -> connector text frame.
///
/// The discriminator is read before the typed decode so an unknown frame is
/// reported as UnknownType instead of being indistinguishable from a broken
/// known frame.
pub fn parse_server_frame(text: &str) -> ParsedServerFrame {
    let value: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(error) => {
            return ParsedServerFrame::Malformed {
                ty: None,
                error: error.to_string(),
            }
        }
    };
    let Some(ty) = value.get("type").and_then(Value::as_str).map(str::to_owned) else {
        return ParsedServerFrame::Malformed {
            ty: None,
            error: String::from("missing type"),
        };
    };
    if !KNOWN_SERVER_TYPES.contains(&ty.as_str()) {
        return ParsedServerFrame::UnknownType(ty);
    }
    match serde_json::from_value::<ServerFrame>(value) {
        Ok(frame) => ParsedServerFrame::Frame(frame),
        Err(error) => ParsedServerFrame::Malformed {
            ty: Some(ty),
            error: error.to_string(),
        },
    }
}

const KNOWN_SERVER_TYPES: &[&str] = &[
    "welcome",
    "session.start",
    "session.message",
    "session.interrupt",
    "session.steer",
    "session.selections",
    "notice.respond",
    "fs.request",
    "terminal.open",
    "terminal.data",
    "terminal.resize",
    "terminal.close",
    "error",
];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hello_round_trips_with_camel_case_fields() {
        let frame = ClientFrame::Hello {
            agent: AgentInfo {
                name: String::from("mbp"),
                platform: String::from("macos"),
                version: String::from("0.1.0"),
            },
        };
        let encoded = frame.encode().expect("encode");
        assert_eq!(
            encoded,
            r#"{"type":"hello","agent":{"name":"mbp","platform":"macos","version":"0.1.0"}}"#
        );
        assert_eq!(
            serde_json::from_str::<ClientFrame>(&encoded).unwrap(),
            frame
        );
    }

    #[test]
    fn item_upsert_keeps_agent_supplied_id_and_arbitrary_content() {
        let frame = ClientFrame::ItemUpsert {
            stream_id: String::from("str_1"),
            item: ItemEnvelope {
                id: String::from("itm_1"),
                item_type: ItemType::Tool,
                status: ItemStatus::WaitingApproval,
                role: Some(Role::Tool),
                content: json!({"command": "ls", "exitCode": null}),
                item_time: String::from("2026-01-01T00:00:00.000000Z"),
            },
        };
        let value: Value = serde_json::from_str(&frame.encode().unwrap()).unwrap();
        assert_eq!(value["type"], json!("item.upsert"));
        assert_eq!(value["streamId"], json!("str_1"));
        assert_eq!(value["item"]["type"], json!("tool"));
        assert_eq!(value["item"]["status"], json!("waiting_approval"));
        assert_eq!(value["item"]["role"], json!("tool"));
        assert_eq!(value["item"]["content"]["command"], json!("ls"));
        assert_eq!(
            value["item"]["itemTime"],
            json!("2026-01-01T00:00:00.000000Z")
        );
    }

    #[test]
    fn error_frame_omits_absent_stream_id() {
        let frame = ClientFrame::Error {
            message: String::from("boom"),
            stream_id: None,
        };
        assert_eq!(
            frame.encode().unwrap(),
            r#"{"type":"error","message":"boom"}"#
        );
        let with_stream = ClientFrame::Error {
            message: String::from("boom"),
            stream_id: Some(String::from("str_1")),
        };
        assert_eq!(
            with_stream.encode().unwrap(),
            r#"{"type":"error","message":"boom","streamId":"str_1"}"#
        );
    }

    #[test]
    fn session_start_parses_from_hub_shaped_json() {
        let text = r#"{"type":"session.start","streamId":"str_1","sessionId":"sess_1","runtime":"codex","cwd":"/tmp","prompt":"hi"}"#;
        match parse_server_frame(text) {
            ParsedServerFrame::Frame(ServerFrame::SessionStart {
                stream_id,
                session_id,
                runtime,
                cwd,
                prompt,
            }) => {
                assert_eq!(stream_id, "str_1");
                assert_eq!(session_id, "sess_1");
                assert_eq!(runtime, "codex");
                assert_eq!(cwd, "/tmp");
                assert_eq!(prompt, "hi");
            }
            other => panic!("unexpected parse: {other:?}"),
        }
    }

    #[test]
    fn session_message_parses_without_a_client_message_id() {
        let text = r#"{"type":"session.message","streamId":"s1","text":"hi"}"#;
        match parse_server_frame(text) {
            ParsedServerFrame::Frame(ServerFrame::SessionMessage {
                stream_id,
                text,
                client_message_id,
            }) => {
                assert_eq!(stream_id, "s1");
                assert_eq!(text, "hi");
                assert_eq!(client_message_id, None);
            }
            other => panic!("unexpected parse: {other:?}"),
        }
    }

    #[test]
    fn session_message_parses_a_present_client_message_id() {
        let text =
            r#"{"type":"session.message","streamId":"s1","text":"hi","clientMessageId":"cm_x"}"#;
        match parse_server_frame(text) {
            ParsedServerFrame::Frame(ServerFrame::SessionMessage {
                client_message_id, ..
            }) => assert_eq!(client_message_id.as_deref(), Some("cm_x")),
            other => panic!("unexpected parse: {other:?}"),
        }

        let null_text =
            r#"{"type":"session.message","streamId":"s1","text":"hi","clientMessageId":null}"#;
        match parse_server_frame(null_text) {
            ParsedServerFrame::Frame(ServerFrame::SessionMessage {
                client_message_id, ..
            }) => assert_eq!(client_message_id, None),
            other => panic!("unexpected parse: {other:?}"),
        }
    }

    #[test]
    fn fs_request_parses_and_a_bad_op_is_malformed() {
        let text = r#"{"type":"fs.request","requestId":"req_1","op":"readText","path":"/w/a.txt"}"#;
        match parse_server_frame(text) {
            ParsedServerFrame::Frame(ServerFrame::FsRequest {
                request_id,
                op,
                path,
            }) => {
                assert_eq!(request_id, "req_1");
                assert_eq!(op, FsOp::ReadText);
                assert_eq!(path, "/w/a.txt");
            }
            other => panic!("unexpected parse: {other:?}"),
        }

        let list = r#"{"type":"fs.request","requestId":"req_2","op":"list","path":"/w"}"#;
        match parse_server_frame(list) {
            ParsedServerFrame::Frame(ServerFrame::FsRequest { op, .. }) => {
                assert_eq!(op, FsOp::List)
            }
            other => panic!("unexpected parse: {other:?}"),
        }

        // An op outside the closed set must not be coerced into one of the two:
        // it is a known type with a payload the contract does not describe.
        let bogus = r#"{"type":"fs.request","requestId":"req_3","op":"delete","path":"/w"}"#;
        match parse_server_frame(bogus) {
            ParsedServerFrame::Malformed { ty, .. } => {
                assert_eq!(ty.as_deref(), Some("fs.request"))
            }
            other => panic!("unexpected parse: {other:?}"),
        }
    }

    #[test]
    fn fs_response_frames_use_the_contract_field_names() {
        let list = ClientFrame::FsResponse(FsResponse::list(
            "req_1",
            "/w",
            vec![FsEntry {
                name: String::from("a.txt"),
                path: String::from("/w/a.txt"),
                kind: FsEntryKind::File,
                size_bytes: 3,
                modified_at: String::from("2026-01-01T00:00:00Z"),
            }],
        ));
        assert_eq!(
            list.encode().expect("encode"),
            r#"{"type":"fs.response","requestId":"req_1","ok":true,"op":"list","path":"/w","entries":[{"name":"a.txt","path":"/w/a.txt","kind":"file","sizeBytes":3,"modifiedAt":"2026-01-01T00:00:00Z"}]}"#
        );

        let text = ClientFrame::FsResponse(FsResponse::read_text("req_2", "/w/a.txt", "abc", 3));
        assert_eq!(
            text.encode().expect("encode"),
            r#"{"type":"fs.response","requestId":"req_2","ok":true,"op":"readText","path":"/w/a.txt","sizeBytes":3,"content":"abc","truncated":false}"#
        );

        let failure = ClientFrame::FsResponse(FsResponse::failure(
            "req_3",
            FsErrorCode::PermissionDenied,
            "denied",
        ));
        assert_eq!(
            failure.encode().expect("encode"),
            r#"{"type":"fs.response","requestId":"req_3","ok":false,"error":{"code":"permission_denied","message":"denied"}}"#
        );
    }

    #[test]
    fn terminal_open_parses_with_and_without_its_optional_fields() {
        let full = r#"{"type":"terminal.open","terminalId":"t1","cols":80,"rows":24,"cwd":"/w/sub","shell":"/bin/bash"}"#;
        match parse_server_frame(full) {
            ParsedServerFrame::Frame(ServerFrame::TerminalOpen {
                terminal_id,
                cols,
                rows,
                cwd,
                shell,
            }) => {
                assert_eq!(terminal_id, "t1");
                assert_eq!((cols, rows), (80, 24));
                assert_eq!(cwd.as_deref(), Some("/w/sub"));
                assert_eq!(shell.as_deref(), Some("/bin/bash"));
            }
            other => panic!("unexpected parse: {other:?}"),
        }

        // Omitted cwd/shell mean "workdir" and "$SHELL": absence must not be a
        // decode error, and an explicit null must read the same way.
        let minimal = r#"{"type":"terminal.open","terminalId":"t2","cols":100,"rows":30}"#;
        match parse_server_frame(minimal) {
            ParsedServerFrame::Frame(ServerFrame::TerminalOpen { cwd, shell, .. }) => {
                assert_eq!(cwd, None);
                assert_eq!(shell, None);
            }
            other => panic!("unexpected parse: {other:?}"),
        }
        let nulls = r#"{"type":"terminal.open","terminalId":"t3","cols":1,"rows":1,"cwd":null,"shell":null}"#;
        match parse_server_frame(nulls) {
            ParsedServerFrame::Frame(ServerFrame::TerminalOpen { cwd, shell, .. }) => {
                assert_eq!(cwd, None);
                assert_eq!(shell, None);
            }
            other => panic!("unexpected parse: {other:?}"),
        }
    }

    #[test]
    fn terminal_data_and_resize_and_close_parse_by_type() {
        let data = r#"{"type":"terminal.data","terminalId":"t1","data":"AAEC/w=="}"#;
        match parse_server_frame(data) {
            ParsedServerFrame::Frame(ServerFrame::TerminalData { terminal_id, data }) => {
                assert_eq!(terminal_id, "t1");
                assert_eq!(data, "AAEC/w==");
            }
            other => panic!("unexpected parse: {other:?}"),
        }

        let resize = r#"{"type":"terminal.resize","terminalId":"t1","cols":100,"rows":30}"#;
        match parse_server_frame(resize) {
            ParsedServerFrame::Frame(ServerFrame::TerminalResize { cols, rows, .. }) => {
                assert_eq!((cols, rows), (100, 30));
            }
            other => panic!("unexpected parse: {other:?}"),
        }

        let close = r#"{"type":"terminal.close","terminalId":"t1"}"#;
        assert_eq!(
            parse_server_frame(close),
            ParsedServerFrame::Frame(ServerFrame::TerminalClose {
                terminal_id: String::from("t1"),
            })
        );
    }

    #[test]
    fn agent_terminal_frames_use_the_contract_field_names() {
        assert_eq!(
            ClientFrame::TerminalOpened {
                terminal_id: String::from("t1"),
            }
            .encode()
            .expect("encode"),
            r#"{"type":"terminal.opened","terminalId":"t1"}"#
        );
        assert_eq!(
            ClientFrame::TerminalData {
                terminal_id: String::from("t1"),
                data: String::from("AAEC/w=="),
            }
            .encode()
            .expect("encode"),
            r#"{"type":"terminal.data","terminalId":"t1","data":"AAEC/w=="}"#
        );
        // A signal death reports null, and the key stays present.
        assert_eq!(
            ClientFrame::TerminalExit {
                terminal_id: String::from("t1"),
                code: None,
            }
            .encode()
            .expect("encode"),
            r#"{"type":"terminal.exit","terminalId":"t1","code":null}"#
        );
        assert_eq!(
            ClientFrame::TerminalExit {
                terminal_id: String::from("t1"),
                code: Some(7),
            }
            .encode()
            .expect("encode"),
            r#"{"type":"terminal.exit","terminalId":"t1","code":7}"#
        );
        assert_eq!(
            ClientFrame::TerminalError {
                terminal_id: String::from("t1"),
                message: String::from("/etc is outside the agent workdir /w"),
                code: String::from("permission_denied"),
            }
            .encode()
            .expect("encode"),
            r#"{"type":"terminal.error","terminalId":"t1","message":"/etc is outside the agent workdir /w","code":"permission_denied"}"#
        );
    }

    #[test]
    fn unknown_frame_type_is_reported_and_not_fatal() {
        let parsed = parse_server_frame(r#"{"type":"future.frame","whatever":1}"#);
        assert_eq!(
            parsed,
            ParsedServerFrame::UnknownType(String::from("future.frame"))
        );
    }

    #[test]
    fn known_type_with_bad_payload_is_malformed_not_unknown() {
        let parsed = parse_server_frame(r#"{"type":"notice.respond","noticeId":7}"#);
        match parsed {
            ParsedServerFrame::Malformed { ty, .. } => {
                assert_eq!(ty.as_deref(), Some("notice.respond"))
            }
            other => panic!("unexpected parse: {other:?}"),
        }
    }

    #[test]
    fn garbage_and_missing_type_are_malformed() {
        assert!(matches!(
            parse_server_frame("not json"),
            ParsedServerFrame::Malformed { ty: None, .. }
        ));
        assert!(matches!(
            parse_server_frame(r#"{"streamId":"str_1"}"#),
            ParsedServerFrame::Malformed { ty: None, .. }
        ));
    }

    #[test]
    fn extra_unknown_fields_are_tolerated() {
        let text =
            r#"{"type":"welcome","serverVersion":"1.2.3","heartbeatSecs":15,"extra":{"a":1}}"#;
        match parse_server_frame(text) {
            ParsedServerFrame::Frame(ServerFrame::Welcome {
                server_version,
                heartbeat_secs,
            }) => {
                assert_eq!(server_version, "1.2.3");
                assert_eq!(heartbeat_secs, 15);
            }
            other => panic!("unexpected parse: {other:?}"),
        }
    }

    #[test]
    fn session_steer_and_selections_parse_as_v1_3_frames() {
        match parse_server_frame(
            r#"{"type":"session.steer","streamId":"str_1","text":"also run the tests"}"#,
        ) {
            ParsedServerFrame::Frame(ServerFrame::SessionSteer { stream_id, text }) => {
                assert_eq!(stream_id, "str_1");
                assert_eq!(text, "also run the tests");
            }
            other => panic!("unexpected parse: {other:?}"),
        }
        match parse_server_frame(r#"{"type":"session.selections","streamId":"str_1","model":"m1"}"#)
        {
            ParsedServerFrame::Frame(ServerFrame::SessionSelections {
                stream_id,
                model,
                permission_mode,
            }) => {
                assert_eq!(stream_id, "str_1");
                assert_eq!(model.as_deref(), Some("m1"));
                // Absent stays absent: a missing key must never become null, which
                // a runtime would have to interpret as "clear the current value".
                assert!(permission_mode.is_none());
            }
            other => panic!("unexpected parse: {other:?}"),
        }
        match parse_server_frame(
            r#"{"type":"session.selections","streamId":"str_1","permissionMode":"read-only"}"#,
        ) {
            ParsedServerFrame::Frame(ServerFrame::SessionSelections {
                model,
                permission_mode,
                ..
            }) => {
                assert!(model.is_none());
                assert_eq!(permission_mode.as_deref(), Some("read-only"));
            }
            other => panic!("unexpected parse: {other:?}"),
        }
    }

    #[test]
    fn a_steer_without_text_parses_so_the_router_can_drop_it_with_a_warning() {
        // Dropping the whole frame decode would take the connection down over a
        // peer's malformed frame; an empty instruction is refused one layer up.
        match parse_server_frame(r#"{"type":"session.steer","streamId":"str_1"}"#) {
            ParsedServerFrame::Frame(ServerFrame::SessionSteer { stream_id, text }) => {
                assert_eq!(stream_id, "str_1");
                assert!(text.is_empty());
            }
            other => panic!("unexpected parse: {other:?}"),
        }
    }

    #[test]
    fn revisions_of_the_frame_set_do_not_swallow_an_unknown_type() {
        assert_eq!(
            parse_server_frame(r#"{"type":"session.capabilities","streamId":"str_1"}"#),
            ParsedServerFrame::UnknownType(String::from("session.capabilities")),
            "session.capabilities only flows hub-ward: from a hub it is an unknown frame"
        );
        assert_eq!(
            parse_server_frame(r#"{"type":"session.nonsense","streamId":"str_1"}"#),
            ParsedServerFrame::UnknownType(String::from("session.nonsense"))
        );
    }
}

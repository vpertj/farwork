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
        #[serde(rename = "clientMessageId")]
        client_message_id: String,
    },
    #[serde(rename = "session.interrupt")]
    SessionInterrupt {
        #[serde(rename = "streamId")]
        stream_id: String,
    },
    #[serde(rename = "notice.respond")]
    NoticeRespond {
        #[serde(rename = "noticeId")]
        notice_id: String,
        decision: Decision,
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
    "notice.respond",
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
}

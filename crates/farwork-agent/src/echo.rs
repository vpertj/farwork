//! Echo runtime: a deliberately fake agent used for hub smoke tests.
//!
//! It is only reachable with --runtime echo and it says so in its own output; it
//! must never be substituted for codex. Its job is to exercise the ingress
//! contract (started / capabilities / item / notice / respond / steer / exit)
//! against a real hub.

use std::time::Duration;

use serde_json::json;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::time::timeout;

use crate::codex::session::{ExitReport, SessionCmd};
use crate::ids;
use crate::protocol::{
    CapabilityFlags, CatalogItem, CatalogSet, ClientFrame, CommandEntry, Decision, ItemEnvelope,
    ItemStatus, ItemType, NoticeEnvelope, NoticeKind, Role, SessionState,
};
use crate::timeutil;

pub const DECISION_TIMEOUT: Duration = Duration::from_secs(60);
pub const NOTICE_ID_TAG: &str = "echo";

/// What echo can do: everything the v1.3 contract has a frame for, because it
/// implements all of it. The catalogs are echo's own placeholders — they must
/// never be mistaken for real models, and the ids say so.
pub fn capabilities_frame(stream_id: &str) -> ClientFrame {
    ClientFrame::SessionCapabilities {
        stream_id: stream_id.to_string(),
        capabilities: CapabilityFlags {
            steer: true,
            interrupt: true,
            selections: true,
            commands: true,
        },
        catalogs: CatalogSet {
            models: vec![
                CatalogItem::new(
                    "echo-fast",
                    "Echo fast",
                    Some("echo placeholder model: it echoes, it does not think"),
                ),
                CatalogItem::new(
                    "echo-slow",
                    "Echo slow",
                    Some("echo placeholder model: identical to echo-fast, listed to exercise the picker"),
                ),
            ],
            permissions: vec![
                CatalogItem::new(
                    "default",
                    "Default",
                    Some("echo never acts, so every mode behaves the same"),
                ),
                CatalogItem::new(
                    "read-only",
                    "Read only",
                    Some("echo never acts, so every mode behaves the same"),
                ),
            ],
        },
        commands: vec![
            CommandEntry::new("/help", Some("list the echo runtime's commands")),
            CommandEntry::new("/clear", Some("clear the echo runtime's timeline")),
        ],
    }
}

pub struct EchoSession {
    stream_id: String,
    prompt: String,
    out: UnboundedSender<ClientFrame>,
    cmd: UnboundedReceiver<SessionCmd>,
    /// Last state reported, so `session.selections` can answer with the unchanged
    /// state instead of guessing one.
    state: SessionState,
    /// Last accepted selection. Echo does not act on it; it records it so the log
    /// shows what the browser asked for.
    selections: (Option<String>, Option<String>),
}

impl EchoSession {
    pub fn new(
        stream_id: String,
        prompt: String,
        out: UnboundedSender<ClientFrame>,
        cmd: UnboundedReceiver<SessionCmd>,
    ) -> Self {
        Self {
            stream_id,
            prompt,
            out,
            cmd,
            state: SessionState::Idle,
            selections: (None, None),
        }
    }

    pub async fn run(mut self) -> ExitReport {
        self.send(ClientFrame::SessionStarted {
            stream_id: self.stream_id.clone(),
            external_session_id: format!("echo-{}", self.stream_id),
        });
        // Straight after session.started, as v1.3 asks: the browser cannot decide
        // which controls to render until it knows what this runtime supports.
        self.send(capabilities_frame(&self.stream_id));
        self.state(SessionState::Running);
        self.message(format!("echo: {}", self.prompt));

        let notice_id = ids::notice_id();
        self.send(ClientFrame::NoticeUpsert {
            stream_id: self.stream_id.clone(),
            notice: NoticeEnvelope {
                id: notice_id.clone(),
                kind: NoticeKind::CommandExecution,
                status: String::from(NoticeEnvelope::OPEN),
                title: String::from("Echo runtime sample approval"),
                body: json!({
                    "runtime": "echo",
                    "sample": true,
                    "reason": "echo never executes anything; this notice only exercises the round trip",
                }),
                options: vec![
                    String::from("approve"),
                    String::from("approve_for_session"),
                    String::from("reject"),
                ],
            },
        });
        self.state(SessionState::WaitingApproval);

        loop {
            let command = match timeout(DECISION_TIMEOUT, self.cmd.recv()).await {
                Ok(Some(command)) => command,
                Ok(None) => {
                    self.send(ClientFrame::NoticeResolved {
                        notice_id,
                        decision: Decision::Reject,
                    });
                    return self.exit(1, "hub command channel closed");
                }
                Err(_) => {
                    self.send(ClientFrame::NoticeResolved {
                        notice_id,
                        decision: Decision::Reject,
                    });
                    self.send(ClientFrame::Error {
                        message: String::from("echo runtime: no decision before the timeout"),
                        stream_id: Some(self.stream_id.clone()),
                    });
                    return self.exit(0, "decision timeout");
                }
            };
            match command {
                SessionCmd::NoticeDecision {
                    notice_id: answered,
                    decision,
                } if answered == notice_id => {
                    self.send(ClientFrame::NoticeResolved {
                        notice_id,
                        decision,
                    });
                    self.message(format!("echo: decision recorded as {decision:?}"));
                    self.state(SessionState::Idle);
                    return self.exit(0, "decision recorded");
                }
                SessionCmd::Interrupt => {
                    self.send(ClientFrame::NoticeResolved {
                        notice_id,
                        decision: Decision::Reject,
                    });
                    self.state(SessionState::Idle);
                    return self.exit(130, "interrupted");
                }
                SessionCmd::Message { text, .. } => {
                    self.message(format!("echo: {text}"));
                }
                SessionCmd::Steer { text } => {
                    // A steer is a timeline event: the instruction is written as a
                    // user item (so the transcript shows what was asked) and
                    // answered with an assistant item (so the round trip is
                    // visible without a second channel).
                    self.user_message(text.clone());
                    self.message(format!("echo: steer received — {text}"));
                }
                SessionCmd::Selections {
                    model,
                    permission_mode,
                } => {
                    tracing::info!(
                        stream_id = %self.stream_id,
                        model = model.as_deref().unwrap_or("-"),
                        permission_mode = permission_mode.as_deref().unwrap_or("-"),
                        "echo recorded a selection"
                    );
                    self.selections = (model, permission_mode);
                    // The state is unchanged by a selection, so the same one is
                    // re-sent: the browser learns the change landed without echo
                    // claiming a transition that never happened.
                    self.send_state();
                }
                SessionCmd::NoticeDecision { .. } => {
                    self.send(ClientFrame::Error {
                        message: String::from("echo runtime: notice id did not match"),
                        stream_id: Some(self.stream_id.clone()),
                    });
                }
            }
        }
    }

    fn message(&self, text: String) {
        self.send(ClientFrame::ItemUpsert {
            stream_id: self.stream_id.clone(),
            item: ItemEnvelope {
                id: ids::item_id(),
                item_type: ItemType::Message,
                status: ItemStatus::Done,
                role: Some(Role::Assistant),
                content: json!({"text": text, "runtime": "echo"}),
                item_time: timeutil::utc_now(),
            },
        });
    }

    /// The user's own words, written into the timeline the way a message would be.
    fn user_message(&self, text: String) {
        self.send(ClientFrame::ItemUpsert {
            stream_id: self.stream_id.clone(),
            item: ItemEnvelope {
                id: ids::item_id(),
                item_type: ItemType::Message,
                status: ItemStatus::Done,
                role: Some(Role::User),
                content: json!({"text": text, "steer": true, "runtime": "echo"}),
                item_time: timeutil::utc_now(),
            },
        });
    }

    fn state(&mut self, state: SessionState) {
        self.state = state;
        self.send_state();
    }

    fn send_state(&self) {
        self.send(ClientFrame::SessionStateFrame {
            stream_id: self.stream_id.clone(),
            state: self.state,
        });
    }

    fn exit(&self, code: i32, reason: &str) -> ExitReport {
        self.send(ClientFrame::SessionExit {
            stream_id: self.stream_id.clone(),
            code,
        });
        ExitReport {
            code,
            reason: String::from(reason),
        }
    }

    fn send(&self, frame: ClientFrame) {
        if self.out.send(frame).is_err() {
            tracing::debug!(target: "farwork_agent::hub", "hub channel closed; dropping frame");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::unbounded_channel;

    fn collect(frames: &[ClientFrame]) -> Vec<&'static str> {
        frames
            .iter()
            .map(|frame| match frame {
                ClientFrame::SessionStarted { .. } => "session.started",
                ClientFrame::SessionCapabilities { .. } => "session.capabilities",
                ClientFrame::SessionStateFrame { .. } => "session.state",
                ClientFrame::ItemUpsert { .. } => "item.upsert",
                ClientFrame::NoticeUpsert { .. } => "notice.upsert",
                ClientFrame::NoticeResolved { .. } => "notice.resolved",
                ClientFrame::SessionExit { .. } => "session.exit",
                ClientFrame::Error { .. } => "error",
                ClientFrame::Heartbeat { .. } => "heartbeat",
                ClientFrame::Hello { .. } => "hello",
                ClientFrame::FsResponse(_) => "fs.response",
                ClientFrame::TerminalOpened { .. } => "terminal.opened",
                ClientFrame::TerminalData { .. } => "terminal.data",
                ClientFrame::TerminalExit { .. } => "terminal.exit",
                ClientFrame::TerminalError { .. } => "terminal.error",
            })
            .collect()
    }

    async fn next_frames(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<ClientFrame>,
        count: usize,
    ) -> Vec<ClientFrame> {
        let mut frames = Vec::new();
        while frames.len() < count {
            frames.push(rx.recv().await.expect("frame"));
        }
        frames
    }

    #[tokio::test]
    async fn echo_round_trip_emits_started_capabilities_item_notice_resolved_and_exit() {
        let (out_tx, mut out_rx) = unbounded_channel();
        let (cmd_tx, cmd_rx) = unbounded_channel();
        let session = EchoSession::new(String::from("str_1"), String::from("hi"), out_tx, cmd_rx);
        let task = tokio::spawn(session.run());

        let opening = next_frames(&mut out_rx, 6).await;
        assert_eq!(
            collect(&opening),
            vec![
                "session.started",
                "session.capabilities",
                "session.state",
                "item.upsert",
                "notice.upsert",
                "session.state"
            ]
        );
        assert!(matches!(
            &opening[0],
            ClientFrame::SessionStarted { external_session_id, .. }
                if external_session_id == "echo-str_1"
        ));
        let notice_id = match &opening[4] {
            ClientFrame::NoticeUpsert { notice, .. } => notice.id.clone(),
            other => panic!("unexpected frame: {other:?}"),
        };
        assert!(notice_id.len() > 4);

        cmd_tx
            .send(SessionCmd::NoticeDecision {
                notice_id: notice_id.clone(),
                decision: Decision::Approve,
            })
            .unwrap();
        let closing = next_frames(&mut out_rx, 4).await;
        assert_eq!(
            collect(&closing),
            vec![
                "notice.resolved",
                "item.upsert",
                "session.state",
                "session.exit"
            ]
        );
        assert!(matches!(
            closing[2],
            ClientFrame::SessionStateFrame {
                state: SessionState::Idle,
                ..
            }
        ));
        assert!(matches!(
            closing[3],
            ClientFrame::SessionExit { code: 0, .. }
        ));
        let report = task.await.expect("echo task");
        assert_eq!(report.code, 0);
    }

    #[test]
    fn echo_capabilities_claim_only_what_echo_implements_with_real_catalogs() {
        let ClientFrame::SessionCapabilities {
            stream_id,
            capabilities,
            catalogs,
            commands,
        } = capabilities_frame("str_1")
        else {
            panic!("echo must answer with session.capabilities");
        };
        assert_eq!(stream_id, "str_1");
        assert!(capabilities.steer && capabilities.interrupt);
        assert!(capabilities.selections && capabilities.commands);
        let models: Vec<&str> = catalogs.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(models, vec!["echo-fast", "echo-slow"]);
        let permissions: Vec<&str> = catalogs.permissions.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(permissions, vec!["default", "read-only"]);
        let names: Vec<&str> = commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["/help", "/clear"]);
        // Every catalog entry names itself; an id-only entry trains the browser to
        // show raw ids.
        assert!(catalogs.models.iter().all(|m| !m.name.is_empty()));
        assert!(catalogs.permissions.iter().all(|p| !p.name.is_empty()));
    }

    #[tokio::test]
    async fn a_steer_becomes_a_user_item_plus_an_assistant_reply() {
        let (out_tx, mut out_rx) = unbounded_channel();
        let (cmd_tx, cmd_rx) = unbounded_channel();
        let session = EchoSession::new(String::from("str_1"), String::from("hi"), out_tx, cmd_rx);
        let _task = tokio::spawn(session.run());
        let _opening = next_frames(&mut out_rx, 6).await;

        cmd_tx
            .send(SessionCmd::Steer {
                text: String::from("also check the tests"),
            })
            .unwrap();
        let steered = next_frames(&mut out_rx, 2).await;
        assert_eq!(collect(&steered), vec!["item.upsert", "item.upsert"]);
        match (&steered[0], &steered[1]) {
            (
                ClientFrame::ItemUpsert { item: user, .. },
                ClientFrame::ItemUpsert {
                    item: assistant, ..
                },
            ) => {
                assert_eq!(user.role, Some(Role::User), "the instruction is the user's");
                assert_eq!(user.content["text"], json!("also check the tests"));
                assert_eq!(assistant.role, Some(Role::Assistant));
                assert!(assistant.content["text"]
                    .as_str()
                    .expect("text")
                    .contains("steer received"));
            }
            other => panic!("unexpected frames: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_selection_is_recorded_and_the_state_is_re_emitted_unchanged() {
        let (out_tx, mut out_rx) = unbounded_channel();
        let (cmd_tx, cmd_rx) = unbounded_channel();
        let session = EchoSession::new(String::from("str_1"), String::from("hi"), out_tx, cmd_rx);
        let _task = tokio::spawn(session.run());
        let _opening = next_frames(&mut out_rx, 6).await;

        cmd_tx
            .send(SessionCmd::Selections {
                model: Some(String::from("echo-slow")),
                permission_mode: None,
            })
            .unwrap();
        let answered = next_frames(&mut out_rx, 1).await;
        assert_eq!(collect(&answered), vec!["session.state"]);
        assert!(matches!(
            &answered[0],
            ClientFrame::SessionStateFrame {
                // Unchanged: the runtime was waiting for the approval before the
                // selection and is still waiting after it.
                state: SessionState::WaitingApproval,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn echo_interrupt_rejects_the_notice_and_reports_a_nonzero_code() {
        let (out_tx, mut out_rx) = unbounded_channel();
        let (cmd_tx, cmd_rx) = unbounded_channel();
        let session = EchoSession::new(String::from("str_1"), String::from("hi"), out_tx, cmd_rx);
        let task = tokio::spawn(session.run());
        let opening = next_frames(&mut out_rx, 6).await;
        let notice_id = match &opening[4] {
            ClientFrame::NoticeUpsert { notice, .. } => notice.id.clone(),
            other => panic!("unexpected frame: {other:?}"),
        };
        cmd_tx.send(SessionCmd::Interrupt).unwrap();
        let closing = next_frames(&mut out_rx, 3).await;
        assert!(matches!(
            &closing[0],
            ClientFrame::NoticeResolved { notice_id: id, decision: Decision::Reject } if id == &notice_id
        ));
        assert!(matches!(
            closing[2],
            ClientFrame::SessionExit { code: 130, .. }
        ));
        assert_eq!(task.await.expect("echo task").code, 130);
    }

    #[tokio::test]
    async fn echo_extra_message_does_not_close_the_session() {
        let (out_tx, mut out_rx) = unbounded_channel();
        let (cmd_tx, cmd_rx) = unbounded_channel();
        let session = EchoSession::new(String::from("str_1"), String::from("hi"), out_tx, cmd_rx);
        let _task = tokio::spawn(session.run());
        let _opening = next_frames(&mut out_rx, 6).await;
        cmd_tx
            .send(SessionCmd::Message {
                text: String::from("more"),
                client_message_id: String::from("cm_2"),
            })
            .unwrap();
        let extra = next_frames(&mut out_rx, 1).await;
        match &extra[0] {
            ClientFrame::ItemUpsert { item, .. } => {
                assert_eq!(item.content["text"], serde_json::json!("echo: more"))
            }
            other => panic!("unexpected frame: {other:?}"),
        }
    }
}

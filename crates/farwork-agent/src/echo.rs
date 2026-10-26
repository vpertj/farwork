//! Echo runtime: a deliberately fake agent used for hub smoke tests.
//!
//! It is only reachable with --runtime echo and it says so in its own output; it
//! must never be substituted for codex. Its job is to exercise the ingress
//! contract (started / item / notice / respond / exit) against a real hub.

use std::time::Duration;

use serde_json::json;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::time::timeout;

use crate::codex::session::{ExitReport, SessionCmd};
use crate::ids;
use crate::protocol::{
    ClientFrame, Decision, ItemEnvelope, ItemStatus, ItemType, NoticeEnvelope, NoticeKind, Role,
    SessionState,
};
use crate::timeutil;

pub const DECISION_TIMEOUT: Duration = Duration::from_secs(60);
pub const NOTICE_ID_TAG: &str = "echo";

pub struct EchoSession {
    stream_id: String,
    prompt: String,
    out: UnboundedSender<ClientFrame>,
    cmd: UnboundedReceiver<SessionCmd>,
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
        }
    }

    pub async fn run(mut self) -> ExitReport {
        self.send(ClientFrame::SessionStarted {
            stream_id: self.stream_id.clone(),
            external_session_id: format!("echo-{}", self.stream_id),
        });
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

    fn state(&self, state: SessionState) {
        self.send(ClientFrame::SessionStateFrame {
            stream_id: self.stream_id.clone(),
            state,
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
    async fn echo_round_trip_emits_started_item_notice_resolved_and_exit() {
        let (out_tx, mut out_rx) = unbounded_channel();
        let (cmd_tx, cmd_rx) = unbounded_channel();
        let session = EchoSession::new(String::from("str_1"), String::from("hi"), out_tx, cmd_rx);
        let task = tokio::spawn(session.run());

        let opening = next_frames(&mut out_rx, 5).await;
        assert_eq!(
            collect(&opening),
            vec![
                "session.started",
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
        let notice_id = match &opening[3] {
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

    #[tokio::test]
    async fn echo_interrupt_rejects_the_notice_and_reports_a_nonzero_code() {
        let (out_tx, mut out_rx) = unbounded_channel();
        let (cmd_tx, cmd_rx) = unbounded_channel();
        let session = EchoSession::new(String::from("str_1"), String::from("hi"), out_tx, cmd_rx);
        let task = tokio::spawn(session.run());
        let opening = next_frames(&mut out_rx, 5).await;
        let notice_id = match &opening[3] {
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
        let _opening = next_frames(&mut out_rx, 5).await;
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

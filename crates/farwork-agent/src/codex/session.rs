//! Session driver: one codex child, one hub stream.
//!
//! The driver owns no protocol knowledge; it only pumps CodexOut values from the
//! mapper to the two peers and routes hub commands back into the mapper.

use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::time::interval;

use crate::codex::mapper::{CodexMapper, CodexOut, MapperConfig, SessionSeed};
use crate::codex::transport::CodexTransport;
use crate::protocol::{ClientFrame, Decision, SessionState};

/// Hub -> session commands, routed by streamId.
#[derive(Debug, Clone)]
pub enum SessionCmd {
    Message {
        text: String,
        client_message_id: String,
    },
    Interrupt,
    NoticeDecision {
        notice_id: String,
        decision: Decision,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitReport {
    pub code: i32,
    pub reason: String,
}

pub struct CodexSession {
    mapper: CodexMapper,
    transport: Box<dyn CodexTransport>,
    out: UnboundedSender<ClientFrame>,
    cmd: UnboundedReceiver<SessionCmd>,
    tick: Duration,
}

impl CodexSession {
    pub fn new(
        transport: Box<dyn CodexTransport>,
        seed: SessionSeed,
        cfg: MapperConfig,
        out: UnboundedSender<ClientFrame>,
        cmd: UnboundedReceiver<SessionCmd>,
    ) -> Self {
        Self {
            mapper: CodexMapper::new(seed, cfg),
            transport,
            out,
            cmd,
            tick: Duration::from_secs(5),
        }
    }

    pub fn stream_id(&self) -> &str {
        self.mapper.stream_id()
    }

    pub async fn run(mut self) -> ExitReport {
        let mut outs = self.mapper.begin();
        if let Some(report) = self.dispatch(&mut outs).await {
            return report;
        }

        let mut timer = interval(self.tick);
        timer.tick().await;
        loop {
            tokio::select! {
                biased;
                inbound = self.transport.next_frame() => {
                    match inbound {
                        Ok(Some(frame)) => {
                            let mut outs = self.mapper.on_rpc(frame);
                            if let Some(report) = self.dispatch(&mut outs).await {
                                return report;
                            }
                        }
                        Ok(None) => {
                            let code = if self.mapper.state() == SessionState::Error { 1 } else { 0 };
                            return self.finish(code, String::from("codex app-server closed its stdout")).await;
                        }
                        Err(error) => {
                            let mut outs = self.mapper.on_disconnect();
                            self.dispatch(&mut outs).await;
                            return self.finish(1, format!("codex transport error: {error}")).await;
                        }
                    }
                }
                Some(cmd) = self.cmd.recv() => {
                    let mut outs = match cmd {
                        SessionCmd::Message { text, client_message_id } => self
                            .mapper
                            .on_user_message(&text, &client_message_id),
                        SessionCmd::Interrupt => self.mapper.on_interrupt(),
                        SessionCmd::NoticeDecision { notice_id, decision } => {
                            self.mapper.on_notice_decision(&notice_id, decision)
                        }
                    };
                    if let Some(report) = self.dispatch(&mut outs).await {
                        return report;
                    }
                }
                _ = timer.tick() => {
                    let mut outs = self.mapper.expire_notices(std::time::Instant::now());
                    if outs.is_empty() { continue; }
                    if let Some(report) = self.dispatch(&mut outs).await {
                        return report;
                    }
                }
                else => {
                    return self.finish(0, String::from("session inputs closed")).await;
                }
            }
        }
    }

    async fn finish(mut self, code: i32, reason: String) -> ExitReport {
        for out in self.mapper.on_disconnect() {
            if let CodexOut::Hub(frame) = out {
                self.send_hub(frame);
            }
        }
        self.send_hub(self.mapper.exit_frame(code));
        ExitReport { code, reason }
    }

    /// Returns Some(report) when a session.exit frame was emitted, meaning the
    /// stream is finished and the driver must stop.
    async fn dispatch(&mut self, outs: &mut Vec<CodexOut>) -> Option<ExitReport> {
        for out in outs.drain(..) {
            match out {
                CodexOut::Rpc(frame) => {
                    if let Err(error) = self.transport.send(frame).await {
                        tracing::warn!(target: "farwork_agent::codex", %error, "failed to send frame to codex");
                        return None;
                    }
                }
                CodexOut::Hub(ClientFrame::SessionExit { code, .. }) => {
                    self.send_hub(ClientFrame::SessionExit {
                        stream_id: String::from(self.mapper.stream_id()),
                        code,
                    });
                    return Some(ExitReport {
                        code,
                        reason: String::from("codex reported a fatal error"),
                    });
                }
                CodexOut::Hub(frame) => self.send_hub(frame),
            }
        }
        None
    }

    fn send_hub(&self, frame: ClientFrame) {
        if self.out.send(frame).is_err() {
            tracing::debug!(target: "farwork_agent::hub", "hub channel closed; dropping frame");
        }
    }
}

/// Convenience for tests and for the worker in hub.rs.
pub fn rpc_frames(frames: &[&Value]) -> Vec<Value> {
    frames.iter().map(|frame| (*frame).clone()).collect()
}

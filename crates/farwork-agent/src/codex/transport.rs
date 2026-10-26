//! codex app-server transport.
//!
//! The wire is newline-delimited JSON-RPC over the child's stdio (the default
//! transport of 'codex app-server', see 'codex app-server --help': --listen
//! stdio://). The trait exists so the whole event/approval state machine is
//! testable against an in-memory fake instead of a live provider.

use std::pin::Pin;
use std::process::Stdio;

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

pub type BoxFuture<'a, T> = Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

pub trait CodexTransport: Send {
    /// Write one JSON-RPC frame as a single line.
    fn send<'a>(&'a mut self, frame: Value) -> BoxFuture<'a, Result<()>>;
    /// Next inbound frame; None means the peer closed its side.
    fn next_frame(&mut self) -> BoxFuture<'_, Result<Option<Value>>>;
}

/// Real transport: one 'codex app-server --stdio' child process.
pub struct StdioTransport {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl StdioTransport {
    pub async fn spawn(bin: &str, args: &[&str]) -> Result<Self> {
        let mut child = Command::new(bin)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawn {bin} {}", args.join(" ")))?;
        let stdin = child.stdin.take().context("codex stdin unavailable")?;
        let stdout = child.stdout.take().context("codex stdout unavailable")?;
        if let Some(stderr) = child.stderr.take() {
            // An undrained stderr pipe fills up and blocks the child, so every
            // line is consumed and logged instead of ignored.
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "farwork_agent::codex", "{line}");
                }
            });
        }
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }
}

impl CodexTransport for StdioTransport {
    fn send<'a>(&'a mut self, frame: Value) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let line = crate::rpc::encode_line(&frame).context("encode codex frame")?;
            self.stdin
                .write_all(line.as_bytes())
                .await
                .context("write codex frame")?;
            self.stdin.flush().await.context("flush codex frame")
        })
    }

    fn next_frame(&mut self) -> BoxFuture<'_, Result<Option<Value>>> {
        Box::pin(async move {
            let mut line = String::new();
            loop {
                line.clear();
                let read = self
                    .stdout
                    .read_line(&mut line)
                    .await
                    .context("read codex frame")?;
                if read == 0 {
                    return Ok(None);
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(trimmed) {
                    Ok(value) => return Ok(Some(value)),
                    // A stray non-JSON line is dropped, exactly like an unknown
                    // frame type: it must not kill the session.
                    Err(error) => {
                        tracing::warn!(target: "farwork_agent::codex", %error, "dropping non-JSON line on stdout");
                    }
                }
            }
        })
    }
}

#[cfg(test)]
pub mod fake {
    use std::sync::{Arc, Mutex};

    use anyhow::Result;
    use serde_json::Value;
    use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

    use super::{BoxFuture, CodexTransport};

    /// Handle owned by the test: push frames in, read frames out.
    pub struct FakeHandle {
        inbound: UnboundedSender<Option<Value>>,
        sent: Arc<Mutex<Vec<Value>>>,
    }

    impl FakeHandle {
        pub fn push(&self, frame: Value) {
            let _ = self.inbound.send(Some(frame));
        }

        pub fn close(&self) {
            let _ = self.inbound.send(None);
        }

        pub fn sent(&self) -> Vec<Value> {
            self.sent.lock().expect("sent lock").clone()
        }
    }

    pub struct FakeTransport {
        inbound: UnboundedReceiver<Option<Value>>,
        sent: Arc<Mutex<Vec<Value>>>,
    }

    impl FakeTransport {
        pub fn pair() -> (Self, FakeHandle) {
            let (tx, rx) = unbounded_channel();
            let sent = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    inbound: rx,
                    sent: Arc::clone(&sent),
                },
                FakeHandle { inbound: tx, sent },
            )
        }
    }

    impl CodexTransport for FakeTransport {
        fn send<'a>(&'a mut self, frame: Value) -> BoxFuture<'a, Result<()>> {
            self.sent.lock().expect("sent lock").push(frame);
            Box::pin(async { Ok(()) })
        }

        fn next_frame(&mut self) -> BoxFuture<'_, Result<Option<Value>>> {
            Box::pin(async move {
                match self.inbound.recv().await {
                    Some(Some(frame)) => Ok(Some(frame)),
                    _ => Ok(None),
                }
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::fake::FakeTransport;
    use super::CodexTransport;

    #[tokio::test]
    async fn fake_transport_records_outbound_and_returns_inbound_in_order() {
        let (mut transport, handle) = FakeTransport::pair();
        transport
            .send(json!({"id": 1, "method": "initialize"}))
            .await
            .unwrap();
        assert_eq!(handle.sent().len(), 1);
        handle.push(json!({"id": 1, "result": {}}));
        let frame = transport.next_frame().await.unwrap().unwrap();
        assert_eq!(frame["id"], json!(1));
    }

    #[tokio::test]
    async fn closed_peer_yields_none() {
        let (mut transport, handle) = FakeTransport::pair();
        handle.close();
        assert_eq!(transport.next_frame().await.unwrap(), None);
    }
}

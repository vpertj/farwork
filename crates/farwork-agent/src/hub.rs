//! Outbound connector ingress client.
//!
//! The connector always dials out (no inbound port, no public IP). One websocket
//! carries hello/heartbeat plus every session frame; each session.start spawns a
//! worker that owns one runtime (codex child or echo) and pumps its frames into
//! the same socket.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio::task::JoinHandle;
use tokio::time::{interval, timeout};
use tokio_tungstenite::tungstenite::Message;

use crate::backoff::Backoff;
use crate::cli::{connector_ws_url, RuntimeKind};
use crate::codex::mapper::{MapperConfig, SessionSeed};
use crate::codex::{CodexSession, SessionCmd, StdioTransport};
use crate::echo::EchoSession;
use crate::identity::AgentIdentity;
use crate::protocol::{parse_server_frame, AgentInfo, ClientFrame, ParsedServerFrame, ServerFrame};
use crate::timeutil;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
pub const FALLBACK_HEARTBEAT_SECS: u64 = 15;

#[derive(Debug, Clone)]
pub struct HubOptions {
    pub hub_url: String,
    pub token: String,
    pub agent: AgentInfo,
    pub runtime: RuntimeKind,
    pub workdir: PathBuf,
    pub codex_bin: String,
    pub once: bool,
}

struct Worker {
    cmd: UnboundedSender<SessionCmd>,
    task: JoinHandle<()>,
}

pub struct Connector {
    opts: HubOptions,
    identity: AgentIdentity,
}

impl Connector {
    pub fn new(opts: HubOptions, identity: AgentIdentity) -> Self {
        Self { opts, identity }
    }

    /// Connect, serve, reconnect. Returns only on --once or a fatal setup error.
    pub async fn run(self) -> Result<()> {
        tracing::info!(
            agent_id = %self.identity.agent_id,
            name = %self.opts.agent.name,
            runtime = self.opts.runtime.as_str(),
            "farwork-agent starting"
        );
        let mut backoff = Backoff::connector_default();
        // Frames owed to the hub that could not be delivered because the socket
        // died: replayed right after the next hello so nothing is silently lost.
        let mut pending: Vec<ClientFrame> = Vec::new();
        loop {
            let mut handshaked = false;
            let outcome = self.connect_once(&mut pending, &mut handshaked).await;
            if handshaked {
                // A completed handshake resets the ladder even if the socket
                // died right after: the hub was reachable.
                backoff.reset();
            }
            if let Err(error) = &outcome {
                tracing::warn!(%error, "connector connection failed");
            }
            if self.opts.once {
                tracing::info!("--once: not reconnecting");
                return Ok(());
            }
            let delay = backoff.next_delay();
            tracing::info!(
                ?delay,
                attempt = backoff.attempt(),
                "reconnecting after backoff"
            );
            tokio::time::sleep(delay).await;
        }
    }

    /// One connection lifetime. 'handshaked' reports whether the welcome landed,
    /// which is what the reconnect ladder keys off.
    async fn connect_once(
        &self,
        pending: &mut Vec<ClientFrame>,
        handshaked: &mut bool,
    ) -> Result<()> {
        let url = connector_ws_url(&self.opts.hub_url, &self.opts.token);
        tracing::info!(hub = %redact_token(&url), "connecting to hub");
        let (stream, _) = timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(&url))
            .await
            .context("websocket connect timed out")?
            .context("websocket connect failed")?;
        let (mut sink, mut source) = stream.split();

        let (tx, mut rx) = unbounded_channel::<ClientFrame>();
        tx.send(ClientFrame::Hello {
            agent: self.opts.agent.clone(),
        })
        .map_err(|_| anyhow!("writer channel closed"))?;
        for frame in pending.drain(..) {
            if tx.send(frame).is_err() {
                break;
            }
        }

        let writer = tokio::spawn(async move {
            while let Some(frame) = rx.recv().await {
                let text = match frame.encode() {
                    Ok(text) => text,
                    Err(error) => {
                        tracing::warn!(%error, "drop unencodable frame");
                        continue;
                    }
                };
                if sink.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        });

        let heartbeat_secs = match self.await_welcome(&mut source).await {
            Ok(secs) => secs,
            Err(error) => {
                writer.abort();
                return Err(error);
            }
        };
        *handshaked = true;
        tracing::info!(heartbeat_secs, "hub welcome received; connector online");

        let mut sessions: HashMap<String, Worker> = HashMap::new();
        let mut heartbeat = interval(Duration::from_secs(heartbeat_secs));
        heartbeat.tick().await;

        let result: Result<()> = loop {
            tokio::select! {
                maybe = source.next() => {
                    match maybe {
                        None => break Err(anyhow!("hub closed the connection")),
                        Some(Err(error)) => break Err(anyhow!("websocket read error: {error}")),
                        Some(Ok(Message::Text(text))) => {
                            self.route(text.as_str(), &tx, &mut sessions);
                        }
                        Some(Ok(Message::Close(_))) => break Err(anyhow!("hub closed the connection")),
                        Some(Ok(_)) => {}
                    }
                }
                _ = heartbeat.tick() => {
                    if tx.send(ClientFrame::Heartbeat { ts: timeutil::utc_now() }).is_err() {
                        break Err(anyhow!("writer task stopped"));
                    }
                }
            }
        };

        // In-flight streams cannot continue without the hub: abort the workers so
        // no orphaned codex child keeps running, and queue an honest report for
        // the next connection.
        for (stream_id, worker) in sessions.drain() {
            worker.task.abort();
            tracing::warn!(%stream_id, "interrupting in-flight session after disconnect");
            pending.push(ClientFrame::Error {
                message: String::from("connector lost the hub connection; session interrupted"),
                stream_id: Some(stream_id.clone()),
            });
            pending.push(ClientFrame::SessionExit { stream_id, code: 1 });
        }
        writer.abort();
        result
    }

    async fn await_welcome(
        &self,
        source: &mut futures_util::stream::SplitStream<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
        >,
    ) -> Result<u64> {
        loop {
            let message = match timeout(HANDSHAKE_TIMEOUT, source.next()).await {
                Err(_) => return Err(anyhow!("timed out waiting for welcome")),
                Ok(None) => return Err(anyhow!("hub closed the socket before welcome")),
                Ok(Some(Err(error))) => {
                    return Err(anyhow!("websocket error during handshake: {error}"))
                }
                Ok(Some(Ok(message))) => message,
            };
            match message {
                Message::Text(text) => match parse_server_frame(text.as_str()) {
                    ParsedServerFrame::Frame(ServerFrame::Welcome { heartbeat_secs, .. }) => {
                        return Ok(heartbeat_secs.max(1))
                    }
                    ParsedServerFrame::Frame(other) => {
                        tracing::warn!(?other, "unexpected frame before welcome")
                    }
                    ParsedServerFrame::UnknownType(ty) => {
                        tracing::debug!(%ty, "dropping unknown frame before welcome")
                    }
                    ParsedServerFrame::Malformed { ty, error } => {
                        tracing::warn!(?ty, %error, "dropping malformed frame before welcome")
                    }
                },
                Message::Close(_) => return Err(anyhow!("hub closed the socket before welcome")),
                _ => {}
            }
        }
    }

    fn route(
        &self,
        text: &str,
        out: &UnboundedSender<ClientFrame>,
        sessions: &mut HashMap<String, Worker>,
    ) {
        match parse_server_frame(text) {
            ParsedServerFrame::Frame(frame) => self.dispatch(frame, out, sessions),
            ParsedServerFrame::UnknownType(ty) => {
                tracing::debug!(%ty, "dropping unknown hub frame");
            }
            ParsedServerFrame::Malformed { ty, error } => {
                tracing::warn!(?ty, %error, "dropping malformed hub frame");
            }
        }
    }

    fn dispatch(
        &self,
        frame: ServerFrame,
        out: &UnboundedSender<ClientFrame>,
        sessions: &mut HashMap<String, Worker>,
    ) {
        match frame {
            ServerFrame::SessionStart {
                stream_id,
                session_id,
                runtime,
                cwd,
                prompt,
            } => {
                if sessions.contains_key(&stream_id) {
                    let _ = out.send(ClientFrame::Error {
                        message: format!("stream {stream_id} is already running"),
                        stream_id: Some(stream_id),
                    });
                    return;
                }
                let requested = RuntimeKind::parse(&runtime);
                if requested != Some(self.opts.runtime) {
                    // Never pretend to be a runtime this process was not started
                    // with: echo must not answer for codex and vice versa.
                    let message = format!(
                        "connector runs runtime {} but the hub asked for {runtime:?}",
                        self.opts.runtime.as_str()
                    );
                    tracing::warn!(%stream_id, %message);
                    let _ = out.send(ClientFrame::Error {
                        message,
                        stream_id: Some(stream_id.clone()),
                    });
                    let _ = out.send(ClientFrame::SessionExit { stream_id, code: 2 });
                    return;
                }
                let (cmd_tx, cmd_rx) = unbounded_channel();
                let task = match self.opts.runtime {
                    RuntimeKind::Codex => self.spawn_codex(
                        stream_id.clone(),
                        session_id,
                        cwd,
                        prompt,
                        out.clone(),
                        cmd_rx,
                    ),
                    RuntimeKind::Echo => {
                        let session =
                            EchoSession::new(stream_id.clone(), prompt, out.clone(), cmd_rx);
                        tokio::spawn(async move {
                            let report = session.run().await;
                            tracing::info!(code = report.code, reason = %report.reason, "echo session finished");
                        })
                    }
                };
                sessions.insert(stream_id, Worker { cmd: cmd_tx, task });
            }
            ServerFrame::SessionMessage {
                stream_id,
                text,
                client_message_id,
            } => self.forward(
                sessions,
                &stream_id,
                SessionCmd::Message {
                    text,
                    client_message_id,
                },
            ),
            ServerFrame::SessionInterrupt { stream_id } => {
                self.forward(sessions, &stream_id, SessionCmd::Interrupt)
            }
            ServerFrame::NoticeRespond {
                notice_id,
                decision,
            } => {
                // notice.respond carries no streamId: find the worker holding it.
                let mut delivered = false;
                for worker in sessions.values() {
                    if worker
                        .cmd
                        .send(SessionCmd::NoticeDecision {
                            notice_id: notice_id.clone(),
                            decision,
                        })
                        .is_ok()
                    {
                        delivered = true;
                    }
                }
                if !delivered {
                    tracing::warn!(%notice_id, "notice.respond with no live session");
                }
            }
            ServerFrame::Error { message, stream_id } => {
                tracing::warn!(?stream_id, %message, "hub reported an error");
            }
            ServerFrame::Welcome { .. } => {
                tracing::debug!("duplicate welcome frame ignored");
            }
        }
    }

    fn forward(&self, sessions: &HashMap<String, Worker>, stream_id: &str, command: SessionCmd) {
        match sessions.get(stream_id) {
            Some(worker) => {
                if worker.cmd.send(command).is_err() {
                    tracing::warn!(%stream_id, "session worker is gone");
                }
            }
            None => tracing::debug!(%stream_id, "frame for unknown stream dropped"),
        }
    }

    fn spawn_codex(
        &self,
        stream_id: String,
        session_id: String,
        cwd: String,
        prompt: String,
        out: UnboundedSender<ClientFrame>,
        cmd_rx: tokio::sync::mpsc::UnboundedReceiver<SessionCmd>,
    ) -> JoinHandle<()> {
        let bin = self.opts.codex_bin.clone();
        let seed = SessionSeed {
            stream_id: stream_id.clone(),
            session_id,
            cwd,
            prompt,
        };
        tokio::spawn(async move {
            let transport = match StdioTransport::spawn(&bin, &["app-server", "--stdio"]).await {
                Ok(transport) => transport,
                Err(error) => {
                    tracing::error!(%error, "cannot start codex app-server");
                    let _ = out.send(ClientFrame::Error {
                        message: format!("cannot start codex app-server: {error}"),
                        stream_id: Some(stream_id.clone()),
                    });
                    let _ = out.send(ClientFrame::SessionExit {
                        stream_id,
                        code: 127,
                    });
                    return;
                }
            };
            tracing::info!(pid = transport.pid(), %stream_id, "codex app-server started");
            let session = CodexSession::new(
                Box::new(transport),
                seed,
                MapperConfig::default(),
                out,
                cmd_rx,
            );
            let report = session.run().await;
            tracing::info!(
                %stream_id,
                code = report.code,
                reason = %report.reason,
                "codex session finished"
            );
        })
    }
}

/// Tokens are credentials: never log the query string verbatim.
fn redact_token(url: &str) -> String {
    match url.split_once("token=") {
        Some((head, tail)) => {
            let suffix = tail.find('&').map(|index| &tail[index..]).unwrap_or("");
            format!("{head}token=<redacted>{suffix}")
        }
        None => url.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr};

    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message;

    use super::*;
    use crate::protocol::Decision;

    fn options(hub_url: String, runtime: RuntimeKind) -> HubOptions {
        HubOptions {
            hub_url,
            token: String::from("cxt_test"),
            agent: AgentInfo {
                name: String::from("test-host"),
                platform: String::from("test"),
                version: String::from("0.1.0"),
            },
            runtime,
            workdir: PathBuf::from("/tmp"),
            codex_bin: String::from("codex"),
            once: true,
        }
    }

    fn identity() -> AgentIdentity {
        AgentIdentity {
            agent_id: String::from("agt_test"),
            name: String::from("test-host"),
            created_at: String::from("2026-01-01T00:00:00.000000Z"),
        }
    }

    async fn read_json(
        source: &mut futures_util::stream::SplitStream<
            tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        >,
    ) -> serde_json::Value {
        loop {
            let message = timeout(Duration::from_secs(10), source.next())
                .await
                .expect("timed out waiting for connector frame")
                .expect("socket closed")
                .expect("websocket error");
            if let Message::Text(text) = message {
                return serde_json::from_str(text.as_str()).expect("json frame");
            }
        }
    }

    async fn write_json(
        sink: &mut futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
            Message,
        >,
        value: serde_json::Value,
    ) {
        sink.send(Message::Text(value.to_string().into()))
            .await
            .expect("send");
    }

    /// A scripted stand-in for the hub: one accepted socket, then a fixed
    /// conversation. This is the closest thing to an e2e test that does not need
    /// PostgreSQL.
    async fn fake_hub() -> (String, tokio::task::JoinHandle<Vec<serde_json::Value>>) {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let websocket = tokio_tungstenite::accept_async(stream)
                .await
                .expect("accept ws");
            let (mut sink, mut source) = websocket.split();
            let mut seen = Vec::new();

            let hello = read_json(&mut source).await;
            assert_eq!(hello["type"], serde_json::json!("hello"));
            seen.push(hello);
            write_json(
                &mut sink,
                serde_json::json!({"type": "welcome", "serverVersion": "0.1.0", "heartbeatSecs": 15}),
            )
            .await;

            write_json(
                &mut sink,
                serde_json::json!({"type": "session.start", "streamId": "str_1",
                    "sessionId": "sess_1", "runtime": "echo", "cwd": "/tmp", "prompt": "hello hub"}),
            )
            .await;

            // Collect up to the notice, then approve it.
            let mut notice_id: Option<String> = None;
            while notice_id.is_none() {
                let frame = read_json(&mut source).await;
                if frame["type"] == serde_json::json!("notice.upsert") {
                    notice_id = frame["notice"]["id"].as_str().map(str::to_owned);
                }
                seen.push(frame);
            }
            write_json(
                &mut sink,
                serde_json::json!({"type": "notice.respond", "noticeId": notice_id.unwrap(),
                    "decision": "approve"}),
            )
            .await;

            // Drain until session.exit.
            loop {
                let frame = read_json(&mut source).await;
                let done = frame["type"] == serde_json::json!("session.exit");
                seen.push(frame);
                if done {
                    break;
                }
            }
            seen
        });
        (format!("ws://127.0.0.1:{port}"), task)
    }

    #[tokio::test]
    async fn echo_session_over_a_real_websocket_follows_the_ingress_contract() {
        let (hub_url, hub_task) = fake_hub().await;
        let connector = Connector::new(options(hub_url, RuntimeKind::Echo), identity());
        let run = tokio::spawn(async move { connector.run().await });

        let seen = timeout(Duration::from_secs(20), hub_task)
            .await
            .expect("fake hub timed out")
            .expect("fake hub task");
        // Printed only with --nocapture; it is the transcript the report quotes.
        for frame in &seen {
            let line = serde_json::to_string(frame).unwrap_or_default();
            println!("TRANSCRIPT {line}");
        }
        let types: Vec<String> = seen
            .iter()
            .map(|frame| frame["type"].as_str().unwrap_or("?").to_owned())
            .collect();
        assert_eq!(types[0], "hello");
        assert_eq!(seen[0]["agent"]["name"], serde_json::json!("test-host"));
        assert!(types.contains(&String::from("session.started")));
        assert!(types.contains(&String::from("item.upsert")));
        assert!(types.contains(&String::from("notice.upsert")));
        assert!(types.contains(&String::from("notice.resolved")));
        let exit = seen
            .iter()
            .find(|frame| frame["type"] == serde_json::json!("session.exit"))
            .expect("session.exit");
        assert_eq!(exit["code"], serde_json::json!(0));
        let states: Vec<&serde_json::Value> = seen
            .iter()
            .filter(|frame| frame["type"] == serde_json::json!("session.state"))
            .map(|frame| &frame["state"])
            .collect();
        assert!(states.contains(&&serde_json::json!("waiting_approval")));
        assert!(states.contains(&&serde_json::json!("idle")));
        let decision = seen
            .iter()
            .find(|frame| frame["type"] == serde_json::json!("notice.resolved"))
            .expect("notice.resolved");
        assert_eq!(decision["decision"], serde_json::json!("approve"));
        let _ = run.await;
    }

    #[tokio::test]
    async fn in_flight_streams_are_reported_on_the_next_connection() {
        // Contract requirement: a disconnect must not silently lose a running
        // stream. The connector aborts the worker and replays an error +
        // session.exit for it right after the next hello.
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let hub = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept 1");
            let websocket = tokio_tungstenite::accept_async(stream).await.expect("ws 1");
            let (mut sink, mut source) = websocket.split();
            let _hello = read_json(&mut source).await;
            write_json(
                &mut sink,
                serde_json::json!({"type": "welcome", "serverVersion": "0.1.0", "heartbeatSecs": 15}),
            )
            .await;
            write_json(
                &mut sink,
                serde_json::json!({"type": "session.start", "streamId": "str_1",
                    "sessionId": "sess_1", "runtime": "echo", "cwd": "/tmp", "prompt": "hi"}),
            )
            .await;
            loop {
                let frame = read_json(&mut source).await;
                if frame["type"] == serde_json::json!("session.started") {
                    break;
                }
            }
            // Abrupt close: no close handshake, like a crashed hub.
            drop(sink);
            drop(source);

            let (stream, _) = listener.accept().await.expect("accept 2");
            let websocket = tokio_tungstenite::accept_async(stream).await.expect("ws 2");
            let (mut sink, mut source) = websocket.split();
            let hello = read_json(&mut source).await;
            write_json(
                &mut sink,
                serde_json::json!({"type": "welcome", "serverVersion": "0.1.0", "heartbeatSecs": 15}),
            )
            .await;
            let error = read_json(&mut source).await;
            let exit = read_json(&mut source).await;
            (hello, error, exit)
        });

        let connector = Connector::new(
            HubOptions {
                once: false,
                ..options(format!("ws://127.0.0.1:{port}"), RuntimeKind::Echo)
            },
            identity(),
        );
        let run = tokio::spawn(async move { connector.run().await });
        let (hello, error, exit) = timeout(Duration::from_secs(25), hub)
            .await
            .expect("fake hub timed out")
            .expect("hub task");
        assert_eq!(hello["type"], serde_json::json!("hello"));
        assert_eq!(error["type"], serde_json::json!("error"));
        assert_eq!(error["streamId"], serde_json::json!("str_1"));
        assert!(error["message"]
            .as_str()
            .unwrap()
            .contains("lost the hub connection"));
        assert_eq!(exit["type"], serde_json::json!("session.exit"));
        assert_eq!(exit["streamId"], serde_json::json!("str_1"));
        assert_eq!(exit["code"], serde_json::json!(1));
        run.abort();
    }

    #[tokio::test]
    async fn unknown_and_malformed_frames_do_not_kill_the_connection() {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let hub = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let websocket = tokio_tungstenite::accept_async(stream).await.expect("ws");
            let (mut sink, mut source) = websocket.split();
            let _hello = read_json(&mut source).await;
            write_json(
                &mut sink,
                serde_json::json!({"type": "welcome", "serverVersion": "0.1.0", "heartbeatSecs": 15}),
            )
            .await;
            // Garbage, an unknown type, then a real frame.
            let _ = sink
                .send(Message::Text(String::from("not json").into()))
                .await;
            write_json(&mut sink, serde_json::json!({"type": "future.frame"})).await;
            write_json(
                &mut sink,
                serde_json::json!({"type": "session.start", "streamId": "str_1",
                    "sessionId": "sess_1", "runtime": "echo", "cwd": "/tmp", "prompt": "still alive"}),
            )
            .await;
            // The connector must still answer with session.started for str_1.
            loop {
                let frame = read_json(&mut source).await;
                if frame["type"] == serde_json::json!("session.started") {
                    assert_eq!(frame["streamId"], serde_json::json!("str_1"));
                    return;
                }
            }
        });
        let connector = Connector::new(
            options(format!("ws://127.0.0.1:{port}"), RuntimeKind::Echo),
            identity(),
        );
        let run = tokio::spawn(async move { connector.run().await });
        timeout(Duration::from_secs(20), hub)
            .await
            .expect("fake hub timed out")
            .expect("hub task");
        run.abort();
    }

    #[tokio::test]
    async fn codex_runtime_request_is_refused_when_the_connector_was_started_as_echo() {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let hub = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let websocket = tokio_tungstenite::accept_async(stream).await.expect("ws");
            let (mut sink, mut source) = websocket.split();
            let _hello = read_json(&mut source).await;
            write_json(
                &mut sink,
                serde_json::json!({"type": "welcome", "serverVersion": "0.1.0", "heartbeatSecs": 15}),
            )
            .await;
            write_json(
                &mut sink,
                serde_json::json!({"type": "session.start", "streamId": "str_9",
                    "sessionId": "sess_9", "runtime": "codex", "cwd": "/tmp", "prompt": "hi"}),
            )
            .await;
            loop {
                let frame = read_json(&mut source).await;
                if frame["type"] == serde_json::json!("session.exit") {
                    assert_eq!(frame["streamId"], serde_json::json!("str_9"));
                    assert_eq!(frame["code"], serde_json::json!(2));
                    return;
                }
            }
        });
        let connector = Connector::new(
            options(format!("ws://127.0.0.1:{port}"), RuntimeKind::Echo),
            identity(),
        );
        let run = tokio::spawn(async move { connector.run().await });
        timeout(Duration::from_secs(20), hub)
            .await
            .expect("fake hub timed out")
            .expect("hub task");
        run.abort();
    }

    #[test]
    fn token_is_redacted_in_logs() {
        assert_eq!(
            redact_token("ws://h:1/api/v2/connector/ws?token=cxt_secret&x=1"),
            "ws://h:1/api/v2/connector/ws?token=<redacted>&x=1"
        );
        assert_eq!(
            redact_token("ws://h:1/api/v2/connector/ws?token=cxt_secret"),
            "ws://h:1/api/v2/connector/ws?token=<redacted>"
        );
        assert_eq!(redact_token("ws://h:1"), "ws://h:1");
    }

    #[test]
    fn notice_respond_without_a_stream_is_parsed_not_dropped() {
        let parsed = parse_server_frame(
            r#"{"type":"notice.respond","noticeId":"ntc_1","decision":"approve_for_session"}"#,
        );
        match parsed {
            ParsedServerFrame::Frame(ServerFrame::NoticeRespond {
                notice_id,
                decision,
            }) => {
                assert_eq!(notice_id, "ntc_1");
                assert_eq!(decision, Decision::ApproveForSession);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}

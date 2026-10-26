//! The DSH bridge transport: loopback TCP, one JSON document per line.
//!
//! The wire is JSON-RPC 2.0 with three rules this module exists to enforce:
//!
//! * a frame is one UTF-8 line, and 8 MiB is the hard ceiling — a peer that keeps
//!   writing without a newline must be cut off, not buffered forever;
//! * writes are serialized, because two concurrent requests that interleave their
//!   bytes produce one unparsable line and lose both answers;
//! * a request the bridge sends *us* is answered with `-32601`. This agent serves
//!   no bridge-facing methods, and silence would leave the peer waiting forever.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::time::timeout;

/// The contract's frame ceiling. Everything above it is a broken peer.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
/// The contract's per-request deadline; a call that passes it is cancelled.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(60);
pub const CANCEL_METHOD: &str = "$/cancelRequest";
pub const METHOD_NOT_FOUND: i64 = -32601;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    TooLarge { bytes: usize },
    NotUtf8,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge { bytes } => write!(
                f,
                "frame of {bytes} bytes exceeds the {MAX_FRAME_BYTES}-byte limit"
            ),
            Self::NotUtf8 => f.write_str("frame is not valid UTF-8"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Newline framing. Feed it raw socket bytes; it hands back whole lines.
#[derive(Debug, Default)]
pub struct Decoder {
    buffer: Vec<u8>,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn buffered(&self) -> usize {
        self.buffer.len()
    }

    /// Append a chunk and split out every complete frame. A buffer that grows past
    /// the ceiling without a newline is an error: waiting for a terminator that a
    /// broken peer never sends would grow until the process dies.
    pub fn feed(&mut self, chunk: &[u8]) -> Result<Vec<String>, FrameError> {
        self.buffer.extend_from_slice(chunk);
        let mut frames = Vec::new();
        while let Some(index) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let mut line: Vec<u8> = self.buffer.drain(..=index).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.is_empty() {
                continue;
            }
            if line.len() > MAX_FRAME_BYTES {
                return Err(FrameError::TooLarge { bytes: line.len() });
            }
            match String::from_utf8(line) {
                Ok(text) => frames.push(text),
                Err(_) => return Err(FrameError::NotUtf8),
            }
        }
        if self.buffer.len() > MAX_FRAME_BYTES {
            return Err(FrameError::TooLarge {
                bytes: self.buffer.len(),
            });
        }
        Ok(frames)
    }
}

/// Request ids are the contract's `aa-<n>` strings, monotonic from 1 so a
/// transcript can be read in send order without a timestamp.
#[derive(Debug, Default)]
pub struct IdGen {
    next: u64,
}

impl IdGen {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn next_id(&mut self) -> String {
        self.next += 1;
        Self::format(self.next)
    }

    pub fn format(seq: u64) -> String {
        format!("aa-{seq}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RpcError {
    #[error("bridge error {code}: {message}")]
    Bridge {
        code: i64,
        /// The bridge's own machine-readable code (`error.data.code`), which is
        /// what callers branch on: the JSON-RPC code is a category, not a cause.
        data_code: Option<String>,
        retryable: bool,
        message: String,
    },
    #[error("{method} timed out (request {id})")]
    Timeout { method: String, id: String },
    #[error("transport: {message}")]
    Transport { message: String },
}

impl RpcError {
    pub fn is_method_not_found(&self) -> bool {
        matches!(self, Self::Bridge { code, .. } if *code == METHOD_NOT_FOUND)
    }

    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Timeout { .. } | Self::Transport { .. } => true,
            Self::Bridge { retryable, .. } => *retryable,
        }
    }

    pub fn data_code(&self) -> Option<&str> {
        match self {
            Self::Bridge { data_code, .. } => data_code.as_deref(),
            _ => None,
        }
    }

    /// A rejected request is the bridge's considered answer: the session must not
    /// be reported as failed, but the caller has to see why nothing happened.
    pub fn message(&self) -> String {
        match self {
            Self::Bridge { message, .. } => message.clone(),
            other => other.to_string(),
        }
    }
}

fn parse_bridge_error(value: &Value) -> RpcError {
    let code = value.get("code").and_then(Value::as_i64).unwrap_or(-32603);
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("bridge error")
        .to_string();
    let data = value.get("data");
    RpcError::Bridge {
        code,
        data_code: data
            .and_then(|data| data.get("code"))
            .and_then(Value::as_str)
            .map(str::to_string),
        retryable: data
            .and_then(|data| data.get("retryable"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        message,
    }
}

/// One notification pushed by the bridge (no `id`, so no answer expected).
#[derive(Debug, Clone, PartialEq)]
pub struct Inbound {
    pub method: String,
    pub params: Value,
}

type Pending = HashMap<String, oneshot::Sender<Result<Value, RpcError>>>;

pub struct RpcClient {
    writer: Mutex<OwnedWriteHalf>,
    pending: Mutex<Pending>,
    ids: Mutex<IdGen>,
}

pub struct Connection {
    pub client: std::sync::Arc<RpcClient>,
    pub notifications: mpsc::UnboundedReceiver<Inbound>,
}

pub async fn dial(address: &str) -> std::io::Result<Connection> {
    let stream = TcpStream::connect(address).await?;
    // Request/response traffic is tiny and latency-sensitive: Nagle would add a
    // round trip of delay to every handshake frame.
    let _ = stream.set_nodelay(true);
    let (reader, writer) = stream.into_split();
    let client = std::sync::Arc::new(RpcClient {
        writer: Mutex::new(writer),
        pending: Mutex::new(HashMap::new()),
        ids: Mutex::new(IdGen::new()),
    });
    let (notes_tx, notes_rx) = mpsc::unbounded_channel();
    let reader_client = std::sync::Arc::clone(&client);
    tokio::spawn(async move {
        read_loop(reader, reader_client, notes_tx).await;
    });
    Ok(Connection {
        client,
        notifications: notes_rx,
    })
}

async fn read_loop(
    mut reader: OwnedReadHalf,
    client: std::sync::Arc<RpcClient>,
    notes: mpsc::UnboundedSender<Inbound>,
) {
    let mut decoder = Decoder::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let read = match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) => {
                tracing::warn!(%error, "dsh rpc read failed");
                break;
            }
        };
        let frames = match decoder.feed(&chunk[..read]) {
            Ok(frames) => frames,
            Err(error) => {
                tracing::error!(%error, "dsh rpc frame rejected; closing the bridge connection");
                break;
            }
        };
        for frame in frames {
            client.handle_frame(&frame, &notes).await;
        }
    }
    // Nothing can answer a pending call any more, so wake every waiter instead of
    // letting them burn their full deadline.
    let mut pending = client.pending.lock().await;
    for (_, sender) in pending.drain() {
        let _ = sender.send(Err(RpcError::Transport {
            message: String::from("the bridge connection closed"),
        }));
    }
}

impl RpcClient {
    async fn handle_frame(&self, text: &str, notes: &mpsc::UnboundedSender<Inbound>) {
        let value: Value = match serde_json::from_str(text) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(%error, "dropping an unparsable dsh frame");
                return;
            }
        };
        let id = value.get("id").cloned().filter(|id| !id.is_null());
        let method = value
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned);
        match (method, id) {
            // A request from the bridge: answer -32601 and keep the connection.
            (Some(method), Some(id)) => {
                let reply = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": METHOD_NOT_FOUND,
                        "message": format!("method not found: {method}"),
                        "data": { "code": "METHOD_NOT_FOUND", "retryable": false },
                    },
                });
                if let Err(error) = self.write_line(&reply).await {
                    tracing::warn!(%error, "cannot answer an inbound dsh request");
                }
            }
            (Some(method), None) => {
                let inbound = Inbound {
                    method,
                    params: value.get("params").cloned().unwrap_or(Value::Null),
                };
                if notes.send(inbound).is_err() {
                    tracing::debug!("dsh notification channel closed; dropping notification");
                }
            }
            (None, Some(id)) => {
                let key = match id.as_str() {
                    Some(text) => text.to_string(),
                    None => id.to_string(),
                };
                let sender = self.pending.lock().await.remove(&key);
                let Some(sender) = sender else {
                    tracing::debug!(%key, "response for an unknown dsh request id");
                    return;
                };
                let outcome = match value.get("error") {
                    Some(error) => Err(parse_bridge_error(error)),
                    None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = sender.send(outcome);
            }
            (None, None) => tracing::warn!("dropping a dsh frame with neither id nor method"),
        }
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        self.call_within(method, params, CALL_TIMEOUT).await
    }

    pub async fn call_within(
        &self,
        method: &str,
        params: Value,
        deadline: Duration,
    ) -> Result<Value, RpcError> {
        let id = self.ids.lock().await.next_id();
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().await.insert(id.clone(), sender);
        let frame = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if let Err(error) = self.write_line(&frame).await {
            self.pending.lock().await.remove(&id);
            return Err(error);
        }
        match timeout(deadline, receiver).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => Err(RpcError::Transport {
                message: format!("{method}: the response channel closed"),
            }),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                // Cancellation is a notification, not a second request: a bridge
                // that already answered simply drops it.
                let _ = self.notify(CANCEL_METHOD, json!({ "id": id })).await;
                Err(RpcError::Timeout {
                    method: method.to_string(),
                    id,
                })
            }
        }
    }

    /// Close the socket.
    ///
    /// The reader task holds its own clone of this client, so dropping the last
    /// caller handle would *not* close the write half — every finished session
    /// would leave a socket open for the life of the daemon.
    pub async fn close(&self) {
        let mut writer = self.writer.lock().await;
        let _ = writer.shutdown().await;
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<(), RpcError> {
        self.write_line(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await
    }

    async fn write_line(&self, value: &Value) -> Result<(), RpcError> {
        let mut text = serde_json::to_string(value).map_err(|error| RpcError::Transport {
            message: error.to_string(),
        })?;
        text.push('\n');
        // One frame at a time: interleaved writes are unparsable on the wire.
        let mut writer = self.writer.lock().await;
        writer
            .write_all(text.as_bytes())
            .await
            .map_err(|error| RpcError::Transport {
                message: error.to_string(),
            })?;
        writer.flush().await.map_err(|error| RpcError::Transport {
            message: error.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use tokio::net::TcpListener;

    #[test]
    fn frames_are_split_across_chunks_and_crlf_is_tolerated() {
        let mut decoder = Decoder::new();
        assert!(decoder.feed(b"{\"a\":1}").unwrap().is_empty());
        assert_eq!(decoder.buffered(), 7);
        let frames = decoder.feed(b"\n{\"b\":2}\r\n").unwrap();
        assert_eq!(
            frames,
            vec![String::from("{\"a\":1}"), String::from("{\"b\":2}")]
        );
        assert_eq!(decoder.buffered(), 0);
        // A chunk carrying several frames yields all of them in order.
        let frames = decoder.feed(b"1\n2\n3\n").unwrap();
        assert_eq!(frames, vec!["1", "2", "3"]);
        // Blank lines are not frames.
        assert!(decoder.feed(b"\n\n").unwrap().is_empty());
    }

    #[test]
    fn a_frame_past_the_limit_is_refused_with_and_without_a_terminator() {
        let mut decoder = Decoder::new();
        let oversized = vec![b'a'; MAX_FRAME_BYTES + 1];
        assert_eq!(
            decoder.feed(&oversized).unwrap_err(),
            FrameError::TooLarge {
                bytes: MAX_FRAME_BYTES + 1
            }
        );

        let mut decoder = Decoder::new();
        let mut terminated = vec![b'a'; MAX_FRAME_BYTES + 1];
        terminated.push(b'\n');
        assert!(matches!(
            decoder.feed(&terminated).unwrap_err(),
            FrameError::TooLarge { .. }
        ));

        // Exactly at the limit is still a frame: the ceiling is inclusive.
        let mut decoder = Decoder::new();
        let mut at_limit = vec![b'a'; MAX_FRAME_BYTES];
        at_limit.push(b'\n');
        let frames = decoder.feed(&at_limit).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].len(), MAX_FRAME_BYTES);
    }

    #[test]
    fn a_non_utf8_frame_is_refused_instead_of_lossily_decoded() {
        let mut decoder = Decoder::new();
        assert_eq!(
            decoder.feed(&[0xff, 0xfe, b'\n']).unwrap_err(),
            FrameError::NotUtf8
        );
    }

    #[test]
    fn request_ids_are_the_contract_aa_prefix_in_send_order() {
        let mut ids = IdGen::new();
        assert_eq!(ids.next_id(), "aa-1");
        assert_eq!(ids.next_id(), "aa-2");
        assert_eq!(IdGen::format(41), "aa-41");
    }

    /// A scripted bridge: one accepted socket, then whatever the test writes.
    async fn bridge<T, F, Fut>(script: F) -> (Connection, tokio::task::JoinHandle<T>)
    where
        F: FnOnce(tokio::net::TcpStream) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = T> + Send,
        T: Send + 'static,
    {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            script(stream).await
        });
        let connection = dial(&format!("127.0.0.1:{port}")).await.expect("dial");
        (connection, task)
    }

    async fn read_frame(reader: &mut OwnedReadHalf) -> Value {
        let mut decoder = Decoder::new();
        let mut chunk = vec![0u8; 8192];
        loop {
            let read = reader.read(&mut chunk).await.expect("read");
            assert!(read > 0, "socket closed before a frame arrived");
            let frames = decoder.feed(&chunk[..read]).expect("frame");
            if let Some(frame) = frames.into_iter().next() {
                return serde_json::from_str(&frame).expect("json");
            }
        }
    }

    async fn write_frame(writer: &mut OwnedWriteHalf, value: Value) {
        writer
            .write_all(format!("{value}\n").as_bytes())
            .await
            .expect("write");
    }

    #[tokio::test]
    async fn an_inbound_request_is_answered_with_method_not_found() {
        let (connection, task) = bridge(|stream| async move {
            let (reader, mut writer) = stream.into_split();
            let mut reader = reader;
            write_frame(
                &mut writer,
                json!({"jsonrpc":"2.0","id":"peer-1","method":"session.list","params":{}}),
            )
            .await;
            read_frame(&mut reader).await
        })
        .await;
        let reply = timeout(Duration::from_secs(5), task)
            .await
            .expect("answer")
            .expect("task");
        assert_eq!(reply["id"], json!("peer-1"));
        assert_eq!(reply["error"]["code"], json!(-32601));
        assert_eq!(reply["error"]["data"]["code"], json!("METHOD_NOT_FOUND"));
        assert_eq!(reply["error"]["data"]["retryable"], json!(false));
        assert!(reply.get("result").is_none());
        drop(connection);
    }

    #[tokio::test]
    async fn calls_round_trip_and_notifications_are_delivered_in_order() {
        let (mut connection, task) = bridge(|stream| async move {
            let (mut reader, mut writer) = stream.into_split();
            let request = read_frame(&mut reader).await;
            assert_eq!(request["id"], json!("aa-1"));
            assert_eq!(request["method"], json!("session.list"));
            write_frame(
                &mut writer,
                json!({"jsonrpc":"2.0","method":"notice.upsert","params":{"noticeId":"n1"}}),
            )
            .await;
            let _ = &mut reader;
            write_frame(
                &mut writer,
                json!({"jsonrpc":"2.0","id":"aa-1","result":{"sessions":[]}}),
            )
            .await;
        })
        .await;
        let result = connection
            .client
            .call("session.list", json!({"limit": 5}))
            .await
            .expect("call");
        assert_eq!(result, json!({"sessions": []}));
        let note = connection.notifications.recv().await.expect("notification");
        assert_eq!(note.method, "notice.upsert");
        assert_eq!(note.params["noticeId"], json!("n1"));
        drop(connection);
        let _ = task.await;
    }

    #[tokio::test]
    async fn a_bridge_error_maps_its_data_code_and_retryability() {
        let (connection, task) = bridge(|stream| async move {
            let (mut reader, mut writer) = stream.into_split();
            let _request = read_frame(&mut reader).await;
            write_frame(
                &mut writer,
                json!({"jsonrpc":"2.0","id":"aa-1","error":{"code":-32010,
                    "message":"session is archived",
                    "data":{"code":"session_archived","retryable":false}}}),
            )
            .await;
        })
        .await;
        let error = connection
            .client
            .call("session.startTurn", json!({}))
            .await
            .unwrap_err();
        assert_eq!(error.data_code(), Some("session_archived"));
        assert!(!error.is_retryable());
        assert!(error.message().contains("archived"));
        drop(connection);
        let _ = task.await;
    }

    #[tokio::test]
    async fn a_timed_out_call_sends_cancel_request_and_frees_its_slot() {
        let (connection, task) = bridge(|stream| async move {
            let (mut reader, mut writer) = stream.into_split();
            let request = read_frame(&mut reader).await;
            // Never answer; wait for the cancellation notification instead.
            let cancel = read_frame(&mut reader).await;
            write_frame(
                &mut writer,
                json!({"jsonrpc":"2.0","id":request["id"],"result":{"late":true}}),
            )
            .await;
            cancel
        })
        .await;
        let error = connection
            .client
            .call_within("session.getState", json!({}), Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(matches!(error, RpcError::Timeout { .. }));
        let cancel = timeout(Duration::from_secs(5), task)
            .await
            .expect("cancel")
            .expect("task");
        assert_eq!(cancel["method"], json!("$/cancelRequest"));
        assert_eq!(cancel["id"], Value::Null);
        assert_eq!(cancel["params"]["id"], json!("aa-1"));
        drop(connection);
    }

    #[tokio::test]
    async fn a_closed_socket_fails_pending_calls_instead_of_hanging() {
        let (connection, task) = bridge(|stream| async move {
            let (mut reader, _writer) = stream.into_split();
            let _request = read_frame(&mut reader).await;
            // Drop everything: the peer is gone.
        })
        .await;
        let error = timeout(
            Duration::from_secs(5),
            connection.client.call("session.list", json!({})),
        )
        .await
        .expect("must not hang")
        .unwrap_err();
        assert!(matches!(error, RpcError::Transport { .. }));
        assert!(error.is_retryable());
        let _ = task.await;
    }
}

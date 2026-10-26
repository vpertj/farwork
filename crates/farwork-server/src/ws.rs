use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use farwork_protocol::{
    ClientToServer, ConnectorToServer, DeviceInfo, ServerToClient, ServerToConnector,
    PROTOCOL_VERSION,
};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::state::{AppState, Route};

/// Exit code pushed to browsers when the device connection dies mid-stream, so
/// an abnormal end is never mistaken for a clean shell exit.
const CONNECTOR_LOST_CODE: i32 = -1;

#[derive(Debug, serde::Deserialize)]
pub struct TokenQuery {
    token: Option<String>,
}

type Sink = SplitSink<WebSocket, Message>;
type Stream = SplitStream<WebSocket>;

/// `/ws/connector`: one long-lived connection per connected device.
pub async fn connector(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<TokenQuery>,
) -> Response {
    if !state.authorize(query.token.as_deref()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    ws.on_upgrade(move |socket| handle_connector(socket, state))
}

/// `/ws/client`: browser connections watching devices and driving terminals.
pub async fn client(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<TokenQuery>,
) -> Response {
    if !state.authorize(query.token.as_deref()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    ws.on_upgrade(move |socket| handle_client(socket, state))
}

async fn handle_connector(socket: WebSocket, state: AppState) {
    let (sink, mut stream) = socket.split();
    let (outgoing, receiver) = mpsc::unbounded_channel();
    let writer = spawn_writer(sink, receiver);

    let Some(device) = await_hello(&mut stream).await else {
        writer.abort();
        return;
    };

    tracing::info!(
        device_id = %device.id,
        device_name = %device.name,
        platform = ?device.platform,
        "connector connected"
    );
    state.devices.connect(device.clone(), outgoing.clone());
    if outgoing
        .send(ServerToConnector::Welcome {
            server_version: PROTOCOL_VERSION.to_string(),
        })
        .is_err()
    {
        writer.abort();
        return;
    }

    while let Some(Ok(frame)) = stream.next().await {
        let Some(text) = text_of(frame) else {
            continue;
        };
        match ConnectorToServer::parse(&text) {
            Ok(message) => {
                state.devices.touch(&device.id);
                handle_connector_frame(message, &state, &outgoing);
            }
            Err(error) => tracing::warn!(%error, "dropping malformed connector frame"),
        }
    }

    state.devices.disconnect(&device.id);
    for (stream_id, route) in state.streams.drain_device(&device.id) {
        let _ = route.client.send(ServerToClient::TerminalExit {
            stream_id,
            code: CONNECTOR_LOST_CODE,
        });
    }
    tracing::info!(device_id = %device.id, "connector disconnected");
    drop(outgoing);
    writer.abort();
}

fn handle_connector_frame(
    message: ConnectorToServer,
    state: &AppState,
    outgoing: &mpsc::UnboundedSender<ServerToConnector>,
) {
    match message {
        ConnectorToServer::Hello { device } => {
            tracing::warn!(device_id = %device.id, "ignoring duplicate hello");
        }
        ConnectorToServer::TerminalOpened { stream_id } => {
            forward_to_client(state, &stream_id, |stream_id| {
                ServerToClient::TerminalOpened { stream_id }
            });
        }
        ConnectorToServer::TerminalData { stream_id, data } => {
            forward_to_client(state, &stream_id, |stream_id| {
                ServerToClient::TerminalData { stream_id, data }
            });
        }
        ConnectorToServer::TerminalExit { stream_id, code } => {
            if let Some(route) = state.streams.remove(&stream_id) {
                let _ = route
                    .client
                    .send(ServerToClient::TerminalExit { stream_id, code });
            }
        }
        ConnectorToServer::Ping => {
            let _ = outgoing.send(ServerToConnector::Pong);
        }
        ConnectorToServer::Pong => {}
    }
}

fn forward_to_client(
    state: &AppState,
    stream_id: &str,
    build: impl FnOnce(String) -> ServerToClient,
) {
    match state.streams.get(stream_id) {
        Some(route) => {
            let _ = route.client.send(build(stream_id.to_string()));
        }
        None => tracing::warn!(%stream_id, "no client route for connector frame"),
    }
}

async fn handle_client(socket: WebSocket, state: AppState) {
    let (sink, mut stream) = socket.split();
    let (outgoing, receiver) = mpsc::unbounded_channel();
    let writer = spawn_writer(sink, receiver);
    let mut owned_streams: Vec<String> = Vec::new();
    let mut watcher: Option<JoinHandle<()>> = None;

    while let Some(Ok(frame)) = stream.next().await {
        let Some(text) = text_of(frame) else {
            continue;
        };
        match ClientToServer::parse(&text) {
            Ok(message) => {
                handle_client_frame(message, &state, &outgoing, &mut owned_streams, &mut watcher);
            }
            Err(error) => tracing::warn!(%error, "dropping malformed client frame"),
        }
    }

    for stream_id in owned_streams {
        if let Some(route) = state.streams.remove(&stream_id) {
            if let Some(connector) = state.devices.sink(&route.device_id) {
                let _ = connector.send(ServerToConnector::TerminalClose { stream_id });
            }
        }
    }
    if let Some(task) = watcher {
        task.abort();
    }
    drop(outgoing);
    writer.abort();
}

fn handle_client_frame(
    message: ClientToServer,
    state: &AppState,
    outgoing: &mpsc::UnboundedSender<ServerToClient>,
    owned_streams: &mut Vec<String>,
    watcher: &mut Option<JoinHandle<()>>,
) {
    match message {
        ClientToServer::DevicesWatch => {
            let _ = outgoing.send(ServerToClient::Devices {
                devices: state.devices.snapshot(),
            });
            if watcher.is_none() {
                let mut changes = state.devices.subscribe();
                let outgoing = outgoing.clone();
                *watcher = Some(tokio::spawn(async move {
                    while let Ok(devices) = changes.recv().await {
                        if outgoing.send(ServerToClient::Devices { devices }).is_err() {
                            break;
                        }
                    }
                }));
            }
        }
        ClientToServer::TerminalOpen {
            device_id,
            stream_id,
            cols,
            rows,
        } => {
            let Some(connector) = state.devices.sink(&device_id) else {
                let _ = outgoing.send(ServerToClient::TerminalError {
                    stream_id,
                    message: format!("device {device_id} is not connected"),
                });
                return;
            };
            state.streams.add(
                stream_id.clone(),
                Route {
                    device_id,
                    client: outgoing.clone(),
                },
            );
            owned_streams.push(stream_id.clone());
            let frame = ServerToConnector::TerminalOpen {
                stream_id: stream_id.clone(),
                cols,
                rows,
            };
            if connector.send(frame).is_err() {
                state.streams.remove(&stream_id);
                owned_streams.retain(|owned| owned != &stream_id);
                let _ = outgoing.send(ServerToClient::TerminalError {
                    stream_id,
                    message: "device connection dropped".to_string(),
                });
            }
        }
        ClientToServer::TerminalInput { stream_id, data } => {
            send_to_connector(state, &stream_id, |stream_id| {
                ServerToConnector::TerminalData { stream_id, data }
            });
        }
        ClientToServer::TerminalResize {
            stream_id,
            cols,
            rows,
        } => {
            send_to_connector(state, &stream_id, |stream_id| {
                ServerToConnector::TerminalResize {
                    stream_id,
                    cols,
                    rows,
                }
            });
        }
        ClientToServer::TerminalClose { stream_id } => {
            if let Some(route) = state.streams.remove(&stream_id) {
                if let Some(connector) = state.devices.sink(&route.device_id) {
                    let _ = connector.send(ServerToConnector::TerminalClose {
                        stream_id: stream_id.clone(),
                    });
                }
            }
            owned_streams.retain(|owned| owned != &stream_id);
            let _ = outgoing.send(ServerToClient::TerminalExit { stream_id, code: 0 });
        }
    }
}

fn send_to_connector(
    state: &AppState,
    stream_id: &str,
    build: impl FnOnce(String) -> ServerToConnector,
) {
    let Some(route) = state.streams.get(stream_id) else {
        tracing::warn!(%stream_id, "frame for unknown stream");
        return;
    };
    match state.devices.sink(&route.device_id) {
        Some(connector) => {
            let _ = connector.send(build(stream_id.to_string()));
        }
        None => tracing::warn!(%stream_id, device_id = %route.device_id, "device is not connected"),
    }
}

async fn await_hello(stream: &mut Stream) -> Option<DeviceInfo> {
    while let Some(frame) = stream.next().await {
        let Ok(frame) = frame else {
            return None;
        };
        let Some(text) = text_of(frame) else {
            continue;
        };
        match ConnectorToServer::parse(&text) {
            Ok(ConnectorToServer::Hello { device }) => return Some(device),
            Ok(other) => tracing::warn!(?other, "expected hello before any other frame"),
            Err(error) => tracing::warn!(%error, "malformed hello frame"),
        }
    }
    None
}

fn text_of(frame: Message) -> Option<String> {
    match frame {
        Message::Text(text) => Some(text.as_str().to_string()),
        Message::Binary(bytes) => String::from_utf8(bytes.to_vec()).ok(),
        Message::Close(_) => None,
        Message::Ping(_) | Message::Pong(_) => None,
    }
}

fn spawn_writer<T>(mut sink: Sink, mut receiver: mpsc::UnboundedReceiver<T>) -> JoinHandle<()>
where
    T: Serialize + Send + 'static,
{
    tokio::spawn(async move {
        while let Some(message) = receiver.recv().await {
            let payload = serde_json::to_string(&message).unwrap_or_default();
            if sink.send(Message::Text(payload.into())).await.is_err() {
                break;
            }
        }
    })
}

use std::net::SocketAddr;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use farwork_protocol::{
    ClientToServer, ConnectorToServer, DeviceInfo, DevicesResponse, HealthResponse, Platform,
    ServerToClient, ServerToConnector,
};
use farwork_server::{serve, AppState};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use uuid::Uuid;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

/// Start a real server on an ephemeral port.
async fn start_server(token: Option<&str>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("listener address");
    let state = AppState::new(token.map(ToString::to_string));
    tokio::spawn(async move {
        if let Err(error) = serve(listener, state).await {
            eprintln!("server stopped: {error}");
        }
    });
    addr
}

/// Minimal HTTP/1.1 client so the test suite needs no extra HTTP crate.
/// Returns `(status, header block, body)`.
async fn http_request(addr: SocketAddr, request: &str) -> (u16, String, String) {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("send request");

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .await
        .expect("read response");
    let status = response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .expect("status code");
    let (head, body) = response
        .split_once("\r\n\r\n")
        .unwrap_or((response.as_str(), ""));
    (status, head.to_string(), body.to_string())
}

async fn http_get(addr: SocketAddr, path: &str, token: Option<&str>) -> (u16, String) {
    let authorization = token
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\n{authorization}Connection: close\r\n\r\n");
    let (status, _head, body) = http_request(addr, &request).await;
    (status, body)
}

fn fake_device(name: &str) -> DeviceInfo {
    DeviceInfo {
        id: Uuid::new_v4().to_string(),
        name: name.to_string(),
        platform: Platform::Macos,
    }
}

async fn next_text(ws: &mut Ws) -> String {
    loop {
        let frame = tokio::time::timeout(FRAME_TIMEOUT, ws.next())
            .await
            .expect("frame before timeout")
            .expect("socket still open")
            .expect("valid websocket frame");
        match frame {
            Message::Text(text) => return text.as_str().to_string(),
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("unexpected frame: {other:?}"),
        }
    }
}

async fn send<T: serde::Serialize>(ws: &mut Ws, message: &T) {
    let payload = serde_json::to_string(message).expect("encode frame");
    ws.send(Message::Text(payload.into()))
        .await
        .expect("send frame");
}

async fn connect_connector(addr: SocketAddr, device: &DeviceInfo) -> Ws {
    let (mut ws, _) = connect_async(format!("ws://{addr}/ws/connector"))
        .await
        .expect("connector handshake");
    send(
        &mut ws,
        &ConnectorToServer::Hello {
            device: device.clone(),
        },
    )
    .await;

    let welcome = next_text(&mut ws).await;
    match ServerToConnector::parse(&welcome).expect("welcome parses") {
        ServerToConnector::Welcome { server_version } => assert_eq!(server_version, "0.1.0"),
        other => panic!("expected welcome, got {other:?}"),
    }
    ws
}

async fn connect_client(addr: SocketAddr) -> Ws {
    let (ws, _) = connect_async(format!("ws://{addr}/ws/client"))
        .await
        .expect("client handshake");
    ws
}

fn server_devices(frame: &str) -> Vec<farwork_protocol::DeviceView> {
    match ServerToClient::parse(frame).expect("server frame parses") {
        ServerToClient::Devices { devices } => devices,
        other => panic!("expected devices push, got {other:?}"),
    }
}

#[tokio::test]
async fn connector_hello_registers_device_as_connected() {
    let addr = start_server(None).await;
    let device = fake_device("mac-mini");
    let _connector = connect_connector(addr, &device).await;

    let (status, body) = http_get(addr, "/api/devices", None).await;
    assert_eq!(status, 200, "body: {body}");
    let devices: DevicesResponse = serde_json::from_str(&body).expect("devices json");
    let listed = devices
        .devices
        .iter()
        .find(|listed| listed.id == device.id)
        .expect("device appears in /api/devices");
    assert_eq!(listed.name, "mac-mini");
    assert_eq!(listed.platform, Platform::Macos);
    assert!(listed.connected, "connector is online");
    assert!(listed.last_seen_ms > 0);
}

#[tokio::test]
async fn health_reports_contract_version() {
    let addr = start_server(None).await;
    let (status, body) = http_get(addr, "/api/health", None).await;
    assert_eq!(status, 200);
    assert_eq!(body, r#"{"status":"ok","version":"0.1.0"}"#);
    let health: HealthResponse = serde_json::from_str(&body).expect("health json");
    assert_eq!(health.status, "ok");
    assert_eq!(health.version, farwork_protocol::PROTOCOL_VERSION);
}

#[tokio::test]
async fn cors_preflight_allows_browser_origin_and_authorization_header() {
    let addr = start_server(None).await;
    let request = format!(
        "OPTIONS /api/devices HTTP/1.1\r\nHost: {addr}\r\nOrigin: http://localhost:5173\r\nAccess-Control-Request-Method: GET\r\nAccess-Control-Request-Headers: authorization\r\nConnection: close\r\n\r\n"
    );
    let (status, head, _body) = http_request(addr, &request).await;
    assert_eq!(status, 200, "preflight must succeed, headers: {head}");
    let head = head.to_ascii_lowercase();
    assert!(
        head.contains("access-control-allow-origin"),
        "missing allow-origin, headers: {head}"
    );
    assert!(
        head.contains("authorization"),
        "authorization header must be allowed, headers: {head}"
    );

    let request = format!(
        "GET /api/health HTTP/1.1\r\nHost: {addr}\r\nOrigin: http://localhost:5173\r\nConnection: close\r\n\r\n"
    );
    let (status, head, _body) = http_request(addr, &request).await;
    assert_eq!(status, 200);
    assert!(
        head.to_ascii_lowercase()
            .contains("access-control-allow-origin"),
        "cross-origin GET must carry allow-origin, headers: {head}"
    );
}

#[tokio::test]
async fn heartbeat_ping_is_answered_and_malformed_frames_are_ignored() {
    let addr = start_server(None).await;
    let device = fake_device("pinger");
    let mut connector = connect_connector(addr, &device).await;

    connector
        .send(Message::Text("not json".to_string().into()))
        .await
        .expect("send garbage");
    connector
        .send(Message::Text("{\"type\":\"nope\"}".to_string().into()))
        .await
        .expect("send unknown type");

    send(&mut connector, &ConnectorToServer::Ping).await;
    assert_eq!(
        ServerToConnector::parse(&next_text(&mut connector).await).expect("connector frame"),
        ServerToConnector::Pong
    );

    // Garbage did not drop the connection: the next ping still gets an answer.
    send(&mut connector, &ConnectorToServer::Ping).await;
    assert_eq!(
        ServerToConnector::parse(&next_text(&mut connector).await).expect("connector frame"),
        ServerToConnector::Pong
    );
}

#[tokio::test]
async fn terminal_frames_round_trip_between_client_and_connector() {
    let addr = start_server(None).await;
    let device = fake_device("linux-box");
    let mut connector = connect_connector(addr, &device).await;

    let mut client = connect_client(addr).await;
    send(&mut client, &ClientToServer::DevicesWatch).await;
    let devices = server_devices(&next_text(&mut client).await);
    assert!(devices
        .iter()
        .any(|listed| listed.id == device.id && listed.connected));

    let stream_id = Uuid::new_v4().to_string();
    send(
        &mut client,
        &ClientToServer::TerminalOpen {
            device_id: device.id.clone(),
            stream_id: stream_id.clone(),
            cols: 80,
            rows: 24,
        },
    )
    .await;

    match ServerToConnector::parse(&next_text(&mut connector).await).expect("connector frame") {
        ServerToConnector::TerminalOpen {
            stream_id: received,
            cols,
            rows,
        } => {
            assert_eq!(received, stream_id);
            assert_eq!((cols, rows), (80, 24));
        }
        other => panic!("expected terminal.open, got {other:?}"),
    }

    send(
        &mut connector,
        &ConnectorToServer::TerminalOpened {
            stream_id: stream_id.clone(),
        },
    )
    .await;
    assert_eq!(
        ServerToClient::parse(&next_text(&mut client).await).expect("client frame"),
        ServerToClient::TerminalOpened {
            stream_id: stream_id.clone()
        }
    );

    let keystrokes = STANDARD.encode(b"echo hi\n");
    send(
        &mut client,
        &ClientToServer::TerminalInput {
            stream_id: stream_id.clone(),
            data: keystrokes.clone(),
        },
    )
    .await;
    assert_eq!(
        ServerToConnector::parse(&next_text(&mut connector).await).expect("connector frame"),
        ServerToConnector::TerminalData {
            stream_id: stream_id.clone(),
            data: keystrokes,
        }
    );

    let output = STANDARD.encode(b"hi\r\n");
    send(
        &mut connector,
        &ConnectorToServer::TerminalData {
            stream_id: stream_id.clone(),
            data: output.clone(),
        },
    )
    .await;
    assert_eq!(
        ServerToClient::parse(&next_text(&mut client).await).expect("client frame"),
        ServerToClient::TerminalData {
            stream_id: stream_id.clone(),
            data: output,
        }
    );

    send(
        &mut client,
        &ClientToServer::TerminalResize {
            stream_id: stream_id.clone(),
            cols: 100,
            rows: 30,
        },
    )
    .await;
    assert_eq!(
        ServerToConnector::parse(&next_text(&mut connector).await).expect("connector frame"),
        ServerToConnector::TerminalResize {
            stream_id: stream_id.clone(),
            cols: 100,
            rows: 30,
        }
    );

    send(
        &mut client,
        &ClientToServer::TerminalClose {
            stream_id: stream_id.clone(),
        },
    )
    .await;
    assert_eq!(
        ServerToClient::parse(&next_text(&mut client).await).expect("client frame"),
        ServerToClient::TerminalExit {
            stream_id: stream_id.clone(),
            code: 0,
        }
    );
    assert_eq!(
        ServerToConnector::parse(&next_text(&mut connector).await).expect("connector frame"),
        ServerToConnector::TerminalClose { stream_id }
    );
}

#[tokio::test]
async fn connector_exit_is_forwarded_to_the_client() {
    let addr = start_server(None).await;
    let device = fake_device("windows-box");
    let mut connector = connect_connector(addr, &device).await;
    let mut client = connect_client(addr).await;

    let stream_id = Uuid::new_v4().to_string();
    send(
        &mut client,
        &ClientToServer::TerminalOpen {
            device_id: device.id.clone(),
            stream_id: stream_id.clone(),
            cols: 80,
            rows: 24,
        },
    )
    .await;
    let _open = next_text(&mut connector).await;

    send(
        &mut connector,
        &ConnectorToServer::TerminalExit {
            stream_id: stream_id.clone(),
            code: 3,
        },
    )
    .await;
    assert_eq!(
        ServerToClient::parse(&next_text(&mut client).await).expect("client frame"),
        ServerToClient::TerminalExit {
            stream_id: stream_id.clone(),
            code: 3,
        }
    );

    // The stream is gone: further input must not resurrect it.
    send(
        &mut client,
        &ClientToServer::TerminalInput {
            stream_id,
            data: STANDARD.encode(b"x"),
        },
    )
    .await;
}

#[tokio::test]
async fn terminal_open_for_unknown_device_reports_error() {
    let addr = start_server(None).await;
    let mut client = connect_client(addr).await;
    let stream_id = Uuid::new_v4().to_string();

    send(
        &mut client,
        &ClientToServer::TerminalOpen {
            device_id: Uuid::new_v4().to_string(),
            stream_id: stream_id.clone(),
            cols: 80,
            rows: 24,
        },
    )
    .await;

    match ServerToClient::parse(&next_text(&mut client).await).expect("client frame") {
        ServerToClient::TerminalError {
            stream_id: received,
            message,
        } => {
            assert_eq!(received, stream_id);
            assert!(message.contains("not connected"), "message: {message}");
        }
        other => panic!("expected terminal.error, got {other:?}"),
    }
}

#[tokio::test]
async fn connector_disconnect_pushes_device_offline() {
    let addr = start_server(None).await;
    let mut client = connect_client(addr).await;
    send(&mut client, &ClientToServer::DevicesWatch).await;
    let initial = server_devices(&next_text(&mut client).await);
    assert!(initial.is_empty());

    let device = fake_device("going-away");
    let mut connector = connect_connector(addr, &device).await;
    let online = server_devices(&next_text(&mut client).await);
    assert!(online
        .iter()
        .any(|listed| listed.id == device.id && listed.connected));

    connector.close(None).await.expect("close connector");
    drop(connector);

    let offline = server_devices(&next_text(&mut client).await);
    let listed = offline
        .iter()
        .find(|listed| listed.id == device.id)
        .expect("device stays listed after disconnect");
    assert!(!listed.connected);
}

#[tokio::test]
async fn token_guards_rest_and_websockets() {
    let addr = start_server(Some("secret")).await;

    assert_eq!(http_get(addr, "/api/devices", None).await.0, 401);
    assert_eq!(http_get(addr, "/api/devices", Some("wrong")).await.0, 401);
    assert_eq!(http_get(addr, "/api/health", None).await.0, 401);

    let (status, body) = http_get(addr, "/api/devices", Some("secret")).await;
    assert_eq!(status, 200);
    assert!(body.contains("\"devices\""), "body: {body}");

    assert!(
        connect_async(format!("ws://{addr}/ws/connector"))
            .await
            .is_err(),
        "connector without token must be rejected"
    );
    assert!(
        connect_async(format!("ws://{addr}/ws/client"))
            .await
            .is_err(),
        "client without token must be rejected"
    );
    assert!(connect_async(format!("ws://{addr}/ws/client?token=secret"))
        .await
        .is_ok());
}

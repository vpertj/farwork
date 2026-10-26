//! File browsing relay end to end, against a real Postgres database.
//!
//! A fake connector (a tokio-tungstenite client) plays the device: it receives
//! `fs.request` frames and answers with `fs.response`, while the REST client
//! plays the browser. The test checks what the browser actually gets, including
//! the timeouts and the error mapping.
//!
//! One test on purpose: every integration test in this crate truncates the same
//! tables, so separate test functions would race each other.
//!
//! Runs only when TEST_DATABASE_URL points at a disposable database.

use std::time::{Duration, Instant};

use axum::body::to_bytes;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use farwork_hub::config::Config;
use farwork_hub::{db, AppState};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tower::ServiceExt as _;

const SETUP_TOKEN: &str = "setup-fs-token";
const SECRET: &str = "fs-integration-secret";
const PASSWORD: &str = "correct horse battery staple";

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn call(
    app: &Router,
    method: Method,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let request = match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .expect("request"),
        None => builder.body(axum::body::Body::empty()).expect("request"),
    };
    let response = app.clone().oneshot(request).await.expect("infallible");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("body");
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).to_string()))
    };
    (status, value)
}

struct Harness {
    app: Router,
    token: String,
    connector_id: String,
    connector_token: String,
}

async fn bootstrap(database_url: &str) -> Harness {
    let config = Config {
        database_url: database_url.to_string(),
        port: 0,
        secret: SECRET.to_string(),
        setup_token: Some(SETUP_TOKEN.to_string()),
        database_max_connections: 5,
        public_url: None,
        static_dir: None,
    };
    let pool = db::connect(&config).await.expect("connect");
    db::migrate(&pool).await.expect("migrate");
    for table in [
        "timeline_items",
        "session_streams",
        "session_notices",
        "session_runtime_states",
        "sessions",
        "projects",
        "connectors",
        "ws_tickets",
        "app_settings",
        "users",
    ] {
        sqlx::query(&format!("delete from {table}"))
            .execute(&pool)
            .await
            .expect("truncate");
    }

    let state = AppState::new(config, pool);
    let app = farwork_hub::app(state);

    let email = format!("fs+{}@example.com", farwork_hub::timeutil::unix_now());
    let (status, salt_body) = call(
        &app,
        Method::POST,
        "/api/v2/auth/password-salt",
        None,
        Some(json!({ "email": email })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "password-salt: {salt_body}");
    let salt = salt_body["salt"].as_str().expect("salt").to_string();
    let verifier = farwork_hub::auth::derive_password_verifier(PASSWORD, &salt);

    let (status, registered) = call(
        &app,
        Method::POST,
        "/api/v2/auth/register",
        None,
        Some(json!({
            "email": email,
            "displayName": "Ada",
            "passwordVerifier": verifier,
            "passwordSalt": salt,
            "setupToken": SETUP_TOKEN,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "register: {registered}");
    let token = registered["accessToken"]
        .as_str()
        .expect("token")
        .to_string();

    let (status, connector) = call(
        &app,
        Method::POST,
        "/api/v2/connectors",
        Some(&token),
        Some(json!({ "name": "Ada laptop", "connectorKind": "cli" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "connector: {connector}");
    let connector_id = connector["connector"]["id"]
        .as_str()
        .expect("connector id")
        .to_string();
    let connector_token = connector["connectorToken"]
        .as_str()
        .expect("connector token")
        .to_string();

    Harness {
        app,
        token,
        connector_id,
        connector_token,
    }
}

/// A JSON-frame WebSocket client with bounded waits, so a missing frame fails the
/// test instead of hanging the suite.
struct Client {
    socket: Socket,
    label: String,
}

impl Client {
    async fn connect(url: &str, label: &str) -> Self {
        let (socket, _) = timeout(
            Duration::from_secs(5),
            tokio_tungstenite::connect_async(url),
        )
        .await
        .expect("connect timeout")
        .expect("connect");
        Self {
            socket,
            label: String::from(label),
        }
    }

    async fn send(&mut self, frame: Value) {
        eprintln!("<<< {} {}", self.label, frame);
        self.socket
            .send(Message::Text(frame.to_string().into()))
            .await
            .expect("send");
    }

    async fn next_frame(&mut self) -> Value {
        let frame = match timeout(Duration::from_secs(30), self.socket.next()).await {
            Err(_) => panic!("{}: frame timeout", self.label),
            Ok(None) => panic!("{}: socket closed", self.label),
            Ok(Some(Err(error))) => panic!("{}: socket error {error}", self.label),
            Ok(Some(Ok(frame))) => frame,
        };
        match frame {
            Message::Text(text) => {
                let value: Value = serde_json::from_str(&text).expect("json frame");
                eprintln!(">>> {} {}", self.label, value);
                value
            }
            Message::Close(_) => panic!("{}: closed while waiting for a frame", self.label),
            _ => panic!("{}: unexpected non-text frame", self.label),
        }
    }

    async fn expect_type(&mut self, expected: &str) -> Value {
        for _ in 0..64 {
            let frame = self.next_frame().await;
            if frame["type"] == expected {
                return frame;
            }
        }
        panic!("{}: never saw a {expected} frame", self.label);
    }
}

fn entry(name: &str, kind: &str, size: u64) -> Value {
    json!({
        "name": name,
        "path": format!("/srv/farwork/{name}"),
        "kind": kind,
        "sizeBytes": size,
        "modifiedAt": "2026-01-02T03:04:05Z",
    })
}

/// Scripted device: matches on the requested operation and path so every
/// scenario is visible in one place.
fn reply_for(request: &Value) -> Option<Value> {
    let request_id = request["requestId"].clone();
    let op = request["op"].as_str().unwrap_or_default();
    let path = request["path"].as_str().unwrap_or_default();
    let failure = |code: &str, message: &str| {
        Some(json!({
            "type": "fs.response",
            "requestId": request_id,
            "ok": false,
            "error": { "code": code, "message": message },
        }))
    };
    match (op, path) {
        ("list", "/srv/farwork") => Some(json!({
            "type": "fs.response",
            "requestId": request_id,
            "ok": true,
            "op": "list",
            "path": "/srv/farwork",
            "entries": [entry("src", "dir", 96), entry("README.md", "file", 42)],
        })),
        ("readText", "/srv/farwork/README.md") => Some(json!({
            "type": "fs.response",
            "requestId": request_id,
            "ok": true,
            "op": "readText",
            "path": "/srv/farwork/README.md",
            "content": "# farwork\n",
            "sizeBytes": 10,
            "truncated": false,
        })),
        ("readText", "/srv/farwork/denied.txt") => failure(
            "permission_denied",
            "/srv/farwork/denied.txt is outside the agent workdir",
        ),
        ("readText", "/srv/farwork/missing.txt") => {
            failure("not_found", "/srv/farwork/missing.txt does not exist")
        }
        ("readText", "/srv/farwork/huge.log") => {
            failure("too_large", "/srv/farwork/huge.log is 5242880 bytes")
        }
        ("readText", "/srv/farwork/broken.bin") => {
            failure("io_error", "/srv/farwork/broken.bin is not valid UTF-8")
        }
        // Answered like a file read so the caller sees a 200: the point of this
        // path is the unmatched frame the device emits before its real reply.
        ("list", "/srv/bogus-response") => Some(json!({
            "type": "fs.response",
            "requestId": request_id,
            "ok": true,
            "op": "list",
            "path": "/srv/bogus-response",
            "entries": [],
        })),
        // Every other path is answered with a body that echoes the path, so the
        // concurrency scenario can tell the two replies apart.
        (_, other) if other.starts_with("/srv/concurrent/") => Some(json!({
            "type": "fs.response",
            "requestId": request_id,
            "ok": true,
            "op": "readText",
            "path": other,
            "content": format!("content of {other}"),
            "sizeBytes": 10,
            "truncated": false,
        })),
        _ => None,
    }
}

#[tokio::test]
async fn fs_relay_end_to_end() {
    let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!(
            "SKIP farwork-hub fs relay test: TEST_DATABASE_URL is not set.
             Point it at a disposable database to run this test, e.g.
               export TEST_DATABASE_URL=postgres://tianjun@127.0.0.1:5432/farwork_test"
        );
        return;
    };
    if database_url.trim().is_empty() {
        eprintln!("SKIP farwork-hub fs relay test: TEST_DATABASE_URL is empty.");
        return;
    }
    let harness = bootstrap(&database_url).await;
    let app = harness.app.clone();
    let token = harness.token.clone();
    let list_url = format!("/api/v2/connectors/{}/fs/list", harness.connector_id);
    let read_url = format!("/api/v2/connectors/{}/fs/readText", harness.connector_id);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let server_app = app.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, server_app).await;
    });

    // --- validation, before any connector is around ----------------------
    let (status, unauthorized) = call(
        &app,
        Method::POST,
        &list_url,
        None,
        Some(json!({ "path": "/srv/farwork" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{unauthorized}");

    let (status, relative) = call(
        &app,
        Method::POST,
        &list_url,
        Some(&token),
        Some(json!({ "path": "srv/farwork" })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a relative path must be refused, not resolved against the hub's cwd: {relative}"
    );
    assert_eq!(relative["detail"]["code"], "invalid_path", "{relative}");

    let (status, empty_path) = call(
        &app,
        Method::POST,
        &list_url,
        Some(&token),
        Some(json!({ "path": "" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{empty_path}");

    let (status, unknown) = call(
        &app,
        Method::POST,
        "/api/v2/connectors/conn_does_not_exist/fs/list",
        Some(&token),
        Some(json!({ "path": "/srv/farwork" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{unknown}");
    assert_eq!(unknown["detail"]["code"], "connector_not_found");

    // The connector exists but has never dialled in.
    let (status, offline) = call(
        &app,
        Method::POST,
        &list_url,
        Some(&token),
        Some(json!({ "path": "/srv/farwork" })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "an offline connector must not answer 200: {offline}"
    );
    assert_eq!(offline["detail"]["code"], "connector_offline", "{offline}");

    // --- the device connects --------------------------------------------
    let mut connector = Client::connect(
        &format!(
            "ws://{addr}/api/v2/connector/ws?token={}",
            harness.connector_token
        ),
        "connector",
    )
    .await;
    connector
        .send(json!({
            "type": "hello",
            "agent": { "name": "fs-connector", "platform": "macos", "version": "0.1.0" },
        }))
        .await;
    connector.expect_type("welcome").await;

    // Two concurrent requests are answered in reverse order: the pairing must
    // hold by requestId, not by arrival order.
    let device = tokio::spawn(async move {
        let mut held: Vec<Value> = Vec::new();
        loop {
            let request = match timeout(Duration::from_secs(45), connector.next_frame()).await {
                Ok(frame) => frame,
                Err(_) => break,
            };
            if request["type"] != "fs.request" {
                continue;
            }
            let path = request["path"].as_str().unwrap_or_default().to_string();
            if path == "/srv/bogus-response" {
                // A response for a requestId the hub never minted. It must be
                // dropped with a warning, not routed and not fatal.
                connector
                    .send(json!({
                        "type": "fs.response",
                        "requestId": "req_not_minted",
                        "ok": true,
                        "op": "list",
                        "path": "/srv/bogus-response",
                        "entries": [],
                    }))
                    .await;
            }
            let Some(reply) = reply_for(&request) else {
                // No scripted answer: stay silent, which is what the timeout
                // scenario needs.
                eprintln!("device: ignoring request for {path}");
                continue;
            };
            if path.starts_with("/srv/concurrent/") {
                held.push(reply);
                if held.len() == 2 {
                    let second = held.pop().expect("second");
                    let first = held.pop().expect("first");
                    connector.send(second).await;
                    connector.send(first).await;
                }
                continue;
            }
            connector.send(reply).await;
        }
    });

    // --- list ------------------------------------------------------------
    let (status, listed) = call(
        &app,
        Method::POST,
        &list_url,
        Some(&token),
        Some(json!({ "path": "/srv/farwork" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "fs/list: {listed}");
    assert_eq!(listed["path"], "/srv/farwork");
    let entries = listed["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["name"], "src");
    assert_eq!(entries[0]["kind"], "dir");
    assert_eq!(entries[1]["name"], "README.md");
    assert_eq!(entries[1]["sizeBytes"], 42);
    assert_eq!(entries[1]["modifiedAt"], "2026-01-02T03:04:05Z");
    assert!(listed["serverTime"].as_str().is_some(), "{listed}");

    // --- readText ---------------------------------------------------------
    let (status, read) = call(
        &app,
        Method::POST,
        &read_url,
        Some(&token),
        Some(json!({ "path": "/srv/farwork/README.md" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "fs/readText: {read}");
    assert_eq!(read["path"], "/srv/farwork/README.md");
    assert_eq!(read["content"], "# farwork\n");
    assert_eq!(read["sizeBytes"], 10);
    assert_eq!(read["truncated"], false);
    assert!(read["serverTime"].as_str().is_some(), "{read}");

    // --- the connector's failures, mapped onto HTTP -----------------------
    for (path, expected, code) in [
        (
            "/srv/farwork/denied.txt",
            StatusCode::FORBIDDEN,
            "permission_denied",
        ),
        (
            "/srv/farwork/missing.txt",
            StatusCode::NOT_FOUND,
            "not_found",
        ),
        (
            "/srv/farwork/huge.log",
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
        ),
        (
            "/srv/farwork/broken.bin",
            StatusCode::BAD_GATEWAY,
            "io_error",
        ),
    ] {
        let (status, body) = call(
            &app,
            Method::POST,
            &read_url,
            Some(&token),
            Some(json!({ "path": path })),
        )
        .await;
        assert_eq!(status, expected, "{path}: {body}");
        assert_eq!(body["detail"]["code"], code, "{path}: {body}");
        assert!(
            body["detail"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains(path),
            "the connector's own message must survive: {body}"
        );
    }

    // --- concurrency ------------------------------------------------------
    let (left, right) = tokio::join!(
        call(
            &app,
            Method::POST,
            &read_url,
            Some(&token),
            Some(json!({ "path": "/srv/concurrent/a.txt" })),
        ),
        call(
            &app,
            Method::POST,
            &read_url,
            Some(&token),
            Some(json!({ "path": "/srv/concurrent/b.txt" })),
        ),
    );
    assert_eq!(left.0, StatusCode::OK, "left: {}", left.1);
    assert_eq!(right.0, StatusCode::OK, "right: {}", right.1);
    assert_eq!(left.1["path"], "/srv/concurrent/a.txt", "{}", left.1);
    assert_eq!(left.1["content"], "content of /srv/concurrent/a.txt");
    assert_eq!(right.1["path"], "/srv/concurrent/b.txt", "{}", right.1);
    assert_eq!(right.1["content"], "content of /srv/concurrent/b.txt");

    // --- an unmatched fs.response must not break the connection ----------
    // The device answers this path with a frame for a requestId the hub never
    // minted, then with the real reply: the bogus one is dropped, the socket
    // stays up, and the caller still gets its 200.
    let (status, bogus) = call(
        &app,
        Method::POST,
        &list_url,
        Some(&token),
        Some(json!({ "path": "/srv/bogus-response" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected: {bogus}");
    assert_eq!(bogus["path"], "/srv/bogus-response", "{bogus}");

    let (status, after) = call(
        &app,
        Method::POST,
        &list_url,
        Some(&token),
        Some(json!({ "path": "/srv/farwork" })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "socket died after a bogus frame: {after}"
    );

    // --- silence becomes 504, and the pending entry is released ----------
    device.abort();
    let mut connector = Client::connect(
        &format!(
            "ws://{addr}/api/v2/connector/ws?token={}",
            harness.connector_token
        ),
        "silent-connector",
    )
    .await;
    connector
        .send(json!({
            "type": "hello",
            "agent": { "name": "silent", "platform": "macos", "version": "0.1.0" },
        }))
        .await;
    connector.expect_type("welcome").await;
    // It reads the request and answers nothing at all.
    let silent = tokio::spawn(async move {
        let request = timeout(Duration::from_secs(30), connector.next_frame())
            .await
            .expect("request");
        assert_eq!(request["type"], "fs.request");
        eprintln!("silent connector received {}", request);
        // Hold the socket open and keep listening, so the timeout — not a
        // disconnect — is what the hub sees.
        loop {
            match timeout(Duration::from_secs(30), connector.next_frame()).await {
                Ok(frame) => {
                    eprintln!("silent connector saw {}", frame);
                    break;
                }
                Err(_) => continue,
            }
        }
    });

    let started = Instant::now();
    let (status, timed_out) = call(
        &app,
        Method::POST,
        &list_url,
        Some(&token),
        Some(json!({ "path": "/srv/farwork" })),
    )
    .await;
    let elapsed = started.elapsed();
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "a silent connector must be a 504: {timed_out}"
    );
    assert_eq!(timed_out["detail"]["code"], "fs_timeout", "{timed_out}");
    assert!(
        elapsed >= Duration::from_secs(10),
        "the hub must wait the full 10s before giving up, waited {elapsed:?}"
    );

    silent.abort();
}

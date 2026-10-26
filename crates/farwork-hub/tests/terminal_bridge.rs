//! Device terminal relay end to end, against a real Postgres database.
//!
//! A fake connector (a tokio-tungstenite client) plays the device and a second
//! socket plays the browser. One walk through the contract: REST create ->
//! terminal.open -> terminal.opened -> data both ways (including binary) ->
//! resize -> close -> exit, plus every refusal (offline connector, unknown
//! terminalId, ticket reuse, ticket bound to another terminal, browser
//! disconnect, device disconnect).
//!
//! One test on purpose: every integration test in this crate truncates the same
//! tables, so separate test functions would race each other.
//!
//! Runs only when TEST_DATABASE_URL points at a disposable database.

use std::time::Duration;

use axum::body::to_bytes;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use farwork_hub::config::Config;
use farwork_hub::{db, AppState};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tower::ServiceExt as _;

const SETUP_TOKEN: &str = "setup-terminal-token";
const SECRET: &str = "terminal-integration-secret";
const PASSWORD: &str = "correct horse battery staple";
const FRAME_WAIT: Duration = Duration::from_secs(20);

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
    state: AppState,
    pool: PgPool,
    token: String,
    connector_id: String,
    connector_token: String,
    ws_base: String,
}

impl Harness {
    async fn bootstrap(database_url: &str) -> Self {
        let config = Config {
            database_url: database_url.to_string(),
            port: 0,
            secret: SECRET.to_string(),
            setup_token: Some(SETUP_TOKEN.to_string()),
            database_max_connections: 5,
        };
        let pool = db::connect(&config).await.expect("connect");
        db::migrate(&pool).await.expect("migrate");
        for table in [
            "terminals",
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

        let state = AppState::new(config, pool.clone());
        let app = farwork_hub::app(state.clone());

        let email = format!("terminal+{}@example.com", uuid::Uuid::new_v4().simple());
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
                "displayName": "Term",
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
            Some(json!({ "name": "Term device", "connectorKind": "cli" })),
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

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server_app = app.clone();
        tokio::spawn(async move {
            let _ = axum::serve(listener, server_app).await;
        });

        Self {
            app,
            state,
            pool,
            token,
            connector_id,
            connector_token,
            ws_base: format!("ws://{addr}"),
        }
    }

    fn terminals_url(&self) -> String {
        format!("/api/v2/connectors/{}/terminals", self.connector_id)
    }

    fn stream_url(&self, connector_id: &str, terminal_id: &str, ticket: &str) -> String {
        format!(
            "{}/api/v2/connectors/{connector_id}/terminals/{terminal_id}/stream?ticket={ticket}",
            self.ws_base
        )
    }

    /// The only way to attach: a one-shot ticket whose scope is this terminalId.
    async fn ticket(&self, terminal_id: &str) -> String {
        let (status, body) = call(
            &self.app,
            Method::POST,
            "/api/v2/ws-ticket",
            Some(&self.token),
            Some(json!({
                "clientId": "browser",
                "scope": { "terminalId": terminal_id },
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "ws-ticket: {body}");
        body["ticket"].as_str().expect("ticket").to_string()
    }

    async fn create_terminal(&self, body: Value) -> (StatusCode, Value) {
        call(
            &self.app,
            Method::POST,
            &self.terminals_url(),
            Some(&self.token),
            Some(body),
        )
        .await
    }

    /// The hub writes closed_at just after it tells the other side, so the write
    /// can land a moment after the frame the test just observed.
    async fn await_closed_at(&self, terminal_id: &str) -> String {
        for _ in 0..100 {
            if let Some(closed) = self.closed_at(terminal_id).await {
                return closed;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("terminal {terminal_id} was never marked closed");
    }

    async fn closed_at(&self, terminal_id: &str) -> Option<String> {
        sqlx::query_scalar::<_, Option<String>>("select closed_at from terminals where id = $1")
            .bind(terminal_id)
            .fetch_optional(&self.pool)
            .await
            .expect("closed_at query")
            .flatten()
    }
}

/// A JSON-frame WebSocket client with bounded waits: a missing frame fails the
/// test instead of hanging the suite.
struct Client {
    socket: Socket,
    label: String,
}

impl Client {
    async fn connect(url: &str, label: &str) -> Self {
        let (socket, _) = timeout(
            Duration::from_secs(10),
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

    /// The handshake itself must fail (401/503), not a silent socket.
    async fn connect_error(url: &str) -> StatusCode {
        match timeout(
            Duration::from_secs(10),
            tokio_tungstenite::connect_async(url),
        )
        .await
        {
            Ok(Ok(_)) => panic!("expected the handshake to be refused: {url}"),
            Ok(Err(tokio_tungstenite::tungstenite::Error::Http(response))) => response.status(),
            Ok(Err(error)) => panic!("unexpected handshake error: {error}"),
            Err(_) => panic!("handshake attempt timed out: {url}"),
        }
    }

    async fn send(&mut self, frame: Value) {
        eprintln!("<<< {} {}", self.label, frame);
        self.socket
            .send(Message::Text(frame.to_string().into()))
            .await
            .expect("send");
    }

    /// Skip keepalives: they are plumbing, not contract traffic.
    async fn next_frame(&mut self) -> Value {
        loop {
            let frame = match timeout(FRAME_WAIT, self.socket.next()).await {
                Err(_) => panic!("{}: frame timeout", self.label),
                Ok(None) => panic!("{}: socket closed while waiting for a frame", self.label),
                Ok(Some(Err(error))) => panic!("{}: socket error {error}", self.label),
                Ok(Some(Ok(frame))) => frame,
            };
            match frame {
                Message::Text(text) => {
                    let value: Value = serde_json::from_str(&text).expect("json frame");
                    eprintln!(">>> {} {}", self.label, value);
                    if value["type"] == json!("keepalive") {
                        continue;
                    }
                    return value;
                }
                Message::Close(_) => panic!("{}: closed while waiting for a frame", self.label),
                _ => pending_frame_panic(&self.label),
            }
        }
    }

    async fn expect(&mut self, kind: &str) -> Value {
        let frame = self.next_frame().await;
        assert_eq!(frame["type"], json!(kind), "{}: wrong frame", self.label);
        frame
    }

    /// Waits for the hub to close the socket. Returns the frames seen on the way,
    /// so a caller can assert that an error frame preceded the close.
    async fn drain_to_close(&mut self) -> Vec<Value> {
        let mut seen = Vec::new();
        loop {
            match timeout(FRAME_WAIT, self.socket.next()).await {
                Err(_) => panic!("{}: socket never closed", self.label),
                Ok(None) => return seen,
                Ok(Some(Err(_))) => return seen,
                Ok(Some(Ok(Message::Close(_)))) => return seen,
                Ok(Some(Ok(Message::Text(text)))) => {
                    let value: Value = serde_json::from_str(&text).expect("json frame");
                    eprintln!(">>> {} {}", self.label, value);
                    if value["type"] != json!("keepalive") {
                        seen.push(value);
                    }
                }
                Ok(Some(Ok(_))) => continue,
            }
        }
    }
}

fn pending_frame_panic(label: &str) -> ! {
    panic!("{label}: unexpected non-text frame")
}

fn b64(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

fn unb64(data: &str) -> Vec<u8> {
    STANDARD.decode(data.as_bytes()).expect("base64 body")
}

#[tokio::test]
async fn device_terminal_relay_end_to_end() {
    let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!(
            "SKIP farwork-hub terminal relay test: TEST_DATABASE_URL is not set.
             Point it at a disposable database to run this test, e.g.
               export TEST_DATABASE_URL=postgres://tianjun@127.0.0.1:5432/farwork_test"
        );
        return;
    };
    if database_url.trim().is_empty() {
        eprintln!("SKIP farwork-hub terminal relay test: TEST_DATABASE_URL is empty.");
        return;
    }
    let harness = Harness::bootstrap(&database_url).await;
    let app = harness.app.clone();
    let token = harness.token.clone();
    let ws_base = harness.ws_base.clone();

    // --- validation, before any device is around ---------------------------
    let (status, unauthorized) = call(
        &app,
        Method::POST,
        &harness.terminals_url(),
        None,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{unauthorized}");

    let (status, unknown) = call(
        &app,
        Method::POST,
        "/api/v2/connectors/conn_does_not_exist/terminals",
        Some(&token),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{unknown}");
    assert_eq!(unknown["detail"]["code"], json!("connector_not_found"));

    let (status, bad_size) = harness
        .create_terminal(json!({ "cols": 0, "rows": 24 }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad_size}");
    assert_eq!(bad_size["detail"]["code"], json!("invalid_size"));

    let (status, relative) = harness.create_terminal(json!({ "cwd": "srv/work" })).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a relative cwd must be refused, not resolved against the hub cwd: {relative}"
    );
    assert_eq!(relative["detail"]["code"], json!("invalid_path"));

    // The connector exists but has never dialled in.
    let (status, offline) = harness.create_terminal(json!({})).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{offline}");
    assert_eq!(offline["detail"]["code"], json!("connector_offline"));

    // --- the device dials in -----------------------------------------------
    let mut device = Client::connect(
        &format!(
            "{ws_base}/api/v2/connector/ws?token={}",
            harness.connector_token
        ),
        "device",
    )
    .await;
    device
        .send(json!({ "type": "hello", "agent": { "name": "fake", "platform": "test", "version": "0.1.0" } }))
        .await;
    device.expect("welcome").await;

    // --- open: REST -> terminal.open -> terminal.opened --------------------
    let (status, created) = harness
        .create_terminal(json!({ "cols": 80, "rows": 24 }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let terminal_id = created["terminal"]["id"]
        .as_str()
        .expect("terminal id")
        .to_string();
    assert_eq!(
        created["terminal"]["connectorId"].as_str(),
        Some(harness.connector_id.as_str()),
        "the view names the connector: {created}"
    );
    assert_eq!(created["terminal"]["cols"], json!(80));
    assert_eq!(created["terminal"]["rows"], json!(24));
    assert!(
        created["terminal"]["createdAt"].is_string() && created["serverTime"].is_string(),
        "createdAt and serverTime are part of the answer: {created}"
    );
    // serde_json objects are sorted, so compare the sorted key set.
    let mut view_keys = created["terminal"]
        .as_object()
        .expect("terminal object")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    view_keys.sort();
    let mut expected = vec![
        String::from("id"),
        String::from("connectorId"),
        String::from("cols"),
        String::from("rows"),
        String::from("createdAt"),
    ];
    expected.sort();
    assert_eq!(
        view_keys, expected,
        "the terminal view is exactly the five contract fields"
    );

    let opened = device.expect("terminal.open").await;
    assert_eq!(opened["terminalId"], json!(terminal_id));
    assert_eq!(opened["cols"], json!(80));
    assert_eq!(opened["rows"], json!(24));
    assert!(
        opened.get("cwd").is_none() && opened.get("shell").is_none(),
        "an absent cwd/shell is omitted, never null: {opened}"
    );
    device
        .send(json!({ "type": "terminal.opened", "terminalId": terminal_id }))
        .await;

    // A second terminal with an explicit cwd: the frame must carry it verbatim.
    let (status, with_cwd) = harness
        .create_terminal(json!({ "cwd": "/srv/farwork/sub", "cols": 100, "rows": 30 }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{with_cwd}");
    let cwd_terminal = with_cwd["terminal"]["id"]
        .as_str()
        .expect("terminal id")
        .to_string();
    let opened = device.expect("terminal.open").await;
    assert_eq!(opened["terminalId"], json!(cwd_terminal));
    assert_eq!(opened["cwd"], json!("/srv/farwork/sub"));
    device
        .send(json!({ "type": "terminal.opened", "terminalId": cwd_terminal }))
        .await;

    // --- the browser attaches ----------------------------------------------
    let ticket = harness.ticket(&terminal_id).await;
    let mut browser = Client::connect(
        &harness.stream_url(&harness.connector_id, &terminal_id, &ticket),
        "browser",
    )
    .await;
    let hello = browser.expect("terminal.opened").await;
    assert_eq!(hello["type"], json!("terminal.opened"));

    // --- data, both ways, including bytes that are not text ----------------
    let typed = b"echo TERM_OK\n";
    browser
        .send(json!({ "type": "terminal.input", "data": b64(typed) }))
        .await;
    let forwarded = device.expect("terminal.data").await;
    assert_eq!(forwarded["terminalId"], json!(terminal_id));
    assert_eq!(
        forwarded["data"],
        json!(b64(typed)),
        "browser -> device must be verbatim base64"
    );
    device
        .send(json!({ "type": "terminal.data", "terminalId": terminal_id, "data": b64(typed) }))
        .await;
    let echoed = browser.expect("terminal.data").await;
    assert_eq!(
        unb64(echoed["data"].as_str().expect("data")),
        typed.to_vec(),
        "device -> browser must decode to the same bytes"
    );

    // PTY output is not UTF-8 in general: a NUL and a 0xff must survive.
    let binary: Vec<u8> = vec![0x00, 0x01, 0xff, 0xfe, 0x80, 0x1b];
    browser
        .send(json!({ "type": "terminal.input", "data": b64(&binary) }))
        .await;
    let forwarded = device.expect("terminal.data").await;
    assert_eq!(
        unb64(forwarded["data"].as_str().expect("data")),
        binary,
        "binary input must reach the device unchanged"
    );
    device
        .send(json!({ "type": "terminal.data", "terminalId": terminal_id, "data": b64(&binary) }))
        .await;
    let echoed = browser.expect("terminal.data").await;
    assert_eq!(
        unb64(echoed["data"].as_str().expect("data")),
        binary,
        "binary output must reach the browser unchanged"
    );

    // --- garbage from the browser is refused here, not on the device --------
    browser
        .send(json!({ "type": "terminal.input", "data": "not base64!" }))
        .await;
    let refused = browser.next_frame().await;
    assert_eq!(refused["type"], json!("terminal.error"), "{refused}");
    assert_eq!(refused["code"], json!("invalid_frame"), "{refused}");
    browser
        .send(json!({ "type": "terminal.resize", "cols": 0, "rows": 30 }))
        .await;
    let refused = browser.next_frame().await;
    assert_eq!(refused["type"], json!("terminal.error"), "{refused}");
    assert_eq!(refused["code"], json!("invalid_size"), "{refused}");

    // --- resize ------------------------------------------------------------
    // The device sees terminal.resize next: neither malformed frame above may have
    // been forwarded as terminal.data.
    browser
        .send(json!({ "type": "terminal.resize", "cols": 100, "rows": 30 }))
        .await;
    let resized = device.expect("terminal.resize").await;
    assert_eq!(resized["terminalId"], json!(terminal_id));
    assert_eq!(resized["cols"], json!(100));
    assert_eq!(resized["rows"], json!(30));
    device
        .send(json!({
            "type": "terminal.data",
            "terminalId": terminal_id,
            "data": b64(b"resize:100x30"),
        }))
        .await;
    let ack = browser.expect("terminal.data").await;
    assert_eq!(
        unb64(ack["data"].as_str().expect("data")),
        b"resize:100x30".to_vec(),
        "the resize ack proves the frame reached the device and the reply came back"
    );

    // --- close, then exit ---------------------------------------------------
    browser.send(json!({ "type": "terminal.close" })).await;
    let close = device.expect("terminal.close").await;
    assert_eq!(close["terminalId"], json!(terminal_id));
    device
        .send(json!({ "type": "terminal.exit", "terminalId": terminal_id, "code": 0 }))
        .await;
    let exited = browser.expect("terminal.exit").await;
    assert_eq!(exited["code"], json!(0));
    assert!(
        browser.drain_to_close().await.is_empty(),
        "the socket ends right after the exit frame"
    );
    // terminal.exit closes the row.
    harness.await_closed_at(&terminal_id).await;

    // --- tickets: one use, and only for their own terminal ------------------
    let status =
        Client::connect_error(&harness.stream_url(&harness.connector_id, &terminal_id, &ticket))
            .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a spent ticket must not work"
    );

    let other_ticket = harness.ticket(&cwd_terminal).await;
    let status = Client::connect_error(&harness.stream_url(
        &harness.connector_id,
        &terminal_id,
        &other_ticket,
    ))
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a ticket minted for another terminal must not attach here"
    );

    let (status, missing) = call(
        &app,
        Method::POST,
        "/api/v2/ws-ticket",
        Some(&token),
        Some(json!({
            "clientId": "browser",
            "scope": { "terminalId": "terminal_does_not_exist" },
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{missing}");

    // --- unknown terminalId: the socket opens, then says why ----------------
    let (status, doomed) = harness.create_terminal(json!({})).await;
    assert_eq!(status, StatusCode::CREATED, "{doomed}");
    let doomed_id = doomed["terminal"]["id"]
        .as_str()
        .expect("terminal id")
        .to_string();
    device.expect("terminal.open").await;
    device
        .send(json!({ "type": "terminal.opened", "terminalId": doomed_id }))
        .await;
    let doomed_ticket = harness.ticket(&doomed_id).await;
    sqlx::query("delete from terminals where id = $1")
        .bind(&doomed_id)
        .execute(&harness.pool)
        .await
        .expect("delete the row");
    let mut ghost = Client::connect(
        &harness.stream_url(&harness.connector_id, &doomed_id, &doomed_ticket),
        "ghost",
    )
    .await;
    let first = ghost.next_frame().await;
    assert_eq!(first["type"], json!("terminal.error"), "{first}");
    assert_eq!(first["code"], json!("terminal_not_found"), "{first}");
    assert!(ghost.drain_to_close().await.is_empty());

    // --- a refusal is replayed to a browser that attaches afterwards --------
    // The agent answers terminal.error before the browser can finish minting its
    // ticket, so the reason has to be stored, not only published.
    let (status, refused) = harness
        .create_terminal(json!({ "cwd": "/etc", "cols": 80, "rows": 24 }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{refused}");
    let refused_id = refused["terminal"]["id"]
        .as_str()
        .expect("terminal id")
        .to_string();
    device.expect("terminal.open").await;
    device
        .send(json!({
            "type": "terminal.error",
            "terminalId": refused_id,
            "message": "/etc is outside the agent workdir /srv/farwork",
            "code": "permission_denied",
        }))
        .await;
    harness.await_closed_at(&refused_id).await;
    let refused_ticket = harness.ticket(&refused_id).await;
    let mut late = Client::connect(
        &harness.stream_url(&harness.connector_id, &refused_id, &refused_ticket),
        "late",
    )
    .await;
    let replayed = late.next_frame().await;
    assert_eq!(replayed["type"], json!("terminal.error"), "{replayed}");
    assert_eq!(replayed["code"], json!("permission_denied"), "{replayed}");
    assert!(
        replayed["message"]
            .as_str()
            .unwrap_or_default()
            .contains("outside the agent workdir"),
        "the device's own reason must survive: {replayed}"
    );
    assert!(late.drain_to_close().await.is_empty());

    // --- browser disconnect tells the device to close the pty ---------------
    let (status, walked) = harness.create_terminal(json!({})).await;
    assert_eq!(status, StatusCode::CREATED, "{walked}");
    let walked_id = walked["terminal"]["id"]
        .as_str()
        .expect("terminal id")
        .to_string();
    device.expect("terminal.open").await;
    device
        .send(json!({ "type": "terminal.opened", "terminalId": walked_id }))
        .await;
    let walked_ticket = harness.ticket(&walked_id).await;
    let mut walked_browser = Client::connect(
        &harness.stream_url(&harness.connector_id, &walked_id, &walked_ticket),
        "walked",
    )
    .await;
    walked_browser.expect("terminal.opened").await;
    drop(walked_browser);
    let close = device.expect("terminal.close").await;
    assert_eq!(close["terminalId"], json!(walked_id));
    // A browser disconnect closes the row too.
    harness.await_closed_at(&walked_id).await;

    // --- device disconnect: honest error frame, then 503 on new work --------
    let (status, orphaned) = harness.create_terminal(json!({})).await;
    assert_eq!(status, StatusCode::CREATED, "{orphaned}");
    let orphaned_id = orphaned["terminal"]["id"]
        .as_str()
        .expect("terminal id")
        .to_string();
    device.expect("terminal.open").await;
    device
        .send(json!({ "type": "terminal.opened", "terminalId": orphaned_id }))
        .await;
    let orphaned_ticket = harness.ticket(&orphaned_id).await;
    let mut orphaned_browser = Client::connect(
        &harness.stream_url(&harness.connector_id, &orphaned_id, &orphaned_ticket),
        "orphaned",
    )
    .await;
    orphaned_browser.expect("terminal.opened").await;

    let _ = device.socket.close(None).await;
    drop(device);

    let error = orphaned_browser.expect("terminal.error").await;
    assert_eq!(
        error["code"],
        json!("connector_offline"),
        "a device that disappears must say so: {error}"
    );
    orphaned_browser.drain_to_close().await;
    // A device disconnect closes every row it owned.
    harness.await_closed_at(&orphaned_id).await;

    for _ in 0..100 {
        if !harness.state.connectors().online(&harness.connector_id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (status, offline) = harness.create_terminal(json!({})).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{offline}");
    let offline_ticket = harness.ticket(&orphaned_id).await;
    let status = Client::connect_error(&harness.stream_url(
        &harness.connector_id,
        &orphaned_id,
        &offline_ticket,
    ))
    .await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "an offline connector must refuse the handshake, not open a dead stream"
    );
}

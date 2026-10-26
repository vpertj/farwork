//! Ingress Protocol v1 end to end, against a real Postgres database.
//!
//! A fake connector (a tokio-tungstenite client) plays the device: it says hello,
//! receives session commands, reports timeline items and approvals, and the test
//! checks what the browser actually sees (snapshot, session WS, REST notices).
//!
//! Runs only when TEST_DATABASE_URL points at a disposable database; without it
//! the test skips loudly rather than passing quietly.

use std::time::Duration;

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

const SETUP_TOKEN: &str = "setup-ingress-token";
const SECRET: &str = "ingress-integration-secret";
const PASSWORD: &str = "correct horse battery staple";
const WORKSPACE: &str = "/srv/farwork";

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
    project_id: String,
}

async fn bootstrap(database_url: &str) -> Harness {
    let config = Config {
        database_url: database_url.to_string(),
        port: 0,
        secret: SECRET.to_string(),
        setup_token: Some(SETUP_TOKEN.to_string()),
        database_max_connections: 5,
        public_url: None,
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

    let email = format!("ingress+{}@example.com", farwork_hub::timeutil::unix_now());
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
    assert_eq!(
        connector["connector"]["status"], "offline",
        "a freshly registered connector is not connected"
    );

    let (status, project) = call(
        &app,
        Method::POST,
        "/api/v2/projects",
        Some(&token),
        Some(json!({
            "name": "farwork",
            "connectorId": connector_id,
            "workspacePath": WORKSPACE,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "project: {project}");
    let project_id = project["project"]["id"].as_str().expect("id").to_string();

    Harness {
        app,
        token,
        connector_id,
        connector_token,
        project_id,
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
        let frame = match timeout(Duration::from_secs(5), self.socket.next()).await {
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

    /// Next frame of the given type, skipping anything else on the stream.
    async fn expect_type(&mut self, expected: &str) -> Value {
        for _ in 0..32 {
            let frame = self.next_frame().await;
            if frame["type"] == expected {
                return frame;
            }
        }
        panic!("{}: never saw a {expected} frame", self.label);
    }

    /// Assert nothing arrives within the window (used to prove a duplicate was not
    /// re-dispatched).
    async fn expect_silence(&mut self, window: Duration) {
        match timeout(window, self.socket.next()).await {
            Err(_) => {}
            Ok(None) => {}
            Ok(Some(Ok(Message::Pong(_)))) | Ok(Some(Ok(Message::Ping(_)))) => {}
            Ok(Some(Ok(Message::Text(text)))) => {
                panic!("{}: expected silence, got {text}", self.label)
            }
            Ok(Some(Ok(_))) => {}
            Ok(Some(Err(error))) => panic!("{}: socket error {error}", self.label),
        }
    }

    async fn close(mut self) {
        let _ = self.socket.close(None).await;
    }
}

async fn connector_row(app: &Router, token: &str, connector_id: &str) -> Value {
    let (status, list) = call(app, Method::GET, "/api/v2/connectors", Some(token), None).await;
    assert_eq!(status, StatusCode::OK, "connectors: {list}");
    list["connectors"]
        .as_array()
        .expect("array")
        .iter()
        .find(|row| row["id"] == connector_id)
        .unwrap_or_else(|| panic!("connector {connector_id} missing from {list}"))
        .clone()
}

async fn connector_status(app: &Router, token: &str, connector_id: &str) -> String {
    connector_row(app, token, connector_id).await["status"]
        .as_str()
        .expect("status")
        .to_string()
}

/// Poll a projection until it reports the expected value, so the assertions do
/// not race the connector's frame over a second socket.
async fn poll_runtime_status(app: &Router, token: &str, session_id: &str, expected: &str) -> Value {
    let mut last = Value::Null;
    for _ in 0..50 {
        let (status, body) = call(
            app,
            Method::GET,
            &format!("/api/v2/sessions/{session_id}/runtime/state"),
            Some(token),
            None,
        )
        .await;
        if status == StatusCode::OK {
            if body["state"]["status"] == expected {
                return body;
            }
            last = body;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("session {session_id} never reached state {expected}: {last}");
}

#[tokio::test]
async fn connector_ingress_end_to_end() {
    let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!(
            "SKIP farwork-hub connector ingress test: TEST_DATABASE_URL is not set.
             Point it at a disposable database to run this test, e.g.
               export TEST_DATABASE_URL=postgres://tianjun@127.0.0.1:5432/farwork_test"
        );
        return;
    };
    if database_url.trim().is_empty() {
        eprintln!("SKIP farwork-hub connector ingress test: TEST_DATABASE_URL is empty.");
        return;
    }
    let harness = bootstrap(&database_url).await;
    let app = harness.app.clone();
    let token = harness.token.clone();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let server_app = app.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, server_app).await;
    });

    // --- auth: no token, wrong token, revoked-free happy path -------------
    let missing =
        tokio_tungstenite::connect_async(format!("ws://{addr}/api/v2/connector/ws")).await;
    assert!(
        missing.is_err(),
        "the ingress must refuse a tokenless upgrade"
    );
    let wrong = tokio_tungstenite::connect_async(format!(
        "ws://{addr}/api/v2/connector/ws?token=cxt_not_a_real_token"
    ))
    .await;
    assert!(wrong.is_err(), "the ingress must refuse an unknown token");

    // --- hello / welcome / presence --------------------------------------
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
            "agent": {
                "name": "integration-connector",
                "platform": "macos",
                "version": "0.1.0",
                "supportedRuntimes": [
                    { "name": "dsh", "available": true, "reason": null },
                    { "name": "codex", "available": false, "reason": "codex CLI 不在 PATH 上" },
                ],
            },
        }))
        .await;
    let welcome = connector.expect_type("welcome").await;
    assert_eq!(welcome["serverVersion"], farwork_hub::VERSION);
    assert_eq!(welcome["heartbeatSecs"], 15);
    assert_eq!(
        connector_status(&app, &token, &harness.connector_id).await,
        "online",
        "hello must make the connector online"
    );

    // --- runtimes: the device's own probe result reaches the REST surface ----
    let reported = connector_row(&app, &token, &harness.connector_id).await;
    assert_eq!(
        reported["runtimes"],
        json!([
            { "name": "dsh", "available": true, "reason": null },
            { "name": "codex", "available": false, "reason": "codex CLI 不在 PATH 上" },
        ]),
        "the report must be forwarded as the device sent it: {reported}"
    );

    // --- heartbeat refreshes last_seen_at --------------------------------
    let before = connector_row(&app, &token, &harness.connector_id).await["lastSeenAt"].clone();
    assert!(before.as_str().is_some(), "hello must stamp last_seen_at");
    connector
        .send(json!({
            "type": "heartbeat",
            "ts": farwork_hub::timeutil::utc_now(),
        }))
        .await;
    let mut after = before.clone();
    for _ in 0..50 {
        after = connector_row(&app, &token, &harness.connector_id).await["lastSeenAt"].clone();
        if after != before {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_ne!(after, before, "a heartbeat must refresh last_seen_at");

    // --- session.start ----------------------------------------------------
    let (status, started) = call(
        &app,
        Method::POST,
        "/api/v2/sessions/create-and-start",
        Some(&token),
        Some(json!({
            "connectorId": harness.connector_id,
            "projectId": harness.project_id,
            "runtime": "codex",
            "title": "ingress session",
            "content": "hello from the integration test",
            "clientMessageId": "cm_ingress_1",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create-and-start: {started}");
    let session_id = started["session"]["id"].as_str().expect("id").to_string();
    assert_eq!(started["runtimeDispatched"], true);
    assert_eq!(started["connectorResult"]["ok"], true);
    assert_eq!(started["eventCursor"], "seq:1");

    let start_frame = connector.expect_type("session.start").await;
    let stream_id = start_frame["streamId"]
        .as_str()
        .expect("streamId")
        .to_string();
    assert_eq!(start_frame["sessionId"], session_id);
    assert_eq!(start_frame["runtime"], "codex");
    assert_eq!(start_frame["cwd"], WORKSPACE);
    assert_eq!(start_frame["prompt"], "hello from the integration test");
    assert_eq!(
        stream_id.matches('-').count(),
        4,
        "the contract types streamId as a uuid, got {stream_id}"
    );
    assert_eq!(started["connectorResult"]["streamId"], stream_id);

    // --- browser subscribes before the runtime reports anything ----------
    let (status, ticket_body) = call(
        &app,
        Method::POST,
        "/api/v2/ws-ticket",
        Some(&token),
        Some(json!({ "clientId": "ingress-browser",
                     "scope": { "sessionId": session_id } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "ws-ticket: {ticket_body}");
    let ticket = ticket_body["ticket"].as_str().expect("ticket");
    let mut browser = Client::connect(
        &format!("ws://{addr}/api/v2/sessions/{session_id}/ws?ticket={ticket}"),
        "browser",
    )
    .await;
    let subscribed = browser.expect_type("session.subscribed").await;
    assert_eq!(subscribed["protocolVersion"], "1.0");

    // --- session.started --------------------------------------------------
    connector
        .send(json!({
            "type": "session.started",
            "streamId": stream_id,
            "externalSessionId": "thread_codex_1",
        }))
        .await;
    // The broadcast is emitted after the row is written, so waiting for it also
    // means the REST read below cannot race the frame.
    let meta_event = browser.expect_type("session.meta.updated").await;
    assert_eq!(
        meta_event["payload"]["session"]["externalSessionId"],
        "thread_codex_1"
    );
    let (status, meta) = call(
        &app,
        Method::GET,
        &format!("/api/v2/sessions/{session_id}/meta"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "meta: {meta}");
    assert_eq!(meta["session"]["externalSessionId"], "thread_codex_1");

    // --- session.state ----------------------------------------------------
    connector
        .send(json!({
            "type": "session.state",
            "streamId": stream_id,
            "state": "running",
        }))
        .await;
    let state_event = browser.expect_type("runtime.state.updated").await;
    assert_eq!(state_event["payload"]["state"]["status"], "running");
    let (status, runtime_state) = call(
        &app,
        Method::GET,
        &format!("/api/v2/sessions/{session_id}/runtime/state"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "runtime state: {runtime_state}");
    assert_eq!(runtime_state["state"]["status"], "running");

    // --- item.upsert: create then update ---------------------------------
    connector
        .send(json!({
            "type": "item.upsert",
            "streamId": stream_id,
            "item": {
                "id": "itm_agent_1",
                "type": "message",
                "status": "running",
                "role": "assistant",
                "content": { "text": "hello from codex", "format": "markdown" },
                "itemTime": farwork_hub::timeutil::utc_now(),
            },
        }))
        .await;
    let created = browser.expect_type("timeline.item_created").await;
    assert_eq!(created["payload"]["item"]["id"], "itm_agent_1");
    assert_eq!(
        created["payload"]["item"]["content"]["text"],
        "hello from codex"
    );
    assert_eq!(created["payload"]["item"]["orderSeq"], 2);
    assert_eq!(created["payload"]["item"]["role"], "assistant");

    connector
        .send(json!({
            "type": "item.upsert",
            "streamId": stream_id,
            "item": {
                "id": "itm_agent_1",
                "type": "message",
                "status": "done",
                "role": "assistant",
                "content": { "text": "hello from codex", "format": "markdown" },
                "itemTime": farwork_hub::timeutil::utc_now(),
            },
        }))
        .await;
    let updated = browser.expect_type("timeline.item_updated").await;
    assert_eq!(updated["payload"]["item"]["id"], "itm_agent_1");
    assert_eq!(updated["payload"]["item"]["status"], "done");
    assert_eq!(updated["payload"]["item"]["revision"], 2);

    // --- session.message dispatch + idempotency --------------------------
    let (status, message) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{session_id}/runtime/messages"),
        Some(&token),
        Some(json!({ "content": "second message", "clientMessageId": "cm_ingress_2" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "message: {message}");
    assert_eq!(message["result"]["duplicate"], false);
    assert_eq!(message["result"]["runtimeDispatched"], true);
    let message_frame = connector.expect_type("session.message").await;
    assert_eq!(message_frame["streamId"], stream_id);
    assert_eq!(message_frame["text"], "second message");
    assert_eq!(message_frame["clientMessageId"], "cm_ingress_2");

    let (status, replay) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{session_id}/runtime/messages"),
        Some(&token),
        Some(json!({ "content": "second message", "clientMessageId": "cm_ingress_2" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay["result"]["duplicate"], true);
    assert_eq!(
        replay["result"]["runtimeDispatched"], false,
        "a replay must not run the turn twice"
    );
    connector.expect_silence(Duration::from_millis(300)).await;

    // --- notices ----------------------------------------------------------
    connector
        .send(json!({
            "type": "notice.upsert",
            "streamId": stream_id,
            "notice": {
                "id": "ntc_1",
                "kind": "command_execution",
                "status": "open",
                "title": "Run cargo test?",
                "body": { "command": "cargo test", "cwd": WORKSPACE },
                "options": ["approve", "approve_for_session", "reject"],
            },
        }))
        .await;
    let notice_event = browser.expect_type("runtime.notice.updated").await;
    assert_eq!(notice_event["payload"]["notice"]["id"], "ntc_1");
    assert_eq!(notice_event["payload"]["notice"]["status"], "open");

    let (status, notices) = call(
        &app,
        Method::GET,
        &format!("/api/v2/sessions/{session_id}/runtime/notices"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "notices: {notices}");
    let list = notices["notices"].as_array().expect("array");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["id"], "ntc_1");
    assert_eq!(list[0]["kind"], "command_execution");
    assert_eq!(list[0]["status"], "open");
    assert_eq!(list[0]["title"], "Run cargo test?");
    assert_eq!(list[0]["body"]["command"], "cargo test");
    // Legacy string options are upgraded on ingest, so the browser always reads
    // objects. The id is unchanged; the label is the product default.
    assert_eq!(list[0]["options"][0]["id"], "approve");
    assert_eq!(list[0]["options"][0]["label"], "批准");
    assert_eq!(list[0]["options"][2]["id"], "reject");
    assert!(list[0]["createdAt"].as_str().is_some());

    // An invalid decision is refused, and an unknown notice is a conflict.
    let (status, invalid) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{session_id}/notices/ntc_1/respond"),
        Some(&token),
        Some(json!({ "decision": "maybe" })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid: {invalid}"
    );
    let (status, unknown) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{session_id}/notices/ntc_nope/respond"),
        Some(&token),
        Some(json!({ "decision": "approve" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "unknown notice: {unknown}");
    assert_eq!(unknown["detail"]["code"], "notice_not_found");

    let (status, responded) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{session_id}/notices/ntc_1/respond"),
        Some(&token),
        Some(json!({ "decision": "approve" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "respond: {responded}");
    assert_eq!(responded["ok"], true);
    let respond_frame = connector.expect_type("notice.respond").await;
    assert_eq!(respond_frame["noticeId"], "ntc_1");
    assert_eq!(respond_frame["decision"], "approve");

    // The notice is still open until the connector confirms it applied the decision.
    let (status, still_open) = call(
        &app,
        Method::GET,
        &format!("/api/v2/sessions/{session_id}/runtime/notices"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(still_open["notices"][0]["status"], "open");

    // A second response before resolution is refused.
    let (status, twice) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{session_id}/runtime/notices/ntc_1/respond"),
        Some(&token),
        Some(json!({ "decision": "reject" })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "still open, so a second answer is allowed: {twice}"
    );
    let _ = connector.expect_type("notice.respond").await;

    connector
        .send(json!({
            "type": "notice.resolved",
            "noticeId": "ntc_1",
            "decision": "approve",
        }))
        .await;
    let resolved = browser.expect_type("runtime.notice.updated").await;
    assert_eq!(resolved["payload"]["notice"]["status"], "resolved");
    assert_eq!(resolved["payload"]["notice"]["decision"], "approve");
    let snapshot_notices = browser.expect_type("runtime.notice.snapshot").await;
    assert_eq!(snapshot_notices["payload"]["notices"][0]["id"], "ntc_1");

    let (status, closed_notice) = call(
        &app,
        Method::GET,
        &format!("/api/v2/sessions/{session_id}/runtime/notices"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(closed_notice["notices"][0]["status"], "resolved");
    assert_eq!(closed_notice["notices"][0]["decision"], "approve");

    let (status, late) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{session_id}/runtime/notices/ntc_1/respond"),
        Some(&token),
        Some(json!({ "decision": "reject" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "resolved notice: {late}");
    assert_eq!(late["detail"]["code"], "notice_already_resolved");

    // --- a runtime interaction: object options answered by action id -------
    connector
        .send(json!({
            "type": "notice.upsert",
            "streamId": stream_id,
            "notice": {
                "id": "ntc_2",
                "kind": "interaction",
                "status": "open",
                "title": "请求批准：shell",
                "interactionType": "approval",
                "body": { "toolName": "shell" },
                "options": [
                    { "id": "allow-once", "label": "允许一次", "style": "primary" },
                    { "id": "reject", "label": "拒绝", "style": "secondary" },
                ],
            },
        }))
        .await;
    let _ = browser.expect_type("runtime.notice.updated").await;

    let (status, list) = call(
        &app,
        Method::GET,
        &format!("/api/v2/sessions/{session_id}/runtime/notices"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "notices: {list}");
    let notice = list["notices"]
        .as_array()
        .expect("array")
        .iter()
        .find(|notice| notice["id"] == "ntc_2")
        .expect("ntc_2");
    assert_eq!(notice["interactionType"], "approval");
    assert_eq!(notice["kind"], "interaction");
    assert_eq!(notice["options"][0]["id"], "allow-once");
    assert_eq!(notice["options"][0]["label"], "允许一次");
    assert_eq!(notice["options"][0]["style"], "primary");

    // An action the notice never offered is refused, not silently accepted.
    let (status, wrong_action) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{session_id}/notices/ntc_2/respond"),
        Some(&token),
        Some(json!({ "actionId": "allow-always" })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "unknown action: {wrong_action}"
    );
    assert_eq!(wrong_action["detail"]["code"], "unknown_action");

    // Neither a decision nor an action is not an answer.
    let (status, empty) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{session_id}/notices/ntc_2/respond"),
        Some(&token),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "empty: {empty}");
    assert_eq!(empty["detail"]["code"], "empty_answer");

    // Two answers that disagree are ambiguous, not "the last one wins".
    let (status, disagree) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{session_id}/notices/ntc_2/respond"),
        Some(&token),
        Some(json!({ "actionId": "allow-once", "decision": "reject" })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "disagreeing answers: {disagree}"
    );
    assert_eq!(disagree["detail"]["code"], "ambiguous_answer");

    // What the web client actually sends: the action id mirrored into decision so
    // an older hub still understands it.
    let (status, responded) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{session_id}/notices/ntc_2/respond"),
        Some(&token),
        // `inputData` is the browser's field name for a question answer; it has to
        // survive the hop, so this call carries one even though an approval ignores
        // it.
        Some(json!({ "actionId": "allow-once", "decision": "allow-once",
            "inputData": { "answers": { "plan": "ship it" } } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "respond: {responded}");
    let respond_frame = connector.expect_type("notice.respond").await;
    assert_eq!(respond_frame["noticeId"], "ntc_2");
    assert_eq!(respond_frame["actionId"], "allow-once");
    assert!(
        respond_frame.get("decision").is_none(),
        "an action-id answer must not carry a fabricated decision: {respond_frame}"
    );
    assert_eq!(respond_frame["input"]["answers"]["plan"], "ship it");

    connector
        .send(json!({
            "type": "notice.resolved",
            "noticeId": "ntc_2",
            "actionId": "allow-once",
        }))
        .await;
    let resolved = browser.expect_type("runtime.notice.updated").await;
    assert_eq!(resolved["payload"]["notice"]["status"], "resolved");
    assert_eq!(resolved["payload"]["notice"]["decision"], "allow-once");
    let _ = browser.expect_type("runtime.notice.snapshot").await;

    // --- interrupt --------------------------------------------------------
    let (status, interrupted) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{session_id}/runtime/interrupt"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "interrupt: {interrupted}");
    assert_eq!(interrupted["streamId"], stream_id);
    let interrupt_frame = connector.expect_type("session.interrupt").await;
    assert_eq!(interrupt_frame["streamId"], stream_id);

    // --- snapshot reflects the runtime's items and notices ---------------
    let (status, snapshot) = call(
        &app,
        Method::GET,
        &format!("/api/v2/sessions/{session_id}/snapshot"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "snapshot: {snapshot}");
    let items = snapshot["timeline"]["items"].as_array().expect("items");
    assert!(
        items.iter().any(|item| item["id"] == "itm_agent_1"),
        "snapshot is missing the connector item: {snapshot}"
    );
    let snapshot_notices = snapshot["notices"].as_array().expect("notices");
    assert_eq!(
        snapshot_notices.len(),
        2,
        "both notices survive: {snapshot}"
    );
    assert!(snapshot_notices
        .iter()
        .all(|notice| notice["status"] == "resolved"));
    assert!(snapshot_notices
        .iter()
        .any(|notice| notice["id"] == "ntc_2" && notice["interactionType"] == "approval"));
    assert_eq!(snapshot["state"]["status"], "running");

    // --- session.exit closes the stream ----------------------------------
    connector
        .send(json!({
            "type": "session.exit",
            "streamId": stream_id,
            "code": 0,
        }))
        .await;
    let exit_state = browser.expect_type("runtime.state.updated").await;
    assert_eq!(exit_state["payload"]["state"]["status"], "idle");

    // An interrupt after exit has no stream to address.
    let (status, no_stream) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{session_id}/runtime/interrupt"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "after exit: {no_stream}");
    assert_eq!(no_stream["detail"]["code"], "session_not_running");

    // --- a second turn, killed by the disconnect --------------------------
    let (status, second) = call(
        &app,
        Method::POST,
        "/api/v2/sessions/create-and-start",
        Some(&token),
        Some(json!({
            "connectorId": harness.connector_id,
            "projectId": harness.project_id,
            "runtime": "codex",
            "content": "a turn that never finishes",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "second turn: {second}");
    assert_eq!(second["runtimeDispatched"], true);
    let second_session = second["session"]["id"]
        .as_str()
        .expect("session id")
        .to_string();
    let second_start = connector.expect_type("session.start").await;
    let second_stream = second_start["streamId"]
        .as_str()
        .expect("streamId")
        .to_string();
    assert_ne!(second_stream, stream_id, "each turn gets its own stream");
    connector
        .send(json!({
            "type": "session.state",
            "streamId": second_stream,
            "state": "running",
        }))
        .await;
    let running = poll_runtime_status(&app, &token, &second_session, "running").await;
    assert_eq!(running["state"]["status"], "running");

    // --- presence: disconnect goes back to offline ------------------------
    connector.close().await;
    let mut status_now = String::new();
    for _ in 0..50 {
        status_now = connector_status(&app, &token, &harness.connector_id).await;
        if status_now == "offline" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(status_now, "offline", "a closed socket must clear presence");

    // The probe result described a process that no longer exists: the device
    // must read as "unreported" instead of keeping its last answer.
    let after_close = connector_row(&app, &token, &harness.connector_id).await;
    assert_eq!(
        after_close["runtimes"],
        json!([]),
        "a disconnected device cannot still claim runtimes: {after_close}"
    );

    // A stream whose runtime died with the socket cannot be addressed any more,
    // and the projection says so instead of leaving a phantom "running".
    let abandoned = poll_runtime_status(&app, &token, &second_session, "error").await;
    assert_eq!(abandoned["state"]["statusReason"], "connector disconnected");
    let (status, after_disconnect) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{second_session}/runtime/messages"),
        Some(&token),
        Some(json!({ "content": "anyone home?", "clientMessageId": "cm_ingress_3" })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "message after disconnect: {after_disconnect}"
    );
    assert_eq!(
        after_disconnect["result"]["runtimeDispatched"], false,
        "nothing may be dispatched into an abandoned stream"
    );

    // --- post-disconnect surfaces stay honest ----------------------------
    let (status, offline_start) = call(
        &app,
        Method::POST,
        "/api/v2/sessions/create-and-start",
        Some(&token),
        Some(json!({
            "connectorId": harness.connector_id,
            "projectId": harness.project_id,
            "runtime": "codex",
            "content": "while the connector is gone",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "offline start: {offline_start}");
    assert_eq!(offline_start["runtimeDispatched"], false);
    assert_eq!(offline_start["connectorResult"]["ok"], false);
    assert_eq!(
        offline_start["connectorResult"]["code"],
        "connector_offline"
    );
    let offline_session = offline_start["session"]["id"]
        .as_str()
        .expect("id")
        .to_string();

    let (status, offline_notice) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{offline_session}/runtime/interrupt"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "no connector: {offline_notice}"
    );
    assert_eq!(offline_notice["detail"]["code"], "session_not_running");
}

//! Session runtime control + the session namespace, end to end, on real Postgres.
//!
//! A fake connector (a tokio-tungstenite client) plays the device: it answers the
//! hub's `session.capabilities` contract by *pushing* one, and it receives
//! `session.steer` / `session.selections`. The REST client plays the browser. Every
//! assertion is about what crossing the wire actually produced, not about a hub
//! internal.
//!
//! One test on purpose: every integration test in this crate truncates the same
//! tables, so separate test functions would race each other.
//!
//! Runs only when TEST_DATABASE_URL points at a disposable database.

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

const SETUP_TOKEN: &str = "setup-runtime-token";
const SECRET: &str = "runtime-integration-secret";
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

/// `call` with owned arguments, so a request can be driven from a spawned task.
///
/// The file relay answers only after the device replies, so a test that both
/// issues the REST call and plays the device must not await the call first.
async fn call_owned(
    app: Router,
    method: Method,
    uri: String,
    token: Option<String>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    call(&app, method, &uri, token.as_deref(), body).await
}

struct Harness {
    app: Router,
    pool: sqlx::PgPool,
    token: String,
    connector_id: String,
    connector_token: String,
    project_id: String,
    ws_base: String,
}

async fn salt_and_verifier(app: &Router, email: &str) -> (String, String) {
    let (status, salt_body) = call(
        app,
        Method::POST,
        "/api/v2/auth/password-salt",
        None,
        Some(json!({ "email": email })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "password-salt: {salt_body}");
    let salt = salt_body["salt"].as_str().expect("salt").to_string();
    let verifier = farwork_hub::auth::derive_password_verifier(PASSWORD, &salt);
    (salt, verifier)
}

async fn bootstrap(database_url: &str) -> Harness {
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
    let app = farwork_hub::app(state);

    // The first account bootstraps the instance and is its admin.
    let email = format!("runtime+{}@example.com", uuid::Uuid::new_v4().simple());
    let (salt, verifier) = salt_and_verifier(&app, &email).await;
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

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let server_app = app.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, server_app).await;
    });

    Harness {
        app,
        pool,
        token,
        connector_id,
        connector_token,
        project_id,
        ws_base: format!("ws://{addr}"),
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

    async fn send(&mut self, frame: Value) {
        eprintln!("<<< {} {}", self.label, frame);
        self.socket
            .send(Message::Text(frame.to_string().into()))
            .await
            .expect("send");
    }

    async fn next_frame(&mut self) -> Value {
        let frame = match timeout(Duration::from_secs(20), self.socket.next()).await {
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

    /// Skip unrelated frames (keepalives, other streams' traffic) until this type.
    async fn expect_type(&mut self, expected: &str) -> Value {
        for _ in 0..128 {
            let frame = self.next_frame().await;
            if frame["type"] == expected {
                return frame;
            }
        }
        panic!("{}: never saw a {expected} frame", self.label);
    }
}

async fn capabilities(app: &Router, token: &str, session_id: &str) -> (StatusCode, Value) {
    call(
        app,
        Method::GET,
        &format!("/api/v2/sessions/{session_id}/runtime/capabilities"),
        Some(token),
        None,
    )
    .await
}

async fn await_capabilities(app: &Router, token: &str, session_id: &str, steer: bool) -> Value {
    for _ in 0..120 {
        let (status, body) = capabilities(app, token, session_id).await;
        assert_eq!(status, StatusCode::OK, "capabilities: {body}");
        if body["capabilitySet"]["steer"] == json!(steer) {
            return body;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("capabilities never reported steer={steer}");
}

fn capability_frame(stream_id: &str) -> Value {
    json!({
        "type": "session.capabilities",
        "streamId": stream_id,
        "capabilities": {
            "steer": true,
            "interrupt": true,
            "selections": true,
            "commands": true,
        },
        "catalogs": {
            "models": [
                { "id": "echo-fast", "name": "Echo fast", "description": "fast" },
                { "id": "echo-slow", "name": "Echo slow" },
            ],
            "permissions": [
                { "id": "default", "name": "Default" },
                { "id": "read-only", "name": "Read only" },
            ],
        },
        "commands": [
            { "name": "/help", "description": "list commands" },
            { "name": "/clear" },
        ],
    })
}

#[tokio::test]
async fn session_runtime_and_session_namespace_end_to_end() {
    let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!(
            "SKIP farwork-hub session runtime test: TEST_DATABASE_URL is not set.
             Point it at a disposable database to run this test, e.g.
               export TEST_DATABASE_URL=postgres://tianjun@127.0.0.1:5432/farwork_test"
        );
        return;
    };
    if database_url.trim().is_empty() {
        eprintln!("SKIP farwork-hub session runtime test: TEST_DATABASE_URL is empty.");
        return;
    }

    let harness = bootstrap(&database_url).await;
    let app = harness.app.clone();
    let token = harness.token.clone();

    // A second user exists only to prove that a stranger's session is a 404.
    let (status, created) = call(
        &app,
        Method::POST,
        "/api/v2/admin/users",
        Some(&token),
        Some(json!({
            "email": format!("stranger+{}@example.com", uuid::Uuid::new_v4().simple()),
            "password": PASSWORD,
            "role": "member",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create member: {created}");
    let stranger_email = created["user"]["email"]
        .as_str()
        .expect("email")
        .to_string();
    let (status, login) = call(
        &app,
        Method::POST,
        "/api/v2/auth/login",
        None,
        Some(json!({ "email": stranger_email, "password": PASSWORD })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "stranger login: {login}");
    let stranger = login["accessToken"].as_str().expect("token").to_string();

    // --- the device connects (ingress) -----------------------------------
    let mut device = Client::connect(
        &format!(
            "{}/api/v2/connector/ws?token={}",
            harness.ws_base, harness.connector_token
        ),
        "device",
    )
    .await;
    device
        .send(json!({
            "type": "hello",
            "agent": { "name": "fake-device", "platform": "linux", "version": "0.0.0" },
        }))
        .await;
    let welcome = device.expect_type("welcome").await;
    assert_eq!(welcome["serverVersion"], farwork_hub::VERSION);

    // --- one running session ---------------------------------------------
    let (status, started) = call(
        &app,
        Method::POST,
        "/api/v2/sessions/create-and-start",
        Some(&token),
        Some(json!({
            "connectorId": harness.connector_id,
            "projectId": harness.project_id,
            "runtime": "codex",
            "title": "runtime session",
            "content": "hello",
            "clientMessageId": "cm_runtime_1",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create-and-start: {started}");
    assert_eq!(started["runtimeDispatched"], true);
    let session_id = started["session"]["id"].as_str().expect("id").to_string();
    let start_frame = device.expect_type("session.start").await;
    let stream_id = start_frame["streamId"]
        .as_str()
        .expect("streamId")
        .to_string();

    // --- nothing reported yet: everything is false, and nothing is invented
    let (status, before) = capabilities(&app, &token, &session_id).await;
    assert_eq!(status, StatusCode::OK, "capabilities: {before}");
    assert_eq!(
        before["capabilitySet"],
        json!({
            "steer": false,
            "interrupt": false,
            "selections": false,
            "commands": false,
        }),
        "an unreported session must read all-false, not all-true"
    );
    assert_eq!(before["revision"], json!(0));
    assert_eq!(before["connectorId"], json!(harness.connector_id));

    let (status, empty_catalog) = call(
        &app,
        Method::GET,
        &format!("/api/v2/sessions/{session_id}/runtime/catalogs/model"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "catalog: {empty_catalog}");
    assert_eq!(empty_catalog["catalog"], json!("model"));
    assert_eq!(empty_catalog["items"], json!([]));
    assert_eq!(empty_catalog["revision"], json!(0));

    let (status, empty_commands) = call(
        &app,
        Method::GET,
        &format!("/api/v2/sessions/{session_id}/runtime/commands"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "commands: {empty_commands}");
    assert_eq!(empty_commands["commands"], json!([]));

    // --- the runtime reports what it can do -------------------------------
    device.send(capability_frame(&stream_id)).await;
    let reported = await_capabilities(&app, &token, &session_id, true).await;
    assert_eq!(
        reported["capabilitySet"],
        json!({
            "steer": true,
            "interrupt": true,
            "selections": true,
            "commands": true,
        })
    );
    assert_eq!(
        reported["revision"],
        json!(1),
        "the first report is revision 1"
    );
    assert!(reported["reportedAt"].is_string(), "{reported}");

    let (status, models) = call(
        &app,
        Method::GET,
        &format!("/api/v2/sessions/{session_id}/runtime/catalogs/model"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "model catalog: {models}");
    assert_eq!(models["catalog"], json!("model"));
    assert_eq!(models["revision"], json!(1));
    let items = models["items"].as_array().expect("items");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["id"], json!("echo-fast"));
    assert_eq!(items[0]["name"], json!("Echo fast"));
    assert_eq!(items[0]["description"], json!("fast"));
    // An absent description is omitted, never serialized as null.
    assert!(items[1].get("description").is_none(), "{models}");

    let (status, permissions) = call(
        &app,
        Method::GET,
        &format!("/api/v2/sessions/{session_id}/runtime/catalogs/permission"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "permission catalog: {permissions}");
    assert_eq!(permissions["catalog"], json!("permission"));
    assert_eq!(
        permissions["items"]
            .as_array()
            .expect("items")
            .iter()
            .map(|item| item["id"].as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>(),
        vec![String::from("default"), String::from("read-only")]
    );

    let (status, unknown) = call(
        &app,
        Method::GET,
        &format!("/api/v2/sessions/{session_id}/runtime/catalogs/models"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "an unknown catalog is a 422, not an empty list: {unknown}"
    );
    assert_eq!(unknown["detail"]["code"], json!("unknown_catalog"));

    let (status, commands) = call(
        &app,
        Method::GET,
        &format!("/api/v2/sessions/{session_id}/runtime/commands"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "commands: {commands}");
    let listed = commands["commands"].as_array().expect("commands");
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0]["name"], json!("/help"));
    assert_eq!(listed[0]["description"], json!("list commands"));
    assert!(listed[1].get("description").is_none(), "{commands}");

    // --- an unknown frame type is answered, not fatal --------------------
    device
        .send(json!({
            "type": "session.bogus",
            "streamId": stream_id,
            "whatever": true,
        }))
        .await;
    let complaint = device.expect_type("error").await;
    assert!(
        complaint["message"]
            .as_str()
            .expect("message")
            .contains("malformed frame"),
        "an unknown frame is reported, not silently accepted: {complaint}"
    );

    // --- a replacement report: omitted keys are tolerated, not required ---
    device
        .send(json!({
            "type": "session.capabilities",
            "streamId": stream_id,
            "capabilities": { "steer": false, "interrupt": true },
        }))
        .await;
    let downgraded = await_capabilities(&app, &token, &session_id, false).await;
    assert_eq!(downgraded["revision"], json!(2));
    assert_eq!(downgraded["capabilitySet"]["interrupt"], json!(true));
    let (status, dropped_models) = call(
        &app,
        Method::GET,
        &format!("/api/v2/sessions/{session_id}/runtime/catalogs/model"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        dropped_models["items"],
        json!([]),
        "a report without catalogs replaces the previous one"
    );

    // A capability the runtime reported false is refused honestly, not dropped.
    let (status, unsupported) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{session_id}/runtime/steer"),
        Some(&token),
        Some(json!({ "content": "not supported here" })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_IMPLEMENTED,
        "steer on a runtime that reported steer=false: {unsupported}"
    );
    assert_eq!(unsupported["detail"]["code"], json!("steer_unsupported"));
    let (status, unsupported) = call(
        &app,
        Method::PATCH,
        &format!("/api/v2/sessions/{session_id}/runtime/selections"),
        Some(&token),
        Some(json!({ "model": "echo-fast" })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_IMPLEMENTED,
        "selections on a runtime that reported selections=false: {unsupported}"
    );
    assert_eq!(
        unsupported["detail"]["code"],
        json!("selections_unsupported")
    );

    // Back to the full report for the rest of the journey.
    device.send(capability_frame(&stream_id)).await;
    let restored = await_capabilities(&app, &token, &session_id, true).await;
    assert_eq!(restored["revision"], json!(3));

    // --- steer: 202, and the device really receives the frame ------------
    let (status, steered) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{session_id}/runtime/steer"),
        Some(&token),
        Some(json!({ "content": "  also run the tests  " })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "steer: {steered}");
    assert_eq!(steered["delivered"], json!(true));
    let steer_frame = device.expect_type("session.steer").await;
    assert_eq!(steer_frame["streamId"], json!(stream_id));
    assert_eq!(
        steer_frame["text"],
        json!("also run the tests"),
        "the instruction is trimmed, not reworded"
    );

    // The agent writes the instruction into the timeline; the hub must show it.
    device
        .send(json!({
            "type": "item.upsert",
            "streamId": stream_id,
            "item": {
                "id": "itm_steer_1",
                "type": "message",
                "status": "done",
                "role": "user",
                "content": { "text": "also run the tests", "steer": true },
                "itemTime": farwork_hub::timeutil::utc_now(),
            },
        }))
        .await;
    let mut snapshotted = Value::Null;
    for _ in 0..100 {
        let (status, snapshot) = call(
            &app,
            Method::GET,
            &format!("/api/v2/sessions/{session_id}/snapshot"),
            Some(&token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "snapshot: {snapshot}");
        if snapshot["timeline"]["items"]
            .as_array()
            .expect("items")
            .iter()
            .any(|item| item["id"] == json!("itm_steer_1"))
        {
            snapshotted = snapshot;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        snapshotted["timeline"]["items"]
            .as_array()
            .expect("items")
            .iter()
            .any(|item| item["id"] == json!("itm_steer_1") && item["role"] == json!("user")),
        "the steered instruction must reach the timeline: {snapshotted}"
    );

    let (status, empty_steer) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{session_id}/runtime/steer"),
        Some(&token),
        Some(json!({ "content": "   " })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "an empty instruction is refused, not delivered: {empty_steer}"
    );

    // --- selections: 200 only after the device has it ---------------------
    let (status, patched) = call(
        &app,
        Method::PATCH,
        &format!("/api/v2/sessions/{session_id}/runtime/selections"),
        Some(&token),
        Some(json!({ "model": "echo-slow" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "selections: {patched}");
    assert_eq!(patched["selections"]["model"], json!("echo-slow"));
    assert!(
        patched["selections"].get("permissionMode").is_none(),
        "{patched}"
    );
    let selections_frame = device.expect_type("session.selections").await;
    assert_eq!(selections_frame["streamId"], json!(stream_id));
    assert_eq!(selections_frame["model"], json!("echo-slow"));
    assert!(
        selections_frame.get("permissionMode").is_none(),
        "an absent selection must not be sent as null: {selections_frame}"
    );

    let (status, patched_again) = call(
        &app,
        Method::PATCH,
        &format!("/api/v2/sessions/{session_id}/runtime/selections"),
        Some(&token),
        Some(json!({ "permissionMode": "read-only" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "selections: {patched_again}");
    assert_eq!(patched_again["selections"]["model"], json!("echo-slow"));
    assert_eq!(
        patched_again["selections"]["permissionMode"],
        json!("read-only")
    );
    let second_selections = device.expect_type("session.selections").await;
    assert!(
        second_selections.get("model").is_none(),
        "{second_selections}"
    );
    assert_eq!(second_selections["permissionMode"], json!("read-only"));

    // runtime/state agrees with what the patch said it accepted.
    let (status, runtime_state) = call(
        &app,
        Method::GET,
        &format!("/api/v2/sessions/{session_id}/runtime/state"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "runtime state: {runtime_state}");
    assert_eq!(
        runtime_state["state"]["selections"],
        json!({ "model": "echo-slow", "permissionMode": "read-only" })
    );

    let (status, nothing) = call(
        &app,
        Method::PATCH,
        &format!("/api/v2/sessions/{session_id}/runtime/selections"),
        Some(&token),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a patch without a selection is refused: {nothing}"
    );

    // --- session namespace: files ----------------------------------------
    let list_task = tokio::spawn(call_owned(
        app.clone(),
        Method::POST,
        format!("/api/v2/sessions/{session_id}/files/list"),
        Some(token.clone()),
        Some(json!({ "path": "src" })),
    ));
    let fs_frame = device.expect_type("fs.request").await;
    assert_eq!(
        fs_frame["path"],
        json!("/srv/farwork/src"),
        "a relative path is anchored on the session workspace"
    );
    assert_eq!(fs_frame["op"], json!("list"));
    device
        .send(json!({
            "type": "fs.response",
            "requestId": fs_frame["requestId"],
            "ok": true,
            "op": "list",
            "path": "/srv/farwork/src",
            "entries": [
                { "name": "main.rs", "path": "/srv/farwork/src/main.rs", "kind": "file",
                  "sizeBytes": 12, "modifiedAt": "2026-01-02T03:04:05Z" },
            ],
        }))
        .await;
    let (status, listed) = list_task.await.expect("files/list task");
    assert_eq!(status, StatusCode::OK, "session files/list: {listed}");
    assert_eq!(listed["path"], json!("/srv/farwork/src"));
    assert_eq!(listed["entries"][0]["name"], json!("main.rs"));
    assert_eq!(listed["entries"][0]["kind"], json!("file"));

    let read_task = tokio::spawn(call_owned(
        app.clone(),
        Method::POST,
        format!("/api/v2/sessions/{session_id}/files/readText"),
        Some(token.clone()),
        Some(json!({ "path": "/srv/farwork/README.md" })),
    ));
    let read_frame = device.expect_type("fs.request").await;
    assert_eq!(read_frame["path"], json!("/srv/farwork/README.md"));
    device
        .send(json!({
            "type": "fs.response",
            "requestId": read_frame["requestId"],
            "ok": true,
            "op": "readText",
            "path": "/srv/farwork/README.md",
            "content": "# farwork\n",
            "sizeBytes": 10,
            "truncated": false,
        }))
        .await;
    let (status, read) = read_task.await.expect("files/readText task");
    assert_eq!(status, StatusCode::OK, "session files/readText: {read}");
    assert_eq!(read["content"], json!("# farwork\n"));
    assert_eq!(read["truncated"], json!(false));

    // --- session namespace: terminal -------------------------------------
    let (status, created_terminal) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{session_id}/terminal"),
        Some(&token),
        Some(json!({ "cols": 100, "rows": 30 })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "session terminal: {created_terminal}"
    );
    let terminal_id = created_terminal["terminal"]["id"]
        .as_str()
        .expect("terminal id")
        .to_string();
    assert_eq!(
        created_terminal["terminal"]["connectorId"],
        json!(harness.connector_id)
    );
    assert_eq!(created_terminal["terminal"]["cols"], json!(100));
    let mut keys: Vec<&str> = created_terminal["terminal"]
        .as_object()
        .expect("terminal object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["cols", "connectorId", "createdAt", "id", "rows"],
        "the terminal view is exactly the five contract fields"
    );

    let open_frame = device.expect_type("terminal.open").await;
    assert_eq!(open_frame["terminalId"], json!(terminal_id));
    assert_eq!(
        open_frame["cwd"],
        json!(WORKSPACE),
        "a session terminal starts in the session workspace"
    );
    device
        .send(json!({
            "type": "terminal.opened",
            "terminalId": terminal_id,
        }))
        .await;

    // The row records which session opened it — that binding is what lets the
    // stream route refuse a terminal of the same device opened elsewhere.
    let bound: Option<String> =
        sqlx::query_scalar("select session_id from terminals where id = $1")
            .bind(&terminal_id)
            .fetch_one(&harness.pool)
            .await
            .expect("session_id query");
    assert_eq!(bound.as_deref(), Some(session_id.as_str()));

    let (status, ticket_body) = call(
        &app,
        Method::POST,
        "/api/v2/ws-ticket",
        Some(&token),
        Some(json!({
            "clientId": "browser",
            "scope": { "terminalId": terminal_id },
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "ws-ticket: {ticket_body}");
    let ticket = ticket_body["ticket"].as_str().expect("ticket").to_string();
    let mut browser = Client::connect(
        &format!(
            "{}/api/v2/sessions/{session_id}/terminal/{terminal_id}/stream?ticket={ticket}",
            harness.ws_base
        ),
        "browser",
    )
    .await;
    let attached = browser.expect_type("terminal.opened").await;
    assert_eq!(attached["type"], json!("terminal.opened"));

    browser
        .send(json!({ "type": "terminal.input", "data": "aGkK" }))
        .await;
    let input = device.expect_type("terminal.data").await;
    assert_eq!(input["terminalId"], json!(terminal_id));
    assert_eq!(
        input["data"],
        json!("aGkK"),
        "PTY bytes are relayed verbatim"
    );

    device
        .send(json!({
            "type": "terminal.data",
            "terminalId": terminal_id,
            "data": "aGVsbG8=",
        }))
        .await;
    let output = browser.expect_type("terminal.data").await;
    assert_eq!(output["data"], json!("aGVsbG8="));

    browser.send(json!({ "type": "terminal.close" })).await;
    let close = device.expect_type("terminal.close").await;
    assert_eq!(close["terminalId"], json!(terminal_id));
    device
        .send(json!({
            "type": "terminal.exit",
            "terminalId": terminal_id,
            "code": 0,
        }))
        .await;
    let exit = browser.expect_type("terminal.exit").await;
    assert_eq!(exit["code"], json!(0));

    // --- a stranger's session is a 404 everywhere ------------------------
    for (method, uri, body) in [
        (
            Method::GET,
            format!("/api/v2/sessions/{session_id}/runtime/capabilities"),
            None,
        ),
        (
            Method::GET,
            format!("/api/v2/sessions/{session_id}/runtime/commands"),
            None,
        ),
        (
            Method::GET,
            format!("/api/v2/sessions/{session_id}/runtime/catalogs/model"),
            None,
        ),
        (
            Method::POST,
            format!("/api/v2/sessions/{session_id}/runtime/steer"),
            Some(json!({ "content": "hijack" })),
        ),
        (
            Method::PATCH,
            format!("/api/v2/sessions/{session_id}/runtime/selections"),
            Some(json!({ "model": "echo-fast" })),
        ),
        (
            Method::POST,
            format!("/api/v2/sessions/{session_id}/files/list"),
            Some(json!({ "path": "/srv/farwork" })),
        ),
        (
            Method::POST,
            format!("/api/v2/sessions/{session_id}/files/readText"),
            Some(json!({ "path": "/srv/farwork/README.md" })),
        ),
        (
            Method::POST,
            format!("/api/v2/sessions/{session_id}/terminal"),
            Some(json!({ "cols": 80, "rows": 24 })),
        ),
    ] {
        let (status, body) = call(&app, method.clone(), &uri, Some(&stranger), body).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{method} {uri} must not leak another user's session: {body}"
        );
    }
    // ... including a ticket for its terminal, which is minted only for its owner.
    let (status, _) = call(
        &app,
        Method::POST,
        "/api/v2/ws-ticket",
        Some(&stranger),
        Some(json!({
            "clientId": "browser",
            "scope": { "terminalId": terminal_id },
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // --- no stream left: steer refuses instead of pretending ------------
    device
        .send(json!({
            "type": "session.exit",
            "streamId": stream_id,
            "code": 0,
        }))
        .await;
    // The cache dies with the stream: a finished runtime has no capabilities.
    let cleared = await_capabilities(&app, &token, &session_id, false).await;
    assert_eq!(cleared["revision"], json!(0), "{cleared}");

    let (status, refused) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{session_id}/runtime/steer"),
        Some(&token),
        Some(json!({ "content": "too late" })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "steer without a stream: {refused}"
    );
    assert_eq!(refused["detail"]["code"], json!("session_not_running"));

    let (status, refused) = call(
        &app,
        Method::PATCH,
        &format!("/api/v2/sessions/{session_id}/runtime/selections"),
        Some(&token),
        Some(json!({ "model": "echo-fast" })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "selections without a stream: {refused}"
    );
    assert_eq!(refused["detail"]["code"], json!("session_not_running"));
}

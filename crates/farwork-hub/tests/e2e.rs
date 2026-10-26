//! End-to-end walk of the P0 hub against a real Postgres database.
//!
//! Runs only when `TEST_DATABASE_URL` points at a disposable database
//! (for example `postgres://tianjun@127.0.0.1:5432/farwork_test`). Without it the
//! test skips loudly rather than passing quietly.

use axum::body::to_bytes;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use farwork_hub::config::Config;
use farwork_hub::{db, AppState};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt as _;

const SETUP_TOKEN: &str = "setup-integration-token";
const SECRET: &str = "integration-secret";
const PASSWORD: &str = "correct horse battery staple";

struct Harness {
    app: Router,
    token: String,
    email: String,
    salt: String,
    verifier: String,
}

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

async fn bootstrap(database_url: &str) -> Option<Harness> {
    let config = Config {
        database_url: database_url.to_string(),
        port: 0,
        secret: SECRET.to_string(),
        setup_token: Some(SETUP_TOKEN.to_string()),
        database_max_connections: 5,
    };
    let pool = db::connect(&config).await.expect("connect");
    db::migrate(&pool).await.expect("migrate");
    // Disposable database: start from a clean slate every run.
    for table in [
        "timeline_items",
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

    let email = format!("ada+{}@example.com", farwork_hub::timeutil::unix_now());
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
    assert!(
        !salt.contains('='),
        "salt must be base64url without padding"
    );
    assert!(!salt_body["serverTime"]
        .as_str()
        .unwrap_or_default()
        .is_empty());

    // Same derivation the browser performs.
    let verifier = farwork_hub::auth::derive_password_verifier(PASSWORD, &salt);
    assert_eq!(verifier.len(), 43, "32 bytes base64url encode to 43 chars");

    Some(Harness {
        app,
        token: String::new(),
        email,
        salt,
        verifier,
    })
}

#[tokio::test]
async fn p0_journey_login_to_websocket_delta() {
    let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!(
            "SKIP farwork-hub integration test: TEST_DATABASE_URL is not set.\n\
             Point it at a disposable database to run this test, e.g.\n  \
             export TEST_DATABASE_URL=postgres://tianjun@127.0.0.1:5432/farwork_test"
        );
        return;
    };
    if database_url.trim().is_empty() {
        eprintln!("SKIP farwork-hub integration test: TEST_DATABASE_URL is empty.");
        return;
    }
    let Some(mut harness) = bootstrap(&database_url).await else {
        return;
    };
    let app = harness.app.clone();

    // --- health + bootstrap config -------------------------------------
    let (status, health) = call(&app, Method::GET, "/healthz", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(health["status"], "ok");
    assert_eq!(health["version"], farwork_hub::VERSION);

    let (status, config) = call(&app, Method::GET, "/api/v2/auth/config", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(config["needsBootstrap"], true);
    assert!(config["setupTokenExpiresAt"].as_str().is_some());

    // --- register (bootstrap admin) ------------------------------------
    let (status, wrong_token) = call(
        &app,
        Method::POST,
        "/api/v2/auth/register",
        None,
        Some(json!({
            "email": harness.email,
            "passwordVerifier": harness.verifier,
            "passwordSalt": harness.salt,
            "setupToken": "nope",
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "bad setup token: {wrong_token}"
    );

    let (status, registered) = call(
        &app,
        Method::POST,
        "/api/v2/auth/register",
        None,
        Some(json!({
            "email": harness.email,
            "displayName": "Ada",
            "passwordVerifier": harness.verifier,
            "passwordSalt": harness.salt,
            "setupToken": SETUP_TOKEN,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "register: {registered}");
    assert_eq!(registered["role"], "admin");
    assert_eq!(registered["tokenType"], "bearer");
    let token = registered["accessToken"]
        .as_str()
        .expect("token")
        .to_string();
    assert_eq!(token.split('.').count(), 3, "HS256 JWT has three segments");
    harness.token = token;

    // registration is closed after bootstrap, and the token is consumed
    let (status, closed) = call(
        &app,
        Method::POST,
        "/api/v2/auth/register",
        None,
        Some(json!({
            "email": format!("second{}@example.com", farwork_hub::timeutil::unix_now()),
            "passwordVerifier": harness.verifier,
            "passwordSalt": harness.salt,
            "setupToken": SETUP_TOKEN,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "closed registration: {closed}"
    );

    // --- login paths ----------------------------------------------------
    let (status, login) = call(
        &app,
        Method::POST,
        "/api/v2/auth/login",
        None,
        Some(json!({ "email": harness.email, "passwordVerifier": harness.verifier })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "login: {login}");
    assert_eq!(login["userId"], registered["userId"]);
    let token = login["accessToken"].as_str().expect("token").to_string();

    let (status, wrong) = call(
        &app,
        Method::POST,
        "/api/v2/auth/login",
        None,
        Some(json!({ "email": harness.email, "passwordVerifier": "wrong" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(wrong["detail"], "invalid credentials");

    let (status, _unknown) = call(
        &app,
        Method::POST,
        "/api/v2/auth/login",
        None,
        Some(json!({ "email": "nobody@example.com", "passwordVerifier": "whatever" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // --- bearer validation ---------------------------------------------
    let (status, _) = call(&app, Method::GET, "/api/v2/auth/me", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "missing bearer");
    let (status, _) = call(
        &app,
        Method::GET,
        "/api/v2/auth/me",
        Some("not-a-jwt"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "malformed bearer");
    let (status, me) = call(&app, Method::GET, "/api/v2/auth/me", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["role"], "admin");

    // --- connector ------------------------------------------------------
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
        .expect("id")
        .to_string();
    assert!(connector["connectorToken"]
        .as_str()
        .expect("token")
        .starts_with("cxt_"));
    assert!(connector["tokenPrefix"]
        .as_str()
        .expect("prefix")
        .starts_with("cxt_"));

    let (status, list) = call(&app, Method::GET, "/api/v2/connectors", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["connectors"].as_array().expect("array").len(), 1);
    assert!(list["serverTime"].as_str().is_some());

    // --- project --------------------------------------------------------
    let (status, project) = call(
        &app,
        Method::POST,
        "/api/v2/projects",
        Some(&token),
        Some(json!({
            "name": "farwork",
            "connectorId": connector_id,
            "workspacePath": "/srv/farwork",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "project: {project}");
    let project_id = project["project"]["id"].as_str().expect("id").to_string();

    let (status, conflict) = call(
        &app,
        Method::POST,
        "/api/v2/projects",
        Some(&token),
        Some(json!({
            "name": "farwork",
            "connectorId": connector_id,
            "workspacePath": "/srv/other",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(conflict["detail"]["code"], "project_name_conflict");

    let (status, projects) = call(&app, Method::GET, "/api/v2/projects", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(projects["projects"].as_array().expect("array").len(), 1);

    // --- session creation ----------------------------------------------
    let (status, invalid) = call(
        &app,
        Method::POST,
        "/api/v2/sessions",
        Some(&token),
        Some(json!({ "connectorId": connector_id, "projectId": project_id })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "import needs externalSessionId"
    );
    assert_eq!(
        invalid["detail"],
        "new sessions must use /sessions/create-and-start"
    );

    let (status, started) = call(
        &app,
        Method::POST,
        "/api/v2/sessions/create-and-start",
        Some(&token),
        Some(json!({
            "connectorId": connector_id,
            "projectId": project_id,
            "runtime": "codex",
            "title": "first session",
            "content": "hello from the integration test",
            "clientMessageId": "cm_it_1",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create-and-start: {started}");
    let session_id = started["session"]["id"].as_str().expect("id").to_string();
    assert_eq!(started["session"]["takeover"], true);
    assert_eq!(started["session"]["status"], "idle");
    assert_eq!(started["session"]["updatedSeq"], 1);
    assert_eq!(started["eventCursor"], "seq:1");
    assert_eq!(started["connectorResult"]["code"], "connector_offline");
    assert_eq!(started["runtimeDispatched"], false);

    let (status, inventory) = call(
        &app,
        Method::GET,
        "/api/v2/sessions/list",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "inventory: {inventory}");
    assert_eq!(inventory["sessions"].as_array().expect("array").len(), 1);

    // --- message write + idempotency -----------------------------------
    let (status, message) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{session_id}/runtime/messages"),
        Some(&token),
        Some(json!({ "content": "second message", "clientMessageId": "cm_it_2" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "message: {message}");
    assert_eq!(message["ok"], true);
    assert_eq!(message["result"]["eventCursor"], "seq:2");
    assert_eq!(message["result"]["duplicate"], false);
    assert_eq!(message["result"]["item"]["role"], "user");
    assert_eq!(message["result"]["item"]["type"], "message");
    assert_eq!(
        message["result"]["item"]["content"]["text"],
        "second message"
    );
    // The web client replaces its optimistic item by this key.
    assert_eq!(
        message["result"]["item"]["source"]["clientMessageId"],
        "cm_it_2"
    );
    let item_id = message["result"]["item"]["id"]
        .as_str()
        .expect("item id")
        .to_string();

    let (status, replay) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{session_id}/runtime/messages"),
        Some(&token),
        Some(json!({ "content": "second message", "clientMessageId": "cm_it_2" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay["result"]["duplicate"], true);
    assert_eq!(replay["result"]["item"]["id"], item_id);
    assert_eq!(replay["result"]["eventCursor"], "seq:2", "no new sequence");

    // --- snapshot -------------------------------------------------------
    let (status, snapshot) = call(
        &app,
        Method::GET,
        &format!("/api/v2/sessions/{session_id}/snapshot"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "snapshot: {snapshot}");
    for field in [
        "session",
        "state",
        "timeline",
        "approvals",
        "notices",
        "effectiveCapabilities",
        "runtimeCapabilities",
        "catalogs",
        "eventCursor",
        "serverTime",
    ] {
        assert!(
            snapshot.get(field).is_some(),
            "snapshot is missing required field {field}"
        );
    }
    assert_eq!(snapshot["eventCursor"], "seq:2");
    assert_eq!(snapshot["timeline"]["nextSeq"], 2);
    assert_eq!(
        snapshot["timeline"]["items"]
            .as_array()
            .expect("items")
            .len(),
        2
    );
    assert_eq!(
        snapshot["approvals"].as_array().expect("approvals").len(),
        0
    );
    assert_eq!(snapshot["state"]["status"], "idle");

    // --- cursor recovery ------------------------------------------------
    let (status, recovered) = call(
        &app,
        Method::GET,
        &format!("/api/v2/sessions/{session_id}/events?after=seq:0"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "events: {recovered}");
    assert_eq!(recovered["snapshotRequired"], false);
    assert_eq!(recovered["nextCursor"], "seq:2");
    let events = recovered["events"].as_array().expect("events");
    assert_eq!(events.len(), 2);
    for event in events {
        for field in [
            "protocolVersion",
            "eventId",
            "sequence",
            "cursor",
            "type",
            "sessionId",
            "emittedAt",
            "payload",
        ] {
            assert!(event.get(field).is_some(), "event missing {field}");
        }
        assert_eq!(event["protocolVersion"], "1.0");
        assert_eq!(event["type"], "timeline.item_created");
        assert_eq!(event["sessionId"], session_id);
    }

    let (status, partial) = call(
        &app,
        Method::GET,
        &format!("/api/v2/sessions/{session_id}/events?after=seq:1"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(partial["events"].as_array().expect("events").len(), 1);
    assert_eq!(partial["nextCursor"], "seq:2");

    let (status, ahead) = call(
        &app,
        Method::GET,
        &format!("/api/v2/sessions/{session_id}/events?after=seq:99"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "ahead: {ahead}");
    assert_eq!(
        ahead["snapshotRequired"], true,
        "an ahead cursor must refetch"
    );
    assert_eq!(ahead["nextCursor"], "seq:2");

    let (status, garbage) = call(
        &app,
        Method::GET,
        &format!("/api/v2/sessions/{session_id}/events?after=nope"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(garbage["snapshotRequired"], true);

    // --- runtime state / capabilities ------------------------------------
    let (status, state) = call(
        &app,
        Method::GET,
        &format!("/api/v2/sessions/{session_id}/runtime/state"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(state["state"]["updatedSeq"], 2);

    let (status, capabilities) = call(
        &app,
        Method::GET,
        &format!("/api/v2/sessions/{session_id}/runtime/capabilities"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(capabilities["connectorId"], connector_id);
    assert_eq!(capabilities["revision"], json!(0));
    // No connector is attached in this journey, so the agent has reported nothing
    // and the truthful capability set is all-false.
    assert_eq!(
        capabilities["capabilitySet"],
        json!({
            "steer": false,
            "interrupt": false,
            "selections": false,
            "commands": false,
        })
    );

    // --- unimplemented surfaces ------------------------------------------
    let (status, sync) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{session_id}/sync"),
        Some(&token),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(sync["error"], "not_implemented");

    // --- websockets -------------------------------------------------------
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let server_app = app.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, server_app).await;
    });

    let (status, ticket_body) = call(
        &app,
        Method::POST,
        "/api/v2/ws-ticket",
        Some(&token),
        Some(json!({ "clientId": "integration-client",
                     "scope": { "sessionId": session_id } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "ws-ticket: {ticket_body}");
    let ticket = ticket_body["ticket"].as_str().expect("ticket").to_string();
    assert!(ticket.starts_with("wst_"));
    assert!(ticket_body["expiresAt"].as_str().is_some());

    let url = format!("ws://{addr}/api/v2/sessions/{session_id}/ws?ticket={ticket}");
    let (mut socket, _) = timeout(
        std::time::Duration::from_secs(5),
        tokio_tungstenite::connect_async(url),
    )
    .await
    .expect("ws connect timeout")
    .expect("ws connect");

    let first = timeout(std::time::Duration::from_secs(5), socket.next())
        .await
        .expect("first frame timeout")
        .expect("frame")
        .expect("ok frame");
    let subscribed: Value = serde_json::from_str(first.to_text().expect("text")).expect("json");
    assert_eq!(subscribed["type"], "session.subscribed");
    assert_eq!(subscribed["protocolVersion"], "1.0");
    assert_eq!(subscribed["cursor"], "seq:2");
    assert_eq!(subscribed["payload"]["eventCursor"], "seq:2");
    assert_eq!(subscribed["payload"]["clientId"], "integration-client");

    // A live write must reach the socket as a delta.
    let (status, _) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{session_id}/runtime/messages"),
        Some(&token),
        Some(json!({ "content": "delta over ws" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let delta_frame = timeout(std::time::Duration::from_secs(5), socket.next())
        .await
        .expect("delta timeout")
        .expect("frame")
        .expect("ok frame");
    let delta: Value = serde_json::from_str(delta_frame.to_text().expect("text")).expect("json");
    assert_eq!(delta["type"], "timeline.item_created");
    assert_eq!(delta["sequence"], 3);
    assert_eq!(delta["payload"]["item"]["content"]["text"], "delta over ws");
    let _ = socket.send(Message::Close(None)).await;

    // Tickets are single use.
    let (status, reused) = call(
        &app,
        Method::POST,
        "/api/v2/ws-ticket",
        Some(&token),
        Some(json!({ "clientId": "integration-client",
                     "scope": { "sessionId": session_id } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let reused_ticket = reused["ticket"].as_str().expect("ticket");
    let first_connect = tokio_tungstenite::connect_async(format!(
        "ws://{addr}/api/v2/sessions/{session_id}/ws?ticket={reused_ticket}"
    ))
    .await;
    assert!(first_connect.is_ok());
    let second_connect = tokio_tungstenite::connect_async(format!(
        "ws://{addr}/api/v2/sessions/{session_id}/ws?ticket={reused_ticket}"
    ))
    .await;
    assert!(second_connect.is_err(), "a ticket must not be reusable");

    // Dashboard socket answers with a snapshot envelope.
    let (status, dashboard_ticket) = call(
        &app,
        Method::POST,
        "/api/v2/ws-ticket",
        Some(&token),
        Some(json!({ "clientId": "integration-client", "scope": { "dashboard": true } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let dashboard_ticket = dashboard_ticket["ticket"].as_str().expect("ticket");
    let (mut dashboard, _) = tokio_tungstenite::connect_async(format!(
        "ws://{addr}/api/v2/dashboard/ws?ticket={dashboard_ticket}"
    ))
    .await
    .expect("dashboard connect");
    let frame = timeout(std::time::Duration::from_secs(5), dashboard.next())
        .await
        .expect("snapshot timeout")
        .expect("frame")
        .expect("ok frame");
    let snapshot: Value = serde_json::from_str(frame.to_text().expect("text")).expect("json");
    assert_eq!(snapshot["type"], "dashboard.snapshot");
    assert_eq!(
        snapshot["connectors"].as_array().expect("connectors").len(),
        1
    );
    assert_eq!(snapshot["projects"].as_array().expect("projects").len(), 1);
    assert_eq!(snapshot["sessions"].as_array().expect("sessions").len(), 1);
}

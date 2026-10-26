//! Session/message sharing against a real PostgreSQL database.
//!
//! The point of these assertions is the feature's core semantic plus its blast
//! radius: a published share is an immutable *copy*, and the endpoint that serves
//! it takes no credentials and leaks nothing beyond the whitelisted snapshot.
//!
//! Runs only when `TEST_DATABASE_URL` points at a disposable database; without it
//! the test skips loudly rather than passing quietly.

use axum::body::to_bytes;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt as _;

use farwork_hub::config::Config;
use farwork_hub::{db, AppState};

const SETUP_TOKEN: &str = "setup-shares-token";
const SECRET: &str = "shares-integration-secret";
const PASSWORD: &str = "correct horse battery staple";
const WORKSPACE: &str = "/srv/farwork-shares";
const TITLE: &str = "Share me";

async fn call_raw(
    app: &Router,
    method: Method,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, String) {
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
    (status, String::from_utf8_lossy(&bytes).to_string())
}

async fn call(
    app: &Router,
    method: Method,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (status, raw) = call_raw(app, method, uri, token, body).await;
    let value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&raw).unwrap_or(Value::String(raw))
    };
    (status, value)
}

struct Harness {
    app: Router,
    /// The session owner.
    token: String,
    /// A second, unrelated account: it must reach nothing of the owner's.
    other_token: String,
    owner_email: String,
    session_id: String,
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
    let pool: PgPool = db::connect(&config).await.expect("connect");
    db::migrate(&pool).await.expect("migrate");
    for table in [
        "shares",
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

    let app = farwork_hub::app(AppState::new(config, pool.clone()));

    let owner_email = format!("shares+{}@example.com", farwork_hub::timeutil::unix_now());
    let (status, salt_body) = call(
        &app,
        Method::POST,
        "/api/v2/auth/password-salt",
        None,
        Some(json!({ "email": owner_email })),
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
            "email": owner_email,
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

    // Second account, straight into the table: registration is closed after
    // bootstrap, and this test only needs a *different* valid token.
    let other_user_id = farwork_hub::ids::user_id();
    let now = farwork_hub::timeutil::utc_now();
    sqlx::query(
        "insert into users (id, email, display_name, password_hash, created_at, updated_at)
         values ($1, $2, 'Mallory', 'pbkdf2_sha256$salt$verifier', $3, $3)",
    )
    .bind(&other_user_id)
    .bind(format!(
        "mallory+{}@example.com",
        farwork_hub::timeutil::unix_now()
    ))
    .bind(&now)
    .execute(&pool)
    .await
    .expect("other user");
    let other_token = farwork_hub::auth::create_user_access_token(SECRET, &other_user_id);

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

    let (status, project) = call(
        &app,
        Method::POST,
        "/api/v2/projects",
        Some(&token),
        Some(json!({
            "name": "farwork-shares",
            "connectorId": connector_id,
            "workspacePath": WORKSPACE,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "project: {project}");
    let project_id = project["project"]["id"].as_str().expect("id").to_string();

    // create-and-start is the writable path (takeover=true) and lands the first
    // user message in the timeline.
    let (status, created) = call(
        &app,
        Method::POST,
        "/api/v2/sessions/create-and-start",
        Some(&token),
        Some(json!({
            "connectorId": connector_id,
            "projectId": project_id,
            "runtime": "codex",
            "title": TITLE,
            "content": "first message",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create-and-start: {created}");
    let session_id = created["session"]["id"]
        .as_str()
        .expect("session id")
        .to_string();
    assert_eq!(created["session"]["takeover"], true);

    Harness {
        app,
        token,
        other_token,
        owner_email,
        session_id,
    }
}

async fn timeline_ids(app: &Router, token: &str, session_id: &str) -> Vec<String> {
    let (status, timeline) = call(
        app,
        Method::GET,
        &format!("/api/v2/sessions/{session_id}/timeline?mode=latest&limit=100"),
        Some(token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "timeline: {timeline}");
    timeline["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|item| item["id"].as_str().expect("item id").to_string())
        .collect()
}

/// Every string a public share must never contain. Checked against the raw body,
/// not the parsed JSON, so a nested or re-nested field cannot slip through.
fn assert_no_sensitive_fields(raw: &str, h: &Harness) {
    let forbidden = [
        "connectorId",
        "connector",
        "workspacePath",
        WORKSPACE,
        "cxt_",
        "token",
        h.owner_email.as_str(),
        "@example.com",
        "sessionId",
        "\"source\"",
        "contentHash",
        "updatedSeq",
        "revision",
        "allowedFileIds",
        "allowed_file_ids",
        h.token.as_str(),
        SECRET,
    ];
    for needle in forbidden {
        assert!(
            !raw.contains(needle),
            "public share leaked {needle:?}: {raw}"
        );
    }
}

#[tokio::test]
async fn share_journey_publishes_an_immutable_snapshot() {
    let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!(
            "SKIP farwork-hub shares test: TEST_DATABASE_URL is not set.\n\
             Point it at a disposable database to run this test, e.g.\n  \
             export TEST_DATABASE_URL=postgres://tianjun@127.0.0.1:5432/farwork_test"
        );
        return;
    };
    if database_url.trim().is_empty() {
        eprintln!("SKIP farwork-hub shares test: TEST_DATABASE_URL is empty.");
        return;
    }
    let harness = bootstrap(&database_url).await;
    let app = harness.app.clone();
    let session = harness.session_id.clone();
    let owner = harness.token.clone();

    // --- create: whole-session share -----------------------------------
    let (status, created) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{session}/shares"),
        Some(&owner),
        Some(json!({ "scope": "session", "allowedFileIds": [] })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create share: {created}");
    let share_id = created["share"]["id"]
        .as_str()
        .expect("share id")
        .to_string();
    assert!(share_id.starts_with("shr_"), "share id shape: {share_id}");
    assert_eq!(created["share"]["scope"], "session");
    assert_eq!(
        created["share"]["urlPath"],
        format!("/api/v2/public/shares/{share_id}")
    );
    assert!(!created["share"]["createdAt"]
        .as_str()
        .unwrap_or_default()
        .is_empty());
    assert!(!created["serverTime"]
        .as_str()
        .unwrap_or_default()
        .is_empty());

    // An unknown scope must not silently fall back to "session".
    let (status, unknown_scope) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{session}/shares"),
        Some(&owner),
        Some(json!({ "scope": "everything" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{unknown_scope}");

    // --- public read: no Authorization header at all --------------------
    let public_uri = format!("/api/v2/public/shares/{share_id}");
    let (status, raw) = call_raw(&app, Method::GET, &public_uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "anonymous public read: {raw}");
    let anonymous: Value = serde_json::from_str(&raw).expect("json");
    assert_eq!(anonymous["share"]["id"], share_id);
    assert_eq!(anonymous["share"]["scope"], "session");
    assert_eq!(anonymous["share"]["sessionTitle"], TITLE);
    let shared_items = anonymous["share"]["timeline"]["items"]
        .as_array()
        .expect("items")
        .clone();
    let ids_before = timeline_ids(&app, &owner, &session).await;
    assert_eq!(shared_items.len(), ids_before.len());
    for item in &shared_items {
        let mut keys: Vec<&str> = item
            .as_object()
            .expect("item object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["content", "id", "itemTime", "orderSeq", "role", "status", "type"],
            "a public item must carry exactly the whitelisted fields: {item}"
        );
        assert!(item["orderSeq"].as_i64().unwrap_or(0) >= 1);
        assert!(!item["content"].as_object().expect("content").is_empty());
    }
    assert_no_sensitive_fields(&raw, &harness);

    // --- the snapshot is immutable: change the session, re-read ----------
    let (status, sent) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{session}/runtime/messages"),
        Some(&owner),
        Some(json!({ "content": "second message after the share was published" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "send message: {sent}");
    let ids_after = timeline_ids(&app, &owner, &session).await;
    assert_eq!(
        ids_after.len(),
        ids_before.len() + 1,
        "the live session must have grown"
    );

    let (status, raw_after) = call_raw(&app, Method::GET, &public_uri, None, None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "public read after the edit: {raw_after}"
    );
    let later: Value = serde_json::from_str(&raw_after).expect("json");
    assert_eq!(
        later["share"], anonymous["share"],
        "a published share must not move when the session does"
    );
    assert_eq!(
        later["share"]["timeline"]["items"]
            .as_array()
            .expect("items")
            .len(),
        ids_before.len(),
        "the post-publication message must not appear in the share"
    );
    assert_no_sensitive_fields(&raw_after, &harness);

    // --- message scope: exactly one item --------------------------------
    let first_item_id = ids_before[0].clone();
    let (status, message_share) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{session}/shares"),
        Some(&owner),
        Some(json!({ "scope": "message", "messageId": first_item_id })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "message share: {message_share}"
    );
    let message_share_id = message_share["share"]["id"]
        .as_str()
        .expect("share id")
        .to_string();
    assert_eq!(message_share["share"]["scope"], "message");
    let (status, raw_message) = call_raw(
        &app,
        Method::GET,
        &format!("/api/v2/public/shares/{message_share_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "message share read: {raw_message}");
    let message_share_body: Value = serde_json::from_str(&raw_message).expect("json");
    let one = message_share_body["share"]["timeline"]["items"]
        .as_array()
        .expect("items");
    assert_eq!(one.len(), 1, "message scope must share exactly one item");
    assert_eq!(one[0]["id"], first_item_id);
    assert_no_sensitive_fields(&raw_message, &harness);

    // A message id that is not in this session is a 404, like an unknown one.
    let (status, missing) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{session}/shares"),
        Some(&owner),
        Some(json!({ "scope": "message", "messageId": "itm_does_not_exist" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown message: {missing}");

    let (status, no_message_id) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{session}/shares"),
        Some(&owner),
        Some(json!({ "scope": "message" })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "message scope without messageId: {no_message_id}"
    );

    // --- another account: nothing of the owner's, except the public link --
    let other = harness.other_token.clone();
    let (status, denied_create) = call(
        &app,
        Method::POST,
        &format!("/api/v2/sessions/{session}/shares"),
        Some(&other),
        Some(json!({ "scope": "session" })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a stranger must not be able to share someone else's session: {denied_create}"
    );

    let (status, denied_timeline) = call(
        &app,
        Method::GET,
        &format!("/api/v2/sessions/{session}/timeline"),
        Some(&other),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the session itself stays private: {denied_timeline}"
    );

    // ...while the public link still works for them, and for nobody-with-no-token.
    let (status, other_read) = call(&app, Method::GET, &public_uri, Some(&other), None).await;
    assert_eq!(status, StatusCode::OK, "public share read: {other_read}");
    assert_eq!(other_read["share"], anonymous["share"]);

    // The public route takes no extractor at all: a junk token is not "more
    // privileged", it is simply ignored, while the same junk token still cannot
    // open any authenticated route.
    let (status, junk_read) = call(
        &app,
        Method::GET,
        &public_uri,
        Some("not-a-real-token"),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the public read must not depend on the Authorization header: {junk_read}"
    );
    assert_eq!(junk_read["share"], anonymous["share"]);
    let (status, junk_private) = call(
        &app,
        Method::GET,
        &format!("/api/v2/sessions/{session}/snapshot"),
        Some("not-a-real-token"),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "every other /api/v2 route still requires a valid token: {junk_private}"
    );

    // --- unknown share id ------------------------------------------------
    let (status, unknown) = call(
        &app,
        Method::GET,
        "/api/v2/public/shares/shr_never_existed",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown share: {unknown}");

    // --- delete: owner only, then the link is gone -----------------------
    let (status, denied_delete) = call(
        &app,
        Method::DELETE,
        &format!("/api/v2/sessions/{session}/shares/{message_share_id}"),
        Some(&other),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a stranger must not be able to revoke a share: {denied_delete}"
    );
    let (status, still_there) = call(
        &app,
        Method::GET,
        &format!("/api/v2/public/shares/{message_share_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "share survived the refused delete: {still_there}"
    );

    let (status, body) = call(
        &app,
        Method::DELETE,
        &format!("/api/v2/sessions/{session}/shares/{message_share_id}"),
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "owner delete: {body}");
    assert!(body.is_null(), "204 must not carry a body: {body}");

    let (status, gone) = call(
        &app,
        Method::GET,
        &format!("/api/v2/public/shares/{message_share_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "deleted share: {gone}");

    let (status, gone_again) = call(
        &app,
        Method::DELETE,
        &format!("/api/v2/sessions/{session}/shares/{message_share_id}"),
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "second delete: {gone_again}");

    // The session share is a different row and must still be readable.
    let (status, survivor) = call(&app, Method::GET, &public_uri, None, None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the other share survived: {survivor}"
    );
    assert_eq!(survivor["share"], anonymous["share"]);
}

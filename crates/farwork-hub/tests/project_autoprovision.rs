//! A session that names a workspace but no project must get that workspace's
//! project — found or created — so the dashboard never shows it as an orphan
//! (upstream `findWorkspaceProject`). Runs only with `TEST_DATABASE_URL` on a
//! disposable database, like the other hub integration tests.

use axum::body::to_bytes;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use farwork_hub::config::Config;
use farwork_hub::{db, AppState};
use serde_json::{json, Value};
use tower::ServiceExt as _;

const SETUP_TOKEN: &str = "setup-autoproject-token";
const SECRET: &str = "autoproject-secret";
const PASSWORD: &str = "correct horse battery staple";

struct Harness {
    app: Router,
    token: String,
    connector_id: String,
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
    Harness {
        app,
        token,
        connector_id,
    }
}

async fn projects(app: &Router, token: &str) -> Vec<Value> {
    let (status, body) = call(app, Method::GET, "/api/v2/projects", Some(token), None).await;
    assert_eq!(status, StatusCode::OK, "projects: {body}");
    body["projects"].as_array().cloned().unwrap_or_default()
}

async fn start_session(
    harness: &Harness,
    cwd: Option<&str>,
    project_id: Option<&str>,
    title: &str,
) -> Value {
    let mut body = json!({
        "connectorId": harness.connector_id,
        "runtime": "codex",
        "title": title,
        "content": "hello from the workspace test",
    });
    if let Some(cwd) = cwd {
        body["cwd"] = Value::String(cwd.to_string());
    }
    if let Some(project_id) = project_id {
        body["projectId"] = Value::String(project_id.to_string());
    }
    let (status, started) = call(
        &harness.app,
        Method::POST,
        "/api/v2/sessions/create-and-start",
        Some(&harness.token),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create-and-start: {started}");
    started
}

fn session_project(started: &Value) -> Option<String> {
    started["session"]["projectId"].as_str().map(str::to_string)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_cwd_binds_to_the_project_that_owns_the_workspace() {
    let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!(
            "SKIP farwork-hub integration test: TEST_DATABASE_URL is not set.
             Point it at a disposable database to run this test, e.g.
               export TEST_DATABASE_URL=postgres://tianjun@127.0.0.1:5432/farwork_test_projects"
        );
        return;
    };
    if database_url.trim().is_empty() {
        eprintln!("SKIP farwork-hub integration test: TEST_DATABASE_URL is empty.");
        return;
    }
    let harness = bootstrap(&database_url).await;

    // --- no projectId + absolute cwd creates and binds the project ------
    let started = start_session(&harness, Some("/tmp/fw-auto/Alpha"), None, "first").await;
    let alpha_id = session_project(&started).expect("the session must own a project");
    let listed = projects(&harness.app, &harness.token).await;
    assert_eq!(listed.len(), 1, "one project expected: {listed:?}");
    assert_eq!(listed[0]["id"], alpha_id);
    assert_eq!(listed[0]["name"], "Alpha", "name is the cwd basename");
    assert_eq!(listed[0]["workspacePath"], "/tmp/fw-auto/Alpha");
    assert_eq!(listed[0]["manuallyCreated"], false);
    assert_eq!(listed[0]["connectorId"], harness.connector_id);

    // --- the second session for the same cwd reuses it ------------------
    let again = start_session(&harness, Some("/tmp/fw-auto/Alpha"), None, "second").await;
    assert_eq!(session_project(&again).as_deref(), Some(alpha_id.as_str()));
    assert_eq!(projects(&harness.app, &harness.token).await.len(), 1);

    // --- trailing slash and case are the same workspace ------------------
    let noisy = start_session(&harness, Some("/tmp/fw-auto/alpha/"), None, "third").await;
    assert_eq!(
        session_project(&noisy).as_deref(),
        Some(alpha_id.as_str()),
        "case and trailing separators must not fork a second project"
    );
    assert_eq!(projects(&harness.app, &harness.token).await.len(), 1);

    // --- the import route shares the same rule --------------------------
    let (status, imported) = call(
        &harness.app,
        Method::POST,
        "/api/v2/sessions",
        Some(&harness.token),
        Some(json!({
            "connectorId": harness.connector_id,
            "runtime": "codex",
            "externalSessionId": "dsh-1",
            "cwd": "/tmp/fw-auto/Alpha",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "import: {imported}");
    assert_eq!(
        imported["session"]["projectId"]
            .as_str()
            .map(str::to_string),
        Some(alpha_id.clone())
    );

    // --- a relative cwd names no workspace: the session stays unowned ---
    let relative = start_session(&harness, Some("relative/path"), None, "relative").await;
    assert_eq!(
        session_project(&relative),
        None,
        "a relative cwd must not invent a project"
    );
    assert_eq!(relative["session"]["cwd"], "relative/path");
    assert_eq!(projects(&harness.app, &harness.token).await.len(), 1);

    // --- no cwd at all is also unowned ----------------------------------
    let bare = start_session(&harness, None, None, "bare").await;
    assert_eq!(session_project(&bare), None);
    assert!(bare["session"]["cwd"].is_null(), "no cwd must stay null");
    assert_eq!(projects(&harness.app, &harness.token).await.len(), 1);

    // --- an explicit projectId is never overridden ----------------------
    let (status, manual) = call(
        &harness.app,
        Method::POST,
        "/api/v2/projects",
        Some(&harness.token),
        Some(json!({
            "name": "chosen by hand",
            "connectorId": harness.connector_id,
            "workspacePath": "/tmp/fw-auto/Beta",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "manual project: {manual}");
    let manual_id = manual["project"]["id"]
        .as_str()
        .expect("manual id")
        .to_string();
    assert_eq!(manual["project"]["manuallyCreated"], true);
    let explicit = start_session(
        &harness,
        Some("/tmp/fw-auto/Beta"),
        Some(&manual_id),
        "explicit",
    )
    .await;
    assert_eq!(
        session_project(&explicit).as_deref(),
        Some(manual_id.as_str())
    );
    let listed = projects(&harness.app, &harness.token).await;
    assert_eq!(
        listed.len(),
        2,
        "explicit binding adds no project: {listed:?}"
    );
    assert_eq!(
        listed
            .iter()
            .filter(|project| project["workspacePath"] == "/tmp/fw-auto/Beta")
            .count(),
        1
    );

    // --- concurrent sessions for one new workspace share one project ----
    // Four requests race the same read-then-create window; the unique index on
    // (user, connector, workspace_key) is what keeps it to one project, and
    // every response must point at the row that won.
    let racers = ["r1", "r2", "r3", "r4"];
    let futures = racers.iter().map(|title| {
        let body = json!({
            "connectorId": harness.connector_id,
            "runtime": "codex",
            "title": title,
            "content": "concurrent",
            "cwd": "/tmp/fw-auto/Gamma",
        });
        call(
            &harness.app,
            Method::POST,
            "/api/v2/sessions/create-and-start",
            Some(&harness.token),
            Some(body),
        )
    });
    let results = futures_util::future::join_all(futures).await;
    let mut gamma_ids = Vec::new();
    for (status, body) in results {
        assert_eq!(status, StatusCode::OK, "concurrent create: {body}");
        gamma_ids.push(
            body["session"]["projectId"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        );
    }
    assert!(
        gamma_ids
            .iter()
            .all(|id| !id.is_empty() && *id == gamma_ids[0]),
        "every concurrent session must bind the same project: {gamma_ids:?}"
    );
    let listed = projects(&harness.app, &harness.token).await;
    let gamma: Vec<&Value> = listed
        .iter()
        .filter(|project| project["workspacePath"] == "/tmp/fw-auto/Gamma")
        .collect();
    assert_eq!(gamma.len(), 1, "one project per workspace: {listed:?}");
    assert_eq!(gamma[0]["id"], gamma_ids[0]);

    // --- the inventory reports the binding ------------------------------
    let (status, inventory) = call(
        &harness.app,
        Method::GET,
        "/api/v2/sessions/list",
        Some(&harness.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "inventory: {inventory}");
    let bound = inventory["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .filter(|session| session["title"] == "first")
        .count();
    assert_eq!(bound, 1);
    assert!(inventory["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .any(|session| session["projectId"] == alpha_id.as_str()));
}

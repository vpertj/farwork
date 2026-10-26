//! End-to-end walk of /api/v2/admin/* against a real Postgres database.
//!
//! Runs only when TEST_DATABASE_URL points at a disposable database; without it
//! every test here skips loudly rather than passing quietly.
//!
//! The tests share one database and each one truncates it in setup, so they take
//! a process-wide mutex: cargo runs one test binary at a time, but the tests
//! *inside* a binary run on parallel threads and would truncate each other.

use axum::body::to_bytes;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use farwork_hub::config::Config;
use farwork_hub::{db, AppState};
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt as _;

const SETUP_TOKEN: &str = "setup-admin-test-token";
const SECRET: &str = "admin-test-secret";
const PASSWORD: &str = "correct horse battery staple";
const ADMIN_EMAIL: &str = "admin@example.com";
const MEMBER_EMAIL: &str = "member@example.com";

/// Serializes the tests in this file; they share one database. Async because the
/// guard is held across awaits, and clippy rejects a std guard there.
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn db_guard() -> tokio::sync::MutexGuard<'static, ()> {
    DB_LOCK.lock().await
}

fn test_database_url() -> Option<String> {
    match std::env::var("TEST_DATABASE_URL") {
        Ok(url) if !url.trim().is_empty() => Some(url),
        _ => {
            eprintln!(
                "SKIP farwork-hub admin integration test: TEST_DATABASE_URL is not set.\n\
                 Point it at a disposable database to run this test, e.g.\n  \
                 export TEST_DATABASE_URL=postgres://tianjun@127.0.0.1:5432/farwork_test"
            );
            None
        }
    }
}

struct Resp {
    status: StatusCode,
    text: String,
    json: Value,
}

impl Resp {
    fn detail_code(&self) -> Option<&str> {
        self.json["detail"]["code"].as_str()
    }

    fn detail_reason(&self) -> Option<&str> {
        self.json["detail"]["reason"].as_str()
    }
}

async fn raw_call(
    app: &Router,
    method: Method,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> Resp {
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
    let text = String::from_utf8_lossy(&bytes).to_string();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|_| Value::String(text.clone()))
    };
    Resp { status, text, json }
}

/// Register through the same two-step flow the browser uses; returns
/// (accessToken, userId).
async fn register(app: &Router, email: &str, setup_token: Option<&str>) -> (String, String) {
    let salt_resp = raw_call(
        app,
        Method::POST,
        "/api/v2/auth/password-salt",
        None,
        Some(json!({ "email": email })),
    )
    .await;
    assert_eq!(
        salt_resp.status,
        StatusCode::OK,
        "password-salt: {}",
        salt_resp.text
    );
    let salt = salt_resp.json["salt"].as_str().expect("salt").to_string();
    let verifier = farwork_hub::auth::derive_password_verifier(PASSWORD, &salt);
    let mut body = json!({
        "email": email,
        "passwordVerifier": verifier,
        "passwordSalt": salt,
    });
    if let Some(token) = setup_token {
        body["setupToken"] = json!(token);
    }
    let resp = raw_call(app, Method::POST, "/api/v2/auth/register", None, Some(body)).await;
    assert_eq!(
        resp.status,
        StatusCode::OK,
        "register {email}: {}",
        resp.text
    );
    (
        resp.json["accessToken"]
            .as_str()
            .expect("token")
            .to_string(),
        resp.json["userId"].as_str().expect("userId").to_string(),
    )
}

struct Harness {
    app: Router,
    pool: PgPool,
    admin_token: String,
    admin_id: String,
    member_token: String,
    member_id: String,
}

impl Harness {
    async fn setup(database_url: &str) -> Self {
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
        // Child rows first: users is referenced by connectors/projects/sessions/shares.
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

        let (admin_token, admin_id) = register(&app, ADMIN_EMAIL, Some(SETUP_TOKEN)).await;
        // Bootstrap closes self-service registration; open it for the member.
        farwork_hub::routes::auth::set_registration_open(&pool, true)
            .await
            .expect("open registration");
        let (member_token, member_id) = register(&app, MEMBER_EMAIL, None).await;

        Self {
            app,
            pool,
            admin_token,
            admin_id,
            member_token,
            member_id,
        }
    }

    async fn call(
        &self,
        method: Method,
        uri: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> Resp {
        raw_call(&self.app, method, uri, token, body).await
    }

    async fn create_user(&self, email: &str, role: &str) -> String {
        let resp = self
            .call(
                Method::POST,
                "/api/v2/admin/users",
                Some(&self.admin_token),
                Some(json!({ "email": email, "password": PASSWORD, "role": role })),
            )
            .await;
        assert_eq!(
            resp.status,
            StatusCode::CREATED,
            "create {role} {email}: {}",
            resp.text
        );
        resp.json["user"]["id"].as_str().expect("id").to_string()
    }

    async fn patch_user(&self, token: &str, id: &str, body: Value) -> Resp {
        self.call(
            Method::PATCH,
            &format!("/api/v2/admin/users/{id}"),
            Some(token),
            Some(body),
        )
        .await
    }

    async fn login(&self, email: &str, password: &str) -> Resp {
        self.call(
            Method::POST,
            "/api/v2/auth/login",
            None,
            Some(json!({ "email": email, "password": password })),
        )
        .await
    }
}

// ------------------------------------------------------------------ tests

#[tokio::test]
async fn admin_endpoints_reject_anonymous_and_members() {
    let Some(url) = test_database_url() else {
        return;
    };
    let _guard = db_guard().await;
    let h = Harness::setup(&url).await;

    let member_id = h.member_id.clone();
    let cases: Vec<(Method, String, Option<Value>)> = vec![
        (Method::GET, "/api/v2/admin/users".to_string(), None),
        (Method::GET, "/api/v2/admin/settings".to_string(), None),
        (
            Method::POST,
            "/api/v2/admin/users".to_string(),
            Some(json!({ "email": "anon@example.com", "password": PASSWORD, "role": "member" })),
        ),
        (
            Method::PATCH,
            format!("/api/v2/admin/users/{member_id}"),
            Some(json!({ "disabled": true })),
        ),
        (
            Method::PATCH,
            "/api/v2/admin/settings".to_string(),
            Some(json!({ "registrationOpen": false })),
        ),
    ];

    for (method, uri, body) in &cases {
        let anonymous = h.call(method.clone(), uri, None, body.clone()).await;
        assert_eq!(
            anonymous.status,
            StatusCode::UNAUTHORIZED,
            "anonymous {method} {uri}: {}",
            anonymous.text
        );

        let denied = h
            .call(method.clone(), uri, Some(&h.member_token), body.clone())
            .await;
        assert_eq!(
            denied.status,
            StatusCode::FORBIDDEN,
            "member {method} {uri}: {}",
            denied.text
        );
        assert_eq!(
            denied.detail_code(),
            Some("forbidden"),
            "member {method} {uri}: {}",
            denied.text
        );
    }

    // The admin survives the same matrix.
    let ok = h
        .call(
            Method::GET,
            "/api/v2/admin/users",
            Some(&h.admin_token),
            None,
        )
        .await;
    assert_eq!(ok.status, StatusCode::OK, "admin list: {}", ok.text);

    // Disabling an admin is honoured on the very next request: the guard
    // re-reads the row instead of trusting the token.
    let second_id = h.create_user("second-admin@example.com", "admin").await;
    let second_login = h.login("second-admin@example.com", PASSWORD).await;
    assert_eq!(second_login.status, StatusCode::OK, "{}", second_login.text);
    let second_token = second_login.json["accessToken"]
        .as_str()
        .expect("token")
        .to_string();
    sqlx::query("update users set disabled = true where id = $1")
        .bind(&second_id)
        .execute(&h.pool)
        .await
        .expect("disable");
    let disabled_admin = h
        .call(
            Method::GET,
            "/api/v2/admin/users",
            Some(&second_token),
            None,
        )
        .await;
    assert_eq!(
        disabled_admin.status,
        StatusCode::FORBIDDEN,
        "disabled admin: {}",
        disabled_admin.text
    );
    assert_eq!(disabled_admin.detail_code(), Some("forbidden"));
}

#[tokio::test]
async fn admin_created_account_can_log_in_and_duplicate_email_conflicts() {
    let Some(url) = test_database_url() else {
        return;
    };
    let _guard = db_guard().await;
    let h = Harness::setup(&url).await;

    let email = "created-admin@example.com";
    let created = h
        .call(
            Method::POST,
            "/api/v2/admin/users",
            Some(&h.admin_token),
            Some(json!({ "email": email, "password": PASSWORD, "role": "admin" })),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
    assert_eq!(created.json["user"]["email"], email);
    assert_eq!(created.json["user"]["role"], "admin");
    assert_eq!(created.json["user"]["disabled"], false);
    assert_eq!(created.json["user"]["emailVerified"], false);
    assert!(created.json["serverTime"].as_str().is_some());
    let created_id = created.json["user"]["id"].as_str().expect("id").to_string();
    assert!(
        !created.text.contains("password") && !created.text.contains("salt"),
        "create response must not mention hash material: {}",
        created.text
    );

    // Stored as the same pbkdf2 verifier the register path writes, never as the
    // plaintext.
    let stored: String = sqlx::query_scalar("select password_hash from users where id = $1")
        .bind(&created_id)
        .fetch_one(&h.pool)
        .await
        .expect("stored hash");
    assert!(
        stored.starts_with("pbkdf2_sha256$"),
        "unexpected stored form: {stored}"
    );
    assert!(
        !stored.contains(PASSWORD),
        "the plaintext password must never reach the column"
    );
    assert_eq!(
        stored.split('$').count(),
        3,
        "algorithm$salt$verifier: {stored}"
    );

    // The password the admin typed is the password that logs in.
    let login = h.login(email, PASSWORD).await;
    assert_eq!(login.status, StatusCode::OK, "{}", login.text);
    assert_eq!(login.json["userId"], created.json["user"]["id"]);
    assert_eq!(login.json["role"], "admin");
    let wrong = h.login(email, "not the password").await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED, "{}", wrong.text);

    // Duplicate email is a conflict, not a second account.
    let duplicate = h
        .call(
            Method::POST,
            "/api/v2/admin/users",
            Some(&h.admin_token),
            Some(json!({ "email": "CREATED-ADMIN@example.com", "password": PASSWORD, "role": "member" })),
        )
        .await;
    assert_eq!(duplicate.status, StatusCode::CONFLICT, "{}", duplicate.text);
    assert!(
        duplicate.text.contains("already in use"),
        "{}",
        duplicate.text
    );

    // Input validation stays a 422.
    let bad_role = h
        .call(
            Method::POST,
            "/api/v2/admin/users",
            Some(&h.admin_token),
            Some(json!({ "email": "owner@example.com", "password": PASSWORD, "role": "owner" })),
        )
        .await;
    assert_eq!(bad_role.status, StatusCode::UNPROCESSABLE_ENTITY);
    let no_password = h
        .call(
            Method::POST,
            "/api/v2/admin/users",
            Some(&h.admin_token),
            Some(json!({ "email": "nopw@example.com", "role": "member" })),
        )
        .await;
    assert_eq!(no_password.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn admin_user_list_never_exposes_hash_material() {
    let Some(url) = test_database_url() else {
        return;
    };
    let _guard = db_guard().await;
    let h = Harness::setup(&url).await;
    h.create_user("listed@example.com", "member").await;

    let full = h
        .call(
            Method::GET,
            "/api/v2/admin/users?limit=100",
            Some(&h.admin_token),
            None,
        )
        .await;
    assert_eq!(full.status, StatusCode::OK, "{}", full.text);
    let users = full.json["users"].as_array().expect("users array");
    assert_eq!(users.len(), 3, "admin + member + created: {}", full.text);
    assert!(full.json["nextCursor"].is_null());

    for needle in [
        "pbkdf2", "password", "Password", "salt", "Salt", "hash", "Hash",
    ] {
        assert!(
            !full.text.contains(needle),
            "user list must not contain {needle:?}: {}",
            full.text
        );
    }
    for user in users {
        let mut keys: Vec<&str> = user
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "createdAt",
                "disabled",
                "displayName",
                "email",
                "emailVerified",
                "id",
                "role"
            ]
        );
        assert!(user["id"].as_str().is_some());
        assert!(user["createdAt"].as_str().is_some());
        assert_eq!(user["emailVerified"], false);
        assert!(user["displayName"].as_str().is_some_and(|v| !v.is_empty()));
    }

    // Keyset paging: limit=1 hands back a cursor that leads to the next row.
    let first = h
        .call(
            Method::GET,
            "/api/v2/admin/users?limit=1",
            Some(&h.admin_token),
            None,
        )
        .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.text);
    assert_eq!(first.json["users"].as_array().expect("users").len(), 1);
    let cursor = first.json["nextCursor"]
        .as_str()
        .expect("nextCursor")
        .to_string();
    let second = h
        .call(
            Method::GET,
            &format!("/api/v2/admin/users?limit=1&cursor={cursor}"),
            Some(&h.admin_token),
            None,
        )
        .await;
    assert_eq!(second.status, StatusCode::OK, "{}", second.text);
    assert_ne!(
        first.json["users"][0]["id"], second.json["users"][0]["id"],
        "the cursor must advance"
    );
    assert!(second.json["nextCursor"].as_str().is_some());

    // An absent/garbage cursor is a 400, and limit is clamped to 100.
    let garbage = h
        .call(
            Method::GET,
            "/api/v2/admin/users?cursor=not-a-cursor",
            Some(&h.admin_token),
            None,
        )
        .await;
    assert_eq!(garbage.status, StatusCode::BAD_REQUEST, "{}", garbage.text);
    let clamped = h
        .call(
            Method::GET,
            "/api/v2/admin/users?limit=500",
            Some(&h.admin_token),
            None,
        )
        .await;
    assert_eq!(clamped.status, StatusCode::OK, "{}", clamped.text);
    assert_eq!(clamped.json["users"].as_array().expect("users").len(), 3);
}

#[tokio::test]
async fn admin_cannot_lock_itself_out() {
    let Some(url) = test_database_url() else {
        return;
    };
    let _guard = db_guard().await;
    let h = Harness::setup(&url).await;

    // Only one active admin exists: disabling yourself is refused outright.
    let self_disable = h
        .patch_user(&h.admin_token, &h.admin_id, json!({ "disabled": true }))
        .await;
    assert_eq!(
        self_disable.status,
        StatusCode::CONFLICT,
        "{}",
        self_disable.text
    );
    assert_eq!(self_disable.detail_code(), Some("admin_lockout"));
    assert_eq!(self_disable.detail_reason(), Some("self_disable"));
    assert!(
        self_disable.json["detail"]["message"]
            .as_str()
            .is_some_and(|msg| msg.contains("disable")),
        "the 409 must explain itself: {}",
        self_disable.text
    );

    // ... and demoting yourself is refused because you are the last admin.
    let self_demote_last = h
        .patch_user(&h.admin_token, &h.admin_id, json!({ "role": "member" }))
        .await;
    assert_eq!(
        self_demote_last.status,
        StatusCode::CONFLICT,
        "{}",
        self_demote_last.text
    );
    assert_eq!(self_demote_last.detail_reason(), Some("last_admin"));

    // Refused means unchanged, not "partially applied".
    let me = h
        .call(Method::GET, "/api/v2/auth/me", Some(&h.admin_token), None)
        .await;
    assert_eq!(me.status, StatusCode::OK, "{}", me.text);
    assert_eq!(me.json["role"], "admin");
    assert_eq!(me.json["disabled"], false);

    // With a second admin alive the self rule is the one reported.
    let second_id = h.create_user("second-admin@example.com", "admin").await;
    let self_demote = h
        .patch_user(&h.admin_token, &h.admin_id, json!({ "role": "member" }))
        .await;
    assert_eq!(
        self_demote.status,
        StatusCode::CONFLICT,
        "{}",
        self_demote.text
    );
    assert_eq!(self_demote.detail_reason(), Some("self_demote"));

    // Somebody else's admin row is demotable while an admin remains...
    let demoted = h
        .patch_user(&h.admin_token, &second_id, json!({ "role": "member" }))
        .await;
    assert_eq!(demoted.status, StatusCode::OK, "{}", demoted.text);
    assert_eq!(demoted.json["user"]["role"], "member");

    // ... which leaves exactly one admin, the caller.
    let listed = h
        .call(
            Method::GET,
            "/api/v2/admin/users?limit=100",
            Some(&h.admin_token),
            None,
        )
        .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.text);
    let admins = listed.json["users"]
        .as_array()
        .expect("users")
        .iter()
        .filter(|user| user["role"] == "admin" && user["disabled"] == false)
        .count();
    assert_eq!(admins, 1, "{}", listed.text);

    // A member is not part of the admin count, so disabling it is a plain PATCH.
    let disabled_member = h
        .patch_user(&h.admin_token, &second_id, json!({ "disabled": true }))
        .await;
    assert_eq!(
        disabled_member.status,
        StatusCode::OK,
        "{}",
        disabled_member.text
    );
    assert_eq!(disabled_member.json["user"]["disabled"], true);
}

#[tokio::test]
async fn admin_patch_and_settings_take_effect() {
    let Some(url) = test_database_url() else {
        return;
    };
    let _guard = db_guard().await;
    let h = Harness::setup(&url).await;

    // --- PATCH /admin/users/{id} ---------------------------------------
    let renamed = h
        .patch_user(
            &h.admin_token,
            &h.member_id,
            json!({ "displayName": "Ada Lovelace" }),
        )
        .await;
    assert_eq!(renamed.status, StatusCode::OK, "{}", renamed.text);
    assert_eq!(renamed.json["user"]["displayName"], "Ada Lovelace");
    assert_eq!(renamed.json["user"]["role"], "member");
    assert!(
        !renamed.text.contains("password") && !renamed.text.contains("salt"),
        "{}",
        renamed.text
    );

    let disabled = h
        .patch_user(&h.admin_token, &h.member_id, json!({ "disabled": true }))
        .await;
    assert_eq!(disabled.status, StatusCode::OK, "{}", disabled.text);
    assert_eq!(disabled.json["user"]["disabled"], true);
    // The change is real: a disabled account cannot log in any more.
    let blocked = h.login(MEMBER_EMAIL, PASSWORD).await;
    assert_eq!(blocked.status, StatusCode::FORBIDDEN, "{}", blocked.text);

    let enabled = h
        .patch_user(&h.admin_token, &h.member_id, json!({ "disabled": false }))
        .await;
    assert_eq!(enabled.status, StatusCode::OK, "{}", enabled.text);
    assert_eq!(enabled.json["user"]["disabled"], false);
    let allowed = h.login(MEMBER_EMAIL, PASSWORD).await;
    assert_eq!(allowed.status, StatusCode::OK, "{}", allowed.text);

    let bad_role = h
        .patch_user(&h.admin_token, &h.member_id, json!({ "role": "owner" }))
        .await;
    assert_eq!(bad_role.status, StatusCode::UNPROCESSABLE_ENTITY);
    let missing = h
        .patch_user(
            &h.admin_token,
            "usr_does_not_exist",
            json!({ "role": "admin" }),
        )
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND, "{}", missing.text);

    // --- GET/PATCH /admin/settings -------------------------------------
    let before = h
        .call(
            Method::GET,
            "/api/v2/admin/settings",
            Some(&h.admin_token),
            None,
        )
        .await;
    assert_eq!(before.status, StatusCode::OK, "{}", before.text);
    assert_eq!(before.json["settings"]["registrationOpen"], true);
    assert_eq!(before.json["settings"]["emailVerificationRequired"], false);
    assert!(before.json["serverTime"].as_str().is_some());

    let patched = h
        .call(
            Method::PATCH,
            "/api/v2/admin/settings",
            Some(&h.admin_token),
            Some(json!({ "registrationOpen": false, "emailVerificationRequired": true })),
        )
        .await;
    assert_eq!(patched.status, StatusCode::OK, "{}", patched.text);
    assert_eq!(patched.json["settings"]["registrationOpen"], false);
    assert_eq!(patched.json["settings"]["emailVerificationRequired"], true);

    // /auth/config is the unauthenticated mirror of the same rows.
    let config = h.call(Method::GET, "/api/v2/auth/config", None, None).await;
    assert_eq!(config.status, StatusCode::OK, "{}", config.text);
    assert_eq!(config.json["registrationOpen"], false);
    assert_eq!(config.json["emailVerificationRequired"], true);

    // Closed registration is enforced, not just echoed.
    let salt = raw_call(
        &h.app,
        Method::POST,
        "/api/v2/auth/password-salt",
        None,
        Some(json!({ "email": "late@example.com" })),
    )
    .await;
    let salt = salt.json["salt"].as_str().expect("salt").to_string();
    let closed = raw_call(
        &h.app,
        Method::POST,
        "/api/v2/auth/register",
        None,
        Some(json!({
            "email": "late@example.com",
            "passwordVerifier": farwork_hub::auth::derive_password_verifier(PASSWORD, &salt),
            "passwordSalt": salt,
        })),
    )
    .await;
    assert_eq!(closed.status, StatusCode::FORBIDDEN, "{}", closed.text);

    // A partial patch leaves the other switch alone, and an empty patch is a
    // no-op that still returns the current values.
    let reopened = h
        .call(
            Method::PATCH,
            "/api/v2/admin/settings",
            Some(&h.admin_token),
            Some(json!({ "registrationOpen": true })),
        )
        .await;
    assert_eq!(reopened.status, StatusCode::OK, "{}", reopened.text);
    assert_eq!(reopened.json["settings"]["registrationOpen"], true);
    assert_eq!(reopened.json["settings"]["emailVerificationRequired"], true);
    let config = h.call(Method::GET, "/api/v2/auth/config", None, None).await;
    assert_eq!(config.json["registrationOpen"], true);
    let noop = h
        .call(
            Method::PATCH,
            "/api/v2/admin/settings",
            Some(&h.admin_token),
            Some(json!({})),
        )
        .await;
    assert_eq!(noop.status, StatusCode::OK, "{}", noop.text);
    assert_eq!(noop.json["settings"]["registrationOpen"], true);
    assert_eq!(noop.json["settings"]["emailVerificationRequired"], true);
}

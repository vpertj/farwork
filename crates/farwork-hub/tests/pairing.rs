//! Pairing codes against a real PostgreSQL database.
//!
//! What these assertions protect: the code is a one-time, short-lived credential
//! that creates a device in the owner's account, the whole flow works with **no**
//! credentials on the claim side, and the plaintext code never lands in the
//! database.
//!
//! Runs only when `TEST_DATABASE_URL` points at a disposable database; without it
//! every test here skips loudly rather than passing quietly.
//!
//! The tests share one database and each one truncates it in setup, so they take a
//! process-wide mutex: cargo runs one test binary at a time, but the tests
//! *inside* a binary run on parallel threads and would truncate each other.

use std::net::SocketAddr;

use axum::body::to_bytes;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt as _;

use farwork_hub::config::Config;
use farwork_hub::{db, AppState};

const SETUP_TOKEN: &str = "setup-pairing-token";
const SECRET: &str = "pairing-integration-secret";
const PASSWORD: &str = "correct horse battery staple";
const OWNER_EMAIL: &str = "pairing-owner@example.com";

static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn db_guard() -> tokio::sync::MutexGuard<'static, ()> {
    DB_LOCK.lock().await
}

fn test_database_url() -> Option<String> {
    match std::env::var("TEST_DATABASE_URL") {
        Ok(url) if !url.trim().is_empty() => Some(url),
        _ => {
            eprintln!(
                "SKIP farwork-hub pairing integration test: TEST_DATABASE_URL is not set.\n\
                 Point it at a disposable database to run this test, e.g.\n  \
                 export TEST_DATABASE_URL=postgres://tianjun@127.0.0.1:5432/farwork_test"
            );
            None
        }
    }
}

fn source(last_octet: u8) -> SocketAddr {
    SocketAddr::from(([10, 0, 0, last_octet], 40_000))
}

struct Resp {
    status: StatusCode,
    json: Value,
    text: String,
}

impl Resp {
    fn code(&self) -> Option<&str> {
        self.json["detail"]["code"].as_str()
    }
}

struct Harness {
    app: Router,
    pool: PgPool,
    owner_token: String,
    owner_id: String,
    other_token: String,
}

/// One HTTP call, optionally carrying the peer address the rate limiter keys on
/// (`ConnectInfo` is what `main` attaches through
/// `into_make_service_with_connect_info`).
async fn call(
    app: &Router,
    method: Method,
    uri: &str,
    token: Option<&str>,
    from: Option<SocketAddr>,
    body: Option<Value>,
) -> Resp {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    if let Some(from) = from {
        builder = builder.extension(ConnectInfo(from));
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
    let json = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or_else(|_| Value::String(text.clone()))
    };
    Resp { status, json, text }
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
    // pairings first: it references both users and connectors.
    for table in [
        "pairings",
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

    let salt_response = call(
        &app,
        Method::POST,
        "/api/v2/auth/password-salt",
        None,
        None,
        Some(json!({ "email": OWNER_EMAIL })),
    )
    .await;
    assert_eq!(
        salt_response.status,
        StatusCode::OK,
        "password-salt: {}",
        salt_response.text
    );
    let salt = salt_response.json["salt"]
        .as_str()
        .expect("salt")
        .to_string();
    let verifier = farwork_hub::auth::derive_password_verifier(PASSWORD, &salt);

    let registered = call(
        &app,
        Method::POST,
        "/api/v2/auth/register",
        None,
        None,
        Some(json!({
            "email": OWNER_EMAIL,
            "displayName": "Owner",
            "passwordVerifier": verifier,
            "passwordSalt": salt,
            "setupToken": SETUP_TOKEN,
        })),
    )
    .await;
    assert_eq!(
        registered.status,
        StatusCode::OK,
        "register: {}",
        registered.text
    );
    let owner_token = registered.json["accessToken"]
        .as_str()
        .expect("token")
        .to_string();
    let owner_id = registered.json["userId"]
        .as_str()
        .expect("user id")
        .to_string();

    // Second account straight into the table: bootstrap closed self-service
    // registration, and this test only needs a *different* valid token.
    let other_user_id = farwork_hub::ids::user_id();
    let now = farwork_hub::timeutil::utc_now();
    sqlx::query(
        "insert into users (id, email, display_name, password_hash, created_at, updated_at)
         values ($1, 'pairing-other@example.com', 'Mallory', 'pbkdf2_sha256$salt$verifier', $2, $2)",
    )
    .bind(&other_user_id)
    .bind(&now)
    .execute(&pool)
    .await
    .expect("other user");
    let other_token = farwork_hub::auth::create_user_access_token(SECRET, &other_user_id);

    Harness {
        app,
        pool,
        owner_token,
        owner_id,
        other_token,
    }
}

/// Mint a code for the owner and return `(pairingId, code, expiresAt)`.
async fn start_pairing(h: &Harness, ttl_seconds: Option<i64>) -> (String, String, String) {
    let body = match ttl_seconds {
        Some(ttl) => json!({ "ttlSeconds": ttl }),
        None => json!({}),
    };
    let response = call(
        &h.app,
        Method::POST,
        "/api/v2/pairing/start",
        Some(&h.owner_token),
        None,
        Some(body),
    )
    .await;
    assert_eq!(
        response.status,
        StatusCode::CREATED,
        "start: {}",
        response.text
    );
    let pairing = &response.json["pairing"];
    (
        pairing["id"].as_str().expect("pairing id").to_string(),
        pairing["code"].as_str().expect("code").to_string(),
        pairing["expiresAt"]
            .as_str()
            .expect("expiresAt")
            .to_string(),
    )
}

async fn claim(h: &Harness, from: SocketAddr, body: Value) -> Resp {
    call(
        &h.app,
        Method::POST,
        "/api/v2/pairing/claim",
        None,
        Some(from),
        Some(body),
    )
    .await
}

async fn status_of(h: &Harness, token: &str, pairing_id: &str) -> Value {
    let response = call(
        &h.app,
        Method::GET,
        &format!("/api/v2/pairing/{pairing_id}"),
        Some(token),
        None,
        None,
    )
    .await;
    assert_eq!(response.status, StatusCode::OK, "status: {}", response.text);
    response.json["pairing"].clone()
}

#[tokio::test]
async fn start_requires_a_login_and_validates_the_ttl() {
    let Some(database_url) = test_database_url() else {
        return;
    };
    let _guard = db_guard().await;
    let h = bootstrap(&database_url).await;

    let anonymous = call(
        &h.app,
        Method::POST,
        "/api/v2/pairing/start",
        None,
        None,
        Some(json!({})),
    )
    .await;
    assert_eq!(
        anonymous.status,
        StatusCode::UNAUTHORIZED,
        "start must not work without a login: {}",
        anonymous.text
    );

    for ttl in [59, 3_601, 0, -600] {
        let rejected = call(
            &h.app,
            Method::POST,
            "/api/v2/pairing/start",
            Some(&h.owner_token),
            None,
            Some(json!({ "ttlSeconds": ttl })),
        )
        .await;
        assert_eq!(
            rejected.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "ttlSeconds={ttl} must be rejected: {}",
            rejected.text
        );
    }

    let (pairing_id, code, expires_at) = start_pairing(&h, None).await;
    assert!(pairing_id.starts_with("pair_"), "{pairing_id}");
    // Shape and alphabet: XXXX-XXXX, no I/L/O/U/0/1.
    assert_eq!(code.len(), 9, "{code}");
    assert_eq!(code.as_bytes()[4], b'-', "{code}");
    for byte in code.bytes() {
        assert!(
            byte == b'-'
                || byte.is_ascii_uppercase() && !b"ILOU".contains(&byte)
                || byte.is_ascii_digit() && !b"01".contains(&byte),
            "{} is not an allowed code character: {code}",
            byte as char
        );
    }

    // The default TTL is 600s, measured against the row rather than the clock.
    let stored: i64 = sqlx::query_scalar("select expires_at_epoch from pairings where id = $1")
        .bind(&pairing_id)
        .fetch_one(&h.pool)
        .await
        .expect("expiry");
    let delta = stored - farwork_hub::timeutil::unix_now();
    assert!(
        (595..=600).contains(&delta),
        "default ttl is 600s, got {delta}"
    );
    assert!(expires_at.ends_with('Z'), "{expires_at}");

    let (short_id, _, _) = start_pairing(&h, Some(60)).await;
    let stored: i64 = sqlx::query_scalar("select expires_at_epoch from pairings where id = $1")
        .bind(&short_id)
        .fetch_one(&h.pool)
        .await
        .expect("expiry");
    let delta = stored - farwork_hub::timeutil::unix_now();
    assert!((55..=60).contains(&delta), "explicit ttl=60, got {delta}");
}

#[tokio::test]
async fn claim_is_unauthenticated_and_creates_the_connector_in_the_owners_account() {
    let Some(database_url) = test_database_url() else {
        return;
    };
    let _guard = db_guard().await;
    let h = bootstrap(&database_url).await;

    let (pairing_id, code, _) = start_pairing(&h, None).await;
    // Lower case and no hyphen: the code is normalized before it is hashed, so a
    // human retyping it is not punished for formatting.
    let typed = code.to_lowercase().replace('-', "");
    let claimed = claim(
        &h,
        source(11),
        json!({ "code": typed, "name": "我的 Mac", "platform": "macOS" }),
    )
    .await;
    assert_eq!(claimed.status, StatusCode::OK, "claim: {}", claimed.text);
    assert!(
        claimed.text.contains("我的 Mac"),
        "the device keeps the name the user gave it: {}",
        claimed.text
    );
    let connector = &claimed.json["connector"];
    let connector_id = connector["id"].as_str().expect("connector id");
    assert!(connector_id.starts_with("conn_"), "{connector_id}");
    assert_eq!(connector["deviceOs"], "macOS");
    assert_eq!(connector["status"], "offline");
    let token = claimed.json["connectorToken"]
        .as_str()
        .expect("connector token")
        .to_string();
    assert!(token.starts_with("cxt_"), "{token}");

    // The token is real: it resolves the same connector by digest, exactly the
    // lookup the ingress handshake performs.
    let found = farwork_hub::routes::connectors::find_connector_by_token(
        &h.pool,
        &farwork_hub::auth::sha256_hex(token.as_bytes()),
    )
    .await
    .expect("lookup")
    .expect("connector by token");
    assert_eq!(found.id, connector_id);
    assert_eq!(found.user_id, h.owner_id);

    // And it is visible through the owner's own connector list.
    let listed = call(
        &h.app,
        Method::GET,
        "/api/v2/connectors",
        Some(&h.owner_token),
        None,
        None,
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.text);
    let ids: Vec<&str> = listed.json["connectors"]
        .as_array()
        .expect("connectors")
        .iter()
        .filter_map(|entry| entry["id"].as_str())
        .collect();
    assert_eq!(ids, vec![connector_id]);

    // The pairing row records the claim, the connector and the source address.
    let row: (Option<String>, Option<String>, Option<String>) =
        sqlx::query_as("select claimed_at, connector_id, attempt_ip from pairings where id = $1")
            .bind(&pairing_id)
            .fetch_one(&h.pool)
            .await
            .expect("pairing row");
    assert!(row.0.is_some(), "claimed_at must be set");
    assert_eq!(row.1.as_deref(), Some(connector_id));
    assert_eq!(row.2.as_deref(), Some("10.0.0.11"));
}

#[tokio::test]
async fn a_code_cannot_be_claimed_twice() {
    let Some(database_url) = test_database_url() else {
        return;
    };
    let _guard = db_guard().await;
    let h = bootstrap(&database_url).await;

    let (_, code, _) = start_pairing(&h, None).await;
    let first = claim(
        &h,
        source(12),
        json!({ "code": code, "name": "first device" }),
    )
    .await;
    assert_eq!(first.status, StatusCode::OK, "first claim: {}", first.text);

    let second = claim(
        &h,
        source(13),
        json!({ "code": code, "name": "second device" }),
    )
    .await;
    assert_eq!(
        second.status,
        StatusCode::CONFLICT,
        "second claim: {}",
        second.text
    );
    assert_eq!(second.code(), Some("code_already_claimed"));

    // A refused second claim must not leave a stray device behind.
    let devices: i64 = sqlx::query_scalar("select count(*) from connectors")
        .fetch_one(&h.pool)
        .await
        .expect("count");
    assert_eq!(devices, 1);
}

#[tokio::test]
async fn an_expired_code_answers_410() {
    let Some(database_url) = test_database_url() else {
        return;
    };
    let _guard = db_guard().await;
    let h = bootstrap(&database_url).await;

    let (pairing_id, code, _) = start_pairing(&h, Some(60)).await;
    // Age the row instead of waiting a minute.
    sqlx::query("update pairings set expires_at_epoch = $2 where id = $1")
        .bind(&pairing_id)
        .bind(farwork_hub::timeutil::unix_now() - 1)
        .execute(&h.pool)
        .await
        .expect("age pairing");

    let claimed = claim(&h, source(14), json!({ "code": code, "name": "late" })).await;
    assert_eq!(
        claimed.status,
        StatusCode::GONE,
        "expired claim: {}",
        claimed.text
    );
    assert_eq!(claimed.code(), Some("code_expired"));
    assert_eq!(
        status_of(&h, &h.owner_token, &pairing_id).await["status"],
        "expired"
    );
    let devices: i64 = sqlx::query_scalar("select count(*) from connectors")
        .fetch_one(&h.pool)
        .await
        .expect("count");
    assert_eq!(devices, 0, "an expired code must not register a device");
}

#[tokio::test]
async fn an_unknown_or_malformed_code_answers_404() {
    let Some(database_url) = test_database_url() else {
        return;
    };
    let _guard = db_guard().await;
    let h = bootstrap(&database_url).await;

    for code in ["ZZZZ-ZZZZ", "4F2K", "4F2K-9QXI", "", "IIII-IIII"] {
        let response = claim(&h, source(15), json!({ "code": code, "name": "probe" })).await;
        assert_eq!(
            response.status,
            StatusCode::NOT_FOUND,
            "{code:?} must be an invalid code: {}",
            response.text
        );
        assert_eq!(response.code(), Some("invalid_code"), "{code:?}");
    }
    let devices: i64 = sqlx::query_scalar("select count(*) from connectors")
        .fetch_one(&h.pool)
        .await
        .expect("count");
    assert_eq!(devices, 0);
}

#[tokio::test]
async fn claim_attempts_are_limited_per_source_address() {
    let Some(database_url) = test_database_url() else {
        return;
    };
    let _guard = db_guard().await;
    let h = bootstrap(&database_url).await;

    for attempt in 1..=10 {
        let response = claim(
            &h,
            source(16),
            json!({ "code": "ZZZZ-ZZZZ", "name": "spray" }),
        )
        .await;
        assert_eq!(
            response.status,
            StatusCode::NOT_FOUND,
            "attempt {attempt} must still be a plain miss: {}",
            response.text
        );
    }
    let limited = claim(
        &h,
        source(16),
        json!({ "code": "ZZZZ-ZZZZ", "name": "spray" }),
    )
    .await;
    assert_eq!(
        limited.status,
        StatusCode::TOO_MANY_REQUESTS,
        "eleventh attempt: {}",
        limited.text
    );
    assert_eq!(limited.code(), Some("too_many_attempts"));

    // A *valid* code from the same address is refused too: the budget is spent.
    let (_, code, _) = start_pairing(&h, None).await;
    let blocked = claim(&h, source(16), json!({ "code": code, "name": "real" })).await;
    assert_eq!(
        blocked.status,
        StatusCode::TOO_MANY_REQUESTS,
        "a spent budget must not be dodged with a valid code: {}",
        blocked.text
    );

    // A different address has its own window, so the limit is per source.
    let other = claim(&h, source(17), json!({ "code": code, "name": "real" })).await;
    assert_eq!(other.status, StatusCode::OK, "other source: {}", other.text);
}

#[tokio::test]
async fn pairing_status_follows_the_claim() {
    let Some(database_url) = test_database_url() else {
        return;
    };
    let _guard = db_guard().await;
    let h = bootstrap(&database_url).await;

    let (pairing_id, code, expires_at) = start_pairing(&h, None).await;
    let pending = status_of(&h, &h.owner_token, &pairing_id).await;
    assert_eq!(pending["status"], "pending");
    assert_eq!(pending["expiresAt"], expires_at.as_str());
    assert!(
        pending.get("connectorId").is_none(),
        "a pending pairing has no connector: {pending}"
    );

    let claimed = claim(&h, source(18), json!({ "code": code, "name": "polled" })).await;
    assert_eq!(claimed.status, StatusCode::OK, "{}", claimed.text);
    let connector_id = claimed.json["connector"]["id"]
        .as_str()
        .expect("connector id")
        .to_string();

    let after = status_of(&h, &h.owner_token, &pairing_id).await;
    assert_eq!(after["status"], "claimed");
    assert_eq!(after["connectorId"], connector_id.as_str());
    assert_eq!(after["expiresAt"], expires_at.as_str());

    // Someone else's pairing id is indistinguishable from one that never existed,
    // so this endpoint cannot be used to enumerate other accounts.
    let stranger = status_of(&h, &h.other_token, &pairing_id).await;
    assert_eq!(stranger["status"], "unknown");
    assert!(stranger["expiresAt"].is_null());
    let missing = status_of(&h, &h.owner_token, "pair_does-not-exist").await;
    assert_eq!(missing["status"], "unknown");
}

#[tokio::test]
async fn neither_the_code_nor_the_connector_token_reaches_the_database() {
    let Some(database_url) = test_database_url() else {
        return;
    };
    let _guard = db_guard().await;
    let h = bootstrap(&database_url).await;

    let (pairing_id, code, _) = start_pairing(&h, None).await;
    let claimed = claim(
        &h,
        source(19),
        json!({ "code": code, "name": "secret check" }),
    )
    .await;
    assert_eq!(claimed.status, StatusCode::OK, "{}", claimed.text);
    let token = claimed.json["connectorToken"]
        .as_str()
        .expect("connector token")
        .to_string();

    // Every column of every row, as text: a nested or newly added column cannot
    // hide a plaintext value from this check.
    let pairing_json: Vec<String> =
        sqlx::query_scalar("select row_to_json(p)::text from pairings p")
            .fetch_all(&h.pool)
            .await
            .expect("pairings");
    let connector_json: Vec<String> =
        sqlx::query_scalar("select row_to_json(c)::text from connectors c")
            .fetch_all(&h.pool)
            .await
            .expect("connectors");
    let dump = format!(
        "{} {} {}",
        pairing_json.join(" "),
        connector_json.join(" "),
        ""
    );
    assert!(
        !dump.contains(&code.chars().filter(|c| *c != '-').collect::<String>()),
        "the plaintext pairing code is in the database: {dump}"
    );
    assert!(
        !dump.contains(&token),
        "the connector token is in the database: {dump}"
    );

    let code_hash: String = sqlx::query_scalar("select code_hash from pairings where id = $1")
        .bind(&pairing_id)
        .fetch_one(&h.pool)
        .await
        .expect("code hash");
    assert_eq!(
        code_hash.len(),
        64,
        "sha256 hex digest expected: {code_hash}"
    );
    assert_ne!(code_hash, code, "the hash is not the code");
}

//! QR mobile login against a real PostgreSQL database.
//!
//! What these assertions protect: the QR token is a one-time, 120-second
//! credential that mints a session for exactly one account with no existing
//! credentials on the phone; and the three token families (access / mobile
//! login / mobile refresh) cannot stand in for each other. A refresh token that
//! works as a bearer token, or an access token that works as a refresh token,
//! would be a privilege escalation that no other test in this crate can see.
//!
//! Runs only when `TEST_DATABASE_URL` points at a disposable database; without
//! it every test here skips loudly rather than passing quietly.
//!
//! The tests share one database and each one truncates it in setup, so they take
//! a process-wide mutex: cargo runs one test binary at a time, but the tests
//! *inside* a binary run on parallel threads and would truncate each other.

use axum::body::to_bytes;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt as _;

use farwork_hub::auth::{self, b64url_decode, create_jwt, sha256_hex};
use farwork_hub::config::Config;
use farwork_hub::{db, AppState};

const SETUP_TOKEN: &str = "setup-mobile-login-token";
const SECRET: &str = "mobile-login-integration-secret";
const PASSWORD: &str = "correct horse battery staple";
const OWNER_EMAIL: &str = "mobile-owner@example.com";
const INVALID_LOGIN: &str = "invalid or expired mobile login token";
const INVALID_REFRESH: &str = "invalid or expired mobile refresh token";

static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Resp {
    status: StatusCode,
    json: Value,
    text: String,
}

struct Harness {
    app: Router,
    pool: PgPool,
    owner_token: String,
    owner_id: String,
}

async fn db_guard() -> tokio::sync::MutexGuard<'static, ()> {
    DB_LOCK.lock().await
}

fn database_url() -> Option<String> {
    match std::env::var("TEST_DATABASE_URL") {
        Ok(url) if !url.trim().is_empty() => Some(url),
        _ => {
            eprintln!(
                "SKIP farwork-hub mobile-login integration test: TEST_DATABASE_URL is not set.\n\
                 Point it at a disposable database to run this test, e.g.\n  \
                 export TEST_DATABASE_URL=postgres://tianjun@127.0.0.1:5432/farwork_test"
            );
            None
        }
    }
}

async fn call(
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
    let json = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or_else(|_| Value::String(text.clone()))
    };
    Resp { status, json, text }
}

async fn bootstrap(url: &str) -> Harness {
    let config = Config {
        database_url: url.to_string(),
        port: 0,
        secret: SECRET.to_string(),
        setup_token: Some(SETUP_TOKEN.to_string()),
        database_max_connections: 5,
        public_url: None,
        static_dir: None,
    };
    let pool = db::connect(&config).await.expect("connect");
    db::migrate(&pool).await.expect("migrate");
    // mobile_login_tokens first: it references users.
    for table in [
        "mobile_login_tokens",
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
    let verifier = auth::derive_password_verifier(PASSWORD, &salt);

    let registered = call(
        &app,
        Method::POST,
        "/api/v2/auth/register",
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
    Harness {
        app,
        pool,
        owner_token: registered.json["accessToken"]
            .as_str()
            .expect("token")
            .to_string(),
        owner_id: registered.json["userId"]
            .as_str()
            .expect("user id")
            .to_string(),
    }
}

/// A second account inserted straight into the table: bootstrap closed
/// self-service registration, and these tests only need a *different* valid id.
async fn insert_user(pool: &PgPool, email: &str, disabled: bool) -> String {
    let user_id = farwork_hub::ids::user_id();
    let now = farwork_hub::timeutil::utc_now();
    sqlx::query(
        "insert into users
            (id, email, display_name, password_hash, role, disabled, created_at, updated_at)
         values ($1, $2, 'Other', 'pbkdf2_sha256$salt$verifier', 'member', $3, $4, $4)",
    )
    .bind(&user_id)
    .bind(email)
    .bind(disabled)
    .bind(&now)
    .execute(pool)
    .await
    .expect("insert user");
    user_id
}

async fn store_login_token(pool: &PgPool, token: &str, user_id: &str, expires_epoch: i64) {
    sqlx::query(
        "insert into mobile_login_tokens (token_hash, user_id, expires_at_epoch, created_at)
         values ($1, $2, $3, $4)",
    )
    .bind(sha256_hex(token.as_bytes()))
    .bind(user_id)
    .bind(expires_epoch)
    .bind(farwork_hub::timeutil::utc_now())
    .execute(pool)
    .await
    .expect("insert token");
}

async fn qr(h: &Harness, token: Option<&str>) -> Resp {
    call(
        &h.app,
        Method::POST,
        "/api/v2/auth/mobile-login/qr",
        token,
        Some(json!({})),
    )
    .await
}

async fn exchange(h: &Harness, login_token: &str, user_id: &str) -> Resp {
    call(
        &h.app,
        Method::POST,
        "/api/v2/auth/mobile-login/exchange",
        None,
        Some(json!({ "loginToken": login_token, "userId": user_id })),
    )
    .await
}

async fn refresh(h: &Harness, refresh_token: &str) -> Resp {
    call(
        &h.app,
        Method::POST,
        "/api/v2/auth/mobile-login/refresh",
        None,
        Some(json!({ "refreshToken": refresh_token })),
    )
    .await
}

fn claim(token: &str, name: &str) -> Value {
    let payload = token.split('.').nth(1).expect("payload");
    let raw = b64url_decode(payload).expect("decode");
    let data: Value = serde_json::from_slice(&raw).expect("json");
    data.get(name).cloned().unwrap_or(Value::Null)
}

fn detail(resp: &Resp) -> String {
    resp.json["detail"].as_str().unwrap_or_default().to_string()
}

#[tokio::test]
async fn qr_requires_a_logged_in_user() {
    let Some(url) = database_url() else { return };
    let _guard = db_guard().await;
    let h = bootstrap(&url).await;

    let anonymous = qr(&h, None).await;
    assert_eq!(
        anonymous.status,
        StatusCode::UNAUTHORIZED,
        "{}",
        anonymous.text
    );

    let garbage = qr(&h, Some("not-a-token")).await;
    assert_eq!(garbage.status, StatusCode::UNAUTHORIZED);

    let minted = qr(&h, Some(&h.owner_token)).await;
    assert_eq!(minted.status, StatusCode::CREATED, "{}", minted.text);
    assert_eq!(minted.json["userId"], h.owner_id.as_str());
    let login_token = minted.json["loginToken"].as_str().expect("loginToken");
    assert!(!login_token.is_empty());
    assert!(minted.json["expiresAt"]
        .as_str()
        .unwrap_or_default()
        .ends_with('Z'));
    assert!(minted.json["serverTime"]
        .as_str()
        .unwrap_or_default()
        .ends_with('Z'));

    // 120 seconds, checked on the signed claim rather than on the ISO string.
    assert_eq!(claim(login_token, "kind"), json!("mobile_login"));
    assert_eq!(claim(login_token, "sub"), json!(h.owner_id));
    let delta =
        claim(login_token, "exp").as_i64().expect("exp") - farwork_hub::timeutil::unix_now();
    assert!((118..=120).contains(&delta), "login token ttl was {delta}");
}

#[tokio::test]
async fn exchange_is_anonymous_and_works_exactly_once() {
    let Some(url) = database_url() else { return };
    let _guard = db_guard().await;
    let h = bootstrap(&url).await;

    let qr_response = qr(&h, Some(&h.owner_token)).await;
    assert_eq!(qr_response.status, StatusCode::CREATED);
    let login_token = qr_response.json["loginToken"]
        .as_str()
        .expect("loginToken")
        .to_string();
    let stored: i64 = sqlx::query_scalar("select count(*) from mobile_login_tokens")
        .fetch_one(&h.pool)
        .await
        .expect("count");
    assert_eq!(
        stored, 1,
        "the QR token is recorded before it is handed out"
    );

    // No Authorization header at all: the token is the credential.
    let first = exchange(&h, &login_token, &h.owner_id).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.text);
    assert_eq!(first.json["auth"]["userId"], h.owner_id.as_str());
    assert_eq!(first.json["auth"]["tokenType"], "bearer");
    let access = first.json["auth"]["accessToken"]
        .as_str()
        .expect("accessToken")
        .to_string();
    let refresh_token = first.json["refreshToken"]
        .as_str()
        .expect("refreshToken")
        .to_string();
    assert!(first.json["expiresAt"]
        .as_str()
        .unwrap_or_default()
        .ends_with('Z'));
    assert_eq!(claim(&refresh_token, "kind"), json!("mobile_refresh"));
    let refresh_delta =
        claim(&refresh_token, "exp").as_i64().expect("exp") - farwork_hub::timeutil::unix_now();
    assert!(
        (refresh_delta - 60 * 60 * 24 * 30).abs() <= 2,
        "refresh ttl was {refresh_delta}"
    );

    let me = call(&h.app, Method::GET, "/api/v2/auth/me", Some(&access), None).await;
    assert_eq!(me.status, StatusCode::OK, "{}", me.text);
    assert_eq!(me.json["userId"], h.owner_id.as_str());

    // The row is gone, so the second exchange has nothing to consume.
    let consumed: i64 = sqlx::query_scalar("select count(*) from mobile_login_tokens")
        .fetch_one(&h.pool)
        .await
        .expect("count");
    assert_eq!(consumed, 0);
    let replay = exchange(&h, &login_token, &h.owner_id).await;
    assert_eq!(replay.status, StatusCode::UNAUTHORIZED, "{}", replay.text);
    assert_eq!(detail(&replay), INVALID_LOGIN);
}

#[tokio::test]
async fn an_expired_login_token_is_rejected() {
    let Some(url) = database_url() else { return };
    let _guard = db_guard().await;
    let h = bootstrap(&url).await;

    // Expired signature: rejected before the row is even looked at.
    let stale = auth::create_signed_token(SECRET, auth::TOKEN_KIND_MOBILE_LOGIN, &h.owner_id, -10);
    store_login_token(
        &h.pool,
        &stale,
        &h.owner_id,
        farwork_hub::timeutil::unix_now() - 10,
    )
    .await;
    let expired_jwt = exchange(&h, &stale, &h.owner_id).await;
    assert_eq!(expired_jwt.status, StatusCode::UNAUTHORIZED);
    assert_eq!(detail(&expired_jwt), INVALID_LOGIN);

    // Live signature, closed window: the stored row is what rejects it.
    let (live, _) = auth::create_mobile_login_token(SECRET, &h.owner_id);
    store_login_token(
        &h.pool,
        &live,
        &h.owner_id,
        farwork_hub::timeutil::unix_now() - 1,
    )
    .await;
    let expired_row = exchange(&h, &live, &h.owner_id).await;
    assert_eq!(expired_row.status, StatusCode::UNAUTHORIZED);
    assert_eq!(detail(&expired_row), INVALID_LOGIN);
}

#[tokio::test]
async fn a_login_token_for_another_user_is_rejected() {
    let Some(url) = database_url() else { return };
    let _guard = db_guard().await;
    let h = bootstrap(&url).await;
    let other_id = insert_user(&h.pool, "mobile-other@example.com", false).await;

    let qr_response = qr(&h, Some(&h.owner_token)).await;
    let login_token = qr_response.json["loginToken"]
        .as_str()
        .expect("loginToken")
        .to_string();

    let mismatched = exchange(&h, &login_token, &other_id).await;
    assert_eq!(
        mismatched.status,
        StatusCode::UNAUTHORIZED,
        "{}",
        mismatched.text
    );
    assert_eq!(detail(&mismatched), INVALID_LOGIN);

    // A rejected userId must not burn the token: the owner still has it.
    let still_stored: i64 = sqlx::query_scalar("select count(*) from mobile_login_tokens")
        .fetch_one(&h.pool)
        .await
        .expect("count");
    assert_eq!(still_stored, 1);
    let ok = exchange(&h, &login_token, &h.owner_id).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text);
}

#[tokio::test]
async fn a_disabled_user_can_neither_exchange_nor_refresh() {
    let Some(url) = database_url() else { return };
    let _guard = db_guard().await;
    let h = bootstrap(&url).await;
    let disabled_id = insert_user(&h.pool, "mobile-disabled@example.com", true).await;

    let (login_token, expires) = auth::create_mobile_login_token(SECRET, &disabled_id);
    store_login_token(&h.pool, &login_token, &disabled_id, expires).await;
    let exchange_response = exchange(&h, &login_token, &disabled_id).await;
    assert_eq!(
        exchange_response.status,
        StatusCode::FORBIDDEN,
        "{}",
        exchange_response.text
    );
    assert_eq!(detail(&exchange_response), "account disabled");

    let refresh_token = auth::create_mobile_refresh_token(SECRET, &disabled_id);
    let refresh_response = refresh(&h, &refresh_token).await;
    assert_eq!(
        refresh_response.status,
        StatusCode::FORBIDDEN,
        "{}",
        refresh_response.text
    );
    assert_eq!(detail(&refresh_response), "account disabled");
}

#[tokio::test]
async fn refresh_mints_a_fresh_access_token() {
    let Some(url) = database_url() else { return };
    let _guard = db_guard().await;
    let h = bootstrap(&url).await;

    let qr_response = qr(&h, Some(&h.owner_token)).await;
    let login_token = qr_response.json["loginToken"]
        .as_str()
        .expect("loginToken")
        .to_string();
    let exchanged = exchange(&h, &login_token, &h.owner_id).await;
    assert_eq!(exchanged.status, StatusCode::OK, "{}", exchanged.text);
    let first_access = exchanged.json["auth"]["accessToken"]
        .as_str()
        .expect("accessToken")
        .to_string();
    let refresh_token = exchanged.json["refreshToken"]
        .as_str()
        .expect("refreshToken")
        .to_string();

    // The JWT is `{exp, sub, kind}` with one-second expiry granularity, so a
    // refresh in the same second is byte-identical to the token it replaces —
    // wait one tick so "is this a *new* token" is a real question.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let refreshed = refresh(&h, &refresh_token).await;
    assert_eq!(refreshed.status, StatusCode::OK, "{}", refreshed.text);
    let fresh_access = refreshed.json["accessToken"]
        .as_str()
        .expect("accessToken")
        .to_string();
    assert_ne!(
        fresh_access, first_access,
        "refresh mints a new session token"
    );
    assert_eq!(refreshed.json["userId"], h.owner_id.as_str());
    assert_eq!(claim(&fresh_access, "kind"), json!("access"));

    let me = call(
        &h.app,
        Method::GET,
        "/api/v2/auth/me",
        Some(&fresh_access),
        None,
    )
    .await;
    assert_eq!(me.status, StatusCode::OK, "{}", me.text);
    assert_eq!(me.json["userId"], h.owner_id.as_str());

    // A garbage refresh token gets the same answer as a wrong-kind one.
    let garbage = refresh(&h, "nonsense").await;
    assert_eq!(garbage.status, StatusCode::UNAUTHORIZED);
    assert_eq!(detail(&garbage), INVALID_REFRESH);
}

#[tokio::test]
async fn access_login_and_refresh_tokens_do_not_interchange() {
    let Some(url) = database_url() else { return };
    let _guard = db_guard().await;
    let h = bootstrap(&url).await;

    let qr_response = qr(&h, Some(&h.owner_token)).await;
    let login_token = qr_response.json["loginToken"]
        .as_str()
        .expect("loginToken")
        .to_string();
    let exchanged = exchange(&h, &login_token, &h.owner_id).await;
    assert_eq!(exchanged.status, StatusCode::OK, "{}", exchanged.text);
    let refresh_token = exchanged.json["refreshToken"]
        .as_str()
        .expect("refreshToken")
        .to_string();

    // access -> refresh endpoint: 401, and the wording must not say "that was an
    // access token" — it is byte-identical to the garbage-token rejection.
    let access_as_refresh = refresh(&h, &h.owner_token).await;
    assert_eq!(access_as_refresh.status, StatusCode::UNAUTHORIZED);
    assert_eq!(detail(&access_as_refresh), INVALID_REFRESH);
    let garbage_refresh = refresh(&h, "nonsense").await;
    assert_eq!(detail(&garbage_refresh), detail(&access_as_refresh));

    // refresh -> AuthedUser: 401.
    let refresh_as_access = call(
        &h.app,
        Method::GET,
        "/api/v2/auth/me",
        Some(&refresh_token),
        None,
    )
    .await;
    assert_eq!(refresh_as_access.status, StatusCode::UNAUTHORIZED);
    assert_eq!(detail(&refresh_as_access), "invalid user access token");

    // mobile_login -> AuthedUser: 401 as well. This is the token closest to an
    // access token (same user, same secret) and it must not authenticate.
    let second_qr = qr(&h, Some(&h.owner_token)).await;
    let fresh_login_token = second_qr.json["loginToken"]
        .as_str()
        .expect("loginToken")
        .to_string();
    let login_as_access = call(
        &h.app,
        Method::GET,
        "/api/v2/auth/me",
        Some(&fresh_login_token),
        None,
    )
    .await;
    assert_eq!(login_as_access.status, StatusCode::UNAUTHORIZED);
    assert_eq!(detail(&login_as_access), "invalid user access token");

    // access -> exchange: 401 (and no stored row can match it either).
    let access_as_login = exchange(&h, &h.owner_token, &h.owner_id).await;
    assert_eq!(access_as_login.status, StatusCode::UNAUTHORIZED);
    assert_eq!(detail(&access_as_login), INVALID_LOGIN);
}

#[tokio::test]
async fn a_token_without_a_kind_claim_is_still_accepted_as_access() {
    let Some(url) = database_url() else { return };
    let _guard = db_guard().await;
    let h = bootstrap(&url).await;

    // Exactly the shape of every session minted before `kind` existed.
    let legacy = create_jwt(
        SECRET,
        &json!({ "sub": h.owner_id, "exp": farwork_hub::timeutil::unix_now() + 3600 }),
    );
    assert!(claim(&legacy, "kind").is_null());
    let me = call(&h.app, Method::GET, "/api/v2/auth/me", Some(&legacy), None).await;
    assert_eq!(me.status, StatusCode::OK, "{}", me.text);
    assert_eq!(me.json["userId"], h.owner_id.as_str());

    let projects = call(&h.app, Method::GET, "/api/v2/projects", Some(&legacy), None).await;
    assert_eq!(projects.status, StatusCode::OK, "{}", projects.text);

    // ... but nowhere else.
    let legacy_as_refresh = refresh(&h, &legacy).await;
    assert_eq!(legacy_as_refresh.status, StatusCode::UNAUTHORIZED);
    assert_eq!(detail(&legacy_as_refresh), INVALID_REFRESH);
    let legacy_as_login = exchange(&h, &legacy, &h.owner_id).await;
    assert_eq!(legacy_as_login.status, StatusCode::UNAUTHORIZED);
    assert_eq!(detail(&legacy_as_login), INVALID_LOGIN);
}

#[tokio::test]
async fn the_plaintext_login_token_never_reaches_the_database() {
    let Some(url) = database_url() else { return };
    let _guard = db_guard().await;
    let h = bootstrap(&url).await;

    let qr_response = qr(&h, Some(&h.owner_token)).await;
    let login_token = qr_response.json["loginToken"]
        .as_str()
        .expect("loginToken")
        .to_string();

    let stored: String = sqlx::query_scalar("select token_hash from mobile_login_tokens")
        .fetch_one(&h.pool)
        .await
        .expect("token_hash");
    assert_eq!(stored, sha256_hex(login_token.as_bytes()));
    assert_eq!(stored.len(), 64);
    assert!(!stored.contains(&login_token));

    // No column of the row may hold the token or any of its segments.
    let rows: String = sqlx::query_scalar(
        "select coalesce(string_agg(token_hash || '|' || user_id || '|' || created_at, E'\n'), '')
           from mobile_login_tokens",
    )
    .fetch_one(&h.pool)
    .await
    .expect("rows");
    assert!(!rows.is_empty());
    assert!(!rows.contains(&login_token));
    for segment in login_token.split('.') {
        assert!(
            !rows.contains(segment),
            "a token segment reached the database: {segment}"
        );
    }

    let by_plaintext: i64 =
        sqlx::query_scalar("select count(*) from mobile_login_tokens where token_hash = $1")
            .bind(&login_token)
            .fetch_one(&h.pool)
            .await
            .expect("lookup by plaintext");
    assert_eq!(by_plaintext, 0);
}

#[tokio::test]
async fn two_qr_codes_in_the_same_second_are_distinct_and_independent() {
    let Some(url) = database_url() else { return };
    let _guard = db_guard().await;
    let h = bootstrap(&url).await;

    // Mint until two consecutive codes share an `exp` claim: that is the
    // same-second case, and it is exactly the case that used to be byte-identical
    // and therefore violate the mobile_login_tokens primary key.
    let mut minted: Vec<String> = Vec::new();
    loop {
        let response = qr(&h, Some(&h.owner_token)).await;
        assert_eq!(
            response.status,
            StatusCode::CREATED,
            "every qr must succeed: {}",
            response.text
        );
        minted.push(
            response.json["loginToken"]
                .as_str()
                .expect("loginToken")
                .to_string(),
        );
        let n = minted.len();
        if n >= 2 && claim(&minted[n - 1], "exp") == claim(&minted[n - 2], "exp") {
            break;
        }
        assert!(
            n < 5,
            "the clock never repeated a second, cannot test the collision case"
        );
    }

    let n = minted.len();
    let distinct: std::collections::HashSet<&String> = minted.iter().collect();
    assert_eq!(
        distinct.len(),
        n,
        "every issuance must be a different token"
    );
    let last_two = &minted[n - 2..];
    assert_eq!(
        claim(&last_two[0], "exp"),
        claim(&last_two[1], "exp"),
        "the pair under test must be minted in the same second"
    );
    assert_ne!(last_two[0], last_two[1]);
    assert_ne!(claim(&last_two[0], "nonce"), claim(&last_two[1], "nonce"));

    // Both rows landed: the second insert did not collide on the primary key.
    let rows: i64 = sqlx::query_scalar("select count(*) from mobile_login_tokens")
        .fetch_one(&h.pool)
        .await
        .expect("count");
    assert_eq!(rows as usize, n);
    for token in &minted {
        let found: i64 =
            sqlx::query_scalar("select count(*) from mobile_login_tokens where token_hash = $1")
                .bind(sha256_hex(token.as_bytes()))
                .fetch_one(&h.pool)
                .await
                .expect("lookup");
        assert_eq!(found, 1, "each minted code owns exactly one row");
    }

    // Independent: spending one code does not disturb the other.
    let first = exchange(&h, &last_two[0], &h.owner_id).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.text);
    let second = exchange(&h, &last_two[1], &h.owner_id).await;
    assert_eq!(second.status, StatusCode::OK, "{}", second.text);
    assert_ne!(
        first.json["auth"]["accessToken"],
        Value::Null,
        "the first exchange minted a session"
    );
    let remaining: i64 = sqlx::query_scalar("select count(*) from mobile_login_tokens")
        .fetch_one(&h.pool)
        .await
        .expect("count");
    assert_eq!(remaining as usize, n - 2);
}

#[tokio::test]
async fn qr_is_forbidden_for_a_disabled_account() {
    let Some(url) = database_url() else { return };
    let _guard = db_guard().await;
    let h = bootstrap(&url).await;
    let disabled_id = insert_user(&h.pool, "mobile-qr-disabled@example.com", true).await;
    // A token minted before the account was disabled: still signed, still valid.
    let stale_access = auth::create_user_access_token(SECRET, &disabled_id);

    let response = qr(&h, Some(&stale_access)).await;
    assert_eq!(response.status, StatusCode::FORBIDDEN, "{}", response.text);
    assert_eq!(detail(&response), "account disabled");

    let rows: i64 = sqlx::query_scalar("select count(*) from mobile_login_tokens")
        .fetch_one(&h.pool)
        .await
        .expect("count");
    assert_eq!(rows, 0, "a forbidden qr must not leave a row behind");
}

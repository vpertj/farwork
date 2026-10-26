//! `GET /api/v2/meta` — the address the hub says it is reachable at.
//!
//! Deliberately database-free (a lazily connected pool): the endpoint answers
//! from configuration and request headers alone, so this suite cannot be broken
//! by another suite truncating the same test database.

use axum::body::to_bytes;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use farwork_hub::config::Config;
use farwork_hub::AppState;
use serde_json::Value;
use tower::ServiceExt as _;

const SECRET: &str = "meta-test-secret";
const UNUSED_DATABASE: &str = "postgres://unused@127.0.0.1:1/unused";

fn app(port: u16, public_url: Option<&str>) -> Router {
    let config = Config {
        database_url: UNUSED_DATABASE.to_string(),
        port,
        secret: SECRET.to_string(),
        setup_token: None,
        database_max_connections: 1,
        public_url: public_url.map(str::to_string),
        static_dir: None,
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy(UNUSED_DATABASE)
        .expect("lazy pool");
    farwork_hub::app(AppState::new(config, pool))
}

async fn get(app: &Router, headers: &[(&str, &str)]) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(Method::GET).uri("/api/v2/meta");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder.body(axum::body::Body::empty()).expect("request");
    let response = app.clone().oneshot(request).await.expect("infallible");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("body");
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

/// No `Authorization`, and `FARWORK_HUB_PUBLIC_URL` wins over the request.
#[tokio::test]
async fn meta_needs_no_credentials_and_reports_the_public_url() {
    // Already normalized by `Config::from_env` (see `config.rs` unit tests).
    let app = app(7300, Some("https://hub.example.com"));
    let (status, body) = get(&app, &[("host", "127.0.0.1:5173")]).await;

    assert_eq!(status, StatusCode::OK, "meta: {body}");
    assert_eq!(body["hubUrl"], "https://hub.example.com");
    assert_eq!(body["scheme"], "https");
    assert_eq!(body["port"], 443);
    assert_eq!(body["version"], farwork_hub::VERSION);
    assert!(
        !body["serverTime"].as_str().unwrap_or_default().is_empty(),
        "serverTime: {body}"
    );
}

/// The regression this endpoint exists for: under `vite dev` the page is 5173 and
/// the hub is 7300, so a Host header carrying 5173 must not become the hub's
/// port — `--hub http://localhost:5173` lands in `agent.json` and never connects.
#[tokio::test]
async fn meta_derives_the_listening_port_and_ignores_the_host_port() {
    let app = app(7300, None);
    let (status, body) = get(&app, &[("host", "127.0.0.1:5173")]).await;

    assert_eq!(status, StatusCode::OK, "meta: {body}");
    assert_eq!(body["hubUrl"], "http://127.0.0.1:7300");
    assert_eq!(body["port"], 7300);
    assert_eq!(body["scheme"], "http");
}

/// `X-Forwarded-Proto` moves the scheme and nothing else: the port still comes
/// from the listening config, and a junk value is ignored rather than echoed.
#[tokio::test]
async fn meta_takes_only_the_scheme_from_x_forwarded_proto() {
    let app = app(7500, None);
    let (status, body) = get(
        &app,
        &[
            ("host", "farwork.example.com:5173"),
            ("x-forwarded-proto", "https"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "meta: {body}");
    assert_eq!(body["hubUrl"], "https://farwork.example.com:7500");
    assert_eq!(body["scheme"], "https");
    assert_eq!(body["port"], 7500);

    let (_, junk) = get(
        &app,
        &[
            ("host", "farwork.example.com"),
            ("x-forwarded-proto", "gopher"),
        ],
    )
    .await;
    assert_eq!(junk["hubUrl"], "http://farwork.example.com:7500");
    assert_eq!(junk["scheme"], "http");
}

/// A public URL may carry its own port and path; neither is rewritten.
#[tokio::test]
async fn meta_keeps_the_port_and_path_of_a_public_url() {
    let app = app(7300, Some("http://127.0.0.1:8080/farwork"));
    let (status, body) = get(&app, &[("host", "127.0.0.1:5173")]).await;

    assert_eq!(status, StatusCode::OK, "meta: {body}");
    assert_eq!(body["hubUrl"], "http://127.0.0.1:8080/farwork");
    assert_eq!(body["port"], 8080);
}

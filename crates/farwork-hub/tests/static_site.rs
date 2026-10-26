//! The optional static surface: `FARWORK_HUB_STATIC_DIR` makes the hub serve the
//! built SPA itself.
//!
//! Database-free like `tests/install_sh.rs` (a lazily connected pool): the static
//! routes never touch Postgres, so no other suite can break them by truncating the
//! shared test database. The fixture lives under the repository's ignored
//! `target/` directory instead of `temp_dir()` so a failing run leaves the exact
//! tree that was served.

use std::path::{Path, PathBuf};

use axum::body::to_bytes;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::Router;
use farwork_hub::config::Config;
use farwork_hub::AppState;
use tower::ServiceExt as _;

const UNUSED_DATABASE: &str = "postgres://unused@127.0.0.1:1/unused";
/// Present only in the fixture's `index.html`: lets a test tell the SPA shell
/// apart from any other 200 the router might produce.
const SHELL_MARKER: &str = "spa-shell-marker";
const BUNDLE_PATH: &str = "/_app/immutable/entry/app.js";

fn app(static_dir: Option<PathBuf>) -> Router {
    let config = Config {
        database_url: UNUSED_DATABASE.to_string(),
        port: 7500,
        secret: "static-site-test-secret".to_string(),
        setup_token: None,
        database_max_connections: 1,
        public_url: None,
        static_dir,
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy(UNUSED_DATABASE)
        .expect("lazy pool");
    farwork_hub::app(AppState::new(config, pool))
}

/// A minimal adapter-static build: the shell plus one content-hashed bundle.
fn fixture(name: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/static-site-tests")
        .join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("_app/immutable/entry")).expect("create fixture directories");
    std::fs::write(
        root.join("index.html"),
        format!("<!doctype html>\n<div id=\"{SHELL_MARKER}\"></div>\n"),
    )
    .expect("write index.html");
    std::fs::write(
        root.join("_app/immutable/entry/app.js"),
        "console.log('bundle');\n",
    )
    .expect("write bundle");
    root
}

async fn get(app: Router, uri: &str) -> (StatusCode, HeaderMap, Vec<u8>) {
    let request = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(axum::body::Body::empty())
        .expect("request");
    let response = app.oneshot(request).await.expect("infallible");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");
    (status, headers, bytes.to_vec())
}

fn header_str(headers: &HeaderMap, name: header::HeaderName) -> String {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

fn text(body: &[u8]) -> String {
    String::from_utf8_lossy(body).to_string()
}

/// `/` is the SPA shell; a deep route is the same bytes, because the client
/// router owns everything after the first `#`.
#[tokio::test]
async fn the_shell_is_served_at_the_root_and_for_unknown_deep_routes() {
    let dir = fixture("shell");
    let index = std::fs::read(dir.join("index.html")).expect("read index.html");

    let (status, headers, body) = get(app(Some(dir.clone())), "/").await;
    assert_eq!(status, StatusCode::OK, "GET /");
    assert_eq!(body, index, "GET / serves index.html verbatim");
    assert!(text(&body).contains(SHELL_MARKER), "GET / is the SPA shell");
    assert!(
        header_str(&headers, header::CONTENT_TYPE).starts_with("text/html"),
        "GET / is HTML, not a download"
    );

    for uri in [
        "/some/deep/route",
        "/settings/profile",
        "/sessions/abc/terminal",
    ] {
        let (status, headers, body) = get(app(Some(dir.clone())), uri).await;
        assert_eq!(status, StatusCode::OK, "{uri} falls back to the shell");
        assert_eq!(body, index, "{uri} returns index.html verbatim");
        assert_eq!(
            header_str(&headers, header::CACHE_CONTROL),
            "no-cache",
            "{uri} must never be cached like an asset"
        );
    }
}

/// The shell names the current bundle hashes, so it revalidates; the hashed
/// bundles under `_app/immutable/` can be cached for a year.
#[tokio::test]
async fn cache_headers_follow_the_filename() {
    let dir = fixture("cache");

    let (status, headers, body) = get(app(Some(dir.clone())), BUNDLE_PATH).await;
    assert_eq!(status, StatusCode::OK, "the bundle is served");
    assert_eq!(text(&body), "console.log('bundle');\n");
    assert_eq!(
        header_str(&headers, header::CACHE_CONTROL),
        "public, max-age=31536000, immutable",
        "content-hashed assets never change under the same name"
    );

    let (status, headers, _) = get(app(Some(dir)), "/").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        header_str(&headers, header::CACHE_CONTROL),
        "no-cache",
        "a cached index.html would keep loading the previous release"
    );
}

/// The one thing the fallback must not swallow: an unknown API path is a 404
/// for the client, never 200 + the SPA shell (which would turn a typo in a fetch
/// URL into a login page and hide the error).
#[tokio::test]
async fn unknown_api_paths_stay_404_json() {
    let dir = fixture("api");
    for uri in [
        "/api/v2/unknown",
        "/api/v2/sessions/nope/nope",
        "/api",
        "/api/nope",
    ] {
        let (status, headers, body) = get(app(Some(dir.clone())), uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri} must stay 404");
        assert_eq!(
            header_str(&headers, header::CONTENT_TYPE),
            "application/json",
            "{uri} answers JSON, not HTML"
        );
        assert!(
            !text(&body).contains(SHELL_MARKER),
            "{uri} must not return the SPA shell"
        );
        let parsed: serde_json::Value = serde_json::from_slice(&body).expect("JSON body");
        assert!(
            parsed.get("detail").is_some(),
            "{uri} keeps the FastAPI-shaped detail body"
        );
    }
}

/// The routes that already lived at the root keep answering themselves.
#[tokio::test]
async fn healthz_and_install_sh_are_unaffected() {
    let dir = fixture("root-routes");

    let (status, _, body) = get(app(Some(dir.clone())), "/healthz").await;
    assert_eq!(status, StatusCode::OK);
    let parsed: serde_json::Value = serde_json::from_slice(&body).expect("JSON body");
    assert_eq!(parsed["status"], "ok");

    let (status, _, body) = get(app(Some(dir)), "/install.sh").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.starts_with(b"#!/bin/sh\n"),
        "/install.sh is still the embedded script, not the shell"
    );
    assert_eq!(
        body,
        std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../install.sh")).expect("repo file"),
        "/install.sh is byte-for-byte the repository copy"
    );
}

/// No `FARWORK_HUB_STATIC_DIR` = no static surface: the pre-existing behaviour
/// (`/` is 404) that the test suites and `pnpm dev` run against.
#[tokio::test]
async fn without_the_variable_the_root_is_a_404() {
    for uri in ["/", "/some/deep/route", "/_app/immutable/entry/app.js"] {
        let (status, _, body) = get(app(None), uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri} stays 404");
        assert!(body.is_empty() || !text(&body).contains(SHELL_MARKER));
    }

    let (status, _, body) = get(app(None), "/healthz").await;
    assert_eq!(status, StatusCode::OK, "the API surface is untouched");
    let parsed: serde_json::Value = serde_json::from_slice(&body).expect("JSON body");
    assert_eq!(parsed["status"], "ok");

    let (status, _, _) = get(app(None), "/api/v2/unknown").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

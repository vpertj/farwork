//! \`GET /install.sh\` — the installer the pairing page pipes into \`sh\`.
//!
//! Database-free like \`tests/meta.rs\` (lazily connected pool): the route answers
//! from a compile-time embedded file, so no other suite can break it by truncating
//! the shared test database.

use axum::body::to_bytes;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::Router;
use farwork_hub::config::Config;
use farwork_hub::AppState;
use tower::ServiceExt as _;

/// The repository file as compiled into the binary.
const EMBEDDED: &str = include_str!("../../../install.sh");
const REPO_FILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../install.sh");
const UNUSED_DATABASE: &str = "postgres://unused@127.0.0.1:1/unused";

fn app() -> Router {
    let config = Config {
        database_url: UNUSED_DATABASE.to_string(),
        port: 7500,
        secret: "install-sh-test-secret".to_string(),
        setup_token: None,
        database_max_connections: 1,
        public_url: None,
        static_dir: None,
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy(UNUSED_DATABASE)
        .expect("lazy pool");
    farwork_hub::app(AppState::new(config, pool))
}

/// No \`Authorization\` header: the pairing page fetches this before any token.
async fn get(uri: &str) -> (StatusCode, HeaderMap, Vec<u8>) {
    let request = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(axum::body::Body::empty())
        .expect("request");
    let response = app().oneshot(request).await.expect("infallible");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");
    (status, headers, bytes.to_vec())
}

fn header_str(headers: &HeaderMap, name: header::HeaderName) -> &str {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
}

/// The whole point: an unauthenticated GET returns the repository file verbatim,
/// so \`curl -fsSL <hub>/install.sh | sh\` runs the script that was tested.
#[tokio::test]
async fn install_sh_is_served_verbatim_without_credentials() {
    let (status, _, body) = get("/install.sh").await;
    assert_eq!(status, StatusCode::OK);

    let embedded = EMBEDDED.as_bytes();
    assert_eq!(body.len(), embedded.len(), "body length");
    assert_eq!(&body[..16], &embedded[..16], "first bytes");
    assert_eq!(
        &body[body.len() - 32..],
        &embedded[embedded.len() - 32..],
        "trailing bytes"
    );
    assert_eq!(body, embedded, "byte-for-byte identical to include_str!");

    // Readable from a pipe: POSIX shebang, trailing newline, and no use of the
    // script's own path (there is none when `sh` reads stdin).
    assert!(
        body.starts_with(b"#!/bin/sh\n"),
        "starts with a POSIX shebang"
    );
    assert!(body.ends_with(b"\n"), "ends with a newline");

    let on_disk = std::fs::read(REPO_FILE).expect("read the repository install.sh");
    assert_eq!(on_disk, body, "served bytes == repository install.sh");
    assert_eq!(on_disk, embedded, "embedded bytes == repository install.sh");

    let text = std::str::from_utf8(&body).expect("install.sh is UTF-8");
    assert!(
        !text.contains("$0") && !text.contains("BASH_SOURCE"),
        "the script must not depend on its own path: it is piped into sh with no argument"
    );
}

/// The headers make it a script to read, not a file to save or a stale copy.
#[tokio::test]
async fn install_sh_headers_say_shell_script_and_no_caching() {
    let (status, headers, _) = get("/install.sh").await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(
        header_str(&headers, header::CONTENT_TYPE),
        "text/x-shellscript; charset=utf-8"
    );
    assert_eq!(
        header_str(&headers, header::CONTENT_DISPOSITION),
        "inline; filename=\"install.sh\""
    );
    assert_eq!(header_str(&headers, header::CACHE_CONTROL), "no-cache");
}

/// One exact path, not a static file server: everything else still 404s.
#[tokio::test]
async fn install_sh_does_not_open_a_static_surface() {
    for uri in [
        "/install.sh.bak",
        "/install.sh/",
        "/install.sh/extra",
        "/install",
        "/README.md",
        "/Cargo.toml",
        "/api/v2/install.sh",
    ] {
        let (status, _, _) = get(uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri} must stay 404");
    }

    let (status, _, _) = get("/healthz").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the neighbouring root route still works"
    );
}

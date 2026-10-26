//! Optional static surface: with `FARWORK_HUB_STATIC_DIR` set, the hub serves the
//! built SPA (`apps/web/build`) itself, so one container holds the front end and
//! the API instead of pairing the hub with an nginx.
//!
//! Two rules shape the wiring:
//! - The route table keeps priority. This is registered as the *fallback* of the
//!   root router, so `/api/v2/*`, `/connector/ws`, `/sessions/{id}/ws`,
//!   `/dashboard/ws`, `/healthz` and `/install.sh` are matched before it runs and
//!   are never answered with HTML.
//! - Only paths *outside* `/api` may fall back to `index.html`. An unknown API
//!   path has to stay a 404: answering 200 + the SPA shell would turn a client
//!   bug into a login screen.
//!
//! Unset variable = no static surface at all (see `Config::static_dir`), which is
//! what the test suites and `pnpm dev` rely on.

use std::path::PathBuf;

use axum::extract::Request;
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde_json::json;
use tower_http::services::{ServeDir, ServeFile};

/// The built SPA to serve; unset means "no static surface".
pub const STATIC_DIR_ENV: &str = "FARWORK_HUB_STATIC_DIR";
/// adapter-static entry point, and the target of every SPA fallback.
pub const INDEX_FILE: &str = "index.html";
/// Vite puts content-hashed filenames under here, so the name *is* the version.
const IMMUTABLE_PREFIX: &str = "/_app/immutable/";
/// A year: a changed name is the only way this file changes.
const IMMUTABLE_CACHE: &str = "public, max-age=31536000, immutable";
/// `index.html` names the current bundle hashes, so a cached copy keeps loading
/// the previous release's assets after a deploy.
const REVALIDATE_CACHE: &str = "no-cache";

/// Serves `dir` and falls back to `dir/index.html` — the hash-routed SPA answers
/// deep links (`/some/deep/route`) with the shell and lets the client route it.
pub fn router(dir: PathBuf) -> Router<()> {
    let index = dir.join(INDEX_FILE);
    Router::new()
        .fallback_service(ServeDir::new(dir).fallback(ServeFile::new(index)))
        .layer(middleware::from_fn(cache_headers))
}

async fn cache_headers(request: Request, next: Next) -> Response {
    let path = request.uri().path().to_string();
    if path == "/api" || path.starts_with("/api/") {
        // Belt and braces: `/api/v2/*` is routed before this fallback runs, but a
        // path outside that nest must still fail instead of returning the shell.
        let method = request.method().clone();
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "detail": format!("no route for {method} {path}") })),
        )
            .into_response();
    }

    let cache = if path.starts_with(IMMUTABLE_PREFIX) {
        IMMUTABLE_CACHE
    } else {
        REVALIDATE_CACHE
    };
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    response
}

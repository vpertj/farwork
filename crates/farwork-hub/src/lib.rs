//! Farwork hub — the Rust replacement for the upstream Python `agent_server`.
//!
//! P0 scope: the `/api/v2` surface needed to run one real session end to end
//! (auth → connector → project → session → timeline → message → WS delta).
//! Anything outside that scope answers `501 not_implemented` instead of
//! pretending to work.

pub mod auth;
pub mod config;
pub mod db;
pub mod error;
pub mod ids;
pub mod models;
pub mod routes;
pub mod state;
pub mod timeline;
pub mod timeutil;

pub use config::Config;
pub use state::AppState;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .nest("/api/v2", routes::api())
        .layer(TraceLayer::new_for_http())
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .with_state(state)
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok", "version": VERSION }))
}

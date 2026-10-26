//! Farwork control-plane server: device registry plus terminal frame routing.

pub mod cli;
pub mod listen;
pub mod registry;
pub mod routes;
pub mod state;
pub mod ws;

pub use state::AppState;

use std::time::Duration;

use axum::http::{header, Method};
use axum::{routing::get, Router};
use tokio::net::TcpListener;
use tower_http::cors::{Any, CorsLayer};

/// Build the full HTTP/WebSocket surface.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(routes::health))
        .route("/api/devices", get(routes::devices))
        .route("/ws/connector", get(ws::connector))
        .route("/ws/client", get(ws::client))
        .layer(cors())
        .with_state(state)
}

/// The web build can point at any API base (`PUBLIC_FARWORK_API`), and the
/// Capacitor shell is cross-origin by nature, so the skeleton accepts every
/// origin. M2 replaces this with an explicit origin allowlist.
fn cors() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
        .expose_headers([header::CONTENT_TYPE])
        .max_age(Duration::from_secs(600))
}

/// Serve until the listener fails.
pub async fn serve(listener: TcpListener, state: AppState) -> std::io::Result<()> {
    axum::serve(listener, router(state)).await
}

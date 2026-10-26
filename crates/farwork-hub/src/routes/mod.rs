//! HTTP and WebSocket routing for the `/api/v2` namespace.

pub mod auth;
pub mod connectors;
pub mod extract;
pub mod projects;
pub mod sessions;
pub mod unsupported;
pub mod ws;

use axum::Router;

use crate::state::AppState;

pub fn api() -> Router<AppState> {
    Router::new()
        .merge(auth::router())
        .merge(connectors::router())
        .merge(projects::router())
        .merge(sessions::router())
        .merge(ws::router())
        .merge(unsupported::router())
}

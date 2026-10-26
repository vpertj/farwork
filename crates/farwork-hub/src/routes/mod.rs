//! HTTP and WebSocket routing for the `/api/v2` namespace.

pub mod auth;
pub mod connector_ws;
pub mod connectors;
pub mod extract;
pub mod fs;
pub mod notices;
pub mod projects;
pub mod sessions;
pub mod terminals;
pub mod unsupported;
pub mod ws;

use axum::Router;

use crate::state::AppState;

pub fn api() -> Router<AppState> {
    Router::new()
        .merge(auth::router())
        .merge(connector_ws::router())
        .merge(connectors::router())
        .merge(fs::router())
        .merge(notices::router())
        .merge(projects::router())
        .merge(sessions::router())
        .merge(terminals::router())
        .merge(ws::router())
        .merge(unsupported::router())
}

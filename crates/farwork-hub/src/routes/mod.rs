//! HTTP and WebSocket routing for the `/api/v2` namespace.

pub mod admin;
pub mod auth;
pub mod connector_ws;
pub mod connectors;
pub mod extract;
pub mod fs;
pub mod meta;
pub mod notices;
pub mod pairing;
pub mod projects;
pub mod runtime;
pub mod sessions;
pub mod shares;
pub mod terminals;
pub mod unsupported;
pub mod ws;

use axum::Router;

use crate::state::AppState;

pub fn api() -> Router<AppState> {
    Router::new()
        .merge(admin::router())
        .merge(auth::router())
        .merge(connector_ws::router())
        .merge(connectors::router())
        .merge(fs::router())
        // Third unauthenticated surface: the hub's own base address, which the
        // pairing page needs *before* it has (or needs) a token.
        .merge(meta::router())
        .merge(notices::router())
        .merge(pairing::router())
        // The second unauthenticated surface: the device claiming a pairing code
        // has no account yet, so the code itself is the credential.
        .merge(pairing::public_router())
        .merge(projects::router())
        .merge(runtime::router())
        .merge(sessions::router())
        .merge(shares::router())
        // The one unauthenticated surface under /api/v2: a public share link is
        // the visitor's only credential, so this router takes no AuthedUser.
        .merge(shares::public_router())
        .merge(terminals::router())
        .merge(ws::router())
        .merge(unsupported::router())
}

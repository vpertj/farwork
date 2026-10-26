//! Everything the upstream v2 API exposes that this P0 build does not
//! implement. These routes answer \`501 not_implemented\` with a machine-readable
//! body — never a fabricated success.

use axum::extract::{OriginalUri, Request};
use axum::routing::any;
use axum::Router;

use crate::error::ApiError;
use crate::state::AppState;

macro_rules! unimplemented_paths {
    ($($path:literal),* $(,)?) => {
        Router::new()$(.route($path, any(not_implemented)))*
    };
}

pub fn router() -> Router<AppState> {
    unimplemented_paths!(
        // Session runtime control. Interrupt, notices, notice responses and the
        // whole runtime/* group (capabilities / steer / commands / selections /
        // catalogs) are implemented (routes::sessions, routes::notices,
        // routes::runtime) and are not listed here.
        "/sessions/{session_id}/sync",
        "/sessions/{session_id}/takeover",
        // The session namespace has no entry left in this table: every
        // /sessions/{id}/files* and /sessions/{id}/terminal* path is implemented
        // (routes::fs, routes::terminals).
        // Sharing: POST/DELETE on the session's shares and the public read are
        // implemented (routes::shares), so only the account-level share list and
        // any share sub-path we have no contract for stay here. That is 2 fewer
        // paths than the 49 M3 left behind: 47.
        "/shares",
        "/shares/{*rest}",
        // Bulk session maintenance.
        "/sessions/archive",
        "/sessions/unarchive",
        "/sessions/read",
        // Device runtime management.
        "/connectors/{connector_id}/runtimes",
        "/connectors/{connector_id}/runtimes/{*rest}",
        "/connectors/{connector_id}/protocol/capabilities",
        "/connectors/{connector_id}/pairing",
        "/runtimes",
        "/runtimes/{*rest}",
        "/devices",
        "/devices/{*rest}",
        // OAuth, email codes, mobile QR login.
        "/auth/oauth/start",
        "/auth/oauth/callback",
        "/auth/oauth/finalize",
        "/auth/mobile-login/qr",
        "/auth/mobile-login/exchange",
        "/auth/mobile-login/refresh",
        "/auth/mobile-login/request",
        "/auth/mobile-login/status",
        "/auth/mobile-login/confirm",
        "/auth/email-code",
        "/auth/change-password",
        "/auth/me/email",
        "/auth/me/profile",
        "/auth/me/avatar",
        // Announcements, agents, usage. Instance administration is implemented
        // (routes::admin) and is not listed here — that is 2 fewer paths than
        // the 47 M3 left behind: 45. The session namespace then took it to 37:
        // /sessions/{id}/terminal, /terminal/{*rest}, /files, /files/{*rest} and
        // the four runtime/* entries are all implemented now. Pairing codes then
        // took it to 35: /pairing and /pairing/{*rest} are now routes::pairing
        // (start, claim, status). /connectors/{id}/pairing stays here — it is the
        // per-device runtime switch, a different thing that has no contract yet.
        "/announcements",
        "/announcements/{*rest}",
        "/agents",
        "/agents/{*rest}",
        "/usage",
        "/usage/{*rest}",
    )
    .fallback(unknown_path)
}

async fn not_implemented(request: Request) -> ApiError {
    let method = request.method().clone();
    let path = request
        .extensions()
        .get::<OriginalUri>()
        .map(|uri| uri.0.path().to_string())
        .unwrap_or_else(|| request.uri().path().to_string());
    ApiError::not_implemented(format!("{method} {path} is not implemented in this build"))
}

async fn unknown_path(request: Request) -> ApiError {
    let method = request.method().clone();
    let path = request
        .extensions()
        .get::<OriginalUri>()
        .map(|uri| uri.0.path().to_string())
        .unwrap_or_else(|| request.uri().path().to_string());
    ApiError::not_found(format!("no route for {method} {path}"))
}

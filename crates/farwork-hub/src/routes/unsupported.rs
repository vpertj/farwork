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
        // Session runtime control (approvals, steer, interrupt, commands).
        "/sessions/{session_id}/runtime/interrupt",
        "/sessions/{session_id}/runtime/steer",
        "/sessions/{session_id}/runtime/commands",
        "/sessions/{session_id}/runtime/notices",
        "/sessions/{session_id}/runtime/notices/{notice_id}/respond",
        "/sessions/{session_id}/runtime/selections",
        "/sessions/{session_id}/runtime/catalogs/{catalog}",
        "/sessions/{session_id}/sync",
        "/sessions/{session_id}/takeover",
        // Terminal + file browsing relay.
        "/sessions/{session_id}/terminal",
        "/sessions/{session_id}/terminal/{*rest}",
        "/sessions/{session_id}/files",
        "/sessions/{session_id}/files/{*rest}",
        // Sharing.
        "/sessions/{session_id}/shares",
        "/sessions/{session_id}/shares/{*rest}",
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
        // Instance administration, announcements, agents, usage.
        "/admin",
        "/admin/{*rest}",
        "/announcements",
        "/announcements/{*rest}",
        "/agents",
        "/agents/{*rest}",
        "/pairing",
        "/pairing/{*rest}",
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

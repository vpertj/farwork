//! \`GET /install.sh\`: the installer the pairing page tells the user to pipe into \`sh\`.
//!
//! The page builds that command as \`curl -fsSL <hubUrl>/install.sh | sh\` from the
//! \`hubUrl\` that \`GET /api/v2/meta\` reports. Serving the script from the hub is
//! what makes the command work at all: a hub that answers \`404\` here turns the one
//! instruction on the pairing page into a failure at the user's shell.
//!
//! Embedded rather than read from disk at startup: the hub ships as a single
//! binary that may run far away from the source tree, and the script it serves
//! must be the exact one this build was tested with. \`include_str!\` makes that a
//! compile-time fact, and the handler returns those bytes unchanged — no injected
//! defaults, no rewritten release URL — so \`sh -n\`, a checksum comparison against
//! the repository file, and the user's \`sh\` all see the same text.
//!
//! Unauthenticated, like \`/healthz\`: it is public install material, it says
//! nothing about any account, and the pairing page fetches it before the device
//! has a token. Registered at the root rather than under \`/api/v2\`, because the
//! root URL is the one the page hands out.

use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;

use crate::state::AppState;

/// Relative to this source file (\`crates/farwork-hub/src/\`), so it resolves at
/// compile time no matter which directory cargo was invoked from.
const INSTALL_SH: &str = include_str!("../../../install.sh");

pub fn router() -> Router<AppState> {
    Router::new().route("/install.sh", get(install_script))
}

async fn install_script() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "inline; filename=\"install.sh\"",
            ),
            // The script moves with the hub binary, so a cached copy could
            // outlive the release it installs.
            (header::CACHE_CONTROL, "no-cache"),
        ],
        INSTALL_SH,
    )
}

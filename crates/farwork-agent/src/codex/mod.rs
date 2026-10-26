//! codex app-server integration.
//!
//! Layering: 'transport' moves JSON-RPC bytes, 'mapper' decides what they mean
//! (timeline items, notices, state), 'session' pumps between mapper, transport
//! and the hub channel.

pub mod mapper;
pub mod session;
pub mod transport;

#[cfg(test)]
mod tests;

pub use mapper::{CodexMapper, CodexOut, MapperConfig, SessionSeed};
pub use session::{CodexSession, ExitReport, SessionCmd};
pub use transport::{CodexTransport, StdioTransport};

use crate::protocol::{CapabilityFlags, CatalogSet, ClientFrame, CommandEntry};

/// What this build can honestly claim about codex app-server.
///
/// The mapper drives `initialize` / `thread/start` / `turn/start` / `turn/interrupt`,
/// so an interrupt is real. There is no steer method, no command catalog and no
/// catalog method for models or permission profiles — so `steer`, `selections`
/// and `commands` are false, and both catalogs are empty. Inventing a model list
/// would hand the browser a picker whose choices mean nothing.
pub fn capabilities_frame(stream_id: &str) -> ClientFrame {
    ClientFrame::SessionCapabilities {
        stream_id: stream_id.to_string(),
        capabilities: CapabilityFlags {
            steer: false,
            interrupt: true,
            selections: false,
            commands: false,
        },
        catalogs: CatalogSet::default(),
        commands: Vec::<CommandEntry>::new(),
    }
}

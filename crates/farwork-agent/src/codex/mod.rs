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

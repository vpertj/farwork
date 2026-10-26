//! Farwork agent: the on-device connector.
//!
//! It dials out to the hub exactly once (websocket, connector token), serves
//! session.start / session.message / session.interrupt / notice.respond, and
//! drives a local 'codex app-server' child over stdio JSON-RPC. All protocol
//! frames live in 'protocol'; the codex mapping state machine lives in
//! 'codex::mapper' and is IO-free so it can be unit tested end to end.

pub mod backoff;
pub mod cli;
pub mod codex;
pub mod echo;
pub mod fsops;
pub mod hub;
pub mod identity;
pub mod ids;
pub mod protocol;
pub mod rpc;
pub mod timeutil;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

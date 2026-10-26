//! Wire contract shared by the Farwork server, the connector and the browser client.
//!
//! Pure data: serde types only, no I/O and no async runtime.

mod device;
mod rest;
mod wire;

pub use device::{DeviceInfo, DeviceView, Platform};
pub use rest::{DevicesResponse, HealthResponse};
pub use wire::{ClientToServer, ConnectorToServer, ServerToClient, ServerToConnector};

/// Version advertised by `/api/health` and by the connector `welcome` frame.
pub const PROTOCOL_VERSION: &str = "0.1.0";

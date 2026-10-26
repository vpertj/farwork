use serde::{Deserialize, Serialize};

use crate::device::DeviceView;
use crate::PROTOCOL_VERSION;

/// Body of `GET /api/health`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

impl HealthResponse {
    pub fn ok() -> Self {
        Self {
            status: "ok".to_string(),
            version: PROTOCOL_VERSION.to_string(),
        }
    }
}

impl Default for HealthResponse {
    fn default() -> Self {
        Self::ok()
    }
}

/// Body of `GET /api/devices`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DevicesResponse {
    pub devices: Vec<DeviceView>,
}

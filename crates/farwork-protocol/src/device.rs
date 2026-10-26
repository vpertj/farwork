use serde::{Deserialize, Serialize};

/// Host platform, serialized as `macos` | `linux` | `windows`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Macos,
    Linux,
    Windows,
}

impl Platform {
    /// Platform of the machine currently compiling/running this crate.
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Macos
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

/// Identity a connector announces in its `hello` frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub platform: Platform,
}

/// Device as rendered in `GET /api/devices` and in the `devices` push.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceView {
    pub id: String,
    pub name: String,
    pub platform: Platform,
    pub connected: bool,
    pub last_seen_ms: u64,
}

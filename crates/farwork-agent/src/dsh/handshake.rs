//! `initialize` and the feature flags that decide what this agent may claim.
//!
//! The flags are load-bearing. A bridge that does not advertise `approval` has no
//! approval prompts to relay, so this agent must not invent them: the browser
//! would show buttons that nothing on the other end listens to.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::rpc::{RpcClient, RpcError};

pub const PROTOCOL_VERSION: &str = "1.0";
pub const RUNTIME: &str = "dsh";
pub const CLIENT_NAME: &str = "farwork-agent";
/// projectionVersion 3 is the current event projection; 2 is still accepted by
/// the contract, and the version we subscribed is the one every batch must carry.
pub const DEFAULT_PROJECTION_VERSION: u64 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub runtime: String,
    #[serde(rename = "runtimeVersion")]
    pub runtime_version: String,
    #[serde(rename = "bridgeVersion")]
    pub bridge_version: String,
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
}

/// `features` from `initialize`. Every flag defaults to false: a field a bridge
/// does not mention is not a capability, it is an unknown.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Features {
    #[serde(default)]
    pub attachments: bool,
    #[serde(default, rename = "sessionDiscovery")]
    pub session_discovery: bool,
    #[serde(default, rename = "timelineSuffixRead")]
    pub timeline_suffix_read: bool,
    #[serde(default)]
    pub approval: bool,
    #[serde(default, rename = "userQuestions")]
    pub user_questions: bool,
    #[serde(default, rename = "readOnly")]
    pub read_only: bool,
    #[serde(default, rename = "snapshotPagination")]
    pub snapshot_pagination: bool,
    #[serde(default, rename = "syncMode")]
    pub sync_mode: Option<String>,
    #[serde(default, rename = "projectionVersion")]
    pub projection_version: Option<u64>,
}

impl Features {
    pub fn projection_version(&self) -> u64 {
        match self.projection_version {
            Some(version) if matches!(version, 2 | 3) => version,
            _ => DEFAULT_PROJECTION_VERSION,
        }
    }

    /// The only sync mode this agent implements is the ordered event stream.
    pub fn events(&self) -> bool {
        self.sync_mode.as_deref() == Some("events")
    }

    /// Approval buttons only exist when the bridge says it can carry them *and*
    /// the runtime is not in a read-only posture that would swallow the answer.
    pub fn approval_available(&self) -> bool {
        self.approval && !self.read_only
    }

    pub fn questions_available(&self) -> bool {
        self.user_questions && !self.read_only
    }
}

#[derive(Debug, Clone)]
pub struct InitializeParams<'a> {
    pub auth_token: &'a str,
    pub connector_id: &'a str,
    pub session_namespace: &'a str,
    pub client_version: &'a str,
}

impl InitializeParams<'_> {
    pub fn to_value(&self) -> Value {
        json!({
            "authToken": self.auth_token,
            "protocolVersion": PROTOCOL_VERSION,
            "runtime": RUNTIME,
            "connectorId": self.connector_id,
            "sessionNamespace": self.session_namespace,
            "clientInfo": { "name": CLIENT_NAME, "version": self.client_version },
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Handshake {
    pub identity: Identity,
    pub features: Features,
    pub storage: Value,
}

#[derive(Debug, thiserror::Error)]
pub enum HandshakeError {
    #[error("initialize was rejected: {0}")]
    Rejected(#[from] RpcError),
    #[error("{address} is not a DSH bridge (identity.runtime = {runtime:?})")]
    WrongRuntime { address: String, runtime: String },
    #[error("DSH bridge protocol {found} is incompatible with protocol {expected}")]
    VersionMismatch { found: String, expected: String },
    #[error("malformed initialize result: {reason}")]
    Invalid { reason: String },
}

#[derive(Debug, Deserialize)]
struct InitializeResult {
    identity: Identity,
    #[serde(default)]
    storage: Value,
    #[serde(default)]
    features: Features,
}

pub async fn initialize(
    client: &RpcClient,
    address: &str,
    params: &InitializeParams<'_>,
) -> Result<Handshake, HandshakeError> {
    let result = client
        .call("initialize", params.to_value())
        .await
        .map_err(HandshakeError::Rejected)?;
    interpret(&result, address)
}

/// Validate a raw `initialize` result. Split out so the rules are testable
/// without a socket: this is where "is this really a DSH bridge?" is decided.
pub fn interpret(result: &Value, address: &str) -> Result<Handshake, HandshakeError> {
    let parsed: InitializeResult =
        serde_json::from_value(result.clone()).map_err(|error| HandshakeError::Invalid {
            reason: error.to_string(),
        })?;
    if parsed.identity.runtime != RUNTIME {
        return Err(HandshakeError::WrongRuntime {
            address: address.to_string(),
            runtime: parsed.identity.runtime,
        });
    }
    if major(&parsed.identity.protocol_version) != major(PROTOCOL_VERSION) {
        return Err(HandshakeError::VersionMismatch {
            found: parsed.identity.protocol_version,
            expected: String::from(PROTOCOL_VERSION),
        });
    }
    Ok(Handshake {
        identity: parsed.identity,
        features: parsed.features,
        storage: parsed.storage,
    })
}

fn major(version: &str) -> Option<&str> {
    version.split('.').next().filter(|major| !major.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(runtime: &str, version: &str, features: Value) -> Value {
        json!({
            "identity": {
                "runtime": runtime,
                "runtimeVersion": "0.2.1-rc.2",
                "bridgeVersion": "2.1.0-desktop",
                "protocolVersion": version,
                "displayName": "DeepSeek Harness",
            },
            "storage": { "mode": "dsh-native", "sameSessionWriterLimit": 1 },
            "features": features,
        })
    }

    #[test]
    fn the_real_handshake_shape_is_accepted_and_its_features_are_read() {
        let handshake = interpret(
            &result(
                "dsh",
                "1.0",
                json!({
                    "attachments": true, "sessionDiscovery": true,
                    "timelineSuffixRead": false, "approval": true,
                    "userQuestions": true, "readOnly": false,
                    "snapshotPagination": true, "syncMode": "events",
                    "projectionVersion": 3,
                }),
            ),
            "127.0.0.1:62082",
        )
        .expect("handshake");
        assert_eq!(handshake.identity.runtime_version, "0.2.1-rc.2");
        assert!(handshake.features.approval);
        assert!(handshake.features.user_questions);
        assert!(handshake.features.events());
        assert_eq!(handshake.features.projection_version(), 3);
    }

    #[test]
    fn absent_features_read_as_unsupported_not_as_supported() {
        let handshake = interpret(&result("dsh", "1.0", json!({})), "ep").expect("handshake");
        assert!(!handshake.features.approval);
        assert!(!handshake.features.approval_available());
        assert!(!handshake.features.questions_available());
        assert!(!handshake.features.events());
        // No projectionVersion means the current one, which is what a 1.0 bridge
        // predating the field would be expected to speak.
        assert_eq!(handshake.features.projection_version(), 3);
    }

    #[test]
    fn a_read_only_runtime_does_not_get_approval_buttons() {
        let handshake = interpret(
            &result("dsh", "1.0", json!({ "approval": true, "readOnly": true })),
            "ep",
        )
        .expect("handshake");
        assert!(handshake.features.approval);
        assert!(!handshake.features.approval_available());
    }

    #[test]
    fn another_runtime_and_another_protocol_major_are_refused() {
        let error = interpret(&result("codex", "1.0", json!({})), "127.0.0.1:1").unwrap_err();
        assert!(error.to_string().contains("not a DSH bridge"));
        let error = interpret(&result("dsh", "2.0", json!({})), "ep").unwrap_err();
        assert!(matches!(error, HandshakeError::VersionMismatch { .. }));
        // A minor bump stays compatible: 1.x may add optional fields.
        assert!(interpret(&result("dsh", "1.7", json!({})), "ep").is_ok());
    }

    #[test]
    fn a_result_missing_the_identity_is_malformed() {
        let error = interpret(&json!({"features": {}}), "ep").unwrap_err();
        assert!(matches!(error, HandshakeError::Invalid { .. }));
    }
}

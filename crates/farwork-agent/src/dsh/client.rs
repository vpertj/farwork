//! Typed wrappers over the bridge methods this agent uses.
//!
//! Thin on purpose: the shapes below are the wire contract, and anything that
//! turns them into our own frames lives in `projection`.

use std::sync::Arc;

use serde_json::{json, Value};

use super::handshake::{self, Handshake, HandshakeError, InitializeParams};
use super::rpc::{RpcClient, RpcError};

/// `session.list` defaults to 100; the probe and the doctor use a handful.
pub const DEFAULT_LIST_LIMIT: u32 = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPair {
    pub session_id: String,
    pub external_session_id: String,
}

impl SessionPair {
    pub fn new(session_id: impl Into<String>, external_session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            external_session_id: external_session_id.into(),
        }
    }

    fn to_value(&self) -> Value {
        json!({
            "sessionId": self.session_id,
            "externalSessionId": self.external_session_id,
        })
    }
}

/// One catalog entry, kept to the two fields a picker needs.
///
/// Models and permissions label entries with `title`; Agent presets label theirs
/// with `name`, so both are accepted and neither is invented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    pub id: String,
    pub title: String,
}

impl CatalogEntry {
    pub fn from_value(value: &Value) -> Option<Self> {
        let id = value.get("id")?.as_str()?.trim();
        if id.is_empty() {
            return None;
        }
        let title = value
            .get("title")
            .or_else(|| value.get("name"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .unwrap_or(id);
        Some(Self {
            id: id.to_string(),
            title: title.to_string(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityEntry {
    pub capability_id: String,
    pub supported: bool,
    pub available: bool,
    pub allowed: bool,
}

impl CapabilityEntry {
    /// All three must hold: "supported" is what the build can do, "available" and
    /// "allowed" are what this session may do right now.
    pub fn effective(&self) -> bool {
        self.supported && self.available && self.allowed
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapabilitySet {
    pub revision: u64,
    pub entries: Vec<CapabilityEntry>,
}

impl CapabilitySet {
    pub fn parse(value: &Value) -> Self {
        let entries = value
            .get("capabilities")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        Some(CapabilityEntry {
                            capability_id: item.get("capabilityId")?.as_str()?.to_string(),
                            supported: item.get("supported")?.as_bool()?,
                            available: item.get("available")?.as_bool().unwrap_or(false),
                            allowed: item.get("allowed")?.as_bool().unwrap_or(false),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            revision: value.get("revision").and_then(Value::as_u64).unwrap_or(0),
            entries,
        }
    }

    pub fn has(&self, capability_id: &str) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.capability_id == capability_id && entry.effective())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscribeResult {
    pub stream_id: String,
    pub projection_version: u64,
    pub checkpoint_version: Option<u64>,
    pub upload_progress_version: Option<u64>,
}

impl SubscribeResult {
    pub fn parse(value: &Value) -> Result<Self, RpcError> {
        let malformed = |reason: &str| RpcError::Bridge {
            code: -32603,
            data_code: Some(String::from("MALFORMED_SUBSCRIBE")),
            retryable: false,
            message: format!("runtime.sync.subscribe returned no {reason}"),
        };
        let stream_id = value
            .get("streamId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| malformed("streamId"))?;
        let projection_version = value
            .get("projectionVersion")
            .and_then(Value::as_u64)
            .ok_or_else(|| malformed("projectionVersion"))?;
        Ok(Self {
            stream_id,
            projection_version,
            checkpoint_version: value.get("checkpointVersion").and_then(Value::as_u64),
            upload_progress_version: value.get("uploadProgressVersion").and_then(Value::as_u64),
        })
    }
}

/// `clientMessageId` for one logical send.
///
/// The bridge dedups on this id, so a retry of the same message must reuse it and
/// a *different* message must not: reusing an id across two different texts would
/// silently drop the second one.
#[derive(Debug, Default)]
pub struct MessageIds {
    last: Option<(String, String)>,
}

impl MessageIds {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn resolve(&mut self, supplied: Option<&str>, text: &str) -> String {
        if let Some(supplied) = supplied.map(str::trim).filter(|id| !id.is_empty()) {
            self.last = Some((text.to_string(), supplied.to_string()));
            return supplied.to_string();
        }
        if let Some((last_text, last_id)) = &self.last {
            if last_text == text {
                return last_id.clone();
            }
        }
        let id = format!("cmid_{}", uuid::Uuid::new_v4().simple());
        self.last = Some((text.to_string(), id.clone()));
        id
    }
}

/// Params of `session.createAndStart`.
///
/// `selections` and `agentPreset` are omitted entirely when nothing was
/// configured: the bridge then applies its own defaults, and an empty object
/// would be a different request. `agentPreset` is top level, not a selection —
/// DSH refuses `createAndStart` without an Agent mode of its own choosing.
pub fn create_and_start_params(
    session_id: &str,
    content: &str,
    client_message_id: &str,
    cwd: &str,
    selections: Option<&Value>,
    agent_preset: Option<&str>,
) -> Value {
    let mut params = json!({
        "sessionId": session_id,
        "content": content,
        "clientMessageId": client_message_id,
        "cwd": cwd,
    });
    if let Some(map) = params.as_object_mut() {
        if let Some(selections) = selections {
            map.insert(String::from("selections"), selections.clone());
        }
        if let Some(agent_preset) = agent_preset {
            map.insert(String::from("agentPreset"), json!(agent_preset));
        }
    }
    params
}

/// Params of `runtime.sync.subscribe`.
///
/// `checkpointVersion: 1` is what makes the bridge use durable checkpoints at all:
/// without it no `checkpoint.*` operation ever arrives and every connection is a
/// full inventory replay.
pub fn subscribe_params(
    subscription_id: &str,
    checkpoint_version: u64,
    upload_progress_version: u64,
) -> Value {
    json!({
        "subscriptionId": subscription_id,
        "checkpointVersion": checkpoint_version,
        "uploadProgressVersion": upload_progress_version,
    })
}

/// A fresh subscription name per `runtime.sync.subscribe`.
///
/// The bridge scopes batches, acks and checkpoint calls to it, so a resubscribe
/// has to look like a new subscription rather than the old one continuing.
pub fn subscription_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

pub struct DshClient {
    rpc: Arc<RpcClient>,
}

impl DshClient {
    pub fn new(rpc: Arc<RpcClient>) -> Self {
        Self { rpc }
    }

    pub fn rpc(&self) -> &Arc<RpcClient> {
        &self.rpc
    }

    pub async fn initialize(
        &self,
        address: &str,
        params: &InitializeParams<'_>,
    ) -> Result<Handshake, HandshakeError> {
        handshake::initialize(&self.rpc, address, params).await
    }

    /// Read-only: the discovery probe the doctor runs before anything is started.
    pub async fn list_sessions(&self, limit: u32) -> Result<Vec<Value>, RpcError> {
        let result = self
            .rpc
            .call("session.list", json!({ "limit": limit }))
            .await?;
        Ok(result
            .get("sessions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    pub async fn list_models(&self) -> Result<Vec<CatalogEntry>, RpcError> {
        self.catalog("catalog.listModels", "models").await
    }

    pub async fn list_permissions(&self) -> Result<Vec<CatalogEntry>, RpcError> {
        self.catalog("catalog.listPermissions", "permissions").await
    }

    /// The Agent modes (presets) the bridge offers; ids are bare words.
    pub async fn list_agent_presets(&self) -> Result<Vec<CatalogEntry>, RpcError> {
        self.catalog("catalog.listAgentPresets", "presets").await
    }

    async fn catalog(&self, method: &str, key: &str) -> Result<Vec<CatalogEntry>, RpcError> {
        let result = self.rpc.call(method, json!({})).await?;
        Ok(result
            .get(key)
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(CatalogEntry::from_value).collect())
            .unwrap_or_default())
    }

    pub async fn get_capabilities(&self, pair: &SessionPair) -> Result<CapabilitySet, RpcError> {
        let result = self
            .rpc
            .call("session.getCapabilities", pair.to_value())
            .await?;
        Ok(CapabilitySet::parse(&result))
    }

    pub async fn get_state(&self, pair: &SessionPair) -> Result<Value, RpcError> {
        self.rpc.call("session.getState", pair.to_value()).await
    }

    pub async fn create_and_start(
        &self,
        session_id: &str,
        content: &str,
        client_message_id: &str,
        cwd: &str,
        selections: Option<&Value>,
        agent_preset: Option<&str>,
    ) -> Result<Value, RpcError> {
        self.rpc
            .call(
                "session.createAndStart",
                create_and_start_params(
                    session_id,
                    content,
                    client_message_id,
                    cwd,
                    selections,
                    agent_preset,
                ),
            )
            .await
    }

    pub async fn start_turn(
        &self,
        pair: &SessionPair,
        content: &str,
        client_message_id: &str,
    ) -> Result<Value, RpcError> {
        let mut params = pair.to_value();
        if let Some(map) = params.as_object_mut() {
            map.insert(String::from("content"), json!(content));
            map.insert(String::from("clientMessageId"), json!(client_message_id));
        }
        self.rpc.call("session.startTurn", params).await
    }

    pub async fn steer(
        &self,
        pair: &SessionPair,
        content: &str,
        client_message_id: &str,
    ) -> Result<Value, RpcError> {
        let mut params = pair.to_value();
        if let Some(map) = params.as_object_mut() {
            map.insert(String::from("content"), json!(content));
            map.insert(String::from("clientMessageId"), json!(client_message_id));
        }
        self.rpc.call("session.steer", params).await
    }

    pub async fn interrupt(&self, pair: &SessionPair) -> Result<Value, RpcError> {
        self.rpc.call("session.interrupt", pair.to_value()).await
    }

    pub async fn update_selections(
        &self,
        pair: &SessionPair,
        selections: Value,
    ) -> Result<Value, RpcError> {
        let mut params = pair.to_value();
        if let Some(map) = params.as_object_mut() {
            map.insert(String::from("selections"), selections);
        }
        self.rpc.call("session.updateSelections", params).await
    }

    pub async fn respond_interaction(
        &self,
        pair: &SessionPair,
        notice_id: &str,
        action_id: &str,
        input: Option<Value>,
    ) -> Result<Value, RpcError> {
        let mut params = pair.to_value();
        if let Some(map) = params.as_object_mut() {
            map.insert(String::from("noticeId"), json!(notice_id));
            map.insert(String::from("actionId"), json!(action_id));
            if let Some(input) = input {
                map.insert(String::from("inputData"), input);
            }
        }
        self.rpc.call("session.respondInteraction", params).await
    }

    pub async fn subscribe(
        &self,
        checkpoint_version: u64,
        upload_progress_version: u64,
    ) -> Result<SubscribeResult, RpcError> {
        let result = self
            .rpc
            .call(
                "runtime.sync.subscribe",
                subscribe_params(
                    &subscription_id(),
                    checkpoint_version,
                    upload_progress_version,
                ),
            )
            .await?;
        SubscribeResult::parse(&result)
    }

    pub async fn ack(
        &self,
        stream_id: &str,
        batch_seq: u64,
        checkpoint: Option<Value>,
    ) -> Result<Value, RpcError> {
        let mut params = json!({ "streamId": stream_id, "batchSeq": batch_seq });
        if let (Some(checkpoint), Some(map)) = (checkpoint, params.as_object_mut()) {
            map.insert(String::from("checkpoint"), checkpoint);
        }
        self.rpc.call("runtime.sync.ack", params).await
    }

    pub async fn unsubscribe(&self) -> Result<Value, RpcError> {
        self.rpc.call("runtime.sync.unsubscribe", json!({})).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogs_keep_ids_titles_and_never_invent_an_entry() {
        let entries: Vec<CatalogEntry> = json!([
            { "id": "dsh:model:a", "title": "DeepSeek-V4-Pro", "selectionId": "x" },
            { "id": "dsh:model:b" },
            { "title": "no id" },
            { "id": "   " },
        ])
        .as_array()
        .expect("array")
        .iter()
        .filter_map(CatalogEntry::from_value)
        .collect();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].title, "DeepSeek-V4-Pro");
        // Agent presets label their entries with `name` instead of `title`.
        let presets: Vec<CatalogEntry> = json!([{ "id": "ptc", "name": "ptc" }])
            .as_array()
            .expect("array")
            .iter()
            .filter_map(CatalogEntry::from_value)
            .collect();
        assert_eq!(presets[0].title, "ptc");
        // A missing title falls back to the id: a picker entry with no label is
        // still clickable, an invented label is not.
        assert_eq!(entries[1].title, "dsh:model:b");
    }

    #[test]
    fn create_and_start_omits_selections_unless_they_were_configured() {
        let params = create_and_start_params("sess_1", "hi", "cm_1", "/w", None, None);
        assert_eq!(params["sessionId"], "sess_1");
        assert_eq!(params["cwd"], "/w");
        assert!(
            params.get("selections").is_none(),
            "an unspecified selection must not send the key at all: {params}"
        );

        let selections = json!({ "permission": "dsh:permission:cmVhZC1vbmx5" });
        let params = create_and_start_params("sess_1", "hi", "cm_1", "/w", Some(&selections), None);
        assert_eq!(params["selections"], selections);
    }

    #[test]
    fn an_agent_preset_is_a_top_level_param_and_absent_when_unset() {
        let params = create_and_start_params("sess_1", "hi", "cm_1", "/w", None, Some("ptc"));
        assert_eq!(params["agentPreset"], json!("ptc"));
        assert!(
            params.get("selections").is_none(),
            "a preset is not a selection: {params}"
        );

        // Nothing configured: the key must be absent, not null — the bridge then
        // keeps its own default mode instead of reading an explicit non-choice.
        let params = create_and_start_params("sess_1", "hi", "cm_1", "/w", None, None);
        assert!(params.get("agentPreset").is_none(), "{params}");
    }

    #[test]
    fn a_capability_needs_supported_available_and_allowed() {
        let set = CapabilitySet::parse(&json!({
            "runtime": "dsh",
            "revision": 7,
            "capabilities": [
                { "capabilityId": "session.interrupt", "supported": true, "available": true, "allowed": true },
                { "capabilityId": "session.steer", "supported": true, "available": true, "allowed": false },
                { "capabilityId": "session.interaction.approval", "supported": false, "available": true, "allowed": true },
            ],
        }));
        assert_eq!(set.revision, 7);
        assert!(set.has("session.interrupt"));
        assert!(
            !set.has("session.steer"),
            "allowed=false is not a capability"
        );
        assert!(!set.has("session.interaction.approval"));
        assert!(!CapabilitySet::default().has("session.interrupt"));
    }

    #[test]
    fn a_subscribe_result_without_a_stream_is_an_error_not_an_empty_stream() {
        let error = SubscribeResult::parse(&json!({ "projectionVersion": 3 })).unwrap_err();
        assert!(error.message().contains("streamId"));
        let parsed = SubscribeResult::parse(&json!({
            "streamId": "str_1", "projectionVersion": 3, "checkpointVersion": 1,
        }))
        .expect("subscribe");
        assert_eq!(parsed.stream_id, "str_1");
        assert_eq!(parsed.projection_version, 3);
        assert_eq!(parsed.checkpoint_version, Some(1));
        assert_eq!(parsed.upload_progress_version, None);
    }

    #[test]
    fn a_subscribe_names_its_subscription_and_asks_for_durable_checkpoints() {
        let params = subscribe_params("abc123", 1, 1);
        assert_eq!(params["subscriptionId"], json!("abc123"));
        // Without checkpointVersion the bridge never sends a checkpoint operation,
        // which is why every connection used to replay the entire inventory.
        assert_eq!(params["checkpointVersion"], json!(1));
        assert_eq!(params["uploadProgressVersion"], json!(1));

        // 32 hex characters, the shape of the reference connector's uuid4().hex, and
        // a new one per subscribe: a retry must not look like the old subscription
        // continuing.
        let first = subscription_id();
        assert_eq!(first.len(), 32);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()), "{first}");
        assert_ne!(first, subscription_id());
    }

    #[test]
    fn a_supplied_client_message_id_is_reused_and_a_new_text_gets_a_new_one() {
        let mut ids = MessageIds::new();
        // The hub's id is authoritative: a retry of the same frame keeps it.
        let first = ids.resolve(Some("cm_1"), "go");
        assert_eq!(first, "cm_1");
        assert_eq!(ids.resolve(Some("cm_1"), "go"), "cm_1");

        // Without a supplied id, a local retry of the same text reuses the mint.
        let mut ids = MessageIds::new();
        let minted = ids.resolve(None, "continue");
        assert!(minted.starts_with("cmid_"));
        assert_eq!(ids.resolve(None, "continue"), minted);
        // A different text must not inherit the previous id: the bridge would
        // treat it as the same message and drop it.
        let other = ids.resolve(None, "something else");
        assert_ne!(other, minted);
    }
}

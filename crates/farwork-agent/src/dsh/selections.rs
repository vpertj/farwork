//! Turning `--dsh-permission` / `--dsh-model` / `--dsh-preset` into what
//! `session.createAndStart` takes.
//!
//! The bridge speaks opaque IDs (`dsh:permission:<base64url(preset)>`), not the
//! names a person reads off a picker. This module is the one place that crosses
//! between the two, and it never guesses: a short name that matches nothing — or
//! more than one catalog entry — is a startup error listing the real choices,
//! because silently running under the default permission is exactly the kind of
//! downgrade the operator would never notice.

use serde_json::{json, Map, Value};
use tokio::time::timeout;

use super::client::{CatalogEntry, DshClient};
use super::discovery::Endpoint;
use super::handshake::InitializeParams;
use super::rpc;
use super::CONNECT_TIMEOUT;

pub const PERMISSION_PREFIX: &str = "dsh:permission:";
pub const MODEL_PREFIX: &str = "dsh:model:";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Permission,
    Model,
    Preset,
}

impl Kind {
    pub fn flag(self) -> &'static str {
        match self {
            Self::Permission => "--dsh-permission",
            Self::Model => "--dsh-model",
            Self::Preset => "--dsh-preset",
        }
    }

    /// A value carrying this prefix is already the bridge's own vocabulary.
    /// Preset ids carry no prefix (the bridge returns bare words like `ptc`), so
    /// every preset value is checked against the catalog.
    pub fn prefix(self) -> Option<&'static str> {
        match self {
            Self::Permission => Some(PERMISSION_PREFIX),
            Self::Model => Some(MODEL_PREFIX),
            Self::Preset => None,
        }
    }

    pub fn catalog(self) -> &'static str {
        match self {
            Self::Permission => "catalog.listPermissions",
            Self::Model => "catalog.listModels",
            Self::Preset => "catalog.listAgentPresets",
        }
    }
}

/// Everything the operator configured for `session.createAndStart`.
///
/// `permission` and `model` become its `selections` object. `preset` does not:
/// the bridge takes the Agent mode as a top-level `agentPreset`, so it is carried
/// here — one place resolved it — but never lands inside `selections`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selections {
    pub permission: Option<String>,
    pub model: Option<String>,
    pub preset: Option<String>,
}

impl Selections {
    pub fn is_empty(&self) -> bool {
        self.permission.is_none() && self.model.is_none() && self.preset.is_none()
    }

    /// `None` when nothing was configured: the bridge's own defaults must win,
    /// and an empty `selections` object is a different (and invalid) request.
    pub fn to_value(&self) -> Option<Value> {
        if self.permission.is_none() && self.model.is_none() {
            return None;
        }
        let mut map = Map::new();
        if let Some(permission) = &self.permission {
            map.insert(String::from("permission"), json!(permission));
        }
        if let Some(model) = &self.model {
            map.insert(String::from("model"), json!(model));
        }
        Some(Value::Object(map))
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SelectionError {
    #[error("{flag} {value:?} matches no entry in {catalog}; available: {available}")]
    NotFound {
        flag: &'static str,
        value: String,
        catalog: &'static str,
        available: String,
    },
    #[error(
        "{flag} {value:?} is ambiguous in {catalog} (matches {matches}); available: {available}"
    )]
    Ambiguous {
        flag: &'static str,
        value: String,
        catalog: &'static str,
        matches: String,
        available: String,
    },
    #[error("{catalog} could not be read from the DSH bridge: {message}")]
    Catalog {
        catalog: &'static str,
        message: String,
    },
    #[error("cannot use the DSH bridge at {address}: {message}")]
    Bridge { address: String, message: String },
}

/// Resolve one flag value against a catalog.
///
/// A full selection ID is echoed verbatim (the bridge stays the authority on
/// which IDs exist); anything else must match exactly one entry by display name
/// or by the ID's last segment, case-insensitively.
pub fn resolve(kind: Kind, raw: &str, entries: &[CatalogEntry]) -> Result<String, SelectionError> {
    let value = raw.trim();
    if let Some(prefix) = kind.prefix() {
        if value.starts_with(prefix) && value.len() > prefix.len() {
            return Ok(value.to_string());
        }
    }
    let needle = value.to_lowercase();
    let mut matched: Vec<&CatalogEntry> = Vec::new();
    if !needle.is_empty() {
        for entry in entries {
            let by_title = entry.title.to_lowercase() == needle;
            let by_id = entry.id.to_lowercase() == needle;
            let by_suffix = id_suffix(&entry.id).to_lowercase() == needle;
            if (by_title || by_id || by_suffix) && !matched.iter().any(|seen| seen.id == entry.id) {
                matched.push(entry);
            }
        }
    }
    match matched.as_slice() {
        [only] => Ok(only.id.clone()),
        [] => Err(SelectionError::NotFound {
            flag: kind.flag(),
            value: value.to_string(),
            catalog: kind.catalog(),
            available: available(entries),
        }),
        many => Err(SelectionError::Ambiguous {
            flag: kind.flag(),
            value: value.to_string(),
            catalog: kind.catalog(),
            matches: many
                .iter()
                .map(|entry| entry.id.clone())
                .collect::<Vec<_>>()
                .join(", "),
            available: available(entries),
        }),
    }
}

fn id_suffix(id: &str) -> &str {
    id.rsplit(':').next().unwrap_or(id)
}

fn available(entries: &[CatalogEntry]) -> String {
    if entries.is_empty() {
        return String::from("<the bridge returned no entries>");
    }
    entries
        .iter()
        .map(|entry| format!("{} ({})", entry.id, entry.title))
        .collect::<Vec<_>>()
        .join(", ")
}

fn needs_catalog(kind: Kind, raw: &str) -> bool {
    let Some(prefix) = kind.prefix() else {
        return true;
    };
    let value = raw.trim();
    !(value.starts_with(prefix) && value.len() > prefix.len())
}

/// Resolve both flags at startup, against the bridge the connector will use.
///
/// The bridge is only dialled when a value actually needs a catalog: a full
/// selection ID, or no flag at all, leaves startup as cheap as it was before.
pub async fn resolve_remote(
    endpoint: &Endpoint,
    connector_id: &str,
    session_namespace: &str,
    client_version: &str,
    permission: Option<&str>,
    model: Option<&str>,
    preset: Option<&str>,
) -> Result<Selections, SelectionError> {
    let permission_needs_catalog = permission.map(|raw| needs_catalog(Kind::Permission, raw));
    let model_needs_catalog = model.map(|raw| needs_catalog(Kind::Model, raw));
    let preset_needs_catalog = preset.is_some();
    if permission_needs_catalog != Some(true)
        && model_needs_catalog != Some(true)
        && !preset_needs_catalog
    {
        return Ok(Selections {
            permission: permission.map(|raw| raw.trim().to_string()),
            model: model.map(|raw| raw.trim().to_string()),
            preset: None,
        });
    }

    let address = endpoint.address();
    let connection = timeout(CONNECT_TIMEOUT, rpc::dial(&address))
        .await
        .map_err(|_| SelectionError::Bridge {
            address: address.clone(),
            message: String::from("connecting timed out"),
        })?
        .map_err(|error| SelectionError::Bridge {
            address: address.clone(),
            message: error.to_string(),
        })?;
    let client = DshClient::new(connection.client.clone());
    let params = InitializeParams {
        auth_token: &endpoint.token,
        connector_id,
        session_namespace,
        client_version,
    };
    let resolved = async {
        client
            .initialize(&address, &params)
            .await
            .map_err(|error| SelectionError::Bridge {
                address: address.clone(),
                message: format!("initialize failed: {error}"),
            })?;
        let mut selections = Selections::default();
        if let Some(raw) = permission {
            selections.permission = Some(match permission_needs_catalog {
                Some(true) => {
                    let entries = client.list_permissions().await.map_err(|error| {
                        SelectionError::Catalog {
                            catalog: Kind::Permission.catalog(),
                            message: error.message(),
                        }
                    })?;
                    resolve(Kind::Permission, raw, &entries)?
                }
                _ => raw.trim().to_string(),
            });
        }
        if let Some(raw) = model {
            selections.model =
                Some(match model_needs_catalog {
                    Some(true) => {
                        let entries = client.list_models().await.map_err(|error| {
                            SelectionError::Catalog {
                                catalog: Kind::Model.catalog(),
                                message: error.message(),
                            }
                        })?;
                        resolve(Kind::Model, raw, &entries)?
                    }
                    _ => raw.trim().to_string(),
                });
        }
        if let Some(raw) = preset {
            let entries =
                client
                    .list_agent_presets()
                    .await
                    .map_err(|error| SelectionError::Catalog {
                        catalog: Kind::Preset.catalog(),
                        message: error.message(),
                    })?;
            selections.preset = Some(resolve(Kind::Preset, raw, &entries)?);
        }
        Ok(selections)
    }
    .await;
    client.rpc().close().await;
    resolved
}

/// Read the Agent-mode catalog only to warn that DSH may require one.
///
/// Advisory by construction: every failure returns `None` after a warning. A hint
/// able to refuse startup would turn a diagnostic into an outage.
pub async fn preset_hint_remote(
    endpoint: &Endpoint,
    connector_id: &str,
    session_namespace: &str,
    client_version: &str,
) -> Option<String> {
    let address = endpoint.address();
    let connection = match timeout(CONNECT_TIMEOUT, rpc::dial(&address)).await {
        Ok(Ok(connection)) => connection,
        Ok(Err(error)) => {
            tracing::warn!(%error, "no agent preset hint: DSH bridge dial failed");
            return None;
        }
        Err(_) => {
            tracing::warn!(address = %address, "no agent preset hint: connecting timed out");
            return None;
        }
    };
    let client = DshClient::new(connection.client.clone());
    let params = InitializeParams {
        auth_token: &endpoint.token,
        connector_id,
        session_namespace,
        client_version,
    };
    let hint = match client.initialize(&address, &params).await {
        Ok(_) => match client.list_agent_presets().await {
            Ok(entries) => preset_hint(None, &entries),
            Err(error) => {
                tracing::warn!(%error, "no agent preset hint: catalog.listAgentPresets failed");
                None
            }
        },
        Err(error) => {
            tracing::warn!(%error, "no agent preset hint: initialize failed");
            None
        }
    };
    client.rpc().close().await;
    hint
}

/// What to say when `--dsh-preset` was not given.
///
/// DSH refuses `createAndStart` until an Agent mode is chosen, but which mode to
/// run is the operator's decision: this lists what the bridge offers and never
/// selects one. An empty catalog means the bridge has no presets, so there is
/// nothing to warn about.
pub fn preset_hint(preset: Option<&str>, entries: &[CatalogEntry]) -> Option<String> {
    if preset.is_some() || entries.is_empty() {
        return None;
    }
    Some(format!(
        "--runtime dsh without --dsh-preset: DSH may refuse to create a session until an Agent mode is chosen; available presets: {}. Nothing was chosen for you",
        available(entries)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog(items: &[(&str, &str)]) -> Vec<CatalogEntry> {
        items
            .iter()
            .map(|(id, title)| CatalogEntry {
                id: (*id).to_string(),
                title: (*title).to_string(),
            })
            .collect()
    }

    /// The shape a real bridge returns, trimmed to the two fields we keep.
    fn permissions() -> Vec<CatalogEntry> {
        catalog(&[
            ("dsh:permission:cmVhZC1vbmx5", "read-only"),
            ("dsh:permission:d29ya3NwYWNlLXdyaXRl", "workspace-write"),
            (
                "dsh:permission:ZGFuZ2VyLWZ1bGwtYWNjZXNz",
                "danger-full-access",
            ),
        ])
    }

    #[test]
    fn a_short_name_resolves_to_the_catalogs_real_selection_id() {
        let entries = permissions();
        assert_eq!(
            resolve(Kind::Permission, "read-only", &entries).expect("hit"),
            "dsh:permission:cmVhZC1vbmx5"
        );
        // Case-insensitive, and the surrounding whitespace is not the name.
        assert_eq!(
            resolve(Kind::Permission, "  Workspace-Write ", &entries).expect("hit"),
            "dsh:permission:d29ya3NwYWNlLXdyaXRl"
        );
        // The id's last segment is accepted as a short name too.
        assert_eq!(
            resolve(Kind::Permission, "cmVhZC1vbmx5", &entries).expect("hit"),
            "dsh:permission:cmVhZC1vbmx5"
        );
    }

    #[test]
    fn an_unknown_short_name_fails_and_lists_every_available_value() {
        let error = resolve(Kind::Permission, "readonly", &permissions()).unwrap_err();
        assert!(matches!(error, SelectionError::NotFound { .. }));
        let shown = error.to_string();
        assert!(shown.contains("--dsh-permission"), "{shown}");
        assert!(shown.contains("catalog.listPermissions"), "{shown}");
        for expected in [
            "dsh:permission:cmVhZC1vbmx5 (read-only)",
            "dsh:permission:d29ya3NwYWNlLXdyaXRl (workspace-write)",
            "dsh:permission:ZGFuZ2VyLWZ1bGwtYWNjZXNz (danger-full-access)",
        ] {
            assert!(shown.contains(expected), "{shown} is missing {expected}");
        }
    }

    #[test]
    fn an_empty_value_is_a_loud_error_not_a_default() {
        let error = resolve(Kind::Permission, "   ", &permissions()).unwrap_err();
        assert!(matches!(error, SelectionError::NotFound { .. }));
        assert!(error.to_string().contains("read-only"));
    }

    #[test]
    fn an_ambiguous_short_name_is_refused_instead_of_picking_one() {
        let entries = catalog(&[
            ("dsh:permission:a", "read-only"),
            ("dsh:permission:b", "read-only"),
        ]);
        let error = resolve(Kind::Permission, "read-only", &entries).unwrap_err();
        match &error {
            SelectionError::Ambiguous { matches, .. } => {
                assert!(matches.contains("dsh:permission:a"));
                assert!(matches.contains("dsh:permission:b"));
            }
            other => panic!("expected ambiguity, got {other:?}"),
        }
        assert!(error.to_string().contains("ambiguous"));
    }

    #[test]
    fn a_full_selection_id_is_echoed_verbatim_even_with_no_catalog() {
        assert_eq!(
            resolve(Kind::Permission, " dsh:permission:Zm9v ", &[]).expect("passthrough"),
            "dsh:permission:Zm9v"
        );
        // A model id may legitimately not be in a cached list; the bridge decides.
        assert_eq!(
            resolve(Kind::Model, "dsh:model:WyJwIiwibSIsbnVsbF0", &[]).expect("passthrough"),
            "dsh:model:WyJwIiwibSIsbnVsbF0"
        );
        // The prefix alone is not an id.
        assert!(resolve(Kind::Permission, "dsh:permission:", &permissions()).is_err());
    }

    #[test]
    fn a_model_short_name_resolves_by_display_name() {
        let entries = catalog(&[
            (
                "dsh:model:WyJkZWVwc2Vlay1vZmZpY2lhbCIsImRlZXBzZWVrLWZsYXNoIixudWxsXQ",
                "DeepSeek-V4.1-Flash",
            ),
            (
                "dsh:model:WyJkZWVwc2Vlay1vZmZpY2lhbCIsImRlZXBzZWVrLXY0LXBybyIsbnVsbF0",
                "DeepSeek-V4-Pro",
            ),
        ]);
        assert_eq!(
            resolve(Kind::Model, "deepseek-v4-pro", &entries).expect("hit"),
            "dsh:model:WyJkZWVwc2Vlay1vZmZpY2lhbCIsImRlZXBzZWVrLXY0LXBybyIsbnVsbF0"
        );
    }

    /// The shape a real bridge returns from `catalog.listAgentPresets`, trimmed.
    fn presets() -> Vec<CatalogEntry> {
        catalog(&[
            ("standard", "standard"),
            ("ptc", "ptc"),
            ("minimal", "minimal"),
            ("cordis", "cordis"),
            ("liangshen", "梁神模式"),
        ])
    }

    #[test]
    fn selections_are_absent_unless_at_least_one_flag_was_given() {
        assert_eq!(Selections::default().to_value(), None);
        assert!(Selections::default().is_empty());
        let only_permission = Selections {
            permission: Some(String::from("dsh:permission:cmVhZC1vbmx5")),
            model: None,
            preset: None,
        };
        assert_eq!(
            only_permission.to_value(),
            Some(json!({ "permission": "dsh:permission:cmVhZC1vbmx5" }))
        );
        let both = Selections {
            permission: Some(String::from("dsh:permission:cmVhZC1vbmx5")),
            model: Some(String::from("dsh:model:a")),
            preset: None,
        };
        assert_eq!(
            both.to_value(),
            Some(json!({ "permission": "dsh:permission:cmVhZC1vbmx5", "model": "dsh:model:a" }))
        );
        // A preset alone is not a selections object at all: it travels as
        // `createAndStart.agentPreset`, so the key must stay absent.
        let only_preset = Selections {
            permission: None,
            model: None,
            preset: Some(String::from("ptc")),
        };
        assert_eq!(only_preset.to_value(), None);
        assert!(!only_preset.is_empty());
    }

    #[test]
    fn a_preset_resolves_by_id_or_by_its_display_name() {
        let entries = presets();
        // The bridge's preset ids are bare words, so a "full id" and a short name
        // are the same lookup: both must land on the catalog's own id.
        assert_eq!(resolve(Kind::Preset, "ptc", &entries).expect("hit"), "ptc");
        assert_eq!(
            resolve(Kind::Preset, " PTC ", &entries).expect("hit"),
            "ptc"
        );
        assert_eq!(
            resolve(Kind::Preset, "梁神模式", &entries).expect("hit"),
            "liangshen"
        );
        assert_eq!(
            resolve(Kind::Preset, "liangshen", &entries).expect("hit"),
            "liangshen"
        );
    }

    #[test]
    fn an_unknown_preset_lists_every_available_preset() {
        let error = resolve(Kind::Preset, "standard-mode", &presets()).unwrap_err();
        assert!(matches!(error, SelectionError::NotFound { .. }));
        let shown = error.to_string();
        assert!(shown.contains("--dsh-preset"), "{shown}");
        assert!(shown.contains("catalog.listAgentPresets"), "{shown}");
        for expected in ["ptc (ptc)", "liangshen (梁神模式)"] {
            assert!(shown.contains(expected), "{shown} is missing {expected}");
        }
        // A prefix the presets do not have is not a free pass.
        assert!(resolve(Kind::Preset, "dsh:preset:ptc", &presets()).is_err());
        assert!(resolve(Kind::Preset, "   ", &presets()).is_err());
    }

    #[test]
    fn an_ambiguous_preset_name_is_refused_instead_of_picking_one() {
        let entries = catalog(&[("ptc-a", "PTC"), ("ptc-b", "PTC")]);
        let error = resolve(Kind::Preset, "ptc", &entries).unwrap_err();
        match &error {
            SelectionError::Ambiguous { matches, .. } => {
                assert!(matches.contains("ptc-a"), "{matches}");
                assert!(matches.contains("ptc-b"), "{matches}");
            }
            other => panic!("expected ambiguity, got {other:?}"),
        }
    }

    #[test]
    fn the_preset_hint_lists_candidates_without_choosing_one() {
        // A configured preset needs no advice.
        assert_eq!(preset_hint(Some("ptc"), &presets()), None);
        // No catalog, nothing to say.
        assert_eq!(preset_hint(None, &[]), None);
        let hint = preset_hint(None, &presets()).expect("hint");
        assert!(hint.contains("--dsh-preset"), "{hint}");
        for expected in ["ptc (ptc)", "liangshen (梁神模式)"] {
            assert!(hint.contains(expected), "{hint} is missing {expected}");
        }
        // The one thing it must never do is decide.
        assert!(hint.contains("Nothing was chosen for you"), "{hint}");
    }

    #[test]
    fn presets_always_need_the_catalog_because_their_ids_have_no_prefix() {
        assert!(needs_catalog(Kind::Preset, "ptc"));
        assert!(needs_catalog(Kind::Preset, ""));
        assert!(!needs_catalog(
            Kind::Permission,
            "dsh:permission:cmVhZC1vbmx5"
        ));
    }

    #[tokio::test]
    async fn full_ids_and_no_flags_do_not_dial_the_bridge() {
        // Port 1 is not listening: reaching the network would fail this test.
        let endpoint = Endpoint {
            host: String::from("127.0.0.1"),
            port: 1,
            token: String::from("t"),
            pid: None,
        };
        let selections = resolve_remote(
            &endpoint,
            "conn_1",
            "farwork-conn_1",
            "0.1.0",
            Some("dsh:permission:cmVhZC1vbmx5"),
            None,
            None,
        )
        .await
        .expect("no catalog needed");
        assert_eq!(
            selections.to_value(),
            Some(json!({ "permission": "dsh:permission:cmVhZC1vbmx5" }))
        );
        assert_eq!(
            resolve_remote(
                &endpoint,
                "conn_1",
                "farwork-conn_1",
                "0.1.0",
                None,
                None,
                None
            )
            .await
            .expect("nothing configured"),
            Selections::default()
        );
    }
}

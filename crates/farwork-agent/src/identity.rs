//! Stable local connector identity, and the credentials paired onto it.
//!
//! The hub authenticates a connector by its token; this file exists so the
//! device keeps the same local identity and display name across restarts
//! (the M1 connector regenerated a device id on every start, which is exactly
//! the bug this avoids).
//!
//! `farwork-agent pair` adds the hub URL and the connector token to the same
//! file, so a restart needs no flags: that is why it stays 0600 (and the
//! directory 0700) now that it can hold a secret.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::timeutil;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentIdentity {
    pub agent_id: String,
    pub name: String,
    pub created_at: String,
    /// Written by `pair`; absent in files created before pairing existed, so
    /// every credential field is optional and omitted when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hub: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connector_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connector_token: Option<String>,
}

impl AgentIdentity {
    fn create(name: &str) -> Self {
        Self {
            agent_id: format!("agt_{}", Uuid::new_v4().simple()),
            name: name.to_owned(),
            created_at: timeutil::utc_now(),
            hub: None,
            connector_id: None,
            connector_token: None,
        }
    }

    /// Record the credentials `pair` redeemed. The token is stored in the same
    /// 0600 file as the identity: one file to lose or delete, and no secret in
    /// the shell history or a log line.
    pub fn set_credentials(&mut self, hub: &str, connector_id: &str, token: &str) {
        self.hub = Some(hub.to_owned());
        self.connector_id = Some(connector_id.to_owned());
        self.connector_token = Some(token.to_owned());
    }

    pub fn stored_hub(&self) -> Option<&str> {
        self.hub.as_deref().map(str::trim).filter(|v| !v.is_empty())
    }

    pub fn stored_token(&self) -> Option<&str> {
        self.connector_token
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
    }
}

/// Default location: $FARWORK_HOME/agent.json, else ~/.farwork/agent.json.
pub fn default_path() -> PathBuf {
    home().join("agent.json")
}

/// The agent's state root: $FARWORK_HOME, else ~/.farwork.
///
/// Single source of truth because more than one thing lives here now (the
/// identity, and the DSH checkpoints that keep reconnects from replaying every
/// session).
pub fn home() -> PathBuf {
    if let Some(home) = std::env::var_os("FARWORK_HOME") {
        return PathBuf::from(home);
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".farwork")
}

/// Load the persisted identity, creating it on first run.
///
/// A changed 'name' (from --name) is persisted while agent_id stays put.
pub fn load_or_create(path: &Path, name: &str) -> Result<AgentIdentity> {
    match fs::read_to_string(path) {
        Ok(raw) => {
            let mut identity: AgentIdentity = serde_json::from_str(&raw)
                .with_context(|| format!("parse agent identity {}", path.display()))?;
            if identity.name != name {
                identity.name = name.to_owned();
                save(path, &identity)?;
            }
            Ok(identity)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let identity = AgentIdentity::create(name);
            save(path, &identity)?;
            Ok(identity)
        }
        Err(error) => Err(error).with_context(|| format!("read agent identity {}", path.display())),
    }
}

/// Persist the identity: directory 0700, file 0600.
///
/// Public because `pair` writes the file outside `load_or_create` (it grows
/// credentials, which is a different event from a rename).
pub fn save(path: &Path, identity: &AgentIdentity) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create state dir {}", parent.display()))?;
        restrict_dir(parent);
    }
    let body = serde_json::to_string_pretty(identity)?;
    fs::write(path, body).with_context(|| format!("write agent identity {}", path.display()))?;
    restrict_file(path);
    Ok(())
}

#[cfg(unix)]
pub(crate) fn restrict_file(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
pub(crate) fn restrict_file(_path: &Path) {}

#[cfg(unix)]
pub(crate) fn restrict_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
pub(crate) fn restrict_dir(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "farwork-agent-identity-{tag}-{}/agent.json",
            Uuid::new_v4().simple()
        ))
    }

    #[test]
    fn creates_then_reuses_the_same_agent_id() {
        let path = temp_path("reuse");
        let first = load_or_create(&path, "mbp").unwrap();
        let second = load_or_create(&path, "mbp").unwrap();
        assert_eq!(first.agent_id, second.agent_id);
        assert!(first.agent_id.starts_with("agt_"));
        assert_eq!(second.name, "mbp");
        assert_eq!(first.created_at, second.created_at);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn renamed_device_keeps_agent_id() {
        let path = temp_path("rename");
        let first = load_or_create(&path, "old-name").unwrap();
        let renamed = load_or_create(&path, "new-name").unwrap();
        assert_eq!(first.agent_id, renamed.agent_id);
        assert_eq!(renamed.name, "new-name");
        assert_eq!(load_or_create(&path, "new-name").unwrap().name, "new-name");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn identity_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let path = temp_path("perms");
        load_or_create(&path, "mbp").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "mode was {mode:o}");
        let dir_mode = fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700, "dir mode was {dir_mode:o}");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn credentials_survive_a_reload_and_stay_owner_only() {
        let path = temp_path("credentials");
        let mut identity = load_or_create(&path, "mbp").unwrap();
        identity.set_credentials("https://hub.example.com", "cn_1", "cxt_secret");
        save(&path, &identity).unwrap();

        let reloaded = load_or_create(&path, "mbp").unwrap();
        assert_eq!(reloaded.stored_hub(), Some("https://hub.example.com"));
        assert_eq!(reloaded.stored_token(), Some("cxt_secret"));
        assert_eq!(reloaded.connector_id.as_deref(), Some("cn_1"));
        assert_eq!(reloaded.agent_id, identity.agent_id);
        // A rename must not drop the credentials it shares a file with.
        let renamed = load_or_create(&path, "other-name").unwrap();
        assert_eq!(renamed.name, "other-name");
        assert_eq!(renamed.stored_token(), Some("cxt_secret"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "mode was {mode:o}");
        }
        // The on-disk shape is the contract other tools read.
        let raw = fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed["hub"], "https://hub.example.com");
        assert_eq!(parsed["connectorId"], "cn_1");
        assert_eq!(parsed["connectorToken"], "cxt_secret");
        assert_eq!(parsed["agentId"], identity.agent_id);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_pre_pairing_file_still_loads_and_gains_no_empty_keys() {
        let path = temp_path("legacy");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"agentId":"agt_old","name":"mbp","createdAt":"2025-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        let identity = load_or_create(&path, "mbp").unwrap();
        assert_eq!(identity.agent_id, "agt_old");
        assert_eq!(identity.stored_hub(), None);
        assert_eq!(identity.stored_token(), None);
        // Unset credentials are omitted, so an old file rewritten by this build
        // does not grow "hub": null and friends.
        let raw = fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("hub"), "{raw}");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn corrupt_identity_file_is_an_error_not_a_silent_regeneration() {
        let path = temp_path("corrupt");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{not json").unwrap();
        let error = load_or_create(&path, "mbp").unwrap_err();
        assert!(error.to_string().contains("parse agent identity"));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}

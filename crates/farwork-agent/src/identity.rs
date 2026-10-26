//! Stable local connector identity.
//!
//! The hub authenticates a connector by its token; this file exists so the
//! device keeps the same local identity and display name across restarts
//! (the M1 connector regenerated a device id on every start, which is exactly
//! the bug this avoids). It is written 0600 and never holds the token.

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
}

impl AgentIdentity {
    fn create(name: &str) -> Self {
        Self {
            agent_id: format!("agt_{}", Uuid::new_v4().simple()),
            name: name.to_owned(),
            created_at: timeutil::utc_now(),
        }
    }
}

/// Default location: $FARWORK_HOME/agent.json, else ~/.farwork/agent.json.
pub fn default_path() -> PathBuf {
    if let Some(home) = std::env::var_os("FARWORK_HOME") {
        return PathBuf::from(home).join("agent.json");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".farwork").join("agent.json")
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
                write(path, &identity)?;
            }
            Ok(identity)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let identity = AgentIdentity::create(name);
            write(path, &identity)?;
            Ok(identity)
        }
        Err(error) => Err(error).with_context(|| format!("read agent identity {}", path.display())),
    }
}

fn write(path: &Path, identity: &AgentIdentity) -> Result<()> {
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
fn restrict_file(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_file(_path: &Path) {}

#[cfg(unix)]
fn restrict_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn restrict_dir(_path: &Path) {}

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
    fn corrupt_identity_file_is_an_error_not_a_silent_regeneration() {
        let path = temp_path("corrupt");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{not json").unwrap();
        let error = load_or_create(&path, "mbp").unwrap_err();
        assert!(error.to_string().contains("parse agent identity"));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}

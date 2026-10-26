//! Last-known `session.capabilities` per session.
//!
//! The frame describes a live runtime process, not a row: persisting it would
//! make a claim about a process that may no longer exist. So it lives in memory
//! and is dropped on `session.exit` and on connector disconnect, which is what
//! makes "no capabilities yet" and "capabilities withdrawn" the same truthful
//! answer — every flag false.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// What the runtime reports it can do `right now`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityFlags {
    // All four are `default`: a frame that omits one must not be dropped, and an
    // omitted flag reads as "not supported" — never as "supported".
    #[serde(default)]
    pub steer: bool,
    #[serde(default)]
    pub interrupt: bool,
    #[serde(default)]
    pub selections: bool,
    #[serde(default)]
    pub commands: bool,
}

impl CapabilityFlags {
    pub fn view(self) -> Value {
        json!({
            "steer": self.steer,
            "interrupt": self.interrupt,
            "selections": self.selections,
            "commands": self.commands,
        })
    }
}

/// One entry of a model or permission catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogItem {
    pub id: String,
    // Lenient on read, always present on write: the id is what a selection patch
    // sends, so a missing name is a display detail, not a reason to drop the
    // whole catalog.
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl CatalogItem {
    pub fn view(&self) -> Value {
        let mut value = json!({ "id": self.id, "name": self.name });
        if let Some(description) = &self.description {
            if let Some(map) = value.as_object_mut() {
                map.insert(
                    String::from("description"),
                    Value::String(description.clone()),
                );
            }
        }
        value
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CatalogSet {
    #[serde(default)]
    pub models: Vec<CatalogItem>,
    #[serde(default)]
    pub permissions: Vec<CatalogItem>,
}

/// One runtime command (`/help`, `/clear`, ...).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandEntry {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl CommandEntry {
    pub fn view(&self) -> Value {
        let mut value = json!({ "name": self.name });
        if let Some(description) = &self.description {
            if let Some(map) = value.as_object_mut() {
                map.insert(
                    String::from("description"),
                    Value::String(description.clone()),
                );
            }
        }
        value
    }
}

/// The two catalog families the REST surface addresses by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Catalog {
    Model,
    Permission,
}

impl Catalog {
    /// `None` for anything else: an unknown catalog is a 422, not an empty list
    /// that would read like "this runtime offers nothing".
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "model" => Some(Self::Model),
            "permission" => Some(Self::Permission),
            _ => None,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Permission => "permission",
        }
    }

    pub fn items(self, catalogs: &CatalogSet) -> &[CatalogItem] {
        match self {
            Self::Model => &catalogs.models,
            Self::Permission => &catalogs.permissions,
        }
    }
}

/// One cached report.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub capabilities: CapabilityFlags,
    pub catalogs: CatalogSet,
    pub commands: Vec<CommandEntry>,
    /// Hub-assigned, bumped on every accepted report. It is the cache's own
    /// version, not a runtime number the agent never sends.
    pub revision: u64,
    pub updated_at: String,
}

#[derive(Default)]
pub struct RuntimeCapsCache {
    entries: Mutex<HashMap<String, Snapshot>>,
}

impl RuntimeCapsCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Snapshot>> {
        match self.entries.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Store a report, replacing the previous one for that session.
    pub fn record(
        &self,
        session_id: &str,
        capabilities: CapabilityFlags,
        catalogs: CatalogSet,
        commands: Vec<CommandEntry>,
        updated_at: impl Into<String>,
    ) -> u64 {
        let mut entries = self.lock();
        let revision = entries
            .get(session_id)
            .map(|previous| previous.revision + 1)
            .unwrap_or(1);
        entries.insert(
            session_id.to_string(),
            Snapshot {
                capabilities,
                catalogs,
                commands,
                revision,
                updated_at: updated_at.into(),
            },
        );
        revision
    }

    pub fn get(&self, session_id: &str) -> Option<Snapshot> {
        self.lock().get(session_id).cloned()
    }

    /// Drop one session's report. Returns whether anything was cached, so a
    /// disconnect can log only the sessions that actually held state.
    pub fn clear(&self, session_id: &str) -> bool {
        self.lock().remove(session_id).is_some()
    }

    pub fn clear_all(&self, session_ids: &[String]) -> usize {
        let mut entries = self.lock();
        session_ids
            .iter()
            .filter(|session_id| entries.remove(*session_id).is_some())
            .count()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, name: &str) -> CatalogItem {
        CatalogItem {
            id: String::from(id),
            name: String::from(name),
            description: None,
        }
    }

    #[test]
    fn an_unreported_session_has_no_snapshot() {
        let cache = RuntimeCapsCache::new();
        assert!(cache.get("sess_1").is_none());
        // The truthful default every REST reader falls back to.
        assert_eq!(
            CapabilityFlags::default(),
            CapabilityFlags {
                steer: false,
                interrupt: false,
                selections: false,
                commands: false,
            }
        );
    }

    #[test]
    fn recording_replaces_the_previous_report_and_bumps_the_revision() {
        let cache = RuntimeCapsCache::new();
        let first = cache.record(
            "sess_1",
            CapabilityFlags {
                steer: true,
                ..CapabilityFlags::default()
            },
            CatalogSet {
                models: vec![item("echo-fast", "Echo fast")],
                permissions: Vec::new(),
            },
            Vec::new(),
            "2026-01-01T00:00:00Z",
        );
        assert_eq!(first, 1);
        let second = cache.record(
            "sess_1",
            CapabilityFlags::default(),
            CatalogSet::default(),
            Vec::new(),
            "2026-01-01T00:00:01Z",
        );
        assert_eq!(second, 2, "the revision tracks the cache, not the runtime");
        let stored = cache.get("sess_1").expect("cached");
        assert!(!stored.capabilities.steer, "the newer report wins");
        assert!(stored.catalogs.models.is_empty());
    }

    #[test]
    fn clearing_removes_the_report_so_the_reader_sees_nothing() {
        let cache = RuntimeCapsCache::new();
        cache.record(
            "sess_1",
            CapabilityFlags::default(),
            CatalogSet::default(),
            Vec::new(),
            "2026-01-01T00:00:00Z",
        );
        assert!(cache.clear("sess_1"));
        assert!(!cache.clear("sess_1"), "clearing twice is not an error");
        assert!(cache.get("sess_1").is_none());

        cache.record(
            "sess_1",
            CapabilityFlags::default(),
            CatalogSet::default(),
            Vec::new(),
            "now",
        );
        cache.record(
            "sess_2",
            CapabilityFlags::default(),
            CatalogSet::default(),
            Vec::new(),
            "now",
        );
        let cleared = cache.clear_all(&[String::from("sess_1"), String::from("sess_gone")]);
        assert_eq!(cleared, 1);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn catalogs_are_addressed_by_name_and_nothing_else() {
        assert_eq!(Catalog::parse("model"), Some(Catalog::Model));
        assert_eq!(Catalog::parse("permission"), Some(Catalog::Permission));
        assert_eq!(Catalog::parse("models"), None);
        assert_eq!(Catalog::parse(""), None);
        assert_eq!(Catalog::Model.key(), "model");

        let catalogs = CatalogSet {
            models: vec![item("m1", "Model 1")],
            permissions: vec![item("p1", "Permission 1")],
        };
        assert_eq!(Catalog::Model.items(&catalogs).len(), 1);
        assert_eq!(Catalog::Permission.items(&catalogs)[0].id, "p1");
    }

    #[test]
    fn views_omit_absent_descriptions_instead_of_sending_null() {
        let plain = CatalogItem {
            id: String::from("m1"),
            name: String::from("Model"),
            description: None,
        }
        .view();
        assert!(plain.get("description").is_none());
        let described = CatalogItem {
            id: String::from("m1"),
            name: String::from("Model"),
            description: Some(String::from("fast")),
        }
        .view();
        assert_eq!(described["description"], json!("fast"));
        assert!(CommandEntry {
            name: String::from("/help"),
            description: None,
        }
        .view()
        .get("description")
        .is_none());
    }
}

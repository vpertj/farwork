//! What each device says it can run, as reported by `hello`.
//!
//! `hello.agent.supportedRuntimes` describes a live process on the device, so it
//! is kept in memory next to the socket instead of in Postgres: a device that is
//! gone cannot have a current opinion about what it can run. A connector that
//! never reported reads as `[]` — unknown, which is exactly what the UI renders
//! as "设备未上报可用运行时" — and the report is dropped when its socket closes,
//! so a stale probe result never survives the process it described.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// One device-reported probe result.
///
/// Every field is lenient on read: an entry without `name` is meaningless and is
/// what the UI drops, and a missing `available` is "not available" — never
/// "available". The hub never invents an entry or a reason.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeReport {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

impl RuntimeReport {
    /// The exact `connector.runtimes` entry shape the web client parses.
    pub fn view(&self) -> Value {
        json!({
            "name": self.name,
            "available": self.available,
            "reason": self.reason,
        })
    }
}

struct Entry {
    /// Which connection produced the report. Only that connection may replace or
    /// clear it, so a socket that lost the route to a reconnect cannot erase the
    /// newer device's answer on its way out.
    connection_id: String,
    runtimes: Vec<RuntimeReport>,
}

#[derive(Default)]
pub struct ConnectorRuntimeCache {
    entries: Mutex<HashMap<String, Entry>>,
}

impl ConnectorRuntimeCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        match self.entries.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Store what one connection reported. `None` (the agent omitted the key) and
    /// an empty list both mean "nothing to say", which reads as unknown. The
    /// newest report always wins: that is what "the device's last answer" means,
    /// and a takeover is exactly the case where the new socket must replace the
    /// old one.
    pub fn record(
        &self,
        connector_id: &str,
        connection_id: &str,
        runtimes: Option<Vec<RuntimeReport>>,
    ) {
        let mut entries = self.lock();
        match runtimes.filter(|list| !list.is_empty()) {
            Some(runtimes) => {
                entries.insert(
                    connector_id.to_string(),
                    Entry {
                        connection_id: connection_id.to_string(),
                        runtimes,
                    },
                );
            }
            None => {
                entries.remove(connector_id);
            }
        }
    }

    /// Drop the report of a connector that just went offline. Returns whether
    /// anything was cached, so a disconnect logs only the devices that had state.
    pub fn clear(&self, connector_id: &str, connection_id: &str) -> bool {
        let mut entries = self.lock();
        let is_current = entries
            .get(connector_id)
            .map(|entry| entry.connection_id == connection_id)
            .unwrap_or(false);
        if is_current {
            entries.remove(connector_id);
        }
        is_current
    }

    /// The connector's last report; `[]` when it never reported (or is gone).
    pub fn runtimes(&self, connector_id: &str) -> Vec<RuntimeReport> {
        self.lock()
            .get(connector_id)
            .map(|entry| entry.runtimes.clone())
            .unwrap_or_default()
    }

    /// The `runtimes` value for a connector view: always an array, never a
    /// missing key, because the client treats both as unknown.
    pub fn views(&self, connector_id: &str) -> Value {
        Value::Array(
            self.runtimes(connector_id)
                .iter()
                .map(RuntimeReport::view)
                .collect(),
        )
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(name: &str, available: bool, reason: Option<&str>) -> RuntimeReport {
        RuntimeReport {
            name: name.to_string(),
            available,
            reason: reason.map(str::to_string),
        }
    }

    #[test]
    fn an_unreported_connector_reads_as_an_empty_array() {
        let cache = ConnectorRuntimeCache::new();
        assert!(cache.runtimes("conn_1").is_empty());
        assert_eq!(cache.views("conn_1"), json!([]));
    }

    #[test]
    fn recording_replaces_the_previous_report_of_the_same_connection() {
        let cache = ConnectorRuntimeCache::new();
        cache.record("conn_1", "c1", Some(vec![report("echo", true, None)]));
        assert_eq!(cache.runtimes("conn_1").len(), 1);
        cache.record(
            "conn_1",
            "c1",
            Some(vec![report("dsh", false, Some("no endpoint"))]),
        );
        let stored = cache.runtimes("conn_1");
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].name, "dsh");
        assert!(!stored[0].available);
        assert_eq!(stored[0].reason.as_deref(), Some("no endpoint"));
    }

    #[test]
    fn an_omitted_key_or_an_empty_list_leaves_nothing_to_report() {
        let cache = ConnectorRuntimeCache::new();
        cache.record("conn_1", "c1", Some(vec![report("echo", true, None)]));
        cache.record("conn_1", "c1", None);
        assert!(cache.runtimes("conn_1").is_empty());
        cache.record("conn_1", "c1", Some(vec![report("echo", true, None)]));
        cache.record("conn_1", "c1", Some(Vec::new()));
        assert!(cache.runtimes("conn_1").is_empty());
    }

    #[test]
    fn a_disconnect_drops_the_report_and_only_the_owning_connection_may() {
        let cache = ConnectorRuntimeCache::new();
        cache.record("conn_1", "c1", Some(vec![report("echo", true, None)]));
        // A reconnecting device owns the route now: the old socket's teardown
        // must not wipe the fresh answer.
        cache.record("conn_1", "c2", Some(vec![report("dsh", true, None)]));
        assert!(!cache.clear("conn_1", "c1"));
        assert_eq!(cache.runtimes("conn_1")[0].name, "dsh");
        assert!(cache.clear("conn_1", "c2"));
        assert!(cache.runtimes("conn_1").is_empty());
        assert_eq!(cache.len(), 0);
        assert!(
            !cache.clear("conn_1", "c2"),
            "clearing twice is not an error"
        );
    }

    #[test]
    fn the_newest_report_wins_so_a_reconnect_replaces_the_old_answer() {
        let cache = ConnectorRuntimeCache::new();
        cache.record("conn_1", "c1", Some(vec![report("echo", true, None)]));
        cache.record("conn_1", "c2", Some(vec![report("dsh", true, None)]));
        assert_eq!(cache.runtimes("conn_1")[0].name, "dsh");
    }

    #[test]
    fn the_view_is_the_exact_shape_the_web_client_parses() {
        let cache = ConnectorRuntimeCache::new();
        cache.record(
            "conn_1",
            "c1",
            Some(vec![
                report("dsh", true, None),
                report(
                    "codex",
                    false,
                    Some("codex CLI 不在 PATH 上（spawn codex ENOENT）"),
                ),
            ]),
        );
        assert_eq!(
            cache.views("conn_1"),
            json!([
                { "name": "dsh", "available": true, "reason": null },
                { "name": "codex", "available": false, "reason": "codex CLI 不在 PATH 上（spawn codex ENOENT）" }
            ])
        );
    }

    #[test]
    fn an_entry_that_omits_fields_is_read_leniently() {
        let parsed: Vec<RuntimeReport> = serde_json::from_value(json!([
            { "name": "dsh", "available": true },
            { "name": "codex" },
        ]))
        .expect("parse");
        assert!(parsed[0].available);
        assert_eq!(parsed[0].reason, None);
        assert!(!parsed[1].available, "a missing flag is not availability");
    }
}

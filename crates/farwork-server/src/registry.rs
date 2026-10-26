use std::collections::HashMap;
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use farwork_protocol::{DeviceInfo, DeviceView, ServerToConnector};
use tokio::sync::{broadcast, mpsc};

const BROADCAST_CAPACITY: usize = 64;

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

struct Entry {
    info: DeviceInfo,
    connected: bool,
    last_seen_ms: u64,
    sink: Option<mpsc::UnboundedSender<ServerToConnector>>,
}

/// In-memory device registry: the only source of truth for connected devices.
pub struct DeviceRegistry {
    entries: RwLock<HashMap<String, Entry>>,
    changes: broadcast::Sender<Vec<DeviceView>>,
}

impl DeviceRegistry {
    pub fn new() -> Self {
        let (changes, _receiver) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            entries: RwLock::new(HashMap::new()),
            changes,
        }
    }

    fn read(&self) -> RwLockReadGuard<'_, HashMap<String, Entry>> {
        self.entries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn write(&self) -> RwLockWriteGuard<'_, HashMap<String, Entry>> {
        self.entries
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Record a connector as connected and remember how to reach it.
    pub fn connect(&self, info: DeviceInfo, sink: mpsc::UnboundedSender<ServerToConnector>) {
        {
            let mut entries = self.write();
            let entry = entries.entry(info.id.clone()).or_insert_with(|| Entry {
                info: info.clone(),
                connected: false,
                last_seen_ms: 0,
                sink: None,
            });
            entry.info = info;
            entry.connected = true;
            entry.last_seen_ms = now_ms();
            entry.sink = Some(sink);
        }
        self.publish();
    }

    /// Mark a connector as gone and drop its frame sink.
    pub fn disconnect(&self, device_id: &str) {
        {
            let mut entries = self.write();
            match entries.get_mut(device_id) {
                Some(entry) => {
                    entry.connected = false;
                    entry.last_seen_ms = now_ms();
                    entry.sink = None;
                }
                None => return,
            }
        }
        self.publish();
    }

    /// Refresh `last_seen_ms` without emitting a change notification.
    pub fn touch(&self, device_id: &str) {
        let mut entries = self.write();
        if let Some(entry) = entries.get_mut(device_id) {
            entry.last_seen_ms = now_ms();
        }
    }

    /// Frame sink of a currently connected device.
    pub fn sink(&self, device_id: &str) -> Option<mpsc::UnboundedSender<ServerToConnector>> {
        self.read()
            .get(device_id)
            .filter(|entry| entry.connected)
            .and_then(|entry| entry.sink.clone())
    }

    /// Stable snapshot ordered by name, then id.
    pub fn snapshot(&self) -> Vec<DeviceView> {
        let mut views: Vec<DeviceView> = self
            .read()
            .values()
            .map(|entry| DeviceView {
                id: entry.info.id.clone(),
                name: entry.info.name.clone(),
                platform: entry.info.platform,
                connected: entry.connected,
                last_seen_ms: entry.last_seen_ms,
            })
            .collect();
        views.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.id.cmp(&right.id))
        });
        views
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Vec<DeviceView>> {
        self.changes.subscribe()
    }

    fn publish(&self) {
        // Fails only when nobody is subscribed, which is not an error.
        let _ = self.changes.send(self.snapshot());
    }
}

impl Default for DeviceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use farwork_protocol::Platform;

    fn info(id: &str, name: &str) -> DeviceInfo {
        DeviceInfo {
            id: id.to_string(),
            name: name.to_string(),
            platform: Platform::Linux,
        }
    }

    fn channel() -> (
        mpsc::UnboundedSender<ServerToConnector>,
        mpsc::UnboundedReceiver<ServerToConnector>,
    ) {
        mpsc::unbounded_channel()
    }

    #[test]
    fn new_registry_is_empty() {
        let registry = DeviceRegistry::new();
        assert!(registry.snapshot().is_empty());
        assert!(registry.sink("missing").is_none());
    }

    #[test]
    fn connect_registers_a_connected_device() {
        let registry = DeviceRegistry::new();
        let (tx, _rx) = channel();
        registry.connect(info("d-1", "mac-mini"), tx);

        let devices = registry.snapshot();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, "d-1");
        assert_eq!(devices[0].name, "mac-mini");
        assert_eq!(devices[0].platform, Platform::Linux);
        assert!(devices[0].connected);
        assert!(devices[0].last_seen_ms > 0);
        assert!(registry.sink("d-1").is_some());
    }

    #[test]
    fn reconnect_updates_identity_instead_of_duplicating() {
        let registry = DeviceRegistry::new();
        let (first, _first_rx) = channel();
        registry.connect(info("d-1", "old-name"), first);
        let (second, _second_rx) = channel();
        registry.connect(info("d-1", "new-name"), second);

        let devices = registry.snapshot();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "new-name");
        assert!(devices[0].connected);
    }

    #[test]
    fn disconnect_marks_device_offline_and_drops_sink() {
        let registry = DeviceRegistry::new();
        let (tx, _rx) = channel();
        registry.connect(info("d-1", "mac-mini"), tx);
        registry.disconnect("d-1");

        let devices = registry.snapshot();
        assert_eq!(devices.len(), 1);
        assert!(!devices[0].connected);
        assert!(registry.sink("d-1").is_none());
    }

    #[test]
    fn disconnect_of_unknown_device_is_a_noop() {
        let registry = DeviceRegistry::new();
        registry.disconnect("nope");
        assert!(registry.snapshot().is_empty());
    }

    #[tokio::test]
    async fn connect_and_disconnect_notify_subscribers() {
        let registry = DeviceRegistry::new();
        let mut changes = registry.subscribe();

        let (tx, _rx) = channel();
        registry.connect(info("d-1", "mac-mini"), tx);
        let connected = changes.recv().await.expect("connect notification");
        assert!(connected[0].connected);

        registry.disconnect("d-1");
        let disconnected = changes.recv().await.expect("disconnect notification");
        assert!(!disconnected[0].connected);
    }

    #[test]
    fn snapshot_is_sorted_by_name_then_id() {
        let registry = DeviceRegistry::new();
        for (id, name) in [("b", "zeta"), ("c", "alpha"), ("a", "alpha")] {
            let (tx, _rx) = channel();
            registry.connect(info(id, name), tx);
        }
        let ids: Vec<String> = registry
            .snapshot()
            .into_iter()
            .map(|device| device.id)
            .collect();
        assert_eq!(ids, vec!["a", "c", "b"]);
    }
}

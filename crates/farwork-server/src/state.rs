use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use farwork_protocol::ServerToClient;
use tokio::sync::mpsc;

use crate::registry::DeviceRegistry;

/// Shared server state; cheap to clone per connection.
#[derive(Clone)]
pub struct AppState {
    token: Option<Arc<str>>,
    pub devices: Arc<DeviceRegistry>,
    pub streams: Arc<StreamRegistry>,
}

impl AppState {
    /// `token == None` means open development mode.
    pub fn new(token: Option<String>) -> Self {
        Self {
            token: token.map(Arc::from),
            devices: Arc::new(DeviceRegistry::new()),
            streams: Arc::new(StreamRegistry::new()),
        }
    }

    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// Accepts any caller in development mode, otherwise compares tokens.
    pub fn authorize(&self, provided: Option<&str>) -> bool {
        match self.token() {
            None => true,
            Some(expected) => provided == Some(expected),
        }
    }
}

/// Where an open terminal stream lives: which device to drive and which
/// browser connection receives its output.
#[derive(Clone)]
pub struct Route {
    pub device_id: String,
    pub client: mpsc::UnboundedSender<ServerToClient>,
}

/// stream_id -> route table shared by all connections.
pub struct StreamRegistry {
    routes: RwLock<HashMap<String, Route>>,
}

impl StreamRegistry {
    pub fn new() -> Self {
        Self {
            routes: RwLock::new(HashMap::new()),
        }
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<String, Route>> {
        self.routes
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, HashMap<String, Route>> {
        self.routes
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn add(&self, stream_id: String, route: Route) {
        self.write().insert(stream_id, route);
    }

    pub fn get(&self, stream_id: &str) -> Option<Route> {
        self.read().get(stream_id).cloned()
    }

    pub fn remove(&self, stream_id: &str) -> Option<Route> {
        self.write().remove(stream_id)
    }

    /// Remove and return every stream owned by a device.
    pub fn drain_device(&self, device_id: &str) -> Vec<(String, Route)> {
        let mut routes = self.write();
        let stream_ids: Vec<String> = routes
            .iter()
            .filter(|(_, route)| route.device_id == device_id)
            .map(|(stream_id, _)| stream_id.clone())
            .collect();
        stream_ids
            .into_iter()
            .filter_map(|stream_id| routes.remove(&stream_id).map(|route| (stream_id, route)))
            .collect()
    }
}

impl Default for StreamRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(device_id: &str) -> (Route, mpsc::UnboundedReceiver<ServerToClient>) {
        let (client, rx) = mpsc::unbounded_channel();
        (
            Route {
                device_id: device_id.to_string(),
                client,
            },
            rx,
        )
    }

    #[test]
    fn offers_are_routed_and_removable() {
        let registry = StreamRegistry::new();
        let (entry, _rx) = route("d-1");
        registry.add("s-1".to_string(), entry);

        assert_eq!(
            registry.get("s-1").map(|route| route.device_id),
            Some("d-1".to_string())
        );
        assert!(registry.remove("s-1").is_some());
        assert!(registry.get("s-1").is_none());
        assert!(registry.remove("s-1").is_none());
    }

    #[test]
    fn drain_device_only_takes_matching_streams() {
        let registry = StreamRegistry::new();
        let (first, _first_rx) = route("d-1");
        registry.add("s-1".to_string(), first);
        let (second, _second_rx) = route("d-1");
        registry.add("s-2".to_string(), second);
        let (other, _other_rx) = route("d-2");
        registry.add("s-3".to_string(), other);

        let mut drained: Vec<String> = registry
            .drain_device("d-1")
            .into_iter()
            .map(|(stream_id, _)| stream_id)
            .collect();
        drained.sort();
        assert_eq!(drained, vec!["s-1", "s-2"]);
        assert!(registry.get("s-3").is_some());
    }

    #[test]
    fn auth_respects_token_presence() {
        let open = AppState::new(None);
        assert!(open.authorize(None));
        assert!(open.authorize(Some("anything")));

        let guarded = AppState::new(Some("secret".to_string()));
        assert!(!guarded.authorize(None));
        assert!(!guarded.authorize(Some("wrong")));
        assert!(guarded.authorize(Some("secret")));
    }
}

//! Request/response pairing for `fs.request` → `fs.response`.
//!
//! The ingress socket is shared, so every file request is parked here under the
//! requestId the hub minted and the socket loop hands the reply back through a
//! oneshot. Whoever leaves first — the reply, the timeout, or a disconnect —
//! removes the entry, so a request nobody answers cannot leak.

use std::collections::HashMap;
use std::sync::Mutex;

use tokio::sync::oneshot;

use crate::ingress::IngressFsResponse;

struct Pending {
    connector_id: String,
    sender: oneshot::Sender<IngressFsResponse>,
}

/// What happened to a connector's reply.
#[derive(Debug, PartialEq, Eq)]
pub enum Delivery {
    Delivered,
    /// The hub never minted this id, or the request already timed out.
    Unknown,
    /// The id exists but belongs to another connector's request.
    WrongConnector,
}

#[derive(Default)]
pub struct FsRelay {
    pending: Mutex<HashMap<String, Pending>>,
}

impl FsRelay {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Pending>> {
        match self.pending.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Park one request and return the receiver its reply will arrive on.
    pub fn register(
        &self,
        connector_id: &str,
        request_id: &str,
    ) -> oneshot::Receiver<IngressFsResponse> {
        let (sender, receiver) = oneshot::channel();
        self.lock().insert(
            request_id.to_string(),
            Pending {
                connector_id: connector_id.to_string(),
                sender,
            },
        );
        receiver
    }

    /// Hand a connector reply to the handler waiting for it. Only the connector
    /// the request was sent to may answer it: requestIds are guessable enough
    /// that one device must not be able to reply for another.
    pub fn deliver(&self, connector_id: &str, reply: IngressFsResponse) -> Delivery {
        let mut pending = self.lock();
        let Some(owner) = pending
            .get(&reply.request_id)
            .map(|entry| entry.connector_id.clone())
        else {
            return Delivery::Unknown;
        };
        if owner != connector_id {
            return Delivery::WrongConnector;
        }
        let Some(entry) = pending.remove(&reply.request_id) else {
            return Delivery::Unknown;
        };
        let _ = entry.sender.send(reply);
        Delivery::Delivered
    }

    /// Drop one parked request without delivering: its timeout already fired, or
    /// the frame could not be handed to the socket.
    pub fn abandon(&self, request_id: &str) -> bool {
        self.lock().remove(request_id).is_some()
    }

    /// Fail every request a connector still owes. Its socket is gone, so no reply
    /// can arrive and the waiting handlers must not sit until the timeout.
    pub fn abandon_connector(&self, connector_id: &str) -> usize {
        let mut pending = self.lock();
        let doomed: Vec<String> = pending
            .iter()
            .filter(|(_, entry)| entry.connector_id == connector_id)
            .map(|(request_id, _)| request_id.clone())
            .collect();
        for request_id in &doomed {
            pending.remove(request_id);
        }
        doomed.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(request_id: &str) -> IngressFsResponse {
        IngressFsResponse {
            request_id: String::from(request_id),
            ok: true,
            op: Some(String::from("list")),
            path: Some(String::from("/w")),
            entries: None,
            size_bytes: None,
            content: None,
            truncated: None,
            error: None,
        }
    }

    #[tokio::test]
    async fn a_reply_reaches_the_request_parked_on_that_id() {
        let relay = FsRelay::new();
        let mut first = relay.register("conn_1", "req_1");
        let mut second = relay.register("conn_1", "req_2");

        assert_eq!(relay.deliver("conn_1", reply("req_2")), Delivery::Delivered);
        assert_eq!(
            second.try_recv().expect("req_2 answered").request_id,
            "req_2"
        );
        assert!(
            first.try_recv().is_err(),
            "concurrent requests must not cross"
        );

        assert_eq!(relay.deliver("conn_1", reply("req_1")), Delivery::Delivered);
        assert_eq!(
            first.try_recv().expect("req_1 answered").request_id,
            "req_1"
        );

        // Both entries are gone once answered: no map growth per request.
        assert_eq!(relay.deliver("conn_1", reply("req_1")), Delivery::Unknown);
    }

    #[tokio::test]
    async fn an_unknown_id_is_refused_and_another_connector_cannot_answer() {
        let relay = FsRelay::new();
        let mut waiting = relay.register("conn_1", "req_1");

        assert_eq!(
            relay.deliver("conn_1", reply("req_unknown")),
            Delivery::Unknown
        );
        assert_eq!(
            relay.deliver("conn_2", reply("req_1")),
            Delivery::WrongConnector,
            "a different connector must not be able to answer this id"
        );
        assert!(waiting.try_recv().is_err(), "still unanswered");
        assert_eq!(relay.deliver("conn_1", reply("req_1")), Delivery::Delivered);
        assert!(waiting.try_recv().is_ok());
    }

    #[tokio::test]
    async fn abandon_drops_the_entry_so_the_waiter_stops_waiting() {
        let relay = FsRelay::new();
        let abandoned = relay.register("conn_1", "req_1");
        assert!(relay.abandon("req_1"));
        assert!(!relay.abandon("req_1"), "removing twice is not an error");
        assert!(
            abandoned.await.is_err(),
            "a dropped sender must surface as a closed channel"
        );

        let kept = relay.register("conn_1", "req_2");
        let other = relay.register("conn_2", "req_3");
        assert_eq!(relay.abandon_connector("conn_1"), 1);
        assert!(kept.await.is_err());
        drop(other);
    }
}

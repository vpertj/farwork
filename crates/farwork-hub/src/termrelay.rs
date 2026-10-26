//! Live terminal channels, one per open terminal.
//!
//! The connector socket is the only path a PTY frame can take into the hub, and
//! one browser socket per terminal is the only path out. This module is the join:
//! `connector_ws` publishes the agent's `terminal.*` frames under their
//! `terminalId`, and the stream socket subscribes to that id.
//!
//! It is in process on purpose — a socket is not shareable across processes, so a
//! channel table is not a database concern. What survives a restart is the
//! `terminals` table, and a restart closes every terminal by definition.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::Value;
use tokio::sync::broadcast;

/// Per-terminal backlog. PTY output is bursty (a `cat` of a log file), and a slow
/// browser must be told that it lagged rather than silently skip bytes: a
/// terminal missing a chunk is a corrupt screen, not a dropped frame.
pub const CHANNEL_CAPACITY: usize = 1024;

/// One browser socket's view of a terminal.
pub struct Subscription {
    pub frames: broadcast::Receiver<Value>,
    /// `terminal.opened` already arrived before this socket existed, so the
    /// handler has to replay it or the browser would wait for an ack it missed.
    pub already_opened: bool,
}

struct Channel {
    sender: broadcast::Sender<Value>,
    opened: bool,
}

#[derive(Default)]
pub struct TerminalRelay {
    channels: Mutex<HashMap<String, Channel>>,
}

impl TerminalRelay {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Channel>> {
        match self.channels.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Reserve the channel before `terminal.open` is dispatched: a fast agent can
    /// answer `terminal.opened` before the REST handler has even returned.
    pub fn register(&self, terminal_id: &str) {
        let (sender, _receiver) = broadcast::channel(CHANNEL_CAPACITY);
        self.lock().insert(
            terminal_id.to_string(),
            Channel {
                sender,
                opened: false,
            },
        );
    }

    pub fn is_registered(&self, terminal_id: &str) -> bool {
        self.lock().contains_key(terminal_id)
    }

    /// `None` when this id was never registered or the terminal is already over.
    pub fn subscribe(&self, terminal_id: &str) -> Option<Subscription> {
        let guard = self.lock();
        let channel = guard.get(terminal_id)?;
        Some(Subscription {
            frames: channel.sender.subscribe(),
            already_opened: channel.opened,
        })
    }

    /// Record that the agent confirmed the shell. Must be called *before* the
    /// matching frame is published, or a socket that subscribes in between would
    /// see neither the flag nor the frame.
    pub fn mark_opened(&self, terminal_id: &str) {
        if let Some(channel) = self.lock().get_mut(terminal_id) {
            channel.opened = true;
        }
    }

    /// Hand one agent frame to every socket watching this terminal. `false` means
    /// the terminal is no longer registered, so the frame had nowhere to go.
    pub fn publish(&self, terminal_id: &str, frame: &Value) -> bool {
        let guard = self.lock();
        let Some(channel) = guard.get(terminal_id) else {
            return false;
        };
        // A send error only means no browser is attached right now.
        let _ = channel.sender.send(frame.clone());
        true
    }

    /// Retire the terminal: subscribers see their channel close, which is how the
    /// stream socket learns to stop reading and close.
    pub fn close(&self, terminal_id: &str) -> bool {
        self.lock().remove(terminal_id).is_some()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::sync::broadcast::error::RecvError;

    use super::*;

    fn frame(kind: &str) -> Value {
        json!({ "type": kind })
    }

    #[tokio::test]
    async fn a_subscriber_receives_what_the_connector_publishes() {
        let relay = TerminalRelay::new();
        relay.register("t1");
        let mut subscription = relay.subscribe("t1").expect("registered");
        assert!(!subscription.already_opened);

        assert!(relay.publish("t1", &frame("terminal.data")));
        assert_eq!(
            subscription.frames.recv().await.expect("frame")["type"],
            "terminal.data"
        );
    }

    #[tokio::test]
    async fn a_late_subscriber_is_told_the_terminal_already_opened() {
        let relay = TerminalRelay::new();
        relay.register("t1");
        relay.mark_opened("t1");
        assert!(relay.publish("t1", &frame("terminal.opened")));

        let subscription = relay.subscribe("t1").expect("registered");
        assert!(
            subscription.already_opened,
            "the ack must be replayable, not lost between agent and browser"
        );
        // The already-published frame is not replayed by the channel itself: the
        // flag is what the handler turns into a replayed terminal.opened.
        assert_eq!(subscription.frames.len(), 0);
    }

    #[tokio::test]
    async fn closing_ends_every_subscriber() {
        let relay = TerminalRelay::new();
        relay.register("t1");
        let mut subscription = relay.subscribe("t1").expect("registered");
        relay.mark_opened("t1");

        assert!(relay.close("t1"));
        assert!(!relay.close("t1"), "closing twice is not an error");
        assert!(matches!(
            subscription.frames.recv().await,
            Err(RecvError::Closed)
        ));
        assert!(relay.subscribe("t1").is_none());
        assert!(!relay.publish("t1", &frame("terminal.data")));
    }

    #[tokio::test]
    async fn an_unregistered_id_has_no_channel() {
        let relay = TerminalRelay::new();
        assert!(relay.subscribe("t-missing").is_none());
        assert!(!relay.publish("t-missing", &frame("terminal.data")));
        assert!(!relay.is_registered("t-missing"));
    }

    #[tokio::test]
    async fn a_slow_subscriber_lags_visibly_instead_of_losing_bytes_silently() {
        let relay = TerminalRelay::new();
        relay.register("t1");
        let mut subscription = relay.subscribe("t1").expect("registered");
        for _ in 0..(CHANNEL_CAPACITY + 8) {
            relay.publish("t1", &frame("terminal.data"));
        }
        assert!(
            matches!(subscription.frames.recv().await, Err(RecvError::Lagged(_))),
            "a lagged terminal stream must be reported, never silently truncated"
        );
    }
}

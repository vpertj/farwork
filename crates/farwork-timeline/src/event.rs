use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cursor::Cursor;
use crate::error::TimelineError;
use crate::hash::{canonical_json, sha256_hex};

/// The twelve event types a session stream may carry.
///
/// Values are byte-identical to upstream `core/events.py`/`api/*` emit sites. The
/// set is closed on purpose: a client can switch exhaustively, and an unknown type
/// arriving from a newer server is a protocol break, not something to ignore.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventType {
    /// A client subscribed to the session stream.
    #[serde(rename = "session.subscribed")]
    SessionSubscribed,
    /// Session metadata changed (title, status, takeover flag, ...).
    #[serde(rename = "session.meta.updated")]
    SessionMetaUpdated,
    /// A new timeline item appeared.
    #[serde(rename = "timeline.item_created")]
    TimelineItemCreated,
    /// An existing timeline item changed.
    #[serde(rename = "timeline.item_updated")]
    TimelineItemUpdated,
    /// The whole item list, replacing whatever the client holds.
    #[serde(rename = "timeline.snapshot")]
    TimelineSnapshot,
    /// Runtime state for the session changed.
    #[serde(rename = "runtime.state.updated")]
    RuntimeStateUpdated,
    /// The full notice list, replacing whatever the client holds.
    #[serde(rename = "runtime.notice.snapshot")]
    RuntimeNoticeSnapshot,
    /// A single notice changed.
    #[serde(rename = "runtime.notice.updated")]
    RuntimeNoticeUpdated,
    /// The effective capability set changed.
    #[serde(rename = "runtime.capability.updated")]
    RuntimeCapabilityUpdated,
    /// A model or permission catalog changed.
    #[serde(rename = "runtime.catalog.updated")]
    RuntimeCatalogUpdated,
    /// The runtime's own projection is untrustworthy; re-pull it.
    #[serde(rename = "runtime.refetch_required")]
    RuntimeRefetchRequired,
    /// The session projection is untrustworthy; re-pull it.
    #[serde(rename = "session.refetch_required")]
    SessionRefetchRequired,
}

impl EventType {
    /// Every event type, in the order upstream documents them.
    pub const ALL: [EventType; 12] = [
        Self::SessionSubscribed,
        Self::SessionMetaUpdated,
        Self::TimelineItemCreated,
        Self::TimelineItemUpdated,
        Self::TimelineSnapshot,
        Self::RuntimeStateUpdated,
        Self::RuntimeNoticeSnapshot,
        Self::RuntimeNoticeUpdated,
        Self::RuntimeCapabilityUpdated,
        Self::RuntimeCatalogUpdated,
        Self::RuntimeRefetchRequired,
        Self::SessionRefetchRequired,
    ];

    /// The wire value (`timeline.item_updated` and friends).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionSubscribed => "session.subscribed",
            Self::SessionMetaUpdated => "session.meta.updated",
            Self::TimelineItemCreated => "timeline.item_created",
            Self::TimelineItemUpdated => "timeline.item_updated",
            Self::TimelineSnapshot => "timeline.snapshot",
            Self::RuntimeStateUpdated => "runtime.state.updated",
            Self::RuntimeNoticeSnapshot => "runtime.notice.snapshot",
            Self::RuntimeNoticeUpdated => "runtime.notice.updated",
            Self::RuntimeCapabilityUpdated => "runtime.capability.updated",
            Self::RuntimeCatalogUpdated => "runtime.catalog.updated",
            Self::RuntimeRefetchRequired => "runtime.refetch_required",
            Self::SessionRefetchRequired => "session.refetch_required",
        }
    }

    /// Whether the event tells the client to discard its projection.
    pub const fn requires_refetch(self) -> bool {
        matches!(
            self,
            Self::RuntimeRefetchRequired | Self::SessionRefetchRequired
        )
    }

    /// Whether the event carries a complete item list rather than a single delta.
    pub const fn carries_snapshot(self) -> bool {
        matches!(self, Self::TimelineSnapshot | Self::RuntimeNoticeSnapshot)
    }
}

impl fmt::Display for EventType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for EventType {
    type Err = TimelineError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == raw)
            .ok_or_else(|| TimelineError::UnknownEnumValue {
                field: "event type",
                value: raw.to_owned(),
            })
    }
}

/// One event on a session's ordered stream.
///
/// `sequence` and `cursor` carry the same information in two shapes; both are
/// emitted because upstream does and because clients persist the string form while
/// storing the numeric form as `BIGINT`. `event_id` is derived from the sequence and
/// the canonical `(type, payload)` pair, so it is stable across replays and distinct
/// between two different payloads at the same sequence — a client can therefore
/// dedupe retries without trusting delivery order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventEnvelope {
    /// Stable id of the form `evt_<sequence>_<12 hex>`.
    pub event_id: String,
    /// Position in the session log; the log is dense, so this is also the count.
    pub sequence: i64,
    /// The same position in wire form.
    pub cursor: Cursor,
    /// Event type; serialised as `type`.
    #[serde(rename = "type")]
    pub kind: EventType,
    /// Session the event belongs to.
    pub session_id: String,
    /// RFC 3339 emission timestamp, supplied by the caller.
    pub emitted_at: String,
    /// Type-specific body.
    #[serde(default)]
    pub payload: Value,
}

impl EventEnvelope {
    /// Build an envelope, validating the sequence and deriving the event id.
    ///
    /// # Errors
    ///
    /// [TimelineError::SequenceOutOfRange] when `sequence` is negative or above
    /// [crate::PROTOCOL_MAX_REVISION].
    pub fn new(
        session_id: impl Into<String>,
        sequence: i64,
        kind: EventType,
        payload: Value,
        emitted_at: impl Into<String>,
    ) -> Result<Self, TimelineError> {
        let cursor = Cursor::new(sequence)?;
        let event_id = derive_event_id(sequence, kind, &payload);
        Ok(Self {
            event_id,
            sequence,
            cursor,
            kind,
            session_id: session_id.into(),
            emitted_at: emitted_at.into(),
            payload,
        })
    }

    /// Whether applying this event alone is enough to rebuild the client's state.
    pub const fn is_self_sufficient(&self) -> bool {
        self.kind.carries_snapshot() || self.kind.requires_refetch()
    }
}

/// `evt_<sequence>_<first 12 hex of sha256(type, payload)>`.
///
/// Truncated for the same reason upstream truncates: the id is an observability and
/// dedupe handle, not a security primitive, and 48 bits of digest collision
/// resistance is plenty for log lines that are also keyed by sequence.
fn derive_event_id(sequence: i64, kind: EventType, payload: &Value) -> String {
    let canonical = canonical_json(&Value::Array(vec![
        Value::String(kind.as_str().to_owned()),
        payload.clone(),
    ]));
    let digest = sha256_hex(canonical.as_bytes());
    format!("evt_{sequence}_{}", &digest[..12])
}

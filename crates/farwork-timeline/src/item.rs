use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::TimelineError;
use crate::hash::state_hash;

/// What a timeline item *is*, on the wire as `type`.
///
/// Values are byte-identical to upstream `models.py:TimelineType`; a connector that
/// already emits `"tool"` keeps working.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineItemKind {
    /// A chat message from the user or the agent.
    Message,
    /// A tool invocation and its outcome.
    Tool,
    /// A produced file, diff, or other artifact.
    Artifact,
    /// A structural separator in the conversation.
    Marker,
    /// Server-authored system note.
    System,
}

impl TimelineItemKind {
    /// The wire value, usable as a PostgreSQL text column value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Tool => "tool",
            Self::Artifact => "artifact",
            Self::Marker => "marker",
            Self::System => "system",
        }
    }
}

impl fmt::Display for TimelineItemKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for TimelineItemKind {
    type Err = TimelineError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw {
            "message" => Ok(Self::Message),
            "tool" => Ok(Self::Tool),
            "artifact" => Ok(Self::Artifact),
            "marker" => Ok(Self::Marker),
            "system" => Ok(Self::System),
            other => Err(TimelineError::UnknownEnumValue {
                field: "type",
                value: other.to_owned(),
            }),
        }
    }
}

/// Lifecycle status of an item, on the wire as `status`.
///
/// Status is part of the item's state hash, so a status-only transition is a real
/// change and must produce an event (see [crate::state_hash]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    /// Accepted but not started.
    Pending,
    /// Currently producing output.
    Running,
    /// Blocked on a human approval.
    WaitingApproval,
    /// Finished successfully.
    Done,
    /// Finished with an error.
    Failed,
    /// Stopped by a client request.
    Cancelled,
    /// Stopped because the session or runtime went away.
    Interrupted,
}

impl ItemStatus {
    /// The wire value, usable as a PostgreSQL text column value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::WaitingApproval => "waiting_approval",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

    /// Whether the item can still change on its own.
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Done | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }
}

impl fmt::Display for ItemStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ItemStatus {
    type Err = TimelineError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "waiting_approval" => Ok(Self::WaitingApproval),
            "done" => Ok(Self::Done),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "interrupted" => Ok(Self::Interrupted),
            other => Err(TimelineError::UnknownEnumValue {
                field: "status",
                value: other.to_owned(),
            }),
        }
    }
}

/// Who authored an item, on the wire as `role`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The human.
    User,
    /// The agent runtime.
    Assistant,
    /// Server-side narration.
    System,
    /// A tool result standing on its own.
    Tool,
}

impl Role {
    /// The wire value, usable as a PostgreSQL text column value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::System => "system",
            Self::Tool => "tool",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for Role {
    type Err = TimelineError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw {
            "user" => Ok(Self::User),
            "assistant" => Ok(Self::Assistant),
            "system" => Ok(Self::System),
            "tool" => Ok(Self::Tool),
            other => Err(TimelineError::UnknownEnumValue {
                field: "role",
                value: other.to_owned(),
            }),
        }
    }
}

/// One committed row of a session timeline.
///
/// Field names follow upstream `models.py:TimelineItem` on the wire (`camelCase`,
/// `type` for the kind, `content` for the payload) so the browser client and the
/// connector speak the same JSON without a rename layer. Three separate counters
/// exist because they answer three different questions:
///
/// * `order_seq` — where the item sits in the conversation. Runtime-assigned,
///   monotonic, and stable across updates so the UI never reorders on a text delta.
/// * `updated_seq` — the event sequence at which this revision was committed. This
///   is what a live stream uses to reconcile with a cursor.
/// * `revision` — how many times the item changed since creation, starting at 1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineItem {
    /// Stable item id, unique within the session.
    pub id: String,
    /// Session that owns the item.
    pub session_id: String,
    /// Item kind; serialised as `type`.
    #[serde(rename = "type")]
    pub kind: TimelineItemKind,
    /// Lifecycle status.
    pub status: ItemStatus,
    /// Author, when the kind has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    /// Display order within the session; `>= 1`.
    pub order_seq: i64,
    /// Event sequence at which this revision was committed; `0` only for an item
    /// that has been staged but not committed.
    pub updated_seq: i64,
    /// Monotonic change counter, starting at `1`.
    pub revision: i64,
    /// SHA-256 of the canonical state, see [crate::state_hash].
    pub content_hash: String,
    /// The runtime-owned content.
    ///
    /// Named `payload` in Rust because "content" collides with too much else in a
    /// Rust codebase, but serialised as `content` to match upstream. This is the
    /// field the content hash covers, and therefore the one whose repetition the
    /// store must not turn into an event.
    #[serde(rename = "content", default)]
    pub payload: Value,
    /// RFC 3339 creation timestamp, supplied by the caller.
    pub created_at: String,
    /// RFC 3339 timestamp of the last committed change, supplied by the caller.
    pub updated_at: String,
}

impl TimelineItem {
    /// Recompute the state hash from the item's current fields.
    ///
    /// Used to detect an item that was mutated outside the timeline API before a
    /// write is accepted.
    pub fn state_hash(&self) -> String {
        state_hash(self.kind, self.status, self.role, &self.payload)
    }

    /// Whether this item can no longer change.
    pub const fn is_terminal(&self) -> bool {
        self.status.is_terminal()
    }
}

/// A runtime-reported item state, before the timeline assigns sequence numbers.
///
/// The split exists so callers cannot invent `updated_seq`/`revision`/`content_hash`:
/// those are the timeline's own bookkeeping and a connector that guessed them would
/// desynchronise every client cursor. An input carries only what the runtime owns.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemInput {
    /// Stable item id, unique within the session.
    pub id: String,
    /// Item kind.
    pub kind: TimelineItemKind,
    /// Lifecycle status.
    pub status: ItemStatus,
    /// Author, when the kind has one.
    pub role: Option<Role>,
    /// Requested display order; `None` lets the timeline allocate the next slot.
    ///
    /// A re-report that omits this keeps the item's existing slot, which is what
    /// stops a text delta from reordering the conversation.
    pub order_seq: Option<i64>,
    /// The runtime-owned content.
    pub payload: Value,
    /// Optional RFC 3339 timestamp overriding the write's `now` argument.
    pub updated_at: Option<String>,
}

impl ItemInput {
    /// Start an input with [ItemStatus::Pending], no role, and no explicit order.
    pub fn new(id: impl Into<String>, kind: TimelineItemKind, payload: Value) -> Self {
        Self {
            id: id.into(),
            kind,
            status: ItemStatus::Pending,
            role: None,
            order_seq: None,
            payload,
            updated_at: None,
        }
    }

    /// Set the status.
    pub fn with_status(mut self, status: ItemStatus) -> Self {
        self.status = status;
        self
    }

    /// Set the author.
    pub fn with_role(mut self, role: Role) -> Self {
        self.role = Some(role);
        self
    }

    /// Pin an explicit display order instead of letting the timeline allocate one.
    pub fn with_order_seq(mut self, order_seq: i64) -> Self {
        self.order_seq = Some(order_seq);
        self
    }

    /// Override the write timestamp.
    pub fn with_updated_at(mut self, updated_at: impl Into<String>) -> Self {
        self.updated_at = Some(updated_at.into());
        self
    }

    /// The state hash this input would produce.
    ///
    /// `order_seq` is excluded: moving an item in the display order does not change
    /// what the item says, and the store compares the two fields separately (see
    /// [crate::Timeline::upsert]).
    pub fn state_hash(&self) -> String {
        state_hash(self.kind, self.status, self.role, &self.payload)
    }
}

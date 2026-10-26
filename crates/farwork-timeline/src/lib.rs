//! # `farwork-timeline` — session timeline as pure domain logic
//!
//! This crate owns the part of a session that is easiest to get subtly wrong and
//! hardest to notice: the ordering of timeline items, the event cursor that lets a
//! reconnecting client fill in exactly the events it missed, the content hash that
//! suppresses no-op writes, and the per-session write fence that stops a reader
//! from observing a half-applied write.
//!
//! It is deliberately IO-free: no HTTP, no PostgreSQL, no clock. Every timestamp is
//! supplied by the caller ([Timeline::upsert] takes `now`), which is what makes the
//! semantics testable without a database and reproducible in CI.
//!
//! ## The contract this crate implements
//!
//! * **Ordering.** Every committed change consumes exactly one event sequence
//!   number, so the log is dense: sequences `1..=cursor` always exist, never twice.
//!   `order_seq` is a separate, monotonically allocated *display* order for items.
//! * **Dedupe.** [TimelineItem::content_hash] is the SHA-256 of the canonical JSON of
//!   the item's runtime-owned state (type, status, role, payload). Re-reporting the
//!   same state commits nothing: no new sequence, no new event.
//! * **Cursors.** [Cursor] is the wire form `seq:N`. [Timeline::events_after]
//!   either returns a hole-free suffix of the log or explicitly refuses with
//!   [Recovery::RefetchRequired] — it never returns a partial delta, because a
//!   partial delta is silently wrong data on the client.
//! * **Fencing.** [SessionFence] serialises writers per session; readers only ever
//!   see committed snapshots (see [InMemoryTimeline] for the read policy).
//!
//! ## Deliberate alignment with the upstream Python server
//!
//! The wire shapes and the value sets mirror `agent_server/core/models.py` and
//! `agent_server/core/events.py` from anywhere-labs/Agents-Anywhere so a connector
//! can be moved over without a translation layer. The differences are listed on the
//! items that carry them ([TimelineItem::payload], [RefetchReason], [TimelineStore]).
#![forbid(unsafe_code)]
#![deny(missing_docs)]

/// The `seq:N` cursor a client persists to resume a stream.
pub mod cursor;
/// Failure modes, all of them pre-commit caller mistakes.
pub mod error;
/// The event envelope and the twelve event types.
pub mod event;
/// The per-session exclusive write lock.
pub mod fence;
/// Canonical JSON and the content hash that drives dedupe.
pub mod hash;
/// Timeline items, their kinds, statuses, and roles.
pub mod item;
/// The storage abstraction and the in-memory backend.
pub mod store;
/// Ordering, sequencing, dedupe, and cursor recovery.
pub mod timeline;

pub use cursor::Cursor;
pub use error::TimelineError;
pub use event::{EventEnvelope, EventType};
pub use fence::SessionFence;
pub use hash::{canonical_json, state_hash};
pub use item::{ItemInput, ItemStatus, Role, TimelineItem, TimelineItemKind};
pub use store::{InMemoryTimeline, TimelineStore};
pub use timeline::{
    classify_change, ApplyOutcome, ChangeKind, Recovery, RefetchReason, ReplaceOutcome, Timeline,
    TimelineSnapshot,
};

/// Build identifier of this crate, mostly useful in health output.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Largest value a sequence, cursor, or revision may take.
///
/// Upstream pins this to `2^53 - 1` (`PROTOCOL_MAX_REVISION`) because the browser
/// client stores sequences in IEEE-754 doubles. Keeping the same ceiling means a
/// sequence minted here is always exactly representable in JavaScript.
pub const PROTOCOL_MAX_REVISION: i64 = 9_007_199_254_740_991;

/// Prefix of the wire form of a cursor (`seq:N`).
pub const EVENT_CURSOR_PREFIX: &str = "seq:";

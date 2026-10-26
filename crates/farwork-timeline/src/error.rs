use crate::PROTOCOL_MAX_REVISION;

/// Every way a timeline operation can refuse to commit.
///
/// The variants are narrow on purpose: this layer performs no IO, so a failure is
/// always a caller mistake that a real backend must reject *before* it writes. That
/// ordering matters — a store that writes first and validates later leaves the log
/// with an event no reader can reconcile.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TimelineError {
    /// A cursor string was not the wire form `seq:<non-negative integer>`.
    #[error("invalid event cursor {value:?}: expected \"seq:<integer>\"")]
    InvalidCursor {
        /// The rejected input, verbatim, so the caller can echo it back in a 400.
        value: String,
    },

    /// A sequence, cursor, or revision left `0..=PROTOCOL_MAX_REVISION`.
    #[error("event sequence {sequence} is outside 0..={}", PROTOCOL_MAX_REVISION)]
    SequenceOutOfRange {
        /// The rejected value.
        sequence: i64,
    },

    /// An append targeted an item id that already exists in the session.
    #[error("item {id:?} is already present in session {session_id:?}")]
    DuplicateItem {
        /// Session the write was scoped to.
        session_id: String,
        /// Item id that already exists.
        id: String,
    },

    /// An update targeted an item id that the session does not hold.
    #[error("item {id:?} is not present in session {session_id:?}")]
    UnknownItem {
        /// Session the write was scoped to.
        session_id: String,
        /// Item id that is missing.
        id: String,
    },

    /// An explicit `order_seq` was below 1 and therefore unsortable.
    #[error("order_seq {order_seq} for item {id:?} must be >= 1")]
    InvalidOrderSeq {
        /// Item the caller tried to place.
        id: String,
        /// The rejected order sequence.
        order_seq: i64,
    },

    /// A refetch floor was outside the range the log actually spans.
    #[error("refetch floor {floor} must be within 0..={sequence}")]
    InvalidFloor {
        /// The rejected floor.
        floor: i64,
        /// The current head sequence, the upper bound of a legal floor.
        sequence: i64,
    },

    /// A text column could not be turned back into an enum variant.
    ///
    /// A PostgreSQL backend reads `type`/`status`/`role` back as text; a value that
    /// this crate cannot represent must fail loudly instead of being defaulted.
    #[error("unknown {field} value {value:?}")]
    UnknownEnumValue {
        /// Which column/field the value came from (`type`, `status`, `role`).
        field: &'static str,
        /// The unrecognised value.
        value: String,
    },
}

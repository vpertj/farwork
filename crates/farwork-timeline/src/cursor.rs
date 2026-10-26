use std::fmt;
use std::str::FromStr;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::TimelineError;
use crate::{EVENT_CURSOR_PREFIX, PROTOCOL_MAX_REVISION};

/// A position in a session's event log, wire-encoded as `seq:N`.
///
/// The cursor is the only thing a reconnecting client has to remember. Two
/// properties make it safe to persist:
///
/// * it is a plain count of committed events, not a timestamp or an opaque token
///   that a restart could invalidate;
/// * it round-trips through the exact upstream wire form, so a token minted by the
///   Python server remains readable (`seq:0` is the "nothing seen yet" cursor).
///
/// Constructing a `Cursor` validates the protocol ceiling, so every `Cursor` in
/// existence is a legal wire value and no downstream code has to re-check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Cursor {
    sequence: i64,
}

impl Cursor {
    /// The cursor a client holds before it has seen any event.
    pub const ZERO: Cursor = Cursor { sequence: 0 };

    /// Build a cursor from a sequence number, rejecting values above the ceiling.
    ///
    /// # Errors
    ///
    /// [TimelineError::SequenceOutOfRange] when `sequence` is negative or above
    /// [PROTOCOL_MAX_REVISION].
    pub fn new(sequence: i64) -> Result<Self, TimelineError> {
        if !(0..=PROTOCOL_MAX_REVISION).contains(&sequence) {
            return Err(TimelineError::SequenceOutOfRange { sequence });
        }
        Ok(Self { sequence })
    }

    /// Wrap a sequence that earlier validation already accepted.
    ///
    /// Only reachable inside this crate, where the value always comes from a
    /// previously validated commit sequence.
    pub(crate) const fn from_validated(sequence: i64) -> Self {
        Self { sequence }
    }

    /// The sequence number this cursor points at.
    pub const fn sequence(self) -> i64 {
        self.sequence
    }

    /// Parse the wire form `seq:N`.
    ///
    /// Strictness is intentional and mirrors `parse_event_cursor` upstream: no
    /// whitespace, no sign, no leading zeros, no trailing junk. A lenient parser
    /// here would let two distinct cursors mean the same position and hide a client
    /// bug behind "`seq:007` works too".
    ///
    /// # Errors
    ///
    /// [TimelineError::InvalidCursor] for anything that is not exactly
    /// `seq:` + canonical decimal digits, and [TimelineError::SequenceOutOfRange]
    /// for a numeric value above the protocol ceiling.
    pub fn parse(raw: &str) -> Result<Self, TimelineError> {
        let invalid = || TimelineError::InvalidCursor {
            value: raw.to_owned(),
        };
        let digits = raw.strip_prefix(EVENT_CURSOR_PREFIX).ok_or_else(invalid)?;
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid());
        }
        if digits.len() > 1 && digits.starts_with('0') {
            return Err(invalid());
        }
        let sequence = digits.parse::<i64>().map_err(|_| invalid())?;
        Self::new(sequence)
    }

    /// Render the wire form `seq:N`.
    pub fn to_wire(self) -> String {
        format!("{EVENT_CURSOR_PREFIX}{}", self.sequence)
    }
}

impl Default for Cursor {
    fn default() -> Self {
        Self::ZERO
    }
}

impl fmt::Display for Cursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_wire())
    }
}

impl FromStr for Cursor {
    type Err = TimelineError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::parse(raw)
    }
}

impl Serialize for Cursor {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_wire())
    }
}

impl<'de> Deserialize<'de> for Cursor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(D::Error::custom)
    }
}

//! ISO-8601 UTC timestamps in the shape the upstream Python server emits
//! (`datetime.now(UTC).isoformat()` with a `Z` suffix and microseconds).

use time::macros::format_description;
use time::OffsetDateTime;

const ISO_MICROS: &[time::format_description::BorrowedFormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:6]Z");

pub fn utc_now() -> String {
    format_utc(OffsetDateTime::now_utc())
}

pub fn format_utc(value: OffsetDateTime) -> String {
    value
        .format(ISO_MICROS)
        .unwrap_or_else(|_| String::from("1970-01-01T00:00:00.000000Z"))
}

/// Unix epoch seconds. Used for expiry columns and ticket lifetimes.
pub fn unix_now() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

pub fn iso_from_epoch(epoch: i64) -> String {
    match OffsetDateTime::from_unix_timestamp(epoch) {
        Ok(value) => format_utc(value),
        Err(_) => String::from("1970-01-01T00:00:00.000000Z"),
    }
}

//! RFC3339 timestamps in the shape the hub already emits, so agent-supplied
//! itemTime / heartbeat ts sort and parse identically on the server side.

use std::time::SystemTime;

use time::format_description::well_known::Rfc3339;
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

/// File timestamps: plain RFC3339, which is what `fs.response` entries advertise.
pub fn format_system_time(value: SystemTime) -> String {
    match OffsetDateTime::from(value).format(&Rfc3339) {
        Ok(text) => text,
        Err(_) => String::from("1970-01-01T00:00:00Z"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_is_fixed_width_rfc3339_with_z_suffix() {
        let now = utc_now();
        assert_eq!(now.len(), 27, "unexpected timestamp: {now}");
        assert!(now.ends_with('Z'));
        assert_eq!(now.as_bytes()[10], b'T');
        assert_eq!(now.as_bytes()[19], b'.');
    }

    #[test]
    fn epoch_formats_deterministically() {
        let value = OffsetDateTime::from_unix_timestamp(0).unwrap();
        assert_eq!(format_utc(value), "1970-01-01T00:00:00.000000Z");
    }
}

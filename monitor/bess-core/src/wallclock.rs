//! Wall-clock time for storage and display (S10.10; DD-06, DD-09, DD-11,
//! DD-95).
//!
//! Everything stored is UTC text in one fixed format,
//! `YYYY-MM-DDTHH:MM:SS.sssZ`, so text order is time order and SQLite can
//! compare timestamps as strings (DD-06). Local time is only for display and
//! for the daily record, which runs from 00:00 local to the next 00:00 local,
//! 23 or 25 hours on a DST day (DD-09). The time zone is passed in; on the Pi
//! it is the system time zone (DD-95), tests use `Europe/Amsterdam`.
//!
//! Nothing here decides whether the clock is valid (DD-11, DD-79): callers
//! only hand in a [`Timestamp`] once `clock_manager` says the clock is valid.

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};

/// One day in milliseconds, for retention windows (S10.7 counts days of
/// 24 h, not local days).
pub const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// A timestamp as stored: `YYYY-MM-DDTHH:MM:SS.sssZ` (DD-06). Sub-millisecond
/// digits are cut off, not rounded, so a stored time is never later than the
/// moment it describes.
#[must_use]
pub fn utc_text(ts: Timestamp) -> String {
    let dt = TimeZone::UTC.to_datetime(ts);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        dt.year(),
        dt.month(),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second(),
        dt.subsec_nanosecond() / 1_000_000
    )
}

/// Reads a stored timestamp back; `None` if the text is not a timestamp.
#[must_use]
pub fn parse_utc_text(text: &str) -> Option<Timestamp> {
    text.parse().ok()
}

/// A timestamp as shown and in the CSV `time_local` column:
/// `YYYY-MM-DD HH:MM:SS` in local time (S10.10, DD-49).
#[must_use]
pub fn local_text(ts: Timestamp, tz: &TimeZone) -> String {
    let dt = tz.to_datetime(ts);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        dt.year(),
        dt.month(),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second()
    )
}

/// The local date a timestamp falls on.
#[must_use]
pub fn local_date(ts: Timestamp, tz: &TimeZone) -> Date {
    tz.to_datetime(ts).date()
}

/// The UTC start and end of a local day: 00:00 local of `day` and of the day
/// after (DD-09). On a DST day the span is 23 or 25 hours. Where midnight
/// does not exist (a zone that springs forward at 00:00), the day starts at
/// the first local time after the gap.
///
/// # Errors
///
/// Only for a date at the very edge of the supported range (year −9999 or
/// 9999).
pub fn local_day_bounds(day: Date, tz: &TimeZone) -> Result<(Timestamp, Timestamp), jiff::Error> {
    let start = day.to_zoned_datetime(tz.clone())?.timestamp();
    let end = day.tomorrow()?.to_zoned_datetime(tz.clone())?.timestamp();
    Ok((start, end))
}

/// The next local midnight after `now`: when the daily job runs (DD-47).
///
/// # Errors
///
/// Only at the very edge of the supported range.
pub fn next_local_midnight(now: Timestamp, tz: &TimeZone) -> Result<Timestamp, jiff::Error> {
    Ok(local_day_bounds(local_date(now, tz), tz)?.1)
}

/// `ts` plus a number of milliseconds, saturating at the ends of the
/// supported range instead of failing.
#[must_use]
pub fn add_ms(ts: Timestamp, ms: i64) -> Timestamp {
    ts.checked_add(SignedDuration::from_millis(ms))
        .unwrap_or(if ms < 0 {
            Timestamp::MIN
        } else {
            Timestamp::MAX
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    fn amsterdam() -> TimeZone {
        TimeZone::get("Europe/Amsterdam").expect("tz database has Europe/Amsterdam")
    }

    fn ts(text: &str) -> Timestamp {
        text.parse().expect("valid timestamp")
    }

    #[test]
    fn utc_text_has_the_fixed_dd06_format() {
        assert_eq!(
            utc_text(ts("2026-11-24T09:05:03Z")),
            "2026-11-24T09:05:03.000Z"
        );
        assert_eq!(
            utc_text(ts("2026-01-02T03:04:05.678Z")),
            "2026-01-02T03:04:05.678Z"
        );
        // Cut off, not rounded up to the next second.
        assert_eq!(
            utc_text(ts("2026-12-31T23:59:59.9999Z")),
            "2026-12-31T23:59:59.999Z"
        );
        assert_eq!(
            utc_text(ts("2026-11-24T10:00:00+01:00")),
            "2026-11-24T09:00:00.000Z"
        );
    }

    #[test]
    fn utc_text_sorts_like_time() {
        let times = [
            "2026-03-29T00:59:59.999Z",
            "2026-03-29T01:00:00Z",
            "2026-10-25T00:00:00.001Z",
            "2027-01-01T00:00:00Z",
        ];
        let texts: Vec<String> = times.iter().map(|t| utc_text(ts(t))).collect();
        let mut sorted = texts.clone();
        sorted.sort();
        assert_eq!(texts, sorted);
    }

    #[test]
    fn stored_text_reads_back() {
        let t = ts("2026-07-01T12:34:56.789Z");
        assert_eq!(parse_utc_text(&utc_text(t)), Some(t));
        assert_eq!(parse_utc_text("not a time"), None);
    }

    #[test]
    fn local_text_shows_local_time() {
        // Winter UTC+1, summer UTC+2.
        assert_eq!(
            local_text(ts("2026-01-15T11:00:00Z"), &amsterdam()),
            "2026-01-15 12:00:00"
        );
        assert_eq!(
            local_text(ts("2026-07-15T11:00:00Z"), &amsterdam()),
            "2026-07-15 13:00:00"
        );
    }

    #[test]
    fn local_date_changes_at_local_midnight() {
        let tz = amsterdam();
        assert_eq!(
            local_date(ts("2026-01-14T22:59:59Z"), &tz),
            date(2026, 1, 14)
        );
        assert_eq!(
            local_date(ts("2026-01-14T23:00:00Z"), &tz),
            date(2026, 1, 15)
        );
    }

    #[test]
    fn normal_day_is_24_hours() {
        let (start, end) = local_day_bounds(date(2026, 1, 15), &amsterdam()).unwrap();
        assert_eq!(utc_text(start), "2026-01-14T23:00:00.000Z");
        assert_eq!(utc_text(end), "2026-01-15T23:00:00.000Z");
    }

    #[test]
    fn dst_days_are_23_and_25_hours() {
        let tz = amsterdam();
        // Spring forward on 2026-03-29: 23 h.
        let (start, end) = local_day_bounds(date(2026, 3, 29), &tz).unwrap();
        assert_eq!(utc_text(start), "2026-03-28T23:00:00.000Z");
        assert_eq!(utc_text(end), "2026-03-29T22:00:00.000Z");
        assert_eq!(end.duration_since(start), SignedDuration::from_hours(23));
        // Fall back on 2026-10-25: 25 h.
        let (start, end) = local_day_bounds(date(2026, 10, 25), &tz).unwrap();
        assert_eq!(utc_text(start), "2026-10-24T22:00:00.000Z");
        assert_eq!(utc_text(end), "2026-10-25T23:00:00.000Z");
        assert_eq!(end.duration_since(start), SignedDuration::from_hours(25));
    }

    #[test]
    fn day_starts_after_a_gap_at_midnight() {
        // America/Havana springs forward at 00:00 (2026-03-08): the day
        // starts at 01:00 local.
        let tz = TimeZone::get("America/Havana").unwrap();
        let (start, _) = local_day_bounds(date(2026, 3, 8), &tz).unwrap();
        assert_eq!(local_text(start, &tz), "2026-03-08 01:00:00");
    }

    #[test]
    fn next_midnight_is_the_end_of_today() {
        let tz = amsterdam();
        let next = next_local_midnight(ts("2026-01-15T12:00:00Z"), &tz).unwrap();
        assert_eq!(utc_text(next), "2026-01-15T23:00:00.000Z");
        // Exactly at midnight, the next one is a day later.
        let next = next_local_midnight(ts("2026-01-15T23:00:00Z"), &tz).unwrap();
        assert_eq!(utc_text(next), "2026-01-16T23:00:00.000Z");
    }

    #[test]
    fn add_ms_saturates() {
        assert_eq!(add_ms(Timestamp::MAX, 1), Timestamp::MAX);
        assert_eq!(add_ms(Timestamp::MIN, -1), Timestamp::MIN);
        assert_eq!(
            add_ms(ts("2026-01-01T00:00:00Z"), DAY_MS),
            ts("2026-01-02T00:00:00Z")
        );
    }
}

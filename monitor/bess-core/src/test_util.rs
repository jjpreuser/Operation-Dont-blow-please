//! Helpers shared by the unit tests of the modules. The integration tests
//! in `tests/` have their own copy in `tests/common`: they cannot see
//! `#[cfg(test)]` items of the library.

use jiff::tz::TimeZone;
use jiff::Timestamp;

pub fn ts(text: &str) -> Timestamp {
    text.parse().expect("valid timestamp")
}

pub fn amsterdam() -> TimeZone {
    TimeZone::get("Europe/Amsterdam").expect("tz database has Europe/Amsterdam")
}

pub fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

//! Shared test data and helpers: CONFIGs and frames built on the UART draft
//! reference CONFIG (`protocol::reference`), fault bitmaps, times.

// Each test file compiles this module separately and uses only part of it.
#![allow(dead_code)]

use jiff::tz::TimeZone;
use jiff::Timestamp;
use protocol::config::Config;
use protocol::messages::{FaultBits, FaultCode, Identity, Telemetry, TELEMETRY_LEN};
use protocol::reference;
use std::time::Duration;

pub const BOOT1: Identity = Identity {
    boot_counter: 1,
    config_revision: 1,
};
pub const BOOT2: Identity = Identity {
    boot_counter: 2,
    config_revision: 1,
};
pub const BOOT1_REV2: Identity = Identity {
    boot_counter: 1,
    config_revision: 2,
};

pub fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// The reference CONFIG with this identity and any further changes.
pub fn config_with(identity: Identity, edit: impl FnOnce(&mut Config)) -> Config {
    let mut config = reference::config();
    config.boot_counter = identity.boot_counter;
    config.config_revision = identity.config_revision;
    edit(&mut config);
    config
}

/// An all-zero TELEMETRY (state IDLE, no faults, nothing valid) with this
/// identity.
pub fn telemetry_frame(identity: Identity) -> Telemetry {
    let mut t = Telemetry::from_bytes(&[0; TELEMETRY_LEN]);
    t.boot_counter = identity.boot_counter;
    t.config_revision = identity.config_revision;
    t
}

/// The fault bitmap with these codes set.
pub fn bits(codes: &[u8]) -> FaultBits {
    codes
        .iter()
        .map(|&n| FaultCode::new(n).expect("code 1-32"))
        .collect()
}

pub fn ts(text: &str) -> Timestamp {
    text.parse().expect("valid timestamp")
}

pub fn amsterdam() -> TimeZone {
    TimeZone::get("Europe/Amsterdam").expect("tz database has Europe/Amsterdam")
}

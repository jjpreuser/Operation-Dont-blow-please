//! Shared test data: the UART draft reference CONFIG and blank frames.

// Each test file compiles this module separately and uses only part of it.
#![allow(dead_code)]

use protocol::config::Config;
use protocol::messages::{Identity, Telemetry, CONFIG_LEN, TELEMETRY_LEN};
use std::time::Duration;

/// CONFIG payload with the UART draft §6–7 example values (revision 1,
/// boot 1, firmware 0.0.0), as in `protocol/tests/common` without the frame
/// envelope. Fan fields: battery on 30.0 / off 27.0 °C, air on 35.0 / off
/// 32.0 °C, `fan_min_pct` 30, `fan_offline_action` 0, `fan_offline_ms`
/// 65535; battery table 27/30/40/45 °C at 0/30/65/100 %, air table
/// 32/35/40/45 °C at 0/30/65/100 %.
const REFERENCE_CONFIG: &str = "
    01 00 01 00 00 00 01 00 00 00 00 00 00 01 04 03 07 00
    80 25 00 00 00 00 10 0E 7A 0D 8C 0A 54 0B 28 23 00 00 64 19 00 00 7C 15
    00 00 7C 15 00 00 32 00 64 00 F4 01 C2 01 9C FF CE FF 26 02 F4 01 BC 02
    96 00 C2 01 90 01 E8 03 E8 03 88 13 10 27 D0 07 34 21 2C 01 0E 01 5E 01
    40 01 1E 46 88 13 FF FF E8 03 E8 03 88 13 B8 0B 04 04 00 00 0E 01 00 00
    00 2C 01 1E 00 00 90 01 41 00 00 C2 01 64 00 00 00 00 00 00 00 00 00 00
    00 00 00 00 00 00 00 00 00 00 00 00 40 01 00 00 00 5E 01 1E 00 00 90 01
    41 00 00 C2 01 64 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
    00 00 00 00";

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
    let bytes: Vec<u8> = REFERENCE_CONFIG
        .split_whitespace()
        .map(|b| u8::from_str_radix(b, 16).expect("hex byte"))
        .collect();
    let raw: [u8; CONFIG_LEN] = bytes.try_into().expect("CONFIG payload is 190 bytes");
    let mut config = Config::from_bytes(&raw);
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

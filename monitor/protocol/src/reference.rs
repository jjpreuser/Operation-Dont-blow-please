//! The example frames of UART draft V2 §10, as the hex text the draft
//! prints. Every crate's tests and the simulated MCU take the reference
//! CONFIG from here, so there is one copy.
//!
//! These are the draft's Python-generated vectors: provisional layout
//! checks, not the C-generated frames S5.11 still requires. Those will join
//! them here once the firmware team has them.

use crate::config::Config;
use crate::messages::CONFIG_LEN;

/// CONFIG with the §6–7 example values (revision 1, boot 1, firmware
/// 0.0.0). Fan fields: battery on 30.0 / off 27.0 °C, air on 35.0 / off
/// 32.0 °C, `fan_min_pct` 30, `fan_offline_action` 0, `fan_offline_ms`
/// 65535; battery table 27/30/40/45 °C at 0/30/65/100 %, air table
/// 32/35/40/45 °C at 0/30/65/100 %.
pub const CONFIG: &str = "
    AA 55 03 20 BE 00 01 00 01 00 00 00 01 00 00 00 00 00 00 01 04 03 07 00
    80 25 00 00 00 00 10 0E 7A 0D 8C 0A 54 0B 28 23 00 00 64 19 00 00 7C 15
    00 00 7C 15 00 00 32 00 64 00 F4 01 C2 01 9C FF CE FF 26 02 F4 01 BC 02
    96 00 C2 01 90 01 E8 03 E8 03 88 13 10 27 D0 07 34 21 2C 01 0E 01 5E 01
    40 01 1E 46 88 13 FF FF E8 03 E8 03 88 13 B8 0B 04 04 00 00 0E 01 00 00
    00 2C 01 1E 00 00 90 01 41 00 00 C2 01 64 00 00 00 00 00 00 00 00 00 00
    00 00 00 00 00 00 00 00 00 00 00 00 40 01 00 00 00 5E 01 1E 00 00 90 01
    41 00 00 C2 01 64 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
    00 00 00 00 0D 76";

/// `CONFIG_REQUEST`, sequence 0.
pub const CONFIG_REQUEST: &str = "AA 55 03 22 00 00 BA F7";

/// HEARTBEAT, boot 1, revision 1, sequence 0.
pub const HEARTBEAT: &str = "AA 55 03 10 08 00 01 00 00 00 01 00 00 00 77 AF";

/// TELEMETRY as published. Validity 0x07DF has bits 5 (current) and 9 (NTC4)
/// swapped against its description, see docs/design/firmware-requests.md §4.
pub const TELEMETRY_PUBLISHED: &str = "
    AA 55 03 01 26 00 E4 0C E4 0C E4 0C E4 0C 90 33 6A FF FD 00 FE 00 FF
    00 00 00 01 00 00 00 00 DF 07 00 01 00 00 00 01 00 00 00 7E 1D 3D E4";

/// TELEMETRY with validity 0x05FF, matching the draft's description
/// (current valid, NTC4 absent), firmware-requests.md §4.
pub const TELEMETRY_CORRECTED: &str = "
    AA 55 03 01 26 00 E4 0C E4 0C E4 0C E4 0C 90 33 6A FF FD 00 FE 00 FF
    00 00 00 01 00 00 00 00 FF 05 00 01 00 00 00 01 00 00 00 7E 1D BF CF";

/// The payload of [`CONFIG`]: without the 6-byte header and the CRC.
pub const CONFIG_PAYLOAD: [u8; CONFIG_LEN] = payload(&parse::<{ 6 + CONFIG_LEN + 2 }>(CONFIG));

/// [`CONFIG_PAYLOAD`] decoded.
#[must_use]
pub fn config() -> Config {
    Config::from_bytes(&CONFIG_PAYLOAD)
}

/// Hex text to bytes, at compile time: a typo in a constant above is a
/// build error, not a test failure.
const fn parse<const N: usize>(text: &str) -> [u8; N] {
    let text = text.as_bytes();
    let mut out = [0; N];
    let (mut i, mut n) = (0, 0);
    while i < text.len() {
        if text[i].is_ascii_whitespace() {
            i += 1;
        } else {
            assert!(n < N && i + 1 < text.len(), "too many hex bytes");
            out[n] = (nibble(text[i]) << 4) | nibble(text[i + 1]);
            n += 1;
            i += 2;
        }
    }
    assert!(n == N, "too few hex bytes");
    out
}

const fn nibble(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'A'..=b'F' => c - b'A' + 10,
        _ => panic!("not an upper-case hex digit"),
    }
}

const fn payload(frame: &[u8; 6 + CONFIG_LEN + 2]) -> [u8; CONFIG_LEN] {
    let mut out = [0; CONFIG_LEN];
    let mut i = 0;
    while i < CONFIG_LEN {
        out[i] = frame[6 + i];
        i += 1;
    }
    out
}

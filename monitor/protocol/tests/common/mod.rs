//! Reference frames from UART draft V2 §10 and test helpers.
//!
//! These are the draft's Python-generated vectors: provisional layout tests, not
//! the C-generated frames S5.11 still requires.

// Each test file compiles this module separately and uses only part of it.
#![allow(dead_code)]

use protocol::crc::crc16;

/// CONFIG with the §6–7 example values (revision 1, boot 1, firmware 0.0.0).
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

/// CONFIG_REQUEST, sequence 0.
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

/// Parses space-separated hex bytes.
pub fn hex(text: &str) -> Vec<u8> {
    text.split_whitespace()
        .map(|b| u8::from_str_radix(b, 16).expect("hex byte"))
        .collect()
}

/// Builds a frame with any version, id and length, for malformed-input tests.
/// The CRC is correct for the given bytes.
pub fn raw_frame(version: u8, id: u8, sequence: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0xAA, 0x55, version, id, payload.len() as u8, sequence];
    out.extend_from_slice(payload);
    let crc = crc16(&out[2..]);
    out.extend_from_slice(&crc.to_le_bytes());
    out
}

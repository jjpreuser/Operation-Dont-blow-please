//! The UART draft V2 §10 reference frames (`protocol::reference`) and test
//! helpers.

// Each test file compiles this module separately and uses only part of it.
#![allow(dead_code, unused_imports)]

use protocol::crc::crc16;

pub use protocol::reference::{
    CONFIG, CONFIG_REQUEST, HEARTBEAT, TELEMETRY_CORRECTED, TELEMETRY_PUBLISHED,
};

/// Parses space-separated hex bytes.
pub fn hex(text: &str) -> Vec<u8> {
    text.split_whitespace()
        .map(|b| u8::from_str_radix(b, 16).expect("hex byte"))
        .collect()
}

/// Builds a frame with any version, id and length, for malformed-input tests.
/// The CRC is correct for the given bytes.
pub fn raw_frame(version: u8, id: u8, sequence: u8, payload: &[u8]) -> Vec<u8> {
    let len = u8::try_from(payload.len()).expect("test payload fits the length byte");
    let mut out = vec![0xAA, 0x55, version, id, len, sequence];
    out.extend_from_slice(payload);
    let crc = crc16(&out[2..]);
    out.extend_from_slice(&crc.to_le_bytes());
    out
}

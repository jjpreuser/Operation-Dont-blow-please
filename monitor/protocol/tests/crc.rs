mod common;

use common::*;
use protocol::crc::crc16;

#[test]
fn check_vector() {
    assert_eq!(crc16(b"123456789"), 0x29B1);
}

#[test]
fn empty_input_is_init_value() {
    assert_eq!(crc16(&[]), 0xFFFF);
}

#[test]
fn matches_every_reference_frame() {
    for text in [
        CONFIG,
        CONFIG_REQUEST,
        HEARTBEAT,
        TELEMETRY_PUBLISHED,
        TELEMETRY_CORRECTED,
    ] {
        let frame = hex(text);
        let n = frame.len();
        let sent = u16::from_le_bytes([frame[n - 2], frame[n - 1]]);
        assert_eq!(crc16(&frame[2..n - 2]), sent, "frame {text}");
    }
}

#[test]
fn single_bit_flip_changes_crc() {
    let data = hex(HEARTBEAT);
    let good = crc16(&data[2..14]);
    for byte in 2..14 {
        for bit in 0..8 {
            let mut bad = data.clone();
            bad[byte] ^= 1 << bit;
            assert_ne!(crc16(&bad[2..14]), good, "byte {byte} bit {bit}");
        }
    }
}

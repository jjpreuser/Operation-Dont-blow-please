mod common;

use common::*;
use protocol::frame::{
    Direction, EncodeError, MAX_FRAME_LEN, MessageId, OVERHEAD, SequenceCounter, encode,
};

#[test]
fn ids_and_lengths_match_the_draft() {
    let table = [
        (0x01, MessageId::Telemetry, 38, Direction::McuToPi),
        (0x02, MessageId::FaultUpdate, 38, Direction::McuToPi),
        (0x10, MessageId::Heartbeat, 8, Direction::PiToMcu),
        (0x20, MessageId::Config, 190, Direction::McuToPi),
        (0x21, MessageId::ConfigAck, 9, Direction::PiToMcu),
        (0x22, MessageId::ConfigRequest, 0, Direction::PiToMcu),
        (0x30, MessageId::FaultReset, 12, Direction::PiToMcu),
        (0x31, MessageId::ResetAck, 13, Direction::McuToPi),
    ];
    for (byte, id, len, dir) in table {
        assert_eq!(MessageId::try_from(byte), Ok(id));
        assert_eq!(id as u8, byte);
        assert_eq!(id.payload_len(), len, "{id:?}");
        assert_eq!(id.direction(), dir, "{id:?}");
    }
    assert_eq!(MessageId::ALL.len(), table.len());
}

#[test]
fn unknown_ids_are_rejected() {
    let known: Vec<u8> = MessageId::ALL.iter().map(|id| *id as u8).collect();
    for byte in 0..=255u8 {
        match MessageId::try_from(byte) {
            Ok(id) => assert_eq!(id as u8, byte),
            Err(unknown) => assert_eq!(unknown, byte, "error returns the byte"),
        }
        assert_eq!(
            MessageId::try_from(byte).is_ok(),
            known.contains(&byte),
            "0x{byte:02X}"
        );
    }
}

#[test]
fn largest_frame_is_config() {
    assert_eq!(MAX_FRAME_LEN, 198);
    let largest = MessageId::ALL.iter().map(|id| id.payload_len()).max();
    assert_eq!(largest, Some(MessageId::Config.payload_len()));
    assert_eq!(MessageId::Config.payload_len() + OVERHEAD, MAX_FRAME_LEN);
}

#[test]
fn encodes_reference_config_request() {
    assert_eq!(
        encode(MessageId::ConfigRequest, 0, &[]).unwrap(),
        hex(CONFIG_REQUEST)
    );
}

#[test]
fn encodes_reference_heartbeat() {
    let payload = [1, 0, 0, 0, 1, 0, 0, 0];
    assert_eq!(
        encode(MessageId::Heartbeat, 0, &payload).unwrap(),
        hex(HEARTBEAT)
    );
}

#[test]
fn encodes_reference_telemetry_and_config() {
    for (id, text) in [
        (MessageId::Telemetry, TELEMETRY_PUBLISHED),
        (MessageId::Telemetry, TELEMETRY_CORRECTED),
        (MessageId::Config, CONFIG),
    ] {
        let frame = hex(text);
        let payload = &frame[6..frame.len() - 2];
        assert_eq!(encode(id, 0, payload).unwrap(), frame, "{id:?}");
    }
}

#[test]
fn sequence_is_written_at_offset_5() {
    let frame = encode(MessageId::ConfigRequest, 0xA7, &[]).unwrap();
    assert_eq!(frame[5], 0xA7);
}

#[test]
fn wrong_payload_length_is_an_error() {
    assert_eq!(
        encode(MessageId::Heartbeat, 0, &[0; 7]),
        Err(EncodeError::WrongLength {
            id: MessageId::Heartbeat,
            expected: 8,
            actual: 7
        })
    );
    assert!(encode(MessageId::ConfigRequest, 0, &[0]).is_err());
}

#[test]
fn sequence_counter_wraps_at_256() {
    let mut seq = SequenceCounter::new(254);
    assert_eq!(seq.next_sequence(), 254);
    assert_eq!(seq.next_sequence(), 255);
    assert_eq!(seq.next_sequence(), 0);
    assert_eq!(seq.next_sequence(), 1);
    assert_eq!(SequenceCounter::default().next_sequence(), 0);
}

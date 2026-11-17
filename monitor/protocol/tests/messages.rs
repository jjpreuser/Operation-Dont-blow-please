//! Typed payloads: reference frames, byte layout per field, unknown codes,
//! length errors and a round trip of every message through the parser.

mod common;

use common::{hex, CONFIG, CONFIG_REQUEST, HEARTBEAT, TELEMETRY_CORRECTED, TELEMETRY_PUBLISHED};
use protocol::frame::{Direction, MessageId};
use protocol::messages::{
    AckStatus, ConfigAck, DecodeError, FaultBits, FaultCode, FaultReset, Heartbeat, McuState,
    Message, ResetAck, ResetResult, Telemetry, Validity, CONFIG_LEN, TELEMETRY_LEN,
};
use protocol::parser::FrameParser;
use std::num::NonZeroU32;

/// Parses one complete frame and decodes it.
fn decode_frame(bytes: &[u8], accept: Direction) -> Message {
    let mut parser = FrameParser::new(accept);
    let frames = parser.push(bytes);
    assert_eq!(frames.len(), 1, "expected exactly one frame");
    Message::try_from(&frames[0]).expect("valid payload")
}

fn telemetry(message: Message) -> Telemetry {
    match message {
        Message::Telemetry(t) => t,
        other => panic!("expected Telemetry, got {other:?}"),
    }
}

/// A telemetry snapshot with a distinct value in every field.
fn distinct_telemetry() -> Telemetry {
    Telemetry {
        cell_mv: [0x0102, 0x0304, 0x0506, 0x0708],
        pack_mv: 0x090A,
        pack_current_10ma: -2, // FE FF
        temperature_deci_c: [0x0B0C, 0x0D0E, -0x0F10, 0x1112],
        state: McuState::Discharging,
        reported_faults: FaultBits(0x1314_1516),
        validity: Validity(0x1718),
        reserved: 0x19,
        boot_counter: 0x1A1B_1C1D,
        config_revision: 0x1E1F_2021,
        soc_centi_pct: 0x2223,
    }
}

// --- Reference TELEMETRY (§10) ---

/// The fields both reference frames share.
fn check_reference_values(t: Telemetry) {
    assert_eq!(t.cell_mv, [3300; 4]);
    assert_eq!(t.pack_mv, 13200);
    assert_eq!(t.pack_current_10ma, -150, "-1.50 A, charging");
    assert_eq!(t.temperature_deci_c, [253, 254, 255, 0]);
    assert_eq!(t.state, McuState::Charging);
    assert!(t.reported_faults.is_empty());
    assert_eq!(t.reserved, 0);
    assert_eq!(t.boot_counter, 1);
    assert_eq!(t.config_revision, 1);
    assert_eq!(t.soc_centi_pct, 7550, "75.50 %");
    assert_eq!(t.valid_soc_centi_pct(), Some(7550));
    for i in 0..4 {
        assert_eq!(t.valid_cell_mv(i), Some(3300));
    }
    assert_eq!(t.valid_pack_mv(), Some(13200));
    assert_eq!(t.valid_temperature_deci_c(0), Some(253));
    assert_eq!(t.valid_temperature_deci_c(1), Some(254));
    assert_eq!(t.valid_temperature_deci_c(2), Some(255));
}

#[test]
fn reference_telemetry_as_published() {
    let t = telemetry(decode_frame(&hex(TELEMETRY_PUBLISHED), Direction::McuToPi));
    check_reference_values(t);
    // Validity 0x07DF: what the bits say, not what the draft's text says
    // (firmware-requests.md §4).
    assert_eq!(t.validity, Validity(0x07DF));
    assert_eq!(t.valid_pack_current_10ma(), None);
    assert_eq!(t.valid_temperature_deci_c(3), Some(0));
}

#[test]
fn reference_telemetry_corrected() {
    let t = telemetry(decode_frame(&hex(TELEMETRY_CORRECTED), Direction::McuToPi));
    check_reference_values(t);
    assert_eq!(t.validity, Validity(0x05FF));
    assert_eq!(t.valid_pack_current_10ma(), Some(-150));
    assert_eq!(t.valid_temperature_deci_c(3), None, "NTC4 absent");
}

#[test]
fn reference_telemetry_re_encodes_exactly() {
    for reference in [TELEMETRY_PUBLISHED, TELEMETRY_CORRECTED] {
        let bytes = hex(reference);
        let message = decode_frame(&bytes, Direction::McuToPi);
        assert_eq!(message.encode(0), bytes);
    }
}

#[test]
fn soc_7550_is_7e_1d_at_offset_36() {
    let t = Telemetry {
        soc_centi_pct: 7550,
        ..distinct_telemetry()
    };
    assert_eq!(t.to_bytes()[36..38], [0x7E, 0x1D]);
}

// --- TELEMETRY layout ---

#[test]
fn telemetry_field_offsets() {
    let b = distinct_telemetry().to_bytes();
    let expected: [u8; TELEMETRY_LEN] = [
        0x02, 0x01, 0x04, 0x03, 0x06, 0x05, 0x08, 0x07, // cell_mv[0..4]
        0x0A, 0x09, // pack_mv
        0xFE, 0xFF, // pack_current -2
        0x0C, 0x0B, 0x0E, 0x0D, 0xF0, 0xF0, 0x12, 0x11, // temperature[0..4]
        0x02, // state DISCHARGING
        0x16, 0x15, 0x14, 0x13, // reported_faults
        0x18, 0x17, // validity
        0x19, // reserved
        0x1D, 0x1C, 0x1B, 0x1A, // boot_counter
        0x21, 0x20, 0x1F, 0x1E, // config_revision
        0x23, 0x22, // soc_cPct
    ];
    assert_eq!(b, expected);
    assert_eq!(Telemetry::from_bytes(&b), distinct_telemetry());
}

#[test]
fn fault_update_has_the_telemetry_layout() {
    let t = distinct_telemetry();
    let as_update = Message::FaultUpdate(t);
    assert_eq!(as_update.payload(), Message::Telemetry(t).payload());
    assert_eq!(
        Message::decode(MessageId::FaultUpdate, &t.to_bytes()),
        Ok(as_update)
    );
}

// --- Validity (DD-41, DD-68) ---

/// Which accessors return a value for a given validity.
fn present(t: Telemetry) -> Vec<&'static str> {
    let names = [
        ("cell1", t.valid_cell_mv(0).is_some()),
        ("cell2", t.valid_cell_mv(1).is_some()),
        ("cell3", t.valid_cell_mv(2).is_some()),
        ("cell4", t.valid_cell_mv(3).is_some()),
        ("pack", t.valid_pack_mv().is_some()),
        ("current", t.valid_pack_current_10ma().is_some()),
        ("ntc1", t.valid_temperature_deci_c(0).is_some()),
        ("ntc2", t.valid_temperature_deci_c(1).is_some()),
        ("ntc3", t.valid_temperature_deci_c(2).is_some()),
        ("ntc4", t.valid_temperature_deci_c(3).is_some()),
        ("soc", t.valid_soc_centi_pct().is_some()),
    ];
    names
        .into_iter()
        .filter_map(|(name, on)| on.then_some(name))
        .collect()
}

#[test]
fn each_validity_bit_gates_one_field() {
    let order = [
        "cell1", "cell2", "cell3", "cell4", "pack", "current", "ntc1", "ntc2", "ntc3", "ntc4",
        "soc",
    ];
    for (bit, name) in order.iter().enumerate() {
        let t = Telemetry {
            validity: Validity(1 << bit),
            ..distinct_telemetry()
        };
        assert_eq!(present(t), [*name], "bit {bit}");
    }
}

#[test]
fn validity_constants_cover_the_defined_bits() {
    let all = Validity::CELLS
        | Validity::PACK_VOLTAGE
        | Validity::CURRENT
        | Validity::NTCS
        | Validity::SOC;
    assert_eq!(all, Validity::DEFINED);
    let t = Telemetry {
        validity: Validity(Validity::DEFINED),
        ..distinct_telemetry()
    };
    assert_eq!(present(t).len(), 11);
}

#[test]
fn reserved_validity_bits_are_ignored() {
    let t = Telemetry {
        validity: Validity(0xF800),
        ..distinct_telemetry()
    };
    assert!(present(t).is_empty());
    // Kept as received, so it is logged and re-encoded unchanged.
    assert_eq!(
        Telemetry::from_bytes(&t.to_bytes()).validity,
        Validity(0xF800)
    );
}

#[test]
fn out_of_range_indexes_are_none() {
    let t = Telemetry {
        validity: Validity(0xFFFF),
        ..distinct_telemetry()
    };
    assert_eq!(t.valid_cell_mv(4), None);
    assert_eq!(t.valid_temperature_deci_c(4), None);
    assert!(!t.validity.cell(usize::MAX));
    assert!(!t.validity.ntc(usize::MAX));
}

#[test]
fn reserved_byte_27_is_kept_but_not_an_error() {
    let mut b = distinct_telemetry().to_bytes();
    b[27] = 0xA5;
    let t = Telemetry::from_bytes(&b);
    assert_eq!(t.reserved, 0xA5);
    assert_eq!(t.to_bytes(), b);
}

// --- State (DD-68) ---

#[test]
fn known_states() {
    let states = [
        McuState::Idle,
        McuState::Charging,
        McuState::Discharging,
        McuState::Fault,
        McuState::EStop,
    ];
    for (byte, state) in (0u8..).zip(states) {
        assert_eq!(McuState::from(byte), state);
        assert_eq!(u8::from(state), byte);
    }
}

#[test]
fn offline_and_other_states_are_unknown() {
    for byte in [5, 6, 0x7F, 0xFF] {
        let state = McuState::from(byte);
        assert_eq!(state, McuState::Unknown(byte));
        assert_eq!(u8::from(state), byte, "kept for logging");
    }
}

// --- Faults (§5, DD-68) ---

#[test]
fn fault_code_numbering() {
    assert_eq!(FaultCode::new(0), None);
    assert_eq!(FaultCode::new(33), None);
    let e01 = FaultCode::new(1).unwrap();
    let e19 = FaultCode::new(19).unwrap();
    let e20 = FaultCode::new(20).unwrap();
    let e32 = FaultCode::new(32).unwrap();
    assert_eq!(e01.mask(), 1);
    assert_eq!(e19.mask(), 1 << 18);
    assert_eq!(e32.mask(), 1 << 31);
    assert!(e19.is_defined());
    assert!(!e20.is_defined());
    assert_eq!(e01.to_string(), "E01");
    assert_eq!(e20.to_string(), "E20");
    assert_eq!(e32.number(), 32);
}

#[test]
fn fault_bits_list_codes_lowest_first() {
    let faults = FaultBits(0b1_0001); // E01 and E05
    let codes: Vec<u8> = faults.codes().map(FaultCode::number).collect();
    assert_eq!(codes, [1, 5]);
    assert_eq!(faults.undefined(), 0);
    assert!(!faults.is_empty());
    assert!(FaultBits(0).is_empty());
    assert_eq!(FaultBits(0).codes().count(), 0);
}

#[test]
fn every_defined_bit_maps_to_its_code() {
    for bit in 0..19u8 {
        let faults = FaultBits(1 << bit);
        let codes: Vec<FaultCode> = faults.codes().collect();
        assert_eq!(codes.len(), 1);
        assert_eq!(codes[0].number(), bit + 1);
        assert!(codes[0].is_defined());
        assert_eq!(faults.undefined(), 0);
    }
}

#[test]
fn reserved_fault_bits_are_unknown_codes() {
    let faults = FaultBits(0x8008_0000 | 0x0000_0800); // bits 31, 19 and E12
    let codes: Vec<String> = faults.codes().map(|c| c.to_string()).collect();
    assert_eq!(codes, ["E12", "E20", "E32"]);
    assert_eq!(faults.undefined(), 0x8008_0000);
    assert_eq!(FaultBits(u32::MAX).codes().count(), 32);
    assert_eq!(FaultBits(u32::MAX).undefined(), !FaultBits::DEFINED);
}

// --- Commands (§8) ---

#[test]
fn reference_heartbeat() {
    let bytes = hex(HEARTBEAT);
    let message = decode_frame(&bytes, Direction::PiToMcu);
    assert_eq!(
        message,
        Message::Heartbeat(Heartbeat {
            accepted_boot_counter: 1,
            accepted_config_revision: 1,
        })
    );
    assert_eq!(message.encode(0), bytes);
}

#[test]
fn heartbeat_layout_and_waiting() {
    let h = Heartbeat {
        accepted_boot_counter: 0x0403_0201,
        accepted_config_revision: 0x0807_0605,
    };
    assert_eq!(h.to_bytes(), [1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(Heartbeat::from_bytes(&h.to_bytes()), h);
    assert_eq!(Heartbeat::WAITING.to_bytes(), [0; 8]);
}

#[test]
fn reference_config_request() {
    let bytes = hex(CONFIG_REQUEST);
    let message = decode_frame(&bytes, Direction::PiToMcu);
    assert_eq!(message, Message::ConfigRequest);
    assert_eq!(message.encode(0), bytes);
}

#[test]
fn reference_config_is_carried_raw() {
    let bytes = hex(CONFIG);
    let message = decode_frame(&bytes, Direction::McuToPi);
    let Message::Config(raw) = &message else {
        panic!("expected Config, got {message:?}");
    };
    assert_eq!(raw.len(), CONFIG_LEN);
    assert_eq!(raw[..], bytes[6..6 + CONFIG_LEN]);
    assert_eq!(message.encode(0), bytes);
}

#[test]
fn config_ack_layout_and_statuses() {
    let ack = ConfigAck {
        boot_counter: 0x0403_0201,
        config_revision: 0x0807_0605,
        status: AckStatus::IdentityMismatch,
    };
    assert_eq!(ack.to_bytes(), [1, 2, 3, 4, 5, 6, 7, 8, 3]);
    assert_eq!(ConfigAck::from_bytes(&ack.to_bytes()), ack);

    let statuses = [
        AckStatus::Accepted,
        AckStatus::UnsupportedSchema,
        AckStatus::InvalidContent,
        AckStatus::IdentityMismatch,
    ];
    for (byte, status) in (0u8..).zip(statuses) {
        assert_eq!(AckStatus::from(byte), status);
        assert_eq!(u8::from(status), byte);
    }
    assert_eq!(AckStatus::from(4), AckStatus::Unknown(4));
    assert_eq!(u8::from(AckStatus::Unknown(0xEE)), 0xEE);
}

#[test]
fn fault_reset_layout() {
    let reset = FaultReset {
        request_id: NonZeroU32::new(0x0403_0201).unwrap(),
        target_boot_counter: 0x0807_0605,
        target_config_revision: 0x0C0B_0A09,
    };
    assert_eq!(reset.to_bytes(), [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
    assert_eq!(FaultReset::from_bytes(&reset.to_bytes()), Ok(reset));
}

#[test]
fn fault_reset_with_request_id_zero_is_rejected() {
    let mut payload = [0u8; 12];
    payload[4] = 1;
    assert_eq!(
        FaultReset::from_bytes(&payload),
        Err(DecodeError::ZeroRequestId)
    );
    assert_eq!(
        Message::decode(MessageId::FaultReset, &payload),
        Err(DecodeError::ZeroRequestId)
    );
}

#[test]
fn reset_ack_layout_and_results() {
    let ack = ResetAck {
        request_id: 0x0403_0201,
        boot_counter: 0x0807_0605,
        config_revision: 0x0C0B_0A09,
        result: ResetResult::ActiveFault,
    };
    assert_eq!(ack.to_bytes(), [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 1]);
    assert_eq!(ResetAck::from_bytes(&ack.to_bytes()), ack);

    let results = [
        ResetResult::Accepted,
        ResetResult::ActiveFault,
        ResetResult::IdentityMismatch,
        ResetResult::NotReady,
    ];
    for (byte, result) in (0u8..).zip(results) {
        assert_eq!(ResetResult::from(byte), result);
        assert_eq!(u8::from(result), byte);
    }
    assert_eq!(ResetResult::from(4), ResetResult::Unknown(4));
    assert_eq!(u8::from(ResetResult::Unknown(0x80)), 0x80);
}

// --- Every message ---

/// One example of every message, in `MessageId::ALL` order.
fn one_of_each() -> Vec<Message> {
    let mut config = [0u8; CONFIG_LEN];
    for (byte, value) in config.iter_mut().zip(0u8..) {
        *byte = value;
    }
    vec![
        Message::Telemetry(distinct_telemetry()),
        Message::FaultUpdate(Telemetry {
            reported_faults: FaultBits(1 << 6),
            state: McuState::Fault,
            ..distinct_telemetry()
        }),
        Message::Heartbeat(Heartbeat {
            accepted_boot_counter: 7,
            accepted_config_revision: 3,
        }),
        Message::Config(Box::new(config)),
        Message::ConfigAck(ConfigAck {
            boot_counter: 7,
            config_revision: 3,
            status: AckStatus::Accepted,
        }),
        Message::ConfigRequest,
        Message::FaultReset(FaultReset {
            request_id: NonZeroU32::new(42).unwrap(),
            target_boot_counter: 7,
            target_config_revision: 3,
        }),
        Message::ResetAck(ResetAck {
            request_id: 42,
            boot_counter: 7,
            config_revision: 3,
            result: ResetResult::NotReady,
        }),
    ]
}

#[test]
fn examples_cover_every_message_id() {
    let ids: Vec<MessageId> = one_of_each().iter().map(Message::id).collect();
    assert_eq!(ids, MessageId::ALL);
}

#[test]
fn payload_length_matches_the_id() {
    for message in one_of_each() {
        assert_eq!(
            message.payload().len(),
            message.id().payload_len(),
            "{message:?}"
        );
    }
}

#[test]
fn every_message_round_trips_through_the_parser() {
    for (sequence, message) in (0u8..).zip(one_of_each()) {
        let id = message.id();
        let bytes = message.encode(sequence);

        let mut parser = FrameParser::new(id.direction());
        let frames = parser.push(&bytes);
        assert_eq!(frames.len(), 1, "{id:?}");
        assert_eq!(frames[0].id, id);
        assert_eq!(frames[0].sequence, sequence);
        assert_eq!(Message::try_from(&frames[0]), Ok(message), "{id:?}");

        // The other side's parser drops it.
        let other = match id.direction() {
            Direction::McuToPi => Direction::PiToMcu,
            Direction::PiToMcu => Direction::McuToPi,
        };
        assert!(FrameParser::new(other).push(&bytes).is_empty(), "{id:?}");
    }
}

#[test]
fn wrong_payload_length_is_an_error() {
    for id in MessageId::ALL {
        let n = id.payload_len();
        let mut lengths = vec![n + 1];
        if n > 0 {
            lengths.push(n - 1);
        }
        for actual in lengths {
            assert_eq!(
                Message::decode(id, &vec![0; actual]),
                Err(DecodeError::WrongLength {
                    id,
                    expected: n,
                    actual,
                }),
                "{id:?} with {actual} bytes"
            );
        }
    }
}

#[test]
fn decode_error_messages() {
    let err = DecodeError::WrongLength {
        id: MessageId::Heartbeat,
        expected: 8,
        actual: 9,
    };
    assert_eq!(err.to_string(), "Heartbeat payload must be 8 bytes, got 9");
    assert_eq!(
        DecodeError::ZeroRequestId.to_string(),
        "FaultReset request_id must be nonzero"
    );
}

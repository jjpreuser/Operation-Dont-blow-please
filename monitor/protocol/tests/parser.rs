mod common;

use common::*;
use protocol::frame::{Direction, MessageId, encode};
use protocol::parser::{FrameParser, ParserStats};

fn pi() -> FrameParser {
    FrameParser::new(Direction::McuToPi)
}

fn ids(frames: &[protocol::frame::Frame]) -> Vec<MessageId> {
    frames.iter().map(|f| f.id).collect()
}

#[test]
fn decodes_reference_frames() {
    let mut p = pi();
    for (text, id) in [
        (TELEMETRY_PUBLISHED, MessageId::Telemetry),
        (TELEMETRY_CORRECTED, MessageId::Telemetry),
        (CONFIG, MessageId::Config),
    ] {
        let bytes = hex(text);
        let frames = p.push(&bytes);
        assert_eq!(frames.len(), 1, "{id:?}");
        assert_eq!(frames[0].id, id);
        assert_eq!(frames[0].sequence, 0);
        assert_eq!(frames[0].payload, &bytes[6..bytes.len() - 2]);
    }
    assert_eq!(p.stats().frames, 3);
    assert_eq!(p.stats().errors(), 0);
    assert_eq!(p.buffered(), 0);
}

#[test]
fn simulator_side_decodes_pi_frames() {
    let mut mcu = FrameParser::new(Direction::PiToMcu);
    let mut stream = hex(HEARTBEAT);
    stream.extend(hex(CONFIG_REQUEST));
    assert_eq!(
        ids(&mcu.push(&stream)),
        [MessageId::Heartbeat, MessageId::ConfigRequest]
    );
}

#[test]
fn one_byte_at_a_time() {
    let bytes = hex(CONFIG);
    let mut p = pi();
    let mut frames = Vec::new();
    for b in &bytes {
        frames.extend(p.push(std::slice::from_ref(b)));
    }
    assert_eq!(ids(&frames), [MessageId::Config]);
    assert_eq!(p.stats().discarded_bytes, 0);
}

#[test]
fn several_frames_in_one_read() {
    let mut stream = Vec::new();
    for seq in 0..5u8 {
        let frame = hex(TELEMETRY_CORRECTED);
        stream.extend(encode(MessageId::Telemetry, seq, &frame[6..44]).unwrap());
    }
    stream.extend(hex(CONFIG));
    let frames = pi().push(&stream);
    assert_eq!(frames.len(), 6);
    let seqs: Vec<u8> = frames[..5].iter().map(|f| f.sequence).collect();
    assert_eq!(seqs, [0, 1, 2, 3, 4]);
    assert_eq!(frames[5].id, MessageId::Config);
}

#[test]
fn every_split_point_gives_the_same_frames() {
    let mut stream = hex(TELEMETRY_CORRECTED);
    stream.extend(hex(CONFIG));
    for cut in 0..=stream.len() {
        let mut p = pi();
        let mut frames = p.push(&stream[..cut]);
        frames.extend(p.push(&stream[cut..]));
        assert_eq!(
            ids(&frames),
            [MessageId::Telemetry, MessageId::Config],
            "cut {cut}"
        );
    }
}

#[test]
fn garbage_before_and_between_frames_is_skipped() {
    let mut stream = b"\x00\xAAjunk\xAA\xAA".to_vec();
    stream.extend(hex(TELEMETRY_CORRECTED));
    stream.extend(b"\x55\x55\xAA");
    stream.extend(hex(TELEMETRY_CORRECTED));
    let mut p = pi();
    assert_eq!(ids(&p.push(&stream)), [MessageId::Telemetry; 2]);
    assert_eq!(p.stats().discarded_bytes, 8 + 3);
    assert_eq!(p.stats().errors(), 0);
}

#[test]
fn header_bytes_inside_a_payload_are_data() {
    // boot_counter = AA 55 03 01, which looks like the start of a TELEMETRY frame.
    let mut payload = hex(TELEMETRY_CORRECTED)[6..44].to_vec();
    payload[28..32].copy_from_slice(&[0xAA, 0x55, 0x03, 0x01]);
    let frame = encode(MessageId::FaultUpdate, 9, &payload).unwrap();
    let mut p = pi();
    let frames = p.push(&frame);
    assert_eq!(ids(&frames), [MessageId::FaultUpdate]);
    assert_eq!(frames[0].payload, payload);
    assert_eq!(
        p.stats(),
        ParserStats {
            frames: 1,
            ..Default::default()
        }
    );
}

#[test]
fn trailing_aa_waits_for_the_next_read() {
    let frame = hex(HEARTBEAT);
    let mut mcu = FrameParser::new(Direction::PiToMcu);
    assert!(mcu.push(b"xyz\xAA").is_empty());
    assert_eq!(mcu.buffered(), 1);
    assert_eq!(ids(&mcu.push(&frame[1..])), [MessageId::Heartbeat]);
}

#[test]
fn debug_shows_only_the_buffered_bytes() {
    let mut mcu = FrameParser::new(Direction::PiToMcu);
    mcu.push(b"\xAA");
    let text = format!("{mcu:?}");
    assert!(text.contains("accept: PiToMcu"), "{text}");
    assert!(text.contains("buffered: [170]"), "{text}");
}

#[test]
fn bad_crc_is_counted_and_the_next_frame_found() {
    let good = hex(TELEMETRY_CORRECTED);
    let mut bad = good.clone();
    bad[20] ^= 0x01;
    let mut stream = bad;
    stream.extend(&good);
    let mut p = pi();
    assert_eq!(ids(&p.push(&stream)), [MessageId::Telemetry]);
    assert_eq!(p.stats().crc_errors, 1);
    assert_eq!(p.stats().frames, 1);
}

#[test]
fn corrupted_crc_bytes_are_detected() {
    let mut frame = hex(CONFIG);
    let last = frame.len() - 1;
    frame[last] ^= 0x80;
    let mut p = pi();
    assert!(p.push(&frame).is_empty());
    assert_eq!(p.stats().crc_errors, 1);
}

#[test]
fn truncated_frame_does_not_hide_the_next_one() {
    // The first frame loses its last 10 bytes; its length field then swallows
    // the start of the next frame, the CRC fails and the parser resyncs.
    let good = hex(TELEMETRY_CORRECTED);
    let mut stream = good[..good.len() - 10].to_vec();
    stream.extend(&good);
    stream.extend(&good);
    let mut p = pi();
    let frames = p.push(&stream);
    assert_eq!(frames.len(), 2);
    assert_eq!(p.stats().crc_errors, 1);
}

#[test]
fn wrong_version_is_counted() {
    let payload = hex(TELEMETRY_CORRECTED)[6..44].to_vec();
    let mut stream = raw_frame(0x02, 0x01, 0, &payload);
    stream.extend(hex(TELEMETRY_CORRECTED));
    let mut p = pi();
    assert_eq!(ids(&p.push(&stream)), [MessageId::Telemetry]);
    assert_eq!(p.stats().version_errors, 1);
}

#[test]
fn unknown_id_is_counted() {
    let mut stream = raw_frame(0x03, 0x99, 0, &[1, 2, 3]);
    stream.extend(hex(TELEMETRY_CORRECTED));
    let mut p = pi();
    assert_eq!(ids(&p.push(&stream)), [MessageId::Telemetry]);
    assert_eq!(p.stats().unknown_id, 1);
}

#[test]
fn frame_for_the_other_direction_is_counted_as_unknown() {
    // The Pi never receives HEARTBEAT; the MCU never receives TELEMETRY.
    let mut p = pi();
    assert!(p.push(&hex(HEARTBEAT)).is_empty());
    assert_eq!(p.stats().unknown_id, 1);

    let mut mcu = FrameParser::new(Direction::PiToMcu);
    assert!(mcu.push(&hex(TELEMETRY_CORRECTED)).is_empty());
    assert_eq!(mcu.stats().unknown_id, 1);
}

#[test]
fn wrong_length_for_the_id_is_counted() {
    let payload = hex(TELEMETRY_CORRECTED)[6..43].to_vec(); // 37 bytes
    let mut stream = raw_frame(0x03, 0x01, 0, &payload);
    stream.extend(hex(TELEMETRY_CORRECTED));
    let mut p = pi();
    assert_eq!(ids(&p.push(&stream)), [MessageId::Telemetry]);
    assert_eq!(p.stats().length_errors, 1);
}

#[test]
fn random_noise_never_panics_and_the_buffer_stays_bounded() {
    // Simple LCG so the test is deterministic without a dependency.
    let mut state: u32 = 0x1234_5678;
    let mut noise = vec![0u8; 100_000];
    for b in &mut noise {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *b = (state >> 24) as u8;
        // Plenty of header bytes so many candidate frames are tried.
        if *b < 16 {
            *b = 0xAA;
        } else if *b < 32 {
            *b = 0x55;
        }
    }
    let mut p = pi();
    for chunk in noise.chunks(333) {
        p.push(chunk);
        assert!(p.buffered() < protocol::frame::MAX_FRAME_LEN);
    }
    // A valid frame after the noise is still found.
    let frames = p.push(&[hex(TELEMETRY_CORRECTED), hex(TELEMETRY_CORRECTED)].concat());
    assert!(!frames.is_empty());
    assert_eq!(frames.last().unwrap().id, MessageId::Telemetry);
}

#[test]
fn large_single_push_is_taken_in_pieces() {
    let one = hex(CONFIG);
    let stream: Vec<u8> = one.iter().copied().cycle().take(one.len() * 20).collect();
    let mut p = pi();
    assert_eq!(p.push(&stream).len(), 20);
    assert_eq!(p.buffered(), 0);
}

//! The simulated MCU end (feature `sim`): numbered frames with its identity,
//! CONFIG on request, the heartbeat it heard.
#![cfg(feature = "sim")]

use protocol::frame::Direction;
use protocol::messages::{Heartbeat, Identity, Message, Telemetry, TELEMETRY_LEN};
use protocol::parser::FrameParser;
use protocol::reference;
use protocol::sim::SimMcu;

fn decode(bytes: &[u8]) -> Vec<(u8, Message)> {
    FrameParser::new(Direction::McuToPi)
        .push(bytes)
        .iter()
        .map(|f| (f.sequence, Message::try_from(f).unwrap()))
        .collect()
}

fn pi_frame(message: &Message) -> Vec<u8> {
    message.encode(0)
}

#[test]
fn frames_are_numbered_and_carry_the_identity() {
    let mut mcu = SimMcu::new(reference::config());
    let mut bytes = mcu.config_frame();
    bytes.extend(mcu.telemetry(Telemetry::from_bytes(&[0; TELEMETRY_LEN])));
    bytes.extend(mcu.fault_update(Telemetry::from_bytes(&[0; TELEMETRY_LEN])));
    let frames = decode(&bytes);
    assert_eq!(
        frames.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert!(matches!(&frames[0].1, Message::Config(c) if **c == reference::CONFIG_PAYLOAD));
    for (_, m) in &frames[1..] {
        let (Message::Telemetry(t) | Message::FaultUpdate(t)) = m else {
            panic!("{m:?}");
        };
        assert_eq!(t.identity(), mcu.identity());
    }
}

#[test]
fn a_config_request_is_answered_with_the_config() {
    let mut mcu = SimMcu::new(reference::config());
    let heard = mcu.receive(&pi_frame(&Message::ConfigRequest));
    assert!(matches!(heard.messages[..], [Message::ConfigRequest]));
    assert!(matches!(
        &decode(&heard.reply)[..],
        [(0, Message::Config(_))]
    ));
}

#[test]
fn the_accepted_identity_comes_from_the_last_heartbeat() {
    let mut mcu = SimMcu::new(reference::config());
    assert_eq!(mcu.accepted(), None);
    mcu.receive(&pi_frame(&Message::Heartbeat(Heartbeat::WAITING)));
    assert_eq!(mcu.accepted(), None, "identity zero: the Pi waits");
    let accepted = Heartbeat {
        accepted_boot_counter: 1,
        accepted_config_revision: 1,
    };
    mcu.receive(&pi_frame(&Message::Heartbeat(accepted)));
    assert_eq!(mcu.accepted(), Some(mcu.identity()));

    mcu.restart();
    assert_eq!(
        mcu.identity(),
        Identity {
            boot_counter: 2,
            config_revision: 1
        }
    );
    assert_eq!(mcu.accepted(), None, "a reset forgets the heartbeat");
}

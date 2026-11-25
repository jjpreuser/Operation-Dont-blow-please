//! The MCU end of the link, simulated (feature `sim`): what the STM32 does
//! on the wire, with none of its battery logic. The display preview and the
//! `bess-core` end-to-end tests put their own battery behaviour on top.
//!
//! - Every frame it sends gets the next sequence number (§3).
//! - TELEMETRY and `FAULT_UPDATE` get its identity stamped in.
//! - It answers each `CONFIG_REQUEST` with its CONFIG (§8).
//! - It keeps the identity of the last HEARTBEAT it heard (DD-26).

use crate::config::Config;
use crate::frame::{Direction, SequenceCounter};
use crate::messages::{Heartbeat, Identity, Message, Telemetry};
use crate::parser::FrameParser;

/// The simulated MCU's side of the protocol.
pub struct SimMcu {
    config: Config,
    sequence: SequenceCounter,
    parser: FrameParser,
    heartbeat: Option<Heartbeat>,
}

/// What the MCU heard in some bytes from the Pi.
#[derive(Debug, Clone, Default)]
pub struct Heard {
    /// Every valid message, in order.
    pub messages: Vec<Message>,
    /// The CONFIG frames that answer the `CONFIG_REQUEST`s among them.
    pub reply: Vec<u8>,
}

impl SimMcu {
    /// An MCU that will send this CONFIG; its identity is the CONFIG's.
    #[must_use]
    pub fn new(config: Config) -> SimMcu {
        SimMcu {
            config,
            sequence: SequenceCounter::new(0),
            parser: FrameParser::new(Direction::PiToMcu),
            heartbeat: None,
        }
    }

    #[must_use]
    pub fn config(&self) -> &Config {
        &self.config
    }

    #[must_use]
    pub fn identity(&self) -> Identity {
        self.config.identity()
    }

    /// An MCU reset: the next `boot_counter`, and no heartbeat heard yet.
    pub fn restart(&mut self) {
        self.config.boot_counter += 1;
        self.heartbeat = None;
    }

    /// Encodes a message with the next sequence number.
    pub fn encode(&mut self, message: &Message) -> Vec<u8> {
        message.encode(self.sequence.next_sequence())
    }

    /// A TELEMETRY frame with this MCU's identity.
    pub fn telemetry(&mut self, t: Telemetry) -> Vec<u8> {
        let t = self.stamp(t);
        self.encode(&Message::Telemetry(t))
    }

    /// A `FAULT_UPDATE` frame with this MCU's identity.
    pub fn fault_update(&mut self, t: Telemetry) -> Vec<u8> {
        let t = self.stamp(t);
        self.encode(&Message::FaultUpdate(t))
    }

    /// A CONFIG frame.
    pub fn config_frame(&mut self) -> Vec<u8> {
        let message = Message::Config(Box::new(self.config.to_bytes()));
        self.encode(&message)
    }

    /// Bytes from the Pi.
    pub fn receive(&mut self, bytes: &[u8]) -> Heard {
        let mut heard = Heard::default();
        for frame in self.parser.push(bytes) {
            let Ok(message) = Message::try_from(&frame) else {
                continue;
            };
            match message {
                Message::ConfigRequest => {
                    let config = self.config_frame();
                    heard.reply.extend(config);
                }
                Message::Heartbeat(h) => self.heartbeat = Some(h),
                _ => {}
            }
            heard.messages.push(message);
        }
        heard
    }

    /// The identity the Pi accepted in the last HEARTBEAT; `None` before
    /// the first one, or while the Pi sends identity zero (it waits for a
    /// CONFIG).
    #[must_use]
    pub fn accepted(&self) -> Option<Identity> {
        self.heartbeat
            .filter(|h| *h != Heartbeat::WAITING)
            .map(|h| Identity {
                boot_counter: h.accepted_boot_counter,
                config_revision: h.accepted_config_revision,
            })
    }

    fn stamp(&self, mut t: Telemetry) -> Telemetry {
        t.boot_counter = self.config.boot_counter;
        t.config_revision = self.config.config_revision;
        t
    }
}

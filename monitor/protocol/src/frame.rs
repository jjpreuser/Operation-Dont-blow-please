//! Frame envelope (UART draft §3).
//!
//! ```text
//! offset 0-1   header            AA 55
//!        2     protocol_version  0x03
//!        3     message_id
//!        4     payload_length N  (exact per message id)
//!        5     sequence          per sender, +1 per frame, wraps at 256
//!        6..   payload           N bytes
//!        6+N   CRC               u16, low byte first, over offsets 2..=5+N
//! ```

use crate::crc::crc16;
use std::fmt;

pub const HEADER: [u8; 2] = [0xAA, 0x55];
pub const PROTOCOL_VERSION: u8 = 0x03;
/// Header, version, id, length and sequence.
pub const PREFIX_LEN: usize = 6;
/// Prefix plus CRC: total frame length = N + `OVERHEAD`.
pub const OVERHEAD: usize = PREFIX_LEN + 2;
/// Largest payload of any known message (CONFIG).
pub const MAX_PAYLOAD_LEN: usize = MessageId::Config.payload_len();
pub const MAX_FRAME_LEN: usize = MAX_PAYLOAD_LEN + OVERHEAD;

/// Which side sends a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    McuToPi,
    PiToMcu,
}

/// Message ids and their exact payload lengths (UART draft §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MessageId {
    Telemetry = 0x01,
    FaultUpdate = 0x02,
    Heartbeat = 0x10,
    Config = 0x20,
    ConfigAck = 0x21,
    ConfigRequest = 0x22,
    FaultReset = 0x30,
    ResetAck = 0x31,
}

impl MessageId {
    pub const ALL: [MessageId; 8] = [
        MessageId::Telemetry,
        MessageId::FaultUpdate,
        MessageId::Heartbeat,
        MessageId::Config,
        MessageId::ConfigAck,
        MessageId::ConfigRequest,
        MessageId::FaultReset,
        MessageId::ResetAck,
    ];

    /// The exact payload length N for this message.
    #[must_use]
    pub const fn payload_len(self) -> usize {
        match self {
            MessageId::Telemetry | MessageId::FaultUpdate => 38,
            MessageId::Heartbeat => 8,
            MessageId::Config => 190,
            MessageId::ConfigAck => 9,
            MessageId::ConfigRequest => 0,
            MessageId::FaultReset => 12,
            MessageId::ResetAck => 13,
        }
    }

    /// Which side sends this message.
    #[must_use]
    pub fn direction(self) -> Direction {
        match self {
            MessageId::Telemetry
            | MessageId::FaultUpdate
            | MessageId::Config
            | MessageId::ResetAck => Direction::McuToPi,
            MessageId::Heartbeat
            | MessageId::ConfigAck
            | MessageId::ConfigRequest
            | MessageId::FaultReset => Direction::PiToMcu,
        }
    }
}

impl TryFrom<u8> for MessageId {
    /// The unknown id byte.
    type Error = u8;

    fn try_from(value: u8) -> Result<MessageId, u8> {
        match value {
            0x01 => Ok(MessageId::Telemetry),
            0x02 => Ok(MessageId::FaultUpdate),
            0x10 => Ok(MessageId::Heartbeat),
            0x20 => Ok(MessageId::Config),
            0x21 => Ok(MessageId::ConfigAck),
            0x22 => Ok(MessageId::ConfigRequest),
            0x30 => Ok(MessageId::FaultReset),
            0x31 => Ok(MessageId::ResetAck),
            other => Err(other),
        }
    }
}

/// A frame that passed version, id, length and CRC checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub id: MessageId,
    pub sequence: u8,
    /// Exactly `id.payload_len()` bytes.
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    /// The payload length does not match the message id.
    WrongLength {
        id: MessageId,
        expected: usize,
        actual: usize,
    },
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EncodeError::WrongLength {
                id,
                expected,
                actual,
            } => write!(f, "{id:?} payload must be {expected} bytes, got {actual}"),
        }
    }
}

impl std::error::Error for EncodeError {}

/// The length byte is a `u8`, so every payload must fit in it.
const _: () = assert!(MAX_PAYLOAD_LEN <= u8::MAX as usize);

/// Builds a complete frame (header through CRC).
///
/// # Errors
///
/// [`EncodeError::WrongLength`] if `payload` is not exactly `id.payload_len()` bytes.
pub fn encode(id: MessageId, sequence: u8, payload: &[u8]) -> Result<Vec<u8>, EncodeError> {
    let expected = id.payload_len();
    if payload.len() != expected {
        return Err(EncodeError::WrongLength {
            id,
            expected,
            actual: payload.len(),
        });
    }
    Ok(build(id, sequence, payload))
}

/// [`encode`] without the length check, for callers whose payload type has
/// the exact length by construction (the typed messages).
pub(crate) fn build(id: MessageId, sequence: u8, payload: &[u8]) -> Vec<u8> {
    debug_assert_eq!(payload.len(), id.payload_len());
    let mut out = Vec::with_capacity(payload.len() + OVERHEAD);
    out.extend_from_slice(&HEADER);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "N <= MAX_PAYLOAD_LEN <= 255, asserted at compile time above"
    )]
    let len = id.payload_len() as u8;
    out.extend_from_slice(&[PROTOCOL_VERSION, id as u8, len, sequence]);
    out.extend_from_slice(payload);
    let crc = crc16(&out[2..]);
    out.extend_from_slice(&crc.to_le_bytes());
    out
}

/// Sender-side sequence counter: +1 per emitted frame, modulo 256.
#[derive(Debug, Clone, Default)]
pub struct SequenceCounter {
    next: u8,
}

impl SequenceCounter {
    /// A counter whose first frame gets sequence `first`.
    #[must_use]
    pub fn new(first: u8) -> SequenceCounter {
        SequenceCounter { next: first }
    }

    /// Returns the sequence for the next frame and advances the counter.
    pub fn next_sequence(&mut self) -> u8 {
        let current = self.next;
        self.next = self.next.wrapping_add(1);
        current
    }
}

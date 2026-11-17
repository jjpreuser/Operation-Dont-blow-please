//! Typed payloads (UART draft §4, §5 and §8), in wire units.
//!
//! Each payload type converts to and from its exact byte array; [`Message`]
//! ties them to a [`MessageId`] and to frames. Decoding checks the length and
//! nothing else: whether a value is plausible (for example a state of charge above
//! 100.00 %) is for the caller to judge. Codes this side does not know are
//! kept, not rejected (DD-68): a `state` outside 0–4, fault bits 19–31, an
//! unknown `CONFIG_ACK` status or `RESET_ACK` result.
//!
//! CONFIG is carried as its raw 190 bytes here; decoding and validating it
//! (§6–7) is the job of a separate module.

use crate::frame::{self, Frame, MessageId};
use std::fmt;
use std::num::NonZeroU32;

pub const TELEMETRY_LEN: usize = MessageId::Telemetry.payload_len();
pub const HEARTBEAT_LEN: usize = MessageId::Heartbeat.payload_len();
pub const CONFIG_LEN: usize = MessageId::Config.payload_len();
pub const CONFIG_ACK_LEN: usize = MessageId::ConfigAck.payload_len();
pub const FAULT_RESET_LEN: usize = MessageId::FaultReset.payload_len();
pub const RESET_ACK_LEN: usize = MessageId::ResetAck.payload_len();

// Little-endian field access at fixed offsets. Callers pass arrays of the exact
// payload length, so every offset is in range.

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn i16_at(b: &[u8], at: usize) -> i16 {
    i16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn put(b: &mut [u8], at: usize, bytes: &[u8]) {
    b[at..at + bytes.len()].copy_from_slice(bytes);
}

/// MCU state byte (TELEMETRY offset 20).
///
/// The draft also defines 5 = OFFLINE, but the Pi derives OFFLINE itself from
/// the link timeout (S8.12) and expects the MCU never to send it, so 5 decodes
/// as `Unknown(5)` like any other value outside 0–4 (DD-68,
/// docs/design/firmware-requests.md §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum McuState {
    Idle,
    Charging,
    Discharging,
    Fault,
    EStop,
    /// A byte outside 0–4, kept so it can be shown and logged.
    Unknown(u8),
}

impl From<u8> for McuState {
    fn from(value: u8) -> McuState {
        match value {
            0 => McuState::Idle,
            1 => McuState::Charging,
            2 => McuState::Discharging,
            3 => McuState::Fault,
            4 => McuState::EStop,
            other => McuState::Unknown(other),
        }
    }
}

impl From<McuState> for u8 {
    fn from(state: McuState) -> u8 {
        match state {
            McuState::Idle => 0,
            McuState::Charging => 1,
            McuState::Discharging => 2,
            McuState::Fault => 3,
            McuState::EStop => 4,
            McuState::Unknown(other) => other,
        }
    }
}

/// TELEMETRY `validity` bitmap (offset 25). A set bit means the field is
/// available and fresh; a clear bit means it is zero-filled and shown as N/A
/// (DD-41). Bits 11–15 are reserved and ignored (DD-68).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Validity(pub u16);

impl Validity {
    /// Bits 0–3: cell groups 1–4.
    pub const CELLS: u16 = 0x000F;
    pub const PACK_VOLTAGE: u16 = 1 << 4;
    pub const CURRENT: u16 = 1 << 5;
    /// Bits 6–9: NTC1–4.
    pub const NTCS: u16 = 0x03C0;
    pub const SOC: u16 = 1 << 10;
    /// Every defined bit (0–10).
    pub const DEFINED: u16 = 0x07FF;

    /// Cell group `index` (0–3); `false` for any other index.
    #[must_use]
    pub fn cell(self, index: usize) -> bool {
        index < 4 && self.0 & (1 << index) != 0
    }

    #[must_use]
    pub fn pack_voltage(self) -> bool {
        self.0 & Validity::PACK_VOLTAGE != 0
    }

    #[must_use]
    pub fn current(self) -> bool {
        self.0 & Validity::CURRENT != 0
    }

    /// NTC `index` (0–3 for NTC1–4); `false` for any other index.
    #[must_use]
    pub fn ntc(self, index: usize) -> bool {
        index < 4 && self.0 & (1 << (6 + index)) != 0
    }

    #[must_use]
    pub fn soc(self) -> bool {
        self.0 & Validity::SOC != 0
    }
}

/// A fault code: E01 is bit 0, so the number is bit + 1 (UART draft §5).
/// E01–E19 are defined; E20–E32 come from reserved bits and are shown and
/// logged as unknown faults (DD-68).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FaultCode(u8);

impl FaultCode {
    /// The highest defined code, E19.
    pub const LAST_DEFINED: u8 = 19;

    /// The code with this number, 1–32.
    #[must_use]
    pub const fn new(number: u8) -> Option<FaultCode> {
        if matches!(number, 1..=32) {
            Some(FaultCode(number))
        } else {
            None
        }
    }

    /// The number after the E (5 for E05).
    #[must_use]
    pub const fn number(self) -> u8 {
        self.0
    }

    /// The `reported_faults` bit for this code.
    #[must_use]
    pub const fn mask(self) -> u32 {
        1 << (self.0 - 1)
    }

    /// Whether the draft defines this code (E01–E19).
    #[must_use]
    pub const fn is_defined(self) -> bool {
        self.0 <= FaultCode::LAST_DEFINED
    }
}

impl fmt::Display for FaultCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "E{:02}", self.0)
    }
}

/// TELEMETRY `reported_faults` bitmap (offset 21): bit n set = code E(n+1)
/// asserted. The Pi's own E12 is merged in by the caller, not here (§5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FaultBits(pub u32);

impl FaultBits {
    /// Bits 0–18, E01–E19.
    pub const DEFINED: u32 = 0x0007_FFFF;

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[must_use]
    pub const fn contains(self, code: FaultCode) -> bool {
        self.0 & code.mask() != 0
    }

    /// The set reserved bits (19–31). Nonzero means firmware and Pi disagree
    /// on the catalogue.
    #[must_use]
    pub const fn undefined(self) -> u32 {
        self.0 & !FaultBits::DEFINED
    }

    /// Every asserted code, lowest first, unknown codes included.
    pub fn codes(self) -> impl Iterator<Item = FaultCode> {
        (1..=32)
            .filter_map(FaultCode::new)
            .filter(move |&code| self.contains(code))
    }
}

/// TELEMETRY and `FAULT_UPDATE` payload (§4, 38 bytes). Both carry the same
/// complete snapshot.
///
/// The fields are the raw wire values: a field whose validity bit is clear is
/// zero-filled on the wire, so read it through the `valid_*` accessors, which
/// return `None` in that case. `bess-core` converts to SI units and inverts
/// the current sign (DD-72).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Telemetry {
    /// Series cell groups 1–4, mV (offsets 0, 2, 4, 6).
    pub cell_mv: [u16; 4],
    /// Pack voltage, mV (offset 8).
    pub pack_mv: u16,
    /// Pack current, 10 mA per count, negative = charging (offset 10).
    pub pack_current_10ma: i16,
    /// NTC1–4, 0.1 °C per count (offsets 12–18): NTC1 core cell, NTC2
    /// terminal joint, NTC3 downstream air, NTC4 optional.
    pub temperature_deci_c: [i16; 4],
    /// Offset 20.
    pub state: McuState,
    /// Offset 21.
    pub reported_faults: FaultBits,
    /// Offset 25.
    pub validity: Validity,
    /// Offset 27. Not interpreted (DD-68); firmware-requests.md §2 proposes
    /// using it for a reset counter later.
    pub reserved: u8,
    /// MCU boot identity, same value as CONFIG (offset 28).
    pub boot_counter: u32,
    /// Same value as CONFIG (offset 32).
    pub config_revision: u32,
    /// State of charge, 0.01 % per count, 0–10000 (offset 36).
    pub soc_centi_pct: u16,
}

impl Telemetry {
    #[must_use]
    pub fn from_bytes(b: &[u8; TELEMETRY_LEN]) -> Telemetry {
        Telemetry {
            cell_mv: std::array::from_fn(|i| u16_at(b, 2 * i)),
            pack_mv: u16_at(b, 8),
            pack_current_10ma: i16_at(b, 10),
            temperature_deci_c: std::array::from_fn(|i| i16_at(b, 12 + 2 * i)),
            state: McuState::from(b[20]),
            reported_faults: FaultBits(u32_at(b, 21)),
            validity: Validity(u16_at(b, 25)),
            reserved: b[27],
            boot_counter: u32_at(b, 28),
            config_revision: u32_at(b, 32),
            soc_centi_pct: u16_at(b, 36),
        }
    }

    #[must_use]
    pub fn to_bytes(self) -> [u8; TELEMETRY_LEN] {
        let mut b = [0; TELEMETRY_LEN];
        for (i, mv) in self.cell_mv.iter().enumerate() {
            put(&mut b, 2 * i, &mv.to_le_bytes());
        }
        put(&mut b, 8, &self.pack_mv.to_le_bytes());
        put(&mut b, 10, &self.pack_current_10ma.to_le_bytes());
        for (i, t) in self.temperature_deci_c.iter().enumerate() {
            put(&mut b, 12 + 2 * i, &t.to_le_bytes());
        }
        b[20] = u8::from(self.state);
        put(&mut b, 21, &self.reported_faults.0.to_le_bytes());
        put(&mut b, 25, &self.validity.0.to_le_bytes());
        b[27] = self.reserved;
        put(&mut b, 28, &self.boot_counter.to_le_bytes());
        put(&mut b, 32, &self.config_revision.to_le_bytes());
        put(&mut b, 36, &self.soc_centi_pct.to_le_bytes());
        b
    }

    /// Cell group `index` (0–3) in mV, if valid.
    #[must_use]
    pub fn valid_cell_mv(self, index: usize) -> Option<u16> {
        self.validity.cell(index).then(|| self.cell_mv[index])
    }

    #[must_use]
    pub fn valid_pack_mv(self) -> Option<u16> {
        self.validity.pack_voltage().then_some(self.pack_mv)
    }

    /// In 10 mA, negative = charging.
    #[must_use]
    pub fn valid_pack_current_10ma(self) -> Option<i16> {
        self.validity.current().then_some(self.pack_current_10ma)
    }

    /// NTC `index` (0–3 for NTC1–4) in 0.1 °C, if valid.
    #[must_use]
    pub fn valid_temperature_deci_c(self, index: usize) -> Option<i16> {
        self.validity
            .ntc(index)
            .then(|| self.temperature_deci_c[index])
    }

    #[must_use]
    pub fn valid_soc_centi_pct(self) -> Option<u16> {
        self.validity.soc().then_some(self.soc_centi_pct)
    }
}

/// HEARTBEAT payload (§8, 8 bytes): the configuration the Pi has installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Heartbeat {
    pub accepted_boot_counter: u32,
    pub accepted_config_revision: u32,
}

impl Heartbeat {
    /// Identity zero: sent while the Pi has no accepted CONFIG.
    pub const WAITING: Heartbeat = Heartbeat {
        accepted_boot_counter: 0,
        accepted_config_revision: 0,
    };

    #[must_use]
    pub fn from_bytes(b: &[u8; HEARTBEAT_LEN]) -> Heartbeat {
        Heartbeat {
            accepted_boot_counter: u32_at(b, 0),
            accepted_config_revision: u32_at(b, 4),
        }
    }

    #[must_use]
    pub fn to_bytes(self) -> [u8; HEARTBEAT_LEN] {
        let mut b = [0; HEARTBEAT_LEN];
        put(&mut b, 0, &self.accepted_boot_counter.to_le_bytes());
        put(&mut b, 4, &self.accepted_config_revision.to_le_bytes());
        b
    }
}

/// `CONFIG_ACK` status byte (§8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AckStatus {
    Accepted,
    UnsupportedSchema,
    InvalidContent,
    IdentityMismatch,
    /// A byte outside 0–3.
    Unknown(u8),
}

impl From<u8> for AckStatus {
    fn from(value: u8) -> AckStatus {
        match value {
            0 => AckStatus::Accepted,
            1 => AckStatus::UnsupportedSchema,
            2 => AckStatus::InvalidContent,
            3 => AckStatus::IdentityMismatch,
            other => AckStatus::Unknown(other),
        }
    }
}

impl From<AckStatus> for u8 {
    fn from(status: AckStatus) -> u8 {
        match status {
            AckStatus::Accepted => 0,
            AckStatus::UnsupportedSchema => 1,
            AckStatus::InvalidContent => 2,
            AckStatus::IdentityMismatch => 3,
            AckStatus::Unknown(other) => other,
        }
    }
}

/// `CONFIG_ACK` payload (§8, 9 bytes): the Pi's verdict on a CONFIG.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigAck {
    pub boot_counter: u32,
    pub config_revision: u32,
    pub status: AckStatus,
}

impl ConfigAck {
    #[must_use]
    pub fn from_bytes(b: &[u8; CONFIG_ACK_LEN]) -> ConfigAck {
        ConfigAck {
            boot_counter: u32_at(b, 0),
            config_revision: u32_at(b, 4),
            status: AckStatus::from(b[8]),
        }
    }

    #[must_use]
    pub fn to_bytes(self) -> [u8; CONFIG_ACK_LEN] {
        let mut b = [0; CONFIG_ACK_LEN];
        put(&mut b, 0, &self.boot_counter.to_le_bytes());
        put(&mut b, 4, &self.config_revision.to_le_bytes());
        b[8] = u8::from(self.status);
        b
    }
}

/// `FAULT_RESET` payload (§8, 12 bytes): reset every eligible fault on the
/// MCU with this identity. `request_id` is nonzero, and a retry reuses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaultReset {
    pub request_id: NonZeroU32,
    pub target_boot_counter: u32,
    pub target_config_revision: u32,
}

impl FaultReset {
    /// # Errors
    ///
    /// [`DecodeError::ZeroRequestId`] if `request_id` is 0.
    pub fn from_bytes(b: &[u8; FAULT_RESET_LEN]) -> Result<FaultReset, DecodeError> {
        Ok(FaultReset {
            request_id: NonZeroU32::new(u32_at(b, 0)).ok_or(DecodeError::ZeroRequestId)?,
            target_boot_counter: u32_at(b, 4),
            target_config_revision: u32_at(b, 8),
        })
    }

    #[must_use]
    pub fn to_bytes(self) -> [u8; FAULT_RESET_LEN] {
        let mut b = [0; FAULT_RESET_LEN];
        put(&mut b, 0, &self.request_id.get().to_le_bytes());
        put(&mut b, 4, &self.target_boot_counter.to_le_bytes());
        put(&mut b, 8, &self.target_config_revision.to_le_bytes());
        b
    }
}

/// `RESET_ACK` result byte (§8). Accepted means the request passed the MCU's
/// checks, not that the outputs are on again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResetResult {
    Accepted,
    /// A fault is still active or its recovery is incomplete.
    ActiveFault,
    IdentityMismatch,
    /// Not ready, or the configuration is invalid.
    NotReady,
    /// A byte outside 0–3.
    Unknown(u8),
}

impl From<u8> for ResetResult {
    fn from(value: u8) -> ResetResult {
        match value {
            0 => ResetResult::Accepted,
            1 => ResetResult::ActiveFault,
            2 => ResetResult::IdentityMismatch,
            3 => ResetResult::NotReady,
            other => ResetResult::Unknown(other),
        }
    }
}

impl From<ResetResult> for u8 {
    fn from(result: ResetResult) -> u8 {
        match result {
            ResetResult::Accepted => 0,
            ResetResult::ActiveFault => 1,
            ResetResult::IdentityMismatch => 2,
            ResetResult::NotReady => 3,
            ResetResult::Unknown(other) => other,
        }
    }
}

/// `RESET_ACK` payload (§8, 13 bytes). `request_id` is the MCU's echo; the
/// caller matches it against its pending request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResetAck {
    pub request_id: u32,
    pub boot_counter: u32,
    pub config_revision: u32,
    pub result: ResetResult,
}

impl ResetAck {
    #[must_use]
    pub fn from_bytes(b: &[u8; RESET_ACK_LEN]) -> ResetAck {
        ResetAck {
            request_id: u32_at(b, 0),
            boot_counter: u32_at(b, 4),
            config_revision: u32_at(b, 8),
            result: ResetResult::from(b[12]),
        }
    }

    #[must_use]
    pub fn to_bytes(self) -> [u8; RESET_ACK_LEN] {
        let mut b = [0; RESET_ACK_LEN];
        put(&mut b, 0, &self.request_id.to_le_bytes());
        put(&mut b, 4, &self.boot_counter.to_le_bytes());
        put(&mut b, 8, &self.config_revision.to_le_bytes());
        b[12] = u8::from(self.result);
        b
    }
}

/// Any message of protocol 0x03.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Telemetry(Telemetry),
    FaultUpdate(Telemetry),
    Heartbeat(Heartbeat),
    /// The raw CONFIG payload, not yet decoded or validated (§6–7).
    Config(Box<[u8; CONFIG_LEN]>),
    ConfigAck(ConfigAck),
    ConfigRequest,
    FaultReset(FaultReset),
    ResetAck(ResetAck),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// The payload length does not match the message id.
    WrongLength {
        id: MessageId,
        expected: usize,
        actual: usize,
    },
    /// A `FAULT_RESET` with `request_id` 0, which §8 forbids.
    ZeroRequestId,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::WrongLength {
                id,
                expected,
                actual,
            } => write!(f, "{id:?} payload must be {expected} bytes, got {actual}"),
            DecodeError::ZeroRequestId => write!(f, "FaultReset request_id must be nonzero"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// `payload` as an array of exactly `N` bytes.
fn exact<const N: usize>(id: MessageId, payload: &[u8]) -> Result<&[u8; N], DecodeError> {
    payload.try_into().map_err(|_| DecodeError::WrongLength {
        id,
        expected: N,
        actual: payload.len(),
    })
}

impl Message {
    #[must_use]
    pub fn id(&self) -> MessageId {
        match self {
            Message::Telemetry(_) => MessageId::Telemetry,
            Message::FaultUpdate(_) => MessageId::FaultUpdate,
            Message::Heartbeat(_) => MessageId::Heartbeat,
            Message::Config(_) => MessageId::Config,
            Message::ConfigAck(_) => MessageId::ConfigAck,
            Message::ConfigRequest => MessageId::ConfigRequest,
            Message::FaultReset(_) => MessageId::FaultReset,
            Message::ResetAck(_) => MessageId::ResetAck,
        }
    }

    /// Decodes the payload of a message with this id.
    ///
    /// # Errors
    ///
    /// [`DecodeError::WrongLength`] if `payload` is not exactly
    /// `id.payload_len()` bytes; [`DecodeError::ZeroRequestId`] for a
    /// `FAULT_RESET` with request id 0.
    pub fn decode(id: MessageId, payload: &[u8]) -> Result<Message, DecodeError> {
        Ok(match id {
            MessageId::Telemetry => Message::Telemetry(Telemetry::from_bytes(exact(id, payload)?)),
            MessageId::FaultUpdate => {
                Message::FaultUpdate(Telemetry::from_bytes(exact(id, payload)?))
            }
            MessageId::Heartbeat => Message::Heartbeat(Heartbeat::from_bytes(exact(id, payload)?)),
            MessageId::Config => Message::Config(Box::new(*exact(id, payload)?)),
            MessageId::ConfigAck => Message::ConfigAck(ConfigAck::from_bytes(exact(id, payload)?)),
            MessageId::ConfigRequest => {
                exact::<0>(id, payload)?;
                Message::ConfigRequest
            }
            MessageId::FaultReset => {
                Message::FaultReset(FaultReset::from_bytes(exact(id, payload)?)?)
            }
            MessageId::ResetAck => Message::ResetAck(ResetAck::from_bytes(exact(id, payload)?)),
        })
    }

    /// The payload bytes, exactly `self.id().payload_len()` long.
    #[must_use]
    pub fn payload(&self) -> Vec<u8> {
        match self {
            Message::Telemetry(t) | Message::FaultUpdate(t) => t.to_bytes().to_vec(),
            Message::Heartbeat(h) => h.to_bytes().to_vec(),
            Message::Config(raw) => raw.to_vec(),
            Message::ConfigAck(a) => a.to_bytes().to_vec(),
            Message::ConfigRequest => Vec::new(),
            Message::FaultReset(r) => r.to_bytes().to_vec(),
            Message::ResetAck(a) => a.to_bytes().to_vec(),
        }
    }

    /// The complete frame, header through CRC.
    #[must_use]
    pub fn encode(&self, sequence: u8) -> Vec<u8> {
        frame::build(self.id(), sequence, &self.payload())
    }
}

impl TryFrom<&Frame> for Message {
    type Error = DecodeError;

    fn try_from(frame: &Frame) -> Result<Message, DecodeError> {
        Message::decode(frame.id, &frame.payload)
    }
}

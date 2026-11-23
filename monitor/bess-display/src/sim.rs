//! A simulated MCU for the preview: it speaks the real UART protocol (bytes
//! with CRC, via the `protocol` crate), so the Pi side runs exactly the code
//! it will run against the STM32. It is not a model of the BMS firmware:
//! the protection logic here is a rough stand-in so the screen can show each
//! state.
//!
//! - TELEMETRY every 1 s; a `FAULT_UPDATE` at once when the fault set changes.
//! - One CONFIG after start-up and one on every `CONFIG_REQUEST` (§8).
//! - "Restart" gives a new `boot_counter`; "Cut link" stops all frames.
//! - Faults: E05 above 55 °C (clear at 50 °C), E17 above 45 °C air (clear at
//!   40 °C), E08 with E-STOP, E09 with "NTC1 sensor fault", E13 with
//!   "Cell imbalance", E24 with "Unknown fault bit".
//! - `SoC` moves 60 times faster than real, so charging is visible.

use protocol::config::Config;
use protocol::frame::{Direction, SequenceCounter};
use protocol::messages::{
    FaultBits, FaultCode, Heartbeat, Identity, McuState, Message, Telemetry, Validity, CONFIG_LEN,
    TELEMETRY_LEN,
};
use protocol::parser::FrameParser;
use std::time::{Duration, Instant};

/// UART draft §6–7 example CONFIG (the same bytes as the protocol crate's
/// reference frame, without the envelope).
const REFERENCE_CONFIG: &str = "
    01 00 01 00 00 00 01 00 00 00 00 00 00 01 04 03 07 00
    80 25 00 00 00 00 10 0E 7A 0D 8C 0A 54 0B 28 23 00 00 64 19 00 00 7C 15
    00 00 7C 15 00 00 32 00 64 00 F4 01 C2 01 9C FF CE FF 26 02 F4 01 BC 02
    96 00 C2 01 90 01 E8 03 E8 03 88 13 10 27 D0 07 34 21 2C 01 0E 01 5E 01
    40 01 1E 46 88 13 FF FF E8 03 E8 03 88 13 B8 0B 04 04 00 00 0E 01 00 00
    00 2C 01 1E 00 00 90 01 41 00 00 C2 01 64 00 00 00 00 00 00 00 00 00 00
    00 00 00 00 00 00 00 00 00 00 00 00 40 01 00 00 00 5E 01 1E 00 00 90 01
    41 00 00 C2 01 64 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
    00 00 00 00";

const TELEMETRY_PERIOD: Duration = Duration::from_secs(1);
/// `SoC` runs this much faster than real time.
const SOC_SPEED_UP: f64 = 60.0;

/// What the battery is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Idle,
    Charging,
    Discharging,
}

/// The demo controls.
#[derive(Debug, Clone, Copy, PartialEq)]
#[expect(clippy::struct_excessive_bools, reason = "independent demo checkboxes")]
pub struct Controls {
    pub mode: Mode,
    /// Magnitude of the current while charging or discharging, A.
    pub current_a: f64,
    /// NTC1, °C.
    pub cell_temp_c: f64,
    /// NTC3, °C.
    pub air_temp_c: f64,
    pub e_stop: bool,
    pub ntc1_fault: bool,
    pub imbalance: bool,
    pub unknown_fault: bool,
    /// No frames at all from the MCU.
    pub link_cut: bool,
}

impl Default for Controls {
    fn default() -> Controls {
        Controls {
            mode: Mode::Charging,
            current_a: 8.0,
            cell_temp_c: 31.0,
            air_temp_c: 28.0,
            e_stop: false,
            ntc1_fault: false,
            imbalance: false,
            unknown_fault: false,
            link_cut: false,
        }
    }
}

fn code(n: u8) -> u32 {
    FaultCode::new(n).map_or(0, FaultCode::mask)
}

/// The simulated MCU.
pub struct SimMcu {
    pub controls: Controls,
    config: Config,
    boot_counter: u32,
    sequence: SequenceCounter,
    rx: FrameParser,
    next_telemetry: Instant,
    last_step: Instant,
    soc_pct: f64,
    e05: bool,
    e17: bool,
    last_faults: u32,
    /// Identity of the latest heartbeat from the Pi.
    accepted: Option<Identity>,
    last_heartbeat: Option<Instant>,
    /// Bytes to send to the Pi.
    outbox: Vec<u8>,
}

impl SimMcu {
    #[must_use]
    pub fn new(now: Instant) -> SimMcu {
        let bytes: Vec<u8> = REFERENCE_CONFIG
            .split_whitespace()
            .filter_map(|b| u8::from_str_radix(b, 16).ok())
            .collect();
        let raw: [u8; CONFIG_LEN] = bytes.try_into().unwrap_or([0; CONFIG_LEN]);
        let mut sim = SimMcu {
            controls: Controls::default(),
            config: Config::from_bytes(&raw),
            boot_counter: 0,
            sequence: SequenceCounter::new(0),
            rx: FrameParser::new(Direction::PiToMcu),
            next_telemetry: now,
            last_step: now,
            soc_pct: 42.0,
            e05: false,
            e17: false,
            last_faults: 0,
            accepted: None,
            last_heartbeat: None,
            outbox: Vec::new(),
        };
        sim.restart(now);
        sim
    }

    /// An MCU reset: new boot counter, CONFIG sent once on its own (§8).
    pub fn restart(&mut self, now: Instant) {
        self.boot_counter += 1;
        self.config.boot_counter = self.boot_counter;
        self.accepted = None;
        self.next_telemetry = now + Duration::from_millis(300);
        self.send_config();
    }

    #[must_use]
    pub fn identity(&self) -> Identity {
        self.config.identity()
    }

    /// The identity the Pi reports in its heartbeat, if it is recent.
    #[must_use]
    pub fn pi_heartbeat(&self, now: Instant) -> Option<Identity> {
        self.last_heartbeat
            .filter(|t| now.saturating_duration_since(*t) <= Duration::from_secs(5))
            .and(self.accepted)
    }

    #[must_use]
    pub fn soc_pct(&self) -> f64 {
        self.soc_pct
    }

    fn send(&mut self, message: &Message) {
        if !self.controls.link_cut {
            let frame = message.encode(self.sequence.next_sequence());
            self.outbox.extend(frame);
        }
    }

    fn send_config(&mut self) {
        let message = Message::Config(Box::new(self.config.to_bytes()));
        self.send(&message);
    }

    /// Bytes from the Pi.
    pub fn receive(&mut self, bytes: &[u8], now: Instant) {
        for frame in self.rx.push(bytes) {
            match Message::try_from(&frame) {
                Ok(Message::ConfigRequest) => self.send_config(),
                Ok(Message::Heartbeat(h)) => {
                    self.last_heartbeat = Some(now);
                    self.accepted = (h != Heartbeat::WAITING).then_some(Identity {
                        boot_counter: h.accepted_boot_counter,
                        config_revision: h.accepted_config_revision,
                    });
                }
                _ => {}
            }
        }
    }

    /// Advances the simulation and returns the bytes for the Pi.
    pub fn step(&mut self, now: Instant) -> Vec<u8> {
        let dt = now.saturating_duration_since(self.last_step).as_secs_f64();
        self.last_step = now;
        let c = self.controls;

        // Rough stand-in for the MCU protection, with hysteresis.
        self.e05 = if self.e05 {
            c.cell_temp_c > 50.0
        } else {
            c.cell_temp_c > 55.0
        };
        self.e17 = if self.e17 {
            c.air_temp_c > 40.0
        } else {
            c.air_temp_c > 45.0
        };
        let faults = [
            (self.e05, 5),
            (c.e_stop, 8),
            (c.ntc1_fault, 9),
            (c.imbalance, 13),
            (self.e17, 17),
            (c.unknown_fault, 24),
        ]
        .iter()
        .filter(|(on, _)| *on)
        .map(|&(_, n)| code(n))
        .sum::<u32>();
        let state = if c.e_stop {
            McuState::EStop
        } else if self.e05 || c.ntc1_fault {
            McuState::Fault
        } else {
            match c.mode {
                Mode::Idle => McuState::Idle,
                Mode::Charging => McuState::Charging,
                Mode::Discharging => McuState::Discharging,
            }
        };
        // Signed current, positive = charging (converted to the wire sign below).
        let current = match state {
            McuState::Charging => c.current_a,
            McuState::Discharging => -c.current_a,
            _ => 0.0,
        };
        let capacity_ah = f64::from(self.config.capacity_mah) / 1000.0;
        self.soc_pct = (self.soc_pct + current * dt * SOC_SPEED_UP / 3600.0 / capacity_ah * 100.0)
            .clamp(0.0, 100.0);

        let fault_changed = faults != self.last_faults;
        if fault_changed || now >= self.next_telemetry {
            let telemetry = self.telemetry(state, faults, current);
            if now >= self.next_telemetry {
                self.next_telemetry = now + TELEMETRY_PERIOD;
                self.send(&Message::Telemetry(telemetry));
            } else {
                self.send(&Message::FaultUpdate(telemetry));
            }
            self.last_faults = faults;
        }
        std::mem::take(&mut self.outbox)
    }

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "values are clamped to the field range first"
    )]
    fn telemetry(&self, state: McuState, faults: u32, current_a: f64) -> Telemetry {
        let c = self.controls;
        // LFP-like: flat around 3.3 V, plus a little IR drop.
        let base = 3.0 + 0.004 * self.soc_pct + 0.01 * current_a;
        let mut cells = [base; 4];
        if c.imbalance {
            cells[2] += 0.08;
        }
        let mv = |v: f64| (v * 1000.0).round().clamp(0.0, 65_535.0) as u16;
        let deci = |t: f64| (t * 10.0).round().clamp(-32_768.0, 32_767.0) as i16;
        let mut validity = Validity::DEFINED & !(1 << 9); // NTC4 not fitted
        if c.ntc1_fault {
            validity &= !(1 << 6);
        }
        let mut t = Telemetry::from_bytes(&[0; TELEMETRY_LEN]);
        t.cell_mv = cells.map(mv);
        t.pack_mv = mv(cells.iter().sum());
        // Wire: 10 mA per count, negative = charging (E5.8).
        t.pack_current_10ma = (-current_a * 100.0).round().clamp(-32_768.0, 32_767.0) as i16;
        t.temperature_deci_c = [
            if c.ntc1_fault { 0 } else { deci(c.cell_temp_c) },
            deci(c.cell_temp_c + 1.5),
            deci(c.air_temp_c),
            0,
        ];
        t.state = state;
        t.reported_faults = FaultBits(faults);
        t.validity = Validity(validity);
        t.boot_counter = self.boot_counter;
        t.config_revision = self.config.config_revision;
        t.soc_centi_pct = (self.soc_pct * 100.0).round().clamp(0.0, 10_000.0) as u16;
        t
    }
}

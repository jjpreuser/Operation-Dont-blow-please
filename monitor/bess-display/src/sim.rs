//! The demo MCU for the preview: [`protocol::sim::SimMcu`] (the MCU end of
//! the real UART protocol) with a rough battery on top, so the screen can
//! show each state. It is not a model of the BMS firmware.
//!
//! - TELEMETRY every 1 s; a `FAULT_UPDATE` at once when the fault set changes.
//! - One CONFIG after start-up and one on every `CONFIG_REQUEST` (§8).
//! - "Restart" gives a new `boot_counter`; "Cut link" stops all frames.
//! - Faults: E05 above 55 °C (clear at 50 °C), E17 above 45 °C air (clear at
//!   40 °C), E08 with E-STOP, E09 with "NTC1 sensor fault", E13 with
//!   "Cell imbalance", E24 with "Unknown fault bit".
//! - `SoC` moves 60 times faster than real, so charging is visible.

use bess_core::db::capacity_ah;
use protocol::messages::{
    FaultBits, FaultCode, Identity, McuState, Message, TELEMETRY_LEN, Telemetry, Validity,
};
use protocol::reference;
use protocol::sim::SimMcu;
use std::time::{Duration, Instant};

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

/// The demo MCU.
pub struct DemoMcu {
    pub controls: Controls,
    mcu: SimMcu,
    next_telemetry: Instant,
    last_step: Instant,
    soc_pct: f64,
    e05: bool,
    e17: bool,
    last_faults: FaultBits,
    last_heartbeat: Option<Instant>,
    /// Bytes to send to the Pi.
    outbox: Vec<u8>,
}

impl DemoMcu {
    /// Boot 1 with the UART draft's reference CONFIG.
    #[must_use]
    pub fn new(now: Instant) -> DemoMcu {
        let mut demo = DemoMcu {
            controls: Controls::default(),
            mcu: SimMcu::new(reference::config()),
            next_telemetry: now,
            last_step: now,
            soc_pct: 42.0,
            e05: false,
            e17: false,
            last_faults: FaultBits(0),
            last_heartbeat: None,
            outbox: Vec::new(),
        };
        demo.boot(now);
        demo
    }

    /// An MCU reset: new boot counter, CONFIG sent once on its own (§8).
    pub fn restart(&mut self, now: Instant) {
        self.mcu.restart();
        self.boot(now);
    }

    fn boot(&mut self, now: Instant) {
        self.next_telemetry = now + Duration::from_millis(300);
        self.send(SimMcu::config_frame);
    }

    #[must_use]
    pub fn identity(&self) -> Identity {
        self.mcu.identity()
    }

    /// The identity the Pi reports in its heartbeat, if it is recent.
    #[must_use]
    pub fn pi_heartbeat(&self, now: Instant) -> Option<Identity> {
        self.last_heartbeat
            .filter(|t| now.saturating_duration_since(*t) <= Duration::from_secs(5))
            .and(self.mcu.accepted())
    }

    #[must_use]
    pub fn soc_pct(&self) -> f64 {
        self.soc_pct
    }

    /// Queues a frame for the Pi, unless the link is cut (then nothing is
    /// sent and no sequence number is used).
    fn send(&mut self, frame: impl FnOnce(&mut SimMcu) -> Vec<u8>) {
        if !self.controls.link_cut {
            let bytes = frame(&mut self.mcu);
            self.outbox.extend(bytes);
        }
    }

    /// Bytes from the Pi; a cut link hears nothing.
    pub fn receive(&mut self, bytes: &[u8], now: Instant) {
        if self.controls.link_cut {
            return;
        }
        let heard = self.mcu.receive(bytes);
        if heard
            .messages
            .iter()
            .any(|m| matches!(m, Message::Heartbeat(_)))
        {
            self.last_heartbeat = Some(now);
        }
        self.outbox.extend(heard.reply);
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
        let faults: FaultBits = [
            (self.e05, 5),
            (c.e_stop, 8),
            (c.ntc1_fault, 9),
            (c.imbalance, 13),
            (self.e17, 17),
            (c.unknown_fault, 24),
        ]
        .into_iter()
        .filter(|&(on, _)| on)
        .filter_map(|(_, n)| FaultCode::new(n))
        .collect();
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
        let capacity = capacity_ah(self.mcu.config());
        self.soc_pct = (self.soc_pct + current * dt * SOC_SPEED_UP / 3600.0 / capacity * 100.0)
            .clamp(0.0, 100.0);

        let fault_changed = faults != self.last_faults;
        if fault_changed || now >= self.next_telemetry {
            let telemetry = self.telemetry(state, faults, current);
            if now >= self.next_telemetry {
                self.next_telemetry = now + TELEMETRY_PERIOD;
                self.send(|mcu| mcu.telemetry(telemetry));
            } else {
                self.send(|mcu| mcu.fault_update(telemetry));
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
    fn telemetry(&self, state: McuState, faults: FaultBits, current_a: f64) -> Telemetry {
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
        t.reported_faults = faults;
        t.validity = Validity(validity);
        t.soc_centi_pct = (self.soc_pct * 100.0).round().clamp(0.0, 10_000.0) as u16;
        t
    }
}

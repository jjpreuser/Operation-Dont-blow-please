//! Fan duty rules and curves (docs/design/fan-control.md §3–4; DD-16, DD-28,
//! DD-42, DD-51, DD-52, DD-53, DD-55; S6.2–S6.5, S6.8).
//!
//! [`FanController`] is the duty part of the `fan_controller` task without
//! the task: the caller runs [`FanController::evaluate`] on every valid
//! TELEMETRY, on a `FAULT_UPDATE` that changes E05, E17 or E-STOP, on every
//! link-state change and on a 1 s timer (DD-51), writes the duty to the fan
//! controllers and then feeds the heartbeat liveness flag.
//!
//! The first matching rule sets the duty (DD-51):
//! 1. E-STOP in the latest valid frame while the link is not OFFLINE: 0 %
//!    (DD-42, E7.7).
//! 2. No CONFIG for the current identity (start-up, `WAIT_CONFIG`): 100 %
//!    (DD-28, DD-55).
//! 3. Link OFFLINE: `fan_offline_action` (DD-16). 2 = 100 %; 1 = the last
//!    duty before OFFLINE, then 0 % after `fan_offline_ms`; 0 and unknown
//!    values = 100 % (DD-28).
//! 4. E05 or E17 active: 100 % (S6.5, S6.8).
//! 5. Otherwise the higher of the battery curve (NTC1) and the air curve
//!    (NTC3) (S6.3).
//!
//! The E12 speed check and the dead-fan rule (DD-54, DD-76, DD-85) are not
//! here yet: they wait for the fan bench tests (D10, D13). The dead-fan rule
//! will slot in between rules 4 and 5.
//!
//! Details the design leaves open (DD-91):
//! - Both curves update their on/off state at every evaluation while ONLINE,
//!   also while rule 4 sets the duty, so the state is current when E05 or E17
//!   clears.
//! - A new CONFIG identity resets both curves, so the start-up rule of DD-53
//!   applies again with the new thresholds.
//! - Rule 3 with action 1 and `fan_offline_ms` = 65535 (undecided, §6) is
//!   treated like action 0: 100 %.
//! - With E05 and E17 both active the reason is E05.
//!
//! Values stay in wire units: temperatures in 0.1 °C, duty in %.

use crate::link::LinkState;
use protocol::config::{Config, FanOfflineAction, FanTable, Table};
use protocol::messages::{FaultCode, Identity, McuState, Telemetry};
use std::fmt;
use std::time::{Duration, Instant};

/// `fan_offline_ms` meaning "not decided yet", not a real timeout (§6).
pub const FAN_OFFLINE_MS_UNDECIDED: u16 = u16::MAX;

const E05: FaultCode = match FaultCode::new(5) {
    Some(code) => code,
    None => unreachable!(),
};
const E17: FaultCode = match FaultCode::new(17) {
    Some(code) => code,
    None => unreachable!(),
};

/// Why the fans run at the duty they do; shown next to the duty (fan-control.md §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reason {
    /// Off: E-STOP (DD-42).
    EStop,
    /// 100 %: no CONFIG yet, "waiting for BMS configuration" (DD-28, DD-55).
    NoConfig,
    /// 100 % while OFFLINE (`fan_offline_action` 0, 2 or unknown).
    Offline,
    /// The last duty before OFFLINE, until `fan_offline_ms` has passed.
    OfflineHold,
    /// Off: OFFLINE for `fan_offline_ms` or longer (DD-16).
    OfflineOff,
    /// 100 %: E05 battery overtemperature (S6.5).
    E05,
    /// 100 %: E17 enclosure air overtemperature (S6.8).
    E17,
    /// The curve that drives the duty; the battery curve on a tie, also when
    /// both are off.
    Curve(Table),
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Reason::EStop => "E-STOP",
            Reason::NoConfig => "waiting for BMS configuration",
            Reason::Offline => "BMS communication lost",
            Reason::OfflineHold => "BMS communication lost, holding",
            Reason::OfflineOff => "BMS communication lost, off",
            Reason::E05 => "E05 overheating",
            Reason::E17 => "E17 enclosure overheating",
            Reason::Curve(Table::Battery) => "battery temperature",
            Reason::Curve(Table::Air) => "air temperature",
        })
    }
}

/// One evaluation's result: the duty for both fans and the rule that set it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FanDemand {
    /// 0–100 %.
    pub duty_pct: u8,
    pub reason: Reason,
}

/// What [`FanController::evaluate`] looks at.
#[derive(Debug, Clone, Copy)]
pub struct FanInputs<'a> {
    /// From `link_supervisor` (D5).
    pub link: LinkState,
    /// The installed CONFIG (`LinkSupervisor::config`), `None` without one.
    pub config: Option<&'a Config>,
    /// The latest valid TELEMETRY or `FAULT_UPDATE` frame.
    pub frame: Option<&'a Telemetry>,
}

/// The table duty at temperature `t_deci_c` (S6.2): linear between the
/// active points, rounded to the nearest whole %, half upward (§7); the end
/// point's duty outside the table (DD-53).
///
/// Expects a table that passed `Config::check` (2–8 points, temperatures
/// strictly increasing, duty 0–100); for an empty table it returns 100.
#[must_use]
pub fn interpolate(table: &FanTable, t_deci_c: i16) -> u8 {
    let points = table.active();
    let (Some(first), Some(last)) = (points.first(), points.last()) else {
        return 100;
    };
    if t_deci_c <= first.temperature_deci_c {
        return first.duty_pct;
    }
    if t_deci_c >= last.temperature_deci_c {
        return last.duty_pct;
    }
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if t_deci_c <= b.temperature_deci_c {
            let dt = i64::from(t_deci_c) - i64::from(a.temperature_deci_c);
            let span = i64::from(b.temperature_deci_c) - i64::from(a.temperature_deci_c);
            let rise = i64::from(b.duty_pct) - i64::from(a.duty_pct);
            // a + dt * rise / span, rounded half up: floor((2n + d) / 2d).
            let duty = i64::from(a.duty_pct) + (2 * dt * rise + span).div_euclid(2 * span);
            return u8::try_from(duty.clamp(0, 100)).unwrap_or(100);
        }
    }
    last.duty_pct
}

/// On/off state of one curve (DD-53).
#[derive(Debug, Clone, Copy, Default)]
struct Curve {
    /// `None` until the first valid temperature.
    on: Option<bool>,
}

impl Curve {
    /// This curve's demand, updating its on/off state. An invalid
    /// temperature demands 100 % and leaves the state as it is (DD-52).
    fn demand(
        &mut self,
        t_deci_c: Option<i16>,
        table: &FanTable,
        on_at: i16,
        off_at: i16,
        min_pct: u8,
    ) -> u8 {
        let Some(t) = t_deci_c else {
            return 100;
        };
        let on = match self.on {
            // Start ON above the off point: never stop fans that may have
            // been running inside the band before a restart.
            None | Some(true) => t > off_at,
            Some(false) => t >= on_at,
        };
        self.on = Some(on);
        if on {
            interpolate(table, t).max(min_pct)
        } else {
            0
        }
    }
}

/// The duty rules of `fan_controller` (fan-control.md §3).
#[derive(Debug, Clone)]
pub struct FanController {
    battery: Curve,
    air: Curve,
    /// Identity of the CONFIG the curve states belong to.
    config_identity: Option<Identity>,
    /// The duty of the last evaluation while not OFFLINE, for the hold rule.
    last_duty: u8,
    /// When the link went OFFLINE.
    offline_since: Option<Instant>,
}

impl Default for FanController {
    fn default() -> FanController {
        FanController::new()
    }
}

impl FanController {
    /// A controller at core start: no curve state, last duty 100 %.
    #[must_use]
    pub fn new() -> FanController {
        FanController {
            battery: Curve::default(),
            air: Curve::default(),
            config_identity: None,
            last_duty: 100,
            offline_since: None,
        }
    }

    /// The duty for both fans at `now`.
    pub fn evaluate(&mut self, now: Instant, inputs: FanInputs<'_>) -> FanDemand {
        if inputs.link == LinkState::Offline {
            self.offline_since.get_or_insert(now);
        } else {
            self.offline_since = None;
        }
        let identity = inputs.config.map(Config::identity);
        if identity != self.config_identity {
            self.battery = Curve::default();
            self.air = Curve::default();
            self.config_identity = identity;
        }
        let demand = self.rule(now, inputs);
        if inputs.link != LinkState::Offline {
            self.last_duty = demand.duty_pct;
        }
        demand
    }

    fn rule(&mut self, now: Instant, inputs: FanInputs<'_>) -> FanDemand {
        let link = inputs.link;
        if link != LinkState::Offline && inputs.frame.is_some_and(|f| f.state == McuState::EStop) {
            return FanDemand {
                duty_pct: 0,
                reason: Reason::EStop,
            };
        }
        let config = match inputs.config {
            Some(config) if link != LinkState::WaitConfig => config,
            _ => {
                return FanDemand {
                    duty_pct: 100,
                    reason: Reason::NoConfig,
                };
            }
        };
        if link == LinkState::Offline {
            return self.offline(now, config);
        }
        let frame = inputs.frame;
        let temperature = |ntc: usize| frame.and_then(|f| f.valid_temperature_deci_c(ntc));
        let battery = self.battery.demand(
            temperature(0),
            &config.battery_table,
            config.battery_fan_on_deci_c,
            config.battery_fan_off_deci_c,
            config.fan_min_pct,
        );
        let air = self.air.demand(
            temperature(2),
            &config.air_table,
            config.air_fan_on_deci_c,
            config.air_fan_off_deci_c,
            config.fan_min_pct,
        );
        let faults = frame.map(|f| f.reported_faults).unwrap_or_default();
        if faults.contains(E05) {
            return FanDemand {
                duty_pct: 100,
                reason: Reason::E05,
            };
        }
        if faults.contains(E17) {
            return FanDemand {
                duty_pct: 100,
                reason: Reason::E17,
            };
        }
        if air > battery {
            FanDemand {
                duty_pct: air,
                reason: Reason::Curve(Table::Air),
            }
        } else {
            FanDemand {
                duty_pct: battery,
                reason: Reason::Curve(Table::Battery),
            }
        }
    }

    fn offline(&self, now: Instant, config: &Config) -> FanDemand {
        let full = FanDemand {
            duty_pct: 100,
            reason: Reason::Offline,
        };
        match config.fan_offline_action {
            FanOfflineAction::HoldThenOff if config.fan_offline_ms != FAN_OFFLINE_MS_UNDECIDED => {
                let since = self.offline_since.unwrap_or(now);
                let hold = Duration::from_millis(u64::from(config.fan_offline_ms));
                if now.saturating_duration_since(since) < hold {
                    FanDemand {
                        duty_pct: self.last_duty,
                        reason: Reason::OfflineHold,
                    }
                } else {
                    FanDemand {
                        duty_pct: 0,
                        reason: Reason::OfflineOff,
                    }
                }
            }
            FanOfflineAction::HoldThenOff
            | FanOfflineAction::Full
            | FanOfflineAction::Undecided
            | FanOfflineAction::Unknown(_) => full,
        }
    }
}

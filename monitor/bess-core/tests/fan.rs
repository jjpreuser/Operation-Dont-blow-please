//! Fan duty rules and curves (fan-control.md §3–4), following the L1 rows of
//! docs/testing/test-plan.md: S6.2 interpolation, S6.3 max of the curves,
//! S6.4 hysteresis, S6.5 / S6.8 forced 100 % on E05 / E17, plus the rule
//! order (DD-51), E-STOP (DD-42), no CONFIG (DD-28, DD-55) and the OFFLINE
//! policy (DD-16).
//!
//! Reference CONFIG tables: battery 27/30/40/45 °C at 0/30/65/100 % with
//! on 30 / off 27 °C, air 32/35/40/45 °C at 0/30/65/100 % with on 35 / off
//! 32 °C, `fan_min_pct` 30.

mod common;

use bess_core::fan::{interpolate, FanController, FanDemand, FanInputs, Reason};
use bess_core::link::LinkState;
use common::{config_with, ms, telemetry_frame, BOOT1, BOOT2};
use protocol::config::{Config, FanOfflineAction, FanPoint, FanTable, Table, TABLE_SLOTS};
use protocol::messages::{FaultBits, FaultCode, McuState, Telemetry, Validity};
use std::time::Instant;

/// °C to 0.1 °C.
fn dc(celsius: f64) -> i16 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "test temperatures are small"
    )]
    let value = (celsius * 10.0).round() as i16;
    value
}

/// A frame with NTC1 (battery) and NTC3 (air) in °C; `None` = invalid.
fn frame(battery: Option<f64>, air: Option<f64>) -> Telemetry {
    let mut t = telemetry_frame(BOOT1);
    let mut validity = 0;
    if let Some(c) = battery {
        t.temperature_deci_c[0] = dc(c);
        validity |= 1 << 6;
    }
    if let Some(c) = air {
        t.temperature_deci_c[2] = dc(c);
        validity |= 1 << 8;
    }
    t.validity = Validity(validity);
    t
}

fn with_faults(mut t: Telemetry, codes: &[u8]) -> Telemetry {
    t.reported_faults = FaultBits(
        codes
            .iter()
            .map(|&n| FaultCode::new(n).expect("code 1-32").mask())
            .sum(),
    );
    t
}

fn with_state(mut t: Telemetry, state: McuState) -> Telemetry {
    t.state = state;
    t
}

fn reference() -> Config {
    config_with(BOOT1, |_| {})
}

fn table(points: &[(f64, u8)]) -> FanTable {
    let mut slots = [FanPoint::default(); TABLE_SLOTS];
    for (slot, &(c, duty_pct)) in slots.iter_mut().zip(points) {
        *slot = FanPoint {
            temperature_deci_c: dc(c),
            duty_pct,
            expected_rpm: 0,
        };
    }
    FanTable {
        count: u8::try_from(points.len()).expect("at most 8 points"),
        points: slots,
    }
}

/// A test bench: controller, CONFIG and a clock.
struct Bench {
    fans: FanController,
    config: Config,
    now: Instant,
}

impl Bench {
    fn new(config: Config) -> Bench {
        Bench {
            fans: FanController::new(),
            config,
            now: Instant::now(),
        }
    }

    fn run(&mut self, link: LinkState, frame: &Telemetry) -> FanDemand {
        self.now += ms(1000);
        let config = (link != LinkState::WaitConfig).then_some(&self.config);
        self.fans.evaluate(
            self.now,
            FanInputs {
                link,
                config,
                frame: Some(frame),
            },
        )
    }

    fn online(&mut self, frame: &Telemetry) -> FanDemand {
        self.run(LinkState::Online, frame)
    }

    /// ONLINE with battery `b` °C and air well below its off point.
    fn battery(&mut self, b: f64) -> FanDemand {
        self.online(&frame(Some(b), Some(20.0)))
    }
}

fn demand(duty_pct: u8, reason: Reason) -> FanDemand {
    FanDemand { duty_pct, reason }
}

const BATTERY: Reason = Reason::Curve(Table::Battery);
const AIR: Reason = Reason::Curve(Table::Air);

// --- S6.2 interpolation -----------------------------------------------------

#[test]
fn interpolation_at_between_and_outside_the_points() {
    let t = reference().battery_table;
    for (c, duty) in [
        (27.0, 0),
        (30.0, 30),
        (40.0, 65),
        (45.0, 100),
        (38.0, 58),  // fan-control.md §4 example
        (35.0, 48),  // 47.5 rounds up
        (42.5, 83),  // 82.5 rounds up
        (20.0, 0),   // below the table: first point
        (60.0, 100), // above the table: last point
    ] {
        assert_eq!(interpolate(&t, dc(c)), duty, "{c} °C");
    }
}

#[test]
fn interpolation_rounds_half_up() {
    let t = table(&[(0.0, 0), (2.0, 1)]);
    assert_eq!(interpolate(&t, 9), 0); // 0.45
    assert_eq!(interpolate(&t, 10), 1); // 0.5
    assert_eq!(interpolate(&t, 11), 1); // 0.55
}

#[test]
fn interpolation_with_two_and_eight_points() {
    let two = table(&[(30.0, 30), (45.0, 100)]);
    assert_eq!(interpolate(&two, dc(37.5)), 65);
    let eight = table(&[
        (20.0, 0),
        (25.0, 10),
        (30.0, 20),
        (35.0, 40),
        (40.0, 60),
        (45.0, 80),
        (50.0, 90),
        (55.0, 100),
    ]);
    assert_eq!(interpolate(&eight, dc(47.5)), 85);
    assert_eq!(interpolate(&eight, dc(32.5)), 30);
    assert_eq!(interpolate(&eight, dc(55.0)), 100);
}

#[test]
fn negative_temperatures_interpolate() {
    let t = table(&[(-10.0, 0), (10.0, 100)]);
    assert_eq!(interpolate(&t, dc(-5.0)), 25);
    assert_eq!(interpolate(&t, dc(-20.0)), 0);
}

#[test]
fn the_table_comes_from_config() {
    let mut bench = Bench::new(config_with(BOOT1, |c| {
        c.battery_table.points[2].duty_pct = 80; // 40 °C: 80 % instead of 65 %
    }));
    assert_eq!(bench.battery(38.0), demand(70, BATTERY));
}

// --- S6.3 max of the battery and air curves ---------------------------------

#[test]
fn duty_is_the_higher_curve() {
    let mut bench = Bench::new(reference());
    assert_eq!(
        bench.online(&frame(Some(38.0), Some(33.0))),
        demand(58, BATTERY)
    );
    let mut bench = Bench::new(reference());
    assert_eq!(
        bench.online(&frame(Some(25.0), Some(40.0))),
        demand(65, AIR)
    );
}

#[test]
fn equal_curves_name_the_battery_curve() {
    let mut bench = Bench::new(reference());
    assert_eq!(
        bench.online(&frame(Some(40.0), Some(40.0))),
        demand(65, BATTERY)
    );
    // Both off: 0 %, battery named.
    let mut bench = Bench::new(reference());
    assert_eq!(
        bench.online(&frame(Some(20.0), Some(20.0))),
        demand(0, BATTERY)
    );
}

#[test]
fn an_invalid_input_demands_100() {
    let mut bench = Bench::new(reference());
    assert_eq!(bench.online(&frame(None, Some(20.0))), demand(100, BATTERY));
    assert_eq!(bench.online(&frame(Some(20.0), None)), demand(100, AIR));
    assert_eq!(bench.online(&frame(None, None)), demand(100, BATTERY));
    // Valid again: the curves take over.
    assert_eq!(
        bench.online(&frame(Some(20.0), Some(20.0))),
        demand(0, BATTERY)
    );
}

#[test]
fn an_invalid_input_keeps_the_curve_state() {
    let mut bench = Bench::new(reference());
    bench.battery(31.0); // ON
    bench.online(&frame(None, Some(20.0)));
    // Back inside the band: still ON, so 30 %, not 0 %.
    assert_eq!(bench.battery(28.0), demand(30, BATTERY));
}

// --- S6.4 hysteresis --------------------------------------------------------

#[test]
fn ramp_up_switches_on_at_30_with_30_percent() {
    let mut bench = Bench::new(reference());
    for c in [25.0, 27.0, 28.0, 29.0, 29.9] {
        assert_eq!(bench.battery(c).duty_pct, 0, "{c} °C");
    }
    assert_eq!(bench.battery(30.0), demand(30, BATTERY));
    assert_eq!(bench.battery(45.0), demand(100, BATTERY));
}

#[test]
fn ramp_down_stays_at_30_percent_until_27() {
    let mut bench = Bench::new(reference());
    bench.battery(45.0);
    assert_eq!(bench.battery(30.0).duty_pct, 30);
    for c in [29.0, 28.0, 27.5, 27.1] {
        assert_eq!(bench.battery(c).duty_pct, 30, "{c} °C");
    }
    assert_eq!(bench.battery(27.0).duty_pct, 0);
    assert_eq!(bench.battery(29.0).duty_pct, 0);
}

#[test]
fn no_oscillation_inside_the_band() {
    let mut bench = Bench::new(reference());
    bench.battery(20.0); // OFF
    for c in [28.0, 29.5, 28.0, 29.5] {
        assert_eq!(bench.battery(c).duty_pct, 0);
    }
    bench.battery(31.0); // ON
    for c in [28.0, 29.5, 28.0, 29.5] {
        assert_eq!(bench.battery(c).duty_pct, 30);
    }
}

#[test]
fn core_start_inside_the_band_starts_on() {
    for (c, duty) in [(27.1, 30), (28.5, 30), (29.9, 30), (27.0, 0), (26.0, 0)] {
        let mut bench = Bench::new(reference());
        assert_eq!(bench.battery(c).duty_pct, duty, "start at {c} °C");
    }
}

#[test]
fn air_curve_has_its_own_hysteresis() {
    let mut bench = Bench::new(reference());
    let air = |c| frame(Some(20.0), Some(c));
    assert_eq!(bench.online(&air(34.0)), demand(30, AIR)); // starts ON above 32
    assert_eq!(bench.online(&air(32.0)), demand(0, BATTERY));
    assert_eq!(bench.online(&air(34.9)).duty_pct, 0);
    assert_eq!(bench.online(&air(35.0)), demand(30, AIR));
}

// --- S6.5 / S6.8 forced 100 % ----------------------------------------------

#[test]
fn e05_forces_100_and_the_curve_returns_when_it_clears() {
    let mut bench = Bench::new(reference());
    let cold = frame(Some(20.0), Some(20.0));
    assert_eq!(bench.online(&cold).duty_pct, 0);
    assert_eq!(
        bench.online(&with_faults(cold, &[5])),
        demand(100, Reason::E05)
    );
    assert_eq!(bench.online(&cold), demand(0, BATTERY));
}

#[test]
fn e17_forces_100_and_the_curve_returns_when_it_clears() {
    let mut bench = Bench::new(reference());
    let cold = frame(Some(20.0), Some(20.0));
    assert_eq!(
        bench.online(&with_faults(cold, &[17])),
        demand(100, Reason::E17)
    );
    assert_eq!(bench.online(&cold), demand(0, BATTERY));
}

#[test]
fn e05_and_e17_together_name_e05() {
    let mut bench = Bench::new(reference());
    let hot = with_faults(frame(Some(20.0), Some(20.0)), &[5, 17]);
    assert_eq!(bench.online(&hot), demand(100, Reason::E05));
}

#[test]
fn other_faults_do_not_force_the_fans() {
    let mut bench = Bench::new(reference());
    let t = with_faults(frame(Some(38.0), Some(20.0)), &[1, 4, 12, 19, 25]);
    assert_eq!(bench.online(&t), demand(58, BATTERY));
}

#[test]
fn curves_keep_tracking_while_e05_forces_100() {
    let mut bench = Bench::new(reference());
    bench.battery(35.0); // ON
    let e05 = |c| with_faults(frame(Some(c), Some(20.0)), &[5]);
    bench.online(&e05(30.0));
    bench.online(&e05(26.0)); // the battery curve goes OFF meanwhile
                              // E05 clears inside the band: the curve is OFF, so 0 %, not 30 %.
    assert_eq!(bench.battery(28.5), demand(0, BATTERY));
}

// --- E-STOP (DD-42) ---------------------------------------------------------

#[test]
fn e_stop_switches_the_fans_off_over_every_other_rule() {
    let mut bench = Bench::new(reference());
    let hot = with_faults(frame(Some(50.0), Some(50.0)), &[5, 17]);
    let stop = with_state(hot, McuState::EStop);
    assert_eq!(bench.online(&stop), demand(0, Reason::EStop));
    // Also before a CONFIG (WAIT_CONFIG still has valid frames).
    assert_eq!(
        bench.run(LinkState::WaitConfig, &stop),
        demand(0, Reason::EStop)
    );
    // Released: the rules below apply again.
    assert_eq!(bench.online(&hot), demand(100, Reason::E05));
}

#[test]
fn a_stale_e_stop_does_not_count_while_offline() {
    let mut bench = Bench::new(reference());
    let stop = with_state(frame(Some(20.0), Some(20.0)), McuState::EStop);
    bench.online(&stop);
    // OFFLINE: the frame is old; the hardware E-stop switches the fans off by
    // itself (E7.7), so the Pi follows the OFFLINE rule.
    assert_eq!(
        bench.run(LinkState::Offline, &stop),
        demand(100, Reason::Offline)
    );
}

// --- No CONFIG (DD-28, DD-55) ----------------------------------------------

#[test]
fn without_config_the_fans_run_at_100() {
    let t = frame(Some(20.0), Some(20.0));
    let mut fans = FanController::new();
    let now = Instant::now();
    for link in [LinkState::Offline, LinkState::WaitConfig, LinkState::Online] {
        let inputs = FanInputs {
            link,
            config: None,
            frame: Some(&t),
        };
        assert_eq!(fans.evaluate(now, inputs), demand(100, Reason::NoConfig));
    }
    // Core start, nothing received at all.
    let inputs = FanInputs {
        link: LinkState::Offline,
        config: None,
        frame: None,
    };
    assert_eq!(fans.evaluate(now, inputs), demand(100, Reason::NoConfig));
}

#[test]
fn wait_config_ignores_a_stale_config() {
    // The link supervisor never passes a CONFIG in WAIT_CONFIG, but if one
    // were passed it must not be used (S2.4).
    let mut fans = FanController::new();
    let config = reference();
    let t = frame(Some(20.0), Some(20.0));
    let inputs = FanInputs {
        link: LinkState::WaitConfig,
        config: Some(&config),
        frame: Some(&t),
    };
    assert_eq!(
        fans.evaluate(Instant::now(), inputs),
        demand(100, Reason::NoConfig)
    );
}

#[test]
fn a_new_config_identity_restarts_the_curves() {
    let mut bench = Bench::new(reference());
    bench.battery(20.0);
    assert_eq!(bench.battery(28.5).duty_pct, 0); // OFF inside the band
    bench.config = config_with(BOOT2, |_| {});
    // New CONFIG: the start-up rule applies again, 28.5 > 27 starts ON.
    assert_eq!(bench.battery(28.5), demand(30, BATTERY));
}

#[test]
fn the_same_config_after_offline_keeps_the_curves() {
    let mut bench = Bench::new(config_with(BOOT1, |c| {
        c.fan_offline_action = FanOfflineAction::Full;
    }));
    bench.battery(20.0);
    bench.battery(28.5); // OFF inside the band
    bench.run(LinkState::Offline, &frame(Some(28.5), Some(20.0)));
    assert_eq!(bench.battery(28.5), demand(0, BATTERY));
}

// --- OFFLINE policy (DD-16, DD-28) -----------------------------------------

#[test]
fn offline_actions_0_2_and_unknown_run_100() {
    for action in [
        FanOfflineAction::Undecided,
        FanOfflineAction::Full,
        FanOfflineAction::Unknown(7),
    ] {
        let mut bench = Bench::new(config_with(BOOT1, |c| c.fan_offline_action = action));
        let cold = frame(Some(20.0), Some(20.0));
        assert_eq!(bench.online(&cold).duty_pct, 0);
        assert_eq!(
            bench.run(LinkState::Offline, &cold),
            demand(100, Reason::Offline),
            "{action:?}"
        );
        // Recovery.
        assert_eq!(bench.online(&cold), demand(0, BATTERY));
    }
}

#[test]
fn offline_hold_keeps_the_last_duty_then_switches_off() {
    let mut bench = Bench::new(config_with(BOOT1, |c| {
        c.fan_offline_action = FanOfflineAction::HoldThenOff;
        c.fan_offline_ms = 10_000;
    }));
    let warm = frame(Some(38.0), Some(20.0));
    assert_eq!(bench.online(&warm).duty_pct, 58);
    let lost = bench.now + ms(1000);
    let mut at = |offset: u64| {
        bench.fans.evaluate(
            lost + ms(offset),
            FanInputs {
                link: LinkState::Offline,
                config: Some(&bench.config),
                frame: Some(&warm),
            },
        )
    };
    assert_eq!(at(0), demand(58, Reason::OfflineHold));
    assert_eq!(at(9_999), demand(58, Reason::OfflineHold));
    assert_eq!(at(10_000), demand(0, Reason::OfflineOff));
    assert_eq!(at(60_000), demand(0, Reason::OfflineOff));
    // Recovery: the curve again, and a later loss holds afresh.
    assert_eq!(bench.online(&warm), demand(58, BATTERY));
    assert_eq!(
        bench.run(LinkState::Offline, &warm),
        demand(58, Reason::OfflineHold)
    );
}

#[test]
fn offline_hold_with_undecided_timeout_runs_100() {
    let mut bench = Bench::new(config_with(BOOT1, |c| {
        c.fan_offline_action = FanOfflineAction::HoldThenOff;
        // fan_offline_ms stays 65535 (undecided) as in the reference CONFIG.
    }));
    let warm = frame(Some(38.0), Some(20.0));
    bench.online(&warm);
    assert_eq!(
        bench.run(LinkState::Offline, &warm),
        demand(100, Reason::Offline)
    );
}

#[test]
fn offline_hold_holds_a_forced_duty_too() {
    let mut bench = Bench::new(config_with(BOOT1, |c| {
        c.fan_offline_action = FanOfflineAction::HoldThenOff;
        c.fan_offline_ms = 10_000;
    }));
    let e05 = with_faults(frame(Some(20.0), Some(20.0)), &[5]);
    bench.online(&e05);
    assert_eq!(
        bench.run(LinkState::Offline, &e05),
        demand(100, Reason::OfflineHold)
    );
}

#[test]
fn reasons_have_display_text() {
    assert_eq!(Reason::E05.to_string(), "E05 overheating");
    assert_eq!(
        Reason::NoConfig.to_string(),
        "waiting for BMS configuration"
    );
    assert_eq!(BATTERY.to_string(), "battery temperature");
    assert_eq!(AIR.to_string(), "air temperature");
}

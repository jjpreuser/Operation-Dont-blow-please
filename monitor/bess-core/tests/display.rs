//! Sample and display logic, following the L1 rows of
//! docs/testing/test-plan.md: S8.8 signed current and power, S8.9 shown
//! state and priority, S8.11 yellow / red colours, plus the active fault
//! list (S8.10) and the live view per link state (S2.4, S8.12, DD-29,
//! DD-69).
//!
//! Reference CONFIG limits: cell trips 3.60 / 2.70 V, charge overcurrent
//! 9.0 A, discharge overcurrent 6.5 A, NTC1 charge trips 50 / 5 °C,
//! discharge trips 55 / −10 °C, terminal 70 °C, air 45 °C, NTC1–3 fitted.

mod common;

use bess_core::colour::{levels, Level, SampleLevels, WarningLevels};
use bess_core::faults::{self, Severity};
use bess_core::link::{LinkState, LinkSupervisor};
use bess_core::live::{LiveTracker, Values};
use bess_core::sample::Sample;
use bess_core::state::{shown_state, Shown, ShownState};
use common::{config_with, ms, telemetry_frame, BOOT1, BOOT2};
use protocol::config::Config;
use protocol::messages::{FaultBits, FaultCode, Identity, McuState, Message, Telemetry, Validity};
use std::time::Instant;

fn close(a: Option<f64>, b: f64) -> bool {
    a.is_some_and(|a| (a - b).abs() < 1e-9)
}

/// A frame with every field valid: cells 3.30 V, pack 13.20 V, wire current
/// `current_10ma`, NTC1–4 at 25 °C, `SoC` 50 %.
fn full_frame(identity: Identity, current_10ma: i16) -> Telemetry {
    let mut t = telemetry_frame(identity);
    t.cell_mv = [3300; 4];
    t.pack_mv = 13_200;
    t.pack_current_10ma = current_10ma;
    t.temperature_deci_c = [250; 4];
    t.soc_centi_pct = 5000;
    t.validity = Validity(Validity::DEFINED);
    t
}

fn reference() -> Config {
    config_with(BOOT1, |_| {})
}

// --- S8.8 signed current and power ------------------------------------------

#[test]
fn current_is_inverted_once_and_converted() {
    let charging = Sample::from_telemetry(&full_frame(BOOT1, -150), 0x07);
    assert!(close(charging.current_a, 1.5)); // wire −1.50 A = charging
    assert!(close(charging.power_w, 13.2 * 1.5));
    let discharging = Sample::from_telemetry(&full_frame(BOOT1, 200), 0x07);
    assert!(close(discharging.current_a, -2.0));
    assert!(close(discharging.power_w, -26.4));
    let idle = Sample::from_telemetry(&full_frame(BOOT1, 0), 0x07);
    assert!(close(idle.power_w, 0.0));
}

#[test]
fn units_are_si() {
    let mut t = full_frame(BOOT1, -150);
    t.cell_mv = [3300, 3312, 3289, 3301];
    t.temperature_deci_c = [253, 254, -55, 0];
    t.soc_centi_pct = 7550;
    let s = Sample::from_telemetry(&t, 0x0F);
    assert!(close(s.pack_v, 13.2));
    assert!(close(s.cell_v[1], 3.312));
    assert!(close(s.t_cell_c, 25.3));
    assert!(close(s.t_terminal_c, 25.4));
    assert!(close(s.t_air_out_c, -5.5));
    assert!(close(s.t_air_in_c, 0.0));
    assert!(close(s.soc_pct, 75.5));
    assert_eq!(s.cell_range(), Some((3.289, 3.312)));
}

#[test]
fn invalid_fields_are_none_and_power_needs_both() {
    let mut t = full_frame(BOOT1, -150);
    t.validity = Validity(Validity::DEFINED & !Validity::CURRENT & !(1 << 2));
    let s = Sample::from_telemetry(&t, 0x0F);
    assert_eq!(s.current_a, None);
    assert_eq!(s.power_w, None);
    assert_eq!(s.cell_v[2], None);
    assert!(s.pack_v.is_some());
    let mut t = full_frame(BOOT1, -150);
    t.validity = Validity(Validity::DEFINED & !Validity::PACK_VOLTAGE);
    assert_eq!(Sample::from_telemetry(&t, 0x0F).power_w, None);
    t.validity = Validity(0);
    assert_eq!(Sample::from_telemetry(&t, 0x0F), Sample::default());
    assert_eq!(Sample::default().cell_range(), None);
}

#[test]
fn ntc4_only_when_fitted() {
    let t = full_frame(BOOT1, 0);
    assert_eq!(Sample::from_telemetry(&t, 0x07).t_air_in_c, None);
    assert!(close(Sample::from_telemetry(&t, 0x0F).t_air_in_c, 25.0));
}

// --- S8.11 colours ----------------------------------------------------------

fn sample() -> Sample {
    Sample::from_telemetry(&full_frame(BOOT1, 0), 0x0F)
}

fn colours(edit: impl FnOnce(&mut Sample), charging: bool) -> SampleLevels {
    let mut s = sample();
    edit(&mut s);
    levels(&s, &reference(), charging, &WarningLevels::default())
}

#[test]
fn normal_values_are_not_coloured() {
    let l = colours(|_| {}, false);
    assert_eq!(l, SampleLevels::default());
    assert_eq!(l.worst(), Level::Normal);
}

#[test]
fn cell_yellow_at_350_and_290_red_at_the_config_trips() {
    for (v, level) in [
        (3.49, Level::Normal),
        (3.50, Level::Warning),
        (3.599, Level::Warning),
        (3.60, Level::Alarm),
        (2.91, Level::Normal),
        (2.90, Level::Warning),
        (2.701, Level::Warning),
        (2.70, Level::Alarm),
    ] {
        assert_eq!(
            colours(|s| s.cell_v[3] = Some(v), false).cell[3],
            level,
            "{v} V"
        );
    }
}

#[test]
fn cell_limits_come_from_config() {
    let config = config_with(BOOT1, |c| c.ov_trip_mv = 3550);
    let mut s = sample();
    s.cell_v[0] = Some(3.55);
    assert_eq!(
        levels(&s, &config, false, &WarningLevels::default()).cell[0],
        Level::Alarm
    );
}

#[test]
fn current_red_at_the_overcurrent_limit_for_its_direction() {
    for (a, level) in [
        (8.99, Level::Normal),
        (9.0, Level::Alarm),
        (-6.49, Level::Normal),
        (-6.5, Level::Alarm),
        (-8.0, Level::Alarm),
        (0.0, Level::Normal),
    ] {
        assert_eq!(
            colours(|s| s.current_a = Some(a), false).current,
            level,
            "{a} A"
        );
    }
}

#[test]
fn cell_temperature_uses_the_charge_limits_while_charging() {
    for (t, charging, level) in [
        (44.9, false, Level::Normal),
        (45.0, false, Level::Warning),
        (50.0, true, Level::Alarm), // chg_ot 50 °C
        (50.0, false, Level::Warning),
        (55.0, false, Level::Alarm), // dis_ot 55 °C
        (10.1, true, Level::Normal),
        (10.0, true, Level::Warning),
        (5.0, true, Level::Alarm), // chg_ut 5 °C
        (5.0, false, Level::Warning),
        (-10.0, false, Level::Alarm), // dis_ut −10 °C
    ] {
        assert_eq!(
            colours(|s| s.t_cell_c = Some(t), charging).t_cell,
            level,
            "{t} °C, charging {charging}"
        );
    }
}

#[test]
fn other_temperatures_have_their_own_red_limit() {
    let l = colours(
        |s| {
            s.t_terminal_c = Some(70.0);
            s.t_air_out_c = Some(45.0);
            s.t_air_in_c = Some(90.0);
        },
        false,
    );
    assert_eq!(l.t_terminal, Level::Alarm);
    assert_eq!(l.t_air_out, Level::Alarm);
    assert_eq!(l.t_air_in, Level::Warning); // NTC4 has no CONFIG limit
    let l = colours(|s| s.t_terminal_c = Some(69.9), false);
    assert_eq!(l.t_terminal, Level::Warning);
    let l = colours(|s| s.t_air_out_c = Some(9.0), false);
    assert_eq!(l.t_air_out, Level::Warning);
}

#[test]
fn soc_yellow_below_20_and_above_85_never_red() {
    for (soc, level) in [
        (20.0, Level::Normal),
        (19.99, Level::Warning),
        (85.0, Level::Normal),
        (85.01, Level::Warning),
        (0.0, Level::Warning),
        (100.0, Level::Warning),
    ] {
        assert_eq!(
            colours(|s| s.soc_pct = Some(soc), false).soc,
            level,
            "{soc} %"
        );
    }
}

#[test]
fn invalid_values_are_never_coloured() {
    let mut s = Sample::default();
    s.cell_v[0] = None;
    let l = levels(&s, &reference(), false, &WarningLevels::default());
    assert_eq!(l, SampleLevels::default());
}

#[test]
fn warning_levels_are_settings() {
    let warn = WarningLevels {
        temp_high_c: 40.0,
        ..WarningLevels::default()
    };
    let mut s = sample();
    s.t_cell_c = Some(41.0);
    assert_eq!(
        levels(&s, &reference(), false, &warn).t_cell,
        Level::Warning
    );
    assert_eq!(
        levels(&s, &reference(), false, &WarningLevels::default()).t_cell,
        Level::Normal
    );
}

#[test]
fn worst_is_the_highest_level() {
    let l = colours(
        |s| {
            s.soc_pct = Some(10.0);
            s.cell_v[2] = Some(3.65);
        },
        false,
    );
    assert_eq!(l.worst(), Level::Alarm);
}

// --- S8.9 shown state -------------------------------------------------------

#[test]
fn shown_state_table() {
    use LinkState::{Offline, Online, WaitConfig};
    use McuState::{Charging, Discharging, EStop, Fault, Idle};
    let s = |state, waiting, lost| Shown {
        state,
        waiting_for_config: waiting,
        comm_lost: lost,
    };
    for (link, mcu, expected) in [
        (Online, Some(Idle), s(ShownState::Idle, false, false)),
        (
            Online,
            Some(Charging),
            s(ShownState::Charging, false, false),
        ),
        (
            Online,
            Some(Discharging),
            s(ShownState::Discharging, false, false),
        ),
        (Online, Some(Fault), s(ShownState::Fault, false, false)),
        (Online, Some(EStop), s(ShownState::EStop, false, false)),
        (
            Online,
            Some(McuState::Unknown(9)),
            s(ShownState::Unknown(9), false, false),
        ),
        (WaitConfig, Some(EStop), s(ShownState::EStop, true, false)),
        (WaitConfig, Some(Fault), s(ShownState::Fault, true, false)),
        (
            WaitConfig,
            Some(Charging),
            s(ShownState::Offline, true, false),
        ),
        (Offline, Some(EStop), s(ShownState::EStop, false, true)),
        (Offline, Some(Fault), s(ShownState::Fault, false, true)),
        (
            Offline,
            Some(Discharging),
            s(ShownState::Offline, false, true),
        ),
        (
            Offline,
            Some(McuState::Unknown(9)),
            s(ShownState::Offline, false, true),
        ),
        (Offline, None, s(ShownState::Offline, false, true)),
    ] {
        assert_eq!(shown_state(link, mcu), expected, "{link:?} {mcu:?}");
    }
}

#[test]
fn shown_state_names() {
    assert_eq!(ShownState::EStop.to_string(), "E-STOP");
    assert_eq!(ShownState::Unknown(7).to_string(), "UNKNOWN (7)");
    assert_eq!(ShownState::Discharging.to_string(), "DISCHARGING");
}

// --- S8.10 active faults ----------------------------------------------------

fn bits(codes: &[u8]) -> FaultBits {
    FaultBits(
        codes
            .iter()
            .map(|&n| FaultCode::new(n).expect("1-32").mask())
            .sum(),
    )
}

#[test]
fn faults_have_table_a_names() {
    let e05 = faults::info(FaultCode::new(5).expect("E05"));
    assert_eq!(
        (e05.name, e05.severity),
        ("OVERHEATING", Some(Severity::Protection))
    );
    let e19 = faults::info(FaultCode::new(19).expect("E19"));
    assert_eq!(e19.name, "DISCHARGE_UNDERTEMP");
    let e25 = faults::info(FaultCode::new(25).expect("E25"));
    assert_eq!((e25.name, e25.severity), ("UNKNOWN", None));
}

#[test]
fn faults_sorted_by_severity_then_code_unknown_last() {
    let list = faults::active(bits(&[25, 13, 1, 8, 3, 15, 11]));
    let codes: Vec<u8> = list.iter().map(|f| f.code.number()).collect();
    assert_eq!(codes, [8, 15, 3, 1, 11, 13, 25]);
}

#[test]
fn red_banner_only_for_critical_faults() {
    assert!(!faults::critical(bits(&[1, 3, 11, 25])));
    assert!(faults::critical(bits(&[1, 16])));
    assert!(!faults::critical(FaultBits(0)));
}

// --- Live view per link state -----------------------------------------------

struct Bench {
    link: LinkSupervisor,
    live: LiveTracker,
    now: Instant,
}

impl Bench {
    fn new() -> Bench {
        let now = Instant::now();
        Bench {
            link: LinkSupervisor::new(now),
            live: LiveTracker::new(),
            now,
        }
    }

    fn frame(&mut self, t: Telemetry) {
        self.now += ms(1000);
        self.link.on_message(self.now, &Message::Telemetry(t));
        self.live.on_frame(&t, &self.link);
    }

    fn config(&mut self, identity: Identity) {
        let config = config_with(identity, |_| {});
        self.link
            .on_message(self.now, &Message::Config(Box::new(config.to_bytes())));
    }

    fn view(&self) -> bess_core::live::LiveState {
        self.live.view(&self.link, None, &WarningLevels::default())
    }
}

#[test]
fn before_config_values_are_hidden_but_faults_show() {
    let mut b = Bench::new();
    let mut t = full_frame(BOOT1, -150);
    t.state = McuState::Fault;
    t.reported_faults = bits(&[15]); // E15 CONFIG_INVALID
    b.frame(t);
    let v = b.view();
    assert_eq!(v.link, LinkState::WaitConfig);
    assert_eq!(v.values, Values::Hidden);
    assert_eq!(v.shown.state, ShownState::Fault);
    assert!(v.shown.waiting_for_config);
    assert_eq!(v.faults[0].name, "CONFIG_INVALID");
    assert!(v.critical_banner);
    assert_eq!(v.firmware, None);
}

#[test]
fn online_values_are_live_and_coloured() {
    let mut b = Bench::new();
    b.frame(full_frame(BOOT1, -150));
    b.config(BOOT1);
    // CONFIG accepted, but no sample under it yet.
    assert_eq!(b.view().values, Values::Hidden);
    let mut t = full_frame(BOOT1, -150);
    t.state = McuState::Charging;
    t.cell_mv[0] = 3600;
    b.frame(t);
    let v = b.view();
    let Values::Live(sample, levels) = v.values else {
        panic!("expected live values, got {:?}", v.values);
    };
    assert!(close(sample.current_a, 1.5));
    assert_eq!(levels.cell[0], Level::Alarm);
    assert_eq!(v.shown.state, ShownState::Charging);
    assert!(close(v.capacity_ah, 9.6));
    assert!(v.firmware.is_some());
    assert!(v.faults.is_empty());
}

#[test]
fn offline_values_are_greyed_and_e_stop_is_kept() {
    let mut b = Bench::new();
    b.frame(full_frame(BOOT1, 0));
    b.config(BOOT1);
    let mut t = full_frame(BOOT1, 0);
    t.state = McuState::EStop;
    t.reported_faults = bits(&[8]);
    b.frame(t);
    b.link.poll(b.now + ms(3000));
    let v = b.view();
    assert_eq!(v.link, LinkState::Offline);
    assert!(matches!(v.values, Values::Stale(_)));
    assert!(v.shown.comm_lost);
    assert_eq!(v.shown.state, ShownState::EStop);
    assert!(v.faults_stale);
    assert!(v.critical_banner);
    // Recovery with the same identity: live again.
    b.frame(full_frame(BOOT1, 0));
    let v = b.view();
    assert!(matches!(v.values, Values::Live(..)));
    assert!(!v.shown.comm_lost);
    assert!(v.faults.is_empty());
}

#[test]
fn mcu_restart_never_shows_the_old_values() {
    let mut b = Bench::new();
    b.frame(full_frame(BOOT1, 0));
    b.config(BOOT1);
    b.frame(full_frame(BOOT1, 0));
    assert!(matches!(b.view().values, Values::Live(..)));
    b.frame(full_frame(BOOT2, 0));
    assert_eq!(b.view().values, Values::Hidden);
    b.config(BOOT2);
    // New CONFIG accepted: still nothing until a frame under it arrives.
    assert_eq!(b.view().values, Values::Hidden);
    b.frame(full_frame(BOOT2, 0));
    assert!(matches!(b.view().values, Values::Live(..)));
}

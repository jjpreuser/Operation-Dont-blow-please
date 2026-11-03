//! L1 unit tests for the fan curve (D4 §4; test plan rows S6.2, S6.3, S6.4).

use bess_core::fan_curve::*;

fn p(temp_c: i16, duty_pct: u8) -> FanPoint {
    FanPoint {
        temp_dc: temp_c * 10,
        duty_pct,
    }
}

/// Battery table in the shape of Table C: 30 % at 30 °C, 65 % at 40 °C,
/// 100 % at 45 °C.
fn battery_table() -> FanTable {
    FanTable::new(&[p(30, 30), p(40, 65), p(45, 100)]).unwrap()
}

fn battery_curve(start_dc: Option<i16>) -> Curve {
    Curve::new(battery_table(), 300, 270, 30, start_dc)
}

// --- table validation (S6.2: 2 to 8 points) ---

#[test]
fn accepts_2_to_8_points() {
    for n in MIN_POINTS..=MAX_POINTS {
        let pts: Vec<_> = (0..n).map(|i| p(20 + i as i16, 50)).collect();
        assert!(FanTable::new(&pts).is_ok(), "{n} points");
    }
}

#[test]
fn rejects_wrong_point_count() {
    assert_eq!(FanTable::new(&[p(30, 30)]), Err(TableError::PointCount(1)));
    let nine: Vec<_> = (0..9).map(|i| p(20 + i, 50)).collect();
    assert_eq!(FanTable::new(&nine), Err(TableError::PointCount(9)));
}

#[test]
fn rejects_unsorted_or_duplicate_temperatures() {
    assert_eq!(
        FanTable::new(&[p(40, 30), p(30, 60)]),
        Err(TableError::NotAscending)
    );
    assert_eq!(
        FanTable::new(&[p(30, 30), p(30, 60)]),
        Err(TableError::NotAscending)
    );
}

#[test]
fn rejects_duty_above_100() {
    assert_eq!(
        FanTable::new(&[p(30, 30), p(40, 101)]),
        Err(TableError::DutyOutOfRange(101))
    );
}

// --- interpolation (S6.2) ---

#[test]
fn interp_at_points() {
    let t = battery_table();
    assert_eq!(t.interp(300), 30);
    assert_eq!(t.interp(400), 65);
    assert_eq!(t.interp(450), 100);
}

#[test]
fn interp_between_points_matches_design_example() {
    // fan-control.md §4: 30 + (38 − 30) / (40 − 30) × (65 − 30) = 58 %
    assert_eq!(battery_table().interp(380), 58);
}

#[test]
fn interp_rounds_half_up() {
    // 30 + 35 × 0.5 = 47.5 → 48
    assert_eq!(battery_table().interp(350), 48);
    // falling segment: 60 − 25 × 0.5 = 47.5 → 48
    let falling = FanTable::new(&[p(30, 60), p(40, 35)]).unwrap();
    assert_eq!(falling.interp(350), 48);
}

#[test]
fn interp_clamps_outside_the_table() {
    let t = battery_table();
    assert_eq!(t.interp(-200), 30);
    assert_eq!(t.interp(299), 30);
    assert_eq!(t.interp(451), 100);
    assert_eq!(t.interp(i16::MAX), 100);
}

#[test]
fn interp_is_monotonic_across_the_table() {
    let t = battery_table();
    let duties: Vec<_> = (250..=500).map(|dc| t.interp(dc)).collect();
    assert!(duties.windows(2).all(|w| w[0] <= w[1]));
}

#[test]
fn changed_config_table_takes_effect() {
    let mut c = battery_curve(Some(350));
    assert_eq!(c.update(Some(380)), 58);
    let new = FanTable::new(&[p(30, 40), p(40, 80), p(45, 100)]).unwrap();
    c = Curve::new(new, 300, 270, 30, Some(380));
    assert_eq!(c.update(Some(380)), 72);
}

// --- hysteresis (S6.4, DD-53) ---

#[test]
fn ramp_up_switches_on_at_30_with_min_duty() {
    let mut c = battery_curve(Some(250));
    assert_eq!(c.update(Some(280)), 0);
    assert_eq!(c.update(Some(299)), 0);
    assert_eq!(c.update(Some(300)), 30);
    assert_eq!(c.state(), CurveState::On);
    assert_eq!(c.update(Some(450)), 100);
}

#[test]
fn ramp_down_holds_min_duty_until_27() {
    let mut c = battery_curve(Some(400));
    assert_eq!(c.update(Some(290)), 30);
    assert_eq!(c.update(Some(271)), 30);
    assert_eq!(c.update(Some(270)), 0);
    assert_eq!(c.state(), CurveState::Off);
}

#[test]
fn no_oscillation_inside_the_band() {
    let mut on = battery_curve(Some(350));
    let mut off = battery_curve(Some(250));
    for dc in [280, 295, 275, 290, 285] {
        assert_eq!(on.update(Some(dc)), 30);
        assert_eq!(off.update(Some(dc)), 0);
    }
}

#[test]
fn start_inside_the_band_keeps_fans_on() {
    assert_eq!(battery_curve(Some(285)).state(), CurveState::On);
    assert_eq!(battery_curve(Some(270)).state(), CurveState::Off);
}

#[test]
fn invalid_sensor_demands_full_speed() {
    let mut c = battery_curve(Some(250));
    assert_eq!(c.update(None), 100);
    assert_eq!(battery_curve(None).state(), CurveState::On);
}

// --- max of the two curves (S6.3) ---

#[test]
fn demand_is_max_of_battery_and_air() {
    // design example: battery 38 °C → 58 %, air 33 °C with curve OFF → 0 %
    let mut battery = battery_curve(Some(380));
    let air_table = FanTable::new(&[p(35, 30), p(45, 100)]).unwrap();
    let mut air = Curve::new(air_table, 350, 320, 30, Some(300));
    let demand = fan_demand(battery.update(Some(380)), air.update(Some(330)));
    assert_eq!(demand, (58, Driving::Battery));

    assert_eq!(fan_demand(40, 70), (70, Driving::Air));
}

#[test]
fn tie_is_driven_by_battery() {
    assert_eq!(fan_demand(50, 50), (50, Driving::Battery));
}

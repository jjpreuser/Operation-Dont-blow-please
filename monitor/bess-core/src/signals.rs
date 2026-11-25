//! The 15 stored signals (docs/design/database.md §1; DD-05, DD-41, DD-72).
//!
//! A [`Sample`] holds what the TELEMETRY frame carries; the fan values come
//! from `fan_controller` (D4). Together they are the signal columns of the
//! aggregate, snapshot and event tables, in the order of
//! [`SIGNAL_COLUMNS`]. `None` is stored as NULL and left out of min/avg/max.

use crate::sample::Sample;

/// Number of stored signals.
pub const SIGNAL_COUNT: usize = 15;

/// The signal column names, unit in the name (D2 §1). They are also the CSV
/// header (S10.11, S10.13).
pub const SIGNAL_COLUMNS: [&str; SIGNAL_COUNT] = [
    "pack_v",
    "current_a",
    "power_w",
    "cg1_v",
    "cg2_v",
    "cg3_v",
    "cg4_v",
    "t_cell_c",
    "t_terminal_c",
    "t_air_out_c",
    "t_air_in_c",
    "soc_pct",
    "fan_duty_pct",
    "fan1_rpm",
    "fan2_rpm",
];

/// Decimals per signal in the CSV export (S10.11), in [`SIGNAL_COLUMNS`]
/// order: the wire resolution (1 mV, 10 mA, 0.1 °C, 0.01 %), 10 mW for
/// power, 0.1 % for the fan duty and 1 rpm for the fan speeds. Min, average
/// and max of a signal use the same number.
pub const SIGNAL_DECIMALS: [usize; SIGNAL_COUNT] = [3, 2, 2, 3, 3, 3, 3, 1, 1, 1, 1, 2, 1, 0, 0];

/// Index of `current_a` in [`Signals::values`].
pub const CURRENT_A: usize = 1;
/// Index of `pack_v` in [`Signals::values`].
pub const PACK_V: usize = 0;

/// One 1 s sample with everything that is stored.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Signals {
    /// The TELEMETRY values in SI units.
    pub sample: Sample,
    /// Commanded duty (both fans get the same, D4).
    pub fan_duty_pct: Option<f64>,
    /// Measured speed per fan (DD-58); `None` until the tach is read.
    pub fan1_rpm: Option<f64>,
    pub fan2_rpm: Option<f64>,
}

impl Signals {
    /// The values in [`SIGNAL_COLUMNS`] order.
    #[must_use]
    pub fn values(&self) -> [Option<f64>; SIGNAL_COUNT] {
        let s = &self.sample;
        [
            s.pack_v,
            s.current_a,
            s.power_w,
            s.cell_v[0],
            s.cell_v[1],
            s.cell_v[2],
            s.cell_v[3],
            s.t_cell_c,
            s.t_terminal_c,
            s.t_air_out_c,
            s.t_air_in_c,
            s.soc_pct,
            self.fan_duty_pct,
            self.fan1_rpm,
            self.fan2_rpm,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_follow_the_column_order() {
        let signals = Signals {
            sample: Sample {
                pack_v: Some(13.2),
                current_a: Some(-2.5),
                power_w: Some(-33.0),
                cell_v: [Some(3.31), Some(3.32), Some(3.33), Some(3.34)],
                t_cell_c: Some(25.0),
                t_terminal_c: Some(26.0),
                t_air_out_c: Some(27.0),
                t_air_in_c: None,
                soc_pct: Some(55.5),
            },
            fan_duty_pct: Some(30.0),
            fan1_rpm: Some(1200.0),
            fan2_rpm: Some(1210.0),
        };
        let values = signals.values();
        let by_name = |name: &str| values[SIGNAL_COLUMNS.iter().position(|c| *c == name).unwrap()];
        assert_eq!(by_name("pack_v"), Some(13.2));
        assert_eq!(by_name("current_a"), Some(-2.5));
        assert_eq!(by_name("power_w"), Some(-33.0));
        assert_eq!(by_name("cg1_v"), Some(3.31));
        assert_eq!(by_name("cg4_v"), Some(3.34));
        assert_eq!(by_name("t_cell_c"), Some(25.0));
        assert_eq!(by_name("t_terminal_c"), Some(26.0));
        assert_eq!(by_name("t_air_out_c"), Some(27.0));
        assert_eq!(by_name("t_air_in_c"), None);
        assert_eq!(by_name("soc_pct"), Some(55.5));
        assert_eq!(by_name("fan_duty_pct"), Some(30.0));
        assert_eq!(by_name("fan1_rpm"), Some(1200.0));
        assert_eq!(by_name("fan2_rpm"), Some(1210.0));
        assert_eq!(values[PACK_V], Some(13.2));
        assert_eq!(values[CURRENT_A], Some(-2.5));
    }

    #[test]
    fn column_names_are_unique() {
        let mut names = SIGNAL_COLUMNS.to_vec();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), SIGNAL_COUNT);
    }
}

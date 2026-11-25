//! Yellow / red value colouring (S8.11, DD-81, DD-92;
//! docs/design/fault-handling.md §4).
//!
//! Colours come from the value itself, not from events, so a value can be
//! red before the MCU trips (detection delay). Red is the protection limit
//! from CONFIG (Table E); yellow is the S8.11 warning level, a display
//! setting of the Pi (DD-81) because CONFIG V2 has no warning levels. Red
//! wins over yellow; an invalid value (N/A) is never coloured.
//!
//! Which CONFIG limit colours which value (DD-92):
//! - cell groups: red at or above `ov_trip_mV`, at or below `uv_trip_mV`;
//! - current: red at or above `chg_oc_mA` while charging (positive) or
//!   `dis_oc_mA` while discharging; no yellow level;
//! - NTC1 (cell): red at or beyond the charge trips (`chg_ot`, `chg_ut`) in
//!   state CHARGING, the discharge trips (`dis_ot`, `dis_ut`) otherwise;
//! - NTC2 (terminal): red at or above `terminal_ot`; NTC3 (air out): red at
//!   or above `air_ot`; NTC4: yellow only;
//! - every temperature: the yellow levels of S8.11;
//! - `SoC`: yellow only; the CONFIG window is the operating range, not a
//!   protection limit.
//!
//! Pack voltage and power have neither.

use crate::sample::Sample;
use protocol::config::Config;

/// How a value is coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Level {
    #[default]
    Normal,
    /// Yellow: warning level.
    Warning,
    /// Red: protection limit.
    Alarm,
}

/// The S8.11 yellow levels. Display settings in the Pi TOML file (DD-81);
/// the defaults are the S8.11 values (TBC in the requirements).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WarningLevels {
    /// Yellow at or above.
    pub cell_high_v: f64,
    /// Yellow at or below.
    pub cell_low_v: f64,
    /// Yellow at or above.
    pub temp_high_c: f64,
    /// Yellow at or below.
    pub temp_low_c: f64,
    /// Yellow below.
    pub soc_low_pct: f64,
    /// Yellow above.
    pub soc_high_pct: f64,
}

impl Default for WarningLevels {
    fn default() -> WarningLevels {
        WarningLevels {
            cell_high_v: 3.50,
            cell_low_v: 2.90,
            temp_high_c: 45.0,
            temp_low_c: 10.0,
            soc_low_pct: 20.0,
            soc_high_pct: 85.0,
        }
    }
}

/// The colour of each coloured value of a [`Sample`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SampleLevels {
    pub current: Level,
    pub cell: [Level; 4],
    pub t_cell: Level,
    pub t_terminal: Level,
    pub t_air_out: Level,
    pub t_air_in: Level,
    pub soc: Level,
}

impl SampleLevels {
    /// The worst level of all values.
    #[must_use]
    pub fn worst(&self) -> Level {
        [
            self.current,
            self.t_cell,
            self.t_terminal,
            self.t_air_out,
            self.t_air_in,
            self.soc,
        ]
        .into_iter()
        .chain(self.cell)
        .max()
        .unwrap_or_default()
    }
}

fn v(mv: u16) -> f64 {
    f64::from(mv) / 1000.0
}

fn a(ma: u32) -> f64 {
    f64::from(ma) / 1000.0
}

fn c(deci: i16) -> f64 {
    f64::from(deci) / 10.0
}

/// Red if `alarm`, else yellow if `warning`.
fn level(alarm: bool, warning: bool) -> Level {
    if alarm {
        Level::Alarm
    } else if warning {
        Level::Warning
    } else {
        Level::Normal
    }
}

/// A temperature against optional red limits (high, low) and the yellow
/// levels.
fn temperature(t: Option<f64>, red: (Option<f64>, Option<f64>), warn: &WarningLevels) -> Level {
    t.map_or(Level::Normal, |t| {
        let (high, low) = red;
        level(
            high.is_some_and(|h| t >= h) || low.is_some_and(|l| t <= l),
            t >= warn.temp_high_c || t <= warn.temp_low_c,
        )
    })
}

/// Colours `sample` against the installed CONFIG. `charging` is whether the
/// shown state is CHARGING, which picks the NTC1 limits.
#[must_use]
pub fn levels(
    sample: &Sample,
    config: &Config,
    charging: bool,
    warn: &WarningLevels,
) -> SampleLevels {
    let cell = sample.cell_v.map(|cv| {
        cv.map_or(Level::Normal, |x| {
            level(
                x >= v(config.ov_trip_mv) || x <= v(config.uv_trip_mv),
                x >= warn.cell_high_v || x <= warn.cell_low_v,
            )
        })
    });
    let current = sample.current_a.map_or(Level::Normal, |i| {
        level(
            (i > 0.0 && i >= a(config.chg_oc_ma)) || (i < 0.0 && -i >= a(config.dis_oc_ma)),
            false,
        )
    });
    let cell_limits = if charging {
        (config.chg_ot_trip_deci_c, config.chg_ut_trip_deci_c)
    } else {
        (config.dis_ot_trip_deci_c, config.dis_ut_trip_deci_c)
    };
    SampleLevels {
        current,
        cell,
        t_cell: temperature(
            sample.t_cell_c,
            (Some(c(cell_limits.0)), Some(c(cell_limits.1))),
            warn,
        ),
        t_terminal: temperature(
            sample.t_terminal_c,
            (Some(c(config.terminal_ot_deci_c)), None),
            warn,
        ),
        t_air_out: temperature(
            sample.t_air_out_c,
            (Some(c(config.air_ot_deci_c)), None),
            warn,
        ),
        t_air_in: temperature(sample.t_air_in_c, (None, None), warn),
        soc: sample.soc_pct.map_or(Level::Normal, |s| {
            level(false, s < warn.soc_low_pct || s > warn.soc_high_pct)
        }),
    }
}

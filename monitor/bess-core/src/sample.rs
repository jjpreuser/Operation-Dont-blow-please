//! One telemetry sample in SI units (docs/design/database.md §1; DD-41,
//! DD-44, DD-72; S8.8).
//!
//! The wire values are converted once, here: mV to V, 10 mA to A with the
//! sign inverted (wire negative = charging, E5.8; here positive = charging),
//! 0.1 °C to °C, 0.01 % to %. A field whose validity bit is clear is `None`
//! (shown as N/A, stored as NULL). Power is `pack_v × current_a` (S8.8), so
//! it is positive while charging too.
//!
//! Conversions divide by a power of ten instead of multiplying by 0.1, so a
//! value and a CONFIG limit with the same wire number compare equal.

use protocol::messages::Telemetry;

/// `ntc_present_mask` bit for NTC4, the optional air-inlet sensor.
const NTC4_PRESENT: u8 = 1 << 3;

/// The telemetry signals of one sample (D2), named as their DB columns.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Sample {
    pub pack_v: Option<f64>,
    /// Positive = charging (DD-72).
    pub current_a: Option<f64>,
    /// `pack_v × current_a`; `None` if either is invalid.
    pub power_w: Option<f64>,
    /// `cg1_v` … `cg4_v`.
    pub cell_v: [Option<f64>; 4],
    /// NTC1, core cell.
    pub t_cell_c: Option<f64>,
    /// NTC2, terminal joint.
    pub t_terminal_c: Option<f64>,
    /// NTC3, air downstream of the pack.
    pub t_air_out_c: Option<f64>,
    /// NTC4, air inlet; `None` unless CONFIG says it is fitted (DD-41).
    pub t_air_in_c: Option<f64>,
    /// From the MCU only (DD-44).
    pub soc_pct: Option<f64>,
}

impl Sample {
    /// Converts a TELEMETRY or `FAULT_UPDATE` frame. `ntc_present_mask` comes
    /// from the installed CONFIG.
    #[must_use]
    pub fn from_telemetry(t: &Telemetry, ntc_present_mask: u8) -> Sample {
        let pack_v = t.valid_pack_mv().map(|mv| f64::from(mv) / 1000.0);
        let current_a = t.valid_pack_current_10ma().map(|c| -f64::from(c) / 100.0);
        let temperature = |i: usize| t.valid_temperature_deci_c(i).map(|d| f64::from(d) / 10.0);
        Sample {
            pack_v,
            current_a,
            power_w: pack_v.zip(current_a).map(|(v, a)| v * a),
            cell_v: std::array::from_fn(|i| t.valid_cell_mv(i).map(|mv| f64::from(mv) / 1000.0)),
            t_cell_c: temperature(0),
            t_terminal_c: temperature(1),
            t_air_out_c: temperature(2),
            t_air_in_c: temperature(3).filter(|_| ntc_present_mask & NTC4_PRESENT != 0),
            soc_pct: t.valid_soc_centi_pct().map(|c| f64::from(c) / 100.0),
        }
    }

    /// Lowest and highest valid cell-group voltage.
    #[must_use]
    pub fn cell_range(&self) -> Option<(f64, f64)> {
        self.cell_v
            .iter()
            .flatten()
            .fold(None, |acc, &v| match acc {
                None => Some((v, v)),
                Some((lo, hi)) => Some((lo.min(v), hi.max(v))),
            })
    }
}

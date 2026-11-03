//! Fan curves (D4 §4, `docs/design/fan-control.md`; S6.2–S6.4, DD-52, DD-53).
//!
//! Each curve (battery: NTC1 + battery table, air: NTC3 + air table) maps a
//! temperature to a fan duty by linear interpolation in a 2–8 point table from
//! CONFIG, with its own on/off hysteresis. The fan demand is the maximum of the
//! two curves. Temperatures are in 0.1 °C, as in the UART frames.

/// Most points a CONFIG fan table can hold (S6.2, UART draft §7).
pub const MAX_POINTS: usize = 8;
/// Fewest points a usable table needs.
pub const MIN_POINTS: usize = 2;

/// One table point: temperature in 0.1 °C and duty in %.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FanPoint {
    pub temp_dc: i16,
    pub duty_pct: u8,
}

/// Why a table from CONFIG is rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableError {
    /// Fewer than 2 or more than 8 points.
    PointCount(usize),
    /// Temperatures are not strictly ascending.
    NotAscending,
    /// A duty above 100 %.
    DutyOutOfRange(u8),
}

/// A validated fan table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FanTable {
    points: Vec<FanPoint>,
}

impl FanTable {
    pub fn new(points: &[FanPoint]) -> Result<Self, TableError> {
        if !(MIN_POINTS..=MAX_POINTS).contains(&points.len()) {
            return Err(TableError::PointCount(points.len()));
        }
        if let Some(p) = points.iter().find(|p| p.duty_pct > 100) {
            return Err(TableError::DutyOutOfRange(p.duty_pct));
        }
        if points.windows(2).any(|w| w[1].temp_dc <= w[0].temp_dc) {
            return Err(TableError::NotAscending);
        }
        Ok(Self {
            points: points.to_vec(),
        })
    }

    /// Duty at `temp_dc`: linear between points, rounded to the nearest
    /// integer with halves upward; clamped to the first and last point.
    pub fn interp(&self, temp_dc: i16) -> u8 {
        let first = self.points[0];
        let last = self.points[self.points.len() - 1];
        if temp_dc <= first.temp_dc {
            return first.duty_pct;
        }
        if temp_dc >= last.temp_dc {
            return last.duty_pct;
        }
        let (lo, hi) = self
            .points
            .windows(2)
            .map(|w| (w[0], w[1]))
            .find(|(_, hi)| temp_dc <= hi.temp_dc)
            .expect("temp_dc is inside the table");
        let dt = i32::from(hi.temp_dc) - i32::from(lo.temp_dc);
        let scaled = i32::from(lo.duty_pct) * dt
            + (i32::from(temp_dc) - i32::from(lo.temp_dc))
                * (i32::from(hi.duty_pct) - i32::from(lo.duty_pct));
        // round(scaled / dt), halves upward, also for falling segments
        let duty = (2 * scaled + dt).div_euclid(2 * dt);
        u8::try_from(duty).expect("duty lies between two table duties")
    }
}

/// On/off state of one curve (DD-53).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurveState {
    On,
    Off,
}

/// One fan curve with hysteresis.
#[derive(Debug, Clone)]
pub struct Curve {
    table: FanTable,
    on_dc: i16,
    off_dc: i16,
    min_pct: u8,
    state: CurveState,
}

impl Curve {
    /// A curve starts ON when the temperature is above its off point, so a
    /// restart never stops fans that were running inside the band. With an
    /// unknown start temperature it starts ON (DD-52).
    pub fn new(
        table: FanTable,
        on_dc: i16,
        off_dc: i16,
        min_pct: u8,
        start_temp_dc: Option<i16>,
    ) -> Self {
        let state = match start_temp_dc {
            Some(t) if t <= off_dc => CurveState::Off,
            _ => CurveState::On,
        };
        Self {
            table,
            on_dc,
            off_dc,
            min_pct,
            state,
        }
    }

    pub fn state(&self) -> CurveState {
        self.state
    }

    /// Updates the state for a new reading and returns the demand in %.
    /// `None` is an invalid sensor: the curve demands 100 % (DD-52).
    pub fn update(&mut self, temp_dc: Option<i16>) -> u8 {
        let Some(t) = temp_dc else {
            return 100;
        };
        self.state = match self.state {
            CurveState::Off if t >= self.on_dc => CurveState::On,
            CurveState::On if t <= self.off_dc => CurveState::Off,
            s => s,
        };
        match self.state {
            CurveState::Off => 0,
            CurveState::On => self.table.interp(t).max(self.min_pct),
        }
    }
}

/// Which curve sets the duty (used for E12, D4 §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Driving {
    Battery,
    Air,
}

/// Fan demand = max(battery, air) (S6.3); on a tie the battery curve drives.
pub fn fan_demand(battery_pct: u8, air_pct: u8) -> (u8, Driving) {
    if air_pct > battery_pct {
        (air_pct, Driving::Air)
    } else {
        (battery_pct, Driving::Battery)
    }
}

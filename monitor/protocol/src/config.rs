//! CONFIG payload (UART draft §6–7, schema 1): decoding, encoding and the
//! validation the Pi runs before it answers with `CONFIG_ACK` (§8).
//!
//! [`Config::from_bytes`] decodes all 190 bytes without judging them and
//! [`Config::to_bytes`] gives the same bytes back, so a rejected CONFIG can
//! still be logged, and the raw bytes stored with an accepted one (DD-43).
//! [`Config::check`] is the Pi's verdict, in the order of §8: schema, then
//! identity, then content, each with its own `CONFIG_ACK` status.
//!
//! The content rules are the §7 validation list and nothing more (DD-67): if
//! the Pi rejected a CONFIG the MCU accepted, charging would stay inhibited.
//! Two §7 items are read as proposed in docs/design/firmware-requests.md §6
//! (threshold ordering, table endpoints). Two are left out because the draft
//! gives no numbers, and leaving them out never rejects a CONFIG the MCU
//! accepts: "capacity within approved battery range" (only `capacity > 0` is
//! checked) and "current/profile limits meet approved battery/AFE ratings".
//!
//! Rules that appear only in the §6–7 description text, not in the list, are
//! not checked either (firmware-requests.md §6 asks firmware about them). The
//! values they would reject are kept, and the caller handles them safely:
//! - `current_profile` outside 0–1 decodes as [`CurrentProfile::Unknown`];
//! - `fan_offline_action` outside 0–2 decodes as [`FanOfflineAction::Unknown`];
//! - an RPM-calibration flag set while an active nonzero-duty point has RPM 0
//!   is accepted, but [`Config::rpm_calibrated`] then reports the table as
//!   uncalibrated, so E12 stays disabled for it (DD-54);
//! - nonzero RPMs with the flag clear are accepted and ignored.
//!
//! Values stay in wire units (mV, mA, 0.1 °C, 0.01 %, ms).

use crate::messages::{i16_at, put, u16_at, u32_at, AckStatus, ConfigAck, Identity, CONFIG_LEN};
use std::fmt;

/// The only CONFIG schema this crate knows (offset 0).
pub const SCHEMA: u16 = 1;
/// `chemistry` value for `LiFePO4`, the only one schema 1 supports.
pub const CHEMISTRY_LFP: u8 = 1;
/// Series cell groups of this build (4S3P).
pub const SERIES_GROUPS: u8 = 4;
/// Cells per group of this build (4S3P).
pub const PARALLEL_CELLS: u8 = 3;
/// NTC1–3 must be fitted; NTC4 is optional (`ntc_present_mask` bits 0–2).
pub const REQUIRED_NTC_MASK: u8 = 0b0111;
/// Fewest and most active points in a fan table.
pub const MIN_POINTS: u8 = 2;
pub const MAX_POINTS: u8 = 8;
/// Slots per fan table, active or not.
pub const TABLE_SLOTS: usize = MAX_POINTS as usize;
/// Encoding bounds for temperatures, 0.1 °C (§7: not operating limits).
pub const TEMPERATURE_MIN_DECI_C: i16 = -300;
pub const TEMPERATURE_MAX_DECI_C: i16 = 1100;
/// Upper bound of `soc_max_centi_pct` (100.00 %).
pub const SOC_FULL_CENTI_PCT: u16 = 10_000;

const BATTERY_TABLE_AT: usize = 110;
const AIR_TABLE_AT: usize = 150;
const POINT_LEN: usize = 5;

/// Firmware version, three semantic-version components (offsets 10–12, S1.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FirmwareVersion {
    pub major: u8,
    pub minor: u8,
    pub patch: u8,
}

impl fmt::Display for FirmwareVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// `current_profile` (offset 17): which set of current limits the MCU uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CurrentProfile {
    /// 0: today's limits.
    Today,
    /// 1: the separately approved 12 A upgrade.
    Upgrade,
    /// Any other byte, kept for display and the log (not a §7 rule).
    Unknown(u8),
}

impl From<u8> for CurrentProfile {
    fn from(value: u8) -> CurrentProfile {
        match value {
            0 => CurrentProfile::Today,
            1 => CurrentProfile::Upgrade,
            other => CurrentProfile::Unknown(other),
        }
    }
}

impl From<CurrentProfile> for u8 {
    fn from(profile: CurrentProfile) -> u8 {
        match profile {
            CurrentProfile::Today => 0,
            CurrentProfile::Upgrade => 1,
            CurrentProfile::Unknown(other) => other,
        }
    }
}

/// `fan_offline_action` (offset 108): what the Pi does with the fans while
/// the link is OFFLINE (DD-16, DD-60).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FanOfflineAction {
    /// 0: policy not decided yet; the Pi runs the fans at 100 % (DD-28).
    Undecided,
    /// 1: hold the last duty, then off after `fan_offline_ms`.
    HoldThenOff,
    /// 2: 100 % while OFFLINE (the Pi's proposal for production, DD-60).
    Full,
    /// Any other byte, kept (not a §7 rule); treat it like `Undecided`.
    Unknown(u8),
}

impl From<u8> for FanOfflineAction {
    fn from(value: u8) -> FanOfflineAction {
        match value {
            0 => FanOfflineAction::Undecided,
            1 => FanOfflineAction::HoldThenOff,
            2 => FanOfflineAction::Full,
            other => FanOfflineAction::Unknown(other),
        }
    }
}

impl From<FanOfflineAction> for u8 {
    fn from(action: FanOfflineAction) -> u8 {
        match action {
            FanOfflineAction::Undecided => 0,
            FanOfflineAction::HoldThenOff => 1,
            FanOfflineAction::Full => 2,
            FanOfflineAction::Unknown(other) => other,
        }
    }
}

/// The two fan tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Table {
    /// Driven by NTC1 (core cell).
    Battery,
    /// Driven by NTC3 (downstream air).
    Air,
}

impl fmt::Display for Table {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Table::Battery => "battery",
            Table::Air => "air",
        })
    }
}

/// `config_flags` (offset 22). Bit 0: battery RPM calibration valid; bit 1:
/// air RPM calibration valid; the other bits are reserved and must be zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ConfigFlags(pub u16);

impl ConfigFlags {
    pub const BATTERY_RPM_CALIBRATED: u16 = 1 << 0;
    pub const AIR_RPM_CALIBRATED: u16 = 1 << 1;
    pub const RESERVED: u16 = !(Self::BATTERY_RPM_CALIBRATED | Self::AIR_RPM_CALIBRATED);

    /// The flag bit as sent; see [`Config::rpm_calibrated`] for whether the
    /// calibration can be used.
    #[must_use]
    pub fn rpm_calibrated(self, table: Table) -> bool {
        let bit = match table {
            Table::Battery => Self::BATTERY_RPM_CALIBRATED,
            Table::Air => Self::AIR_RPM_CALIBRATED,
        };
        self.0 & bit != 0
    }
}

/// One fan table point (5 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FanPoint {
    /// 0.1 °C (+0).
    pub temperature_deci_c: i16,
    /// 0–100 % (+2).
    pub duty_pct: u8,
    /// Expected fan speed at this duty, 0 = not calibrated (+3).
    pub expected_rpm: u16,
}

impl FanPoint {
    fn from_bytes(b: &[u8], at: usize) -> FanPoint {
        FanPoint {
            temperature_deci_c: i16_at(b, at),
            duty_pct: b[at + 2],
            expected_rpm: u16_at(b, at + 3),
        }
    }

    fn write(self, b: &mut [u8], at: usize) {
        put(b, at, &self.temperature_deci_c.to_le_bytes());
        b[at + 2] = self.duty_pct;
        put(b, at + 3, &self.expected_rpm.to_le_bytes());
    }
}

/// A fan table: `count` active points first, the rest of the 8 slots zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FanTable {
    /// Active points, 2–8 when valid (offset 106 or 107).
    pub count: u8,
    pub points: [FanPoint; TABLE_SLOTS],
}

impl FanTable {
    /// The active points; all 8 slots if `count` is above 8.
    #[must_use]
    pub fn active(&self) -> &[FanPoint] {
        &self.points[..usize::from(self.count).min(TABLE_SLOTS)]
    }
}

/// The decoded CONFIG payload, every field as sent.
///
/// Field names follow §6 with the unit as suffix. Decoding never fails; use
/// [`Config::check`] before using any value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Offset 0. Only [`SCHEMA`] is supported; the layout below is schema 1.
    pub schema: u16,
    /// Offset 2.
    pub config_revision: u32,
    /// Offset 6.
    pub boot_counter: u32,
    /// Offsets 10–12.
    pub firmware: FirmwareVersion,
    /// Offset 13, 1 = LFP.
    pub chemistry: u8,
    /// Offset 14.
    pub series_groups: u8,
    /// Offset 15.
    pub parallel_cells: u8,
    /// Offset 16, bits 0–3 = NTC1–4.
    pub ntc_present_mask: u8,
    /// Offset 17.
    pub current_profile: CurrentProfile,
    /// Pack capacity, mAh (offset 18). Not multiplied by the series groups.
    pub capacity_mah: u32,
    /// Offset 22.
    pub flags: ConfigFlags,

    /// Cell overvoltage trip, mV (offset 24).
    pub ov_trip_mv: u16,
    /// Offset 26.
    pub ov_recover_mv: u16,
    /// Cell undervoltage trip, mV (offset 28).
    pub uv_trip_mv: u16,
    /// Offset 30.
    pub uv_recover_mv: u16,
    /// Charge overcurrent, mA, positive magnitude (offset 32).
    pub chg_oc_ma: u32,
    /// Discharge overcurrent, mA (offset 36).
    pub dis_oc_ma: u32,
    /// Discharge overload threshold, mA (offset 40).
    pub dis_overload_ma: u32,
    /// Discharge peak cap, mA (offset 44).
    pub dis_peak_ma: u32,

    /// Charge undertemperature trip, 0.1 °C (offset 48).
    pub chg_ut_trip_deci_c: i16,
    /// Offset 50.
    pub chg_ut_recover_deci_c: i16,
    /// Charge overtemperature trip (offset 52).
    pub chg_ot_trip_deci_c: i16,
    /// Offset 54.
    pub chg_ot_recover_deci_c: i16,
    /// Discharge undertemperature trip (offset 56).
    pub dis_ut_trip_deci_c: i16,
    /// Offset 58.
    pub dis_ut_recover_deci_c: i16,
    /// Discharge overtemperature trip (offset 60).
    pub dis_ot_trip_deci_c: i16,
    /// Offset 62.
    pub dis_ot_recover_deci_c: i16,
    /// Terminal overtemperature, NTC2 (offset 64).
    pub terminal_ot_deci_c: i16,
    /// Terminal trip on NTC2 − NTC1 above this, a difference (offset 66).
    pub terminal_delta_deci_c: i16,
    /// Enclosure air warning, NTC3 (offset 68).
    pub air_ot_deci_c: i16,
    /// Offset 70.
    pub air_recover_deci_c: i16,

    /// Offset 72.
    pub voltage_detect_ms: u16,
    /// Offset 74.
    pub temp_detect_ms: u16,
    /// Offset 76.
    pub recover_ms: u16,
    /// Offset 78.
    pub overload_ms: u16,

    /// State-of-charge operating window, 0.01 % (offsets 80 and 82).
    pub soc_min_centi_pct: u16,
    pub soc_max_centi_pct: u16,

    /// Fan hysteresis, 0.1 °C (offsets 84–90).
    pub battery_fan_on_deci_c: i16,
    pub battery_fan_off_deci_c: i16,
    pub air_fan_on_deci_c: i16,
    pub air_fan_off_deci_c: i16,
    /// Lowest duty while a fan is on, % (offset 92).
    pub fan_min_pct: u8,
    /// E12 when a fan runs below this % of its expected RPM (offset 93).
    pub fan_speed_min_pct: u8,
    /// Offset 94.
    pub fan_fault_ms: u16,
    /// Offset 96; 65535 = undecided, not a real timeout.
    pub fan_offline_ms: u16,
    /// Offset 98.
    pub telemetry_ms: u16,
    /// Offset 100.
    pub heartbeat_ms: u16,
    /// Offset 102.
    pub mcu_pi_timeout_ms: u16,
    /// Offset 104.
    pub pi_data_timeout_ms: u16,

    /// Count at offset 106, points at 110.
    pub battery_table: FanTable,
    /// Count at offset 107, points at 150.
    pub air_table: FanTable,
    /// Offset 108.
    pub fan_offline_action: FanOfflineAction,
    /// Offset 109, must be zero.
    pub reserved: u8,
}

fn table_from_bytes(b: &[u8], count: u8, at: usize) -> FanTable {
    FanTable {
        count,
        points: std::array::from_fn(|i| FanPoint::from_bytes(b, at + POINT_LEN * i)),
    }
}

fn write_table(b: &mut [u8], table: &FanTable, at: usize) {
    for (i, point) in table.points.iter().enumerate() {
        point.write(b, at + POINT_LEN * i);
    }
}

impl Config {
    /// Decodes every field. Never fails and keeps every byte, so
    /// `Config::from_bytes(b).to_bytes() == *b` for any `b`.
    #[must_use]
    pub fn from_bytes(b: &[u8; CONFIG_LEN]) -> Config {
        Config {
            schema: u16_at(b, 0),
            config_revision: u32_at(b, 2),
            boot_counter: u32_at(b, 6),
            firmware: FirmwareVersion {
                major: b[10],
                minor: b[11],
                patch: b[12],
            },
            chemistry: b[13],
            series_groups: b[14],
            parallel_cells: b[15],
            ntc_present_mask: b[16],
            current_profile: CurrentProfile::from(b[17]),
            capacity_mah: u32_at(b, 18),
            flags: ConfigFlags(u16_at(b, 22)),
            ov_trip_mv: u16_at(b, 24),
            ov_recover_mv: u16_at(b, 26),
            uv_trip_mv: u16_at(b, 28),
            uv_recover_mv: u16_at(b, 30),
            chg_oc_ma: u32_at(b, 32),
            dis_oc_ma: u32_at(b, 36),
            dis_overload_ma: u32_at(b, 40),
            dis_peak_ma: u32_at(b, 44),
            chg_ut_trip_deci_c: i16_at(b, 48),
            chg_ut_recover_deci_c: i16_at(b, 50),
            chg_ot_trip_deci_c: i16_at(b, 52),
            chg_ot_recover_deci_c: i16_at(b, 54),
            dis_ut_trip_deci_c: i16_at(b, 56),
            dis_ut_recover_deci_c: i16_at(b, 58),
            dis_ot_trip_deci_c: i16_at(b, 60),
            dis_ot_recover_deci_c: i16_at(b, 62),
            terminal_ot_deci_c: i16_at(b, 64),
            terminal_delta_deci_c: i16_at(b, 66),
            air_ot_deci_c: i16_at(b, 68),
            air_recover_deci_c: i16_at(b, 70),
            voltage_detect_ms: u16_at(b, 72),
            temp_detect_ms: u16_at(b, 74),
            recover_ms: u16_at(b, 76),
            overload_ms: u16_at(b, 78),
            soc_min_centi_pct: u16_at(b, 80),
            soc_max_centi_pct: u16_at(b, 82),
            battery_fan_on_deci_c: i16_at(b, 84),
            battery_fan_off_deci_c: i16_at(b, 86),
            air_fan_on_deci_c: i16_at(b, 88),
            air_fan_off_deci_c: i16_at(b, 90),
            fan_min_pct: b[92],
            fan_speed_min_pct: b[93],
            fan_fault_ms: u16_at(b, 94),
            fan_offline_ms: u16_at(b, 96),
            telemetry_ms: u16_at(b, 98),
            heartbeat_ms: u16_at(b, 100),
            mcu_pi_timeout_ms: u16_at(b, 102),
            pi_data_timeout_ms: u16_at(b, 104),
            battery_table: table_from_bytes(b, b[106], BATTERY_TABLE_AT),
            air_table: table_from_bytes(b, b[107], AIR_TABLE_AT),
            fan_offline_action: FanOfflineAction::from(b[108]),
            reserved: b[109],
        }
    }

    #[must_use]
    pub fn to_bytes(&self) -> [u8; CONFIG_LEN] {
        let mut b = [0; CONFIG_LEN];
        put(&mut b, 0, &self.schema.to_le_bytes());
        put(&mut b, 2, &self.config_revision.to_le_bytes());
        put(&mut b, 6, &self.boot_counter.to_le_bytes());
        b[10] = self.firmware.major;
        b[11] = self.firmware.minor;
        b[12] = self.firmware.patch;
        b[13] = self.chemistry;
        b[14] = self.series_groups;
        b[15] = self.parallel_cells;
        b[16] = self.ntc_present_mask;
        b[17] = u8::from(self.current_profile);
        put(&mut b, 18, &self.capacity_mah.to_le_bytes());
        put(&mut b, 22, &self.flags.0.to_le_bytes());
        put(&mut b, 24, &self.ov_trip_mv.to_le_bytes());
        put(&mut b, 26, &self.ov_recover_mv.to_le_bytes());
        put(&mut b, 28, &self.uv_trip_mv.to_le_bytes());
        put(&mut b, 30, &self.uv_recover_mv.to_le_bytes());
        put(&mut b, 32, &self.chg_oc_ma.to_le_bytes());
        put(&mut b, 36, &self.dis_oc_ma.to_le_bytes());
        put(&mut b, 40, &self.dis_overload_ma.to_le_bytes());
        put(&mut b, 44, &self.dis_peak_ma.to_le_bytes());
        put(&mut b, 48, &self.chg_ut_trip_deci_c.to_le_bytes());
        put(&mut b, 50, &self.chg_ut_recover_deci_c.to_le_bytes());
        put(&mut b, 52, &self.chg_ot_trip_deci_c.to_le_bytes());
        put(&mut b, 54, &self.chg_ot_recover_deci_c.to_le_bytes());
        put(&mut b, 56, &self.dis_ut_trip_deci_c.to_le_bytes());
        put(&mut b, 58, &self.dis_ut_recover_deci_c.to_le_bytes());
        put(&mut b, 60, &self.dis_ot_trip_deci_c.to_le_bytes());
        put(&mut b, 62, &self.dis_ot_recover_deci_c.to_le_bytes());
        put(&mut b, 64, &self.terminal_ot_deci_c.to_le_bytes());
        put(&mut b, 66, &self.terminal_delta_deci_c.to_le_bytes());
        put(&mut b, 68, &self.air_ot_deci_c.to_le_bytes());
        put(&mut b, 70, &self.air_recover_deci_c.to_le_bytes());
        put(&mut b, 72, &self.voltage_detect_ms.to_le_bytes());
        put(&mut b, 74, &self.temp_detect_ms.to_le_bytes());
        put(&mut b, 76, &self.recover_ms.to_le_bytes());
        put(&mut b, 78, &self.overload_ms.to_le_bytes());
        put(&mut b, 80, &self.soc_min_centi_pct.to_le_bytes());
        put(&mut b, 82, &self.soc_max_centi_pct.to_le_bytes());
        put(&mut b, 84, &self.battery_fan_on_deci_c.to_le_bytes());
        put(&mut b, 86, &self.battery_fan_off_deci_c.to_le_bytes());
        put(&mut b, 88, &self.air_fan_on_deci_c.to_le_bytes());
        put(&mut b, 90, &self.air_fan_off_deci_c.to_le_bytes());
        b[92] = self.fan_min_pct;
        b[93] = self.fan_speed_min_pct;
        put(&mut b, 94, &self.fan_fault_ms.to_le_bytes());
        put(&mut b, 96, &self.fan_offline_ms.to_le_bytes());
        put(&mut b, 98, &self.telemetry_ms.to_le_bytes());
        put(&mut b, 100, &self.heartbeat_ms.to_le_bytes());
        put(&mut b, 102, &self.mcu_pi_timeout_ms.to_le_bytes());
        put(&mut b, 104, &self.pi_data_timeout_ms.to_le_bytes());
        b[106] = self.battery_table.count;
        b[107] = self.air_table.count;
        b[108] = u8::from(self.fan_offline_action);
        b[109] = self.reserved;
        write_table(&mut b, &self.battery_table, BATTERY_TABLE_AT);
        write_table(&mut b, &self.air_table, AIR_TABLE_AT);
        b
    }

    #[must_use]
    pub fn identity(&self) -> Identity {
        Identity {
            boot_counter: self.boot_counter,
            config_revision: self.config_revision,
        }
    }

    /// The `CONFIG_ACK` for this CONFIG with the given status.
    #[must_use]
    pub fn ack(&self, status: AckStatus) -> ConfigAck {
        ConfigAck {
            boot_counter: self.boot_counter,
            config_revision: self.config_revision,
            status,
        }
    }

    #[must_use]
    pub fn table(&self, table: Table) -> &FanTable {
        match table {
            Table::Battery => &self.battery_table,
            Table::Air => &self.air_table,
        }
    }

    /// Whether the expected RPMs of `table` can be used for E12: its flag is
    /// set and every active point with a nonzero duty has a nonzero RPM (§7).
    /// When false, E12 is disabled for that table and the display shows
    /// calibration unavailable (DD-54).
    #[must_use]
    pub fn rpm_calibrated(&self, table: Table) -> bool {
        self.flags.rpm_calibrated(table)
            && self
                .table(table)
                .active()
                .iter()
                .all(|p| p.duty_pct == 0 || p.expected_rpm != 0)
    }

    /// The Pi's verdict on this CONFIG, checked in the order of §8: schema
    /// (status 1), identity against the latest valid TELEMETRY (status 3),
    /// content (status 2).
    ///
    /// # Errors
    ///
    /// The first stage that fails, as a [`Rejection`]; its
    /// [`Rejection::status`] goes in the `CONFIG_ACK`.
    pub fn check(&self, telemetry: Identity) -> Result<(), Rejection> {
        if self.schema != SCHEMA {
            return Err(Rejection::UnsupportedSchema(self.schema));
        }
        if self.identity() != telemetry {
            return Err(Rejection::IdentityMismatch {
                config: self.identity(),
                telemetry,
            });
        }
        let violations = self.violations();
        if violations.is_empty() {
            Ok(())
        } else {
            Err(Rejection::InvalidContent(violations))
        }
    }

    /// Every broken §7 content rule, in list order; empty if the content is
    /// valid. Assumes schema 1 and does not look at the identity.
    #[must_use]
    pub fn violations(&self) -> Vec<Violation> {
        let mut out = Vec::new();
        self.check_reserved(&mut out);
        self.check_build(&mut out);
        for table in [Table::Battery, Table::Air] {
            self.check_table(table, &mut out);
        }
        self.check_ordering(&mut out);
        if !(self.soc_min_centi_pct < self.soc_max_centi_pct
            && self.soc_max_centi_pct <= SOC_FULL_CENTI_PCT)
        {
            out.push(Violation::SocRange {
                min: self.soc_min_centi_pct,
                max: self.soc_max_centi_pct,
            });
        }
        for (field, value) in self.periods() {
            if value == 0 {
                out.push(Violation::ZeroPeriod(field));
            }
        }
        for (field, value) in self.temperatures() {
            if !(TEMPERATURE_MIN_DECI_C..=TEMPERATURE_MAX_DECI_C).contains(&value) {
                out.push(Violation::TemperatureOutOfRange { field, value });
            }
        }
        out
    }

    /// Reserved bits and bytes zero: `config_flags` bits 2–15,
    /// `ntc_present_mask` bits 4–7 and byte 109. Inactive table slots are
    /// checked per table.
    fn check_reserved(&self, out: &mut Vec<Violation>) {
        if self.flags.0 & ConfigFlags::RESERVED != 0 {
            out.push(Violation::ReservedFlags(self.flags.0));
        }
        if self.ntc_present_mask & 0xF0 != 0 {
            out.push(Violation::ReservedNtcBits(self.ntc_present_mask));
        }
        if self.reserved != 0 {
            out.push(Violation::ReservedByte(self.reserved));
        }
    }

    /// Chemistry, cell arrangement, capacity and fitted NTCs.
    fn check_build(&self, out: &mut Vec<Violation>) {
        if self.chemistry != CHEMISTRY_LFP {
            out.push(Violation::Chemistry(self.chemistry));
        }
        if self.series_groups != SERIES_GROUPS {
            out.push(Violation::SeriesGroups(self.series_groups));
        }
        if self.parallel_cells != PARALLEL_CELLS {
            out.push(Violation::ParallelCells(self.parallel_cells));
        }
        if self.capacity_mah == 0 {
            out.push(Violation::ZeroCapacity);
        }
        if self.ntc_present_mask & REQUIRED_NTC_MASK != REQUIRED_NTC_MASK {
            out.push(Violation::MissingNtc(self.ntc_present_mask));
        }
    }

    /// Count 2–8, inactive slots zero, active temperatures within the
    /// encoding bounds and strictly increasing, duty 0–100 and nondecreasing,
    /// and the endpoints of firmware-requests.md §6. A table with a bad count
    /// gets only that violation.
    fn check_table(&self, table: Table, out: &mut Vec<Violation>) {
        let t = self.table(table);
        if !(MIN_POINTS..=MAX_POINTS).contains(&t.count) {
            out.push(Violation::PointCount {
                table,
                count: t.count,
            });
            return;
        }
        let active = t.active();
        for (index, point) in t.points.iter().enumerate().skip(active.len()) {
            if *point != FanPoint::default() {
                out.push(Violation::InactivePointNotZero { table, index });
            }
        }
        for (index, point) in active.iter().enumerate() {
            let value = point.temperature_deci_c;
            if !(TEMPERATURE_MIN_DECI_C..=TEMPERATURE_MAX_DECI_C).contains(&value) {
                out.push(Violation::PointTemperatureOutOfRange {
                    table,
                    index,
                    value,
                });
            }
            if point.duty_pct > 100 {
                out.push(Violation::DutyAbove100 {
                    table,
                    index,
                    duty: point.duty_pct,
                });
            }
        }
        for (index, pair) in active.windows(2).enumerate() {
            if pair[1].temperature_deci_c <= pair[0].temperature_deci_c {
                out.push(Violation::TemperaturesNotIncreasing {
                    table,
                    index: index + 1,
                });
            }
            if pair[1].duty_pct < pair[0].duty_pct {
                out.push(Violation::DutyDecreasing {
                    table,
                    index: index + 1,
                });
            }
        }
        self.check_endpoints(table, active, out);
    }

    /// Pi reading of "table endpoints consistent" (firmware-requests.md §6):
    /// the first point at or below the off point with 0 %, the last point
    /// 100 %, every point at or above the on point at least `fan_min_pct`.
    fn check_endpoints(&self, table: Table, active: &[FanPoint], out: &mut Vec<Violation>) {
        let (on, off) = match table {
            Table::Battery => (self.battery_fan_on_deci_c, self.battery_fan_off_deci_c),
            Table::Air => (self.air_fan_on_deci_c, self.air_fan_off_deci_c),
        };
        let (Some(first), Some(last)) = (active.first(), active.last()) else {
            return;
        };
        if first.temperature_deci_c > off || first.duty_pct != 0 {
            out.push(Violation::FirstPoint { table });
        }
        if last.duty_pct != 100 {
            out.push(Violation::LastPointNot100 { table });
        }
        for (index, point) in active.iter().enumerate() {
            if point.temperature_deci_c >= on && point.duty_pct < self.fan_min_pct {
                out.push(Violation::BelowFanMin { table, index });
            }
        }
    }

    /// Pi reading of "threshold ordering" (firmware-requests.md §6): each
    /// recover value on the safe side of its trip, hysteresis off below on.
    fn check_ordering(&self, out: &mut Vec<Violation>) {
        let mut below = |lower: (&'static str, i32), upper: (&'static str, i32)| {
            if lower.1 >= upper.1 {
                out.push(Violation::Order {
                    lower: lower.0,
                    upper: upper.0,
                });
            }
        };
        let mv = |name, v: u16| (name, i32::from(v));
        let dc = |name, v: i16| (name, i32::from(v));
        below(
            mv("ov_recover_mV", self.ov_recover_mv),
            mv("ov_trip_mV", self.ov_trip_mv),
        );
        below(
            mv("uv_trip_mV", self.uv_trip_mv),
            mv("uv_recover_mV", self.uv_recover_mv),
        );
        below(
            mv("uv_recover_mV", self.uv_recover_mv),
            mv("ov_recover_mV", self.ov_recover_mv),
        );
        below(
            dc("chg_ut_trip_dC", self.chg_ut_trip_deci_c),
            dc("chg_ut_recover_dC", self.chg_ut_recover_deci_c),
        );
        below(
            dc("chg_ot_recover_dC", self.chg_ot_recover_deci_c),
            dc("chg_ot_trip_dC", self.chg_ot_trip_deci_c),
        );
        below(
            dc("dis_ut_trip_dC", self.dis_ut_trip_deci_c),
            dc("dis_ut_recover_dC", self.dis_ut_recover_deci_c),
        );
        below(
            dc("dis_ot_recover_dC", self.dis_ot_recover_deci_c),
            dc("dis_ot_trip_dC", self.dis_ot_trip_deci_c),
        );
        below(
            dc("air_recover_dC", self.air_recover_deci_c),
            dc("air_ot_dC", self.air_ot_deci_c),
        );
        below(
            dc("battery_fan_off_dC", self.battery_fan_off_deci_c),
            dc("battery_fan_on_dC", self.battery_fan_on_deci_c),
        );
        below(
            dc("air_fan_off_dC", self.air_fan_off_deci_c),
            dc("air_fan_on_dC", self.air_fan_on_deci_c),
        );
    }

    /// "All periods nonzero": every `_ms` field, with the §6 names.
    fn periods(&self) -> [(&'static str, u16); 10] {
        [
            ("voltage_detect_ms", self.voltage_detect_ms),
            ("temp_detect_ms", self.temp_detect_ms),
            ("recover_ms", self.recover_ms),
            ("overload_ms", self.overload_ms),
            ("fan_fault_ms", self.fan_fault_ms),
            ("fan_offline_ms", self.fan_offline_ms),
            ("telemetry_ms", self.telemetry_ms),
            ("heartbeat_ms", self.heartbeat_ms),
            ("mcu_pi_timeout_ms", self.mcu_pi_timeout_ms),
            ("pi_data_timeout_ms", self.pi_data_timeout_ms),
        ]
    }

    /// The fixed temperature fields, for the encoding bounds; fan table
    /// points are checked per table. `terminal_delta_dC` is a difference,
    /// not a temperature, so it is not bounded.
    fn temperatures(&self) -> [(&'static str, i16); 15] {
        [
            ("chg_ut_trip_dC", self.chg_ut_trip_deci_c),
            ("chg_ut_recover_dC", self.chg_ut_recover_deci_c),
            ("chg_ot_trip_dC", self.chg_ot_trip_deci_c),
            ("chg_ot_recover_dC", self.chg_ot_recover_deci_c),
            ("dis_ut_trip_dC", self.dis_ut_trip_deci_c),
            ("dis_ut_recover_dC", self.dis_ut_recover_deci_c),
            ("dis_ot_trip_dC", self.dis_ot_trip_deci_c),
            ("dis_ot_recover_dC", self.dis_ot_recover_deci_c),
            ("terminal_ot_dC", self.terminal_ot_deci_c),
            ("air_ot_dC", self.air_ot_deci_c),
            ("air_recover_dC", self.air_recover_deci_c),
            ("battery_fan_on_dC", self.battery_fan_on_deci_c),
            ("battery_fan_off_dC", self.battery_fan_off_deci_c),
            ("air_fan_on_dC", self.air_fan_on_deci_c),
            ("air_fan_off_dC", self.air_fan_off_deci_c),
        ]
    }
}

/// One broken §7 content rule. Field names are the §6 names, for the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    /// `config_flags` bits 2–15 not zero.
    ReservedFlags(u16),
    /// `ntc_present_mask` bits 4–7 not zero.
    ReservedNtcBits(u8),
    /// Byte 109 not zero.
    ReservedByte(u8),
    /// A slot past `count` is not all zero.
    InactivePointNotZero {
        table: Table,
        index: usize,
    },
    Chemistry(u8),
    SeriesGroups(u8),
    ParallelCells(u8),
    ZeroCapacity,
    /// NTC1, NTC2 or NTC3 missing from the mask.
    MissingNtc(u8),
    /// Count outside 2–8.
    PointCount {
        table: Table,
        count: u8,
    },
    /// Active point temperature outside −300..1100.
    PointTemperatureOutOfRange {
        table: Table,
        index: usize,
        value: i16,
    },
    DutyAbove100 {
        table: Table,
        index: usize,
        duty: u8,
    },
    /// Point `index` is not warmer than the one before it.
    TemperaturesNotIncreasing {
        table: Table,
        index: usize,
    },
    /// Point `index` has a lower duty than the one before it.
    DutyDecreasing {
        table: Table,
        index: usize,
    },
    /// First point above the off point or not 0 %.
    FirstPoint {
        table: Table,
    },
    LastPointNot100 {
        table: Table,
    },
    /// Point at or above the on point with a duty below `fan_min_pct`.
    BelowFanMin {
        table: Table,
        index: usize,
    },
    /// `lower` must be strictly below `upper`.
    Order {
        lower: &'static str,
        upper: &'static str,
    },
    /// Not `min < max <= 10000`.
    SocRange {
        min: u16,
        max: u16,
    },
    ZeroPeriod(&'static str),
    /// A fixed temperature field outside −300..1100.
    TemperatureOutOfRange {
        field: &'static str,
        value: i16,
    },
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Violation::ReservedFlags(v) => write!(f, "config_flags reserved bits set ({v:#06x})"),
            Violation::ReservedNtcBits(v) => {
                write!(f, "ntc_present_mask reserved bits set ({v:#04x})")
            }
            Violation::ReservedByte(v) => write!(f, "reserved byte 109 is {v}, must be 0"),
            Violation::InactivePointNotZero { table, index } => {
                write!(f, "{table} table slot {index} is past count but not zero")
            }
            Violation::Chemistry(v) => write!(f, "chemistry {v}, only {CHEMISTRY_LFP} (LFP)"),
            Violation::SeriesGroups(v) => {
                write!(f, "series_groups {v}, this build has {SERIES_GROUPS}")
            }
            Violation::ParallelCells(v) => {
                write!(f, "parallel_cells {v}, this build has {PARALLEL_CELLS}")
            }
            Violation::ZeroCapacity => f.write_str("capacity_mAh is 0"),
            Violation::MissingNtc(v) => write!(f, "ntc_present_mask {v:#04x} lacks NTC1-3"),
            Violation::PointCount { table, count } => {
                write!(f, "{table}_fan_count {count}, must be 2-8")
            }
            Violation::PointTemperatureOutOfRange {
                table,
                index,
                value,
            } => write!(
                f,
                "{table} table point {index} temperature {value} outside -300..1100"
            ),
            Violation::DutyAbove100 { table, index, duty } => {
                write!(f, "{table} table point {index} duty {duty} % above 100")
            }
            Violation::TemperaturesNotIncreasing { table, index } => write!(
                f,
                "{table} table point {index} not warmer than point {}",
                index - 1
            ),
            Violation::DutyDecreasing { table, index } => write!(
                f,
                "{table} table point {index} duty below point {}",
                index - 1
            ),
            Violation::FirstPoint { table } => write!(
                f,
                "{table} table first point must be at or below {table}_fan_off with 0 %"
            ),
            Violation::LastPointNot100 { table } => {
                write!(f, "{table} table last point must be 100 %")
            }
            Violation::BelowFanMin { table, index } => write!(
                f,
                "{table} table point {index} at or above {table}_fan_on is below fan_min_pct"
            ),
            Violation::Order { lower, upper } => write!(f, "{lower} must be below {upper}"),
            Violation::SocRange { min, max } => {
                write!(f, "SoC window {min}-{max}, needs min < max <= 10000")
            }
            Violation::ZeroPeriod(field) => write!(f, "{field} is 0"),
            Violation::TemperatureOutOfRange { field, value } => {
                write!(f, "{field} {value} outside -300..1100")
            }
        }
    }
}

/// Why the Pi did not accept a CONFIG.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    /// Schema other than [`SCHEMA`]; the rest is not looked at.
    UnsupportedSchema(u16),
    /// The CONFIG belongs to another boot or revision than the MCU reports.
    IdentityMismatch {
        config: Identity,
        telemetry: Identity,
    },
    /// At least one content rule broken, all listed.
    InvalidContent(Vec<Violation>),
}

impl Rejection {
    /// The `CONFIG_ACK` status for this rejection (§8).
    #[must_use]
    pub fn status(&self) -> AckStatus {
        match self {
            Rejection::UnsupportedSchema(_) => AckStatus::UnsupportedSchema,
            Rejection::InvalidContent(_) => AckStatus::InvalidContent,
            Rejection::IdentityMismatch { .. } => AckStatus::IdentityMismatch,
        }
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Rejection::UnsupportedSchema(schema) => {
                write!(f, "CONFIG schema {schema} unsupported (expected {SCHEMA})")
            }
            Rejection::IdentityMismatch { config, telemetry } => write!(
                f,
                "CONFIG is boot {} revision {}, telemetry reports boot {} revision {}",
                config.boot_counter,
                config.config_revision,
                telemetry.boot_counter,
                telemetry.config_revision
            ),
            Rejection::InvalidContent(violations) => {
                f.write_str("CONFIG content invalid: ")?;
                for (i, v) in violations.iter().enumerate() {
                    if i > 0 {
                        f.write_str("; ")?;
                    }
                    write!(f, "{v}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for Rejection {}

//! Names, severities and descriptions of the error codes, and the active
//! fault list (S8.5, S8.10; docs/design/fault-handling.md §1, §4).
//!
//! PROVISIONAL: this table is a hand copy of fault-handling.md §1 (Table A).
//! S4.1 asks for one Table A source file from which firmware, Rust,
//! dashboard and documentation are generated, with a CI check; that needs
//! the firmware team. When it exists, this module is generated from it.
//!
//! Codes E20–E32 come from reserved bits; they are shown as `UNKNOWN`
//! (DD-68) without a severity, after the known codes.

use protocol::messages::{FaultBits, FaultCode};
use std::fmt;

/// Table A severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// Logged and shown only.
    Warning,
    /// Charging or discharging stopped, recovers automatically.
    Protection,
    /// Stopped, explicit reset needed after the cause is removed.
    LatchedProtection,
    /// All outputs off, manual reset; shows the red banner (S8.10).
    Critical,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Severity::Warning => "Warning",
            Severity::Protection => "Protection",
            Severity::LatchedProtection => "Latched protection",
            Severity::Critical => "Critical",
        })
    }
}

/// One Table A row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaultInfo {
    pub code: FaultCode,
    pub name: &'static str,
    /// `None` for a code Table A does not define.
    pub severity: Option<Severity>,
    pub description: &'static str,
}

use Severity::{Critical, LatchedProtection, Protection, Warning};

/// E01–E19, in code order.
const TABLE_A: [(&str, Severity, &str); 19] = [
    (
        "OVERVOLTAGE",
        Protection,
        "A cell voltage is above the maximum cell voltage. Charging stopped.",
    ),
    (
        "UNDERVOLTAGE",
        Protection,
        "A cell voltage is below the minimum cell voltage. Discharging stopped.",
    ),
    (
        "OVERCURRENT_CHARGE",
        LatchedProtection,
        "Charge current is above the charge current limit.",
    ),
    (
        "OVERCURRENT_DISCHARGE",
        LatchedProtection,
        "Discharge current is above the discharge current limit.",
    ),
    (
        "OVERHEATING",
        Protection,
        "Battery temperature > 55 °C. Battery cut off, fans 100 %.",
    ),
    (
        "UNDERTEMPERATURE",
        Protection,
        "Battery temperature too low for charging. Charging stopped.",
    ),
    (
        "SHORT_CIRCUIT",
        Critical,
        "Short circuit detected at the output. All outputs off.",
    ),
    (
        "ESTOP",
        Critical,
        "Emergency stop pressed. All outputs off.",
    ),
    (
        "TEMP_SENSOR_FAULT",
        Critical,
        "A temperature sensor is open, shorted or implausible.",
    ),
    (
        "AFE_COMM_FAULT",
        Critical,
        "MCU cannot communicate with the cell-monitoring chip (AFE).",
    ),
    (
        "PI_COMM_LOST",
        Warning,
        "No data exchange between MCU and Raspberry Pi.",
    ),
    ("FAN_FAULT", Warning, "Fan does not reach expected speed."),
    (
        "CELL_IMBALANCE",
        Warning,
        "Voltage difference between cells larger than allowed.",
    ),
    (
        "WATCHDOG_RESET",
        Warning,
        "MCU was restarted by the watchdog.",
    ),
    (
        "CONFIG_INVALID",
        Critical,
        "Battery configuration failed the start-up check. Outputs stay off.",
    ),
    (
        "TERMINAL_OVERTEMP",
        Critical,
        "Terminal joint (NTC2) too hot, or much hotter than the cells. All outputs off.",
    ),
    (
        "ENCLOSURE_OVERTEMP",
        Warning,
        "Air downstream of the battery (NTC3) too hot. Fans 100 %.",
    ),
    (
        "CHARGE_OVERTEMP",
        Protection,
        "Battery temperature too high for charging (> 50 °C). Charging stopped.",
    ),
    (
        "DISCHARGE_UNDERTEMP",
        Protection,
        "Battery temperature too low for discharging (< −10 °C). Discharging stopped.",
    ),
];

/// The E20–E32 row of the error code table (fault-handling.md §1): the
/// severity an unknown code is logged with. Table A gives these codes none;
/// on the screen they keep no severity (sorted last, no red banner, DD-92),
/// in the event log they are PROTECTION faults with a snapshot (DD-68,
/// DD-97).
pub const UNKNOWN_LOG_SEVERITY: Severity = Severity::Protection;

/// The severity a code is logged with: its Table A severity, or
/// [`UNKNOWN_LOG_SEVERITY`] for E20–E32.
#[must_use]
pub fn log_severity(code: FaultCode) -> Severity {
    info(code).severity.unwrap_or(UNKNOWN_LOG_SEVERITY)
}

/// The Table A row for `code`, or `UNKNOWN` for E20–E32.
#[must_use]
pub fn info(code: FaultCode) -> FaultInfo {
    match TABLE_A.get(usize::from(code.number()) - 1) {
        Some(&(name, severity, description)) => FaultInfo {
            code,
            name,
            severity: Some(severity),
            description,
        },
        None => FaultInfo {
            code,
            name: "UNKNOWN",
            severity: None,
            description: "Fault bit not defined in Table A; firmware and Pi disagree on the error codes.",
        },
    }
}

/// The active faults in display order (fault-handling.md §4, as the LED,
/// S3.7): highest severity first, then lowest code; unknown codes last.
#[must_use]
pub fn active(bits: FaultBits) -> Vec<FaultInfo> {
    let mut list: Vec<FaultInfo> = bits.codes().map(info).collect();
    list.sort_by_key(|f| {
        (
            std::cmp::Reverse(f.severity.map_or(0, |s| s as u8 + 1)),
            f.code,
        )
    });
    list
}

/// Whether the red banner is shown: any Critical fault active (S8.10).
#[must_use]
pub fn critical(bits: FaultBits) -> bool {
    bits.codes()
        .any(|c| info(c).severity == Some(Severity::Critical))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(n: u8) -> FaultCode {
        FaultCode::new(n).unwrap()
    }

    #[test]
    fn known_codes_log_their_table_a_severity() {
        assert_eq!(log_severity(code(5)), Severity::Protection);
        assert_eq!(log_severity(code(3)), Severity::LatchedProtection);
        assert_eq!(log_severity(code(8)), Severity::Critical);
        assert_eq!(log_severity(code(13)), Severity::Warning);
    }

    #[test]
    fn unknown_codes_follow_the_e20_e32_row() {
        for n in 20..=32 {
            assert_eq!(log_severity(code(n)), UNKNOWN_LOG_SEVERITY);
            // On the screen they still have no severity: last, no banner.
            let shown = info(code(n));
            assert_eq!((shown.name, shown.severity), ("UNKNOWN", None));
        }
        assert_eq!(UNKNOWN_LOG_SEVERITY, Severity::Protection);
        assert!(!critical(FaultBits(1 << 19)));
    }
}

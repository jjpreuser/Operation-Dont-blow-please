//! The `event` table: faults, warnings and system events in one table
//! (docs/design/database.md §2; S10.5; DD-20, DD-22, DD-23, DD-36, DD-82,
//! DD-83).
//!
//! Every raise is a new row (DD-83); a fault's row gets `cleared_ts` when it
//! clears. Events are never deleted by the app (DD-23): compaction and the
//! size guard leave the table alone, and deleting a snapshot only clears the
//! event's `snapshot_id`. Names and descriptions are not stored, only code,
//! kind and severity (DD-82). Deciding *when* an event happens is the
//! `event_detector`'s and the other tasks' job; this is the storage side.

use super::{Result, Store, APP_VERSION};
use crate::faults;
use crate::signals::Signals;
use crate::state::ShownState;
use crate::wallclock::utc_text;
use jiff::Timestamp;
use protocol::config::FirmwareVersion;
use protocol::messages::{Identity, McuState, Validity};
use rusqlite::types::Value;
use rusqlite::Connection;
use std::fmt::Write as _;
use std::sync::OnceLock;

/// `event.kind` (DD-20, DD-33).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// Protection, latched protection or critical: gets a snapshot (DD-21).
    Fault,
    Warning,
    System,
}

impl EventKind {
    fn as_str(self) -> &'static str {
        match self {
            EventKind::Fault => "FAULT",
            EventKind::Warning => "WARNING",
            EventKind::System => "SYSTEM",
        }
    }
}

/// `event.severity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventSeverity {
    Critical,
    Protection,
    Latched,
    Warning,
    /// System events.
    Info,
}

impl EventSeverity {
    fn as_str(self) -> &'static str {
        match self {
            EventSeverity::Critical => "CRITICAL",
            EventSeverity::Protection => "PROTECTION",
            EventSeverity::Latched => "LATCHED",
            EventSeverity::Warning => "WARNING",
            EventSeverity::Info => "INFO",
        }
    }
}

impl From<faults::Severity> for EventSeverity {
    fn from(s: faults::Severity) -> EventSeverity {
        match s {
            faults::Severity::Critical => EventSeverity::Critical,
            faults::Severity::Protection => EventSeverity::Protection,
            faults::Severity::LatchedProtection => EventSeverity::Latched,
            faults::Severity::Warning => EventSeverity::Warning,
        }
    }
}

/// `event.source`: the MCU, or the Pi itself (E12, DD-39).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventSource {
    Mcu,
    Pi,
}

impl EventSource {
    fn as_str(self) -> &'static str {
        match self {
            EventSource::Mcu => "MCU",
            EventSource::Pi => "PI",
        }
    }
}

/// `event.system_type` for `SYSTEM` events (D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemType {
    Startup,
    Shutdown,
    LogGap,
    StateChange,
    McuRestart,
    ConfigChange,
    ConfigAccepted,
    ConfigRejected,
    FwChange,
    FaultReset,
    TimeInvalid,
    TimeAdjusted,
    PinLock,
    SnapshotDeleted,
    BackupFailed,
    FanCtrlReset,
    FramesLost,
    FanReenabled,
}

impl SystemType {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SystemType::Startup => "STARTUP",
            SystemType::Shutdown => "SHUTDOWN",
            SystemType::LogGap => "LOG_GAP",
            SystemType::StateChange => "STATE_CHANGE",
            SystemType::McuRestart => "MCU_RESTART",
            SystemType::ConfigChange => "CONFIG_CHANGE",
            SystemType::ConfigAccepted => "CONFIG_ACCEPTED",
            SystemType::ConfigRejected => "CONFIG_REJECTED",
            SystemType::FwChange => "FW_CHANGE",
            SystemType::FaultReset => "FAULT_RESET",
            SystemType::TimeInvalid => "TIME_INVALID",
            SystemType::TimeAdjusted => "TIME_ADJUSTED",
            SystemType::PinLock => "PIN_LOCK",
            SystemType::SnapshotDeleted => "SNAPSHOT_DELETED",
            SystemType::BackupFailed => "BACKUP_FAILED",
            SystemType::FanCtrlReset => "FAN_CTRL_RESET",
            SystemType::FramesLost => "FRAMES_LOST",
            SystemType::FanReenabled => "FAN_REENABLED",
        }
    }
}

/// The values of the frame that raised an event: values at report (DD-22,
/// DD-35).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameValues {
    pub signals: Signals,
    pub state: McuState,
    pub validity: Validity,
}

/// One event to store.
#[derive(Debug, Clone, PartialEq)]
pub struct NewEvent {
    /// Receive time (S10.5).
    pub ts: Timestamp,
    pub kind: EventKind,
    /// Table A code 1–19, 20–32 for an unknown bit (DD-68); `None` for a
    /// system event.
    pub code: Option<u8>,
    pub severity: EventSeverity,
    pub source: EventSource,
    /// Set for system events only.
    pub system_type: Option<SystemType>,
    pub frame: Option<FrameValues>,
    pub detail: Option<String>,
    /// From the installed CONFIG (DD-36).
    pub firmware: Option<FirmwareVersion>,
    pub identity: Option<Identity>,
    pub snapshot_id: Option<i64>,
}

impl NewEvent {
    /// A system event raised by the Pi, severity INFO.
    #[must_use]
    pub fn system(ts: Timestamp, system_type: SystemType, detail: Option<String>) -> NewEvent {
        NewEvent {
            ts,
            kind: EventKind::System,
            code: None,
            severity: EventSeverity::Info,
            source: EventSource::Pi,
            system_type: Some(system_type),
            frame: None,
            detail,
            firmware: None,
            identity: None,
            snapshot_id: None,
        }
    }
}

/// The MCU state as stored, the same names as on the screen (S8.9).
fn state_text(state: McuState) -> String {
    match state {
        McuState::Idle => ShownState::Idle,
        McuState::Charging => ShownState::Charging,
        McuState::Discharging => ShownState::Discharging,
        McuState::Fault => ShownState::Fault,
        McuState::EStop => ShownState::EStop,
        McuState::Unknown(byte) => ShownState::Unknown(byte),
    }
    .to_string()
}

fn insert_event_sql() -> &'static str {
    static SQL: OnceLock<String> = OnceLock::new();
    SQL.get_or_init(|| {
        let mut columns = String::from("ts, kind, code, severity, source, system_type");
        for s in crate::signals::SIGNAL_COLUMNS {
            let _ = write!(columns, ", {s}");
        }
        columns.push_str(
            ", state, validity, detail, fw_version, app_version, boot_counter, config_revision, snapshot_id",
        );
        let n = columns.split(',').count();
        let placeholders: Vec<String> = (1..=n).map(|i| format!("?{i}")).collect();
        format!("INSERT INTO event ({columns}) VALUES ({})", placeholders.join(", "))
    })
}

impl Store {
    /// Stores an event and returns its id.
    ///
    /// # Errors
    ///
    /// SQLite errors, including a CHECK failure for an event that mixes up
    /// code and system type (a fault without a code, a system event with
    /// one).
    pub fn insert_event(&mut self, e: &NewEvent) -> Result<i64> {
        insert_event_on(&self.conn, e)
    }

    /// Marks an active fault or warning cleared (S9.9). Returns `false` if
    /// the event does not exist or was already cleared; the first clear time
    /// is kept.
    ///
    /// # Errors
    ///
    /// SQLite errors.
    pub fn clear_event(&mut self, id: i64, cleared: Timestamp) -> Result<bool> {
        let changed = self
            .conn
            .prepare_cached(
                "UPDATE event SET cleared_ts = ?2 WHERE id = ?1 AND cleared_ts IS NULL",
            )?
            .execute((id, utc_text(cleared)))?;
        Ok(changed > 0)
    }
}

/// [`Store::insert_event`] on any connection, so it can run inside another
/// write's transaction.
pub(super) fn insert_event_on(conn: &Connection, e: &NewEvent) -> Result<i64> {
    let text = |s: &str| Value::Text(s.to_owned());
    let opt_int = |v: Option<i64>| v.map_or(Value::Null, Value::Integer);
    let mut params = vec![
        Value::Text(utc_text(e.ts)),
        text(e.kind.as_str()),
        opt_int(e.code.map(i64::from)),
        text(e.severity.as_str()),
        text(e.source.as_str()),
        e.system_type.map_or(Value::Null, |t| text(t.as_str())),
    ];
    let values = e.frame.map(|f| f.signals.values());
    params.extend(
        values
            .unwrap_or_default()
            .map(|v| v.map_or(Value::Null, Value::Real)),
    );
    params.extend([
        e.frame
            .map_or(Value::Null, |f| Value::Text(state_text(f.state))),
        opt_int(e.frame.map(|f| i64::from(f.validity.0))),
        e.detail.clone().map_or(Value::Null, Value::Text),
        e.firmware
            .map_or(Value::Null, |v| Value::Text(v.to_string())),
        text(APP_VERSION),
        opt_int(e.identity.map(|i| i64::from(i.boot_counter))),
        opt_int(e.identity.map(|i| i64::from(i.config_revision))),
        opt_int(e.snapshot_id),
    ]);
    conn.prepare_cached(insert_event_sql())?
        .execute(rusqlite::params_from_iter(params))?;
    Ok(conn.last_insert_rowid())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_util::{close, ts};

    fn fault(code: u8) -> NewEvent {
        let mut signals = Signals::default();
        signals.sample.pack_v = Some(14.7);
        signals.sample.t_cell_c = Some(56.3);
        signals.fan_duty_pct = Some(100.0);
        NewEvent {
            ts: ts("2026-11-24T10:00:00.250Z"),
            kind: EventKind::Fault,
            code: Some(code),
            severity: faults::Severity::Protection.into(),
            source: EventSource::Mcu,
            system_type: None,
            frame: Some(FrameValues {
                signals,
                state: McuState::Fault,
                validity: Validity(0x05FF),
            }),
            detail: None,
            firmware: Some(FirmwareVersion {
                major: 1,
                minor: 2,
                patch: 3,
            }),
            identity: Some(Identity {
                boot_counter: 7,
                config_revision: 2,
            }),
            snapshot_id: None,
        }
    }

    #[test]
    fn a_fault_is_stored_with_the_values_at_report() {
        type Row = (
            String,
            String,
            i64,
            String,
            String,
            Option<String>,
            f64,
            f64,
            Option<f64>,
            f64,
        );
        let mut store = Store::open_in_memory().unwrap();
        let id = store.insert_event(&fault(5)).unwrap();
        let row: Row = store
            .conn()
            .query_row(
                "SELECT ts, kind, code, severity, source, system_type,
                        pack_v, t_cell_c, current_a, fan_duty_pct FROM event WHERE id = ?1",
                [id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                        r.get(9)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(row.0, "2026-11-24T10:00:00.250Z");
        assert_eq!((row.1.as_str(), row.2), ("FAULT", 5));
        assert_eq!((row.3.as_str(), row.4.as_str()), ("PROTECTION", "MCU"));
        assert_eq!(row.5, None);
        assert!(close(row.6, 14.7) && close(row.7, 56.3) && close(row.9, 100.0));
        assert_eq!(row.8, None, "invalid at report: NULL");

        let (state, validity, fw, app, boot, rev, cleared): (
            String,
            i64,
            String,
            String,
            i64,
            i64,
            Option<String>,
        ) = store
            .conn()
            .query_row(
                "SELECT state, validity, fw_version, app_version, boot_counter, config_revision,
                        cleared_ts FROM event WHERE id = ?1",
                [id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(state, "FAULT");
        assert_eq!(validity, 0x05FF);
        assert_eq!(fw, "1.2.3");
        assert_eq!(app, APP_VERSION);
        assert_eq!((boot, rev), (7, 2));
        assert_eq!(cleared, None, "active");
    }

    #[test]
    fn every_raise_is_a_new_row_and_clears_once() {
        let mut store = Store::open_in_memory().unwrap();
        let first = store.insert_event(&fault(5)).unwrap();
        let second = store.insert_event(&fault(5)).unwrap();
        assert_ne!(first, second, "no merging of repeats (DD-83)");
        assert!(store
            .clear_event(first, ts("2026-11-24T10:00:07Z"))
            .unwrap());
        assert!(!store
            .clear_event(first, ts("2026-11-24T10:05:00Z"))
            .unwrap());
        assert!(!store.clear_event(9999, ts("2026-11-24T10:05:00Z")).unwrap());
        let cleared: Option<String> = store
            .conn()
            .query_row("SELECT cleared_ts FROM event WHERE id = ?1", [first], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(cleared.as_deref(), Some("2026-11-24T10:00:07.000Z"));
        let still_active: Option<String> = store
            .conn()
            .query_row(
                "SELECT cleared_ts FROM event WHERE id = ?1",
                [second],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(still_active, None);
    }

    #[test]
    fn system_events_have_a_type_and_no_code() {
        let mut store = Store::open_in_memory().unwrap();
        let e = NewEvent::system(
            ts("2026-11-24T10:00:00Z"),
            SystemType::FwChange,
            Some("1.2.3 -> 1.3.0".into()),
        );
        let id = store.insert_event(&e).unwrap();
        let (kind, sev, source, ty, detail, code): (
            String,
            String,
            String,
            String,
            String,
            Option<i64>,
        ) = store
            .conn()
            .query_row(
                "SELECT kind, severity, source, system_type, detail, code FROM event WHERE id = ?1",
                [id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            (kind.as_str(), sev.as_str(), source.as_str(), ty.as_str()),
            ("SYSTEM", "INFO", "PI", "FW_CHANGE")
        );
        assert_eq!(detail, "1.2.3 -> 1.3.0");
        assert_eq!(code, None);
    }

    #[test]
    fn mixed_up_events_are_refused() {
        let mut store = Store::open_in_memory().unwrap();
        let mut no_code = fault(5);
        no_code.code = None;
        assert!(store.insert_event(&no_code).is_err());
        let mut system_with_code =
            NewEvent::system(ts("2026-11-24T10:00:00Z"), SystemType::Startup, None);
        system_with_code.code = Some(3);
        assert!(store.insert_event(&system_with_code).is_err());
        let mut out_of_range = fault(5);
        out_of_range.code = Some(33);
        assert!(store.insert_event(&out_of_range).is_err());
    }

    #[test]
    fn unknown_codes_and_states_are_kept() {
        let mut store = Store::open_in_memory().unwrap();
        let mut e = fault(25);
        e.severity = EventSeverity::Protection;
        if let Some(frame) = e.frame.as_mut() {
            frame.state = McuState::Unknown(9);
        }
        let id = store.insert_event(&e).unwrap();
        let (code, state): (i64, String) = store
            .conn()
            .query_row("SELECT code, state FROM event WHERE id = ?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((code, state.as_str()), (25, "UNKNOWN (9)"));
    }

    #[test]
    fn severities_map_from_table_a() {
        use faults::Severity as S;
        let names: Vec<&str> = [S::Critical, S::Protection, S::LatchedProtection, S::Warning]
            .map(|s| EventSeverity::from(s).as_str())
            .to_vec();
        assert_eq!(names, ["CRITICAL", "PROTECTION", "LATCHED", "WARNING"]);
    }
}

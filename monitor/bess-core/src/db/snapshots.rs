//! What `db_writer` does with the event detector's writes: fault events,
//! clears and snapshots (docs/design/fault-handling.md §2–3; S10.5, S10.6;
//! DD-07, DD-21, DD-37, DD-97).
//!
//! At most one snapshot is open (`complete` = 0): the detector extends it
//! for an overlapping fault instead of starting another, and appends to it
//! until 60 s after the last fault. A raise and its snapshot part are one
//! transaction, so an event never points at a missing snapshot.

use super::events::{insert_event_on, state_text};
use super::{Result, Store};
use crate::detector::{SnapshotPart, SnapshotRow, Write};
use crate::signals::SIGNAL_COLUMNS;
use crate::wallclock::utc_text;
use protocol::messages::{FaultBits, FaultCode};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension};
use std::fmt::Write as _;
use std::sync::OnceLock;

fn insert_row_sql() -> &'static str {
    static SQL: OnceLock<String> = OnceLock::new();
    SQL.get_or_init(|| {
        let mut columns = String::from("snapshot_id, ts, state");
        for s in SIGNAL_COLUMNS {
            let _ = write!(columns, ", {s}");
        }
        let n = 3 + SIGNAL_COLUMNS.len();
        let placeholders: Vec<String> = (1..=n).map(|i| format!("?{i}")).collect();
        // A repeated time (clock stepped back) keeps the first row.
        format!(
            "INSERT OR IGNORE INTO snapshot_sample ({columns}) VALUES ({})",
            placeholders.join(", ")
        )
    })
}

fn open_snapshot(conn: &Connection) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM snapshot WHERE complete = 0 ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?)
}

fn insert_rows(conn: &Connection, snapshot_id: i64, rows: &[SnapshotRow]) -> Result<()> {
    let mut stmt = conn.prepare_cached(insert_row_sql())?;
    for row in rows {
        let mut params = vec![
            Value::Integer(snapshot_id),
            Value::Text(utc_text(row.ts)),
            Value::Text(state_text(row.state)),
        ];
        params.extend(
            row.signals
                .values()
                .map(|v| v.map_or(Value::Null, Value::Real)),
        );
        stmt.execute(rusqlite::params_from_iter(params))?;
    }
    Ok(())
}

fn complete_open(conn: &Connection) -> Result<usize> {
    Ok(conn.execute("UPDATE snapshot SET complete = 1 WHERE complete = 0", [])?)
}

impl Store {
    /// Applies one detector write.
    ///
    /// # Errors
    ///
    /// SQLite errors; a failed write changes nothing.
    pub fn apply(&mut self, write: &Write) -> Result<()> {
        let tx = self.conn.transaction()?;
        match write {
            Write::Raise { event, snapshot } => {
                let snapshot_id = match snapshot {
                    SnapshotPart::None => None,
                    SnapshotPart::Start {
                        ts_start,
                        ts_end,
                        rows,
                    } => {
                        // Never two open snapshots.
                        complete_open(&tx)?;
                        tx.execute(
                            "INSERT INTO snapshot (ts_start, ts_end, complete) VALUES (?1, ?2, 0)",
                            (utc_text(*ts_start), utc_text(*ts_end)),
                        )?;
                        let id = tx.last_insert_rowid();
                        insert_rows(&tx, id, rows)?;
                        Some(id)
                    }
                    SnapshotPart::Extend { ts_end } => {
                        let id = open_snapshot(&tx)?;
                        if let Some(id) = id {
                            tx.execute(
                                "UPDATE snapshot SET ts_end = max(ts_end, ?2) WHERE id = ?1",
                                (id, utc_text(*ts_end)),
                            )?;
                        }
                        id
                    }
                };
                let mut event = event.clone();
                event.snapshot_id = snapshot_id;
                insert_event_on(&tx, &event)?;
            }
            Write::Clear { code, ts, detail } => {
                tx.execute(
                    "UPDATE event SET cleared_ts = ?2,
                         detail = CASE WHEN ?3 IS NULL THEN detail
                                       WHEN detail IS NULL THEN ?3
                                       ELSE detail || '; ' || ?3 END
                     WHERE code = ?1 AND kind IN ('FAULT', 'WARNING') AND source = 'MCU'
                       AND cleared_ts IS NULL",
                    (code, utc_text(*ts), detail.as_deref()),
                )?;
            }
            Write::System(event) => {
                insert_event_on(&tx, event)?;
            }
            Write::SnapshotRows(rows) => {
                if let Some(id) = open_snapshot(&tx)? {
                    insert_rows(&tx, id, rows)?;
                }
            }
            Write::SnapshotComplete => {
                complete_open(&tx)?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// The codes of the MCU faults and warnings still open in the database:
    /// what the event detector starts from (DD-37).
    ///
    /// # Errors
    ///
    /// SQLite errors.
    pub fn open_fault_codes(&self) -> Result<FaultBits> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT DISTINCT code FROM event
             WHERE kind IN ('FAULT', 'WARNING') AND source = 'MCU' AND cleared_ts IS NULL",
        )?;
        let mut bits = 0;
        for code in stmt.query_map([], |r| r.get::<_, u8>(0))? {
            if let Some(code) = FaultCode::new(code?) {
                bits |= code.mask();
            }
        }
        Ok(FaultBits(bits))
    }

    /// At core start: a snapshot left open by a crash gets no more rows, so
    /// it is marked complete (and the size guard may delete it later).
    /// Returns how many there were.
    ///
    /// # Errors
    ///
    /// SQLite errors.
    pub fn complete_stale_snapshots(&mut self) -> Result<usize> {
        complete_open(&self.conn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_util::ts;
    use crate::db::{EventKind, EventSeverity, EventSource, NewEvent, SystemType};
    use crate::signals::Signals;
    use crate::wallclock::add_ms;
    use protocol::messages::McuState;

    fn fault(code: u8, at: &str) -> NewEvent {
        NewEvent {
            ts: ts(at),
            kind: EventKind::Fault,
            code: Some(code),
            severity: EventSeverity::Protection,
            source: EventSource::Mcu,
            system_type: None,
            frame: None,
            detail: None,
            firmware: None,
            identity: None,
            snapshot_id: None,
        }
    }

    fn rows(from: &str, n: i64) -> Vec<SnapshotRow> {
        (0..n)
            .map(|s| {
                let mut signals = Signals::default();
                signals.sample.pack_v = Some(13.0);
                SnapshotRow {
                    ts: add_ms(ts(from), s * 1000),
                    state: McuState::Charging,
                    signals,
                }
            })
            .collect()
    }

    fn count(store: &Store, sql: &str) -> i64 {
        store.conn().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    fn start(at: &str, before: Vec<SnapshotRow>) -> SnapshotPart {
        SnapshotPart::Start {
            ts_start: add_ms(ts(at), -60_000),
            ts_end: add_ms(ts(at), 60_000),
            rows: before,
        }
    }

    #[test]
    fn a_fault_with_a_snapshot_is_one_linked_write() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .apply(&Write::Raise {
                event: fault(5, "2026-11-24T10:01:00Z"),
                snapshot: start("2026-11-24T10:01:00Z", rows("2026-11-24T10:00:00Z", 61)),
            })
            .unwrap();
        let (id, ts_start, ts_end, complete): (i64, String, String, i64) = store
            .conn()
            .query_row(
                "SELECT id, ts_start, ts_end, complete FROM snapshot",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(ts_start, "2026-11-24T10:00:00.000Z");
        assert_eq!(ts_end, "2026-11-24T10:02:00.000Z");
        assert_eq!(complete, 0);
        assert_eq!(count(&store, "SELECT count(*) FROM snapshot_sample"), 61);
        let state: String = store
            .conn()
            .query_row("SELECT state FROM snapshot_sample LIMIT 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "CHARGING");
        let linked: i64 = store
            .conn()
            .query_row("SELECT snapshot_id FROM event", [], |r| r.get(0))
            .unwrap();
        assert_eq!(linked, id);
    }

    #[test]
    fn an_overlapping_fault_extends_the_open_snapshot() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .apply(&Write::Raise {
                event: fault(5, "2026-11-24T10:01:00Z"),
                snapshot: start("2026-11-24T10:01:00Z", rows("2026-11-24T10:00:00Z", 61)),
            })
            .unwrap();
        store
            .apply(&Write::SnapshotRows(rows("2026-11-24T10:01:01Z", 10)))
            .unwrap();
        store
            .apply(&Write::Raise {
                event: fault(1, "2026-11-24T10:01:30Z"),
                snapshot: SnapshotPart::Extend {
                    ts_end: ts("2026-11-24T10:02:30Z"),
                },
            })
            .unwrap();
        assert_eq!(count(&store, "SELECT count(*) FROM snapshot"), 1);
        assert_eq!(
            count(&store, "SELECT count(DISTINCT snapshot_id) FROM event"),
            1
        );
        let end: String = store
            .conn()
            .query_row("SELECT ts_end FROM snapshot", [], |r| r.get(0))
            .unwrap();
        assert_eq!(end, "2026-11-24T10:02:30.000Z");
        assert_eq!(count(&store, "SELECT count(*) FROM snapshot_sample"), 71);
    }

    #[test]
    fn rows_go_to_the_open_snapshot_until_it_is_complete() {
        let mut store = Store::open_in_memory().unwrap();
        // Rows without an open snapshot are dropped.
        store
            .apply(&Write::SnapshotRows(rows("2026-11-24T09:00:00Z", 5)))
            .unwrap();
        assert_eq!(count(&store, "SELECT count(*) FROM snapshot_sample"), 0);
        store
            .apply(&Write::Raise {
                event: fault(5, "2026-11-24T10:01:00Z"),
                snapshot: start("2026-11-24T10:01:00Z", Vec::new()),
            })
            .unwrap();
        let after = rows("2026-11-24T10:01:01Z", 10);
        store.apply(&Write::SnapshotRows(after.clone())).unwrap();
        // The same rows again (a clock step) are not doubled.
        store.apply(&Write::SnapshotRows(after)).unwrap();
        store.apply(&Write::SnapshotComplete).unwrap();
        assert_eq!(count(&store, "SELECT count(*) FROM snapshot_sample"), 10);
        assert_eq!(count(&store, "SELECT complete FROM snapshot"), 1);
        // Nothing is open any more.
        store
            .apply(&Write::SnapshotRows(rows("2026-11-24T10:05:00Z", 3)))
            .unwrap();
        assert_eq!(count(&store, "SELECT count(*) FROM snapshot_sample"), 10);
    }

    #[test]
    fn a_new_snapshot_completes_one_left_open() {
        let mut store = Store::open_in_memory().unwrap();
        for at in ["2026-11-24T10:00:00Z", "2026-11-24T11:00:00Z"] {
            store
                .apply(&Write::Raise {
                    event: fault(5, at),
                    snapshot: start(at, Vec::new()),
                })
                .unwrap();
        }
        assert_eq!(
            count(&store, "SELECT count(*) FROM snapshot WHERE complete = 0"),
            1
        );
        assert_eq!(count(&store, "SELECT count(*) FROM snapshot"), 2);
    }

    #[test]
    fn a_clear_closes_the_open_event_of_that_code_only() {
        let mut store = Store::open_in_memory().unwrap();
        for code in [5, 13] {
            store
                .apply(&Write::Raise {
                    event: fault(code, "2026-11-24T10:00:00Z"),
                    snapshot: SnapshotPart::None,
                })
                .unwrap();
        }
        store
            .apply(&Write::Clear {
                code: 5,
                ts: ts("2026-11-24T10:00:30Z"),
                detail: Some("cleared while Pi/link was down".into()),
            })
            .unwrap();
        let (cleared, detail): (String, String) = store
            .conn()
            .query_row(
                "SELECT cleared_ts, detail FROM event WHERE code = 5",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(cleared, "2026-11-24T10:00:30.000Z");
        assert_eq!(detail, "cleared while Pi/link was down");
        assert_eq!(
            store.open_fault_codes().unwrap(),
            FaultBits(FaultCode::new(13).unwrap().mask())
        );
        // A second clear of the same code finds nothing open.
        store
            .apply(&Write::Clear {
                code: 5,
                ts: ts("2026-11-24T10:05:00Z"),
                detail: None,
            })
            .unwrap();
        let cleared: String = store
            .conn()
            .query_row("SELECT cleared_ts FROM event WHERE code = 5", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(cleared, "2026-11-24T10:00:30.000Z");
    }

    #[test]
    fn a_clear_detail_is_added_to_the_raise_detail() {
        let mut store = Store::open_in_memory().unwrap();
        let mut e = fault(5, "2026-11-24T10:00:00Z");
        e.detail = Some("active at reconnect, start time unknown".into());
        store
            .apply(&Write::Raise {
                event: e,
                snapshot: SnapshotPart::None,
            })
            .unwrap();
        store
            .apply(&Write::Clear {
                code: 5,
                ts: ts("2026-11-24T11:00:00Z"),
                detail: Some("cleared while Pi/link was down".into()),
            })
            .unwrap();
        let detail: String = store
            .conn()
            .query_row("SELECT detail FROM event", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            detail,
            "active at reconnect, start time unknown; cleared while Pi/link was down"
        );
    }

    #[test]
    fn open_codes_leave_out_system_events_and_cleared_faults() {
        let mut store = Store::open_in_memory().unwrap();
        assert_eq!(store.open_fault_codes().unwrap(), FaultBits(0));
        store
            .apply(&Write::System(NewEvent::system(
                ts("2026-11-24T10:00:00Z"),
                SystemType::StateChange,
                None,
            )))
            .unwrap();
        for code in [1, 8, 25] {
            store
                .apply(&Write::Raise {
                    event: fault(code, "2026-11-24T10:00:00Z"),
                    snapshot: SnapshotPart::None,
                })
                .unwrap();
        }
        store
            .apply(&Write::Clear {
                code: 1,
                ts: ts("2026-11-24T10:00:01Z"),
                detail: None,
            })
            .unwrap();
        let expected = [8, 25]
            .iter()
            .map(|&c| FaultCode::new(c).unwrap().mask())
            .fold(0, |a, m| a | m);
        assert_eq!(store.open_fault_codes().unwrap(), FaultBits(expected));
    }

    #[test]
    fn stale_snapshots_are_completed_at_start_up() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .apply(&Write::Raise {
                event: fault(5, "2026-11-24T10:00:00Z"),
                snapshot: start("2026-11-24T10:00:00Z", Vec::new()),
            })
            .unwrap();
        assert_eq!(store.complete_stale_snapshots().unwrap(), 1);
        assert_eq!(store.complete_stale_snapshots().unwrap(), 0);
        assert_eq!(count(&store, "SELECT complete FROM snapshot"), 1);
    }
}

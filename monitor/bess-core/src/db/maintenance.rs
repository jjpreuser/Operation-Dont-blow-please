//! The `maintenance` task's database work: the daily job and the size guard
//! (docs/design/data-lifecycle.md §4, docs/design/database.md §4; S10.4,
//! S10.7, S10.8; DD-23, DD-47).
//!
//! DD-47 order after local midnight: close the 23:50 interval (the
//! aggregator, before calling this), daily record, compaction, size guard,
//! backup. The backup (S10.14, DD-48) is not written yet.

use super::config::capacity_ah;
use super::events::{insert_event_on, NewEvent, SystemType};
use super::{Result, Store};
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::Timestamp;
use rusqlite::OptionalExtension;

/// What one run of the daily job did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DailyReport {
    /// Days that got their daily record, oldest first (more than one after
    /// the Pi was off at midnight).
    pub days_written: Vec<Date>,
    /// 10-min rows deleted by compaction.
    pub rows_compacted: usize,
    /// Snapshots deleted by the size guard.
    pub snapshots_deleted: Vec<i64>,
}

impl Store {
    /// The daily job after local midnight, also run at start-up and when the
    /// clock becomes valid to catch up missed days (DD-47). `capacity_ah` is
    /// `CAP_AH` of the installed CONFIG; without one the newest accepted
    /// `mcu_config` row is used, else `equiv_cycles` stays NULL (D3 §4).
    /// `soft_limit_bytes` is `DB_SIZE_SOFT_LIMIT` (proposal 150 MB).
    ///
    /// # Errors
    ///
    /// SQLite errors. Days written before the error stay written; the next
    /// run picks up the rest.
    pub fn run_daily_job(
        &mut self,
        now: Timestamp,
        tz: &TimeZone,
        capacity_ah_now: Option<f64>,
        soft_limit_bytes: u64,
    ) -> Result<DailyReport> {
        let capacity = match capacity_ah_now {
            Some(c) => Some(c),
            None => self.latest_accepted_config()?.as_ref().map(capacity_ah),
        };
        let mut report = DailyReport::default();
        for day in self.days_missing_daily(now, tz)? {
            if self.write_daily(day, tz, capacity)? {
                report.days_written.push(day);
            }
        }
        report.rows_compacted = self.compact_10min(now)?;
        report.snapshots_deleted = self.size_guard(now, soft_limit_bytes)?;
        Ok(report)
    }

    /// The size guard (DD-23, S10.8): while the bytes in use are over
    /// `soft_limit_bytes`, deletes the oldest complete snapshot with its
    /// samples and logs `SNAPSHOT_DELETED`. Events stay; their `snapshot_id`
    /// becomes NULL. A snapshot still being written is never deleted. Space
    /// is counted in whole free pages, so it can delete a little more than
    /// the bare minimum. Runs daily and after each completed snapshot (D3 §4).
    /// Returns the deleted snapshot ids, oldest first.
    ///
    /// # Errors
    ///
    /// SQLite errors.
    pub fn size_guard(&mut self, now: Timestamp, soft_limit_bytes: u64) -> Result<Vec<i64>> {
        let mut deleted = Vec::new();
        loop {
            let used = self.used_bytes()?;
            if used <= soft_limit_bytes {
                break;
            }
            let oldest: Option<(i64, String, String)> = self
                .conn
                .query_row(
                    "SELECT id, ts_start, ts_end FROM snapshot WHERE complete = 1
                     ORDER BY ts_start, id LIMIT 1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            let Some((id, start, end)) = oldest else {
                break; // nothing left to delete; the events are never touched
            };
            // The deletion and its event together, or neither.
            let tx = self.conn.transaction()?;
            tx.execute("DELETE FROM snapshot WHERE id = ?1", [id])?;
            insert_event_on(
                &tx,
                &NewEvent::system(
                    now,
                    SystemType::SnapshotDeleted,
                    Some(format!(
                        "snapshot {id} ({start} to {end}) deleted: database {used} bytes, \
                         soft limit {soft_limit_bytes}"
                    )),
                ),
            )?;
            tx.commit()?;
            deleted.push(id);
        }
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregate::{AggRow, Stat};
    use crate::db::events::{EventKind, EventSeverity, EventSource};
    use crate::test_util::{amsterdam, close, ts};
    use crate::wallclock::{add_ms, utc_text};
    use jiff::civil::date;
    use protocol::config::Config;
    use protocol::messages::{AckStatus, CONFIG_LEN};

    /// A snapshot of 121 one-second rows (±60 s), written directly: the
    /// snapshot writer comes with the event detector.
    fn add_snapshot(store: &Store, start: &str, complete: bool) -> i64 {
        let conn = store.conn();
        conn.execute(
            "INSERT INTO snapshot (ts_start, ts_end, complete) VALUES (?1, ?1, ?2)",
            (start, i64::from(complete)),
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        let base = ts(start);
        for s in 0..121 {
            conn.execute(
                "INSERT INTO snapshot_sample (snapshot_id, ts, state, pack_v, current_a, power_w,
                     cg1_v, cg2_v, cg3_v, cg4_v, t_cell_c, t_terminal_c, t_air_out_c, soc_pct,
                     fan_duty_pct)
                 VALUES (?1, ?2, 'FAULT', 13.1, 2.0, 26.2, 3.3, 3.3, 3.3, 3.3, 30.0, 31.0, 32.0,
                         55.0, 100.0)",
                (id, utc_text(add_ms(base, s * 1000))),
            )
            .unwrap();
        }
        id
    }

    fn fault_event(store: &mut Store, snapshot_id: i64) {
        store
            .insert_event(&NewEvent {
                ts: ts("2026-11-01T00:00:00Z"),
                kind: EventKind::Fault,
                code: Some(5),
                severity: EventSeverity::Protection,
                source: EventSource::Mcu,
                system_type: None,
                frame: None,
                detail: None,
                firmware: None,
                identity: None,
                snapshot_id: Some(snapshot_id),
            })
            .unwrap();
    }

    fn count(store: &Store, sql: &str) -> i64 {
        store.conn().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn size_guard_deletes_the_oldest_snapshots_until_under_the_limit() {
        let mut store = Store::open_in_memory().unwrap();
        let mut ids = Vec::new();
        for day in 1..=20 {
            let id = add_snapshot(&store, &format!("2026-11-{day:02}T12:00:00.000Z"), true);
            fault_event(&mut store, id);
            ids.push(id);
        }
        let incomplete = add_snapshot(&store, "2026-11-21T12:00:00.000Z", false);
        let limit = store.used_bytes().unwrap() - 60_000;

        let deleted = store.size_guard(ts("2026-11-24T00:00:00Z"), limit).unwrap();
        assert!(!deleted.is_empty());
        assert!(store.used_bytes().unwrap() <= limit);
        // Oldest first, and only a few: not everything.
        assert_eq!(deleted, ids[..deleted.len()].to_vec());
        assert!(deleted.len() < 10, "deleted {} snapshots", deleted.len());

        // Samples went with them; the snapshot still being written stays.
        let first = deleted[0];
        assert_eq!(
            count(
                &store,
                &format!("SELECT count(*) FROM snapshot_sample WHERE snapshot_id = {first}")
            ),
            0
        );
        assert_eq!(
            count(
                &store,
                &format!("SELECT count(*) FROM snapshot WHERE id = {incomplete}")
            ),
            1
        );
        // Every fault event is kept; the deleted ones lost their snapshot link.
        let n = i64::try_from(deleted.len()).unwrap();
        assert_eq!(
            count(&store, "SELECT count(*) FROM event WHERE kind = 'FAULT'"),
            20
        );
        assert_eq!(
            count(
                &store,
                "SELECT count(*) FROM event WHERE kind = 'FAULT' AND snapshot_id IS NULL"
            ),
            n
        );
        // One SNAPSHOT_DELETED event per deletion.
        assert_eq!(
            count(
                &store,
                "SELECT count(*) FROM event WHERE system_type = 'SNAPSHOT_DELETED'"
            ),
            n
        );
        let detail: String = store
            .conn()
            .query_row(
                "SELECT detail FROM event WHERE system_type = 'SNAPSHOT_DELETED' ORDER BY id LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(detail.starts_with(&format!("snapshot {first} (2026-11-01T12:00:00.000Z")));
        assert!(store.integrity_ok().unwrap());
    }

    #[test]
    fn size_guard_does_nothing_under_the_limit() {
        let mut store = Store::open_in_memory().unwrap();
        add_snapshot(&store, "2026-11-01T12:00:00.000Z", true);
        let used = store.used_bytes().unwrap();
        assert!(store
            .size_guard(ts("2026-11-24T00:00:00Z"), used)
            .unwrap()
            .is_empty());
        assert_eq!(count(&store, "SELECT count(*) FROM snapshot"), 1);
    }

    #[test]
    fn size_guard_never_deletes_events() {
        let mut store = Store::open_in_memory().unwrap();
        for _ in 0..50 {
            store
                .insert_event(&NewEvent::system(
                    ts("2026-11-01T00:00:00Z"),
                    SystemType::StateChange,
                    None,
                ))
                .unwrap();
        }
        // Over any limit, but no snapshot left to delete: events stay.
        assert!(store
            .size_guard(ts("2026-11-24T00:00:00Z"), 0)
            .unwrap()
            .is_empty());
        assert_eq!(count(&store, "SELECT count(*) FROM event"), 50);
    }

    fn ten_min(at: &str, ah_dis: f64) -> AggRow {
        let mut stats = [Stat::default(); 15];
        stats[0] = Stat {
            min: Some(13.0),
            avg: Some(13.1),
            max: Some(13.2),
        };
        AggRow {
            ts: ts(at),
            sample_count: 600,
            stats,
            ah_chg: 0.0,
            ah_dis,
            wh_chg: 0.0,
            wh_dis: ah_dis * 13.0,
        }
    }

    #[test]
    fn daily_job_catches_up_missed_days_then_compacts() {
        let tz = amsterdam();
        let mut store = Store::open_in_memory().unwrap();
        // A 12 Ah CONFIG was accepted once; none is installed now.
        let mut config = Config::from_bytes(&[0; CONFIG_LEN]);
        config.capacity_mah = 12_000;
        store
            .insert_mcu_config(ts("2026-01-01T00:00:00Z"), &config, AckStatus::Accepted)
            .unwrap();
        // Rows on 1 January (old) and on 20 and 21 January; the Pi was off
        // at those midnights.
        store
            .insert_10min(&ten_min("2026-01-01T08:00:00Z", 1.0))
            .unwrap();
        store
            .insert_10min(&ten_min("2026-01-20T10:00:00Z", 3.0))
            .unwrap();
        store
            .insert_10min(&ten_min("2026-01-21T10:00:00Z", 6.0))
            .unwrap();
        // Today's row is left alone.
        store
            .insert_10min(&ten_min("2026-01-31T08:00:00Z", 1.0))
            .unwrap();

        let now = ts("2026-01-31T09:00:00Z");
        let report = store.run_daily_job(now, &tz, None, u64::MAX).unwrap();
        assert_eq!(
            report.days_written,
            vec![date(2026, 1, 1), date(2026, 1, 20), date(2026, 1, 21)]
        );
        // 1 January is over 30 days old and now has a daily record.
        assert_eq!(report.rows_compacted, 1);
        assert!(report.snapshots_deleted.is_empty());
        let cycles: f64 = store
            .conn()
            .query_row(
                "SELECT equiv_cycles FROM agg_daily WHERE day = '2026-01-21'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(close(cycles, 6.0 / 12.0));
        assert_eq!(count(&store, "SELECT count(*) FROM agg_10min"), 3);

        // A second run has nothing left to do.
        let again = store.run_daily_job(now, &tz, None, u64::MAX).unwrap();
        assert_eq!(again, DailyReport::default());
    }

    #[test]
    fn daily_job_prefers_the_installed_config_capacity() {
        let tz = amsterdam();
        let mut store = Store::open_in_memory().unwrap();
        store
            .insert_10min(&ten_min("2026-01-20T10:00:00Z", 5.0))
            .unwrap();
        store
            .run_daily_job(ts("2026-01-21T00:00:05Z"), &tz, Some(10.0), u64::MAX)
            .unwrap();
        let cycles: f64 = store
            .conn()
            .query_row("SELECT equiv_cycles FROM agg_daily", [], |r| r.get(0))
            .unwrap();
        assert!(close(cycles, 0.5));
    }

    #[test]
    fn daily_job_without_any_capacity_leaves_cycles_null() {
        let tz = amsterdam();
        let mut store = Store::open_in_memory().unwrap();
        store
            .insert_10min(&ten_min("2026-01-20T10:00:00Z", 5.0))
            .unwrap();
        store
            .run_daily_job(ts("2026-01-21T00:00:05Z"), &tz, None, u64::MAX)
            .unwrap();
        let cycles: Option<f64> = store
            .conn()
            .query_row("SELECT equiv_cycles FROM agg_daily", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cycles, None);
    }
}

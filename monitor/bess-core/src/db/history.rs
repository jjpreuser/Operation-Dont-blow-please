//! Reading `agg_10min`, `agg_daily` and fault snapshots back for graphs and
//! CSV export (docs/design/data-lifecycle.md §6; S9.6–S9.8, S10.11; DD-10,
//! DD-49). The tier choice and the ring buffer are in
//! [`history`](crate::history), the CSV layout in [`csv`](crate::csv).
//!
//! These run on a read-only connection ([`Store::open_read_only`]), so a
//! large export never blocks `db_writer` (WAL). The CSV functions stream:
//! one row is read and written at a time.

use super::{Result, Store, StoreError};
use crate::aggregate::{INTERVAL_MS, Stat};
use crate::csv;
use crate::history::{AggTable, Point, Range, SampleRow, SignalSet, Totals};
use crate::signals::{SIGNAL_COLUMNS, SIGNAL_COUNT};
use crate::wallclock::{add_ms, parse_utc_text, utc_text};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use rusqlite::{OptionalExtension, Row};
use std::fmt::Write as _;
use std::sync::OnceLock;

/// The rows of `table` that overlap `[?1, ?2)`, oldest first: a 10-min row
/// whose interval started before the range but ends inside it is included,
/// so a graph does not lose its first point. A 10-min row covers
/// `[ts, ts + 10 min)`, so it overlaps when `ts > from − 10 min`; a daily
/// row covers `[ts_start, ts_end)` (DD-09).
fn select_sql(table: AggTable) -> &'static str {
    fn build(time: &str, from: &str, rest: &str) -> String {
        let mut columns = format!("{time}, sample_count");
        for s in SIGNAL_COLUMNS {
            let _ = write!(columns, ", {s}_min, {s}_avg, {s}_max");
        }
        columns.push_str(", ah_chg, ah_dis, wh_chg, wh_dis");
        format!("SELECT {columns}{rest} {from}")
    }
    static TEN_MIN: OnceLock<String> = OnceLock::new();
    static DAILY: OnceLock<String> = OnceLock::new();
    match table {
        AggTable::TenMin => TEN_MIN.get_or_init(|| {
            build(
                "ts",
                "FROM agg_10min WHERE ts > ?1 AND ts < ?2 ORDER BY ts",
                "",
            )
        }),
        AggTable::Daily => DAILY.get_or_init(|| {
            build(
                "ts_start",
                "FROM agg_daily WHERE ts_end > ?1 AND ts_start < ?2 ORDER BY ts_start",
                ", equiv_cycles",
            )
        }),
    }
}

fn snapshot_sql() -> &'static str {
    static SQL: OnceLock<String> = OnceLock::new();
    SQL.get_or_init(|| {
        let mut columns = String::from("ts, state");
        for s in SIGNAL_COLUMNS {
            let _ = write!(columns, ", {s}");
        }
        format!("SELECT {columns} FROM snapshot_sample WHERE snapshot_id = ?1 ORDER BY ts")
    })
}

/// Row counts of the tables graphs and exports read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StoredCounts {
    pub ten_min: u64,
    pub daily: u64,
    pub snapshots: u64,
    pub events: u64,
}

fn timestamp(row: &Row, column: usize) -> Result<Timestamp> {
    let text: String = row.get(column)?;
    parse_utc_text(&text).ok_or(StoreError::BadTimestamp(text))
}

fn point_from_row(row: &Row, table: AggTable) -> Result<Point> {
    let mut stats = [Stat::default(); SIGNAL_COUNT];
    for (i, stat) in stats.iter_mut().enumerate() {
        let c = 2 + 3 * i;
        *stat = Stat {
            min: row.get(c)?,
            avg: row.get(c + 1)?,
            max: row.get(c + 2)?,
        };
    }
    let t = 2 + 3 * SIGNAL_COUNT;
    Ok(Point {
        ts: timestamp(row, 0)?,
        sample_count: row.get(1)?,
        stats,
        totals: Some(Totals {
            ah_chg: row.get(t)?,
            ah_dis: row.get(t + 1)?,
            wh_chg: row.get(t + 2)?,
            wh_dis: row.get(t + 3)?,
            equiv_cycles: match table {
                AggTable::TenMin => None,
                AggTable::Daily => row.get(t + 4)?,
            },
        }),
    })
}

fn sample_from_row(row: &Row) -> Result<SampleRow> {
    let mut values = [None; SIGNAL_COUNT];
    for (i, v) in values.iter_mut().enumerate() {
        *v = row.get(2 + i)?;
    }
    Ok(SampleRow {
        ts: timestamp(row, 0)?,
        state: row.get(1)?,
        values,
    })
}

impl Store {
    /// Calls `each` for every row of `table` that overlaps `range`, oldest
    /// first.
    fn for_each_point(
        &self,
        table: AggTable,
        range: &Range,
        mut each: impl FnMut(Point) -> Result<()>,
    ) -> Result<()> {
        let from = match table {
            AggTable::TenMin => add_ms(range.from, -INTERVAL_MS),
            AggTable::Daily => range.from,
        };
        let mut stmt = self.conn.prepare_cached(select_sql(table))?;
        let mut rows = stmt.query((utc_text(from), utc_text(range.to)))?;
        while let Some(row) = rows.next()? {
            each(point_from_row(row, table)?)?;
        }
        Ok(())
    }

    /// The points of `table` that overlap `range`, oldest first, for a
    /// graph (S9.6–S9.8). Intervals without samples have no row, so a gap
    /// between two points is a gap in the data (DD-10).
    ///
    /// # Errors
    ///
    /// SQLite errors, and [`StoreError::BadTimestamp`] for a row whose time
    /// does not read back.
    pub fn points(&self, table: AggTable, range: &Range) -> Result<Vec<Point>> {
        let mut points = Vec::new();
        self.for_each_point(table, range, |p| {
            points.push(p);
            Ok(())
        })?;
        Ok(points)
    }

    /// Streams the CSV export of `table` over `range` (S10.11, DD-49,
    /// DD-62) into `out` and returns the number of data rows. `out` should
    /// be buffered.
    ///
    /// # Errors
    ///
    /// As [`Store::points`], and [`StoreError::Io`] when writing to `out`
    /// fails (for example the download was cancelled).
    pub fn write_csv(
        &self,
        table: AggTable,
        range: &Range,
        tz: &TimeZone,
        signals: &SignalSet,
        out: &mut impl std::io::Write,
    ) -> Result<usize> {
        out.write_all(csv::aggregate_header(table, signals).as_bytes())?;
        let mut count = 0;
        self.for_each_point(table, range, |p| {
            out.write_all(csv::aggregate_line(&p, table, tz, signals).as_bytes())?;
            count += 1;
            Ok(())
        })?;
        Ok(count)
    }

    /// How many rows the read-back tables hold: for a status line (the
    /// System info page, S9.11, and the display preview).
    ///
    /// # Errors
    ///
    /// SQLite errors.
    pub fn stored_counts(&self) -> Result<StoredCounts> {
        let count = |sql: &str| -> Result<u64> {
            let n: i64 = self.conn.query_row(sql, [], |r| r.get(0))?;
            Ok(u64::try_from(n).unwrap_or(0))
        };
        Ok(StoredCounts {
            ten_min: count("SELECT count(*) FROM agg_10min")?,
            daily: count("SELECT count(*) FROM agg_daily")?,
            snapshots: count("SELECT count(*) FROM snapshot")?,
            events: count("SELECT count(*) FROM event")?,
        })
    }

    /// The newest fault snapshot: its id and start time.
    ///
    /// # Errors
    ///
    /// As [`Store::points`].
    pub fn latest_snapshot(&self) -> Result<Option<(i64, Timestamp)>> {
        let found: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT id, ts_start FROM snapshot ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        found
            .map(|(id, text)| {
                parse_utc_text(&text)
                    .map(|ts| (id, ts))
                    .ok_or(StoreError::BadTimestamp(text))
            })
            .transpose()
    }

    /// Whether a snapshot with this id exists.
    fn snapshot_exists(&self, snapshot_id: i64) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM snapshot WHERE id = ?1",
                [snapshot_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// The 1 s rows of a fault snapshot, oldest first (D3 §6: exported per
    /// fault from the fault log); `None` when there is no such snapshot
    /// (never made, or deleted by the size guard).
    ///
    /// # Errors
    ///
    /// As [`Store::points`].
    pub fn snapshot_rows(&self, snapshot_id: i64) -> Result<Option<Vec<SampleRow>>> {
        if !self.snapshot_exists(snapshot_id)? {
            return Ok(None);
        }
        let mut rows = Vec::new();
        self.for_each_snapshot_row(snapshot_id, |row| {
            rows.push(row);
            Ok(())
        })?;
        Ok(Some(rows))
    }

    /// Streams the CSV export of a fault snapshot into `out` and returns the
    /// number of data rows; `None`, with nothing written, when there is no
    /// such snapshot.
    ///
    /// # Errors
    ///
    /// As [`Store::write_csv`].
    pub fn write_snapshot_csv(
        &self,
        snapshot_id: i64,
        tz: &TimeZone,
        signals: &SignalSet,
        out: &mut impl std::io::Write,
    ) -> Result<Option<usize>> {
        if !self.snapshot_exists(snapshot_id)? {
            return Ok(None);
        }
        out.write_all(csv::sample_header(signals).as_bytes())?;
        let mut count = 0;
        self.for_each_snapshot_row(snapshot_id, |row| {
            out.write_all(csv::sample_line(&row, tz, signals).as_bytes())?;
            count += 1;
            Ok(())
        })?;
        Ok(Some(count))
    }

    /// Calls `each` for every row of the snapshot, oldest first.
    fn for_each_snapshot_row(
        &self,
        snapshot_id: i64,
        mut each: impl FnMut(SampleRow) -> Result<()>,
    ) -> Result<()> {
        let mut stmt = self.conn.prepare_cached(snapshot_sql())?;
        let mut rows = stmt.query([snapshot_id])?;
        while let Some(row) = rows.next()? {
            each(sample_from_row(row)?)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregate::AggRow;
    use crate::history::{Tier, choose_tier};
    use crate::test_util::{amsterdam, close, ts};
    use jiff::civil::date;

    fn range(from: &str, to: &str) -> Range {
        Range {
            from: ts(from),
            to: ts(to),
        }
    }

    /// A 10-min row with `pack_v` min/avg/max, the cell temperature never
    /// valid, and some charge.
    fn row(at: &str, count: u32, pack: (f64, f64, f64)) -> AggRow {
        let mut stats = [Stat::default(); SIGNAL_COUNT];
        stats[0] = Stat {
            min: Some(pack.0),
            avg: Some(pack.1),
            max: Some(pack.2),
        };
        stats[1] = Stat {
            min: Some(-2.0),
            avg: Some(1.5),
            max: Some(8.0),
        };
        AggRow {
            ts: ts(at),
            sample_count: count,
            stats,
            ah_chg: 0.25,
            ah_dis: 0.125,
            wh_chg: 3.3,
            wh_dis: 1.6,
        }
    }

    fn store_with_10min(times: &[&str]) -> Store {
        let mut store = Store::open_in_memory().unwrap();
        for (i, at) in (0_u32..).zip(times) {
            let v = 13.0 + f64::from(i) / 10.0;
            store
                .insert_10min(&row(at, 600, (v - 0.1, v, v + 0.1)))
                .unwrap();
        }
        store
    }

    #[test]
    fn ten_min_points_overlap_the_range_and_come_oldest_first() {
        let store = store_with_10min(&[
            "2026-11-25T09:50:00Z",
            "2026-11-25T10:00:00Z",
            "2026-11-25T10:10:00Z",
            "2026-11-25T10:20:00Z",
            "2026-11-25T10:30:00Z",
        ]);
        // 10:05 to 10:30: the 10:00 row ends inside the range, the 10:30
        // row starts at the end, which is excluded; 09:50 ends at 10:00.
        let points = store
            .points(
                AggTable::TenMin,
                &range("2026-11-25T10:05:00Z", "2026-11-25T10:30:00Z"),
            )
            .unwrap();
        let times: Vec<Timestamp> = points.iter().map(|p| p.ts).collect();
        assert_eq!(
            times,
            vec![
                ts("2026-11-25T10:00:00Z"),
                ts("2026-11-25T10:10:00Z"),
                ts("2026-11-25T10:20:00Z")
            ]
        );
        // A range starting on a boundary does not take the row before it.
        let points = store
            .points(
                AggTable::TenMin,
                &range("2026-11-25T10:00:00Z", "2026-11-25T10:10:00Z"),
            )
            .unwrap();
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].ts, ts("2026-11-25T10:00:00Z"));
    }

    #[test]
    fn a_ten_min_point_reads_back_what_was_stored() {
        let store = store_with_10min(&["2026-11-25T10:00:00Z"]);
        let points = store
            .points(
                AggTable::TenMin,
                &range("2026-11-25T00:00:00Z", "2026-11-26T00:00:00Z"),
            )
            .unwrap();
        let p = &points[0];
        assert_eq!(p.sample_count, 600);
        assert!(close(p.stats[0].min.unwrap(), 12.9));
        assert!(close(p.stats[0].avg.unwrap(), 13.0));
        assert!(close(p.stats[0].max.unwrap(), 13.1));
        assert_eq!(p.stats[7], Stat::default(), "NULL stays None (DD-41)");
        let totals = p.totals.unwrap();
        assert!(close(totals.ah_chg, 0.25));
        assert!(close(totals.wh_dis, 1.6));
        assert_eq!(totals.equiv_cycles, None);
    }

    #[test]
    fn a_gap_in_the_data_is_a_gap_between_points() {
        // No samples 10:10 to 10:30: no rows there (DD-10).
        let store = store_with_10min(&["2026-11-25T10:00:00Z", "2026-11-25T10:30:00Z"]);
        let points = store
            .points(
                AggTable::TenMin,
                &range("2026-11-25T10:00:00Z", "2026-11-25T11:00:00Z"),
            )
            .unwrap();
        assert_eq!(points.len(), 2);
        assert_eq!(points[1].ts, ts("2026-11-25T10:30:00Z"));
    }

    /// 10-min rows on 14, 15 and 16 January and on the DST day 29 March,
    /// with their daily records.
    fn store_with_days() -> Store {
        let mut store = store_with_10min(&[
            "2026-01-14T12:00:00Z",
            "2026-01-15T08:00:00Z",
            "2026-01-15T20:00:00Z",
            "2026-01-16T12:00:00Z",
            "2026-03-29T12:00:00Z",
        ]);
        for day in [
            date(2026, 1, 14),
            date(2026, 1, 15),
            date(2026, 1, 16),
            date(2026, 3, 29),
        ] {
            assert!(store.write_daily(day, &amsterdam(), Some(6.0)).unwrap());
        }
        store
    }

    #[test]
    fn daily_points_overlap_the_range_by_local_day() {
        let store = store_with_days();
        // 15 January 12:00 local to 16 January 00:30 local: the 15th, and
        // the 16th, which starts inside the range.
        let points = store
            .points(
                AggTable::Daily,
                &range("2026-01-15T11:00:00Z", "2026-01-15T23:30:00Z"),
            )
            .unwrap();
        let times: Vec<Timestamp> = points.iter().map(|p| p.ts).collect();
        assert_eq!(
            times,
            vec![ts("2026-01-14T23:00:00Z"), ts("2026-01-15T23:00:00Z")],
            "keyed by 00:00 local"
        );
        let day15 = &points[0];
        assert_eq!(day15.sample_count, 1200);
        assert!(close(day15.stats[0].min.unwrap(), 13.0)); // 13.1 - 0.1
        assert!(close(day15.stats[0].avg.unwrap(), 13.15));
        let totals = day15.totals.unwrap();
        assert!(close(totals.ah_dis, 0.25));
        assert!(close(totals.equiv_cycles.unwrap(), 0.25 / 6.0));
    }

    #[test]
    fn a_dst_day_is_one_point_of_23_hours() {
        let store = store_with_days();
        let points = store
            .points(
                AggTable::Daily,
                &range("2026-03-29T00:00:00Z", "2026-03-29T01:00:00Z"),
            )
            .unwrap();
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].ts, ts("2026-03-28T23:00:00Z"));
    }

    /// Splits a CSV export into its header and rows of cells.
    fn parse(csv: &[u8]) -> (Vec<String>, Vec<Vec<String>>) {
        let text = std::str::from_utf8(csv).unwrap();
        assert!(text.ends_with("\r\n"));
        let mut lines = text.split("\r\n").filter(|l| !l.is_empty());
        let header = lines.next().unwrap().split(',').map(String::from).collect();
        let rows = lines
            .map(|l| l.split(',').map(String::from).collect())
            .collect();
        (header, rows)
    }

    fn cell<'a>(header: &[String], row: &'a [String], name: &str) -> &'a str {
        &row[header.iter().position(|h| h == name).unwrap()]
    }

    #[test]
    fn the_10min_csv_matches_the_database() {
        let store = store_with_10min(&[
            "2026-11-25T10:00:00Z",
            "2026-11-25T10:10:00Z",
            "2026-11-25T10:20:00Z",
        ]);
        let r = range("2026-11-25T00:00:00Z", "2026-11-26T00:00:00Z");
        let mut out = Vec::new();
        let count = store
            .write_csv(
                AggTable::TenMin,
                &r,
                &amsterdam(),
                &SignalSet::all(),
                &mut out,
            )
            .unwrap();
        assert_eq!(count, 3);
        let (header, rows) = parse(&out);
        assert_eq!(header.len(), 3 + 3 * SIGNAL_COUNT + 4);
        let points = store.points(AggTable::TenMin, &r).unwrap();
        for (row, p) in rows.iter().zip(&points) {
            assert_eq!(row.len(), header.len());
            assert_eq!(cell(&header, row, "timestamp_utc"), utc_text(p.ts));
            assert_eq!(
                cell(&header, row, "sample_count"),
                p.sample_count.to_string()
            );
            let avg: f64 = cell(&header, row, "pack_v_avg").parse().unwrap();
            assert!((avg - p.stats[0].avg.unwrap()).abs() < 0.000_5);
            assert_eq!(cell(&header, row, "current_a_min"), "-2.00");
            assert_eq!(cell(&header, row, "t_cell_c_max"), "", "NULL is empty");
            assert_eq!(cell(&header, row, "ah_chg"), "0.2500");
        }
        assert_eq!(cell(&header, &rows[0], "time_local"), "2026-11-25 11:00:00");
    }

    #[test]
    fn the_csv_has_only_the_chosen_signals() {
        let store = store_with_10min(&["2026-11-25T10:00:00Z"]);
        let mut out = Vec::new();
        store
            .write_csv(
                AggTable::TenMin,
                &range("2026-11-25T00:00:00Z", "2026-11-26T00:00:00Z"),
                &amsterdam(),
                &SignalSet::only(["soc_pct", "pack_v"]).unwrap(),
                &mut out,
            )
            .unwrap();
        let (header, rows) = parse(&out);
        assert_eq!(
            header,
            [
                "timestamp_utc",
                "time_local",
                "sample_count",
                "pack_v_min",
                "pack_v_avg",
                "pack_v_max",
                "soc_pct_min",
                "soc_pct_avg",
                "soc_pct_max",
                "ah_chg",
                "ah_dis",
                "wh_chg",
                "wh_dis"
            ]
        );
        assert_eq!(rows[0].len(), header.len());
    }

    #[test]
    fn a_range_across_the_30_day_boundary_exports_the_daily_records() {
        // S10.11 "a range across tiers": the tier that covers the start of
        // the range is used for all of it (DD-49).
        let store = store_with_days();
        let now = ts("2026-02-10T12:00:00Z");
        let r = range("2026-01-01T00:00:00Z", "2026-02-10T12:00:00Z");
        let tier = choose_tier(&r, now, None);
        assert_eq!(tier, Tier::Daily);
        let mut out = Vec::new();
        let count = store
            .write_csv(
                tier.table().unwrap(),
                &r,
                &amsterdam(),
                &SignalSet::all(),
                &mut out,
            )
            .unwrap();
        assert_eq!(count, 3);
        let (header, rows) = parse(&out);
        assert_eq!(header.last().unwrap(), "equiv_cycles");
        assert_eq!(cell(&header, &rows[1], "time_local"), "2026-01-15 00:00:00");
        assert_eq!(cell(&header, &rows[1], "equiv_cycles"), "0.0417");
    }

    #[test]
    fn an_empty_range_is_the_header_only() {
        let store = store_with_10min(&["2026-11-25T10:00:00Z"]);
        let mut out = Vec::new();
        let count = store
            .write_csv(
                AggTable::TenMin,
                &range("2026-11-24T00:00:00Z", "2026-11-24T01:00:00Z"),
                &amsterdam(),
                &SignalSet::all(),
                &mut out,
            )
            .unwrap();
        assert_eq!(count, 0);
        assert_eq!(parse(&out).1.len(), 0);
    }

    #[test]
    fn a_failing_writer_is_an_io_error() {
        struct Broken;
        impl std::io::Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let store = store_with_10min(&["2026-11-25T10:00:00Z"]);
        let result = store.write_csv(
            AggTable::TenMin,
            &range("2026-11-25T00:00:00Z", "2026-11-26T00:00:00Z"),
            &amsterdam(),
            &SignalSet::all(),
            &mut Broken,
        );
        assert!(matches!(result, Err(StoreError::Io(_))));
    }

    /// A snapshot with 3 rows, stored out of order, one value NULL.
    fn store_with_snapshot() -> (Store, i64) {
        let store = Store::open_in_memory().unwrap();
        let conn = store.conn();
        conn.execute(
            "INSERT INTO snapshot (ts_start, ts_end, complete)
             VALUES ('2026-11-25T10:00:00.000Z', '2026-11-25T10:00:02.000Z', 1)",
            [],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        for (at, state, pack) in [
            ("2026-11-25T10:00:02.000Z", "FAULT", Some(12.5)),
            ("2026-11-25T10:00:00.000Z", "DISCHARGING", Some(12.9)),
            ("2026-11-25T10:00:01.000Z", "FAULT", None),
        ] {
            conn.execute(
                "INSERT INTO snapshot_sample (snapshot_id, ts, state, pack_v, current_a)
                 VALUES (?1, ?2, ?3, ?4, -3.5)",
                (id, at, state, pack),
            )
            .unwrap();
        }
        (store, id)
    }

    #[test]
    fn snapshot_rows_come_back_in_time_order() {
        let (store, id) = store_with_snapshot();
        let rows = store.snapshot_rows(id).unwrap().unwrap();
        let states: Vec<&str> = rows.iter().map(|r| r.state.as_deref().unwrap()).collect();
        assert_eq!(states, ["DISCHARGING", "FAULT", "FAULT"]);
        assert_eq!(rows[0].ts, ts("2026-11-25T10:00:00Z"));
        assert_eq!(rows[0].values[0], Some(12.9));
        assert_eq!(rows[1].values[0], None);
        assert_eq!(rows[2].values[1], Some(-3.5));
    }

    #[test]
    fn the_snapshot_csv_has_one_line_per_second() {
        let (store, id) = store_with_snapshot();
        let mut out = Vec::new();
        let count = store
            .write_snapshot_csv(
                id,
                &amsterdam(),
                &SignalSet::only(["pack_v", "current_a"]).unwrap(),
                &mut out,
            )
            .unwrap();
        assert_eq!(count, Some(3));
        assert_eq!(
            std::str::from_utf8(&out).unwrap(),
            "timestamp_utc,time_local,state,pack_v,current_a\r\n\
             2026-11-25T10:00:00.000Z,2026-11-25 11:00:00,DISCHARGING,12.900,-3.50\r\n\
             2026-11-25T10:00:01.000Z,2026-11-25 11:00:01,FAULT,,-3.50\r\n\
             2026-11-25T10:00:02.000Z,2026-11-25 11:00:02,FAULT,12.500,-3.50\r\n"
        );
    }

    #[test]
    fn counts_and_the_latest_snapshot() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.stored_counts().unwrap(), StoredCounts::default());
        assert_eq!(store.latest_snapshot().unwrap(), None);
        let (store, id) = store_with_snapshot();
        assert_eq!(
            store.latest_snapshot().unwrap(),
            Some((id, ts("2026-11-25T10:00:00Z")))
        );
        let counts = store.stored_counts().unwrap();
        assert_eq!((counts.snapshots, counts.ten_min, counts.daily), (1, 0, 0));
        let store = store_with_days();
        let counts = store.stored_counts().unwrap();
        assert_eq!((counts.ten_min, counts.daily), (5, 4));
    }

    #[test]
    fn a_missing_snapshot_writes_nothing() {
        let (store, id) = store_with_snapshot();
        let mut out = Vec::new();
        assert_eq!(
            store
                .write_snapshot_csv(id + 1, &amsterdam(), &SignalSet::all(), &mut out)
                .unwrap(),
            None
        );
        assert!(out.is_empty());
        assert_eq!(store.snapshot_rows(id + 1).unwrap(), None);
    }

    #[test]
    fn an_export_reads_beside_the_writer_on_a_read_only_connection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bess.db");
        let mut writer = Store::open(&path).unwrap();
        writer
            .insert_10min(&row("2026-11-25T10:00:00Z", 600, (13.0, 13.1, 13.2)))
            .unwrap();
        let reader = Store::open_read_only(&path).unwrap();
        let r = range("2026-11-25T00:00:00Z", "2026-11-26T00:00:00Z");
        assert_eq!(reader.points(AggTable::TenMin, &r).unwrap().len(), 1);
        // The writer keeps writing while the reader is open (WAL).
        writer
            .insert_10min(&row("2026-11-25T10:10:00Z", 600, (13.0, 13.1, 13.2)))
            .unwrap();
        let mut out = Vec::new();
        let count = reader
            .write_csv(
                AggTable::TenMin,
                &r,
                &amsterdam(),
                &SignalSet::all(),
                &mut out,
            )
            .unwrap();
        assert_eq!(count, 2);
    }
}

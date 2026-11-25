//! `agg_10min` and `agg_daily` (docs/design/data-lifecycle.md §3–4; S10.3,
//! S10.4, S10.7; DD-09, DD-10, DD-45 to DD-47).

use super::{KEEP_10MIN_DAYS, Result, Store};
use crate::aggregate::{AggRow, Stat};
use crate::signals::SIGNAL_COLUMNS;
use crate::wallclock::{DAY_MS, add_ms, local_date, local_day_bounds, utc_text};
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan};
use rusqlite::types::Value;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::sync::OnceLock;

fn real(v: Option<f64>) -> Value {
    v.map_or(Value::Null, Value::Real)
}

/// `INSERT` of one `agg_10min` row. A second row for the same interval (the
/// core restarted inside it, or the clock stepped back) is merged into the
/// first: min of the mins, max of the maxes, averages weighted by
/// `sample_count` (DD-10), counts and Ah / Wh added (D3 §3).
fn insert_10min_sql() -> &'static str {
    static SQL: OnceLock<String> = OnceLock::new();
    SQL.get_or_init(|| {
        let mut columns = String::from("ts, sample_count");
        let mut update = String::from("sample_count = sample_count + excluded.sample_count");
        for s in SIGNAL_COLUMNS {
            let _ = write!(columns, ", {s}_min, {s}_avg, {s}_max");
            let _ = write!(
                update,
                ",\n  {s}_min = coalesce(min({s}_min, excluded.{s}_min), {s}_min, excluded.{s}_min)\
                 ,\n  {s}_max = coalesce(max({s}_max, excluded.{s}_max), {s}_max, excluded.{s}_max)\
                 ,\n  {s}_avg = (coalesce({s}_avg * sample_count, 0)\
                 + coalesce(excluded.{s}_avg * excluded.sample_count, 0))\
                 / nullif(({s}_avg IS NOT NULL) * sample_count\
                 + (excluded.{s}_avg IS NOT NULL) * excluded.sample_count, 0)"
            );
        }
        columns.push_str(", ah_chg, ah_dis, wh_chg, wh_dis");
        for c in ["ah_chg", "ah_dis", "wh_chg", "wh_dis"] {
            let _ = write!(update, ",\n  {c} = {c} + excluded.{c}");
        }
        let n = 2 + 3 * SIGNAL_COLUMNS.len() + 4;
        format!(
            "INSERT INTO agg_10min ({columns}) VALUES ({})\nON CONFLICT (ts) DO UPDATE SET\n  {update}",
            super::placeholders(n)
        )
    })
}

/// The daily record from the 10-min rows in `[?2, ?3)`: min of the mins, max
/// of the maxes, averages weighted by `sample_count` (DD-10), Ah / Wh summed,
/// `equiv_cycles` = `ah_dis` / `?4` (`CAP_AH`, NULL when unknown). Nothing is
/// inserted for a day without 10-min rows, or one that already has a daily
/// row.
fn insert_daily_sql() -> &'static str {
    static SQL: OnceLock<String> = OnceLock::new();
    SQL.get_or_init(|| {
        let mut columns = String::from("day, ts_start, ts_end, sample_count");
        let mut select = String::from("?1, ?2, ?3, sum(sample_count)");
        for s in SIGNAL_COLUMNS {
            let _ = write!(columns, ", {s}_min, {s}_avg, {s}_max");
            let _ = write!(
                select,
                ",\n  min({s}_min), sum({s}_avg * sample_count)\
                 / nullif(sum(({s}_avg IS NOT NULL) * sample_count), 0), max({s}_max)"
            );
        }
        format!(
            "INSERT INTO agg_daily ({columns}, ah_chg, ah_dis, wh_chg, wh_dis, equiv_cycles)\n\
             SELECT {select},\n  sum(ah_chg), sum(ah_dis), sum(wh_chg), sum(wh_dis), sum(ah_dis) / ?4\n\
             FROM agg_10min WHERE ts >= ?2 AND ts < ?3\n\
             HAVING count(*) > 0\n\
             ON CONFLICT (day) DO NOTHING"
        )
    })
}

impl Store {
    /// Writes one closed 10-min interval, merging it into an existing row
    /// for the same interval.
    ///
    /// # Errors
    ///
    /// SQLite errors.
    pub fn insert_10min(&mut self, row: &AggRow) -> Result<()> {
        let mut params = Vec::with_capacity(2 + 3 * SIGNAL_COLUMNS.len() + 4);
        params.push(Value::Text(utc_text(row.ts)));
        params.push(Value::Integer(i64::from(row.sample_count)));
        for Stat { min, avg, max } in row.stats {
            params.extend([real(min), real(avg), real(max)]);
        }
        params.extend([row.ah_chg, row.ah_dis, row.wh_chg, row.wh_dis].map(Value::Real));
        self.conn
            .prepare_cached(insert_10min_sql())?
            .execute(rusqlite::params_from_iter(params))?;
        Ok(())
    }

    /// Writes the daily record of the local day `day` (DD-09: 00:00 local to
    /// the next 00:00 local, 23 or 25 h on DST days). `capacity_ah` is
    /// `CAP_AH` for `equiv_cycles`. Returns `false` when there was nothing to
    /// write (no 10-min rows that day, or the record already exists).
    ///
    /// # Errors
    ///
    /// SQLite errors.
    pub fn write_daily(
        &mut self,
        day: Date,
        tz: &TimeZone,
        capacity_ah: Option<f64>,
    ) -> Result<bool> {
        let (start, end) = local_day_bounds(day, tz)?;
        let inserted = self.conn.prepare_cached(insert_daily_sql())?.execute((
            day.to_string(),
            utc_text(start),
            utc_text(end),
            capacity_ah,
        ))?;
        Ok(inserted > 0)
    }

    /// The finished local days of the last [`KEEP_10MIN_DAYS`] days that
    /// have 10-min rows but no daily record, oldest first: what the daily job
    /// writes, including days missed while the Pi was off at midnight (D3
    /// §4, DD-47). Today is never in it.
    ///
    /// # Errors
    ///
    /// SQLite errors.
    pub fn days_missing_daily(&self, now: Timestamp, tz: &TimeZone) -> Result<Vec<Date>> {
        let today = local_date(now, tz);
        let first = today.checked_sub(KEEP_10MIN_DAYS.days())?;
        let (from, _) = local_day_bounds(first, tz)?;
        let (today_start, _) = local_day_bounds(today, tz)?;

        let mut stmt = self
            .conn
            .prepare_cached("SELECT ts FROM agg_10min WHERE ts >= ?1 AND ts < ?2")?;
        let mut days = BTreeSet::new();
        for ts in stmt.query_map((utc_text(from), utc_text(today_start)), |r| {
            r.get::<_, String>(0)
        })? {
            if let Some(ts) = crate::wallclock::parse_utc_text(&ts?) {
                days.insert(local_date(ts, tz));
            }
        }

        let mut stmt = self
            .conn
            .prepare_cached("SELECT day FROM agg_daily WHERE day >= ?1")?;
        for day in stmt.query_map([first.to_string()], |r| r.get::<_, String>(0))? {
            if let Ok(day) = day?.parse::<Date>() {
                days.remove(&day);
            }
        }
        Ok(days.into_iter().collect())
    }

    /// Compaction (DD-09, S10.7): deletes 10-min rows older than
    /// [`KEEP_10MIN_DAYS`] × 24 h, but only those whose day already has a
    /// daily record, so a failed daily job never loses data. Returns the
    /// number of rows deleted.
    ///
    /// # Errors
    ///
    /// SQLite errors.
    pub fn compact_10min(&mut self, now: Timestamp) -> Result<usize> {
        let cutoff = utc_text(add_ms(now, -KEEP_10MIN_DAYS * DAY_MS));
        Ok(self
            .conn
            .prepare_cached(
                "DELETE FROM agg_10min WHERE ts < ?1 AND EXISTS (
                 SELECT 1 FROM agg_daily d
                 WHERE agg_10min.ts >= d.ts_start AND agg_10min.ts < d.ts_end)",
            )?
            .execute([cutoff])?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregate::Aggregator;
    use crate::signals::Signals;
    use crate::test_util::{amsterdam, close, ts};
    use jiff::civil::date;
    use std::time::Duration;

    fn row(at: &str, count: u32, pack: (f64, f64, f64), ah: (f64, f64)) -> AggRow {
        let mut stats = [Stat::default(); 15];
        stats[0] = Stat {
            min: Some(pack.0),
            avg: Some(pack.1),
            max: Some(pack.2),
        };
        AggRow {
            ts: ts(at),
            sample_count: count,
            stats,
            ah_chg: ah.0,
            ah_dis: ah.1,
            wh_chg: ah.0 * 13.0,
            wh_dis: ah.1 * 13.0,
        }
    }

    fn get_f64(store: &Store, sql: &str, key: &str) -> Option<f64> {
        store.conn().query_row(sql, [key], |r| r.get(0)).unwrap()
    }

    #[test]
    fn a_closed_interval_is_stored_with_its_utc_start() {
        let mut store = Store::open_in_memory().unwrap();
        let mut agg = Aggregator::new();
        let mut s = Signals::default();
        s.sample.pack_v = Some(13.25);
        s.sample.current_a = Some(-1.5);
        agg.add(ts("2026-11-24T10:03:00Z"), &s, Duration::from_secs(1));
        store.insert_10min(&agg.flush().unwrap()).unwrap();

        let (key, count, pack_avg, cur_min, t_in, ah_dis): (String, i64, f64, f64, Option<f64>, f64) = store
            .conn()
            .query_row(
                "SELECT ts, sample_count, pack_v_avg, current_a_min, t_air_in_c_max, ah_dis FROM agg_10min",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .unwrap();
        assert_eq!(key, "2026-11-24T10:00:00.000Z");
        assert_eq!(count, 1);
        assert!(close(pack_avg, 13.25));
        assert!(close(cur_min, -1.5));
        assert_eq!(t_in, None, "invalid signal stored as NULL");
        assert!(close(ah_dis, 1.5 / 3600.0));
    }

    #[test]
    fn a_second_row_for_the_same_interval_is_merged() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .insert_10min(&row(
                "2026-11-24T10:00:00Z",
                100,
                (13.0, 13.2, 13.4),
                (0.1, 0.0),
            ))
            .unwrap();
        store
            .insert_10min(&row(
                "2026-11-24T10:00:00Z",
                300,
                (12.8, 13.6, 13.9),
                (0.2, 0.05),
            ))
            .unwrap();
        let (count, min, avg, max, ah_chg, ah_dis): (i64, f64, f64, f64, f64, f64) = store
            .conn()
            .query_row(
                "SELECT sample_count, pack_v_min, pack_v_avg, pack_v_max, ah_chg, ah_dis FROM agg_10min",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .unwrap();
        assert_eq!(count, 400);
        assert!(close(min, 12.8));
        assert!(close(max, 13.9));
        assert!(close(avg, (13.2 * 100.0 + 13.6 * 300.0) / 400.0));
        assert!(close(ah_chg, 0.3));
        assert!(close(ah_dis, 0.05));
    }

    #[test]
    fn merging_keeps_a_value_that_only_one_row_has() {
        let mut store = Store::open_in_memory().unwrap();
        let mut first = row("2026-11-24T10:00:00Z", 100, (13.0, 13.0, 13.0), (0.0, 0.0));
        first.stats[0] = Stat::default(); // pack_v never valid in the first part
        store.insert_10min(&first).unwrap();
        store
            .insert_10min(&row(
                "2026-11-24T10:00:00Z",
                50,
                (12.0, 12.5, 13.0),
                (0.0, 0.0),
            ))
            .unwrap();
        let sql = "SELECT pack_v_min FROM agg_10min WHERE ts = ?1";
        let key = "2026-11-24T10:00:00.000Z";
        assert_eq!(get_f64(&store, sql, key), Some(12.0));
        let avg = get_f64(
            &store,
            "SELECT pack_v_avg FROM agg_10min WHERE ts = ?1",
            key,
        )
        .unwrap();
        assert!(close(avg, 12.5), "only the row that has the value counts");
        // Neither row has t_cell_c: stays NULL.
        assert_eq!(
            get_f64(
                &store,
                "SELECT t_cell_c_avg FROM agg_10min WHERE ts = ?1",
                key
            ),
            None
        );
    }

    fn day_of_rows(store: &mut Store, day_start_utc: &str, rows: u32) {
        for i in 0..rows {
            let mut r = row("2026-01-01T00:00:00Z", 600, (13.0, 13.1, 13.2), (0.5, 0.25));
            r.ts = add_ms(ts(day_start_utc), i64::from(i) * 600_000);
            store.insert_10min(&r).unwrap();
        }
    }

    #[test]
    fn daily_record_from_a_normal_day() {
        let tz = amsterdam();
        let mut store = Store::open_in_memory().unwrap();
        // 2026-01-15 local = 2026-01-14T23:00Z .. 2026-01-15T23:00Z: 144 rows.
        day_of_rows(&mut store, "2026-01-14T23:00:00Z", 144);
        // A row just outside the day on each side.
        store
            .insert_10min(&row(
                "2026-01-14T22:50:00Z",
                600,
                (1.0, 1.0, 1.0),
                (9.0, 9.0),
            ))
            .unwrap();
        store
            .insert_10min(&row(
                "2026-01-15T23:00:00Z",
                600,
                (99.0, 99.0, 99.0),
                (9.0, 9.0),
            ))
            .unwrap();

        assert!(
            store
                .write_daily(date(2026, 1, 15), &tz, Some(10.0))
                .unwrap()
        );
        let (start, end, count, min, avg, max, ah_chg, ah_dis, cycles): (
            String,
            String,
            i64,
            f64,
            f64,
            f64,
            f64,
            f64,
            f64,
        ) = store
            .conn()
            .query_row(
                "SELECT ts_start, ts_end, sample_count, pack_v_min, pack_v_avg, pack_v_max,
                        ah_chg, ah_dis, equiv_cycles
                 FROM agg_daily WHERE day = '2026-01-15'",
                [],
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
                    ))
                },
            )
            .unwrap();
        assert_eq!(start, "2026-01-14T23:00:00.000Z");
        assert_eq!(end, "2026-01-15T23:00:00.000Z");
        assert_eq!(count, 144 * 600);
        assert!(close(min, 13.0) && close(avg, 13.1) && close(max, 13.2));
        assert!(close(ah_chg, 144.0 * 0.5));
        assert!(close(ah_dis, 144.0 * 0.25));
        assert!(close(cycles, 144.0 * 0.25 / 10.0));
    }

    #[test]
    fn daily_average_is_weighted_by_sample_count() {
        let tz = amsterdam();
        let mut store = Store::open_in_memory().unwrap();
        store
            .insert_10min(&row(
                "2026-01-15T08:00:00Z",
                600,
                (13.0, 13.0, 13.0),
                (0.0, 0.0),
            ))
            .unwrap();
        store
            .insert_10min(&row(
                "2026-01-15T08:10:00Z",
                200,
                (14.0, 14.0, 14.0),
                (0.0, 0.0),
            ))
            .unwrap();
        store.write_daily(date(2026, 1, 15), &tz, None).unwrap();
        let avg = get_f64(
            &store,
            "SELECT pack_v_avg FROM agg_daily WHERE day = ?1",
            "2026-01-15",
        )
        .unwrap();
        assert!(close(avg, (13.0 * 600.0 + 14.0 * 200.0) / 800.0));
        // Unknown capacity: no equivalent cycles.
        assert_eq!(
            get_f64(
                &store,
                "SELECT equiv_cycles FROM agg_daily WHERE day = ?1",
                "2026-01-15"
            ),
            None
        );
    }

    #[test]
    fn dst_days_take_23_and_25_hours_of_rows() {
        let tz = amsterdam();
        let mut store = Store::open_in_memory().unwrap();
        // Spring forward 2026-03-29: 23 h = 138 intervals; fill 26 h around it.
        day_of_rows(&mut store, "2026-03-28T22:00:00Z", 26 * 6);
        store.write_daily(date(2026, 3, 29), &tz, None).unwrap();
        // Fall back 2026-10-25: 25 h = 150 intervals.
        day_of_rows(&mut store, "2026-10-24T21:00:00Z", 28 * 6);
        store.write_daily(date(2026, 10, 25), &tz, None).unwrap();

        let count = |day: &str| -> i64 {
            store
                .conn()
                .query_row(
                    "SELECT sample_count FROM agg_daily WHERE day = ?1",
                    [day],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert_eq!(count("2026-03-29"), 23 * 6 * 600);
        assert_eq!(count("2026-10-25"), 25 * 6 * 600);
    }

    #[test]
    fn no_daily_record_for_a_day_without_rows_or_twice() {
        let tz = amsterdam();
        let mut store = Store::open_in_memory().unwrap();
        assert!(!store.write_daily(date(2026, 1, 15), &tz, None).unwrap());
        day_of_rows(&mut store, "2026-01-14T23:00:00Z", 3);
        assert!(store.write_daily(date(2026, 1, 15), &tz, None).unwrap());
        assert!(!store.write_daily(date(2026, 1, 15), &tz, None).unwrap());
        let days: i64 = store
            .conn()
            .query_row("SELECT count(*) FROM agg_daily", [], |r| r.get(0))
            .unwrap();
        assert_eq!(days, 1);
    }

    #[test]
    fn missing_days_are_found_for_catch_up() {
        let tz = amsterdam();
        let mut store = Store::open_in_memory().unwrap();
        // Rows on 10, 11 (already has a daily record) and 13 January, and
        // today (15 January).
        day_of_rows(&mut store, "2026-01-09T23:00:00Z", 2);
        day_of_rows(&mut store, "2026-01-10T23:00:00Z", 2);
        store.write_daily(date(2026, 1, 11), &tz, None).unwrap();
        day_of_rows(&mut store, "2026-01-12T23:00:00Z", 2);
        day_of_rows(&mut store, "2026-01-14T23:00:00Z", 2);
        let now = ts("2026-01-15T08:00:00Z");
        assert_eq!(
            store.days_missing_daily(now, &tz).unwrap(),
            vec![date(2026, 1, 10), date(2026, 1, 13)]
        );
        // A row 40 days back is outside the catch-up window.
        day_of_rows(&mut store, "2025-12-05T23:00:00Z", 1);
        assert_eq!(store.days_missing_daily(now, &tz).unwrap().len(), 2);
    }

    #[test]
    fn a_row_just_before_local_midnight_belongs_to_that_day() {
        let tz = amsterdam();
        let mut store = Store::open_in_memory().unwrap();
        // 22:50Z in January is 23:50 local on the 14th.
        store
            .insert_10min(&row(
                "2026-01-14T22:50:00Z",
                600,
                (13.0, 13.0, 13.0),
                (0.0, 0.0),
            ))
            .unwrap();
        assert_eq!(
            store
                .days_missing_daily(ts("2026-01-15T00:00:05Z"), &tz)
                .unwrap(),
            vec![date(2026, 1, 14)]
        );
    }

    #[test]
    fn compaction_deletes_only_old_rows_of_days_with_a_daily_record() {
        let tz = amsterdam();
        let mut store = Store::open_in_memory().unwrap();
        // Two old days: 1 Jan has a daily record, 2 Jan has not (failed job).
        day_of_rows(&mut store, "2025-12-31T23:00:00Z", 144);
        day_of_rows(&mut store, "2026-01-01T23:00:00Z", 144);
        store.write_daily(date(2026, 1, 1), &tz, None).unwrap();
        // A recent day with a daily record.
        day_of_rows(&mut store, "2026-01-19T23:00:00Z", 144);
        store.write_daily(date(2026, 1, 20), &tz, None).unwrap();

        let now = ts("2026-02-10T00:05:00Z");
        assert_eq!(store.compact_10min(now).unwrap(), 144);
        let left = |from: &str, to: &str| -> i64 {
            store
                .conn()
                .query_row(
                    "SELECT count(*) FROM agg_10min WHERE ts >= ?1 AND ts < ?2",
                    [from, to],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert_eq!(
            left("2025-12-31T23:00:00.000Z", "2026-01-01T23:00:00.000Z"),
            0
        );
        assert_eq!(
            left("2026-01-01T23:00:00.000Z", "2026-01-02T23:00:00.000Z"),
            144
        );
        assert_eq!(
            left("2026-01-19T23:00:00.000Z", "2026-01-20T23:00:00.000Z"),
            144
        );
        // Daily records are never deleted.
        let days: i64 = store
            .conn()
            .query_row("SELECT count(*) FROM agg_daily", [], |r| r.get(0))
            .unwrap();
        assert_eq!(days, 2);
    }

    #[test]
    fn ten_minute_rows_are_kept_at_least_30_days() {
        let tz = amsterdam();
        let mut store = Store::open_in_memory().unwrap();
        day_of_rows(&mut store, "2026-01-14T23:00:00Z", 144);
        store.write_daily(date(2026, 1, 15), &tz, None).unwrap();
        // Exactly 30 days after the first row: nothing is older than 30 days.
        assert_eq!(store.compact_10min(ts("2026-02-13T23:00:00Z")).unwrap(), 0);
        // Each row goes once it is more than 30 days old.
        assert_eq!(
            store.compact_10min(ts("2026-02-13T23:00:00.001Z")).unwrap(),
            1
        );
        assert_eq!(
            store.compact_10min(ts("2026-02-14T23:00:00Z")).unwrap(),
            143
        );
    }
}

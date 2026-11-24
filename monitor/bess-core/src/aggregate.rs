//! 10-minute aggregation, the `aggregator` task's logic
//! (docs/design/data-lifecycle.md §3; S10.3; DD-10, DD-41, DD-45, DD-46).
//!
//! Intervals are fixed 10 minutes aligned to UTC `:00`, `:10`, … `:50`, keyed
//! by their UTC start. The open interval is held in RAM: per signal min, max,
//! sum and count of the valid values, the number of samples, and Ah / Wh
//! charged and discharged. The first sample at or after the next boundary
//! closes it (the caller sends the row to `db_writer` and the new interval
//! starts with that sample); without samples a timer closes it 2 s after the
//! boundary. A row is only made from at least one sample, so an interval
//! without samples is never written and graphs show the gap (DD-10).
//!
//! Ah / Wh: each sample with a valid current counts for exactly one telemetry
//! period, so missing samples are not filled in and a clock step cannot add
//! or remove charge (DD-46).

use crate::signals::{Signals, CURRENT_A, PACK_V, SIGNAL_COUNT};
use crate::wallclock::add_ms;
use jiff::Timestamp;
use std::time::Duration;

/// Interval length.
pub const INTERVAL_MS: i64 = 10 * 60 * 1000;

/// How long after a boundary the timer closes an interval that got no
/// closing sample (DD-45).
pub const CLOSE_AFTER_MS: i64 = 2000;

/// Min, average and max of one signal over an interval; all `None` when the
/// signal had no valid value in it (DD-41).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Stat {
    pub min: Option<f64>,
    pub avg: Option<f64>,
    pub max: Option<f64>,
}

/// One `agg_10min` row.
#[derive(Debug, Clone, PartialEq)]
pub struct AggRow {
    /// UTC start of the interval.
    pub ts: Timestamp,
    /// Samples in the interval, valid or not (DD-10); at most 600 at 1 s.
    pub sample_count: u32,
    /// Per signal, in `SIGNAL_COLUMNS` order.
    pub stats: [Stat; SIGNAL_COUNT],
    pub ah_chg: f64,
    pub ah_dis: f64,
    pub wh_chg: f64,
    pub wh_dis: f64,
}

/// The UTC start of the interval `ts` falls in.
#[must_use]
pub fn interval_start(ts: Timestamp) -> Timestamp {
    let ms = ts.as_millisecond().div_euclid(INTERVAL_MS) * INTERVAL_MS;
    Timestamp::from_millisecond(ms).unwrap_or(ts)
}

#[derive(Debug, Clone, Copy, Default)]
struct Acc {
    min: f64,
    max: f64,
    sum: f64,
    count: u32,
}

impl Acc {
    fn add(&mut self, v: f64) {
        if self.count == 0 {
            self.min = v;
            self.max = v;
        } else {
            self.min = self.min.min(v);
            self.max = self.max.max(v);
        }
        self.sum += v;
        self.count += 1;
    }

    fn stat(&self) -> Stat {
        if self.count == 0 {
            return Stat::default();
        }
        Stat {
            min: Some(self.min),
            avg: Some(self.sum / f64::from(self.count)),
            max: Some(self.max),
        }
    }
}

#[derive(Debug, Clone)]
struct Open {
    start: Timestamp,
    sample_count: u32,
    acc: [Acc; SIGNAL_COUNT],
    ah_chg: f64,
    ah_dis: f64,
    wh_chg: f64,
    wh_dis: f64,
}

impl Open {
    fn new(start: Timestamp) -> Open {
        Open {
            start,
            sample_count: 0,
            acc: [Acc::default(); SIGNAL_COUNT],
            ah_chg: 0.0,
            ah_dis: 0.0,
            wh_chg: 0.0,
            wh_dis: 0.0,
        }
    }

    fn add(&mut self, signals: &Signals, period: Duration) {
        let values = signals.values();
        self.sample_count += 1;
        for (acc, value) in self.acc.iter_mut().zip(values) {
            if let Some(v) = value.filter(|v| v.is_finite()) {
                acc.add(v);
            }
        }
        // DD-46: one telemetry period per sample with a valid current.
        let hours = period.as_secs_f64() / 3600.0;
        if let Some(i) = values[CURRENT_A] {
            let ah = i.abs() * hours;
            let wh = values[PACK_V].map(|v| (v * i).abs() * hours);
            if i > 0.0 {
                self.ah_chg += ah;
                self.wh_chg += wh.unwrap_or(0.0);
            } else if i < 0.0 {
                self.ah_dis += ah;
                self.wh_dis += wh.unwrap_or(0.0);
            }
        }
    }

    fn row(&self) -> AggRow {
        AggRow {
            ts: self.start,
            sample_count: self.sample_count,
            stats: self.acc.map(|a| a.stat()),
            ah_chg: self.ah_chg,
            ah_dis: self.ah_dis,
            wh_chg: self.wh_chg,
            wh_dis: self.wh_dis,
        }
    }
}

/// The open 10-minute interval.
#[derive(Debug, Clone, Default)]
pub struct Aggregator {
    open: Option<Open>,
}

impl Aggregator {
    #[must_use]
    pub fn new() -> Aggregator {
        Aggregator::default()
    }

    /// Adds one sample received at `utc`. `period` is the CONFIG
    /// `telemetry_ms`. Returns the closed row when this sample falls in
    /// another interval than the open one; the sample then starts the new
    /// interval. A sample from an earlier interval (the clock stepped back)
    /// also closes the open one; `db_writer` merges it into an existing row
    /// with the same `ts` (D3 §3).
    pub fn add(&mut self, utc: Timestamp, signals: &Signals, period: Duration) -> Option<AggRow> {
        let start = interval_start(utc);
        let closed = match &self.open {
            Some(open) if open.start == start => None,
            Some(_) => self.open.take().map(|o| o.row()),
            None => None,
        };
        self.open
            .get_or_insert_with(|| Open::new(start))
            .add(signals, period);
        closed
    }

    /// The timer: closes the open interval once `now` is 2 s past its end.
    pub fn poll(&mut self, now: Timestamp) -> Option<AggRow> {
        match self.next_deadline() {
            Some(deadline) if now >= deadline => self.open.take().map(|o| o.row()),
            _ => None,
        }
    }

    /// When [`Aggregator::poll`] should run next; `None` with no open
    /// interval.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Timestamp> {
        self.open
            .as_ref()
            .map(|o| add_ms(o.start, INTERVAL_MS + CLOSE_AFTER_MS))
    }

    /// Clean shutdown: the open interval as a partial row (D5 §1).
    pub fn flush(&mut self) -> Option<AggRow> {
        self.open.take().map(|o| o.row())
    }

    /// The row the open interval would give now, for the live graphs.
    #[must_use]
    pub fn peek(&self) -> Option<AggRow> {
        self.open.as_ref().map(Open::row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signals::SIGNAL_COLUMNS;

    const SECOND: Duration = Duration::from_secs(1);

    fn ts(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    fn at(base: Timestamp, secs: i64) -> Timestamp {
        add_ms(base, secs * 1000)
    }

    fn signals(pack_v: f64, current_a: f64) -> Signals {
        let mut s = Signals::default();
        s.sample.pack_v = Some(pack_v);
        s.sample.current_a = Some(current_a);
        s.sample.power_w = Some(pack_v * current_a);
        s
    }

    fn stat(row: &AggRow, column: &str) -> Stat {
        row.stats[SIGNAL_COLUMNS.iter().position(|c| *c == column).unwrap()]
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn intervals_align_to_utc_ten_minutes() {
        assert_eq!(
            interval_start(ts("2026-11-24T10:00:00Z")),
            ts("2026-11-24T10:00:00Z")
        );
        assert_eq!(
            interval_start(ts("2026-11-24T10:09:59.999Z")),
            ts("2026-11-24T10:00:00Z")
        );
        assert_eq!(
            interval_start(ts("2026-11-24T10:10:00Z")),
            ts("2026-11-24T10:10:00Z")
        );
        assert_eq!(
            interval_start(ts("2026-11-24T23:55:01Z")),
            ts("2026-11-24T23:50:00Z")
        );
        // Before 1970 too (div_euclid, not truncation).
        assert_eq!(
            interval_start(ts("1969-12-31T23:59:59Z")),
            ts("1969-12-31T23:50:00Z")
        );
    }

    #[test]
    fn full_interval_has_600_samples_and_exact_stats() {
        let base = ts("2026-11-24T10:00:00Z");
        let mut agg = Aggregator::new();
        for s in 0..600 {
            let v = 13.0 + f64::from(s % 3) * 0.1; // 13.0, 13.1, 13.2
            assert!(agg
                .add(at(base, i64::from(s)), &signals(v, 2.0), SECOND)
                .is_none());
        }
        // The first sample at the boundary closes the interval.
        let row = agg.add(at(base, 600), &signals(13.0, 2.0), SECOND).unwrap();
        assert_eq!(row.ts, base);
        assert_eq!(row.sample_count, 600);
        let pack = stat(&row, "pack_v");
        assert!(close(pack.min.unwrap(), 13.0));
        assert!(close(pack.max.unwrap(), 13.2));
        assert!(close(pack.avg.unwrap(), 13.1));
        let current = stat(&row, "current_a");
        assert_eq!((current.min, current.max), (Some(2.0), Some(2.0)));
        // 2 A for 600 s = 1/3 Ah charged.
        assert!(close(row.ah_chg, 2.0 * 600.0 / 3600.0));
        assert!(close(row.ah_dis, 0.0));
        // Wh = Σ |V·I|·1 s / 3600 = 2 A × 13.1 V avg × 600 s / 3600.
        assert!(close(row.wh_chg, 2.0 * 13.1 * 600.0 / 3600.0));
        // The closing sample started the next interval.
        assert_eq!(agg.peek().unwrap().ts, ts("2026-11-24T10:10:00Z"));
        assert_eq!(agg.peek().unwrap().sample_count, 1);
    }

    #[test]
    fn discharge_goes_to_ah_dis_and_zero_current_to_neither() {
        let base = ts("2026-11-24T10:00:00Z");
        let mut agg = Aggregator::new();
        for s in 0..360 {
            agg.add(at(base, s), &signals(12.8, -5.0), SECOND);
        }
        for s in 360..420 {
            agg.add(at(base, s), &signals(12.8, 0.0), SECOND);
        }
        let row = agg.flush().unwrap();
        assert!(close(row.ah_dis, 5.0 * 360.0 / 3600.0)); // 0.5 Ah
        assert!(close(row.wh_dis, 5.0 * 12.8 * 360.0 / 3600.0));
        assert!(close(row.ah_chg, 0.0));
        assert!(close(row.wh_chg, 0.0));
        assert_eq!(row.sample_count, 420);
    }

    #[test]
    fn telemetry_period_from_config_sets_the_charge_per_sample() {
        let base = ts("2026-11-24T10:00:00Z");
        let mut agg = Aggregator::new();
        // 500 ms telemetry: 1200 samples of 3.6 A = 0.6 Ah.
        for s in 0..1200 {
            agg.add(
                add_ms(base, s * 500),
                &signals(13.0, 3.6),
                Duration::from_millis(500),
            );
        }
        let row = agg.flush().unwrap();
        assert_eq!(row.sample_count, 1200);
        assert!(close(row.ah_chg, 0.6));
    }

    #[test]
    fn missing_samples_are_not_filled_in() {
        // DD-46: 100 samples spread over 10 minutes count as 100 s of charge.
        let base = ts("2026-11-24T10:00:00Z");
        let mut agg = Aggregator::new();
        for s in 0..100 {
            agg.add(at(base, s * 6), &signals(13.0, 36.0), SECOND);
        }
        let row = agg.flush().unwrap();
        assert_eq!(row.sample_count, 100);
        assert!(close(row.ah_chg, 1.0));
    }

    #[test]
    fn invalid_values_are_left_out_of_min_avg_max() {
        let base = ts("2026-11-24T10:00:00Z");
        let mut agg = Aggregator::new();
        let mut no_current = signals(13.0, 0.0);
        no_current.sample.current_a = None;
        no_current.sample.power_w = None;
        agg.add(base, &no_current, SECOND);
        agg.add(at(base, 1), &signals(13.4, 4.0), SECOND);
        let row = agg.flush().unwrap();
        assert_eq!(row.sample_count, 2);
        let current = stat(&row, "current_a");
        assert_eq!(
            current,
            Stat {
                min: Some(4.0),
                avg: Some(4.0),
                max: Some(4.0)
            }
        );
        let pack = stat(&row, "pack_v");
        assert!(close(pack.avg.unwrap(), 13.2));
        // Never valid in the interval: NULL in all three columns.
        assert_eq!(stat(&row, "t_air_in_c"), Stat::default());
        assert_eq!(stat(&row, "fan1_rpm"), Stat::default());
        // The sample without a current counts no charge.
        assert!(close(row.ah_chg, 4.0 / 3600.0));
    }

    #[test]
    fn current_without_pack_voltage_counts_ah_but_not_wh() {
        let base = ts("2026-11-24T10:00:00Z");
        let mut agg = Aggregator::new();
        let mut s = signals(13.0, 3.6);
        s.sample.pack_v = None;
        agg.add(base, &s, SECOND);
        let row = agg.flush().unwrap();
        assert!(close(row.ah_chg, 0.001));
        assert!(close(row.wh_chg, 0.0));
    }

    #[test]
    fn timer_closes_an_interval_2s_after_the_boundary() {
        let base = ts("2026-11-24T10:00:00Z");
        let mut agg = Aggregator::new();
        assert_eq!(agg.next_deadline(), None);
        assert_eq!(agg.poll(base), None);
        agg.add(at(base, 30), &signals(13.0, 1.0), SECOND);
        assert_eq!(agg.next_deadline(), Some(ts("2026-11-24T10:10:02Z")));
        assert_eq!(agg.poll(ts("2026-11-24T10:10:01.999Z")), None);
        let row = agg.poll(ts("2026-11-24T10:10:02Z")).unwrap();
        assert_eq!(row.ts, base);
        assert_eq!(row.sample_count, 1);
        // Nothing open any more: no empty row for the next interval.
        assert_eq!(agg.poll(ts("2026-11-24T10:30:00Z")), None);
        assert_eq!(agg.flush(), None);
    }

    #[test]
    fn link_lost_mid_interval_gives_a_partial_row_and_no_empty_rows() {
        let base = ts("2026-11-24T10:00:00Z");
        let mut agg = Aggregator::new();
        for s in 0..120 {
            agg.add(at(base, s), &signals(13.0, 1.0), SECOND);
        }
        // Link back 25 minutes later, in the 10:20 interval.
        let row = agg
            .add(ts("2026-11-24T10:25:00Z"), &signals(13.0, 1.0), SECOND)
            .unwrap();
        assert_eq!((row.ts, row.sample_count), (base, 120));
        let row = agg.flush().unwrap();
        assert_eq!((row.ts, row.sample_count), (ts("2026-11-24T10:20:00Z"), 1));
    }

    #[test]
    fn clock_step_back_closes_the_open_interval() {
        let mut agg = Aggregator::new();
        agg.add(ts("2026-11-24T10:12:00Z"), &signals(13.0, 1.0), SECOND);
        let row = agg
            .add(ts("2026-11-24T10:05:00Z"), &signals(13.0, 1.0), SECOND)
            .unwrap();
        assert_eq!(row.ts, ts("2026-11-24T10:10:00Z"));
        assert_eq!(agg.peek().unwrap().ts, ts("2026-11-24T10:00:00Z"));
    }

    #[test]
    fn non_finite_values_are_ignored() {
        let base = ts("2026-11-24T10:00:00Z");
        let mut agg = Aggregator::new();
        let mut s = signals(13.0, 1.0);
        s.fan1_rpm = Some(f64::NAN);
        agg.add(base, &s, SECOND);
        assert_eq!(stat(&agg.flush().unwrap(), "fan1_rpm"), Stat::default());
    }
}

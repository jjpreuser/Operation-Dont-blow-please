//! Reading the data back for graphs and CSV export
//! (docs/design/data-lifecycle.md §6; S9.6–S9.8, S10.11; DD-10, DD-49).
//!
//! A request names a UTC time range. [`choose_tier`] picks the tier that
//! covers it (DD-49): the 1 h ring buffer when the range starts inside the
//! last hour, `agg_10min` when it starts inside the last 30 days, else
//! `agg_daily`; a coarser tier can be asked for, never a finer one. Every
//! tier comes back as the same [`Point`] list, so a graph draws the average
//! line and the min–max band the same way for all three (S9.8); a 1 s point
//! has min = avg = max. Points carry their `sample_count` and the graph
//! breaks the line where an interval is missing (DD-10).
//!
//! The ring buffer part is here, without I/O; the two database tiers are
//! read by [`Store::points`](crate::db::Store::points) and exported by
//! [`Store::write_csv`](crate::db::Store::write_csv). The CSV format is in
//! [`csv`](crate::csv).
//!
//! The newest data is not in a stored tier yet: the open 10-min interval is
//! in RAM until its boundary, and today's daily record is written at 00:00
//! local. Ring buffer samples taken without a valid clock have no UTC time
//! (DD-11) and are left out of a UTC range.

use crate::aggregate::Stat;
use crate::ring::RingBuffer;
use crate::signals::{SIGNAL_COLUMNS, SIGNAL_COUNT};
use crate::wallclock::{DAY_MS, add_ms};
use jiff::Timestamp;

/// How far back the ring buffer tier is used (DD-49): its own span.
pub const RING_TIER_MS: i64 = 60 * 60 * 1000;

/// How far back the 10-min tier is used: the 10-min retention (S10.7).
pub const TEN_MIN_TIER_MS: i64 = crate::db::KEEP_10MIN_DAYS * DAY_MS;

/// Where the points of a range come from, finest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// 1 s samples from the ring buffer (RAM, the last hour).
    Ring,
    /// `agg_10min`.
    TenMin,
    /// `agg_daily`.
    Daily,
}

impl Tier {
    /// The database table of the tier; `None` for the ring buffer.
    #[must_use]
    pub fn table(self) -> Option<AggTable> {
        match self {
            Tier::Ring => None,
            Tier::TenMin => Some(AggTable::TenMin),
            Tier::Daily => Some(AggTable::Daily),
        }
    }
}

/// A tier that lives in the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AggTable {
    TenMin,
    Daily,
}

/// A UTC time range, `from` included, `to` excluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub from: Timestamp,
    pub to: Timestamp,
}

/// The tier for `range` at `now` (DD-49): the finest tier that still holds
/// data from the start of the range, or `coarser` when that is coarser. A
/// finer tier than the range allows is never used, because it no longer has
/// the data.
#[must_use]
pub fn choose_tier(range: &Range, now: Timestamp, coarser: Option<Tier>) -> Tier {
    let covering = if range.from >= add_ms(now, -RING_TIER_MS) {
        Tier::Ring
    } else if range.from >= add_ms(now, -TEN_MIN_TIER_MS) {
        Tier::TenMin
    } else {
        Tier::Daily
    };
    coarser.map_or(covering, |c| c.max(covering))
}

/// Charge, energy and cycles over an interval: the aggregate tiers only.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Totals {
    pub ah_chg: f64,
    pub ah_dis: f64,
    pub wh_chg: f64,
    pub wh_dis: f64,
    /// Daily records only (S10.4); `None` without `CAP_AH`.
    pub equiv_cycles: Option<f64>,
}

/// One point of a graph or one CSV row.
#[derive(Debug, Clone, PartialEq)]
pub struct Point {
    /// The sample time, or the UTC start of the interval (for a daily
    /// record 00:00 local, DD-09).
    pub ts: Timestamp,
    /// Samples behind the point: 1 for the ring buffer (DD-10).
    pub sample_count: u32,
    /// Per signal, in [`SIGNAL_COLUMNS`] order; `None` where the signal was
    /// never valid (DD-41).
    pub stats: [Stat; SIGNAL_COUNT],
    /// `None` for the ring buffer.
    pub totals: Option<Totals>,
}

/// One 1 s row with the MCU state: a ring buffer sample or a snapshot row,
/// for the 1 s CSV exports.
#[derive(Debug, Clone, PartialEq)]
pub struct SampleRow {
    pub ts: Timestamp,
    /// The shown state text (`IDLE`, `CHARGING`, …), as stored with snapshot
    /// rows.
    pub state: Option<String>,
    pub values: [Option<f64>; SIGNAL_COUNT],
}

/// Which signals a CSV export has (S10.11: a chosen set of values).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalSet([bool; SIGNAL_COUNT]);

impl Default for SignalSet {
    fn default() -> Self {
        SignalSet::all()
    }
}

impl SignalSet {
    /// Every signal.
    #[must_use]
    pub fn all() -> SignalSet {
        SignalSet([true; SIGNAL_COUNT])
    }

    /// The named signals ([`SIGNAL_COLUMNS`] names), kept in column order
    /// whatever order they are named in.
    ///
    /// # Errors
    ///
    /// The first name that is not a signal.
    pub fn only<'a>(names: impl IntoIterator<Item = &'a str>) -> Result<SignalSet, &'a str> {
        let mut set = [false; SIGNAL_COUNT];
        for name in names {
            let i = SIGNAL_COLUMNS.iter().position(|c| *c == name).ok_or(name)?;
            set[i] = true;
        }
        Ok(SignalSet(set))
    }

    /// The indices of the chosen signals, in column order.
    pub fn indices(&self) -> impl Iterator<Item = usize> + '_ {
        (0..SIGNAL_COUNT).filter(|&i| self.0[i])
    }
}

fn in_range(ts: Timestamp, range: &Range) -> bool {
    ts >= range.from && ts < range.to
}

/// The ring buffer samples in `range` as points, oldest first.
#[must_use]
pub fn ring_points(ring: &RingBuffer, range: &Range) -> Vec<Point> {
    ring.iter()
        .filter_map(|e| {
            let ts = e.utc.filter(|ts| in_range(*ts, range))?;
            Some(Point {
                ts,
                sample_count: 1,
                stats: e.signals.values().map(|v| Stat {
                    min: v,
                    avg: v,
                    max: v,
                }),
                totals: None,
            })
        })
        .collect()
}

/// The ring buffer samples in `range` as 1 s rows, oldest first, for the
/// CSV export.
#[must_use]
pub fn ring_rows(ring: &RingBuffer, range: &Range) -> Vec<SampleRow> {
    ring.iter()
        .filter_map(|e| {
            let ts = e.utc.filter(|ts| in_range(*ts, range))?;
            Some(SampleRow {
                ts,
                state: Some(crate::db::state_text(e.state)),
                values: e.signals.values(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ring::RingEntry;
    use crate::signals::Signals;
    use crate::test_util::ts;
    use protocol::messages::McuState;
    use std::time::{Duration, Instant};

    fn range(from: &str, to: &str) -> Range {
        Range {
            from: ts(from),
            to: ts(to),
        }
    }

    const NOW: &str = "2026-11-25T12:00:00Z";

    #[test]
    fn a_range_inside_the_last_hour_reads_the_ring_buffer() {
        let now = ts(NOW);
        assert_eq!(
            choose_tier(&range("2026-11-25T11:30:00Z", NOW), now, None),
            Tier::Ring
        );
        // Exactly one hour back is still the ring buffer (DD-49: ≤ 1 h).
        assert_eq!(
            choose_tier(&range("2026-11-25T11:00:00Z", NOW), now, None),
            Tier::Ring
        );
        // A range that starts later than now as well.
        assert_eq!(
            choose_tier(
                &range("2026-11-25T12:30:00Z", "2026-11-25T13:00:00Z"),
                now,
                None
            ),
            Tier::Ring
        );
    }

    #[test]
    fn a_range_inside_the_last_30_days_reads_agg_10min() {
        let now = ts(NOW);
        assert_eq!(
            choose_tier(&range("2026-11-25T10:59:59.999Z", NOW), now, None),
            Tier::TenMin
        );
        // 30 × 24 h back, the 10-min retention (S10.7).
        assert_eq!(
            choose_tier(&range("2026-10-26T12:00:00Z", NOW), now, None),
            Tier::TenMin
        );
    }

    #[test]
    fn an_older_range_reads_agg_daily_also_when_it_crosses_the_boundary() {
        let now = ts(NOW);
        assert_eq!(
            choose_tier(&range("2026-10-26T11:59:59.999Z", NOW), now, None),
            Tier::Daily
        );
        // From 40 days ago up to now: the tier that covers the start.
        assert_eq!(
            choose_tier(&range("2026-10-16T00:00:00Z", NOW), now, None),
            Tier::Daily
        );
    }

    #[test]
    fn a_coarser_tier_can_be_asked_for_but_not_a_finer_one() {
        let now = ts(NOW);
        let week = range("2026-11-18T12:00:00Z", NOW);
        assert_eq!(choose_tier(&week, now, Some(Tier::Daily)), Tier::Daily);
        assert_eq!(choose_tier(&week, now, Some(Tier::Ring)), Tier::TenMin);
        let year = range("2025-11-25T12:00:00Z", NOW);
        assert_eq!(choose_tier(&year, now, Some(Tier::TenMin)), Tier::Daily);
    }

    #[test]
    fn only_the_aggregate_tiers_have_a_table() {
        assert_eq!(Tier::Ring.table(), None);
        assert_eq!(Tier::TenMin.table(), Some(AggTable::TenMin));
        assert_eq!(Tier::Daily.table(), Some(AggTable::Daily));
    }

    fn ring_with(utc: &[Option<&str>]) -> RingBuffer {
        let t0 = Instant::now();
        let mut ring = RingBuffer::new();
        for (i, at) in (0_u32..).zip(utc) {
            let mut signals = Signals::default();
            signals.sample.pack_v = Some(13.0 + f64::from(i) / 100.0);
            signals.fan_duty_pct = Some(30.0);
            ring.push(RingEntry {
                at: t0 + Duration::from_secs(u64::from(i)),
                utc: at.map(ts),
                state: if i == 0 {
                    McuState::Charging
                } else {
                    McuState::Idle
                },
                signals,
            });
        }
        ring
    }

    #[test]
    fn ring_points_are_the_samples_in_the_range() {
        let ring = ring_with(&[
            Some("2026-11-25T11:59:57Z"),
            Some("2026-11-25T11:59:58Z"),
            Some("2026-11-25T11:59:59Z"),
            Some("2026-11-25T12:00:00Z"),
        ]);
        let points = ring_points(&ring, &range("2026-11-25T11:59:58Z", NOW));
        // `from` included, `to` excluded.
        let times: Vec<Timestamp> = points.iter().map(|p| p.ts).collect();
        assert_eq!(
            times,
            vec![ts("2026-11-25T11:59:58Z"), ts("2026-11-25T11:59:59Z")]
        );
        let p = &points[0];
        assert_eq!(p.sample_count, 1);
        assert_eq!(p.totals, None);
        // min = avg = max = the sample.
        assert_eq!(
            p.stats[0],
            Stat {
                min: Some(13.01),
                avg: Some(13.01),
                max: Some(13.01)
            }
        );
        assert_eq!(p.stats[1], Stat::default(), "an invalid signal stays None");
    }

    #[test]
    fn ring_samples_without_a_valid_clock_are_left_out() {
        let ring = ring_with(&[None, Some("2026-11-25T11:59:59Z"), None]);
        let points = ring_points(&ring, &range("2026-11-25T11:00:00Z", NOW));
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].ts, ts("2026-11-25T11:59:59Z"));
    }

    #[test]
    fn ring_rows_carry_the_state_text() {
        let ring = ring_with(&[Some("2026-11-25T11:59:58Z"), Some("2026-11-25T11:59:59Z")]);
        let rows = ring_rows(&ring, &range("2026-11-25T11:00:00Z", NOW));
        let states: Vec<Option<&str>> = rows.iter().map(|r| r.state.as_deref()).collect();
        assert_eq!(states, vec![Some("CHARGING"), Some("IDLE")]);
        assert_eq!(rows[1].values[12], Some(30.0));
    }

    #[test]
    fn a_signal_set_keeps_column_order_and_refuses_unknown_names() {
        let set = SignalSet::only(["t_cell_c", "pack_v"]).unwrap();
        assert_eq!(set.indices().collect::<Vec<_>>(), vec![0, 7]);
        assert_eq!(SignalSet::only(["pack_v", "volts"]), Err("volts"));
        assert_eq!(SignalSet::all().indices().count(), SIGNAL_COUNT);
        assert_eq!(SignalSet::default(), SignalSet::all());
    }
}

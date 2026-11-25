//! The CSV export format (docs/design/data-lifecycle.md §6,
//! docs/design/csv-excel-test.md; S10.11, S10.13; DD-49, DD-62).
//!
//! For English-locale Excel (DD-62): comma-separated, `.` as decimal point,
//! UTF-8 without BOM (ASCII in practice), CRLF line ends, no `sep=` line. The
//! header names the columns with their unit, as the database does (D2 §1).
//! Every row starts with `timestamp_utc` (the stored ISO 8601 text, DD-06)
//! and `time_local` (`YYYY-MM-DD HH:MM:SS`, which Excel reads as a date).
//!
//! Two layouts:
//! - 1 s rows (ring buffer, fault snapshots): `state`, then one column per
//!   signal.
//! - Aggregates (`agg_10min`, `agg_daily`): `sample_count`, then
//!   `<signal>_min`, `_avg`, `_max` per signal, then `ah_chg`, `ah_dis`,
//!   `wh_chg`, `wh_dis`, and for daily records `equiv_cycles`.
//!
//! An invalid or missing value is an empty cell (DD-41). Numbers have a fixed
//! number of decimals per signal ([`SIGNAL_DECIMALS`]).
//!
//! The functions build one line at a time, so a caller streams an export of
//! any size without holding it in memory.

use crate::history::{AggTable, Point, SampleRow, SignalSet};
use crate::signals::{SIGNAL_COLUMNS, SIGNAL_DECIMALS};
use crate::wallclock::{local_text, utc_text};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use std::fmt::Write as _;

/// Line end (DD-62).
pub const LINE_END: &str = "\r\n";

/// Decimals of `ah_chg` / `ah_dis` (0.1 mAh).
const AH_DECIMALS: usize = 4;
/// Decimals of `wh_chg` / `wh_dis` (1 mWh).
const WH_DECIMALS: usize = 3;
/// Decimals of `equiv_cycles`.
const CYCLES_DECIMALS: usize = 4;

/// The header of a 1 s export.
#[must_use]
pub fn sample_header(signals: &SignalSet) -> String {
    let mut line = String::from("timestamp_utc,time_local,state");
    for i in signals.indices() {
        let _ = write!(line, ",{}", SIGNAL_COLUMNS[i]);
    }
    line.push_str(LINE_END);
    line
}

/// One line of a 1 s export.
#[must_use]
pub fn sample_line(row: &SampleRow, tz: &TimeZone, signals: &SignalSet) -> String {
    let mut line = times(row.ts, tz);
    line.push(',');
    if let Some(state) = &row.state {
        push_text(&mut line, state);
    }
    for i in signals.indices() {
        line.push(',');
        push_number(&mut line, row.values[i], SIGNAL_DECIMALS[i]);
    }
    line.push_str(LINE_END);
    line
}

/// The header of an aggregate export.
#[must_use]
pub fn aggregate_header(table: AggTable, signals: &SignalSet) -> String {
    let mut line = String::from("timestamp_utc,time_local,sample_count");
    for i in signals.indices() {
        let s = SIGNAL_COLUMNS[i];
        let _ = write!(line, ",{s}_min,{s}_avg,{s}_max");
    }
    line.push_str(",ah_chg,ah_dis,wh_chg,wh_dis");
    if table == AggTable::Daily {
        line.push_str(",equiv_cycles");
    }
    line.push_str(LINE_END);
    line
}

/// One line of an aggregate export.
#[must_use]
pub fn aggregate_line(
    point: &Point,
    table: AggTable,
    tz: &TimeZone,
    signals: &SignalSet,
) -> String {
    let mut line = times(point.ts, tz);
    let _ = write!(line, ",{}", point.sample_count);
    for i in signals.indices() {
        let stat = point.stats[i];
        for v in [stat.min, stat.avg, stat.max] {
            line.push(',');
            push_number(&mut line, v, SIGNAL_DECIMALS[i]);
        }
    }
    let totals = point.totals;
    for (v, decimals) in [
        (totals.map(|t| t.ah_chg), AH_DECIMALS),
        (totals.map(|t| t.ah_dis), AH_DECIMALS),
        (totals.map(|t| t.wh_chg), WH_DECIMALS),
        (totals.map(|t| t.wh_dis), WH_DECIMALS),
    ] {
        line.push(',');
        push_number(&mut line, v, decimals);
    }
    if table == AggTable::Daily {
        line.push(',');
        push_number(
            &mut line,
            totals.and_then(|t| t.equiv_cycles),
            CYCLES_DECIMALS,
        );
    }
    line.push_str(LINE_END);
    line
}

/// Writes a whole 1 s export (header and rows) and returns the number of
/// rows. `out` should be buffered.
///
/// # Errors
///
/// Write errors from `out`.
pub fn write_samples<'a>(
    out: &mut impl std::io::Write,
    tz: &TimeZone,
    signals: &SignalSet,
    rows: impl IntoIterator<Item = &'a SampleRow>,
) -> std::io::Result<usize> {
    out.write_all(sample_header(signals).as_bytes())?;
    let mut count = 0;
    for row in rows {
        out.write_all(sample_line(row, tz, signals).as_bytes())?;
        count += 1;
    }
    Ok(count)
}

/// `timestamp_utc,time_local`.
fn times(ts: Timestamp, tz: &TimeZone) -> String {
    format!("{},{}", utc_text(ts), local_text(ts, tz))
}

/// A number with a fixed number of decimals; empty when `None` or not
/// finite. A value that rounds to zero is written without a minus sign.
fn push_number(line: &mut String, v: Option<f64>, decimals: usize) {
    let Some(v) = v.filter(|v| v.is_finite()) else {
        return;
    };
    let text = format!("{v:.decimals$}");
    match text.strip_prefix('-') {
        Some(digits) if digits.bytes().all(|b| b == b'0' || b == b'.') => line.push_str(digits),
        _ => line.push_str(&text),
    }
}

/// A text cell, quoted when it holds a comma, quote or line break.
fn push_text(line: &mut String, text: &str) {
    if text.contains([',', '"', '\r', '\n']) {
        line.push('"');
        line.push_str(&text.replace('"', "\"\""));
        line.push('"');
    } else {
        line.push_str(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregate::Stat;
    use crate::history::Totals;
    use crate::signals::SIGNAL_COUNT;
    use crate::test_util::{amsterdam, ts};

    fn number(v: Option<f64>, decimals: usize) -> String {
        let mut s = String::new();
        push_number(&mut s, v, decimals);
        s
    }

    #[test]
    fn numbers_have_fixed_decimals_and_a_dot() {
        assert_eq!(number(Some(13.284), 3), "13.284");
        assert_eq!(number(Some(13.25), 3), "13.250");
        assert_eq!(number(Some(-1.25), 2), "-1.25");
        assert_eq!(number(Some(24.649_999), 1), "24.6");
        assert_eq!(number(Some(0.1 + 0.2), 2), "0.30");
        assert_eq!(number(Some(1234.6), 0), "1235");
        // No thousands separator.
        assert_eq!(number(Some(12_345.0), 1), "12345.0");
    }

    #[test]
    fn invalid_values_are_empty_cells() {
        assert_eq!(number(None, 3), "");
        assert_eq!(number(Some(f64::NAN), 3), "");
        assert_eq!(number(Some(f64::INFINITY), 3), "");
    }

    #[test]
    fn a_value_that_rounds_to_zero_has_no_minus_sign() {
        assert_eq!(number(Some(-0.001), 2), "0.00");
        assert_eq!(number(Some(-0.0), 0), "0");
        assert_eq!(number(Some(-0.004), 2), "0.00");
        assert_eq!(number(Some(-0.005_1), 2), "-0.01");
    }

    #[test]
    fn text_with_a_comma_or_quote_is_quoted() {
        let text = |t: &str| {
            let mut s = String::new();
            push_text(&mut s, t);
            s
        };
        assert_eq!(text("UNKNOWN (7)"), "UNKNOWN (7)");
        assert_eq!(text("a,b"), "\"a,b\"");
        assert_eq!(text("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn sample_header_names_every_signal_with_its_unit() {
        let header = sample_header(&SignalSet::all());
        assert!(header.ends_with("\r\n"));
        let columns: Vec<&str> = header.trim_end().split(',').collect();
        assert_eq!(&columns[..3], ["timestamp_utc", "time_local", "state"]);
        assert_eq!(&columns[3..], SIGNAL_COLUMNS);
    }

    #[test]
    fn a_sample_line_in_winter_and_summer_time() {
        let mut values = [None; SIGNAL_COUNT];
        values[0] = Some(13.284);
        values[1] = Some(-1.25);
        values[7] = Some(24.6);
        let signals = SignalSet::only(["pack_v", "current_a", "t_cell_c", "t_air_in_c"]).unwrap();
        let row = |at: &str| SampleRow {
            ts: ts(at),
            state: Some("DISCHARGING".into()),
            values,
        };
        assert_eq!(
            sample_line(&row("2026-01-15T11:00:00Z"), &amsterdam(), &signals),
            "2026-01-15T11:00:00.000Z,2026-01-15 12:00:00,DISCHARGING,13.284,-1.25,24.6,\r\n"
        );
        assert_eq!(
            sample_line(&row("2026-07-15T11:00:00Z"), &amsterdam(), &signals),
            "2026-07-15T11:00:00.000Z,2026-07-15 13:00:00,DISCHARGING,13.284,-1.25,24.6,\r\n"
        );
    }

    #[test]
    fn a_sample_line_has_as_many_cells_as_the_header() {
        let row = SampleRow {
            ts: ts("2026-01-15T11:00:00Z"),
            state: None,
            values: [Some(1.0); SIGNAL_COUNT],
        };
        let all = SignalSet::all();
        let cells = |s: &str| s.trim_end().split(',').count();
        assert_eq!(
            cells(&sample_line(&row, &amsterdam(), &all)),
            cells(&sample_header(&all))
        );
    }

    #[test]
    fn aggregate_header_has_min_avg_max_per_signal_and_the_totals() {
        let ten = aggregate_header(AggTable::TenMin, &SignalSet::all());
        let columns: Vec<&str> = ten.trim_end().split(',').collect();
        assert_eq!(columns.len(), 3 + 3 * SIGNAL_COUNT + 4);
        assert_eq!(
            &columns[..6],
            [
                "timestamp_utc",
                "time_local",
                "sample_count",
                "pack_v_min",
                "pack_v_avg",
                "pack_v_max"
            ]
        );
        assert_eq!(
            &columns[columns.len() - 4..],
            ["ah_chg", "ah_dis", "wh_chg", "wh_dis"]
        );
        let daily = aggregate_header(AggTable::Daily, &SignalSet::all());
        assert!(daily.ends_with(",wh_dis,equiv_cycles\r\n"));
    }

    #[test]
    fn an_aggregate_line() {
        let mut stats = [Stat::default(); SIGNAL_COUNT];
        stats[0] = Stat {
            min: Some(13.1),
            avg: Some(13.204_6),
            max: Some(13.3),
        };
        let point = Point {
            ts: ts("2026-01-14T23:00:00Z"),
            sample_count: 86_400,
            stats,
            totals: Some(Totals {
                ah_chg: 3.333_33,
                ah_dis: 2.5,
                wh_chg: 44.1,
                wh_dis: 32.75,
                equiv_cycles: Some(0.416_666),
            }),
        };
        let signals = SignalSet::only(["pack_v", "current_a"]).unwrap();
        assert_eq!(
            aggregate_line(&point, AggTable::Daily, &amsterdam(), &signals),
            "2026-01-14T23:00:00.000Z,2026-01-15 00:00:00,86400,13.100,13.205,13.300,,,,\
             3.3333,2.5000,44.100,32.750,0.4167\r\n"
        );
        let ten_min = aggregate_line(&point, AggTable::TenMin, &amsterdam(), &signals);
        assert!(
            ten_min.ends_with(",32.750\r\n"),
            "no equiv_cycles: {ten_min}"
        );
    }

    #[test]
    fn write_samples_streams_the_header_and_every_row() {
        let rows: Vec<SampleRow> = (0..3)
            .map(|i| SampleRow {
                ts: ts("2026-01-15T11:00:00Z") + jiff::SignedDuration::from_secs(i),
                state: Some("IDLE".into()),
                values: [None; SIGNAL_COUNT],
            })
            .collect();
        let mut out = Vec::new();
        let count = write_samples(&mut out, &amsterdam(), &SignalSet::all(), &rows).unwrap();
        assert_eq!(count, 3);
        let text = String::from_utf8(out).unwrap();
        assert!(text.is_ascii());
        assert!(!text.starts_with("sep="));
        assert_eq!(text.matches("\r\n").count(), 4);
        assert_eq!(text.matches('\n').count(), 4, "every line ends in CRLF");
    }
}

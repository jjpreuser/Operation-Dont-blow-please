//! The CSV export format (docs/design/data-lifecycle.md §6,
//! docs/design/csv-excel-test.md; S10.11, S10.13; DD-49, DD-62).
//!
//! Excel opens a `.csv` by double-click with the list separator and decimal
//! sign of the Windows number settings, so no single file splits into
//! columns everywhere (D12). Two [`Format`]s, chosen on export (DD-62):
//! - [`Format::English`], the default: `,` between cells, `.` decimal point.
//! - [`Format::Dutch`], for Excel with a decimal comma (Dutch, German,
//!   French, … settings): `;` between cells, `,` as decimal sign.
//!
//! Both: UTF-8 without BOM (ASCII in practice), CRLF line ends, never a
//! `sep=` line (it makes Dutch Excel read 13.284 as 13284). The header
//! names the columns with their unit, as the database does (D2 §1). Every
//! row starts with `timestamp_utc` (the stored ISO 8601 text, DD-06) and
//! `time_local` (`YYYY-MM-DD HH:MM:SS`, which Excel reads as a date).
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
use jiff::tz::TimeZone;

/// Line end (DD-62).
pub const LINE_END: &str = "\r\n";

/// Decimals of `ah_chg` / `ah_dis` (0.1 mAh).
const AH_DECIMALS: usize = 4;
/// Decimals of `wh_chg` / `wh_dis` (1 mWh).
const WH_DECIMALS: usize = 3;
/// Decimals of `equiv_cycles`.
const CYCLES_DECIMALS: usize = 4;

/// Which Excel number settings the file is for (DD-62).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Format {
    /// `,` between cells, `.` decimal point: Excel with English number
    /// settings.
    #[default]
    English,
    /// `;` between cells, `,` decimal sign: Excel with a decimal comma
    /// (Dutch, German, French, … number settings).
    Dutch,
}

impl Format {
    /// The cell separator.
    #[must_use]
    pub fn separator(self) -> char {
        match self {
            Format::English => ',',
            Format::Dutch => ';',
        }
    }

    /// The decimal sign.
    #[must_use]
    pub fn decimal(self) -> char {
        match self {
            Format::English => '.',
            Format::Dutch => ',',
        }
    }

    /// The format for a decimal sign, as Windows keeps it in its number
    /// settings (`sDecimal`): a decimal comma gives [`Format::Dutch`].
    #[must_use]
    pub fn for_decimal_sign(sign: &str) -> Format {
        if sign.trim() == "," {
            Format::Dutch
        } else {
            Format::English
        }
    }
}

/// What every export line needs besides its data.
#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    /// For `time_local` (DD-95).
    pub tz: TimeZone,
    /// The signal columns (S10.11: a chosen set of values).
    pub signals: SignalSet,
    pub format: Format,
}

/// One line being built: cells joined by the format's separator.
struct Line {
    text: String,
    format: Format,
    empty: bool,
}

impl Line {
    fn new(format: Format) -> Line {
        Line {
            text: String::new(),
            format,
            empty: true,
        }
    }

    fn next_cell(&mut self) -> &mut String {
        if !self.empty {
            self.text.push(self.format.separator());
        }
        self.empty = false;
        &mut self.text
    }

    /// A cell that never holds a separator or quote (names, times, counts).
    fn plain(&mut self, text: &str) {
        self.next_cell().push_str(text);
    }

    /// A text cell, quoted when it holds the separator, a quote or a line
    /// break.
    fn text(&mut self, text: &str) {
        let separator = self.format.separator();
        let cell = self.next_cell();
        if text.contains([separator, '"', '\r', '\n']) {
            cell.push('"');
            cell.push_str(&text.replace('"', "\"\""));
            cell.push('"');
        } else {
            cell.push_str(text);
        }
    }

    /// A number with a fixed number of decimals; empty when `None` or not
    /// finite. A value that rounds to zero is written without a minus sign.
    fn number(&mut self, v: Option<f64>, decimals: usize) {
        let decimal = self.format.decimal();
        let cell = self.next_cell();
        let Some(v) = v.filter(|v| v.is_finite()) else {
            return;
        };
        let text = format!("{v:.decimals$}");
        let text = match text.strip_prefix('-') {
            Some(digits) if digits.bytes().all(|b| b == b'0' || b == b'.') => digits,
            _ => &text,
        };
        for c in text.chars() {
            cell.push(if c == '.' { decimal } else { c });
        }
    }

    /// `timestamp_utc` and `time_local`.
    fn times(&mut self, ts: jiff::Timestamp, tz: &TimeZone) {
        self.plain(&utc_text(ts));
        self.plain(&local_text(ts, tz));
    }

    fn finish(mut self) -> String {
        self.text.push_str(LINE_END);
        self.text
    }
}

/// The header of a 1 s export.
#[must_use]
pub fn sample_header(options: &Options) -> String {
    let mut line = Line::new(options.format);
    for name in ["timestamp_utc", "time_local", "state"] {
        line.plain(name);
    }
    for i in options.signals.indices() {
        line.plain(SIGNAL_COLUMNS[i]);
    }
    line.finish()
}

/// One line of a 1 s export.
#[must_use]
pub fn sample_line(row: &SampleRow, options: &Options) -> String {
    let mut line = Line::new(options.format);
    line.times(row.ts, &options.tz);
    line.text(row.state.as_deref().unwrap_or(""));
    for i in options.signals.indices() {
        line.number(row.values[i], SIGNAL_DECIMALS[i]);
    }
    line.finish()
}

/// The header of an aggregate export.
#[must_use]
pub fn aggregate_header(table: AggTable, options: &Options) -> String {
    let mut line = Line::new(options.format);
    for name in ["timestamp_utc", "time_local", "sample_count"] {
        line.plain(name);
    }
    for i in options.signals.indices() {
        let s = SIGNAL_COLUMNS[i];
        for stat in ["min", "avg", "max"] {
            line.plain(&format!("{s}_{stat}"));
        }
    }
    for name in ["ah_chg", "ah_dis", "wh_chg", "wh_dis"] {
        line.plain(name);
    }
    if table == AggTable::Daily {
        line.plain("equiv_cycles");
    }
    line.finish()
}

/// One line of an aggregate export.
#[must_use]
pub fn aggregate_line(point: &Point, table: AggTable, options: &Options) -> String {
    let mut line = Line::new(options.format);
    line.times(point.ts, &options.tz);
    line.plain(&point.sample_count.to_string());
    for i in options.signals.indices() {
        let stat = point.stats[i];
        for v in [stat.min, stat.avg, stat.max] {
            line.number(v, SIGNAL_DECIMALS[i]);
        }
    }
    let totals = point.totals;
    line.number(totals.map(|t| t.ah_chg), AH_DECIMALS);
    line.number(totals.map(|t| t.ah_dis), AH_DECIMALS);
    line.number(totals.map(|t| t.wh_chg), WH_DECIMALS);
    line.number(totals.map(|t| t.wh_dis), WH_DECIMALS);
    if table == AggTable::Daily {
        line.number(totals.and_then(|t| t.equiv_cycles), CYCLES_DECIMALS);
    }
    line.finish()
}

/// Writes a whole 1 s export (header and rows) and returns the number of
/// rows. `out` should be buffered.
///
/// # Errors
///
/// Write errors from `out`.
pub fn write_samples<'a>(
    out: &mut impl std::io::Write,
    options: &Options,
    rows: impl IntoIterator<Item = &'a SampleRow>,
) -> std::io::Result<usize> {
    out.write_all(sample_header(options).as_bytes())?;
    let mut count = 0;
    for row in rows {
        out.write_all(sample_line(row, options).as_bytes())?;
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregate::Stat;
    use crate::history::Totals;
    use crate::signals::SIGNAL_COUNT;
    use crate::test_util::{amsterdam, ts};

    fn options(signals: SignalSet, format: Format) -> Options {
        Options {
            tz: amsterdam(),
            signals,
            format,
        }
    }

    fn english() -> Options {
        options(SignalSet::all(), Format::English)
    }

    fn number(v: Option<f64>, decimals: usize, format: Format) -> String {
        let mut line = Line::new(format);
        line.number(v, decimals);
        line.text
    }

    fn text(t: &str, format: Format) -> String {
        let mut line = Line::new(format);
        line.text(t);
        line.text
    }

    #[test]
    fn numbers_have_fixed_decimals_and_a_dot() {
        let n = |v, d| number(Some(v), d, Format::English);
        assert_eq!(n(13.284, 3), "13.284");
        assert_eq!(n(13.25, 3), "13.250");
        assert_eq!(n(-1.25, 2), "-1.25");
        assert_eq!(n(24.649_999, 1), "24.6");
        assert_eq!(n(0.1 + 0.2, 2), "0.30");
        assert_eq!(n(1234.6, 0), "1235");
        // No thousands separator.
        assert_eq!(n(12_345.0, 1), "12345.0");
    }

    #[test]
    fn the_dutch_format_has_a_decimal_comma() {
        let n = |v, d| number(Some(v), d, Format::Dutch);
        assert_eq!(n(13.284, 3), "13,284");
        assert_eq!(n(-1.25, 2), "-1,25");
        assert_eq!(n(1234.6, 0), "1235");
        assert_eq!(n(12_345.0, 1), "12345,0", "no thousands separator");
        assert_eq!(n(-0.001, 2), "0,00");
    }

    #[test]
    fn invalid_values_are_empty_cells() {
        for format in [Format::English, Format::Dutch] {
            assert_eq!(number(None, 3, format), "");
            assert_eq!(number(Some(f64::NAN), 3, format), "");
            assert_eq!(number(Some(f64::INFINITY), 3, format), "");
        }
    }

    #[test]
    fn a_value_that_rounds_to_zero_has_no_minus_sign() {
        let n = |v, d| number(Some(v), d, Format::English);
        assert_eq!(n(-0.001, 2), "0.00");
        assert_eq!(n(-0.0, 0), "0");
        assert_eq!(n(-0.004, 2), "0.00");
        assert_eq!(n(-0.005_1, 2), "-0.01");
    }

    #[test]
    fn text_with_the_separator_or_a_quote_is_quoted() {
        assert_eq!(text("UNKNOWN (7)", Format::English), "UNKNOWN (7)");
        assert_eq!(text("a,b", Format::English), "\"a,b\"");
        assert_eq!(text("a,b", Format::Dutch), "a,b");
        assert_eq!(text("a;b", Format::Dutch), "\"a;b\"");
        assert_eq!(text("say \"hi\"", Format::English), "\"say \"\"hi\"\"\"",);
    }

    #[test]
    fn the_format_follows_the_windows_decimal_sign() {
        assert_eq!(Format::for_decimal_sign(","), Format::Dutch);
        assert_eq!(Format::for_decimal_sign(" , "), Format::Dutch);
        assert_eq!(Format::for_decimal_sign("."), Format::English);
        assert_eq!(Format::for_decimal_sign(""), Format::English);
        assert_eq!(Format::default(), Format::English);
    }

    #[test]
    fn sample_header_names_every_signal_with_its_unit() {
        let header = sample_header(&english());
        assert!(header.ends_with("\r\n"));
        let columns: Vec<&str> = header.trim_end().split(',').collect();
        assert_eq!(&columns[..3], ["timestamp_utc", "time_local", "state"]);
        assert_eq!(&columns[3..], SIGNAL_COLUMNS);
        let dutch = sample_header(&options(SignalSet::all(), Format::Dutch));
        assert_eq!(dutch, header.replace(',', ";"));
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
        let en = options(signals, Format::English);
        assert_eq!(
            sample_line(&row("2026-01-15T11:00:00Z"), &en),
            "2026-01-15T11:00:00.000Z,2026-01-15 12:00:00,DISCHARGING,13.284,-1.25,24.6,\r\n"
        );
        assert_eq!(
            sample_line(&row("2026-07-15T11:00:00Z"), &en),
            "2026-07-15T11:00:00.000Z,2026-07-15 13:00:00,DISCHARGING,13.284,-1.25,24.6,\r\n"
        );
    }

    #[test]
    fn a_dutch_sample_line_splits_on_semicolons() {
        // D12 variant C: the one that opens correctly in Dutch Excel.
        let mut values = [None; SIGNAL_COUNT];
        values[0] = Some(13.284);
        values[1] = Some(-1.25);
        values[7] = Some(24.6);
        let signals = SignalSet::only(["pack_v", "current_a", "t_cell_c", "t_air_in_c"]).unwrap();
        let row = SampleRow {
            ts: ts("2026-01-15T11:00:00Z"),
            state: Some("DISCHARGING".into()),
            values,
        };
        assert_eq!(
            sample_line(&row, &options(signals, Format::Dutch)),
            "2026-01-15T11:00:00.000Z;2026-01-15 12:00:00;DISCHARGING;13,284;-1,25;24,6;\r\n"
        );
    }

    #[test]
    fn a_sample_line_has_as_many_cells_as_the_header() {
        let row = SampleRow {
            ts: ts("2026-01-15T11:00:00Z"),
            state: None,
            values: [Some(1.0); SIGNAL_COUNT],
        };
        for format in [Format::English, Format::Dutch] {
            let o = options(SignalSet::all(), format);
            let cells = |s: &str| s.trim_end().split(format.separator()).count();
            assert_eq!(
                cells(&sample_line(&row, &o)),
                cells(&sample_header(&o)),
                "{format:?}"
            );
        }
    }

    #[test]
    fn aggregate_header_has_min_avg_max_per_signal_and_the_totals() {
        let ten = aggregate_header(AggTable::TenMin, &english());
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
        let daily = aggregate_header(AggTable::Daily, &english());
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
        let en = options(signals, Format::English);
        assert_eq!(
            aggregate_line(&point, AggTable::Daily, &en),
            "2026-01-14T23:00:00.000Z,2026-01-15 00:00:00,86400,13.100,13.205,13.300,,,,\
             3.3333,2.5000,44.100,32.750,0.4167\r\n"
        );
        let ten_min = aggregate_line(&point, AggTable::TenMin, &en);
        assert!(
            ten_min.ends_with(",32.750\r\n"),
            "no equiv_cycles: {ten_min}"
        );
        assert_eq!(
            aggregate_line(&point, AggTable::Daily, &options(signals, Format::Dutch)),
            "2026-01-14T23:00:00.000Z;2026-01-15 00:00:00;86400;13,100;13,205;13,300;;;;\
             3,3333;2,5000;44,100;32,750;0,4167\r\n"
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
        let count = write_samples(&mut out, &english(), &rows).unwrap();
        assert_eq!(count, 3);
        let text = String::from_utf8(out).unwrap();
        assert!(text.is_ascii());
        assert!(!text.starts_with("sep="));
        assert_eq!(text.matches("\r\n").count(), 4);
        assert_eq!(text.matches('\n').count(), 4, "every line ends in CRLF");
    }
}

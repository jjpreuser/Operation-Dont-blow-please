//! CSV export from the preview's demo panel (S10.11, S10.13; DD-49, DD-62,
//! DD-101).
//!
//! On the Pi the export is a dashboard download through `api_server`; until
//! that exists the preview writes the same CSV to a folder, so the stored
//! data can be looked at in Excel. The data is the real core's: the ring
//! buffer it fills and the database its `db_writer` thread writes.

use bess_core::csv::{Format, Options};
use bess_core::db::{Store, StoredCounts};
use bess_core::history::{self, Range, SignalSet, Tier, choose_tier};
use bess_core::tasks::hub::SharedRing;
use bess_core::wallclock::add_ms;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

/// The ranges the panel offers: label and length in ms (`None`: everything
/// stored).
pub const SPANS: [(&str, Option<i64>); 7] = [
    ("Last 10 minutes", Some(10 * MINUTE_MS)),
    ("Last hour", Some(60 * MINUTE_MS)),
    ("Last 24 hours", Some(24 * 60 * MINUTE_MS)),
    ("Last 7 days", Some(7 * 24 * 60 * MINUTE_MS)),
    ("Last 30 days", Some(30 * 24 * 60 * MINUTE_MS)),
    ("Last year", Some(365 * 24 * 60 * MINUTE_MS)),
    ("Everything", None),
];

const MINUTE_MS: i64 = 60 * 1000;

/// "Everything" starts here: before any data this app can have written.
const EVERYTHING_FROM: &str = "2000-01-01T00:00:00Z";

/// The tier's name in the panel and in file names.
#[must_use]
pub fn tier_name(tier: Tier) -> &'static str {
    match tier {
        Tier::Ring => "1 s ring buffer",
        Tier::TenMin => "10-min records",
        Tier::Daily => "daily records",
    }
}

fn tier_file(tier: Tier) -> &'static str {
    match tier {
        Tier::Ring => "1s",
        Tier::TenMin => "10min",
        Tier::Daily => "daily",
    }
}

/// Why a tier can be empty, for the panel.
#[must_use]
pub fn empty_hint(tier: Tier) -> &'static str {
    match tier {
        Tier::Ring => {
            "the ring buffer fills from the first sample after the link is ONLINE (not saved across restarts)"
        }
        Tier::TenMin => {
            "a 10-min record is written at each :00, :10, :20 … and when the preview closes"
        }
        Tier::Daily => "a daily record is written at 00:00 local, or at start-up for days missed",
    }
}

/// The range ending at `now` that `span_ms` asks for.
#[must_use]
pub fn range(span_ms: Option<i64>, now: Timestamp) -> Range {
    let from = span_ms.map_or_else(
        || EVERYTHING_FROM.parse().unwrap_or(Timestamp::MIN),
        |ms| add_ms(now, -ms),
    );
    Range { from, to: now }
}

/// The tier a range is read from, as [`choose_tier`] picks it.
#[must_use]
pub fn tier_for(span_ms: Option<i64>, now: Timestamp, coarser: Option<Tier>) -> Tier {
    choose_tier(&range(span_ms, now), now, coarser)
}

/// What an export did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exported {
    /// The tier read; `None` for a fault snapshot.
    pub tier: Option<Tier>,
    pub rows: usize,
    /// The file written; `None` when there were no rows (nothing written).
    pub path: Option<PathBuf>,
}

/// Where the files go and where the data comes from.
#[derive(Debug, Clone)]
pub struct Sources {
    pub ring: SharedRing,
    /// The database file; `None` when the preview runs without one.
    pub db: Option<PathBuf>,
    pub dir: PathBuf,
    pub tz: TimeZone,
    /// The Excel number settings the files are for (DD-62).
    pub format: Format,
}

impl Sources {
    /// Every signal, in the chosen format.
    fn options(&self) -> Options {
        Options {
            tz: self.tz.clone(),
            signals: SignalSet::all(),
            format: self.format,
        }
    }

    fn store(&self) -> Result<Store, String> {
        let db = self
            .db
            .as_ref()
            .ok_or("the preview runs without a database file")?;
        Store::open_read_only(db).map_err(|e| format!("{}: {e}", db.display()))
    }

    /// Row counts of the database, for the panel.
    ///
    /// # Errors
    ///
    /// When the database cannot be opened or read.
    pub fn counts(&self) -> Result<StoredCounts, String> {
        self.store()?.stored_counts().map_err(|e| e.to_string())
    }

    /// Exports the range `span_ms` before `now` (DD-49): the ring buffer,
    /// the 10-min or the daily records, whichever covers it, or `coarser`.
    ///
    /// # Errors
    ///
    /// A message for the panel when the database or the file fails.
    pub fn export(
        &self,
        span_ms: Option<i64>,
        coarser: Option<Tier>,
        now: Timestamp,
    ) -> Result<Exported, String> {
        let range = range(span_ms, now);
        let tier = choose_tier(&range, now, coarser);
        let name = format!("bess-{}-{}.csv", tier_file(tier), stamp(now, &self.tz));
        let options = self.options();
        let mut file = Pending::new(self.dir.join(name));
        let rows = match tier.table() {
            None => {
                // Copied out, so the ring buffer is locked only briefly.
                let rows = history::ring_rows(&self.ring.lock(), &range);
                if !rows.is_empty() {
                    bess_core::csv::write_samples(file.open()?, &options, &rows)
                        .map_err(|e| e.to_string())?;
                }
                rows.len()
            }
            Some(table) => {
                let store = self.store()?;
                if store
                    .points(table, &range)
                    .map_err(|e| e.to_string())?
                    .is_empty()
                {
                    0
                } else {
                    store
                        .write_csv(table, &range, &options, file.open()?)
                        .map_err(|e| e.to_string())?
                }
            }
        };
        Ok(Exported {
            tier: Some(tier),
            rows,
            path: file.finish()?,
        })
    }

    /// Exports the newest fault snapshot (±60 s of 1 s rows, DD-07).
    ///
    /// # Errors
    ///
    /// A message for the panel; also when there is no snapshot yet.
    pub fn export_latest_snapshot(&self, now: Timestamp) -> Result<Exported, String> {
        let store = self.store()?;
        let (id, _) = store
            .latest_snapshot()
            .map_err(|e| e.to_string())?
            .ok_or("no fault snapshot yet: raise a FAULT, for example E05 with a hot cell")?;
        let name = format!("bess-snapshot-{id}-{}.csv", stamp(now, &self.tz));
        let mut file = Pending::new(self.dir.join(name));
        let rows = store
            .write_snapshot_csv(id, &self.options(), file.open()?)
            .map_err(|e| e.to_string())?
            .unwrap_or(0);
        Ok(Exported {
            tier: None,
            rows,
            path: file.finish()?,
        })
    }
}

/// `YYYYMMDD-HHMMSS` in local time, for file names.
fn stamp(now: Timestamp, tz: &TimeZone) -> String {
    tz.to_datetime(now).strftime("%Y%m%d-%H%M%S").to_string()
}

/// A file created only when the first byte is written.
struct Pending {
    path: PathBuf,
    out: Option<BufWriter<File>>,
}

impl Pending {
    fn new(path: PathBuf) -> Pending {
        Pending { path, out: None }
    }

    fn open(&mut self) -> Result<&mut BufWriter<File>, String> {
        if self.out.is_none() {
            if let Some(dir) = self.path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            let file =
                File::create(&self.path).map_err(|e| format!("{}: {e}", self.path.display()))?;
            self.out = Some(BufWriter::new(file));
        }
        Ok(self.out.as_mut().expect("just opened"))
    }

    fn finish(self) -> Result<Option<PathBuf>, String> {
        match self.out {
            None => Ok(None),
            Some(mut out) => {
                out.flush()
                    .map_err(|e| format!("{}: {e}", self.path.display()))?;
                Ok(Some(self.path))
            }
        }
    }
}

/// The format for this PC's Excel. Windows keeps the decimal sign of its
/// number settings as `sDecimal` under `HKCU\Control Panel\International`,
/// and Excel splits a `.csv` by those settings (D12). Elsewhere, or when it
/// cannot be read, the default English format.
#[must_use]
pub fn system_format() -> Format {
    if !cfg!(windows) {
        return Format::default();
    }
    std::process::Command::new("reg")
        .args([
            "query",
            r"HKCU\Control Panel\International",
            "/v",
            "sDecimal",
        ])
        .output()
        .ok()
        .and_then(|out| reg_value(&String::from_utf8_lossy(&out.stdout), "sDecimal"))
        .map_or_else(Format::default, |sign| Format::for_decimal_sign(&sign))
}

/// The value of `name` in the output of `reg query`, whose line reads
/// `    sDecimal    REG_SZ    ,`.
fn reg_value(output: &str, name: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        (words.next()? == name && words.next()? == "REG_SZ")
            .then(|| words.collect::<Vec<_>>().join(" "))
    })
}

/// Opens a file or folder with the desktop's default app (Excel for a
/// `.csv` on a TE laptop, the file manager for a folder).
///
/// # Errors
///
/// When the opener cannot be started.
pub fn open_path(path: &Path) -> Result<(), String> {
    let opener = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(opener)
        .arg(path)
        .spawn()
        .map(drop)
        .map_err(|e| format!("{opener}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bess_core::aggregate::{AggRow, Stat};
    use bess_core::ring::RingEntry;
    use bess_core::signals::{SIGNAL_COUNT, Signals};
    use protocol::messages::McuState;
    use std::time::{Duration, Instant};

    fn ts(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    const NOW: &str = "2026-11-25T12:00:00Z";

    fn sources(dir: &Path, db: Option<PathBuf>) -> Sources {
        Sources {
            ring: SharedRing::new(),
            db,
            dir: dir.join("exports"),
            tz: TimeZone::get("Europe/Amsterdam").unwrap(),
            format: Format::English,
        }
    }

    #[test]
    fn the_dutch_format_splits_on_semicolons() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = sources(dir.path(), Some(db_with_10min(dir.path())));
        s.format = Format::Dutch;
        let done = s.export(SPANS[2].1, None, ts(NOW)).unwrap();
        let text = std::fs::read_to_string(done.path.unwrap()).unwrap();
        assert!(text.starts_with("timestamp_utc;time_local;sample_count;pack_v_min;"));
        assert!(text.contains("2026-11-25 10:10:00;600;13,000;13,100;13,200"));
    }

    #[test]
    fn the_windows_decimal_sign_is_read_from_reg_query() {
        let dutch = "\r\nHKEY_CURRENT_USER\\Control Panel\\International\r\n    sDecimal    REG_SZ    ,\r\n\r\n";
        assert_eq!(reg_value(dutch, "sDecimal").as_deref(), Some(","));
        let english = "    sDecimal    REG_SZ    .";
        assert_eq!(reg_value(english, "sDecimal").as_deref(), Some("."));
        assert_eq!(reg_value("ERROR: not found", "sDecimal"), None);
        assert_eq!(reg_value("    sList    REG_SZ    ;", "sDecimal"), None);
    }

    fn fill_ring(ring: &SharedRing, seconds: u32) {
        let t0 = Instant::now();
        let mut ring = ring.lock();
        for i in 0..seconds {
            let mut signals = Signals::default();
            signals.sample.pack_v = Some(13.2);
            ring.push(RingEntry {
                at: t0 + Duration::from_secs(u64::from(i)),
                utc: Some(add_ms(ts(NOW), -i64::from(seconds - i) * 1000)),
                state: McuState::Charging,
                signals,
            });
        }
    }

    fn db_with_10min(dir: &Path) -> PathBuf {
        let path = dir.join("bess.db");
        let mut store = Store::open(&path).unwrap();
        let mut stats = [Stat::default(); SIGNAL_COUNT];
        stats[0] = Stat {
            min: Some(13.0),
            avg: Some(13.1),
            max: Some(13.2),
        };
        for at in ["2026-11-25T09:00:00Z", "2026-11-25T09:10:00Z"] {
            store
                .insert_10min(&AggRow {
                    ts: ts(at),
                    sample_count: 600,
                    stats,
                    ah_chg: 0.1,
                    ah_dis: 0.0,
                    wh_chg: 1.3,
                    wh_dis: 0.0,
                })
                .unwrap();
        }
        path
    }

    #[test]
    fn the_last_10_minutes_come_from_the_ring_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let s = sources(dir.path(), None);
        fill_ring(&s.ring, 900);
        let done = s.export(SPANS[0].1, None, ts(NOW)).unwrap();
        assert_eq!(done.tier, Some(Tier::Ring));
        assert_eq!(done.rows, 600);
        let path = done.path.unwrap();
        assert!(path.ends_with("bess-1s-20261125-130000.csv"), "{path:?}");
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.starts_with("timestamp_utc,time_local,state,pack_v,"));
        assert_eq!(text.lines().count(), 601);
        assert!(text.contains(",CHARGING,13.200,"));
    }

    #[test]
    fn the_last_24_hours_come_from_the_10min_records() {
        let dir = tempfile::tempdir().unwrap();
        let s = sources(dir.path(), Some(db_with_10min(dir.path())));
        let done = s.export(SPANS[2].1, None, ts(NOW)).unwrap();
        assert_eq!((done.tier, done.rows), (Some(Tier::TenMin), 2));
        let text = std::fs::read_to_string(done.path.unwrap()).unwrap();
        assert!(text.starts_with("timestamp_utc,time_local,sample_count,pack_v_min,"));
        assert!(
            text.contains("2026-11-25T09:10:00.000Z,2026-11-25 10:10:00,600,13.000,13.100,13.200")
        );
    }

    #[test]
    fn nothing_is_written_when_the_range_has_no_rows() {
        let dir = tempfile::tempdir().unwrap();
        let s = sources(dir.path(), Some(db_with_10min(dir.path())));
        // Daily asked for, but no daily record exists yet.
        let done = s.export(SPANS[2].1, Some(Tier::Daily), ts(NOW)).unwrap();
        assert_eq!(
            (done.tier, done.rows, done.path),
            (Some(Tier::Daily), 0, None)
        );
        assert!(!s.dir.exists(), "no empty file, no folder");
    }

    #[test]
    fn without_a_database_only_the_ring_buffer_exports() {
        let dir = tempfile::tempdir().unwrap();
        let s = sources(dir.path(), None);
        assert!(s.export(SPANS[2].1, None, ts(NOW)).is_err());
        assert!(s.export_latest_snapshot(ts(NOW)).is_err());
        assert!(s.counts().is_err());
    }

    #[test]
    fn a_snapshot_is_exported_or_explained() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_with_10min(dir.path());
        let s = sources(dir.path(), Some(db));
        let err = s.export_latest_snapshot(ts(NOW)).unwrap_err();
        assert!(err.starts_with("no fault snapshot yet"), "{err}");
        assert_eq!(s.counts().unwrap().ten_min, 2);
    }

    #[test]
    fn everything_reads_the_daily_records() {
        let now = ts(NOW);
        assert_eq!(tier_for(None, now, None), Tier::Daily);
        assert_eq!(tier_for(SPANS[1].1, now, None), Tier::Ring);
        assert_eq!(tier_for(SPANS[4].1, now, None), Tier::TenMin);
        assert_eq!(tier_for(SPANS[1].1, now, Some(Tier::TenMin)), Tier::TenMin);
    }
}

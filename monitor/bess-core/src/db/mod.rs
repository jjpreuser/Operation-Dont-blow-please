//! The SQLite database `bess.db` (docs/design/database.md, D2;
//! docs/design/data-lifecycle.md, D3; S10.1–S10.8, S10.10).
//!
//! [`Store`] wraps one SQLite connection. The write connection belongs to the
//! `db_writer` thread only (DD-15); every write is its own transaction, so a
//! power loss loses at most the write in progress (S10.1). Opened for writing
//! it sets WAL with `synchronous=FULL` (DD-12) and foreign keys on, and runs
//! the migrations before anything else uses the file (DD-73). Readers (the
//! API, CSV export, backups) open their own read-only connections with
//! [`Store::open_read_only`], which WAL lets run beside the writer.
//!
//! The thread and its message channel come with the tokio runtime; this
//! module is what it calls.

mod aggregates;
mod config;
mod events;
mod maintenance;
mod schema;
mod snapshots;

pub use config::capacity_ah;
pub(crate) use events::state_text;
pub use events::{EventKind, EventSeverity, EventSource, FrameValues, NewEvent, SystemType};
pub use maintenance::DailyReport;
pub use schema::SCHEMA_VERSION;

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::fmt;
use std::path::Path;
use std::time::Duration;

/// The Pi app version stored with every event (DD-36).
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 10-min records are kept this many days (S10.7), and missing daily records
/// are caught up this far back (D3 §4).
pub const KEEP_10MIN_DAYS: i64 = 30;

/// How long a connection waits for a lock before giving up. Only the backup
/// and the writer ever contend, briefly.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// What can go wrong with the database.
#[derive(Debug)]
pub enum StoreError {
    Sqlite(rusqlite::Error),
    /// The file was written by a newer app (a downgrade): it is left alone
    /// rather than written with an older layout.
    SchemaTooNew {
        found: u32,
        supported: u32,
    },
    /// A read-only connection found a schema this build does not know.
    SchemaMismatch {
        found: u32,
        expected: u32,
    },
    /// A date at the very edge of the supported range.
    Time(jiff::Error),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Sqlite(e) => write!(f, "SQLite: {e}"),
            StoreError::SchemaTooNew { found, supported } => write!(
                f,
                "database schema version {found} is newer than this app supports ({supported})"
            ),
            StoreError::SchemaMismatch { found, expected } => {
                write!(f, "database schema version {found}, expected {expected}")
            }
            StoreError::Time(e) => write!(f, "time: {e}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StoreError::Sqlite(e) => Some(e),
            StoreError::Time(e) => Some(e),
            StoreError::SchemaTooNew { .. } | StoreError::SchemaMismatch { .. } => None,
        }
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> StoreError {
        StoreError::Sqlite(e)
    }
}

impl From<jiff::Error> for StoreError {
    fn from(e: jiff::Error) -> StoreError {
        StoreError::Time(e)
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// `core_state` keys (D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreKey {
    /// Written every 60 s, for `LOG_GAP` (DD-30).
    LastAliveTs,
    LastBootCounter,
    LastConfigRevision,
    LastResetRequestId,
    CommErrorTotal,
    FramesLostTotal,
    SohRestPoint,
    LastBackupTs,
    /// The fan switched off for E12 (DD-85).
    FanOff,
}

impl CoreKey {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            CoreKey::LastAliveTs => "last_alive_ts",
            CoreKey::LastBootCounter => "last_boot_counter",
            CoreKey::LastConfigRevision => "last_config_revision",
            CoreKey::LastResetRequestId => "last_reset_request_id",
            CoreKey::CommErrorTotal => "comm_error_total",
            CoreKey::FramesLostTotal => "frames_lost_total",
            CoreKey::SohRestPoint => "soh_rest_point",
            CoreKey::LastBackupTs => "last_backup_ts",
            CoreKey::FanOff => "fan_off",
        }
    }
}

/// One connection to `bess.db`.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens (or creates) the database for writing: WAL, `synchronous=FULL`,
    /// foreign keys on, migrations run. Only the `db_writer` thread does this
    /// (DD-15).
    ///
    /// # Errors
    ///
    /// SQLite errors (file not writable, not a database, a migration failed;
    /// a failed migration leaves the file at the previous version) and
    /// [`StoreError::SchemaTooNew`] for a file from a newer app.
    pub fn open(path: &Path) -> Result<Store> {
        Store::setup(Connection::open(path)?)
    }

    /// An in-memory database with the full schema, for tests and tools. It
    /// has no WAL (SQLite keeps an in-memory database in memory journal
    /// mode).
    ///
    /// # Errors
    ///
    /// SQLite errors.
    pub fn open_in_memory() -> Result<Store> {
        Store::setup(Connection::open_in_memory()?)
    }

    fn setup(mut conn: Connection) -> Result<Store> {
        conn.busy_timeout(BUSY_TIMEOUT)?;
        // journal_mode is stored in the file; the rest is per connection.
        let _mode: String =
            conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        migrate(&mut conn)?;
        Ok(Store { conn })
    }

    /// Opens an existing database read-only, for the API, CSV export and
    /// backups (DD-15). Writes through it fail.
    ///
    /// # Errors
    ///
    /// SQLite errors (no such file) and [`StoreError::SchemaMismatch`] when
    /// the file is not at [`SCHEMA_VERSION`] (not migrated yet, or newer).
    pub fn open_read_only(path: &Path) -> Result<Store> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        let store = Store { conn };
        let found = store.schema_version()?;
        if found != SCHEMA_VERSION {
            return Err(StoreError::SchemaMismatch {
                found,
                expected: SCHEMA_VERSION,
            });
        }
        Ok(store)
    }

    /// `PRAGMA user_version`.
    ///
    /// # Errors
    ///
    /// SQLite errors.
    pub fn schema_version(&self) -> Result<u32> {
        Ok(self
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))?)
    }

    /// `PRAGMA integrity_check` (S10.1, S10.12): `true` when it says `ok`.
    ///
    /// # Errors
    ///
    /// SQLite errors.
    pub fn integrity_ok(&self) -> Result<bool> {
        let result: String = self
            .conn
            .pragma_query_value(None, "integrity_check", |row| row.get(0))?;
        Ok(result == "ok")
    }

    /// Bytes in use: the file size without the free pages. The file does not
    /// shrink without `VACUUM`, which is never run automatically (DD-23), so
    /// the size guard counts this instead.
    ///
    /// # Errors
    ///
    /// SQLite errors.
    pub fn used_bytes(&self) -> Result<u64> {
        let pragma = |name| -> Result<u64> {
            let value: i64 = self.conn.pragma_query_value(None, name, |row| row.get(0))?;
            // Page counts and sizes are never negative.
            Ok(u64::try_from(value).unwrap_or(0))
        };
        let pages = pragma("page_count")?.saturating_sub(pragma("freelist_count")?);
        Ok(pages * pragma("page_size")?)
    }

    /// A `core_state` value.
    ///
    /// # Errors
    ///
    /// SQLite errors.
    pub fn core_state(&self, key: CoreKey) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM core_state WHERE key = ?1",
                [key.as_str()],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Sets a `core_state` value.
    ///
    /// # Errors
    ///
    /// SQLite errors.
    pub fn set_core_state(&mut self, key: CoreKey, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO core_state (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            [key.as_str(), value],
        )?;
        Ok(())
    }

    /// The connection, for tests that check rows directly.
    #[cfg(test)]
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }
}

/// Runs the migrations the file has not had yet, each in its own
/// transaction together with its `user_version` (DD-73).
fn migrate(conn: &mut Connection) -> Result<()> {
    let found: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if found > SCHEMA_VERSION {
        return Err(StoreError::SchemaTooNew {
            found,
            supported: SCHEMA_VERSION,
        });
    }
    for (version, sql) in (1_u32..).zip(schema::migrations()).skip(found as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(&sql)?;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
    }
    Ok(())
}

/// Helpers shared by the tests of the db submodules.
#[cfg(test)]
pub(crate) mod test_util {
    use jiff::tz::TimeZone;
    use jiff::Timestamp;

    pub fn ts(text: &str) -> Timestamp {
        text.parse().expect("valid timestamp")
    }

    pub fn amsterdam() -> TimeZone {
        TimeZone::get("Europe/Amsterdam").expect("tz database has Europe/Amsterdam")
    }

    pub fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_db() -> (TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bess.db");
        (dir, path)
    }

    #[test]
    fn new_file_gets_wal_full_sync_foreign_keys_and_the_schema() {
        let (_dir, path) = temp_db();
        let store = Store::open(&path).unwrap();
        let conn = store.conn();
        let mode: String = conn
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        let sync: i64 = conn
            .pragma_query_value(None, "synchronous", |r| r.get(0))
            .unwrap();
        assert_eq!(sync, 2, "synchronous=FULL");
        let fk: i64 = conn
            .pragma_query_value(None, "foreign_keys", |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 1);
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        assert!(store.integrity_ok().unwrap());
    }

    #[test]
    fn every_d2_table_exists_and_is_strict() {
        let store = Store::open_in_memory().unwrap();
        let tables = [
            "agg_10min",
            "agg_daily",
            "event",
            "mcu_config",
            "snapshot",
            "snapshot_sample",
            "soh",
            "setting",
            "core_state",
            "upload_watermark",
        ];
        for table in tables {
            let strict: i64 = store
                .conn()
                .query_row(
                    "SELECT strict FROM pragma_table_list WHERE name = ?1",
                    [table],
                    |r| r.get(0),
                )
                .unwrap_or_else(|_| panic!("table {table} missing"));
            assert_eq!(strict, 1, "{table} is STRICT");
        }
    }

    #[test]
    fn aggregate_tables_have_45_signal_columns() {
        let store = Store::open_in_memory().unwrap();
        for table in ["agg_10min", "agg_daily"] {
            let count: i64 = store
                .conn()
                .query_row(
                    "SELECT count(*) FROM pragma_table_info(?1)
                     WHERE name LIKE '%\\_min' ESCAPE '\\'
                        OR name LIKE '%\\_avg' ESCAPE '\\'
                        OR name LIKE '%\\_max' ESCAPE '\\'",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 45, "{table}");
        }
    }

    #[test]
    fn strict_tables_refuse_wrong_types() {
        let store = Store::open_in_memory().unwrap();
        let wrong = store.conn().execute(
            "INSERT INTO core_state (key, value) VALUES ('k', x'00')",
            [],
        );
        assert!(wrong.is_err());
    }

    #[test]
    fn reopening_keeps_data_and_does_not_migrate_again() {
        let (_dir, path) = temp_db();
        {
            let mut store = Store::open(&path).unwrap();
            store
                .set_core_state(CoreKey::LastAliveTs, "2026-11-24T10:00:00.000Z")
                .unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(
            store.core_state(CoreKey::LastAliveTs).unwrap().as_deref(),
            Some("2026-11-24T10:00:00.000Z")
        );
        assert!(store.integrity_ok().unwrap());
    }

    #[test]
    fn a_file_from_a_newer_app_is_left_alone() {
        let (_dir, path) = temp_db();
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
                .unwrap();
        }
        match Store::open(&path) {
            Err(StoreError::SchemaTooNew { found, supported }) => {
                assert_eq!((found, supported), (SCHEMA_VERSION + 1, SCHEMA_VERSION));
            }
            other => panic!("expected SchemaTooNew, got {other:?}"),
        }
        // Nothing was created in it.
        let conn = Connection::open(&path).unwrap();
        let tables: i64 = conn
            .query_row("SELECT count(*) FROM sqlite_schema", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tables, 0);
    }

    #[test]
    fn read_only_connection_reads_beside_the_writer_and_cannot_write() {
        let (_dir, path) = temp_db();
        let mut writer = Store::open(&path).unwrap();
        writer.set_core_state(CoreKey::FanOff, "none").unwrap();
        let reader = Store::open_read_only(&path).unwrap();
        assert_eq!(
            reader.core_state(CoreKey::FanOff).unwrap().as_deref(),
            Some("none")
        );
        // The writer keeps writing while the reader is open (WAL).
        writer.set_core_state(CoreKey::FanOff, "fan2").unwrap();
        assert_eq!(
            reader.core_state(CoreKey::FanOff).unwrap().as_deref(),
            Some("fan2")
        );
        let mut reader = reader;
        assert!(reader.set_core_state(CoreKey::FanOff, "fan1").is_err());
    }

    #[test]
    fn read_only_connection_refuses_an_unmigrated_file() {
        let (_dir, path) = temp_db();
        drop(Connection::open(&path).unwrap());
        assert!(matches!(
            Store::open_read_only(&path),
            Err(StoreError::SchemaMismatch { found: 0, .. })
        ));
    }

    #[test]
    fn core_state_overwrites() {
        let mut store = Store::open_in_memory().unwrap();
        assert_eq!(store.core_state(CoreKey::FramesLostTotal).unwrap(), None);
        store.set_core_state(CoreKey::FramesLostTotal, "3").unwrap();
        store.set_core_state(CoreKey::FramesLostTotal, "5").unwrap();
        assert_eq!(
            store
                .core_state(CoreKey::FramesLostTotal)
                .unwrap()
                .as_deref(),
            Some("5")
        );
    }

    #[test]
    fn errors_say_what_happened() {
        let e = StoreError::SchemaTooNew {
            found: 3,
            supported: 1,
        };
        assert_eq!(
            e.to_string(),
            "database schema version 3 is newer than this app supports (1)"
        );
    }
}

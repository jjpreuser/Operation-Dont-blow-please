//! The database schema and its migrations (docs/design/database.md §2;
//! DD-05, DD-06, DD-73).
//!
//! Migration `n` takes `PRAGMA user_version` from `n - 1` to `n`. A migration
//! is never edited once released: a change is a new migration. That is why
//! version 1 builds its signal columns from its own frozen copy of the
//! signal list instead of [`SIGNAL_COLUMNS`](crate::signals::SIGNAL_COLUMNS);
//! a test checks that the two still agree.

use std::fmt::Write as _;

/// The schema version this build writes.
pub const SCHEMA_VERSION: u32 = 1;

/// The signal columns as created by version 1.
pub(crate) const V1_SIGNALS: [&str; 15] = [
    "pack_v",
    "current_a",
    "power_w",
    "cg1_v",
    "cg2_v",
    "cg3_v",
    "cg4_v",
    "t_cell_c",
    "t_terminal_c",
    "t_air_out_c",
    "t_air_in_c",
    "soc_pct",
    "fan_duty_pct",
    "fan1_rpm",
    "fan2_rpm",
];

/// The SQL of every migration, oldest first.
pub(crate) fn migrations() -> Vec<String> {
    vec![v1()]
}

/// `<signal>_min REAL, <signal>_avg REAL, <signal>_max REAL` for every signal.
fn min_avg_max_columns() -> String {
    V1_SIGNALS.iter().fold(String::new(), |mut out, s| {
        let _ = write!(
            out,
            "    {s}_min REAL,\n    {s}_avg REAL,\n    {s}_max REAL,\n"
        );
        out
    })
}

/// `<signal> REAL` for every signal.
fn value_columns() -> String {
    V1_SIGNALS.iter().fold(String::new(), |mut out, s| {
        let _ = writeln!(out, "    {s} REAL,");
        out
    })
}

/// Version 1: every D2 table. All timestamps are UTC text (DD-06), all
/// signals REAL in SI units (DD-72), all tables STRICT (DD-73).
fn v1() -> String {
    let stats = min_avg_max_columns();
    let values = value_columns();
    format!(
        "
CREATE TABLE agg_10min (
    ts TEXT PRIMARY KEY NOT NULL,
    sample_count INTEGER NOT NULL CHECK (sample_count > 0),
{stats}    ah_chg REAL NOT NULL,
    ah_dis REAL NOT NULL,
    wh_chg REAL NOT NULL,
    wh_dis REAL NOT NULL
) STRICT;

CREATE TABLE agg_daily (
    day TEXT PRIMARY KEY NOT NULL,
    ts_start TEXT NOT NULL,
    ts_end TEXT NOT NULL,
    sample_count INTEGER NOT NULL CHECK (sample_count > 0),
{stats}    ah_chg REAL NOT NULL,
    ah_dis REAL NOT NULL,
    wh_chg REAL NOT NULL,
    wh_dis REAL NOT NULL,
    equiv_cycles REAL
) STRICT;

CREATE TABLE snapshot (
    id INTEGER PRIMARY KEY,
    ts_start TEXT NOT NULL,
    ts_end TEXT NOT NULL,
    complete INTEGER NOT NULL DEFAULT 0 CHECK (complete IN (0, 1))
) STRICT;

-- Clustered by snapshot, so deleting one frees whole pages (size guard).
CREATE TABLE snapshot_sample (
    snapshot_id INTEGER NOT NULL REFERENCES snapshot(id) ON DELETE CASCADE,
    ts TEXT NOT NULL,
    state TEXT,
{values}    PRIMARY KEY (snapshot_id, ts)
) STRICT, WITHOUT ROWID;

CREATE TABLE mcu_config (
    boot_counter INTEGER NOT NULL,
    config_revision INTEGER NOT NULL,
    received_ts TEXT NOT NULL,
    config_schema INTEGER NOT NULL,
    fw_version TEXT NOT NULL,
    status INTEGER NOT NULL,
    raw BLOB NOT NULL,
    PRIMARY KEY (boot_counter, config_revision)
) STRICT;

CREATE TABLE event (
    id INTEGER PRIMARY KEY,
    ts TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('FAULT', 'WARNING', 'SYSTEM')),
    code INTEGER CHECK (code BETWEEN 1 AND 32),
    severity TEXT NOT NULL
        CHECK (severity IN ('CRITICAL', 'PROTECTION', 'LATCHED', 'WARNING', 'INFO')),
    source TEXT NOT NULL CHECK (source IN ('MCU', 'PI')),
    system_type TEXT,
{values}    state TEXT,
    validity INTEGER,
    cleared_ts TEXT,
    detail TEXT,
    fw_version TEXT,
    app_version TEXT NOT NULL,
    boot_counter INTEGER,
    config_revision INTEGER,
    snapshot_id INTEGER REFERENCES snapshot(id) ON DELETE SET NULL,
    -- A fault or warning has a code; a system event has a type instead.
    CHECK ((kind = 'SYSTEM') = (system_type IS NOT NULL)),
    CHECK ((kind = 'SYSTEM') = (code IS NULL))
) STRICT;

CREATE INDEX event_ts ON event (ts);
CREATE INDEX event_kind_severity_ts ON event (kind, severity, ts);
-- So deleting a snapshot (ON DELETE SET NULL) does not scan every event.
CREATE INDEX event_snapshot ON event (snapshot_id) WHERE snapshot_id IS NOT NULL;

CREATE TABLE soh (
    ts TEXT PRIMARY KEY NOT NULL,
    soh_pct REAL NOT NULL,
    capacity_ah REAL,
    ah_counted REAL,
    soc_start_pct REAL,
    soc_end_pct REAL,
    rest_start_ts TEXT
) STRICT;

CREATE TABLE setting (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
) STRICT;

CREATE TABLE core_state (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
) STRICT;

CREATE TABLE upload_watermark (
    table_name TEXT PRIMARY KEY NOT NULL,
    last_sent_key TEXT,
    last_attempt_ts TEXT
) STRICT;
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signals::SIGNAL_COLUMNS;

    #[test]
    fn version_1_signals_match_the_current_signal_list() {
        // A new signal needs a new migration, not a change to version 1.
        assert_eq!(V1_SIGNALS, SIGNAL_COLUMNS);
    }

    #[test]
    fn one_migration_per_version() {
        assert_eq!(migrations().len(), SCHEMA_VERSION as usize);
    }
}

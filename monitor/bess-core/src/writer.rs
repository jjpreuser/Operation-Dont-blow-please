//! The `db_writer` thread: the only owner of the SQLite write connection
//! (docs/design/architecture.md §2; DD-12, DD-15).
//!
//! Every task sends it [`DbCommand`]s; it applies them in order, each as its
//! own transaction (S10.1). A failed write is reported on stderr (the
//! systemd journal) and the thread goes on with the next one, so one bad
//! row can never stop the logging. When every sender is gone the thread
//! ends and hands the store back.
//!
//! Commands from one task are applied in the order that task sent them;
//! between tasks the order is the order of arrival.

use crate::aggregate::AggRow;
use crate::db::{CoreKey, NewEvent, Result, Store};
use crate::detector::Write;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use protocol::config::Config;
use protocol::messages::AckStatus;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

/// One thing for the `db_writer` thread to do.
#[derive(Debug, Clone, PartialEq)]
pub enum DbCommand {
    /// A closed 10-min interval (`aggregator`).
    Agg(Box<AggRow>),
    /// A fault, clear or snapshot write (`event_detector`).
    Detector(Write),
    /// A system event (`link_supervisor`, start-up and shutdown).
    Event(NewEvent),
    /// A validated CONFIG and the `CONFIG_ACK` status it got (DD-43).
    McuConfig {
        received: Timestamp,
        config: Box<Config>,
        status: AckStatus,
    },
    /// A `core_state` value (`maintenance`: `last_alive_ts`).
    CoreState { key: CoreKey, value: String },
    /// The daily job (DD-47, `aggregator`); `capacity_ah` from the installed
    /// CONFIG.
    DailyJob {
        now: Timestamp,
        capacity_ah: Option<f64>,
    },
    /// The size guard (`maintenance`, after a snapshot is complete, D3 §4).
    SizeGuard { now: Timestamp },
}

/// What the writer needs besides the commands.
#[derive(Debug, Clone)]
pub struct WriterSettings {
    /// The local time zone for the daily record (DD-95).
    pub tz: TimeZone,
    /// `DB_SIZE_SOFT_LIMIT` (DD-23, proposal 150 MB).
    pub soft_limit_bytes: u64,
}

/// The proposed `DB_SIZE_SOFT_LIMIT` (D2 §4).
pub const DEFAULT_SOFT_LIMIT_BYTES: u64 = 150 * 1024 * 1024;

/// Applies one command.
///
/// # Errors
///
/// The store's errors.
pub fn apply(store: &mut Store, command: &DbCommand, settings: &WriterSettings) -> Result<()> {
    match command {
        DbCommand::Agg(row) => store.insert_10min(row),
        DbCommand::Detector(write) => store.apply(write),
        DbCommand::Event(event) => store.insert_event(event).map(|_| ()),
        DbCommand::McuConfig {
            received,
            config,
            status,
        } => store
            .insert_mcu_config(*received, config, *status)
            .map(|_| ()),
        DbCommand::CoreState { key, value } => store.set_core_state(*key, value),
        DbCommand::DailyJob { now, capacity_ah } => store
            .run_daily_job(*now, &settings.tz, *capacity_ah, settings.soft_limit_bytes)
            .map(|_| ()),
        DbCommand::SizeGuard { now } => store
            .size_guard(*now, settings.soft_limit_bytes)
            .map(|_| ()),
    }
}

/// Starts the `db_writer` thread. Returns its sender and the handle that
/// gives the store back when every sender is dropped.
///
/// # Errors
///
/// Only if the OS cannot start a thread.
pub fn spawn(
    store: Store,
    settings: WriterSettings,
) -> std::io::Result<(Sender<DbCommand>, JoinHandle<Store>)> {
    let (tx, rx) = mpsc::channel();
    let handle = thread::Builder::new()
        .name("db_writer".into())
        .spawn(move || run(store, &rx, &settings))?;
    Ok((tx, handle))
}

fn run(mut store: Store, rx: &Receiver<DbCommand>, settings: &WriterSettings) -> Store {
    for command in rx {
        if let Err(e) = apply(&mut store, &command, settings) {
            eprintln!("db_writer: {e} ({})", describe(&command));
        }
    }
    store
}

fn describe(command: &DbCommand) -> &'static str {
    match command {
        DbCommand::Agg(_) => "10-min row",
        DbCommand::Detector(_) => "event detector write",
        DbCommand::Event(_) => "event",
        DbCommand::McuConfig { .. } => "mcu_config row",
        DbCommand::CoreState { .. } => "core_state",
        DbCommand::DailyJob { .. } => "daily job",
        DbCommand::SizeGuard { .. } => "size guard",
    }
}

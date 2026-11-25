//! `bess-core`: the Pi monitor core service (docs/design/lifecycle.md §1).
//!
//! Start-up: open the database (migrations first, DD-73), close a snapshot
//! a crash left open, read the open faults and the last firmware, start
//! `db_writer`, log `STARTUP` and `LOG_GAP` (DD-30), open the MCU UART and
//! run the core until SIGTERM or Ctrl-C, then shut down cleanly.
//!
//! ```text
//! bess-core [--db PATH] [--serial DEVICE]
//! ```
//!
//! Defaults: `/var/lib/bess/bess.db` and `/dev/ttyAMA0` (DD-88).

use bess_core::db::Store;
use bess_core::runtime::{self, Core, interim_clock, link_pair, startup_events};
use bess_core::writer::{self, DEFAULT_SOFT_LIMIT_BYTES, DbCommand, WriterSettings};
use jiff::tz::TimeZone;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

const DEFAULT_DB: &str = "/var/lib/bess/bess.db";
const DEFAULT_SERIAL: &str = "/dev/ttyAMA0";

struct Args {
    db: PathBuf,
    serial: PathBuf,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        db: PathBuf::from(DEFAULT_DB),
        serial: PathBuf::from(DEFAULT_SERIAL),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--db" => args.db = PathBuf::from(value()?),
            "--serial" => args.serial = PathBuf::from(value()?),
            "--help" | "-h" => {
                return Err("usage: bess-core [--db PATH] [--serial DEVICE]".into());
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(args)
}

fn main() -> ExitCode {
    match start() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("bess-core: {e}");
            ExitCode::FAILURE
        }
    }
}

fn start() -> Result<(), String> {
    let args = parse_args()?;
    let tz = TimeZone::system();

    let mut store = Store::open(&args.db).map_err(|e| format!("{}: {e}", args.db.display()))?;
    let startup = runtime::prepare_store(&mut store).map_err(|e| e.to_string())?;
    if startup.stale_snapshots > 0 {
        eprintln!(
            "bess-core: {} snapshot(s) left open by the last run marked complete",
            startup.stale_snapshots
        );
    }

    let (db, writer) = writer::spawn(
        store,
        WriterSettings {
            tz: tz.clone(),
            soft_limit_bytes: DEFAULT_SOFT_LIMIT_BYTES,
        },
    )
    .map_err(|e| e.to_string())?;
    // Without a valid clock these are not logged yet (the DD-31 queue is
    // still to do).
    if let Some(now) = interim_clock() {
        for event in startup_events(now, startup.last_alive, startup.clean) {
            let _ = db.send(DbCommand::Event(event));
        }
    }

    let (link, end) = link_pair();
    let port = open_port(&args.serial)?;
    let reader_port = port.try_clone().map_err(|e| e.to_string())?;
    runtime::spawn_reader(reader_port, end.from_mcu).map_err(|e| e.to_string())?;
    let tx_thread = runtime::spawn_writer(port, end.to_mcu).map_err(|e| e.to_string())?;

    let core = Core::new(tz, startup.open_faults, startup.last_firmware);
    // `api_server` will hold the live state receiver (DD-14).
    let _live = core.live();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let summary = rt.block_on(core.run(link, db.clone(), Arc::new(interim_clock), stop_signal()));
    eprintln!(
        "bess-core: stopped; link {}, {} frames lost, parser {:?}",
        summary.link, summary.frames_lost, summary.parser
    );
    drop(rt);
    drop(db);
    let _ = tx_thread.join();
    writer
        .join()
        .map_err(|_| "db_writer thread panicked".to_string())?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn open_port(path: &std::path::Path) -> Result<std::fs::File, String> {
    runtime::open_serial(path).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(not(target_os = "linux"))]
fn open_port(path: &std::path::Path) -> Result<std::fs::File, String> {
    Err(format!(
        "{}: the MCU UART is only supported on the Pi (Linux); use the tests or bess-display for a simulated MCU",
        path.display()
    ))
}

/// SIGTERM (systemd stop) or Ctrl-C.
async fn stop_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = term.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

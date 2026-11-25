//! The Pi side of the preview: the real `bess-core` core ([`Core::run`],
//! every task of D1) against the demo MCU, over the UART bytes of a
//! [`link_pair`] instead of a serial port.
//!
//! The screen reads the core's live-state `watch`, as `api_server` will
//! (DD-14). The core writes a real database through the real `db_writer`
//! thread, opened with the same start-up steps as `bess-core` (D5 §1), so
//! the demo panel can export what is stored (S10.11). On the way a thread
//! keeps the newest events for the demo panel, with the text the core logs.
//! Closing the window stops the core cleanly: the open 10-min interval and
//! `SHUTDOWN` are written.

use crate::sim::DemoMcu;
use bess_core::db::Store;
use bess_core::detector::Write;
use bess_core::link::LinkView;
use bess_core::live::LiveState;
use bess_core::runtime::{
    Core, LinkEnd, Startup, Summary, interim_clock, link_pair, prepare_store, startup_events,
};
use bess_core::tasks::hub::{self, SharedRing};
use bess_core::writer::{self, DEFAULT_SOFT_LIMIT_BYTES, DbCommand, WriterSettings};
use jiff::tz::TimeZone;
use protocol::messages::FaultBits;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tokio::sync::{oneshot, watch};

/// Events kept for the demo panel.
const EVENT_LOG_LEN: usize = 8;

/// How often the demo MCU runs.
const MCU_STEP: Duration = Duration::from_millis(100);

/// How long closing the window waits for the core to stop.
const STOP_WAIT: Duration = Duration::from_secs(3);

/// The running preview. Its tasks run on the tokio runtime it was started
/// on.
pub struct Preview {
    pub mcu: Arc<Mutex<DemoMcu>>,
    pub live: watch::Receiver<LiveState>,
    pub link: watch::Receiver<LinkView>,
    /// Newest first: seconds since start and the event text.
    pub events: Arc<Mutex<VecDeque<(f64, String)>>>,
    /// The core's 1 h ring buffer.
    pub ring: SharedRing,
    /// The database file the core writes; `None` when it runs on an
    /// in-memory database.
    pub db: Option<PathBuf>,
    /// Why the database file is not used.
    pub db_error: Option<String>,
    stop: Option<oneshot::Sender<()>>,
    core: Option<tokio::task::JoinHandle<Summary>>,
    writer: Option<JoinHandle<Store>>,
}

impl Preview {
    /// Stops the core as SIGTERM does on the Pi and waits until the
    /// database has everything.
    pub fn stop(&mut self, runtime: &tokio::runtime::Runtime) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let Some(core) = self.core.take() else {
            return;
        };
        // The timer is made inside the runtime, so inside the async block.
        let stopped = runtime.block_on(async { tokio::time::timeout(STOP_WAIT, core).await });
        // Only a stopped core has dropped its queue, so only then does the
        // writer end.
        if matches!(stopped, Ok(Ok(_)))
            && let Some(writer) = self.writer.take()
        {
            let _ = writer.join();
        }
    }
}

/// Starts the core and the demo MCU on the current tokio runtime. With
/// `db` the core writes that database file (created if missing); without
/// it, or when the file cannot be opened, an in-memory one.
///
/// # Panics
///
/// Outside a tokio runtime, or when the OS cannot start a thread.
#[must_use]
pub fn start(db: Option<PathBuf>) -> Preview {
    let tz = TimeZone::system();
    let (store, startup, db, db_error) = open_store(db);
    let (to_writer, writer) = writer::spawn(
        store,
        WriterSettings {
            tz: tz.clone(),
            soft_limit_bytes: DEFAULT_SOFT_LIMIT_BYTES,
        },
    )
    .expect("db_writer thread");
    let events = Arc::new(Mutex::new(VecDeque::new()));
    let db_queue = event_log(events.clone(), to_writer);
    if let Some(now) = interim_clock() {
        for event in startup_events(now, startup.last_alive, startup.clean) {
            let _ = db_queue.send(DbCommand::Event(event));
        }
    }

    let mcu = Arc::new(Mutex::new(DemoMcu::new(hub::now())));
    let (link, end) = link_pair();
    let core = Core::new(tz, startup.open_faults, startup.last_firmware);
    let (live, link_rx, ring) = (core.live(), core.link(), core.ring());
    let (stop, stopped) = oneshot::channel();
    let core = tokio::spawn(core.run(link, db_queue, Arc::new(interim_clock), async {
        let _ = stopped.await;
    }));
    tokio::spawn(run_mcu(mcu.clone(), end));
    Preview {
        mcu,
        live,
        link: link_rx,
        events,
        ring,
        db,
        db_error,
        stop: Some(stop),
        core: Some(core),
        writer: Some(writer),
    }
}

/// The store for the core: the file, or in memory when there is none or it
/// fails (with the reason).
fn open_store(path: Option<PathBuf>) -> (Store, Startup, Option<PathBuf>, Option<String>) {
    let mut error = None;
    if let Some(path) = path {
        match open_file(&path) {
            Ok((store, startup)) => return (store, startup, Some(path), None),
            Err(e) => error = Some(e),
        }
    }
    let mut store = Store::open_in_memory().expect("in-memory database");
    let startup = prepare_store(&mut store).unwrap_or(Startup {
        open_faults: FaultBits(0),
        last_firmware: None,
        last_alive: None,
        clean: false,
        stale_snapshots: 0,
    });
    (store, startup, None, error)
}

fn open_file(path: &Path) -> Result<(Store, Startup), String> {
    let named = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| named(&e))?;
    }
    let mut store = Store::open(path).map_err(|e| named(&e))?;
    let startup = prepare_store(&mut store).map_err(|e| named(&e))?;
    Ok((store, startup))
}

/// Locks a mutex; a panic elsewhere does not stop the preview.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The demo MCU every [`MCU_STEP`]: the bytes from the core in, its frames
/// out.
async fn run_mcu(mcu: Arc<Mutex<DemoMcu>>, mut end: LinkEnd) {
    let mut tick = tokio::time::interval(MCU_STEP);
    loop {
        tick.tick().await;
        let now = hub::now();
        let bytes = {
            let mut mcu = lock(&mcu);
            while let Ok(bytes) = end.to_mcu.try_recv() {
                mcu.receive(&bytes, now);
            }
            mcu.step(now)
        };
        if !bytes.is_empty() && end.from_mcu.send(bytes).await.is_err() {
            return;
        }
    }
}

/// Sits in front of `db_writer`: keeps the event texts for the panel and
/// passes every command on. It ends, and so does the writer, when the core
/// has dropped its queue.
fn event_log(
    events: Arc<Mutex<VecDeque<(f64, String)>>>,
    writer: mpsc::Sender<DbCommand>,
) -> mpsc::Sender<DbCommand> {
    let (db, commands) = mpsc::channel();
    let started = Instant::now();
    std::thread::spawn(move || {
        for command in commands {
            if let Some(text) = describe(&command) {
                let mut events = lock(&events);
                events.push_front((started.elapsed().as_secs_f64(), text));
                events.truncate(EVENT_LOG_LEN);
            }
            if writer.send(command).is_err() {
                return;
            }
        }
    });
    db
}

fn describe(command: &DbCommand) -> Option<String> {
    match command {
        DbCommand::Event(e) | DbCommand::Detector(Write::System(e)) => {
            let ty = e.system_type?.as_str();
            Some(match &e.detail {
                Some(detail) => format!("{ty} {detail}"),
                None => ty.to_string(),
            })
        }
        DbCommand::Detector(Write::Raise { event, .. }) => {
            Some(format!("E{:02} raised", event.code?))
        }
        DbCommand::Detector(Write::Clear { code, .. }) => Some(format!("E{code:02} cleared")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    //! End to end over the UART bytes: the demo MCU and the real core, on
    //! tokio's paused clock.

    use super::{Preview, lock, start};
    use bess_core::fan::Reason;
    use bess_core::link::LinkState;
    use bess_core::live::{LiveState, Values};
    use bess_core::state::ShownState;
    use bess_core::tasks::hub;
    use std::time::Duration;

    /// Runs `ms` milliseconds of virtual time.
    async fn run(p: &Preview, ms: u64) -> LiveState {
        tokio::time::sleep(Duration::from_millis(ms)).await;
        p.live.borrow().clone()
    }

    /// Whether an event starting with `prefix` is logged; the log thread
    /// runs in real time, so give it a moment.
    fn logged(p: &Preview, prefix: &str) -> bool {
        (0..200).any(|_| {
            let found = lock(&p.events).iter().any(|(_, e)| e.starts_with(prefix));
            if !found {
                std::thread::sleep(Duration::from_millis(5));
            }
            found
        })
    }

    #[tokio::test(start_paused = true)]
    async fn handshake_reaches_online_and_the_mcu_hears_its_identity() {
        let p = start(None);
        let v = run(&p, 2500).await;
        assert_eq!(v.link, LinkState::Online);
        assert!(matches!(v.values, Values::Live(..)));
        assert_eq!(v.shown.state, ShownState::Charging);
        let mcu = lock(&p.mcu);
        assert_eq!(mcu.pi_heartbeat(hub::now()), Some(mcu.identity()));
        drop(mcu);
        assert!(logged(&p, "CONFIG_ACCEPTED"));
    }

    #[tokio::test(start_paused = true)]
    async fn cut_link_greys_the_values_and_recovers() {
        let p = start(None);
        run(&p, 2500).await;
        lock(&p.mcu).controls.link_cut = true;
        let v = run(&p, 4000).await;
        assert_eq!(v.link, LinkState::Offline);
        assert!(v.shown.comm_lost);
        assert!(matches!(v.values, Values::Stale(_)));
        lock(&p.mcu).controls.link_cut = false;
        let v = run(&p, 1500).await;
        assert_eq!(v.link, LinkState::Online);
        assert!(matches!(v.values, Values::Live(..)));
        assert_eq!(p.link.borrow().outages, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn mcu_restart_goes_through_wait_config_to_online() {
        let p = start(None);
        run(&p, 2500).await;
        lock(&p.mcu).restart(hub::now());
        let v = run(&p, 2500).await;
        assert_eq!(v.link, LinkState::Online);
        let mcu = lock(&p.mcu);
        assert_eq!(mcu.identity().boot_counter, 2);
        assert_eq!(mcu.pi_heartbeat(hub::now()), Some(mcu.identity()));
        drop(mcu);
        assert!(logged(&p, "MCU_RESTART"));
    }

    #[tokio::test(start_paused = true)]
    async fn e_stop_switches_fans_off_and_shows_the_banner() {
        let p = start(None);
        run(&p, 2500).await;
        lock(&p.mcu).controls.e_stop = true;
        let v = run(&p, 300).await; // FAULT_UPDATE, not the next 1 s frame
        assert_eq!(v.shown.state, ShownState::EStop);
        assert!(v.critical_banner);
        assert_eq!(
            v.fan.map(|f| (f.duty_pct, f.reason)),
            Some((0, Reason::EStop))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn hot_battery_runs_the_fans_at_100_with_e05() {
        let p = start(None);
        run(&p, 2500).await;
        lock(&p.mcu).controls.cell_temp_c = 60.0;
        let v = run(&p, 1500).await;
        assert_eq!(
            v.fan.map(|f| (f.duty_pct, f.reason)),
            Some((100, Reason::E05))
        );
        assert_eq!(v.shown.state, ShownState::Fault);
        assert_eq!(v.faults[0].name, "OVERHEATING");
        assert!(logged(&p, "E05 raised"));
    }

    /// A plain test with its own paused runtime: `Preview::stop` blocks on
    /// the runtime, as the window does when it closes.
    #[test]
    fn the_core_writes_the_database_file_and_stops_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("data").join("bess.db");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .unwrap();
        let mut p = {
            let _entered = rt.enter();
            start(Some(db.clone()))
        };
        assert_eq!(
            (p.db.as_deref(), p.db_error.as_deref()),
            (Some(db.as_path()), None)
        );
        rt.block_on(async { tokio::time::sleep(Duration::from_secs(5)).await });
        assert_eq!(p.live.borrow().link, LinkState::Online);
        assert!(
            p.ring.lock().len() >= 3,
            "the export reads this ring buffer"
        );
        p.stop(&rt);

        let store = bess_core::db::Store::open_read_only(&db).unwrap();
        let counts = store.stored_counts().unwrap();
        // The open 10-min interval is written as a partial row on a clean stop.
        assert_eq!(counts.ten_min, 1);
        assert_eq!(
            store.last_system_type().unwrap().as_deref(),
            Some("SHUTDOWN")
        );
        assert!(
            counts.events >= 3,
            "STARTUP, CONFIG_ACCEPTED, SHUTDOWN: {counts:?}"
        );
    }
}

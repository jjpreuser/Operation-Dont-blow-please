//! The whole Pi chain against a simulated MCU over real UART bytes
//! (docs/design/architecture.md §2, lifecycle.md, data-lifecycle.md,
//! fault-handling.md): every task of D1 with its channels, from the bytes to
//! the database, on tokio's paused clock so a 20-minute run takes
//! milliseconds; the start-up events; the port threads. No serial port, but
//! the same code above it.

mod common;

use bess_core::db::{CoreKey, Store, SystemType};
use bess_core::detector::{DETAIL_CLEARED_WHILE_DOWN, Write};
use bess_core::link::{LinkState, LinkView};
use bess_core::live::{LiveState, Values};
use bess_core::runtime::{self, Core, Summary, link_pair, startup_events};
use bess_core::tasks::hub::Clock;
use bess_core::wallclock::add_ms;
use bess_core::writer::{self, DEFAULT_SOFT_LIMIT_BYTES, DbCommand, WriterSettings};
use common::{BOOT1, amsterdam, bits, telemetry_frame, ts};
use protocol::messages::{AckStatus, FaultBits, McuState, Message, Validity};
use protocol::reference;
use protocol::sim::SimMcu;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// The MCU end ([`SimMcu`]) with a steady battery: TELEMETRY on demand,
/// CONFIG on request.
struct Mcu {
    sim: SimMcu,
    state: McuState,
    faults: FaultBits,
}

impl Mcu {
    fn new() -> Mcu {
        Mcu {
            sim: SimMcu::new(reference::config()),
            state: McuState::Charging,
            faults: FaultBits(0),
        }
    }

    fn telemetry(&mut self) -> Vec<u8> {
        let mut t = telemetry_frame(self.sim.identity());
        t.state = self.state;
        t.reported_faults = self.faults;
        t.cell_mv = [3300; 4];
        t.pack_mv = 13_200;
        t.pack_current_10ma = -200; // 2 A charging
        t.temperature_deci_c = [250, 260, 240, 0];
        t.soc_centi_pct = 5000;
        t.validity = Validity(Validity::DEFINED & !(1 << 9)); // no NTC4
        self.sim.telemetry(t)
    }
}

/// The running core, the simulated MCU and a database file. The MCU sends
/// one TELEMETRY a second of virtual time and answers a `CONFIG_REQUEST` at
/// once, as the real one does.
struct Bench {
    dir: TempDir,
    path: PathBuf,
    mcu: Arc<Mutex<Mcu>>,
    to_core: mpsc::Sender<Vec<u8>>,
    received: Arc<Mutex<Vec<Message>>>,
    commands: Arc<Mutex<Vec<DbCommand>>>,
    t0: Instant,
    live: watch::Receiver<LiveState>,
    link: watch::Receiver<LinkView>,
    stop: oneshot::Sender<()>,
    core: JoinHandle<Summary>,
    writer: std::thread::JoinHandle<Store>,
}

/// What a finished run left.
struct Done {
    _dir: TempDir,
    path: PathBuf,
    store: Store,
    summary: Summary,
    received: Vec<Message>,
    commands: Vec<DbCommand>,
}

impl Bench {
    /// Starts the core with the wall clock at `utc0` now; `clock: false`
    /// keeps it invalid.
    fn start(utc0: &str, clock: bool) -> Bench {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bess.db");
        let mut store = Store::open(&path).unwrap();
        let settings = WriterSettings {
            tz: amsterdam(),
            soft_limit_bytes: DEFAULT_SOFT_LIMIT_BYTES,
        };
        // db_writer, keeping a copy of every command.
        let commands = Arc::new(Mutex::new(Vec::new()));
        let (db, db_rx) = std::sync::mpsc::channel::<DbCommand>();
        let log = commands.clone();
        let writer = std::thread::spawn(move || {
            for command in db_rx {
                writer::apply(&mut store, &command, &settings).unwrap();
                log.lock().unwrap().push(command);
            }
            store
        });

        let (link, mut end) = link_pair();
        let mcu = Arc::new(Mutex::new(Mcu::new()));
        let received = Arc::new(Mutex::new(Vec::new()));
        {
            // A weak sender, so dropping the bench's sender closes the port.
            let (mcu, received, to_core) =
                (mcu.clone(), received.clone(), end.from_mcu.downgrade());
            tokio::spawn(async move {
                while let Some(bytes) = end.to_mcu.recv().await {
                    let heard = mcu.lock().unwrap().sim.receive(&bytes);
                    received.lock().unwrap().extend(heard.messages);
                    if let (false, Some(to_core)) = (heard.reply.is_empty(), to_core.upgrade()) {
                        let _ = to_core.send(heard.reply).await;
                    }
                }
            });
        }

        let t0 = Instant::now();
        let utc0 = ts(utc0);
        let clock: Clock = Arc::new(move || {
            let ms = i64::try_from((Instant::now() - t0).as_millis()).unwrap();
            clock.then(|| add_ms(utc0, ms))
        });
        let core = Core::new(amsterdam(), FaultBits(0), None);
        let (live, link_rx) = (core.live(), core.link());
        let (stop, stop_rx) = oneshot::channel();
        let core = tokio::spawn(core.run(link, db, clock, async {
            let _ = stop_rx.await;
        }));
        Bench {
            dir,
            path,
            mcu,
            to_core: end.from_mcu,
            received,
            commands,
            t0,
            live,
            link: link_rx,
            stop,
            core,
            writer,
        }
    }

    fn at(&self, secs: u64) -> Instant {
        self.t0 + Duration::from_secs(secs)
    }

    fn mcu(&self) -> std::sync::MutexGuard<'_, Mcu> {
        self.mcu.lock().unwrap()
    }

    /// One TELEMETRY a second, from `from` to `to` s.
    async fn run(&self, from: u64, to: u64) {
        for s in from..=to {
            tokio::time::sleep_until(self.at(s)).await;
            let bytes = self.mcu().telemetry();
            self.to_core.send(bytes).await.unwrap();
        }
        self.settle().await;
    }

    /// The MCU sends nothing until `to` s; only the timers run.
    async fn silence(&self, to: u64) {
        tokio::time::sleep_until(self.at(to)).await;
        self.settle().await;
    }

    /// Lets every task handle what it has (10 ms of virtual time).
    async fn settle(&self) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    fn link_state(&self) -> LinkState {
        self.link.borrow().state
    }

    /// SIGTERM: stops the core and the writer.
    async fn finish(self) -> Done {
        self.stop.send(()).unwrap();
        let summary = self.core.await.unwrap();
        drop(self.to_core);
        let store = self.writer.join().unwrap();
        let received = std::mem::take(&mut *self.received.lock().unwrap());
        let commands = std::mem::take(&mut *self.commands.lock().unwrap());
        Done {
            _dir: self.dir,
            path: self.path,
            store,
            summary,
            received,
            commands,
        }
    }
}

impl Done {
    fn q(&self, sql: &str) -> i64 {
        let conn = rusqlite::Connection::open(&self.path).unwrap();
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    fn details(&self, ty: SystemType) -> Vec<String> {
        self.commands
            .iter()
            .filter_map(|c| match c {
                DbCommand::Event(e) | DbCommand::Detector(Write::System(e))
                    if e.system_type == Some(ty) =>
                {
                    e.detail.clone()
                }
                _ => None,
            })
            .collect()
    }
}

#[tokio::test(start_paused = true)]
async fn handshake_reaches_online_and_logs_the_config() {
    let b = Bench::start("2026-11-24T10:00:00Z", true);
    b.run(0, 2).await;
    assert_eq!(b.link_state(), LinkState::Online);
    // Live values on the screen (S2.4), with the fan duty.
    let live = b.live.borrow().clone();
    assert!(matches!(live.values, Values::Live(..)));
    assert!(live.fan.is_some());
    let d = b.finish().await;
    // The MCU got a CONFIG_REQUEST, a CONFIG_ACK (accepted) and then
    // heartbeats with the accepted identity (DD-26).
    assert!(
        d.received
            .iter()
            .any(|m| matches!(m, Message::ConfigRequest))
    );
    assert!(
        d.received
            .iter()
            .any(|m| matches!(m, Message::ConfigAck(a) if a.status == AckStatus::Accepted))
    );
    let Some(Message::Heartbeat(last)) = d
        .received
        .iter()
        .rev()
        .find(|m| matches!(m, Message::Heartbeat(_)))
    else {
        panic!("no heartbeat in {:?}", d.received);
    };
    assert_eq!(last.accepted_boot_counter, BOOT1.boot_counter);
    assert_eq!(last.accepted_config_revision, BOOT1.config_revision);

    assert_eq!(d.q("SELECT count(*) FROM mcu_config WHERE status = 0"), 1);
    assert_eq!(
        d.q("SELECT count(*) FROM event WHERE system_type = 'CONFIG_ACCEPTED'"),
        1
    );
    assert_eq!(
        d.details(SystemType::StateChange),
        vec!["link OFFLINE -> WAIT_CONFIG", "link WAIT_CONFIG -> ONLINE"]
    );
    assert_eq!(d.summary.link, LinkState::Online);
    assert_eq!(d.summary.frames_lost, 0);
}

#[tokio::test(start_paused = true)]
async fn heartbeats_go_out_every_second() {
    let b = Bench::start("2026-11-24T10:00:00Z", true);
    b.run(0, 10).await;
    let d = b.finish().await;
    let heartbeats = d
        .received
        .iter()
        .filter(|m| matches!(m, Message::Heartbeat(_)))
        .count();
    // One at start, one at once after the CONFIG (D5 §1), then 1 s apart.
    assert!((11..=13).contains(&heartbeats), "{heartbeats} heartbeats");
}

#[tokio::test(start_paused = true)]
async fn telemetry_makes_10_minute_rows() {
    let b = Bench::start("2026-11-24T10:00:00Z", true);
    b.run(0, 1205).await;
    let d = b.finish().await;
    // First sample at 1 s (ONLINE from the CONFIG at 0 s): 599 in the 10:00
    // interval, 600 in the 10:10 one; 10:20 written as a partial row at
    // shutdown.
    assert_eq!(d.q("SELECT count(*) FROM agg_10min"), 3);
    assert_eq!(
        d.q("SELECT sample_count FROM agg_10min WHERE ts = '2026-11-24T10:00:00.000Z'"),
        599
    );
    assert_eq!(
        d.q("SELECT sample_count FROM agg_10min WHERE ts = '2026-11-24T10:10:00.000Z'"),
        600
    );
    assert_eq!(
        d.q("SELECT sample_count FROM agg_10min WHERE ts = '2026-11-24T10:20:00.000Z'"),
        6
    );
    // 13.2 V, 2 A charging: 1/3 Ah in a full interval (DD-46); the fan duty
    // of fan_controller is stored too.
    let conn = rusqlite::Connection::open(&d.path).unwrap();
    let (v, a, ah, fan): (f64, f64, f64, Option<f64>) = conn
        .query_row(
            "SELECT pack_v_avg, current_a_avg, ah_chg, fan_duty_pct_avg FROM agg_10min
             WHERE ts = '2026-11-24T10:10:00.000Z'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert!((v - 13.2).abs() < 1e-9 && (a - 2.0).abs() < 1e-9);
    assert!((ah - 600.0 * 2.0 / 3600.0).abs() < 1e-9);
    assert!(fan.is_some());
    // last_alive_ts written every minute (DD-30).
    let alive = d
        .commands
        .iter()
        .filter(|c| {
            matches!(
                c,
                DbCommand::CoreState {
                    key: CoreKey::LastAliveTs,
                    ..
                }
            )
        })
        .count();
    assert!((21..=23).contains(&alive), "{alive} last_alive_ts writes");
}

#[tokio::test(start_paused = true)]
async fn a_fault_makes_an_event_and_a_complete_snapshot() {
    let b = Bench::start("2026-11-24T10:00:00Z", true);
    b.run(0, 100).await;
    {
        let mut mcu = b.mcu();
        mcu.faults = bits(&[5]);
        mcu.state = McuState::Fault;
    }
    b.run(101, 130).await;
    {
        let mut mcu = b.mcu();
        mcu.faults = FaultBits(0);
        mcu.state = McuState::Idle;
    }
    b.run(131, 200).await;
    let d = b.finish().await;

    assert_eq!(
        d.q("SELECT count(*) FROM event WHERE code = 5 AND kind = 'FAULT'"),
        1
    );
    assert_eq!(
        d.q("SELECT count(*) FROM event WHERE code = 5 AND cleared_ts IS NOT NULL"),
        1
    );
    // ±60 s around the fault at 101 s: 41 ..= 161. The 60 s before come
    // from the ring buffer, which already held the 101 s sample.
    assert_eq!(d.q("SELECT count(*) FROM snapshot WHERE complete = 1"), 1);
    assert_eq!(d.q("SELECT count(*) FROM snapshot_sample"), 121);
    assert_eq!(
        d.q("SELECT count(*) FROM event WHERE code = 5 AND snapshot_id IS NOT NULL"),
        1
    );
    // The values at report came from the frame (DD-22).
    assert_eq!(
        d.q("SELECT count(*) FROM event WHERE code = 5 AND pack_v = 13.2"),
        1
    );
    // maintenance ran the size guard after the snapshot was complete.
    let complete = d
        .commands
        .iter()
        .position(|c| *c == DbCommand::Detector(Write::SnapshotComplete))
        .unwrap();
    assert!(
        d.commands[complete..]
            .iter()
            .any(|c| matches!(c, DbCommand::SizeGuard { .. }))
    );
    // The MCU state changes were logged.
    assert!(
        d.details(SystemType::StateChange)
            .contains(&"MCU state CHARGING -> FAULT".to_string())
    );
}

#[tokio::test(start_paused = true)]
async fn a_fault_that_clears_during_a_link_loss_is_closed_on_reconnect() {
    let b = Bench::start("2026-11-24T10:00:00Z", true);
    b.run(0, 10).await;
    b.mcu().faults = bits(&[13]);
    b.run(11, 20).await;
    b.silence(25).await; // more than 3 s: OFFLINE
    assert_eq!(b.link_state(), LinkState::Offline);
    assert!(matches!(b.live.borrow().values, Values::Stale(_)));
    b.mcu().faults = FaultBits(0);
    b.run(26, 30).await;
    assert_eq!(b.link_state(), LinkState::Online, "same identity: DD-27");
    let d = b.finish().await;
    assert!(
        d.details(SystemType::StateChange)
            .contains(&"link ONLINE -> OFFLINE".to_string())
    );
    let conn = rusqlite::Connection::open(&d.path).unwrap();
    let detail: String = conn
        .query_row("SELECT detail FROM event WHERE code = 13", [], |r| r.get(0))
        .unwrap();
    assert_eq!(detail, DETAIL_CLEARED_WHILE_DOWN);
    // The gap is a gap in the samples, not filled (DD-10): 1..=20 and
    // 26..=30.
    assert_eq!(
        d.q("SELECT sample_count FROM agg_10min WHERE ts = '2026-11-24T10:00:00.000Z'"),
        25
    );
    // The sequence jump across the outage is not counted as lost frames.
    assert_eq!(d.summary.frames_lost, 0);
}

#[tokio::test(start_paused = true)]
async fn an_mcu_restart_waits_for_the_new_config() {
    let b = Bench::start("2026-11-24T10:00:00Z", true);
    b.run(0, 5).await;
    b.mcu().sim.restart(); // boot 1 -> 2, CONFIG only on request
    b.run(6, 8).await;
    assert_eq!(b.link_state(), LinkState::Online);
    let d = b.finish().await;
    assert_eq!(d.details(SystemType::McuRestart), vec!["boot 1 -> 2"]);
    assert_eq!(d.q("SELECT count(*) FROM mcu_config"), 2);
}

#[tokio::test(start_paused = true)]
async fn lost_frames_are_counted_and_logged() {
    let b = Bench::start("2026-11-24T10:00:00Z", true);
    b.run(0, 5).await;
    // Three frames lost on the wire: their sequence numbers are skipped.
    for _ in 0..3 {
        let _ = b.mcu().telemetry();
    }
    b.run(6, 8).await;
    let d = b.finish().await;
    assert_eq!(d.summary.frames_lost, 3);
    assert_eq!(d.details(SystemType::FramesLost), vec!["3 frames lost"]);
}

#[tokio::test(start_paused = true)]
async fn shutdown_writes_the_partial_row_and_shutdown_last() {
    let b = Bench::start("2026-11-24T10:00:00Z", true);
    b.run(0, 30).await;
    let d = b.finish().await;
    assert_eq!(
        d.q("SELECT sample_count FROM agg_10min WHERE ts = '2026-11-24T10:00:00.000Z'"),
        30
    );
    assert_eq!(
        d.q("SELECT count(*) FROM event WHERE system_type = 'SHUTDOWN'"),
        1
    );
    assert_eq!(
        d.store.last_system_type().unwrap().as_deref(),
        Some("SHUTDOWN")
    );
    // SHUTDOWN and last_alive_ts are the last two commands (D5 §1).
    let n = d.commands.len();
    assert!(
        matches!(&d.commands[n - 2], DbCommand::Event(e) if e.system_type == Some(SystemType::Shutdown))
    );
    assert!(matches!(
        d.commands[n - 1],
        DbCommand::CoreState {
            key: CoreKey::LastAliveTs,
            ..
        }
    ));
    assert!(d.store.core_state(CoreKey::LastAliveTs).unwrap().is_some());
}

#[tokio::test(start_paused = true)]
async fn without_a_valid_clock_nothing_is_timestamped() {
    let b = Bench::start("2026-11-24T10:00:00Z", false);
    b.run(0, 5).await;
    // The link still works and the screen still gets live values.
    assert_eq!(b.link_state(), LinkState::Online);
    assert!(matches!(b.live.borrow().values, Values::Live(..)));
    let d = b.finish().await;
    assert!(
        d.commands.is_empty(),
        "DD-11: no record without a valid clock"
    );
    assert!(
        d.received
            .iter()
            .any(|m| matches!(m, Message::Heartbeat(_)))
    );
}

#[tokio::test(start_paused = true)]
async fn the_daily_job_runs_at_the_first_valid_clock_and_after_midnight() {
    let b = Bench::start("2026-01-15T22:59:50Z", true); // 23:59:50 local
    let daily = |b: &Bench| {
        b.commands
            .lock()
            .unwrap()
            .iter()
            .filter(|c| matches!(c, DbCommand::DailyJob { .. }))
            .count()
    };
    // The writer thread runs in real time: give it a moment.
    let settled = |b: &Bench, n: usize| {
        for _ in 0..200 {
            if daily(b) >= n {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        daily(b)
    };
    b.silence(0).await;
    assert_eq!(settled(&b, 1), 1, "catch-up at the first valid clock");
    b.silence(14).await; // up to 00:00:04 local
    assert_eq!(settled(&b, 2), 1);
    b.silence(16).await; // 00:00:05 local and after
    assert_eq!(settled(&b, 2), 2);
    let d = b.finish().await;
    let Some(DbCommand::DailyJob { now, .. }) = d
        .commands
        .iter()
        .rfind(|c| matches!(c, DbCommand::DailyJob { .. }))
    else {
        unreachable!()
    };
    assert_eq!(*now, ts("2026-01-15T23:00:05Z"));
}

#[tokio::test(start_paused = true)]
async fn a_closed_port_stops_the_core() {
    let b = Bench::start("2026-11-24T10:00:00Z", true);
    b.run(0, 3).await;
    // The port reader thread ends (a port error): its sender goes. No stop
    // signal is given.
    let Bench {
        to_core,
        core,
        writer,
        stop: _stop,
        ..
    } = b;
    drop(to_core);
    let summary = core.await.unwrap();
    assert_eq!(summary.link, LinkState::Online);
    let store = writer.join().unwrap();
    assert_eq!(
        store.last_system_type().unwrap().as_deref(),
        Some("SHUTDOWN")
    );
}

#[test]
fn startup_events_tell_clean_crash_and_power_loss_apart() {
    let now = ts("2026-11-24T10:00:00Z");
    let first = startup_events(now, None, false);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].system_type, Some(SystemType::Startup));

    let kind = |last: &str, clean: bool| -> String {
        let events = startup_events(now, Some(ts(last)), clean);
        assert_eq!(events[1].system_type, Some(SystemType::LogGap));
        events[1].detail.clone().unwrap()
    };
    assert!(kind("2026-11-24T09:59:30Z", true).starts_with("clean:"));
    assert!(kind("2026-11-24T09:59:30Z", false).starts_with("crash:"));
    assert!(kind("2026-11-24T09:00:00Z", false).starts_with("power loss:"));
    assert_eq!(
        kind("2026-11-24T09:00:00Z", false),
        "power loss: nothing logged from 2026-11-24T09:00:00.000Z to 2026-11-24T10:00:00.000Z"
    );
}

/// The reader and writer threads move bytes between a port and the tasks.
#[test]
fn the_port_threads_move_the_bytes() {
    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let (to_task, mut from_port) = mpsc::channel(8);
    let reader = runtime::spawn_reader(std::io::Cursor::new(vec![1_u8, 2, 3]), to_task).unwrap();
    assert_eq!(from_port.blocking_recv(), Some(vec![1, 2, 3]));
    drop(from_port);
    reader.join().unwrap();

    let sink = Sink::default();
    let (to_port, from_task) = mpsc::channel(8);
    let writer = runtime::spawn_writer(sink.clone(), from_task).unwrap();
    to_port.blocking_send(vec![0xAA, 0x55]).unwrap();
    to_port.blocking_send(vec![0x03]).unwrap();
    drop(to_port);
    writer.join().unwrap();
    assert_eq!(*sink.0.lock().unwrap(), vec![0xAA, 0x55, 0x03]);
}

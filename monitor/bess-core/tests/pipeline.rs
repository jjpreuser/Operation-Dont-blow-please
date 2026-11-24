//! The whole Pi chain against a simulated MCU over real UART bytes
//! (docs/design/lifecycle.md, data-lifecycle.md, fault-handling.md): the
//! core pipeline from bytes to the database, the start-up events, and the
//! async runtime with the port threads. No serial port, but the same code
//! above it.

mod common;

use bess_core::db::{CoreKey, Store, SystemType};
use bess_core::detector::{Write, DETAIL_CLEARED_WHILE_DOWN};
use bess_core::link::LinkState;
use bess_core::live::Values;
use bess_core::pipeline::{startup_events, Core, DbCommand, Effects};
use bess_core::runtime::{self, link_pair};
use bess_core::wallclock::add_ms;
use bess_core::writer::{self, WriterSettings, DEFAULT_SOFT_LIMIT_BYTES};
use common::{config_with, telemetry_frame, BOOT1};
use jiff::tz::TimeZone;
use jiff::Timestamp;
use protocol::config::Config;
use protocol::frame::{Direction, SequenceCounter};
use protocol::messages::{AckStatus, FaultBits, FaultCode, Identity, McuState, Message, Validity};
use protocol::parser::FrameParser;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn amsterdam() -> TimeZone {
    TimeZone::get("Europe/Amsterdam").unwrap()
}

fn ts(text: &str) -> Timestamp {
    text.parse().unwrap()
}

fn bits(codes: &[u8]) -> FaultBits {
    FaultBits(
        codes
            .iter()
            .map(|&c| FaultCode::new(c).unwrap().mask())
            .fold(0, |a, m| a | m),
    )
}

/// A simulated MCU: TELEMETRY on demand, CONFIG on request.
struct Mcu {
    sequence: SequenceCounter,
    parser: FrameParser,
    identity: Identity,
    config: Config,
    state: McuState,
    faults: FaultBits,
}

impl Mcu {
    fn new() -> Mcu {
        Mcu {
            sequence: SequenceCounter::new(0),
            parser: FrameParser::new(Direction::PiToMcu),
            identity: BOOT1,
            config: config_with(BOOT1, |_| {}),
            state: McuState::Charging,
            faults: FaultBits(0),
        }
    }

    fn telemetry(&mut self) -> Vec<u8> {
        let mut t = telemetry_frame(self.identity);
        t.state = self.state;
        t.reported_faults = self.faults;
        t.cell_mv = [3300; 4];
        t.pack_mv = 13_200;
        t.pack_current_10ma = -200; // 2 A charging
        t.temperature_deci_c = [250, 260, 240, 0];
        t.soc_centi_pct = 5000;
        t.validity = Validity(Validity::DEFINED & !(1 << 9)); // no NTC4
        Message::Telemetry(t).encode(self.sequence.next_sequence())
    }

    fn config(&mut self) -> Vec<u8> {
        Message::Config(Box::new(self.config.to_bytes())).encode(self.sequence.next_sequence())
    }

    fn receive(&mut self, bytes: &[u8]) -> Vec<Message> {
        self.parser
            .push(bytes)
            .iter()
            .filter_map(|f| Message::try_from(f).ok())
            .collect()
    }
}

/// Core, MCU and a database file, stepped one TELEMETRY a second.
struct Bench {
    _dir: TempDir,
    path: PathBuf,
    core: Core,
    mcu: Mcu,
    t0: Instant,
    utc0: Timestamp,
    store: Store,
    settings: WriterSettings,
    /// The time of the current step.
    now: Instant,
    utc: Timestamp,
    /// What the MCU received and what went to the database, in order.
    received: Vec<Message>,
    commands: Vec<DbCommand>,
}

impl Bench {
    fn new(utc0: &str) -> Bench {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bess.db");
        let t0 = Instant::now();
        Bench {
            store: Store::open(&path).unwrap(),
            _dir: dir,
            path,
            core: Core::new(t0, amsterdam(), FaultBits(0), None),
            mcu: Mcu::new(),
            t0,
            utc0: ts(utc0),
            settings: WriterSettings {
                tz: amsterdam(),
                soft_limit_bytes: DEFAULT_SOFT_LIMIT_BYTES,
            },
            now: t0,
            utc: ts(utc0),
            received: Vec::new(),
            commands: Vec::new(),
        }
    }

    fn set_time(&mut self, secs: u64) {
        self.now = self.t0 + Duration::from_secs(secs);
        self.utc = add_ms(self.utc0, i64::try_from(secs * 1000).unwrap());
    }

    /// Hands the core's output to the MCU and the database; answers a
    /// `CONFIG_REQUEST` at once, as the MCU does.
    fn apply(&mut self, fx: Effects) {
        let messages = self.mcu.receive(&fx.to_mcu);
        let requested = messages.iter().any(|m| matches!(m, Message::ConfigRequest));
        self.received.extend(messages);
        for command in &fx.db {
            writer::apply(&mut self.store, command, &self.settings).unwrap();
        }
        self.commands.extend(fx.db);
        if requested {
            let bytes = self.mcu.config();
            let fx = self.core.on_bytes(self.now, Some(self.utc), &bytes);
            self.apply(fx);
        }
    }

    fn telemetry(&mut self, secs: u64) {
        self.set_time(secs);
        let bytes = self.mcu.telemetry();
        let fx = self.core.on_bytes(self.now, Some(self.utc), &bytes);
        self.apply(fx);
    }

    fn run(&mut self, from: u64, to: u64) {
        for s in from..=to {
            self.telemetry(s);
        }
    }

    /// The MCU sends nothing; only the core's timers run.
    fn silence(&mut self, from: u64, to: u64) {
        for s in from..=to {
            self.set_time(s);
            let fx = self.core.poll(self.now, Some(self.utc));
            self.apply(fx);
        }
    }

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

#[test]
fn handshake_reaches_online_and_logs_the_config() {
    let mut b = Bench::new("2026-11-24T10:00:00Z");
    b.run(0, 2);
    assert_eq!(b.core.link_state(), LinkState::Online);
    // The MCU got a CONFIG_REQUEST, a CONFIG_ACK (accepted) and then
    // heartbeats with the accepted identity (DD-26).
    assert!(b
        .received
        .iter()
        .any(|m| matches!(m, Message::ConfigRequest)));
    assert!(b
        .received
        .iter()
        .any(|m| matches!(m, Message::ConfigAck(a) if a.status == AckStatus::Accepted)));
    let Some(Message::Heartbeat(last)) = b
        .received
        .iter()
        .rev()
        .find(|m| matches!(m, Message::Heartbeat(_)))
    else {
        panic!("no heartbeat in {:?}", b.received);
    };
    assert_eq!(last.accepted_boot_counter, BOOT1.boot_counter);
    assert_eq!(last.accepted_config_revision, BOOT1.config_revision);

    assert_eq!(b.q("SELECT count(*) FROM mcu_config WHERE status = 0"), 1);
    assert_eq!(
        b.q("SELECT count(*) FROM event WHERE system_type = 'CONFIG_ACCEPTED'"),
        1
    );
    assert_eq!(
        b.details(SystemType::StateChange),
        vec!["link OFFLINE -> WAIT_CONFIG", "link WAIT_CONFIG -> ONLINE"]
    );
    // Live values on the screen (S2.4).
    assert!(matches!(b.core.live().values, Values::Live(..)));
}

#[test]
fn telemetry_makes_10_minute_rows() {
    let mut b = Bench::new("2026-11-24T10:00:00Z");
    b.run(0, 1205);
    // First sample at 1 s (ONLINE from the CONFIG at 0 s): 599 in the 10:00
    // interval, 600 in the 10:10 one; 10:20 is still open.
    assert_eq!(b.q("SELECT count(*) FROM agg_10min"), 2);
    assert_eq!(
        b.q("SELECT sample_count FROM agg_10min WHERE ts = '2026-11-24T10:00:00.000Z'"),
        599
    );
    assert_eq!(
        b.q("SELECT sample_count FROM agg_10min WHERE ts = '2026-11-24T10:10:00.000Z'"),
        600
    );
    // 13.2 V, 2 A charging: 1/3 Ah in a full interval (DD-46); the fan duty
    // of the core is stored too.
    let conn = rusqlite::Connection::open(&b.path).unwrap();
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
    // last_alive_ts written (DD-30).
    let alive: String = conn
        .query_row(
            "SELECT value FROM core_state WHERE key = 'last_alive_ts'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(alive.starts_with("2026-11-24T10:"));
}

#[test]
fn a_fault_makes_an_event_and_a_complete_snapshot() {
    let mut b = Bench::new("2026-11-24T10:00:00Z");
    b.run(0, 100);
    b.mcu.faults = bits(&[5]);
    b.mcu.state = McuState::Fault;
    b.run(101, 130);
    b.mcu.faults = FaultBits(0);
    b.mcu.state = McuState::Idle;
    b.run(131, 200);

    assert_eq!(
        b.q("SELECT count(*) FROM event WHERE code = 5 AND kind = 'FAULT'"),
        1
    );
    assert_eq!(
        b.q("SELECT count(*) FROM event WHERE code = 5 AND cleared_ts IS NOT NULL"),
        1
    );
    // ±60 s around the fault at 101 s: 41 ..= 161.
    assert_eq!(b.q("SELECT count(*) FROM snapshot WHERE complete = 1"), 1);
    assert_eq!(b.q("SELECT count(*) FROM snapshot_sample"), 121);
    assert_eq!(
        b.q("SELECT count(*) FROM event WHERE code = 5 AND snapshot_id IS NOT NULL"),
        1
    );
    // The values at report came from the frame (DD-22).
    assert_eq!(
        b.q("SELECT count(*) FROM event WHERE code = 5 AND pack_v = 13.2"),
        1
    );
    // The size guard ran after the snapshot.
    assert!(b
        .commands
        .iter()
        .any(|c| matches!(c, DbCommand::SizeGuard { .. })));
    // The MCU state changes were logged.
    assert!(b
        .details(SystemType::StateChange)
        .contains(&"MCU state CHARGING -> FAULT".to_string()));
}

#[test]
fn a_fault_that_clears_during_a_link_loss_is_closed_on_reconnect() {
    let mut b = Bench::new("2026-11-24T10:00:00Z");
    b.run(0, 10);
    b.mcu.faults = bits(&[13]);
    b.run(11, 20);
    b.silence(21, 25); // more than 3 s: OFFLINE
    assert_eq!(b.core.link_state(), LinkState::Offline);
    assert!(b
        .details(SystemType::StateChange)
        .contains(&"link ONLINE -> OFFLINE".to_string()));
    b.mcu.faults = FaultBits(0);
    b.run(26, 30);
    assert_eq!(b.core.link_state(), LinkState::Online);
    let conn = rusqlite::Connection::open(&b.path).unwrap();
    let detail: String = conn
        .query_row("SELECT detail FROM event WHERE code = 13", [], |r| r.get(0))
        .unwrap();
    assert_eq!(detail, DETAIL_CLEARED_WHILE_DOWN);
    // The 10 s gap is a gap in the samples, not filled (DD-10).
    assert_eq!(b.q("SELECT count(*) FROM agg_10min"), 0);
}

#[test]
fn an_mcu_restart_waits_for_the_new_config() {
    let mut b = Bench::new("2026-11-24T10:00:00Z");
    b.run(0, 5);
    b.mcu.identity = Identity {
        boot_counter: 2,
        config_revision: 1,
    };
    b.mcu.config = config_with(b.mcu.identity, |_| {});
    b.run(6, 8);
    assert_eq!(b.core.link_state(), LinkState::Online);
    assert_eq!(b.details(SystemType::McuRestart), vec!["boot 1 -> 2"]);
    assert_eq!(b.q("SELECT count(*) FROM mcu_config"), 2);
}

#[test]
fn shutdown_writes_the_partial_row_and_shutdown() {
    let mut b = Bench::new("2026-11-24T10:00:00Z");
    b.run(0, 30);
    let fx = b.core.shutdown(Some(add_ms(b.utc, 500)));
    b.apply(fx);
    assert_eq!(
        b.q("SELECT sample_count FROM agg_10min WHERE ts = '2026-11-24T10:00:00.000Z'"),
        30
    );
    assert_eq!(
        b.q("SELECT count(*) FROM event WHERE system_type = 'SHUTDOWN'"),
        1
    );
    assert_eq!(
        b.store.last_system_type().unwrap().as_deref(),
        Some("SHUTDOWN")
    );
    assert!(b.store.core_state(CoreKey::LastAliveTs).unwrap().is_some());
}

#[test]
fn without_a_valid_clock_nothing_is_timestamped() {
    let t0 = Instant::now();
    let mut core = Core::new(t0, amsterdam(), FaultBits(0), None);
    let mut mcu = Mcu::new();
    let mut to_mcu = Vec::new();
    for s in 0..5 {
        let now = t0 + Duration::from_secs(s);
        let fx = core.on_bytes(now, None, &mcu.telemetry());
        assert!(fx.db.is_empty(), "DD-11: no record without a valid clock");
        to_mcu.extend(mcu.receive(&fx.to_mcu));
        if to_mcu.iter().any(|m| matches!(m, Message::ConfigRequest)) {
            let fx = core.on_bytes(now, None, &mcu.config());
            assert!(fx.db.is_empty());
            to_mcu.extend(mcu.receive(&fx.to_mcu));
        }
    }
    // The link still works and the screen still gets live values.
    assert_eq!(core.link_state(), LinkState::Online);
    assert!(to_mcu.iter().any(|m| matches!(m, Message::Heartbeat(_))));
    assert!(matches!(core.live().values, Values::Live(..)));
}

#[test]
fn the_daily_job_runs_at_the_first_valid_clock_and_after_midnight() {
    let mut b = Bench::new("2026-01-15T22:59:50Z"); // 23:59:50 local
    let daily = |b: &Bench| {
        b.commands
            .iter()
            .filter(|c| matches!(c, DbCommand::DailyJob { .. }))
            .count()
    };
    b.silence(0, 0);
    assert_eq!(daily(&b), 1, "catch-up at the first valid clock");
    b.silence(1, 14); // up to 00:00:04 local
    assert_eq!(daily(&b), 1);
    b.silence(15, 16); // 00:00:05 local and after
    assert_eq!(daily(&b), 2);
    let DbCommand::DailyJob { now, .. } = b
        .commands
        .iter()
        .rfind(|c| matches!(c, DbCommand::DailyJob { .. }))
        .unwrap()
    else {
        unreachable!()
    };
    assert_eq!(*now, ts("2026-01-15T23:00:05Z"));
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

/// The async loop with the port threads, against the simulated MCU.
#[tokio::test]
async fn the_runtime_answers_the_mcu_and_shuts_down_cleanly() {
    let (link, mut end) = link_pair();
    let (db_tx, db_rx) = std::sync::mpsc::channel();
    let core = Core::new(Instant::now(), amsterdam(), FaultBits(0), None);
    let (live, live_rx) = tokio::sync::watch::channel(core.live());
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(runtime::run(
        core,
        link,
        db_tx,
        live,
        || Some(Timestamp::now()),
        async {
            let _ = stop_rx.await;
        },
    ));

    let mut mcu = Mcu::new();
    let mut online = false;
    for _ in 0..30 {
        end.from_mcu.send(mcu.telemetry()).await.unwrap();
        let Ok(Some(bytes)) =
            tokio::time::timeout(Duration::from_millis(200), end.to_mcu.recv()).await
        else {
            continue;
        };
        let messages = mcu.receive(&bytes);
        if messages.iter().any(|m| matches!(m, Message::ConfigRequest)) {
            end.from_mcu.send(mcu.config()).await.unwrap();
        }
        if messages.iter().any(
            |m| matches!(m, Message::Heartbeat(h) if h.accepted_boot_counter == BOOT1.boot_counter),
        ) {
            online = true;
            break;
        }
    }
    assert!(online, "heartbeat with the accepted identity");
    assert!(matches!(live_rx.borrow().values, Values::Live(..)));

    stop_tx.send(()).unwrap();
    let core = task.await.unwrap();
    assert_eq!(core.link_state(), LinkState::Online);
    let commands: Vec<DbCommand> = db_rx.try_iter().collect();
    assert!(commands.iter().any(
        |c| matches!(c, DbCommand::Event(e) if e.system_type == Some(SystemType::ConfigAccepted))
    ));
    assert!(matches!(
        commands.iter().rfind(|c| matches!(c, DbCommand::Event(_))),
        Some(DbCommand::Event(e)) if e.system_type == Some(SystemType::Shutdown)
    ));
}

/// The reader and writer threads move bytes between a port and the task.
#[test]
fn the_port_threads_move_the_bytes() {
    use std::sync::{Arc, Mutex};

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

    let (to_task, mut from_port) = tokio::sync::mpsc::channel(8);
    let reader = runtime::spawn_reader(std::io::Cursor::new(vec![1_u8, 2, 3]), to_task).unwrap();
    assert_eq!(from_port.blocking_recv(), Some(vec![1, 2, 3]));
    drop(from_port);
    reader.join().unwrap();

    let sink = Sink::default();
    let (to_port, from_task) = tokio::sync::mpsc::channel(8);
    let writer = runtime::spawn_writer(sink.clone(), from_task).unwrap();
    to_port.blocking_send(vec![0xAA, 0x55]).unwrap();
    to_port.blocking_send(vec![0x03]).unwrap();
    drop(to_port);
    writer.join().unwrap();
    assert_eq!(*sink.0.lock().unwrap(), vec![0xAA, 0x55, 0x03]);
}

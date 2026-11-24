//! The running core: the tasks of docs/design/architecture.md §2 started,
//! wired and stopped (docs/design/lifecycle.md §1; DD-70, DD-98, DD-99).
//!
//! - The MCU is on the Pi's PL011 UART pins (GPIO14/15, `/dev/ttyAMA0`,
//!   DD-88), one fixed device: `open_serial` (Linux only) sets it to raw 115200 8N1
//!   with `rustix` termios (DD-98). A reader and a writer thread move the
//!   bytes between the port and the `uart_rx` / `uart_tx` tasks, so no task
//!   blocks on the port.
//! - [`Core::run`] spawns the tasks of [`crate::tasks`] with their channels
//!   and runs `uart_rx` itself. Shutdown is a chain: `uart_rx` stops, so
//!   `link_supervisor` sees its queue close and stops, which closes the link
//!   watch and both broadcasts, so the other tasks write what they hold (the
//!   partial 10-min row, the open snapshot) and stop. Then `SHUTDOWN` and
//!   `last_alive_ts` are written last (D5 §1).
//!
//! The byte link is two channels, so tests run the same tasks against a
//! simulated MCU without a serial port.

use crate::db::{CoreKey, NewEvent, SystemType, APP_VERSION};
use crate::link::{LinkState, LinkView};
use crate::live::LiveState;
use crate::tasks::aggregator::{self, AggregatorIo, AggregatorTask};
use crate::tasks::detector::{self, DetectorIo, DetectorTask};
use crate::tasks::fan::{self, FanIo, FanTask};
use crate::tasks::hub::{self, Clock, SharedRing, BROADCAST, QUEUE};
use crate::tasks::link::{self as link_task, LinkIo, LinkTask};
use crate::tasks::live::{self, LiveIo, LiveTask};
use crate::tasks::maintenance::{self, MaintenanceIo, MaintenanceTask};
use crate::tasks::uart::{self, UartRx, UartTx};
use crate::wallclock::utc_text;
use crate::writer::DbCommand;
use jiff::tz::TimeZone;
use jiff::Timestamp;
use protocol::config::FirmwareVersion;
use protocol::messages::FaultBits;
use protocol::parser::ParserStats;
use std::future::Future;
use std::io::{Read, Write};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use tokio::sync::{broadcast, mpsc as tmpsc, watch};

/// Bytes per read from the port: about 20 ms at 115200 baud.
const READ_CHUNK: usize = 256;

/// Channel depth between the port threads and the tasks.
const LINK_QUEUE: usize = 64;

/// The year this build was made: an RTC or system clock before it is not
/// trusted (DD-79).
pub const BUILD_YEAR: i16 = 2026;

/// A restart within this time of the last `last_alive_ts` without a
/// `SHUTDOWN` is a crash, later a power loss (DD-30).
pub const CRASH_WITHIN_MS: i64 = 2 * 60 * 1000;

/// The byte link to the MCU, as seen by the tasks.
#[derive(Debug)]
pub struct Link {
    pub from_mcu: tmpsc::Receiver<Vec<u8>>,
    pub to_mcu: tmpsc::Sender<Vec<u8>>,
}

/// The other end of a [`Link`]: what the port threads or a simulated MCU
/// hold.
#[derive(Debug)]
pub struct LinkEnd {
    pub from_mcu: tmpsc::Sender<Vec<u8>>,
    pub to_mcu: tmpsc::Receiver<Vec<u8>>,
}

/// A connected pair.
#[must_use]
pub fn link_pair() -> (Link, LinkEnd) {
    let (rx_tx, rx_rx) = tmpsc::channel(LINK_QUEUE);
    let (tx_tx, tx_rx) = tmpsc::channel(LINK_QUEUE);
    (
        Link {
            from_mcu: rx_rx,
            to_mcu: tx_tx,
        },
        LinkEnd {
            from_mcu: rx_tx,
            to_mcu: tx_rx,
        },
    )
}

/// The clock rule until `clock_manager` reads chrony and the RTC (DD-79):
/// the system clock counts as valid once its year is at least
/// [`BUILD_YEAR`].
#[must_use]
pub fn interim_clock() -> Option<Timestamp> {
    let now = Timestamp::now();
    (now.to_zoned_datetime(jiff::tz::TimeZone::UTC).year() >= BUILD_YEAR).then_some(now)
}

/// The `STARTUP` and `LOG_GAP` events of a core start (DD-30, S10.17).
/// `last_alive` is `core_state.last_alive_ts`; `clean` says whether the
/// previous run ended with `SHUTDOWN`.
#[must_use]
pub fn startup_events(now: Timestamp, last_alive: Option<Timestamp>, clean: bool) -> Vec<NewEvent> {
    let mut events = vec![NewEvent::system(
        now,
        SystemType::Startup,
        Some(format!("app {APP_VERSION}")),
    )];
    if let Some(last) = last_alive {
        let kind = if clean {
            "clean"
        } else if now.as_millisecond() - last.as_millisecond() < CRASH_WITHIN_MS {
            "crash"
        } else {
            "power loss"
        };
        events.push(NewEvent::system(
            now,
            SystemType::LogGap,
            Some(format!(
                "{kind}: nothing logged from {} to {}",
                utc_text(last),
                utc_text(now)
            )),
        ));
    }
    events
}

/// What the core knew when it stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    /// Frame parser counters (S5.9).
    pub parser: ParserStats,
    /// Frames lost since core start (DD-66).
    pub frames_lost: u64,
    /// The link state at the end.
    pub link: LinkState,
}

/// The core before it runs: what it got from the database at start-up,
/// and the two `watch`es other parts read (the live state for `api_server`,
/// DD-14; the link).
#[derive(Debug)]
pub struct Core {
    tz: TimeZone,
    open_faults: FaultBits,
    last_firmware: Option<FirmwareVersion>,
    live: watch::Sender<LiveState>,
    link: watch::Sender<LinkView>,
}

impl Core {
    /// `open_faults` and `last_firmware` come from the database
    /// (`Store::open_fault_codes`, the newest accepted CONFIG), so the
    /// detector reconciles (DD-37) and a firmware update made while the Pi
    /// was off is logged (DD-90). `tz` is the local time zone (DD-95).
    #[must_use]
    pub fn new(
        tz: TimeZone,
        open_faults: FaultBits,
        last_firmware: Option<FirmwareVersion>,
    ) -> Core {
        Core {
            tz,
            open_faults,
            last_firmware,
            live: watch::channel(live::initial()).0,
            link: watch::channel(LinkView::default()).0,
        }
    }

    /// What the screen shows, updated on every change (S8).
    #[must_use]
    pub fn live(&self) -> watch::Receiver<LiveState> {
        self.live.subscribe()
    }

    /// The link as `link_supervisor` publishes it.
    #[must_use]
    pub fn link(&self) -> watch::Receiver<LinkView> {
        self.link.subscribe()
    }

    /// Runs the tasks until `shutdown` resolves or the MCU side of the link
    /// closes, then stops them in order and writes `SHUTDOWN` (D5 §1).
    /// `db` is the `db_writer` thread's queue, `clock` the wall clock.
    ///
    /// # Panics
    ///
    /// If one of the tasks panicked (in the Pi build a panic aborts the
    /// process instead, DD-17).
    pub async fn run<S>(
        self,
        link: Link,
        db: mpsc::Sender<DbCommand>,
        clock: Clock,
        shutdown: S,
    ) -> Summary
    where
        S: Future<Output = ()>,
    {
        let start = hub::now();
        let ring = SharedRing::new();
        let (rx_frames, from_rx) = tmpsc::channel(QUEUE);
        let (to_tx, from_link) = tmpsc::channel(QUEUE);
        let (frames, _) = broadcast::channel(BROADCAST);
        let (samples, samples_rx) = broadcast::channel(BROADCAST);
        let (demand, demand_rx) = watch::channel(None);
        let (alive, alive_rx) = watch::channel(start);
        let (snapshot_done, snapshot_rx) = tmpsc::channel(QUEUE);
        // Every receiver exists before the first frame is sent.
        let fan_io = FanIo {
            frames: frames.subscribe(),
            link: self.link.subscribe(),
            demand,
            alive,
        };
        let live_io = LiveIo {
            frames: frames.subscribe(),
            link: self.link.subscribe(),
            fan: demand_rx.clone(),
            live: self.live,
        };
        let agg_io = AggregatorIo {
            samples: samples_rx,
            link: self.link.subscribe(),
            db: db.clone(),
        };
        let det_io = DetectorIo {
            frames: frames.subscribe(),
            db: db.clone(),
            snapshot_done,
        };
        let maint_io = MaintenanceIo {
            snapshot_done: snapshot_rx,
            db: db.clone(),
        };
        let link_io = LinkIo {
            from_rx,
            to_tx,
            view: self.link,
            frames,
            samples,
            ring: ring.clone(),
            fan: demand_rx,
            db: db.clone(),
        };

        let link_h = tokio::spawn(link_task::run(
            LinkTask::new(start, self.last_firmware),
            link_io,
            clock.clone(),
        ));
        let tx_h = tokio::spawn(uart::run_tx(
            UartTx::new(),
            from_link,
            link.to_mcu,
            alive_rx,
        ));
        let fan_h = tokio::spawn(fan::run(FanTask::new(), fan_io));
        let live_h = tokio::spawn(live::run(LiveTask::new(), live_io));
        let agg_h = tokio::spawn(aggregator::run(
            AggregatorTask::new(self.tz),
            agg_io,
            clock.clone(),
        ));
        let det_h = tokio::spawn(detector::run(
            DetectorTask::new(self.open_faults, ring),
            det_io,
        ));
        let maint_h = tokio::spawn(maintenance::run(
            MaintenanceTask::new(),
            maint_io,
            clock.clone(),
        ));

        let parser = uart::run_rx(
            UartRx::new(),
            link.from_mcu,
            rx_frames,
            clock.clone(),
            shutdown,
        )
        .await;

        let link_state = link_h.await.expect("link_supervisor panicked");
        tx_h.await.expect("uart_tx panicked");
        fan_h.await.expect("fan_controller panicked");
        live_h.await.expect("live_state panicked");
        agg_h.await.expect("aggregator panicked");
        let frames_lost = det_h.await.expect("event_detector panicked");
        maint_h.await.expect("maintenance panicked");

        if let Some(utc) = clock() {
            for command in shutdown_records(utc) {
                hub::send_db(&db, command);
            }
        }
        Summary {
            parser,
            frames_lost,
            link: link_state,
        }
    }
}

/// The last two records of a clean stop, after every task has written what
/// it held (D5 §1): `SHUTDOWN`, then `last_alive_ts`.
fn shutdown_records(utc: Timestamp) -> [DbCommand; 2] {
    [
        DbCommand::Event(NewEvent::system(utc, SystemType::Shutdown, None)),
        DbCommand::CoreState {
            key: CoreKey::LastAliveTs,
            value: utc_text(utc),
        },
    ]
}

/// The port reader thread: bytes from the port to `uart_rx`. Ends when the
/// task is gone or the port fails.
///
/// # Errors
///
/// Only if the OS cannot start a thread.
pub fn spawn_reader<R>(
    mut port: R,
    to_task: tmpsc::Sender<Vec<u8>>,
) -> std::io::Result<JoinHandle<()>>
where
    R: Read + Send + 'static,
{
    thread::Builder::new()
        .name("uart_rx_port".into())
        .spawn(move || {
            let mut buf = [0_u8; READ_CHUNK];
            loop {
                match port.read(&mut buf) {
                    // No data within the port's read timeout, or end of a test
                    // stream: look again unless the task is gone.
                    Ok(0) => {
                        if to_task.is_closed() {
                            return;
                        }
                        thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Ok(n) => {
                        if to_task.blocking_send(buf[..n].to_vec()).is_err() {
                            return;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => {
                        eprintln!("uart_rx: {e}");
                        return;
                    }
                }
            }
        })
}

/// The port writer thread: the bytes `uart_tx` encoded, to the port (the
/// only writer to it, DD-70). Ends when `uart_tx` drops its sender.
///
/// # Errors
///
/// Only if the OS cannot start a thread.
pub fn spawn_writer<W>(
    mut port: W,
    mut from_task: tmpsc::Receiver<Vec<u8>>,
) -> std::io::Result<JoinHandle<()>>
where
    W: Write + Send + 'static,
{
    thread::Builder::new()
        .name("uart_tx_port".into())
        .spawn(move || {
            while let Some(bytes) = from_task.blocking_recv() {
                if let Err(e) = port.write_all(&bytes).and_then(|()| port.flush()) {
                    eprintln!("uart_tx: {e}");
                }
            }
        })
}

/// Opens the MCU UART (`/dev/ttyAMA0`, DD-88) as raw 115200 8N1 (S5.8,
/// DD-98): no echo, no line editing, no flow control, no modem lines. A
/// read returns after at most 0.1 s without data, so the reader thread can
/// notice a shutdown.
///
/// # Errors
///
/// The device cannot be opened or configured.
#[cfg(target_os = "linux")]
pub fn open_serial(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    use rustix::fs::{Mode, OFlags};
    use rustix::termios::{
        tcflush, tcgetattr, tcsetattr, ControlModes, OptionalActions, QueueSelector,
        SpecialCodeIndex,
    };
    let fd = rustix::fs::open(
        path,
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let mut t = tcgetattr(&fd)?;
    t.make_raw(); // 8 data bits, no parity, no echo or line editing
    t.control_modes
        .remove(ControlModes::CSTOPB | ControlModes::PARENB | ControlModes::CRTSCTS);
    t.control_modes
        .insert(ControlModes::CLOCAL | ControlModes::CREAD);
    t.special_codes[SpecialCodeIndex::VMIN] = 0;
    t.special_codes[SpecialCodeIndex::VTIME] = 1;
    t.set_speed(115_200)?;
    tcsetattr(&fd, OptionalActions::Now, &t)?;
    tcflush(&fd, QueueSelector::IOFlush)?;
    Ok(std::fs::File::from(fd))
}

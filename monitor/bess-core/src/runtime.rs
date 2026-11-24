//! The running core: the [`Core`] pipeline in a tokio task, between the
//! serial port and the `db_writer` thread (docs/design/architecture.md §2,
//! docs/design/lifecycle.md §1; DD-70, DD-98, DD-99).
//!
//! - The MCU is on the Pi's PL011 UART pins (GPIO14/15, `/dev/ttyAMA0`,
//!   DD-88), one fixed device: `open_serial` (Linux only) sets it to raw 115200 8N1
//!   with `rustix` termios (DD-98). A reader and a writer thread move the
//!   bytes between the port and the task, so the task never blocks on the
//!   port; `uart_tx` is that one writer thread (DD-70).
//! - The task waits for bytes, the next timer deadline of the core, or the
//!   shutdown signal; it publishes the live state on a `watch` channel for
//!   the API (DD-14) and sends database commands to `db_writer`.
//! - The heartbeat comes from the same task as the link supervisor and the
//!   fan rules, so it stops when that task hangs (the DD-26 liveness gate;
//!   the fan output writer joins the gate when it exists).
//!
//! The byte link is two channels, so tests and the preview run the same
//! loop against a simulated MCU without a serial port.

use crate::live::LiveState;
use crate::pipeline::{Core, DbCommand, Effects};
use jiff::Timestamp;
use std::future::Future;
use std::io::{Read, Write};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Instant;
use tokio::sync::{mpsc as tmpsc, watch};

/// Bytes per read from the port: about 20 ms at 115200 baud.
const READ_CHUNK: usize = 256;

/// Channel depth between the port threads and the task.
const LINK_QUEUE: usize = 64;

/// The year this build was made: an RTC or system clock before it is not
/// trusted (DD-79).
pub const BUILD_YEAR: i16 = 2026;

/// The byte link to the MCU, as seen by the core task.
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

/// Runs the core until `shutdown` resolves or the MCU side of the link
/// closes, then writes the shutdown records (D5 §1) and returns the core.
pub async fn run<C, S>(
    mut core: Core,
    mut link: Link,
    db: mpsc::Sender<DbCommand>,
    live: watch::Sender<LiveState>,
    clock: C,
    shutdown: S,
) -> Core
where
    C: Fn() -> Option<Timestamp>,
    S: Future<Output = ()>,
{
    tokio::pin!(shutdown);
    loop {
        let deadline = tokio::time::Instant::from_std(core.next_deadline(Instant::now()));
        let fx = tokio::select! {
            bytes = link.from_mcu.recv() => match bytes {
                Some(bytes) => core.on_bytes(Instant::now(), clock(), &bytes),
                None => break,
            },
            () = tokio::time::sleep_until(deadline) => core.poll(Instant::now(), clock()),
            () = &mut shutdown => break,
        };
        deliver(fx, &link, &db).await;
        live.send_replace(core.live());
    }
    let fx = core.shutdown(clock());
    deliver(fx, &link, &db).await;
    core
}

async fn deliver(fx: Effects, link: &Link, db: &mpsc::Sender<DbCommand>) {
    if !fx.to_mcu.is_empty() {
        // A closed port shows up as a link timeout; nothing else to do here.
        let _ = link.to_mcu.send(fx.to_mcu).await;
    }
    for command in fx.db {
        if db.send(command).is_err() {
            eprintln!("bess-core: db_writer has stopped; command dropped");
        }
    }
}

/// The reader thread: bytes from the port to the task. Ends when the task
/// is gone or the port fails.
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
        .name("uart_rx".into())
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

/// The writer thread (`uart_tx`, the only writer to the port, DD-70).
/// Ends when the task drops its sender.
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
        .name("uart_tx".into())
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

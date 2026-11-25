//! The state hub of docs/design/architecture.md §2: what the tasks send each
//! other (DD-70).
//!
//! - `uart_rx` → `link_supervisor`: every valid MCU frame as an [`RxFrame`]
//!   (`mpsc`), in arrival order.
//! - `link_supervisor` → everyone: the link as a [`LinkView`] (`watch`, the
//!   "mcu config" of D1); every MCU frame as an [`McuFrame`] stamped with
//!   the link after it (the "fault frames" `broadcast`, DD-34); every 1 s
//!   sample as a [`Sampled`] (the "sample stream" `broadcast`), put in the
//!   [`SharedRing`] before it is sent.
//! - `fan_controller` → `live_state`, `link_supervisor`, `uart_tx`: the duty
//!   (`watch`) and when it last ran (`watch`, the DD-26 liveness gate).
//! - Every task → `db_writer`: [`DbCommand`]s
//!   (`std::sync::mpsc`, the writer is a thread).
//!
//! `uart_rx` hands every frame to `link_supervisor` first, which forwards
//! it: a frame then always carries the link state *after* it was handled,
//! so a TELEMETRY with a new identity is never sampled with the old CONFIG
//! (S2.4) and the detector sees the state the frame caused (DD-69).

use crate::link::LinkView;
use crate::ring::{RingBuffer, RingEntry};
use crate::signals::Signals;
use crate::writer::DbCommand;
use jiff::Timestamp;
use protocol::messages::{Message, Telemetry};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// The wall clock as the tasks read it: `None` while it is not valid
/// (DD-11). `clock_manager` will provide it (DD-79); until then
/// [`crate::runtime::interim_clock`].
pub type Clock = Arc<dyn Fn() -> Option<Timestamp> + Send + Sync>;

/// Depth of the `mpsc` queues between tasks.
pub const QUEUE: usize = 64;

/// Depth of the two `broadcast` channels: about 25 s of frames at 10 Hz, so
/// a task only falls behind if it hangs.
pub const BROADCAST: usize = 256;

/// The periodic timers (the 1 s fan timer of DD-51, the aggregate close,
/// the daily job, `last_alive_ts`) run this often.
pub const TICK: Duration = Duration::from_secs(1);

/// The monotonic time now. Through tokio, so the tests can run the tasks
/// in paused time.
#[must_use]
pub fn now() -> Instant {
    tokio::time::Instant::now().into_std()
}

/// Sends one command to `db_writer`; if it has stopped, says so on stderr
/// (the journal) and goes on (DD-15).
pub fn send_db(db: &std::sync::mpsc::Sender<DbCommand>, command: DbCommand) {
    if db.send(command).is_err() {
        eprintln!("bess-core: db_writer has stopped; command dropped");
    }
}

/// Waits until `deadline`, or forever without one.
pub async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(tokio::time::Instant::from_std(d)).await,
        None => std::future::pending().await,
    }
}

/// One valid frame from the MCU, from `uart_rx` to `link_supervisor`.
#[derive(Debug, Clone, PartialEq)]
pub struct RxFrame {
    /// Receive time, monotonic.
    pub at: Instant,
    /// Receive time, UTC; `None` while the clock is not valid.
    pub utc: Option<Timestamp>,
    /// Frame envelope sequence number.
    pub sequence: u8,
    /// `None` for a valid frame whose payload the Pi cannot decode (the
    /// parser counted it); its sequence number still counts (DD-66).
    pub message: Option<Message>,
}

/// What an [`McuFrame`] is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FrameKind {
    /// A TELEMETRY. `signals` only while ONLINE (S2.4): the frame is then
    /// also a 1 s sample, in the ring buffer and on the sample stream.
    Telemetry {
        telemetry: Telemetry,
        signals: Option<Signals>,
    },
    /// A `FAULT_UPDATE`; `signals` only while ONLINE.
    FaultUpdate {
        telemetry: Telemetry,
        signals: Option<Signals>,
    },
    /// Any other valid MCU frame (CONFIG, `RESET_ACK`, one the Pi cannot
    /// decode): only its sequence number counts (DD-66).
    Other,
}

/// One MCU frame on the fault-frame broadcast, in arrival order (DD-34).
#[derive(Debug, Clone, PartialEq)]
pub struct McuFrame {
    /// Receive time, monotonic.
    pub at: Instant,
    /// Receive time, UTC; `None` while the clock is not valid.
    pub utc: Option<Timestamp>,
    /// Frame envelope sequence number.
    pub sequence: u8,
    /// The link after `link_supervisor` handled this frame.
    pub link: LinkView,
    pub kind: FrameKind,
}

impl McuFrame {
    /// The TELEMETRY or `FAULT_UPDATE` payload.
    #[must_use]
    pub fn telemetry(&self) -> Option<&Telemetry> {
        match &self.kind {
            FrameKind::Telemetry { telemetry, .. } | FrameKind::FaultUpdate { telemetry, .. } => {
                Some(telemetry)
            }
            FrameKind::Other => None,
        }
    }

    /// The 15 signals of this frame (ONLINE only).
    #[must_use]
    pub fn signals(&self) -> Option<Signals> {
        match self.kind {
            FrameKind::Telemetry { signals, .. } | FrameKind::FaultUpdate { signals, .. } => {
                signals
            }
            FrameKind::Other => None,
        }
    }

    /// The 1 s sample of a TELEMETRY while ONLINE, as it is in the ring
    /// buffer.
    #[must_use]
    pub fn sample(&self) -> Option<RingEntry> {
        match self.kind {
            FrameKind::Telemetry {
                telemetry,
                signals: Some(signals),
            } => Some(RingEntry {
                at: self.at,
                utc: self.utc,
                state: telemetry.state,
                signals,
            }),
            _ => None,
        }
    }
}

/// One 1 s sample on the sample stream (D1 `broadcast<Sample>`, 1 Hz).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sampled {
    pub entry: RingEntry,
    /// CONFIG `telemetry_ms`: how long this sample counts for (DD-46).
    pub period: Duration,
}

/// The 1 h ring buffer of the state hub (DD-04, DD-13): `link_supervisor`
/// fills it as it publishes a sample, so it already holds a sample when any
/// task receives the frame; `event_detector` reads the 60 s before a fault
/// from it and `api_server` will read the last hour. Locked only briefly,
/// never across an `.await`.
#[derive(Debug, Clone, Default)]
pub struct SharedRing(Arc<Mutex<RingBuffer>>);

impl SharedRing {
    #[must_use]
    pub fn new() -> SharedRing {
        SharedRing::default()
    }

    /// The buffer. A task that panicked while holding it cannot have left
    /// it half-written (`push` is one step), so a poisoned lock is used as
    /// is.
    pub fn lock(&self) -> MutexGuard<'_, RingBuffer> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

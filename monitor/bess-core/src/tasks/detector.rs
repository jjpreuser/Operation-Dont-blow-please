//! The `event_detector` task (docs/design/architecture.md §2,
//! fault-handling.md §2–3, §6; DD-07, DD-34, DD-37, DD-66, DD-97).
//!
//! It runs the [`EventDetector`] on every frame of the fault-frame
//! broadcast, in arrival order (DD-34), with the 60 s before a fault from
//! the shared ring buffer, which already holds the frame's own sample. A
//! change in the link's outage count tells it the link was down since the
//! frame before (DD-37); so does a gap in the broadcast if it fell behind.
//! Its timer completes a snapshot when no samples come. Writes go to
//! `db_writer`; a completed snapshot is reported to `maintenance` for the
//! size guard (D3 §4). At shutdown the open snapshot is written with what
//! it has.

use super::hub::{McuFrame, SharedRing, now, send_db, sleep_until};
use crate::detector::{EventDetector, Frame, Write};
use crate::writer::DbCommand;
use protocol::messages::FaultBits;
use std::time::Instant;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, mpsc};

/// The `event_detector` task's logic around [`EventDetector`].
#[derive(Debug)]
pub struct DetectorTask {
    det: EventDetector,
    ring: SharedRing,
    /// [`LinkView::outages`](crate::link::LinkView::outages) of the frame
    /// before.
    outages: u32,
}

impl DetectorTask {
    /// `open` are the codes of the open fault events in the database, so the
    /// first frame reconciles (DD-37).
    #[must_use]
    pub fn new(open: FaultBits, ring: SharedRing) -> DetectorTask {
        DetectorTask {
            det: EventDetector::new(open),
            ring,
            outages: 0,
        }
    }

    /// One frame from the broadcast.
    pub fn on_frame(&mut self, frame: &McuFrame) -> Vec<Write> {
        if frame.link.outages != self.outages {
            self.outages = frame.link.outages;
            self.det.on_offline();
        }
        let mut out = Vec::new();
        let Some(telemetry) = frame.telemetry() else {
            self.det.on_other_frame(frame.sequence);
            return out;
        };
        if let Some(entry) = frame.sample() {
            out.extend(self.det.on_sample(&entry));
        }
        // Events need a valid clock (DD-11); the DD-31 queue is still to do.
        if let Some(utc) = frame.utc {
            let f = Frame {
                at: frame.at,
                utc,
                sequence: frame.sequence,
                telemetry,
                link: frame.link.state,
                signals: frame.signals(),
                firmware: frame.link.config().map(|c| c.firmware),
            };
            let ring = self.ring.lock();
            out.extend(self.det.on_frame(&f, &ring));
        }
        out
    }

    /// Frames were missed: the next one reconciles, and the gap is not
    /// counted as lost on the UART.
    pub fn missed_frames(&mut self) {
        self.det.on_offline();
    }

    /// The timer.
    pub fn poll(&mut self, now: Instant) -> Vec<Write> {
        self.det.poll(now)
    }

    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        self.det.next_deadline()
    }

    /// Shutdown: the open snapshot with what it has.
    pub fn flush(&mut self) -> Vec<Write> {
        self.det.flush()
    }

    /// Frames lost since core start (DD-66, S9.11).
    #[must_use]
    pub fn frames_lost_total(&self) -> u64 {
        self.det.frames_lost_total()
    }
}

/// The channels of the `event_detector` task.
#[derive(Debug)]
pub struct DetectorIo {
    pub frames: broadcast::Receiver<McuFrame>,
    pub db: std::sync::mpsc::Sender<DbCommand>,
    /// "A snapshot is complete" to `maintenance`.
    pub snapshot_done: mpsc::Sender<()>,
}

/// Runs the task until the broadcast closes; returns the lost-frame count.
pub async fn run(mut task: DetectorTask, mut io: DetectorIo) -> u64 {
    loop {
        let writes = tokio::select! {
            frame = io.frames.recv() => match frame {
                Ok(frame) => task.on_frame(&frame),
                Err(RecvError::Lagged(n)) => {
                    eprintln!("event_detector: {n} frames missed; reconciling with the next");
                    task.missed_frames();
                    continue;
                }
                Err(RecvError::Closed) => break,
            },
            () = sleep_until(task.next_deadline()) => task.poll(now()),
        };
        deliver(writes, &io).await;
    }
    let writes = task.flush();
    deliver(writes, &io).await;
    task.frames_lost_total()
}

async fn deliver(writes: Vec<Write>, io: &DetectorIo) {
    for w in writes {
        let complete = w == Write::SnapshotComplete;
        send_db(&io.db, DbCommand::Detector(w));
        if complete {
            // After the write above, so the size guard comes after it.
            let _ = io.snapshot_done.send(()).await;
        }
    }
}

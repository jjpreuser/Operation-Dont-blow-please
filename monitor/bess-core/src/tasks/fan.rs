//! The `fan_controller` task (docs/design/architecture.md §2,
//! fan-control.md §3–4; DD-16, DD-26, DD-42, DD-51, DD-91).
//!
//! It runs the [`FanController`] rules on every MCU frame, on every link
//! change and on its 1 s timer (so the OFFLINE hold-then-off rule runs on
//! without frames), with the CONFIG and link state from the link watch and
//! the latest frame from the fault-frame broadcast. It publishes the duty
//! on a `watch` (for the live state and the stored samples) and the time it
//! last ran, which `uart_tx` checks before every HEARTBEAT (DD-26).
//!
//! Still to come: the EMC2101 output, the RPM reading and E12 (fan bench
//! tests, DD-39), `FAN_CTRL_RESET` and the dead-fan rule (DD-84 to DD-86).

use super::hub::{McuFrame, TICK, now};
use crate::fan::{FanController, FanDemand, FanInputs};
use crate::link::LinkView;
use protocol::messages::Telemetry;
use std::time::Instant;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, watch};

/// The `fan_controller` task's logic around [`FanController`].
#[derive(Debug, Clone, Default)]
pub struct FanTask {
    ctl: FanController,
    /// The latest valid frame, kept as the live state keeps it: only while
    /// the link uses the fault bitmap (DD-69).
    frame: Option<Telemetry>,
}

impl FanTask {
    #[must_use]
    pub fn new() -> FanTask {
        FanTask::default()
    }

    /// One MCU frame from the broadcast.
    pub fn on_frame(&mut self, frame: &McuFrame) {
        if let (true, Some(t)) = (frame.link.shows_faults(), frame.telemetry()) {
            self.frame = Some(*t);
        }
    }

    /// The duty now.
    pub fn evaluate(&mut self, now: Instant, link: &LinkView) -> FanDemand {
        self.ctl.evaluate(
            now,
            FanInputs {
                link: link.state,
                config: link.config(),
                frame: self.frame.as_ref(),
            },
        )
    }
}

/// The channels of the `fan_controller` task.
#[derive(Debug)]
pub struct FanIo {
    pub frames: broadcast::Receiver<McuFrame>,
    pub link: watch::Receiver<LinkView>,
    pub demand: watch::Sender<Option<FanDemand>>,
    /// When the task last evaluated the rules (the DD-26 gate).
    pub alive: watch::Sender<Instant>,
}

/// Runs the task until `link_supervisor` stops.
pub async fn run(mut task: FanTask, mut io: FanIo) {
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            frame = io.frames.recv() => match frame {
                Ok(frame) => task.on_frame(&frame),
                Err(RecvError::Lagged(n)) => eprintln!("fan_controller: {n} frames missed"),
                Err(RecvError::Closed) => break,
            },
            changed = io.link.changed() => if changed.is_err() { break },
            _ = tick.tick() => {}
        }
        let link = io.link.borrow_and_update().clone();
        let now = now();
        let demand = Some(task.evaluate(now, &link));
        io.demand.send_if_modified(|d| {
            let changed = *d != demand;
            *d = demand;
            changed
        });
        io.alive.send_replace(now);
    }
}

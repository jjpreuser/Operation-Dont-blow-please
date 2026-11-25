//! The live state of the state hub (docs/design/architecture.md §2 "live
//! state"; S8, S8.9, S8.12; DD-29, DD-69).
//!
//! D1 draws one `watch<LiveState>` that `uart_rx`, `link_supervisor`,
//! `clock_manager` and `fan_controller` all feed. A watch has one writer, so
//! this small `live_state` task merges them: the frames from the
//! fault-frame broadcast, the link watch and the fan duty watch go through
//! the [`LiveTracker`], and every change is published for `api_server`
//! (DD-14, DD-18). The clock joins when `clock_manager` exists.

use super::hub::McuFrame;
use crate::colour::WarningLevels;
use crate::fan::FanDemand;
use crate::link::LinkView;
use crate::live::{LiveState, LiveTracker};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, watch};

/// The `live_state` task's logic around [`LiveTracker`].
#[derive(Debug, Clone, Default)]
pub struct LiveTask {
    tracker: LiveTracker,
    warn: WarningLevels,
}

impl LiveTask {
    #[must_use]
    pub fn new() -> LiveTask {
        LiveTask::default()
    }

    /// One MCU frame, with the link as it was after that frame.
    pub fn on_frame(&mut self, frame: &McuFrame) {
        if let Some(t) = frame.telemetry() {
            self.tracker.on_frame(t, &frame.link);
        }
    }

    /// The screen contents with the link and the fan duty now.
    #[must_use]
    pub fn view(&self, link: &LinkView, fan: Option<FanDemand>) -> LiveState {
        self.tracker.view(link, fan, &self.warn)
    }
}

/// What the screen shows at core start: OFFLINE, nothing known.
#[must_use]
pub fn initial() -> LiveState {
    LiveTask::new().view(&LinkView::default(), None)
}

/// The channels of the `live_state` task.
#[derive(Debug)]
pub struct LiveIo {
    pub frames: broadcast::Receiver<McuFrame>,
    pub link: watch::Receiver<LinkView>,
    pub fan: watch::Receiver<Option<FanDemand>>,
    pub live: watch::Sender<LiveState>,
}

/// Runs the task until `link_supervisor` or `fan_controller` stops.
pub async fn run(mut task: LiveTask, mut io: LiveIo) {
    loop {
        tokio::select! {
            frame = io.frames.recv() => match frame {
                Ok(frame) => task.on_frame(&frame),
                // The next frame brings the screen up to date.
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            },
            changed = io.link.changed() => if changed.is_err() { break },
            changed = io.fan.changed() => if changed.is_err() { break },
        }
        // The link watch is at least as new as any frame received (it is
        // published first), so the view uses it, not the frame's copy.
        let state = task.view(&io.link.borrow_and_update(), *io.fan.borrow_and_update());
        io.live.send_if_modified(|s| {
            let changed = *s != state;
            if changed {
                *s = state;
            }
            changed
        });
    }
}

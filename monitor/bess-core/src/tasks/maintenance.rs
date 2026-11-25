//! The `maintenance` task (docs/design/architecture.md §2, data-lifecycle.md
//! §4; D3 §4; DD-23, DD-30).
//!
//! For now: the size guard after each completed snapshot (D3 §4), and
//! `core_state.last_alive_ts` every 60 s, from which the next start tells a
//! crash from a power loss (DD-30). The retention delete runs inside the
//! daily job (DD-47). Still to come: the daily backup, keep 7 (S10.14).

use super::hub::{Clock, TICK, now, send_db};
use crate::db::CoreKey;
use crate::wallclock::utc_text;
use crate::writer::DbCommand;
use jiff::Timestamp;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// `core_state.last_alive_ts` is written this often (DD-30).
pub const ALIVE_EVERY: Duration = Duration::from_secs(60);

/// The `maintenance` task's timers.
#[derive(Debug, Clone, Default)]
pub struct MaintenanceTask {
    next_alive: Option<Instant>,
}

impl MaintenanceTask {
    #[must_use]
    pub fn new() -> MaintenanceTask {
        MaintenanceTask::default()
    }

    /// The 1 s timer: `last_alive_ts` once the clock is valid, then every
    /// [`ALIVE_EVERY`].
    pub fn tick(&mut self, now: Instant, utc: Option<Timestamp>) -> Option<DbCommand> {
        let utc = utc?;
        if self.next_alive.is_some_and(|t| now < t) {
            return None;
        }
        self.next_alive = Some(now + ALIVE_EVERY);
        Some(DbCommand::CoreState {
            key: CoreKey::LastAliveTs,
            value: utc_text(utc),
        })
    }
}

/// The channels of the `maintenance` task.
#[derive(Debug)]
pub struct MaintenanceIo {
    /// "A snapshot is complete" from `event_detector`; closed when it stops.
    pub snapshot_done: mpsc::Receiver<()>,
    pub db: std::sync::mpsc::Sender<DbCommand>,
}

/// Runs the task until `event_detector` stops.
pub async fn run(mut task: MaintenanceTask, mut io: MaintenanceIo, clock: Clock) {
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            done = io.snapshot_done.recv() => {
                if done.is_none() {
                    break;
                }
                if let Some(now) = clock() {
                    send_db(&io.db, DbCommand::SizeGuard { now });
                }
            }
            _ = tick.tick() => {
                if let Some(command) = task.tick(now(), clock()) {
                    send_db(&io.db, command);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_alive_waits_for_a_valid_clock_then_runs_every_minute() {
        let t0 = Instant::now();
        let utc: Timestamp = "2026-11-24T10:00:00Z".parse().unwrap();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut task = MaintenanceTask::new();
        assert!(task.tick(at(0), None).is_none(), "DD-11");
        assert!(matches!(
            task.tick(at(1), Some(utc)),
            Some(DbCommand::CoreState { key: CoreKey::LastAliveTs, value })
                if value == "2026-11-24T10:00:00.000Z"
        ));
        assert!(task.tick(at(60), Some(utc)).is_none());
        assert!(task.tick(at(61), Some(utc)).is_some());
    }
}

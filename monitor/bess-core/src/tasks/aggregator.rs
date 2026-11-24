//! The `aggregator` task (docs/design/architecture.md §2, data-lifecycle.md
//! §3–4; S10.3, S10.4; DD-09, DD-10, DD-11, DD-45, DD-47, DD-74).
//!
//! It takes every 1 s sample from the sample stream into the open 10-min
//! interval, closes the interval on its 1 s timer when no closing sample
//! comes (2 s after the boundary, DD-45), and starts the daily job: at the
//! first valid clock (catch-up) and then at 00:00:05 local, once the 23:50
//! interval has closed (DD-47). At shutdown the open interval is written as
//! a partial row (D5 §1). Samples without a valid clock are in the ring
//! buffer only (DD-11, DD-74).

use super::hub::{send_db, Clock, Sampled, TICK};
use crate::aggregate::{AggRow, Aggregator};
use crate::db::capacity_ah;
use crate::link::LinkView;
use crate::wallclock::{add_ms, next_local_midnight};
use crate::writer::DbCommand;
use jiff::tz::TimeZone;
use jiff::Timestamp;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, watch};

/// The daily job runs this long after local midnight, once the 2 s close
/// timer has closed the 23:50 interval (DD-47).
pub const DAILY_AFTER_MIDNIGHT_MS: i64 = 5000;

/// If the next local midnight cannot be computed (a time zone without one
/// in range), the daily job is tried again this much later instead of on
/// every tick.
const DAILY_RETRY_MS: i64 = 3_600_000;

/// The `aggregator` task's logic around [`Aggregator`].
#[derive(Debug, Clone)]
pub struct AggregatorTask {
    agg: Aggregator,
    tz: TimeZone,
    /// `None` until the first valid clock: then the daily job catches up.
    next_daily: Option<Timestamp>,
}

impl AggregatorTask {
    /// `tz` is the local time zone of the daily record (DD-95).
    #[must_use]
    pub fn new(tz: TimeZone) -> AggregatorTask {
        AggregatorTask {
            agg: Aggregator::new(),
            tz,
            next_daily: None,
        }
    }

    /// One sample; returns the interval it closed, if any. Without a valid
    /// clock nothing is aggregated (DD-11).
    pub fn on_sample(&mut self, sample: &Sampled) -> Option<AggRow> {
        let utc = sample.entry.utc?;
        self.agg.add(utc, &sample.entry.signals, sample.period)
    }

    /// The 1 s timer with a valid clock: the 10-min close and the daily job.
    /// `capacity_ah` from the installed CONFIG.
    pub fn tick(&mut self, utc: Timestamp, capacity_ah: Option<f64>) -> Vec<DbCommand> {
        let mut out = Vec::new();
        if let Some(row) = self.agg.poll(utc) {
            out.push(DbCommand::Agg(Box::new(row)));
        }
        if self.next_daily.is_none_or(|t| utc >= t) {
            out.push(DbCommand::DailyJob {
                now: utc,
                capacity_ah,
            });
            self.next_daily = Some(next_local_midnight(utc, &self.tz).map_or_else(
                |_| add_ms(utc, DAILY_RETRY_MS),
                |m| add_ms(m, DAILY_AFTER_MIDNIGHT_MS),
            ));
        }
        out
    }

    /// Shutdown: the open interval as a partial row.
    pub fn flush(&mut self) -> Option<AggRow> {
        self.agg.flush()
    }

    /// The open interval as it is now.
    #[must_use]
    pub fn peek(&self) -> Option<AggRow> {
        self.agg.peek()
    }
}

/// The channels of the `aggregator` task.
#[derive(Debug)]
pub struct AggregatorIo {
    pub samples: broadcast::Receiver<Sampled>,
    /// For the capacity of the installed CONFIG.
    pub link: watch::Receiver<LinkView>,
    pub db: std::sync::mpsc::Sender<DbCommand>,
}

/// Runs the task until the sample stream closes, then writes the partial
/// row.
pub async fn run(mut task: AggregatorTask, mut io: AggregatorIo, clock: Clock) {
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            sample = io.samples.recv() => match sample {
                Ok(sample) => {
                    if let Some(row) = task.on_sample(&sample) {
                        send_db(&io.db, DbCommand::Agg(Box::new(row)));
                    }
                }
                // Missed samples are a gap, as without TELEMETRY (DD-10, DD-46).
                Err(RecvError::Lagged(n)) => eprintln!("aggregator: {n} samples missed"),
                Err(RecvError::Closed) => break,
            },
            _ = tick.tick() => {
                if let Some(utc) = clock() {
                    let capacity = io.link.borrow().config().map(capacity_ah);
                    for command in task.tick(utc, capacity) {
                        send_db(&io.db, command);
                    }
                }
            }
        }
    }
    if let Some(row) = task.flush() {
        send_db(&io.db, DbCommand::Agg(Box::new(row)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ring::RingEntry;
    use crate::signals::Signals;
    use protocol::messages::McuState;
    use std::time::{Duration, Instant};

    fn ts(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    fn sample(utc: Option<Timestamp>) -> Sampled {
        Sampled {
            entry: RingEntry {
                at: Instant::now(),
                utc,
                state: McuState::Charging,
                signals: Signals::default(),
            },
            period: Duration::from_secs(1),
        }
    }

    #[test]
    fn only_samples_with_a_valid_clock_are_aggregated() {
        let mut task = AggregatorTask::new(TimeZone::UTC);
        assert!(task.on_sample(&sample(None)).is_none());
        assert!(task.peek().is_none(), "DD-11: ring buffer only");
        task.on_sample(&sample(Some(ts("2026-11-24T10:07:05Z"))));
        let row = task.flush().unwrap();
        assert_eq!((row.ts, row.sample_count), (ts("2026-11-24T10:00:00Z"), 1));
    }

    #[test]
    fn the_timer_closes_the_interval_2_s_after_the_boundary() {
        let mut task = AggregatorTask::new(TimeZone::UTC);
        task.tick(ts("2026-11-24T10:00:00Z"), None); // the catch-up job
        task.on_sample(&sample(Some(ts("2026-11-24T10:09:59Z"))));
        assert!(task.tick(ts("2026-11-24T10:10:01Z"), None).is_empty());
        assert!(matches!(
            task.tick(ts("2026-11-24T10:10:02Z"), None).as_slice(),
            [DbCommand::Agg(row)] if row.sample_count == 1
        ));
    }

    #[test]
    fn the_daily_job_catches_up_then_runs_after_local_midnight() {
        let tz = TimeZone::get("Europe/Amsterdam").unwrap();
        let mut task = AggregatorTask::new(tz);
        let daily = |cmds: Vec<DbCommand>| {
            cmds.iter()
                .filter(|c| matches!(c, DbCommand::DailyJob { .. }))
                .count()
        };
        assert_eq!(daily(task.tick(ts("2026-01-15T22:59:50Z"), Some(30.0))), 1);
        assert_eq!(daily(task.tick(ts("2026-01-15T23:00:04Z"), None)), 0);
        let cmds = task.tick(ts("2026-01-15T23:00:05Z"), Some(30.0)); // 00:00:05 local
        assert!(matches!(
            cmds.as_slice(),
            [DbCommand::DailyJob { now, capacity_ah: Some(c) }]
                if *now == ts("2026-01-15T23:00:05Z") && (*c - 30.0).abs() < 1e-9
        ));
        assert_eq!(daily(task.tick(ts("2026-01-15T23:00:06Z"), None)), 0);
    }
}

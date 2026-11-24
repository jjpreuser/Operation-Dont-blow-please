//! The core pipeline: every module of `bess-core` between the MCU bytes and
//! the database, in one place and in a fixed order (docs/design/
//! architecture.md §2; DD-70, DD-99).
//!
//! [`Core`] takes the bytes from the UART and the time, and returns the
//! bytes to send to the MCU and the commands for `db_writer`. It has no I/O
//! of its own, so the whole chain runs in `cargo test` on a PC against a
//! simulated MCU (S1.5); [`crate::runtime`] runs it in a tokio task with the
//! serial port, the timer and the `db_writer` thread around it.
//!
//! Per MCU frame, in arrival order (DD-34): the link supervisor; for
//! TELEMETRY and `FAULT_UPDATE` the live state and the fan rules; for a
//! TELEMETRY while ONLINE the 1 s sample into the ring buffer and the
//! 10-min aggregate (DD-74) and the snapshot rows; then the event detector
//! with the ring buffer already holding this sample. One task doing this in
//! one order is what keeps a snapshot, an event and the live state
//! consistent with each other (DD-99).
//!
//! Everything timestamped needs a valid clock (DD-11): without one the
//! samples still go to the ring buffer, but no event or aggregate is made.

use crate::aggregate::{AggRow, Recorder};
use crate::colour::WarningLevels;
use crate::db::{CoreKey, NewEvent, SystemType, APP_VERSION};
use crate::detector::{EventDetector, Frame, Write};
use crate::fan::{FanController, FanDemand, FanInputs};
use crate::link::{LinkEvent, LinkState, LinkSupervisor, Output};
use crate::live::{LiveState, LiveTracker};
use crate::ring::RingEntry;
use crate::sample::Sample;
use crate::signals::Signals;
use crate::wallclock::{add_ms, next_local_midnight, utc_text};
use jiff::tz::TimeZone;
use jiff::Timestamp;
use protocol::config::{Config, FirmwareVersion};
use protocol::frame::{Direction, SequenceCounter};
use protocol::messages::{AckStatus, FaultBits, Message, Telemetry};
use protocol::parser::{FrameParser, ParserStats};
use std::time::{Duration, Instant};

/// `core_state.last_alive_ts` is written this often (DD-30).
pub const ALIVE_EVERY: Duration = Duration::from_secs(60);

/// The daily job runs this long after local midnight, once the aggregator's
/// 2 s timer has closed the 23:50 interval (DD-47).
pub const DAILY_AFTER_MIDNIGHT_MS: i64 = 5000;

/// The core's timers run at least this often (the 1 s fan timer of DD-51).
pub const TICK: Duration = Duration::from_secs(1);

/// A restart within this time of the last `last_alive_ts` without a
/// `SHUTDOWN` is a crash, later a power loss (DD-30).
pub const CRASH_WITHIN_MS: i64 = 2 * 60 * 1000;

/// One thing for the `db_writer` thread to do, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum DbCommand {
    /// A closed 10-min interval.
    Agg(Box<AggRow>),
    /// A fault, clear or snapshot write from the event detector.
    Detector(Write),
    /// A system event.
    Event(NewEvent),
    /// A validated CONFIG and the `CONFIG_ACK` status it got (DD-43).
    McuConfig {
        received: Timestamp,
        config: Box<Config>,
        status: AckStatus,
    },
    CoreState {
        key: CoreKey,
        value: String,
    },
    /// The daily job (DD-47); `capacity_ah` from the installed CONFIG.
    DailyJob {
        now: Timestamp,
        capacity_ah: Option<f64>,
    },
    /// The size guard, after a snapshot is complete (D3 §4).
    SizeGuard {
        now: Timestamp,
    },
}

/// What one step of the core produced.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Effects {
    /// Encoded frames for the MCU, in order.
    pub to_mcu: Vec<u8>,
    pub db: Vec<DbCommand>,
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

/// A detector write as a command; a completed snapshot is followed by the
/// size guard (D3 §4).
fn push_detector(w: Write, utc: Option<Timestamp>, fx: &mut Effects) {
    let complete = w == Write::SnapshotComplete;
    fx.db.push(DbCommand::Detector(w));
    if let (true, Some(now)) = (complete, utc) {
        fx.db.push(DbCommand::SizeGuard { now });
    }
}

/// The Pi side of the BMS link, from bytes to database commands.
#[derive(Debug)]
pub struct Core {
    parser: FrameParser,
    link: LinkSupervisor,
    live: LiveTracker,
    fans: FanController,
    fan: Option<FanDemand>,
    recorder: Recorder,
    detector: EventDetector,
    sequence: SequenceCounter,
    warn: WarningLevels,
    tz: TimeZone,
    /// `None` until the first valid clock: then the daily job catches up.
    next_daily: Option<Timestamp>,
    next_alive: Option<Instant>,
}

impl Core {
    /// At core start. `open_faults` and `last_firmware` come from the
    /// database (`Store::open_fault_codes`, the newest accepted CONFIG), so
    /// the detector reconciles (DD-37) and a firmware update made while the
    /// Pi was off is logged (DD-90). `tz` is the local time zone (DD-95).
    #[must_use]
    pub fn new(
        now: Instant,
        tz: TimeZone,
        open_faults: FaultBits,
        last_firmware: Option<FirmwareVersion>,
    ) -> Core {
        Core {
            parser: FrameParser::new(Direction::McuToPi),
            link: LinkSupervisor::new(now).with_last_firmware(last_firmware),
            live: LiveTracker::new(),
            fans: FanController::new(),
            fan: None,
            recorder: Recorder::new(),
            detector: EventDetector::new(open_faults),
            sequence: SequenceCounter::new(0),
            warn: WarningLevels::default(),
            tz,
            next_daily: None,
            next_alive: None,
        }
    }

    /// Bytes from the MCU, received at `now`; `utc` is `None` while the
    /// clock is not valid.
    pub fn on_bytes(&mut self, now: Instant, utc: Option<Timestamp>, bytes: &[u8]) -> Effects {
        let mut fx = Effects::default();
        for frame in self.parser.push(bytes) {
            let Ok(message) = Message::try_from(&frame) else {
                continue; // a valid frame the Pi cannot decode; the parser counted it
            };
            let before = self.link.state();
            let outputs = self.link.on_message(now, &message);
            self.after_link(before, utc, outputs, &mut fx);
            match &message {
                Message::Telemetry(t) | Message::FaultUpdate(t) => {
                    self.live.on_frame(t, &self.link);
                    self.run_fans(now);
                    let telemetry = matches!(message, Message::Telemetry(_));
                    self.on_mcu_frame(now, utc, frame.sequence, t, telemetry, &mut fx);
                }
                _ => self.detector.on_other_frame(frame.sequence),
            }
        }
        self.timers(now, utc, &mut fx);
        fx
    }

    /// The timers, also without bytes: link timeout and heartbeat, fans,
    /// snapshot completion, the 10-min close, the daily job and
    /// `last_alive_ts`.
    pub fn poll(&mut self, now: Instant, utc: Option<Timestamp>) -> Effects {
        let mut fx = Effects::default();
        self.timers(now, utc, &mut fx);
        fx
    }

    /// When [`Core::poll`] should run next.
    #[must_use]
    pub fn next_deadline(&self, now: Instant) -> Instant {
        let mut deadline = (now + TICK).min(self.link.next_deadline());
        if let Some(d) = self.detector.next_deadline() {
            deadline = deadline.min(d);
        }
        deadline
    }

    /// Clean shutdown (D5 §1): the open interval as a partial row, the open
    /// snapshot with what it has, `SHUTDOWN`.
    pub fn shutdown(&mut self, utc: Option<Timestamp>) -> Effects {
        let mut fx = Effects::default();
        let Some(utc) = utc else {
            return fx;
        };
        if let Some(row) = self.recorder.aggregator.flush() {
            fx.db.push(DbCommand::Agg(Box::new(row)));
        }
        for w in self.detector.flush() {
            push_detector(w, Some(utc), &mut fx);
        }
        fx.db.push(DbCommand::Event(NewEvent::system(
            utc,
            SystemType::Shutdown,
            None,
        )));
        fx.db.push(DbCommand::CoreState {
            key: CoreKey::LastAliveTs,
            value: utc_text(utc),
        });
        fx
    }

    /// What the screen shows now (S8).
    #[must_use]
    pub fn live(&self) -> LiveState {
        self.live.view(&self.link, self.fan, &self.warn)
    }

    #[must_use]
    pub fn link_state(&self) -> LinkState {
        self.link.state()
    }

    /// Frame parser counters (S5.9, S9.11).
    #[must_use]
    pub fn parser_stats(&self) -> ParserStats {
        self.parser.stats()
    }

    /// Frames lost since core start (DD-66).
    #[must_use]
    pub fn frames_lost_total(&self) -> u64 {
        self.detector.frames_lost_total()
    }

    fn run_fans(&mut self, now: Instant) {
        self.fan = Some(self.fans.evaluate(
            now,
            FanInputs {
                link: self.link.state(),
                config: self.link.config(),
                frame: self.live.frame(),
            },
        ));
    }

    fn on_mcu_frame(
        &mut self,
        now: Instant,
        utc: Option<Timestamp>,
        sequence: u8,
        t: &Telemetry,
        telemetry: bool,
        fx: &mut Effects,
    ) {
        let online = self.link.state() == LinkState::Online;
        let config = self.link.config().filter(|_| online);
        let ntc_mask = config.map(|c| c.ntc_present_mask);
        let period = config.map(|c| Duration::from_millis(u64::from(c.telemetry_ms)));
        let firmware = self.link.config().map(|c| c.firmware);
        // Measurements only with the installed CONFIG (S2.4, DD-69).
        let signals = ntc_mask.map(|mask| Signals {
            sample: Sample::from_telemetry(t, mask),
            fan_duty_pct: self.fan.map(|f| f64::from(f.duty_pct)),
            fan1_rpm: None,
            fan2_rpm: None,
        });
        if telemetry {
            if let (Some(signals), Some(period)) = (signals, period) {
                let entry = RingEntry {
                    at: now,
                    utc,
                    state: t.state,
                    signals,
                };
                if let Some(row) = self.recorder.on_sample(entry, period) {
                    fx.db.push(DbCommand::Agg(Box::new(row)));
                }
                for w in self.detector.on_sample(&entry) {
                    push_detector(w, utc, fx);
                }
            }
        }
        // Events need a valid clock (DD-11); the DD-31 queue is still to do.
        if let Some(utc) = utc {
            let frame = Frame {
                at: now,
                utc,
                sequence,
                telemetry: t,
                link: self.link.state(),
                signals,
                firmware,
            };
            for w in self.detector.on_frame(&frame, &self.recorder.ring) {
                push_detector(w, Some(utc), fx);
            }
        }
    }

    fn timers(&mut self, now: Instant, utc: Option<Timestamp>, fx: &mut Effects) {
        let before = self.link.state();
        let outputs = self.link.poll(now);
        self.after_link(before, utc, outputs, fx);
        // The 1 s timer of DD-51; running it more often changes nothing.
        self.run_fans(now);
        for w in self.detector.poll(now) {
            push_detector(w, utc, fx);
        }
        let Some(utc) = utc else {
            return;
        };
        if let Some(row) = self.recorder.aggregator.poll(utc) {
            fx.db.push(DbCommand::Agg(Box::new(row)));
        }
        // First valid clock: catch up at once (DD-47); then after midnight.
        if self.next_daily.is_none_or(|t| utc >= t) {
            fx.db.push(DbCommand::DailyJob {
                now: utc,
                capacity_ah: self.link.config().map(crate::db::capacity_ah),
            });
            self.next_daily = next_local_midnight(utc, &self.tz)
                .ok()
                .map(|m| add_ms(m, DAILY_AFTER_MIDNIGHT_MS));
        }
        if self.next_alive.is_none_or(|t| now >= t) {
            fx.db.push(DbCommand::CoreState {
                key: CoreKey::LastAliveTs,
                value: utc_text(utc),
            });
            self.next_alive = Some(now + ALIVE_EVERY);
        }
    }

    /// Link supervisor outputs: frames for the MCU, events for the DB; and
    /// the detector learns when the link went OFFLINE (DD-37, DD-66).
    fn after_link(
        &mut self,
        before: LinkState,
        utc: Option<Timestamp>,
        outputs: Vec<Output>,
        fx: &mut Effects,
    ) {
        if before != LinkState::Offline && self.link.state() == LinkState::Offline {
            self.detector.on_offline();
        }
        for output in outputs {
            match output {
                Output::Send(message) => {
                    fx.to_mcu
                        .extend(message.encode(self.sequence.next_sequence()));
                }
                Output::Event(event) => {
                    if let Some(utc) = utc {
                        fx.db.extend(self.link_event(utc, event));
                    }
                }
            }
        }
    }

    fn link_event(&self, utc: Timestamp, event: LinkEvent) -> Vec<DbCommand> {
        let identity = self.link.identity();
        let firmware = self.link.config().map(|c| c.firmware);
        let system = |ty, detail: String| {
            DbCommand::Event(NewEvent {
                firmware,
                identity,
                ..NewEvent::system(utc, ty, Some(detail))
            })
        };
        match event {
            LinkEvent::StateChange { from, to } => {
                vec![system(
                    SystemType::StateChange,
                    format!("link {from} -> {to}"),
                )]
            }
            LinkEvent::Repeated { from, to, count } => vec![system(
                SystemType::StateChange,
                format!("link {from} -> {to} repeated {count} times within 10 s"),
            )],
            LinkEvent::McuRestart { previous, current } => vec![system(
                SystemType::McuRestart,
                format!("boot {} -> {}", previous.boot_counter, current.boot_counter),
            )],
            LinkEvent::ConfigChange { previous, current } => vec![system(
                SystemType::ConfigChange,
                format!(
                    "revision {} -> {}",
                    previous.config_revision, current.config_revision
                ),
            )],
            LinkEvent::ConfigAccepted(config) => {
                let event = DbCommand::Event(NewEvent {
                    firmware: Some(config.firmware),
                    identity: Some(config.identity()),
                    ..NewEvent::system(
                        utc,
                        SystemType::ConfigAccepted,
                        Some(format!(
                            "boot {} revision {}, firmware {}",
                            config.boot_counter, config.config_revision, config.firmware
                        )),
                    )
                });
                vec![
                    DbCommand::McuConfig {
                        received: utc,
                        config,
                        status: AckStatus::Accepted,
                    },
                    event,
                ]
            }
            LinkEvent::ConfigRejected { config, rejection } => {
                let event = DbCommand::Event(NewEvent {
                    firmware: Some(config.firmware),
                    identity: Some(config.identity()),
                    ..NewEvent::system(utc, SystemType::ConfigRejected, Some(rejection.to_string()))
                });
                vec![
                    DbCommand::McuConfig {
                        received: utc,
                        config,
                        status: rejection.status(),
                    },
                    event,
                ]
            }
            LinkEvent::FirmwareChange { previous, current } => vec![system(
                SystemType::FwChange,
                format!("{previous} -> {current}"),
            )],
        }
    }
}

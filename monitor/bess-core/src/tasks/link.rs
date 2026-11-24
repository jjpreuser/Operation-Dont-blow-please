//! The `link_supervisor` task (docs/design/architecture.md §2,
//! lifecycle.md §2–3; DD-26, DD-27, DD-70, DD-80, DD-90).
//!
//! It runs the [`LinkSupervisor`] on every frame from `uart_rx` and on its
//! timers, and then publishes, in this order:
//! 1. the link as a [`LinkView`] on its `watch` ("mcu config"), if it
//!    changed;
//! 2. `CONFIG_REQUEST`, `CONFIG_ACK` and HEARTBEAT to `uart_tx`;
//! 3. the link events and CONFIG rows to `db_writer` (with a valid clock,
//!    DD-11);
//! 4. for a TELEMETRY while ONLINE, the 1 s sample: into the ring buffer,
//!    then on the sample stream;
//! 5. the frame on the fault-frame broadcast, stamped with the link after
//!    it.
//!
//! So a task that gets a frame finds the link watch and the ring buffer at
//! least as new as that frame.

use super::hub::{now, send_db, Clock, FrameKind, McuFrame, RxFrame, Sampled, SharedRing};
use crate::db::{NewEvent, SystemType};
use crate::fan::FanDemand;
use crate::link::{LinkEvent, LinkState, LinkSupervisor, LinkView, Output};
use crate::sample::Sample;
use crate::signals::Signals;
use crate::writer::DbCommand;
use jiff::Timestamp;
use protocol::config::FirmwareVersion;
use protocol::messages::{AckStatus, Message, Telemetry};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc, watch};

/// What one step of the link task produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LinkStep {
    /// For `uart_tx`, in order.
    pub to_mcu: Vec<Message>,
    pub db: Vec<DbCommand>,
    /// The 1 s sample of this frame, if any.
    pub sample: Option<Sampled>,
    /// The frame for the fault-frame broadcast.
    pub frame: Option<McuFrame>,
}

/// The `link_supervisor` task's logic around [`LinkSupervisor`].
#[derive(Debug, Clone)]
pub struct LinkTask {
    sup: LinkSupervisor,
}

impl LinkTask {
    /// At core start; `last_firmware` from the newest accepted CONFIG in the
    /// database, so a firmware update made while the Pi was off is logged
    /// (DD-90).
    #[must_use]
    pub fn new(now: Instant, last_firmware: Option<FirmwareVersion>) -> LinkTask {
        LinkTask {
            sup: LinkSupervisor::new(now).with_last_firmware(last_firmware),
        }
    }

    #[must_use]
    pub fn supervisor(&self) -> &LinkSupervisor {
        &self.sup
    }

    #[must_use]
    pub fn view(&self) -> LinkView {
        self.sup.view()
    }

    #[must_use]
    pub fn next_deadline(&self) -> Instant {
        self.sup.next_deadline()
    }

    /// One frame from `uart_rx`. `fan` is the duty now, stored with the
    /// sample (S10.3).
    pub fn on_frame(&mut self, rx: &RxFrame, fan: Option<FanDemand>) -> LinkStep {
        let mut step = LinkStep::default();
        if let Some(message) = &rx.message {
            let outputs = self.sup.on_message(rx.at, message);
            self.outputs(rx.utc, outputs, &mut step);
        }
        let kind = match &rx.message {
            Some(Message::Telemetry(t)) => FrameKind::Telemetry {
                telemetry: *t,
                signals: self.signals(t, fan),
            },
            Some(Message::FaultUpdate(t)) => FrameKind::FaultUpdate {
                telemetry: *t,
                signals: self.signals(t, fan),
            },
            _ => FrameKind::Other,
        };
        let frame = McuFrame {
            at: rx.at,
            utc: rx.utc,
            sequence: rx.sequence,
            link: self.sup.view(),
            kind,
        };
        step.sample = frame
            .sample()
            .zip(self.period())
            .map(|(entry, period)| Sampled { entry, period });
        step.frame = Some(frame);
        step
    }

    /// The timers: link timeout, `CONFIG_REQUEST`, HEARTBEAT, the DD-80 rate
    /// limit.
    pub fn poll(&mut self, now: Instant, utc: Option<Timestamp>) -> LinkStep {
        let mut step = LinkStep::default();
        let outputs = self.sup.poll(now);
        self.outputs(utc, outputs, &mut step);
        step
    }

    /// Measurements only with the installed CONFIG, only ONLINE (S2.4,
    /// DD-69).
    fn signals(&self, t: &Telemetry, fan: Option<FanDemand>) -> Option<Signals> {
        let config = self.sup.config().filter(|_| self.sup.shows_values())?;
        Some(Signals {
            sample: Sample::from_telemetry(t, config.ntc_present_mask),
            fan_duty_pct: fan.map(|f| f64::from(f.duty_pct)),
            fan1_rpm: None,
            fan2_rpm: None,
        })
    }

    fn period(&self) -> Option<Duration> {
        self.sup
            .config()
            .map(|c| Duration::from_millis(u64::from(c.telemetry_ms)))
    }

    fn outputs(&self, utc: Option<Timestamp>, outputs: Vec<Output>, step: &mut LinkStep) {
        for output in outputs {
            match output {
                Output::Send(message) => step.to_mcu.push(message),
                // Events need a valid clock (DD-11); the DD-31 queue is still
                // to do.
                Output::Event(event) => {
                    if let Some(utc) = utc {
                        step.db.extend(self.link_event(utc, event));
                    }
                }
            }
        }
    }

    fn link_event(&self, utc: Timestamp, event: LinkEvent) -> Vec<DbCommand> {
        let identity = self.sup.identity();
        let firmware = self.sup.config().map(|c| c.firmware);
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

/// The channels of the `link_supervisor` task.
#[derive(Debug)]
pub struct LinkIo {
    /// Frames from `uart_rx`; closed when `uart_rx` stops.
    pub from_rx: mpsc::Receiver<RxFrame>,
    pub to_tx: mpsc::Sender<Message>,
    pub view: watch::Sender<LinkView>,
    pub frames: broadcast::Sender<McuFrame>,
    pub samples: broadcast::Sender<Sampled>,
    pub ring: SharedRing,
    pub fan: watch::Receiver<Option<FanDemand>>,
    pub db: std::sync::mpsc::Sender<DbCommand>,
}

/// Runs the task until `uart_rx` stops; returns the last link state.
/// Returning drops the link watch, both broadcasts and the queue to
/// `uart_tx`, which ends the tasks that read them.
pub async fn run(mut task: LinkTask, mut io: LinkIo, clock: Clock) -> LinkState {
    loop {
        let deadline = tokio::time::Instant::from_std(task.next_deadline());
        let step = tokio::select! {
            rx = io.from_rx.recv() => {
                let Some(rx) = rx else { break };
                let fan = *io.fan.borrow();
                task.on_frame(&rx, fan)
            }
            () = tokio::time::sleep_until(deadline) => task.poll(now(), clock()),
        };
        publish(&task, step, &io).await;
    }
    task.view().state
}

async fn publish(task: &LinkTask, step: LinkStep, io: &LinkIo) {
    let view = task.view();
    io.view.send_if_modified(|v| {
        let changed = *v != view;
        if changed {
            *v = view;
        }
        changed
    });
    for message in step.to_mcu {
        // `uart_tx` only stops after this task.
        let _ = io.to_tx.send(message).await;
    }
    for command in step.db {
        send_db(&io.db, command);
    }
    if let Some(sample) = step.sample {
        io.ring.lock().push(sample.entry);
        // No receiver is not an error: at shutdown the readers go first.
        let _ = io.samples.send(sample);
    }
    if let Some(frame) = step.frame {
        let _ = io.frames.send(frame);
    }
}

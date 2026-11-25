//! BMS link supervisor: OFFLINE / `WAIT_CONFIG` / ONLINE (docs/design/lifecycle.md
//! §2–3; DD-26, DD-27, DD-28, DD-55, DD-80; UART draft §4, §8).
//!
//! [`LinkSupervisor`] is the `link_supervisor` task's logic without the task.
//! The caller feeds it every valid MCU frame ([`LinkSupervisor::on_message`])
//! and wakes it at [`LinkSupervisor::next_deadline`] ([`LinkSupervisor::poll`]).
//! Both return [`Output`]s: messages for `uart_tx` and events for `db_writer`.
//! Time is the monotonic clock ([`Instant`]), never wall time, so an NTP step
//! cannot take the link down (D5 §2).
//!
//! The rules, beyond the D5 state diagram:
//! - Only TELEMETRY and `FAULT_UPDATE` carry the MCU identity (`boot_counter`
//!   and `config_revision`, §4). Any valid MCU frame restarts the link timeout,
//!   but only these two move the link out of OFFLINE.
//! - A changed identity drops the installed CONFIG (DD-55), logs
//!   `MCU_RESTART` (new `boot_counter`) or `CONFIG_CHANGE` (same boot, new
//!   revision) and leads to `WAIT_CONFIG`. With the identity unchanged, a link
//!   that comes back resumes ONLINE without reading the CONFIG again (DD-27).
//! - A CONFIG is checked ([`Config::check`]) against the latest telemetry
//!   identity and answered with a `CONFIG_ACK` in `WAIT_CONFIG`, and in ONLINE
//!   when it has the installed identity (a repeat is acknowledged again). The
//!   MCU also sends one CONFIG on its own after start-up (§8); one that arrives
//!   while OFFLINE, or in ONLINE with another identity, is not answered: the
//!   next TELEMETRY shows the new identity and the Pi asks for it.
//! - A rejected CONFIG is acknowledged with its status every time, but logged
//!   once per CONFIG identity (D5 §2).
//! - The link timeout (`pi_data_timeout_ms`) and the heartbeat period
//!   (`heartbeat_ms`) come from the installed CONFIG (DD-24); without one they
//!   are 3 s and 1 s. `CONFIG_REQUEST` goes out every 1 s in `WAIT_CONFIG` (§8).
//! - HEARTBEAT carries the installed identity, or zero without one (§8). It is
//!   sent in every link state, since the MCU may not hear the Pi while the Pi
//!   still hears the MCU. Stopping it when the fan controller is not alive
//!   (DD-26) is the job of `uart_tx`, which knows both liveness flags.
//! - Every link state change is logged as a `STATE_CHANGE` event at once,
//!   except a repeat: the same change (say ONLINE -> OFFLINE) within 10 s of
//!   its last log is counted, and the count is logged as
//!   [`LinkEvent::Repeated`] when the 10 s are over. So a loose connector
//!   logs at most one event per kind of change per 10 s (DD-80).

use protocol::config::{Config, FirmwareVersion, Rejection};
use protocol::messages::{AckStatus, CONFIG_LEN, Heartbeat, Identity, Message};
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Link timeout before a CONFIG is installed (D5 §2, S8.12).
pub const DEFAULT_DATA_TIMEOUT: Duration = Duration::from_secs(3);
/// Heartbeat period before a CONFIG is installed (DD-26).
pub const DEFAULT_HEARTBEAT_PERIOD: Duration = Duration::from_secs(1);
/// `CONFIG_REQUEST` period in `WAIT_CONFIG` (§8).
pub const CONFIG_REQUEST_PERIOD: Duration = Duration::from_secs(1);
/// A link state change repeated within this time of its last log is
/// counted, not logged one by one (DD-80).
pub const FLAP_WINDOW: Duration = Duration::from_secs(10);

/// The BMS link state (D5 §2, DD-27).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinkState {
    /// No valid frame for the link timeout, or none yet since core start.
    /// Shows "BMS communication lost" (S8.12).
    Offline,
    /// Frames arrive, but no CONFIG is installed for their identity. Faults
    /// are shown, measurements are not (S2.4, DD-69).
    WaitConfig,
    /// A CONFIG for the current identity is installed and acknowledged.
    Online,
}

impl fmt::Display for LinkState {
    /// The names used in the event log (`ONLINE -> OFFLINE`).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            LinkState::Offline => "OFFLINE",
            LinkState::WaitConfig => "WAIT_CONFIG",
            LinkState::Online => "ONLINE",
        })
    }
}

/// Something for the event log or the `mcu_config` table (D2, DD-43).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkEvent {
    /// SYSTEM `STATE_CHANGE`, `detail` = `from -> to` (DD-80).
    StateChange { from: LinkState, to: LinkState },
    /// SYSTEM `STATE_CHANGE` with a count: the change `from -> to` happened
    /// `count` more times within [`FLAP_WINDOW`] after it was last logged
    /// (DD-80).
    Repeated {
        from: LinkState,
        to: LinkState,
        count: u32,
    },
    /// `MCU_RESTART`: the `boot_counter` changed.
    McuRestart {
        previous: Identity,
        current: Identity,
    },
    /// `CONFIG_CHANGE`: same boot, new `config_revision`.
    ConfigChange {
        previous: Identity,
        current: Identity,
    },
    /// `CONFIG_ACCEPTED`: this CONFIG is now installed; store it with its raw
    /// bytes (`Config::to_bytes`) in `mcu_config` (DD-43).
    ConfigAccepted(Box<Config>),
    /// `CONFIG_REJECTED`, once per CONFIG identity; store it in `mcu_config`
    /// as rejected (DD-43).
    ConfigRejected {
        config: Box<Config>,
        rejection: Rejection,
    },
    /// `FW_CHANGE` (S7.5): an accepted CONFIG carries another firmware
    /// version than the one accepted before it (in this run, or the one
    /// given to [`LinkSupervisor::with_last_firmware`]).
    FirmwareChange {
        previous: FirmwareVersion,
        current: FirmwareVersion,
    },
}

/// What [`LinkSupervisor`] asks the caller to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    /// Send this message to the MCU (via `uart_tx`, DD-70).
    Send(Message),
    /// Log this (via `db_writer`, DD-15).
    Event(LinkEvent),
}

/// One kind of link state change and its rate-limit window.
#[derive(Debug, Clone, Copy)]
struct Window {
    from: LinkState,
    to: LinkState,
    end: Instant,
    /// Repeats inside the window, not logged yet.
    repeats: u32,
}

/// Rate-limits the logging of link state changes (DD-80), per kind of
/// change, so a normal start-up (OFFLINE -> `WAIT_CONFIG` -> ONLINE) is
/// logged in full while a loose connector is not.
#[derive(Debug, Clone, Default)]
struct FlapFilter {
    /// At most one per kind of change, so at most 6.
    windows: Vec<Window>,
}

impl FlapFilter {
    fn change(&mut self, from: LinkState, to: LinkState, now: Instant, out: &mut Vec<Output>) {
        if let Some(w) = self
            .windows
            .iter_mut()
            .find(|w| w.from == from && w.to == to)
        {
            w.repeats += 1;
        } else {
            out.push(Output::Event(LinkEvent::StateChange { from, to }));
            self.windows.push(Window {
                from,
                to,
                end: now + FLAP_WINDOW,
                repeats: 0,
            });
        }
    }

    /// Closes the windows that are over: one that counted repeats reports
    /// them and opens the next window; one that counted none is removed.
    fn flush(&mut self, now: Instant, out: &mut Vec<Output>) {
        self.windows.retain_mut(|w| {
            if now < w.end {
                return true;
            }
            if w.repeats == 0 {
                return false;
            }
            out.push(Output::Event(LinkEvent::Repeated {
                from: w.from,
                to: w.to,
                count: w.repeats,
            }));
            w.repeats = 0;
            w.end = now + FLAP_WINDOW;
            true
        });
    }

    fn next_end(&self) -> Option<Instant> {
        self.windows.iter().map(|w| w.end).min()
    }
}

/// The link as the other tasks see it: published by the `link_supervisor`
/// task on a `watch` and carried with every MCU frame it forwards
/// (docs/design/architecture.md §2 "mcu config"; DD-24, DD-70).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkView {
    pub state: LinkState,
    /// The installed CONFIG, the only source of limits, capacity and fan
    /// tables (DD-24).
    pub config: Option<Arc<Config>>,
    /// Identity of the latest TELEMETRY or `FAULT_UPDATE`.
    pub identity: Option<Identity>,
    /// How often the link went OFFLINE since core start. A task that sees
    /// it change knows the link was down in between (DD-37), even when it
    /// never saw the OFFLINE state itself.
    pub outages: u32,
}

impl Default for LinkView {
    /// Core start: OFFLINE, nothing known.
    fn default() -> LinkView {
        LinkView {
            state: LinkState::Offline,
            config: None,
            identity: None,
            outages: 0,
        }
    }
}

impl LinkView {
    /// The installed CONFIG.
    #[must_use]
    pub fn config(&self) -> Option<&Config> {
        self.config.as_deref()
    }

    /// Live measurements may be shown and sampled: only ONLINE (S2.4).
    #[must_use]
    pub fn shows_values(&self) -> bool {
        self.state == LinkState::Online
    }

    /// The fault bitmap of a frame is used: `WAIT_CONFIG` and ONLINE (DD-69).
    #[must_use]
    pub fn shows_faults(&self) -> bool {
        self.state != LinkState::Offline
    }
}

/// The `link_supervisor` logic (D5 §2).
#[derive(Debug, Clone)]
pub struct LinkSupervisor {
    state: LinkState,
    /// When the last valid MCU frame arrived.
    last_frame: Option<Instant>,
    /// Identity of the latest TELEMETRY or `FAULT_UPDATE`.
    identity: Option<Identity>,
    /// The installed CONFIG. Kept while OFFLINE so an unchanged identity can
    /// resume ONLINE (DD-27); dropped when the identity changes (DD-55).
    config: Option<Arc<Config>>,
    /// Firmware of the last accepted CONFIG, kept when the CONFIG is dropped.
    firmware: Option<FirmwareVersion>,
    /// CONFIG identity whose rejection has been logged.
    rejected: Option<Identity>,
    /// How often the link went OFFLINE ([`LinkView::outages`]).
    outages: u32,
    next_heartbeat: Instant,
    next_request: Instant,
    flap: FlapFilter,
}

/// The next time a periodic message is due: one period on, or one period
/// from now if the caller woke up later than that.
fn advance(due: Instant, period: Duration, now: Instant) -> Instant {
    let next = due + period;
    if next <= now { now + period } else { next }
}

impl LinkSupervisor {
    /// A supervisor at core start: OFFLINE, no CONFIG, the first heartbeat
    /// (identity zero) due at once.
    #[must_use]
    pub fn new(now: Instant) -> LinkSupervisor {
        LinkSupervisor {
            state: LinkState::Offline,
            last_frame: None,
            identity: None,
            config: None,
            firmware: None,
            rejected: None,
            outages: 0,
            next_heartbeat: now,
            next_request: now,
            flap: FlapFilter::default(),
        }
    }

    /// Seeds the firmware version of the last accepted CONFIG from an
    /// earlier run (the latest accepted `mcu_config` row, DD-43), so
    /// `FW_CHANGE` also catches an update made while the Pi was off.
    #[must_use]
    pub fn with_last_firmware(mut self, firmware: Option<FirmwareVersion>) -> LinkSupervisor {
        self.firmware = firmware;
        self
    }

    #[must_use]
    pub fn state(&self) -> LinkState {
        self.state
    }

    /// Whether live measurements may be shown and sampled: only ONLINE
    /// (S2.4). Read it after [`on_message`](Self::on_message): a TELEMETRY
    /// with a new identity has already moved the link to `WAIT_CONFIG`, so its
    /// values are never used with the old CONFIG.
    #[must_use]
    pub fn shows_values(&self) -> bool {
        self.state == LinkState::Online
    }

    /// Whether the fault bitmap of a frame is used: `WAIT_CONFIG` and ONLINE
    /// (DD-69). Nothing is raised or cleared while OFFLINE (DD-37).
    #[must_use]
    pub fn shows_faults(&self) -> bool {
        self.state != LinkState::Offline
    }

    /// The installed CONFIG, the only source of limits, capacity and fan
    /// tables (DD-24). `None` means the fans run at 100 % (DD-28, DD-55).
    #[must_use]
    pub fn config(&self) -> Option<&Config> {
        self.config.as_deref()
    }

    /// Identity of the latest TELEMETRY or `FAULT_UPDATE`, also while OFFLINE.
    #[must_use]
    pub fn identity(&self) -> Option<Identity> {
        self.identity
    }

    /// What the other tasks need of the link (the `watch` of
    /// docs/design/architecture.md §2).
    #[must_use]
    pub fn view(&self) -> LinkView {
        LinkView {
            state: self.state,
            config: self.config.clone(),
            identity: self.identity,
            outages: self.outages,
        }
    }

    /// The HEARTBEAT payload: the installed identity, or zero (§8).
    #[must_use]
    pub fn heartbeat(&self) -> Heartbeat {
        self.config
            .as_ref()
            .map_or(Heartbeat::WAITING, |c| Heartbeat {
                accepted_boot_counter: c.boot_counter,
                accepted_config_revision: c.config_revision,
            })
    }

    /// `pi_data_timeout_ms` of the installed CONFIG, else 3 s.
    #[must_use]
    pub fn data_timeout(&self) -> Duration {
        self.config
            .as_ref()
            .map_or(DEFAULT_DATA_TIMEOUT, |c| ms(c.pi_data_timeout_ms))
    }

    /// `heartbeat_ms` of the installed CONFIG, else 1 s.
    #[must_use]
    pub fn heartbeat_period(&self) -> Duration {
        self.config
            .as_ref()
            .map_or(DEFAULT_HEARTBEAT_PERIOD, |c| ms(c.heartbeat_ms))
    }

    /// When [`poll`](Self::poll) next has something to do. Calling it earlier
    /// is harmless; calling it later delays that work by as much.
    #[must_use]
    pub fn next_deadline(&self) -> Instant {
        let mut next = self.next_heartbeat;
        if self.state == LinkState::WaitConfig {
            next = next.min(self.next_request);
        }
        if let (true, Some(last)) = (self.state != LinkState::Offline, self.last_frame) {
            next = next.min(last + self.data_timeout());
        }
        if let Some(end) = self.flap.next_end() {
            next = next.min(end);
        }
        next
    }

    /// Handles one valid frame from the MCU (CRC and length already checked
    /// by the parser). Pi → MCU messages are ignored.
    pub fn on_message(&mut self, now: Instant, message: &Message) -> Vec<Output> {
        let mut out = Vec::new();
        self.flap.flush(now, &mut out);
        match message {
            Message::Telemetry(t) | Message::FaultUpdate(t) => {
                self.last_frame = Some(now);
                self.on_identity(t.identity(), now, &mut out);
            }
            Message::Config(raw) => {
                self.last_frame = Some(now);
                self.on_config(raw, now, &mut out);
            }
            Message::ResetAck(_) => self.last_frame = Some(now),
            Message::Heartbeat(_)
            | Message::ConfigAck(_)
            | Message::ConfigRequest
            | Message::FaultReset(_) => {}
        }
        out
    }

    /// Timeouts and periodic messages due at `now`.
    pub fn poll(&mut self, now: Instant) -> Vec<Output> {
        let mut out = Vec::new();
        self.flap.flush(now, &mut out);
        if let (true, Some(last)) = (self.state != LinkState::Offline, self.last_frame)
            && now.saturating_duration_since(last) >= self.data_timeout()
        {
            self.enter(LinkState::Offline, now, &mut out);
        }
        if self.state == LinkState::WaitConfig && now >= self.next_request {
            out.push(Output::Send(Message::ConfigRequest));
            self.next_request = advance(self.next_request, CONFIG_REQUEST_PERIOD, now);
        }
        if now >= self.next_heartbeat {
            out.push(Output::Send(Message::Heartbeat(self.heartbeat())));
            self.next_heartbeat = advance(self.next_heartbeat, self.heartbeat_period(), now);
        }
        out
    }

    fn on_identity(&mut self, current: Identity, now: Instant, out: &mut Vec<Output>) {
        if let Some(previous) = self.identity.filter(|&p| p != current) {
            out.push(Output::Event(
                if previous.boot_counter == current.boot_counter {
                    LinkEvent::ConfigChange { previous, current }
                } else {
                    LinkEvent::McuRestart { previous, current }
                },
            ));
        }
        self.identity = Some(current);
        if self
            .config
            .as_ref()
            .is_some_and(|c| c.identity() != current)
        {
            self.config = None; // DD-55: fans to 100 % until the new CONFIG
        }
        let next = if self.config.is_some() {
            LinkState::Online
        } else {
            LinkState::WaitConfig
        };
        self.enter(next, now, out);
    }

    fn on_config(&mut self, raw: &[u8; CONFIG_LEN], now: Instant, out: &mut Vec<Output>) {
        let Some(telemetry) = self.identity else {
            return;
        };
        let config = Config::from_bytes(raw);
        let answer = match self.state {
            LinkState::Offline => false,
            LinkState::WaitConfig => true,
            LinkState::Online => {
                self.config.as_ref().map(|c| c.identity()) == Some(config.identity())
            }
        };
        if !answer {
            return;
        }
        match config.check(telemetry) {
            Ok(()) => {
                out.push(Output::Send(Message::ConfigAck(
                    config.ack(AckStatus::Accepted),
                )));
                if self.config.as_deref() != Some(&config) {
                    self.install(config, now, out);
                }
                self.enter(LinkState::Online, now, out);
            }
            Err(rejection) => {
                out.push(Output::Send(Message::ConfigAck(
                    config.ack(rejection.status()),
                )));
                if self.rejected != Some(config.identity()) {
                    self.rejected = Some(config.identity());
                    out.push(Output::Event(LinkEvent::ConfigRejected {
                        config: Box::new(config),
                        rejection,
                    }));
                }
            }
        }
    }

    /// Installs an accepted CONFIG and sends a heartbeat with its identity at
    /// once, so the MCU need not wait up to a period to stop inhibiting
    /// charging (D5 §1).
    fn install(&mut self, config: Config, now: Instant, out: &mut Vec<Output>) {
        if let Some(previous) = self.firmware.filter(|&p| p != config.firmware) {
            out.push(Output::Event(LinkEvent::FirmwareChange {
                previous,
                current: config.firmware,
            }));
        }
        self.firmware = Some(config.firmware);
        out.push(Output::Event(LinkEvent::ConfigAccepted(Box::new(
            config.clone(),
        ))));
        self.config = Some(Arc::new(config));
        out.push(Output::Send(Message::Heartbeat(self.heartbeat())));
        self.next_heartbeat = now + self.heartbeat_period();
    }

    fn enter(&mut self, to: LinkState, now: Instant, out: &mut Vec<Output>) {
        let from = self.state;
        if from == to {
            return;
        }
        self.state = to;
        if to == LinkState::Offline {
            self.outages = self.outages.wrapping_add(1);
        }
        self.flap.change(from, to, now, out);
        if to == LinkState::WaitConfig {
            out.push(Output::Send(Message::ConfigRequest));
            self.next_request = now + CONFIG_REQUEST_PERIOD;
        }
    }
}

fn ms(value: u16) -> Duration {
    Duration::from_millis(u64::from(value))
}

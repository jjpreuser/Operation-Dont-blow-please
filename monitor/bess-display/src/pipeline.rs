//! The Pi side of the preview: the real `bess-core` logic between the
//! simulated MCU and the screen, in one thread.
//!
//! Bytes from the MCU go through the `protocol` stream parser, then to the
//! link supervisor, the live tracker and the fan controller; messages the
//! link supervisor asks to send are encoded and handed back to the MCU.
//! In the real system these run as `bess-core` tasks and the screen gets
//! the live state over the Unix socket (DD-02, DD-14); this file stands in
//! for that until `bess-api` exists.

use bess_core::colour::WarningLevels;
use bess_core::fan::{FanController, FanDemand, FanInputs};
use bess_core::link::{LinkEvent, LinkSupervisor, Output};
use bess_core::live::{LiveState, LiveTracker};
use protocol::frame::{Direction, SequenceCounter};
use protocol::messages::Message;
use protocol::parser::{FrameParser, ParserStats};
use std::collections::VecDeque;
use std::time::Instant;

/// Events kept for the demo panel.
const EVENT_LOG_LEN: usize = 8;

pub struct Pipeline {
    parser: FrameParser,
    link: LinkSupervisor,
    live: LiveTracker,
    fans: FanController,
    fan: Option<FanDemand>,
    sequence: SequenceCounter,
    warn: WarningLevels,
    started: Instant,
    /// Newest first: seconds since start and the event text.
    pub events: VecDeque<(f64, String)>,
}

impl Pipeline {
    #[must_use]
    pub fn new(now: Instant) -> Pipeline {
        Pipeline {
            parser: FrameParser::new(Direction::McuToPi),
            link: LinkSupervisor::new(now),
            live: LiveTracker::new(),
            fans: FanController::new(),
            fan: None,
            sequence: SequenceCounter::new(0),
            warn: WarningLevels::default(),
            started: now,
            events: VecDeque::new(),
        }
    }

    /// Feeds bytes from the MCU, runs the timers and returns the bytes to
    /// send to the MCU.
    pub fn step(&mut self, now: Instant, from_mcu: &[u8]) -> Vec<u8> {
        let mut to_mcu = Vec::new();
        for frame in self.parser.push(from_mcu) {
            let Ok(message) = Message::try_from(&frame) else {
                continue;
            };
            let out = self.link.on_message(now, &message);
            self.handle(now, out, &mut to_mcu);
            if let Message::Telemetry(t) | Message::FaultUpdate(t) = &message {
                self.live.on_frame(t, &self.link);
            }
            self.run_fans(now);
        }
        let out = self.link.poll(now);
        self.handle(now, out, &mut to_mcu);
        // The 1 s timer of DD-51; running it more often changes nothing.
        self.run_fans(now);
        to_mcu
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

    fn handle(&mut self, now: Instant, outputs: Vec<Output>, to_mcu: &mut Vec<u8>) {
        for output in outputs {
            match output {
                Output::Send(message) => {
                    to_mcu.extend(message.encode(self.sequence.next_sequence()));
                }
                Output::Event(event) => {
                    let at = now.saturating_duration_since(self.started).as_secs_f64();
                    self.events.push_front((at, describe(&event)));
                    self.events.truncate(EVENT_LOG_LEN);
                }
            }
        }
    }

    #[must_use]
    pub fn view(&self) -> LiveState {
        self.live.view(&self.link, self.fan, &self.warn)
    }

    #[must_use]
    pub fn stats(&self) -> ParserStats {
        self.parser.stats()
    }
}

fn describe(event: &LinkEvent) -> String {
    match event {
        LinkEvent::StateChange { from, to } => format!("STATE_CHANGE {from} -> {to}"),
        LinkEvent::Repeated { from, to, count } => {
            format!("STATE_CHANGE {from} -> {to} repeated {count}x")
        }
        LinkEvent::McuRestart { previous, current } => format!(
            "MCU_RESTART boot {} -> {}",
            previous.boot_counter, current.boot_counter
        ),
        LinkEvent::ConfigChange { previous, current } => format!(
            "CONFIG_CHANGE revision {} -> {}",
            previous.config_revision, current.config_revision
        ),
        LinkEvent::ConfigAccepted(c) => format!(
            "CONFIG_ACCEPTED boot {} revision {}",
            c.boot_counter, c.config_revision
        ),
        LinkEvent::ConfigRejected { rejection, .. } => format!("CONFIG_REJECTED {rejection}"),
        LinkEvent::FirmwareChange { previous, current } => {
            format!("FW_CHANGE {previous} -> {current}")
        }
    }
}

#[cfg(test)]
mod tests {
    //! End to end over the UART bytes: simulated MCU, real parser, link
    //! supervisor, fan controller and live tracker.

    use super::Pipeline;
    use crate::sim::SimMcu;
    use bess_core::fan::Reason;
    use bess_core::link::LinkState;
    use bess_core::live::{LiveState, Values};
    use bess_core::state::ShownState;
    use std::time::{Duration, Instant};

    struct Loop {
        sim: SimMcu,
        pi: Pipeline,
        to_mcu: Vec<u8>,
        now: Instant,
    }

    impl Loop {
        fn new() -> Loop {
            let now = Instant::now();
            Loop {
                sim: SimMcu::new(now),
                pi: Pipeline::new(now),
                to_mcu: Vec::new(),
                now,
            }
        }

        /// Runs `ms` milliseconds in 100 ms steps.
        fn run(&mut self, ms: u64) -> LiveState {
            for _ in 0..ms / 100 {
                self.now += Duration::from_millis(100);
                let pending = std::mem::take(&mut self.to_mcu);
                if !self.sim.controls.link_cut {
                    self.sim.receive(&pending, self.now);
                }
                let bytes = self.sim.step(self.now);
                self.to_mcu = self.pi.step(self.now, &bytes);
            }
            self.pi.view()
        }
    }

    #[test]
    fn handshake_reaches_online_and_the_mcu_hears_its_identity() {
        let mut l = Loop::new();
        let v = l.run(2500);
        assert_eq!(v.link, LinkState::Online);
        assert!(matches!(v.values, Values::Live(..)));
        assert_eq!(v.shown.state, ShownState::Charging);
        assert_eq!(l.sim.pi_heartbeat(l.now), Some(l.sim.identity()));
        assert_eq!(l.pi.stats().errors(), 0);
    }

    #[test]
    fn cut_link_greys_the_values_and_recovers() {
        let mut l = Loop::new();
        l.run(2500);
        l.sim.controls.link_cut = true;
        let v = l.run(4000);
        assert_eq!(v.link, LinkState::Offline);
        assert!(v.shown.comm_lost);
        assert!(matches!(v.values, Values::Stale(_)));
        l.sim.controls.link_cut = false;
        let v = l.run(1500);
        assert_eq!(v.link, LinkState::Online);
        assert!(matches!(v.values, Values::Live(..)));
    }

    #[test]
    fn mcu_restart_goes_through_wait_config_to_online() {
        let mut l = Loop::new();
        l.run(2500);
        l.sim.restart(l.now);
        let v = l.run(2500);
        assert_eq!(v.link, LinkState::Online);
        assert_eq!(l.sim.pi_heartbeat(l.now), Some(l.sim.identity()));
        assert!(l
            .pi
            .events
            .iter()
            .any(|(_, e)| e.starts_with("MCU_RESTART")));
    }

    #[test]
    fn e_stop_switches_fans_off_and_shows_the_banner() {
        let mut l = Loop::new();
        l.run(2500);
        l.sim.controls.e_stop = true;
        let v = l.run(300); // FAULT_UPDATE, not the next 1 s frame
        assert_eq!(v.shown.state, ShownState::EStop);
        assert!(v.critical_banner);
        assert_eq!(
            v.fan.map(|f| (f.duty_pct, f.reason)),
            Some((0, Reason::EStop))
        );
    }

    #[test]
    fn hot_battery_runs_the_fans_at_100_with_e05() {
        let mut l = Loop::new();
        l.run(2500);
        l.sim.controls.cell_temp_c = 60.0;
        let v = l.run(1500);
        assert_eq!(
            v.fan.map(|f| (f.duty_pct, f.reason)),
            Some((100, Reason::E05))
        );
        assert_eq!(v.shown.state, ShownState::Fault);
        assert_eq!(v.faults[0].name, "OVERHEATING");
    }
}

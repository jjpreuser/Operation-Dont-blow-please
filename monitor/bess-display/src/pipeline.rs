//! The Pi side of the preview: the real `bess-core` core ([`Core::run`],
//! every task of D1) against the demo MCU, over the UART bytes of a
//! [`link_pair`] instead of a serial port.
//!
//! The screen reads the core's live-state `watch`, as `api_server` will
//! (DD-14). The preview keeps no database: a thread takes the commands
//! meant for `db_writer` and keeps the newest events for the demo panel,
//! with the text the core logs.

use crate::sim::DemoMcu;
use bess_core::detector::Write;
use bess_core::link::LinkView;
use bess_core::live::LiveState;
use bess_core::runtime::{Core, LinkEnd, interim_clock, link_pair};
use bess_core::tasks::hub;
use bess_core::writer::DbCommand;
use protocol::messages::FaultBits;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// Events kept for the demo panel.
const EVENT_LOG_LEN: usize = 8;

/// How often the demo MCU runs.
const MCU_STEP: Duration = Duration::from_millis(100);

/// The running preview. Its tasks run on the tokio runtime it was started
/// on.
pub struct Preview {
    pub mcu: Arc<Mutex<DemoMcu>>,
    pub live: watch::Receiver<LiveState>,
    pub link: watch::Receiver<LinkView>,
    /// Newest first: seconds since start and the event text.
    pub events: Arc<Mutex<VecDeque<(f64, String)>>>,
}

/// Starts the core and the demo MCU on the current tokio runtime.
///
/// # Panics
///
/// Outside a tokio runtime.
#[must_use]
pub fn start() -> Preview {
    let mcu = Arc::new(Mutex::new(DemoMcu::new(hub::now())));
    let events = Arc::new(Mutex::new(VecDeque::new()));
    let (link, end) = link_pair();
    let core = Core::new(jiff::tz::TimeZone::system(), FaultBits(0), None);
    let (live, link_rx) = (core.live(), core.link());
    tokio::spawn(core.run(
        link,
        event_log(events.clone()),
        Arc::new(interim_clock),
        std::future::pending(),
    ));
    tokio::spawn(run_mcu(mcu.clone(), end));
    Preview {
        mcu,
        live,
        link: link_rx,
        events,
    }
}

/// Locks a mutex; a panic elsewhere does not stop the preview.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The demo MCU every [`MCU_STEP`]: the bytes from the core in, its frames
/// out.
async fn run_mcu(mcu: Arc<Mutex<DemoMcu>>, mut end: LinkEnd) {
    let mut tick = tokio::time::interval(MCU_STEP);
    loop {
        tick.tick().await;
        let now = hub::now();
        let bytes = {
            let mut mcu = lock(&mcu);
            while let Ok(bytes) = end.to_mcu.try_recv() {
                mcu.receive(&bytes, now);
            }
            mcu.step(now)
        };
        if !bytes.is_empty() && end.from_mcu.send(bytes).await.is_err() {
            return;
        }
    }
}

/// Stands in for `db_writer`: keeps the event texts, drops the rest.
fn event_log(events: Arc<Mutex<VecDeque<(f64, String)>>>) -> mpsc::Sender<DbCommand> {
    let (db, commands) = mpsc::channel();
    let started = Instant::now();
    std::thread::spawn(move || {
        for command in commands {
            if let Some(text) = describe(&command) {
                let mut events = lock(&events);
                events.push_front((started.elapsed().as_secs_f64(), text));
                events.truncate(EVENT_LOG_LEN);
            }
        }
    });
    db
}

fn describe(command: &DbCommand) -> Option<String> {
    match command {
        DbCommand::Event(e) | DbCommand::Detector(Write::System(e)) => {
            let ty = e.system_type?.as_str();
            Some(match &e.detail {
                Some(detail) => format!("{ty} {detail}"),
                None => ty.to_string(),
            })
        }
        DbCommand::Detector(Write::Raise { event, .. }) => {
            Some(format!("E{:02} raised", event.code?))
        }
        DbCommand::Detector(Write::Clear { code, .. }) => Some(format!("E{code:02} cleared")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    //! End to end over the UART bytes: the demo MCU and the real core, on
    //! tokio's paused clock.

    use super::{Preview, lock, start};
    use bess_core::fan::Reason;
    use bess_core::link::LinkState;
    use bess_core::live::{LiveState, Values};
    use bess_core::state::ShownState;
    use bess_core::tasks::hub;
    use std::time::Duration;

    /// Runs `ms` milliseconds of virtual time.
    async fn run(p: &Preview, ms: u64) -> LiveState {
        tokio::time::sleep(Duration::from_millis(ms)).await;
        p.live.borrow().clone()
    }

    /// Whether an event starting with `prefix` is logged; the log thread
    /// runs in real time, so give it a moment.
    fn logged(p: &Preview, prefix: &str) -> bool {
        (0..200).any(|_| {
            let found = lock(&p.events).iter().any(|(_, e)| e.starts_with(prefix));
            if !found {
                std::thread::sleep(Duration::from_millis(5));
            }
            found
        })
    }

    #[tokio::test(start_paused = true)]
    async fn handshake_reaches_online_and_the_mcu_hears_its_identity() {
        let p = start();
        let v = run(&p, 2500).await;
        assert_eq!(v.link, LinkState::Online);
        assert!(matches!(v.values, Values::Live(..)));
        assert_eq!(v.shown.state, ShownState::Charging);
        let mcu = lock(&p.mcu);
        assert_eq!(mcu.pi_heartbeat(hub::now()), Some(mcu.identity()));
        drop(mcu);
        assert!(logged(&p, "CONFIG_ACCEPTED"));
    }

    #[tokio::test(start_paused = true)]
    async fn cut_link_greys_the_values_and_recovers() {
        let p = start();
        run(&p, 2500).await;
        lock(&p.mcu).controls.link_cut = true;
        let v = run(&p, 4000).await;
        assert_eq!(v.link, LinkState::Offline);
        assert!(v.shown.comm_lost);
        assert!(matches!(v.values, Values::Stale(_)));
        lock(&p.mcu).controls.link_cut = false;
        let v = run(&p, 1500).await;
        assert_eq!(v.link, LinkState::Online);
        assert!(matches!(v.values, Values::Live(..)));
        assert_eq!(p.link.borrow().outages, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn mcu_restart_goes_through_wait_config_to_online() {
        let p = start();
        run(&p, 2500).await;
        lock(&p.mcu).restart(hub::now());
        let v = run(&p, 2500).await;
        assert_eq!(v.link, LinkState::Online);
        let mcu = lock(&p.mcu);
        assert_eq!(mcu.identity().boot_counter, 2);
        assert_eq!(mcu.pi_heartbeat(hub::now()), Some(mcu.identity()));
        drop(mcu);
        assert!(logged(&p, "MCU_RESTART"));
    }

    #[tokio::test(start_paused = true)]
    async fn e_stop_switches_fans_off_and_shows_the_banner() {
        let p = start();
        run(&p, 2500).await;
        lock(&p.mcu).controls.e_stop = true;
        let v = run(&p, 300).await; // FAULT_UPDATE, not the next 1 s frame
        assert_eq!(v.shown.state, ShownState::EStop);
        assert!(v.critical_banner);
        assert_eq!(
            v.fan.map(|f| (f.duty_pct, f.reason)),
            Some((0, Reason::EStop))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn hot_battery_runs_the_fans_at_100_with_e05() {
        let p = start();
        run(&p, 2500).await;
        lock(&p.mcu).controls.cell_temp_c = 60.0;
        let v = run(&p, 1500).await;
        assert_eq!(
            v.fan.map(|f| (f.duty_pct, f.reason)),
            Some((100, Reason::E05))
        );
        assert_eq!(v.shown.state, ShownState::Fault);
        assert_eq!(v.faults[0].name, "OVERHEATING");
        assert!(logged(&p, "E05 raised"));
    }
}

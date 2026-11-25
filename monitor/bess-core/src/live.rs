//! The live state shown on the touchscreen and the dashboard (S8,
//! docs/design/architecture.md §2 "live state"; lifecycle.md §2, §6).
//!
//! [`LiveTracker`] keeps what the screen needs between frames: the latest
//! valid frame (for the state and the faults) and the last sample taken
//! while ONLINE. [`LiveTracker::view`] builds a [`LiveState`] from it, the
//! link supervisor and the fan controller:
//! - ONLINE: live values, coloured (S8.11);
//! - `WAIT_CONFIG`: no values (S2.4), but the state and faults of the
//!   frames (DD-69);
//! - OFFLINE: the last values greyed and "BMS communication lost" (S8.12),
//!   the last known faults, and a last-known E-STOP or FAULT kept (DD-29).
//!
//! Later this struct goes into `bess-api` and reaches the display over the
//! Unix socket (DD-14, DD-18).

use crate::colour::{SampleLevels, WarningLevels, levels};
use crate::db::capacity_ah;
use crate::fan::FanDemand;
use crate::faults::{self, FaultInfo};
use crate::link::{LinkState, LinkView};
use crate::sample::Sample;
use crate::state::{Shown, ShownState, shown_state};
use protocol::config::FirmwareVersion;
use protocol::messages::Telemetry;

/// The measured values and whether they may be shown.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Values {
    /// Nothing to show: no sample since the CONFIG was installed (S2.4).
    Hidden,
    /// Fresh values (ONLINE) with their colours.
    Live(Sample, SampleLevels),
    /// The last values before the link was lost, shown greyed (S8.12).
    Stale(Sample),
}

/// What the screen shows.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveState {
    pub link: LinkState,
    pub shown: Shown,
    pub values: Values,
    /// Active faults in display order; from the last frame while OFFLINE.
    pub faults: Vec<FaultInfo>,
    /// The faults are from before the link was lost.
    pub faults_stale: bool,
    /// Red banner: a Critical fault is active (S8.10); stays while OFFLINE.
    pub critical_banner: bool,
    pub fan: Option<FanDemand>,
    /// Firmware of the installed CONFIG.
    pub firmware: Option<FirmwareVersion>,
    /// Rated capacity from the installed CONFIG, Ah.
    pub capacity_ah: Option<f64>,
}

/// Keeps the latest frame and sample between frames.
#[derive(Debug, Clone, Default)]
pub struct LiveTracker {
    frame: Option<Telemetry>,
    sample: Option<Sample>,
}

impl LiveTracker {
    #[must_use]
    pub fn new() -> LiveTracker {
        LiveTracker::default()
    }

    /// Takes a valid TELEMETRY or `FAULT_UPDATE` frame, with the link as it
    /// is after `LinkSupervisor::on_message` handled the same frame, so the
    /// link state already reflects its identity.
    pub fn on_frame(&mut self, frame: &Telemetry, link: &LinkView) {
        if link.shows_faults() {
            self.frame = Some(*frame);
        }
        self.sample = match link.config() {
            Some(config) if link.shows_values() => {
                Some(Sample::from_telemetry(frame, config.ntc_present_mask))
            }
            // A new identity: the old values must not come back (S2.4).
            _ => None,
        };
    }

    /// The latest valid frame kept for the state and faults.
    #[must_use]
    pub fn frame(&self) -> Option<&Telemetry> {
        self.frame.as_ref()
    }

    /// The screen contents now.
    #[must_use]
    pub fn view(&self, link: &LinkView, fan: Option<FanDemand>, warn: &WarningLevels) -> LiveState {
        let state = link.state;
        let shown = shown_state(state, self.frame.map(|f| f.state));
        let config = link.config();
        let values = match (state, self.sample, config) {
            (LinkState::Online, Some(sample), Some(config)) => Values::Live(
                sample,
                levels(&sample, config, shown.state == ShownState::Charging, warn),
            ),
            (LinkState::Offline, Some(sample), _) => Values::Stale(sample),
            _ => Values::Hidden,
        };
        let bits = self.frame.map(|f| f.reported_faults).unwrap_or_default();
        LiveState {
            link: state,
            shown,
            values,
            faults: faults::active(bits),
            faults_stale: state == LinkState::Offline,
            critical_banner: faults::critical(bits),
            fan,
            firmware: config.map(|c| c.firmware),
            capacity_ah: config.map(capacity_ah),
        }
    }
}

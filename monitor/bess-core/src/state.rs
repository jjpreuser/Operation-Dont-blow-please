//! The system state the screen shows (docs/design/lifecycle.md §6; S8.9,
//! DD-29, DD-69).
//!
//! The MCU sends its state in every frame; the Pi adds OFFLINE from the link.
//! Priority E-STOP > FAULT > OFFLINE > CHARGING / DISCHARGING > IDLE: while
//! the link is down or waiting for CONFIG, a last-known E-STOP or FAULT stays
//! shown, because the Pi cannot know it has cleared.

use crate::link::LinkState;
use protocol::messages::McuState;
use std::fmt;

/// One of the six S8.9 states, or a state byte this side does not know
/// (shown and logged as unknown, DD-68).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShownState {
    Idle,
    Charging,
    Discharging,
    Fault,
    EStop,
    Offline,
    Unknown(u8),
}

impl fmt::Display for ShownState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShownState::Idle => f.write_str("IDLE"),
            ShownState::Charging => f.write_str("CHARGING"),
            ShownState::Discharging => f.write_str("DISCHARGING"),
            ShownState::Fault => f.write_str("FAULT"),
            ShownState::EStop => f.write_str("E-STOP"),
            ShownState::Offline => f.write_str("OFFLINE"),
            ShownState::Unknown(byte) => write!(f, "UNKNOWN ({byte})"),
        }
    }
}

/// The shown state and the notice that goes with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shown {
    pub state: ShownState,
    /// "Waiting for BMS configuration" (`WAIT_CONFIG`).
    pub waiting_for_config: bool,
    /// "BMS communication lost", live values greyed (S8.12).
    pub comm_lost: bool,
}

impl From<McuState> for ShownState {
    fn from(state: McuState) -> ShownState {
        match state {
            McuState::Idle => ShownState::Idle,
            McuState::Charging => ShownState::Charging,
            McuState::Discharging => ShownState::Discharging,
            McuState::Fault => ShownState::Fault,
            McuState::EStop => ShownState::EStop,
            McuState::Unknown(byte) => ShownState::Unknown(byte),
        }
    }
}

/// The D5 §6 table: `last` is the state of the latest valid frame, kept
/// while OFFLINE; `None` before any frame.
#[must_use]
pub fn shown_state(link: LinkState, last: Option<McuState>) -> Shown {
    let keep = |s: Option<McuState>| match s {
        Some(McuState::EStop) => ShownState::EStop,
        Some(McuState::Fault) => ShownState::Fault,
        _ => ShownState::Offline,
    };
    match link {
        LinkState::Online => Shown {
            state: last.map_or(ShownState::Offline, ShownState::from),
            waiting_for_config: false,
            comm_lost: false,
        },
        LinkState::WaitConfig => Shown {
            state: keep(last),
            waiting_for_config: true,
            comm_lost: false,
        },
        LinkState::Offline => Shown {
            state: keep(last),
            waiting_for_config: false,
            comm_lost: true,
        },
    }
}

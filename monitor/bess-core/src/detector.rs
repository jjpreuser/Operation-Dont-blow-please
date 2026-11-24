//! The `event_detector` task's logic (docs/design/fault-handling.md §2–3,
//! §6; S10.5, S10.6, S1.12; DD-07, DD-21, DD-22, DD-33, DD-34, DD-37,
//! DD-66, DD-68, DD-69, DD-83, DD-97).
//!
//! The MCU decides what a fault is; the Pi records it. Every TELEMETRY and
//! `FAULT_UPDATE` carries the complete fault bitmap, so the detector diffs
//! each frame with the one before, in arrival order (DD-34): a new bit is a
//! raise, a bit gone is a clear. Every raise is a new event (DD-83). A code
//! is either active or not, so the writer clears "the open event of this
//! code" and needs no event ids back from the database.
//!
//! - **Reconciliation** (DD-37): at core start the detector gets the codes
//!   of the open events in the database; after OFFLINE or an MCU restart it
//!   keeps its own. The first frame after any of these is compared with that
//!   set: a code in both stays open, one only in the set is cleared "while
//!   Pi/link was down", one only in the frame is raised "active at
//!   reconnect".
//! - **OFFLINE**: nothing is raised or cleared; the Pi cannot see the MCU.
//! - **`WAIT_CONFIG`** (DD-69): faults are raised and cleared as usual, but
//!   without measured values and without a snapshot.
//! - **Snapshots** (DD-07, DD-21): a FAULT (not a WARNING) raised while
//!   ONLINE opens a snapshot with the 60 s before it from the ring buffer at
//!   once; the 60 s after are appended about every 10 s from the 1 s samples.
//!   A fault while one is open extends it to 60 s after that fault. Only one
//!   snapshot is open at a time.
//! - **Lost frames** (DD-66): a jump in the MCU sequence number between two
//!   frames of one continuous link is counted and logged as `FRAMES_LOST`,
//!   at most once a minute.
//! - **MCU state changes** are logged as `STATE_CHANGE` (D6 §1).
//!
//! Events need a valid clock: the caller passes the UTC receive time, and
//! keeps events from before the clock is valid in its RAM queue (DD-31).

use crate::db::{EventKind, EventSeverity, EventSource, FrameValues, NewEvent, SystemType};
use crate::faults::{self, Severity};
use crate::link::LinkState;
use crate::ring::{RingBuffer, RingEntry};
use crate::signals::Signals;
use crate::wallclock::add_ms;
use jiff::Timestamp;
use protocol::config::FirmwareVersion;
use protocol::messages::{FaultBits, FaultCode, Identity, McuState, Telemetry};
use std::time::{Duration, Instant};

/// Snapshot data kept before a fault (S10.6).
pub const SNAPSHOT_BEFORE: Duration = Duration::from_secs(60);
/// Snapshot data kept after the last fault in it (S10.6, DD-07).
pub const SNAPSHOT_AFTER: Duration = Duration::from_secs(60);
/// How often the rows after a fault are appended (DD-07: about every 10 s).
pub const SNAPSHOT_APPEND_EVERY: Duration = Duration::from_secs(10);
/// `FRAMES_LOST` is logged at most this often (DD-66).
pub const FRAMES_LOST_EVERY: Duration = Duration::from_secs(60);

/// `detail` of a fault found active in the first frame after a gap (D6 §3).
pub const DETAIL_ACTIVE_AT_RECONNECT: &str = "active at reconnect, start time unknown";
/// `detail` added to a fault that cleared while the Pi or the link was down.
pub const DETAIL_CLEARED_WHILE_DOWN: &str = "cleared while Pi/link was down";

fn ms(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

/// One 1 s row of a snapshot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SnapshotRow {
    pub ts: Timestamp,
    pub state: McuState,
    pub signals: Signals,
}

impl SnapshotRow {
    /// A ring buffer entry as a row; `None` without a UTC time.
    fn from_entry(e: &RingEntry) -> Option<SnapshotRow> {
        e.utc.map(|ts| SnapshotRow {
            ts,
            state: e.state,
            signals: e.signals,
        })
    }
}

/// What a raised fault does to the snapshots.
#[derive(Debug, Clone, PartialEq)]
pub enum SnapshotPart {
    /// No snapshot: a warning, or raised in `WAIT_CONFIG`.
    None,
    /// A new snapshot with the rows from before the fault.
    Start {
        ts_start: Timestamp,
        ts_end: Timestamp,
        rows: Vec<SnapshotRow>,
    },
    /// The fault falls inside the open snapshot, which now ends later.
    Extend { ts_end: Timestamp },
}

/// One thing for `db_writer` to do, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum Write {
    /// A fault or warning raised, with its snapshot part; the event is
    /// linked to that snapshot.
    Raise {
        event: NewEvent,
        snapshot: SnapshotPart,
    },
    /// The open event of this code cleared; `detail` is added to it.
    Clear {
        code: u8,
        ts: Timestamp,
        detail: Option<String>,
    },
    /// A system event (`STATE_CHANGE`, `FRAMES_LOST`).
    System(NewEvent),
    /// More rows for the open snapshot.
    SnapshotRows(Vec<SnapshotRow>),
    /// The open snapshot is complete; run the size guard next (D3 §4).
    SnapshotComplete,
}

/// One TELEMETRY or `FAULT_UPDATE` frame for the detector.
#[derive(Debug, Clone, Copy)]
pub struct Frame<'a> {
    /// Receive time, monotonic.
    pub at: Instant,
    /// Receive time, UTC (valid clock).
    pub utc: Timestamp,
    /// Frame envelope sequence number.
    pub sequence: u8,
    pub telemetry: &'a Telemetry,
    /// The link state after the link supervisor handled this frame.
    pub link: LinkState,
    /// The 15 signals of this frame (fan values from `fan_controller`);
    /// `None` in `WAIT_CONFIG`.
    pub signals: Option<Signals>,
    /// From the installed CONFIG; `None` in `WAIT_CONFIG` (DD-69).
    pub firmware: Option<FirmwareVersion>,
}

#[derive(Debug, Clone)]
struct OpenSnapshot {
    end_at: Instant,
    /// The newest ring entry already in the snapshot or pending.
    last_row_at: Instant,
    last_append_at: Instant,
    pending: Vec<SnapshotRow>,
}

/// The fault set the Pi has seen, and the open snapshot.
#[derive(Debug, Clone)]
pub struct EventDetector {
    active: FaultBits,
    reconcile: bool,
    identity: Option<Identity>,
    state: Option<McuState>,
    last_sequence: Option<u8>,
    lost_pending: u32,
    lost_total: u64,
    last_lost_event: Option<Instant>,
    snapshot: Option<OpenSnapshot>,
}

impl EventDetector {
    /// At core start: `open` are the codes of the events the database still
    /// has open (`Store::open_fault_codes`). The first frame reconciles them.
    #[must_use]
    pub fn new(open: FaultBits) -> EventDetector {
        EventDetector {
            active: open,
            reconcile: true,
            identity: None,
            state: None,
            last_sequence: None,
            lost_pending: 0,
            lost_total: 0,
            last_lost_event: None,
            snapshot: None,
        }
    }

    /// The faults the Pi last saw active.
    #[must_use]
    pub fn active(&self) -> FaultBits {
        self.active
    }

    /// Frames lost since core start (S9.11 counter).
    #[must_use]
    pub fn frames_lost_total(&self) -> u64 {
        self.lost_total
    }

    /// The link went OFFLINE: the next frame reconciles, and the sequence
    /// gap across the outage is not counted as lost frames.
    pub fn on_offline(&mut self) {
        self.reconcile = true;
        self.last_sequence = None;
    }

    /// One TELEMETRY or `FAULT_UPDATE`, in arrival order. For a TELEMETRY,
    /// call [`EventDetector::on_sample`] with its ring entry first, so a new
    /// snapshot includes it. `ring` gives the 60 s before a fault.
    pub fn on_frame(&mut self, frame: &Frame<'_>, ring: &RingBuffer) -> Vec<Write> {
        let mut out = Vec::new();
        if frame.link == LinkState::Offline {
            return out; // nothing raised or cleared while OFFLINE (DD-37)
        }
        self.expire(frame.at, &mut out);
        let t = frame.telemetry;
        let identity = t.identity();
        if self.identity.is_some_and(|i| i != identity) {
            // MCU restart or new CONFIG revision: reconcile, and its sequence
            // numbers start again.
            self.reconcile = true;
            self.last_sequence = None;
        }
        self.identity = Some(identity);
        self.count_lost(frame, &mut out);

        if let Some(previous) = self.state.filter(|s| *s != t.state) {
            out.push(Write::System(self.system_event(
                frame,
                SystemType::StateChange,
                format!(
                    "MCU state {} -> {}",
                    crate::db::state_text(previous),
                    crate::db::state_text(t.state)
                ),
            )));
        }
        self.state = Some(t.state);

        let now = t.reported_faults;
        let (raised_detail, cleared_detail) = if self.reconcile {
            (
                Some(DETAIL_ACTIVE_AT_RECONNECT),
                Some(DETAIL_CLEARED_WHILE_DOWN),
            )
        } else {
            (None, None)
        };
        for code in self.active.codes().filter(|c| !now.contains(*c)) {
            out.push(Write::Clear {
                code: code.number(),
                ts: frame.utc,
                detail: cleared_detail.map(str::to_owned),
            });
        }
        let before = self.active;
        for code in now.codes().filter(|c| !before.contains(*c)) {
            let event = fault_event(frame, code, raised_detail);
            let snapshot = if event.kind == EventKind::Fault && frame.link == LinkState::Online {
                self.snapshot_for(frame, ring)
            } else {
                SnapshotPart::None
            };
            out.push(Write::Raise { event, snapshot });
        }
        self.active = now;
        self.reconcile = false;
        out
    }

    /// One 1 s TELEMETRY sample after it went into the ring buffer: rows
    /// after a fault for the open snapshot, appended about every 10 s.
    pub fn on_sample(&mut self, entry: &RingEntry) -> Vec<Write> {
        let mut out = Vec::new();
        if let Some(s) = self.snapshot.as_mut() {
            if entry.at > s.last_row_at && entry.at <= s.end_at {
                s.last_row_at = entry.at;
                s.pending.extend(SnapshotRow::from_entry(entry));
            }
        }
        self.expire(entry.at, &mut out);
        if let Some(s) = self.snapshot.as_mut() {
            if entry.at.saturating_duration_since(s.last_append_at) >= SNAPSHOT_APPEND_EVERY
                && !s.pending.is_empty()
            {
                s.last_append_at = entry.at;
                out.push(Write::SnapshotRows(std::mem::take(&mut s.pending)));
            }
        }
        out
    }

    /// The timer: completes the open snapshot once its window has passed,
    /// also when no samples arrive (link lost).
    pub fn poll(&mut self, now: Instant) -> Vec<Write> {
        let mut out = Vec::new();
        self.expire(now, &mut out);
        out
    }

    /// When [`EventDetector::poll`] should run next.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        self.snapshot
            .as_ref()
            .map(|s| s.end_at + Duration::from_millis(1))
    }

    /// Clean shutdown: the open snapshot with what it has, complete.
    pub fn flush(&mut self) -> Vec<Write> {
        let mut out = Vec::new();
        if let Some(s) = self.snapshot.take() {
            if !s.pending.is_empty() {
                out.push(Write::SnapshotRows(s.pending));
            }
            out.push(Write::SnapshotComplete);
        }
        out
    }

    fn expire(&mut self, now: Instant, out: &mut Vec<Write>) {
        if self.snapshot.as_ref().is_some_and(|s| now > s.end_at) {
            out.extend(self.flush());
        }
    }

    fn snapshot_for(&mut self, frame: &Frame<'_>, ring: &RingBuffer) -> SnapshotPart {
        let end_at = frame.at + SNAPSHOT_AFTER;
        let ts_end = add_ms(frame.utc, ms(SNAPSHOT_AFTER));
        if let Some(s) = self.snapshot.as_mut() {
            s.end_at = end_at;
            return SnapshotPart::Extend { ts_end };
        }
        let rows: Vec<SnapshotRow> = match frame.at.checked_sub(SNAPSHOT_BEFORE) {
            Some(from) => ring.since(from).collect::<Vec<_>>(),
            None => ring.iter().collect(),
        }
        .into_iter()
        .filter(|e| e.at <= frame.at)
        .filter_map(SnapshotRow::from_entry)
        .collect();
        self.snapshot = Some(OpenSnapshot {
            end_at,
            last_row_at: frame.at,
            last_append_at: frame.at,
            pending: Vec::new(),
        });
        SnapshotPart::Start {
            ts_start: add_ms(frame.utc, -ms(SNAPSHOT_BEFORE)),
            ts_end,
            rows,
        }
    }

    /// The sequence number of any other valid MCU frame (CONFIG,
    /// `RESET_ACK`): the MCU numbers every frame it sends, so these count
    /// for lost-frame detection too. A loss found here is logged with the
    /// next TELEMETRY or `FAULT_UPDATE`.
    pub fn on_other_frame(&mut self, sequence: u8) {
        self.track_sequence(sequence);
    }

    fn track_sequence(&mut self, sequence: u8) {
        if let Some(last) = self.last_sequence {
            // A repeated number is a duplicate frame, not 255 lost ones.
            if sequence != last {
                let lost = sequence.wrapping_sub(last).wrapping_sub(1);
                self.lost_pending += u32::from(lost);
                self.lost_total += u64::from(lost);
            }
        }
        self.last_sequence = Some(sequence);
    }

    fn count_lost(&mut self, frame: &Frame<'_>, out: &mut Vec<Write>) {
        self.track_sequence(frame.sequence);
        let due = self
            .last_lost_event
            .is_none_or(|t| frame.at.saturating_duration_since(t) >= FRAMES_LOST_EVERY);
        if self.lost_pending > 0 && due {
            let n = std::mem::take(&mut self.lost_pending);
            self.last_lost_event = Some(frame.at);
            out.push(Write::System(self.system_event(
                frame,
                SystemType::FramesLost,
                format!("{n} frames lost"),
            )));
        }
    }

    fn system_event(&self, frame: &Frame<'_>, ty: SystemType, detail: String) -> NewEvent {
        NewEvent {
            firmware: frame.firmware,
            identity: self.identity,
            ..NewEvent::system(frame.utc, ty, Some(detail))
        }
    }
}

/// The event for a raised code (DD-22, DD-33, DD-36, DD-68, DD-97).
fn fault_event(frame: &Frame<'_>, code: FaultCode, detail: Option<&str>) -> NewEvent {
    let info = faults::info(code);
    let kind = match info.severity {
        Some(Severity::Warning) => EventKind::Warning,
        // Unknown codes E20–E32 count as faults (DD-68).
        _ => EventKind::Fault,
    };
    let t = frame.telemetry;
    NewEvent {
        ts: frame.utc,
        kind,
        code: Some(code.number()),
        // Table A gives an unknown code no severity; it is stored as
        // PROTECTION (DD-97).
        severity: info
            .severity
            .map_or(EventSeverity::Protection, EventSeverity::from),
        source: EventSource::Mcu,
        system_type: None,
        frame: Some(FrameValues {
            signals: frame.signals,
            state: t.state,
            validity: t.validity,
        }),
        detail: detail.map(str::to_owned),
        firmware: frame.firmware,
        identity: Some(t.identity()),
        snapshot_id: None,
    }
}

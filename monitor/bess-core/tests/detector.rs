//! L1 tests of the event detector (docs/design/fault-handling.md §2–3, §6):
//! every error code raised and cleared (S1.12), snapshots (S10.6),
//! reconciliation (DD-37), OFFLINE and `WAIT_CONFIG` (DD-69), lost frames
//! (DD-66), state changes; and the whole chain into a SQLite file (S10.5).

mod common;

use bess_core::db::{EventKind, EventSeverity, Store, SystemType};
use bess_core::detector::{
    EventDetector, Frame, SnapshotPart, Write, DETAIL_ACTIVE_AT_RECONNECT,
    DETAIL_CLEARED_WHILE_DOWN,
};
use bess_core::faults;
use bess_core::link::LinkState;
use bess_core::ring::{RingBuffer, RingEntry};
use bess_core::signals::Signals;
use common::{telemetry_frame, BOOT1, BOOT2};
use jiff::Timestamp;
use protocol::config::FirmwareVersion;
use protocol::messages::{FaultBits, FaultCode, Identity, McuState};
use std::time::{Duration, Instant};

const FW: FirmwareVersion = FirmwareVersion {
    major: 1,
    minor: 0,
    patch: 0,
};

fn bits(codes: &[u8]) -> FaultBits {
    FaultBits(
        codes
            .iter()
            .map(|&c| FaultCode::new(c).unwrap().mask())
            .fold(0, |a, m| a | m),
    )
}

/// A detector with a ring buffer, fed frames at whole seconds.
struct Rig {
    t0: Instant,
    utc0: Timestamp,
    ring: RingBuffer,
    det: EventDetector,
    sequence: u8,
    identity: Identity,
    state: McuState,
    link: LinkState,
}

impl Rig {
    fn new(open: FaultBits) -> Rig {
        Rig {
            t0: Instant::now(),
            utc0: "2026-11-24T10:00:00Z".parse().unwrap(),
            ring: RingBuffer::new(),
            det: EventDetector::new(open),
            sequence: 0,
            identity: BOOT1,
            state: McuState::Idle,
            link: LinkState::Online,
        }
    }

    fn at(&self, ms: u64) -> Instant {
        self.t0 + Duration::from_millis(ms)
    }

    fn utc(&self, ms: u64) -> Timestamp {
        self.utc0 + Duration::from_millis(ms)
    }

    fn signals(secs: f64) -> Signals {
        let mut s = Signals::default();
        s.sample.pack_v = Some(13.0 + secs / 1000.0);
        s.fan_duty_pct = Some(30.0);
        s
    }

    fn frame(&mut self, ms: u64, faults: FaultBits) -> Vec<Write> {
        self.sequence = self.sequence.wrapping_add(1);
        let mut t = telemetry_frame(self.identity);
        t.reported_faults = faults;
        t.state = self.state;
        let online = self.link == LinkState::Online;
        #[expect(clippy::cast_precision_loss, reason = "test times are small")]
        let signals = online.then(|| Rig::signals(ms as f64));
        let frame = Frame {
            at: self.at(ms),
            utc: self.utc(ms),
            sequence: self.sequence,
            telemetry: &t,
            link: self.link,
            signals,
            firmware: online.then_some(FW),
        };
        self.det.on_frame(&frame, &self.ring)
    }

    /// A TELEMETRY at `secs`: its sample first, then the frame.
    fn telemetry(&mut self, secs: u64, faults: FaultBits) -> Vec<Write> {
        let mut out = Vec::new();
        if self.link == LinkState::Online {
            #[expect(clippy::cast_precision_loss, reason = "test times are small")]
            let entry = RingEntry {
                at: self.at(secs * 1000),
                utc: Some(self.utc(secs * 1000)),
                state: self.state,
                signals: Rig::signals((secs * 1000) as f64),
            };
            self.ring.push(entry);
            out.extend(self.det.on_sample(&entry));
        }
        out.extend(self.frame(secs * 1000, faults));
        out
    }

    /// TELEMETRY every second from `from` to `to` (inclusive) with `faults`.
    fn run(&mut self, from: u64, to: u64, faults: FaultBits) -> Vec<Write> {
        (from..=to)
            .flat_map(|s| self.telemetry(s, faults))
            .collect()
    }
}

fn raises(writes: &[Write]) -> Vec<(u8, &SnapshotPart, Option<&str>)> {
    writes
        .iter()
        .filter_map(|w| match w {
            Write::Raise { event, snapshot } => {
                Some((event.code.unwrap(), snapshot, event.detail.as_deref()))
            }
            _ => None,
        })
        .collect()
}

fn clears(writes: &[Write]) -> Vec<(u8, Option<&str>)> {
    writes
        .iter()
        .filter_map(|w| match w {
            Write::Clear { code, detail, .. } => Some((*code, detail.as_deref())),
            _ => None,
        })
        .collect()
}

fn systems(writes: &[Write]) -> Vec<(SystemType, String)> {
    writes
        .iter()
        .filter_map(|w| match w {
            Write::System(e) => {
                Some((e.system_type.unwrap(), e.detail.clone().unwrap_or_default()))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn every_error_code_has_a_trigger_and_a_recovery() {
    // S1.12: E01–E19 from Table A and two unknown codes (DD-68).
    for code in (1..=19).chain([20, 32]) {
        let mut rig = Rig::new(FaultBits(0));
        assert!(
            rig.run(0, 70, FaultBits(0)).is_empty(),
            "E{code:02}: quiet start"
        );

        // Trigger.
        let writes = rig.telemetry(71, bits(&[code]));
        let [Write::Raise { event, snapshot }] = writes.as_slice() else {
            panic!("E{code:02}: one raise expected, got {writes:?}");
        };
        let info = faults::info(FaultCode::new(code).unwrap());
        let warning = info.severity == Some(faults::Severity::Warning);
        assert_eq!(event.code, Some(code));
        assert_eq!(event.ts, rig.utc(71_000));
        assert_eq!(
            event.kind,
            if warning {
                EventKind::Warning
            } else {
                EventKind::Fault
            },
            "E{code:02} kind (DD-33)"
        );
        let expected_severity = info
            .severity
            .map_or(EventSeverity::Protection, EventSeverity::from);
        assert_eq!(event.severity, expected_severity, "E{code:02} severity");
        // Values at report (DD-22) and identity (DD-36).
        let values = event.frame.unwrap();
        assert!(values.signals.is_some());
        assert_eq!(event.identity, Some(BOOT1));
        assert_eq!(event.firmware, Some(FW));
        // Snapshot only for FAULT (DD-21), with the 60 s before.
        match snapshot {
            SnapshotPart::Start {
                rows,
                ts_start,
                ts_end,
            } if !warning => {
                assert_eq!(rows.len(), 61, "E{code:02}: 60 s before + the fault second");
                assert_eq!(*ts_start, rig.utc(11_000));
                assert_eq!(*ts_end, rig.utc(131_000));
            }
            SnapshotPart::None if warning => {}
            other => panic!("E{code:02}: unexpected snapshot {other:?}"),
        }

        // Still active: nothing new.
        assert!(raises(&rig.run(72, 80, bits(&[code]))).is_empty());

        // Recovery.
        let writes = rig.telemetry(81, FaultBits(0));
        assert_eq!(clears(&writes), vec![(code, None)], "E{code:02} clear");
        assert!(raises(&writes).is_empty());
    }
}

#[test]
fn a_short_fault_between_two_telemetry_frames_is_logged() {
    let mut rig = Rig::new(FaultBits(0));
    rig.run(0, 10, FaultBits(0));
    // FAULT_UPDATE raise at 10.2 s, FAULT_UPDATE clear at 10.6 s.
    let up = rig.frame(10_200, bits(&[3]));
    assert_eq!(raises(&up).len(), 1);
    let down = rig.frame(10_600, FaultBits(0));
    assert_eq!(clears(&down), vec![(3, None)]);
    // The next TELEMETRY changes nothing.
    let next = rig.telemetry(11, FaultBits(0));
    assert!(raises(&next).is_empty() && clears(&next).is_empty());
}

#[test]
fn a_duplicate_frame_raises_nothing_twice() {
    let mut rig = Rig::new(FaultBits(0));
    rig.run(0, 5, FaultBits(0));
    assert_eq!(raises(&rig.frame(5_100, bits(&[5]))).len(), 1);
    rig.sequence = rig.sequence.wrapping_sub(1); // the same frame again
    let again = rig.frame(5_100, bits(&[5]));
    assert!(raises(&again).is_empty());
    assert!(
        systems(&again).is_empty(),
        "a duplicate is not a lost frame"
    );
}

#[test]
fn snapshot_after_part_is_appended_every_10s_and_completed() {
    let mut rig = Rig::new(FaultBits(0));
    rig.run(0, 119, FaultBits(0));
    let writes = rig.telemetry(120, bits(&[5]));
    let [(5, SnapshotPart::Start { rows, .. }, None)] = raises(&writes).as_slice() else {
        panic!("{writes:?}");
    };
    assert_eq!(rows.len(), 61); // 60 ..= 120

    let mut appended = Vec::new();
    let mut appends = 0;
    let mut completed_at = None;
    for s in 121..=200 {
        for w in rig.telemetry(s, bits(&[5])) {
            match w {
                Write::SnapshotRows(rows) => {
                    appends += 1;
                    appended.extend(rows);
                }
                Write::SnapshotComplete => completed_at = Some(s),
                _ => {}
            }
        }
    }
    // 60 s after the fault: 121 ..= 180, complete at the first sample after.
    assert_eq!(appended.len(), 60);
    assert_eq!(appended.first().unwrap().ts, rig.utc(121_000));
    assert_eq!(appended.last().unwrap().ts, rig.utc(180_000));
    assert_eq!(completed_at, Some(181));
    assert!(
        (6..=7).contains(&appends),
        "about every 10 s, got {appends}"
    );
}

#[test]
fn an_overlapping_fault_extends_the_snapshot() {
    let mut rig = Rig::new(FaultBits(0));
    rig.run(0, 119, FaultBits(0));
    rig.telemetry(120, bits(&[5]));
    let writes = rig.run(121, 150, bits(&[5]));
    assert!(!writes.contains(&Write::SnapshotComplete));
    // E01 at 150 s, inside the window of E05.
    let writes = rig.telemetry(151, bits(&[1, 5]));
    let [(1, SnapshotPart::Extend { ts_end }, None)] = raises(&writes).as_slice() else {
        panic!("{writes:?}");
    };
    assert_eq!(*ts_end, rig.utc(211_000));
    // Complete only 60 s after the second fault.
    let rest = rig.run(152, 212, bits(&[1, 5]));
    let rows: usize = rest
        .iter()
        .map(|w| match w {
            Write::SnapshotRows(r) => r.len(),
            _ => 0,
        })
        .sum();
    assert!(rest.contains(&Write::SnapshotComplete));
    // Rows 151 ..= 211 here: 151 was still pending when E01 came.
    assert_eq!(rows, 61);
}

#[test]
fn a_fault_after_the_window_starts_a_new_snapshot() {
    let mut rig = Rig::new(FaultBits(0));
    rig.run(0, 99, FaultBits(0));
    rig.telemetry(100, bits(&[5]));
    let writes = rig.run(101, 200, FaultBits(0));
    assert!(writes.contains(&Write::SnapshotComplete));
    let writes = rig.telemetry(201, bits(&[2]));
    assert!(matches!(
        raises(&writes).as_slice(),
        [(2, SnapshotPart::Start { .. }, None)]
    ));
}

#[test]
fn a_fault_just_after_start_up_has_a_short_before_part() {
    let mut rig = Rig::new(FaultBits(0));
    rig.run(0, 9, FaultBits(0));
    let writes = rig.telemetry(10, bits(&[7]));
    let [(7, SnapshotPart::Start { rows, .. }, None)] = raises(&writes).as_slice() else {
        panic!("{writes:?}");
    };
    assert_eq!(rows.len(), 11); // 0 ..= 10, all the ring buffer has
}

#[test]
fn the_timer_completes_a_snapshot_when_samples_stop() {
    let mut rig = Rig::new(FaultBits(0));
    rig.run(0, 70, FaultBits(0));
    rig.telemetry(71, bits(&[5]));
    rig.run(72, 75, bits(&[5]));
    // Link lost: no more samples. The deadline is just after 131 s.
    let deadline = rig.det.next_deadline().unwrap();
    assert!(deadline > rig.at(131_000) && deadline <= rig.at(131_001));
    assert!(rig.det.poll(rig.at(131_000)).is_empty());
    let writes = rig.det.poll(deadline);
    assert_eq!(writes.last(), Some(&Write::SnapshotComplete));
    // The pending rows 72 ..= 75 were written first.
    assert!(matches!(&writes[0], Write::SnapshotRows(r) if r.len() == 4));
    assert_eq!(rig.det.next_deadline(), None);
}

#[test]
fn shutdown_flushes_and_completes_the_open_snapshot() {
    let mut rig = Rig::new(FaultBits(0));
    rig.run(0, 70, FaultBits(0));
    rig.telemetry(71, bits(&[5]));
    rig.run(72, 74, bits(&[5]));
    let writes = rig.det.flush();
    assert!(matches!(&writes[0], Write::SnapshotRows(r) if r.len() == 3));
    assert_eq!(writes[1], Write::SnapshotComplete);
    assert!(rig.det.flush().is_empty());
}

#[test]
fn core_start_reconciles_with_the_open_events() {
    // The DB still has E05 and E13 open; the first frame has E05 and E01.
    let mut rig = Rig::new(bits(&[5, 13]));
    let writes = rig.telemetry(0, bits(&[1, 5]));
    assert_eq!(clears(&writes), vec![(13, Some(DETAIL_CLEARED_WHILE_DOWN))]);
    let raised = raises(&writes);
    assert_eq!(raised.len(), 1);
    assert_eq!(raised[0].0, 1);
    assert_eq!(raised[0].2, Some(DETAIL_ACTIVE_AT_RECONNECT));
    // From then on, normal diffs without detail.
    let writes = rig.telemetry(1, bits(&[1]));
    assert_eq!(clears(&writes), vec![(5, None)]);
}

#[test]
fn nothing_is_raised_or_cleared_while_offline() {
    let mut rig = Rig::new(FaultBits(0));
    rig.run(0, 5, bits(&[5]));
    rig.det.on_offline();
    rig.link = LinkState::Offline;
    assert!(rig.frame(6_000, FaultBits(0)).is_empty());
    // Back: the first frame reconciles.
    rig.link = LinkState::Online;
    let writes = rig.telemetry(30, bits(&[2]));
    assert_eq!(clears(&writes), vec![(5, Some(DETAIL_CLEARED_WHILE_DOWN))]);
    assert_eq!(raises(&writes)[0].2, Some(DETAIL_ACTIVE_AT_RECONNECT));
    // The sequence jump across the outage is not a loss.
    assert!(systems(&writes).is_empty());
}

#[test]
fn an_mcu_restart_reconciles() {
    let mut rig = Rig::new(FaultBits(0));
    rig.run(0, 5, bits(&[14]));
    rig.identity = BOOT2;
    rig.sequence = 200; // a new boot numbers from somewhere else
    let writes = rig.telemetry(6, bits(&[14]));
    // E14 still active after the restart: kept, no new event.
    assert!(raises(&writes).is_empty() && clears(&writes).is_empty());
    let writes = rig.telemetry(7, FaultBits(0));
    assert_eq!(clears(&writes), vec![(14, None)]);
    assert!(systems(&writes).is_empty());
}

#[test]
fn wait_config_faults_have_no_values_and_no_snapshot() {
    let mut rig = Rig::new(FaultBits(0));
    rig.link = LinkState::WaitConfig;
    rig.frame(0, FaultBits(0));
    let writes = rig.frame(1_000, bits(&[15]));
    let [Write::Raise { event, snapshot }] = writes.as_slice() else {
        panic!("{writes:?}");
    };
    assert_eq!(event.code, Some(15));
    assert_eq!(event.kind, EventKind::Fault);
    assert_eq!(*snapshot, SnapshotPart::None, "DD-69");
    let values = event.frame.unwrap();
    assert!(
        values.signals.is_none(),
        "measurements hidden in WAIT_CONFIG"
    );
    assert_eq!(event.firmware, None);
    // Cleared there too.
    assert_eq!(clears(&rig.frame(2_000, FaultBits(0))), vec![(15, None)]);
}

#[test]
fn lost_frames_are_logged_at_most_once_a_minute() {
    let mut rig = Rig::new(FaultBits(0));
    rig.telemetry(0, FaultBits(0));
    rig.sequence = rig.sequence.wrapping_add(2); // 2 frames lost
    let writes = rig.telemetry(1, FaultBits(0));
    assert_eq!(
        systems(&writes),
        vec![(SystemType::FramesLost, "2 frames lost".into())]
    );
    // Another 3 lost 10 s later: counted, not logged yet.
    rig.run(2, 10, FaultBits(0));
    rig.sequence = rig.sequence.wrapping_add(3);
    assert!(systems(&rig.telemetry(11, FaultBits(0))).is_empty());
    // A minute after the first event, with the next frame.
    let writes = rig.run(12, 61, FaultBits(0));
    assert_eq!(
        systems(&writes),
        vec![(SystemType::FramesLost, "3 frames lost".into())]
    );
    assert_eq!(rig.det.frames_lost_total(), 5);
}

#[test]
fn the_sequence_wraps_and_other_frames_count() {
    let mut rig = Rig::new(FaultBits(0));
    rig.sequence = 253;
    // 254, 255, 0, 1: no loss across the wrap.
    assert!(systems(&rig.run(0, 3, FaultBits(0))).is_empty());
    // A CONFIG frame between two TELEMETRY frames is not a loss.
    rig.sequence = rig.sequence.wrapping_add(1);
    rig.det.on_other_frame(rig.sequence);
    assert!(systems(&rig.telemetry(4, FaultBits(0))).is_empty());
    assert_eq!(rig.det.frames_lost_total(), 0);
}

#[test]
fn mcu_state_changes_are_logged() {
    let mut rig = Rig::new(FaultBits(0));
    // The first frame has no previous state.
    assert!(systems(&rig.telemetry(0, FaultBits(0))).is_empty());
    rig.state = McuState::Charging;
    let writes = rig.telemetry(1, FaultBits(0));
    assert_eq!(
        systems(&writes),
        vec![(SystemType::StateChange, "MCU state IDLE -> CHARGING".into())]
    );
    let Some(Write::System(e)) = writes.first() else {
        panic!()
    };
    assert_eq!((e.identity, e.firmware), (Some(BOOT1), Some(FW)));
    rig.state = McuState::Unknown(9);
    assert_eq!(
        systems(&rig.telemetry(2, FaultBits(0))),
        vec![(
            SystemType::StateChange,
            "MCU state CHARGING -> UNKNOWN (9)".into()
        )]
    );
    assert!(systems(&rig.telemetry(3, FaultBits(0))).is_empty());
}

/// The detector's writes applied to a real SQLite file, then a core restart.
#[test]
fn faults_and_snapshots_end_up_in_the_database_and_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bess.db");
    let mut store = Store::open(&path).unwrap();
    let mut rig = Rig::new(store.open_fault_codes().unwrap());

    let mut writes = rig.run(0, 119, FaultBits(0));
    writes.extend(rig.telemetry(120, bits(&[5])));
    writes.extend(rig.run(121, 140, bits(&[5])));
    writes.extend(rig.telemetry(141, bits(&[5, 13])));
    writes.extend(rig.run(142, 200, bits(&[13])));
    for w in &writes {
        store.apply(w).unwrap();
    }

    let conn = rusqlite::Connection::open(&path).unwrap();
    let q = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(
        q("SELECT count(*) FROM event WHERE kind = 'FAULT' AND code = 5"),
        1
    );
    assert_eq!(
        q("SELECT count(*) FROM event WHERE kind = 'WARNING' AND code = 13"),
        1
    );
    assert_eq!(
        q("SELECT count(*) FROM event WHERE code = 5 AND cleared_ts IS NOT NULL"),
        1
    );
    assert_eq!(
        q("SELECT count(*) FROM event WHERE code = 13 AND snapshot_id IS NULL"),
        1
    );
    // One complete snapshot: 60 ..= 180, 121 rows (S10.6).
    assert_eq!(q("SELECT count(*) FROM snapshot WHERE complete = 1"), 1);
    assert_eq!(q("SELECT count(*) FROM snapshot_sample"), 121);
    let (start, end): (String, String) = conn
        .query_row("SELECT min(ts), max(ts) FROM snapshot_sample", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(start, "2026-11-24T10:01:00.000Z");
    assert_eq!(end, "2026-11-24T10:03:00.000Z");
    assert_eq!(
        q("SELECT count(*) FROM event WHERE code = 5 AND snapshot_id IS NOT NULL"),
        1
    );

    // Core restart: E13 is still open in the DB; the MCU cleared it meanwhile.
    drop(store);
    let mut store = Store::open(&path).unwrap();
    assert_eq!(store.complete_stale_snapshots().unwrap(), 0);
    let open = store.open_fault_codes().unwrap();
    assert_eq!(open, bits(&[13]));
    let mut rig = Rig::new(open);
    for w in rig.telemetry(0, FaultBits(0)) {
        store.apply(&w).unwrap();
    }
    let detail: String = conn
        .query_row("SELECT detail FROM event WHERE code = 13", [], |r| r.get(0))
        .unwrap();
    assert_eq!(detail, DETAIL_CLEARED_WHILE_DOWN);
    assert_eq!(store.open_fault_codes().unwrap(), FaultBits(0));
    // Events are never deleted (S10.5): all still there.
    assert_eq!(q("SELECT count(*) FROM event WHERE kind != 'SYSTEM'"), 2);
}

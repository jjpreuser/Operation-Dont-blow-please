//! The 1 h ring buffer of 1 s samples, RAM only (S10.2; DD-04, DD-13,
//! DD-74).
//!
//! It feeds the live graphs (S9.7, the last hour) and the 60 s before a fault
//! in a snapshot (S10.6, DD-07). It is never written to disk and starts empty
//! after a restart. Entries are kept by monotonic time, so a clock step does
//! not reorder or drop them; the UTC time is carried along when the clock is
//! valid (without a valid clock the ring buffer is the only place a sample
//! goes, DD-74).

use crate::signals::Signals;
use jiff::Timestamp;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// One hour of 1 s samples.
pub const RING_CAPACITY: usize = 3600;

/// How far back the ring buffer reaches.
pub const RING_SPAN: Duration = Duration::from_secs(3600);

/// One sample in the ring buffer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RingEntry {
    /// Receive time, monotonic.
    pub at: Instant,
    /// Receive time, UTC; `None` while the clock is not valid (DD-11).
    pub utc: Option<Timestamp>,
    pub signals: Signals,
}

/// The last hour of samples, oldest first.
#[derive(Debug, Clone)]
pub struct RingBuffer {
    entries: VecDeque<RingEntry>,
}

impl Default for RingBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl RingBuffer {
    #[must_use]
    pub fn new() -> RingBuffer {
        RingBuffer {
            entries: VecDeque::with_capacity(RING_CAPACITY),
        }
    }

    /// Adds a sample and drops what is more than an hour older than it, or
    /// beyond [`RING_CAPACITY`] entries.
    pub fn push(&mut self, entry: RingEntry) {
        if self.entries.len() == RING_CAPACITY {
            self.entries.pop_front();
        }
        let at = entry.at;
        self.entries.push_back(entry);
        while self
            .entries
            .front()
            .is_some_and(|e| at.saturating_duration_since(e.at) > RING_SPAN)
        {
            self.entries.pop_front();
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// All entries, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = &RingEntry> {
        self.entries.iter()
    }

    /// The entries received at or after `from`, oldest first: the 60 s before
    /// a fault is `since(fault_at - 60 s)`.
    pub fn since(&self, from: Instant) -> impl Iterator<Item = &RingEntry> {
        let start = self.entries.partition_point(|e| e.at < from);
        self.entries.range(start..)
    }

    /// The newest entry.
    #[must_use]
    pub fn latest(&self) -> Option<&RingEntry> {
        self.entries.back()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(t0: Instant, secs: u64, value: f64) -> RingEntry {
        let mut signals = Signals::default();
        signals.sample.pack_v = Some(value);
        RingEntry {
            at: t0 + Duration::from_secs(secs),
            utc: None,
            signals,
        }
    }

    fn pack_v(e: &RingEntry) -> f64 {
        e.signals.sample.pack_v.unwrap()
    }

    #[test]
    fn starts_empty() {
        let ring = RingBuffer::new();
        assert!(ring.is_empty());
        assert_eq!(ring.len(), 0);
        assert!(ring.latest().is_none());
    }

    #[test]
    fn holds_the_last_hour_of_1s_samples() {
        let t0 = Instant::now();
        let mut ring = RingBuffer::new();
        for s in 0..5000_u32 {
            ring.push(entry(t0, u64::from(s), f64::from(s)));
        }
        assert_eq!(ring.len(), RING_CAPACITY);
        // Oldest first: samples 1400 to 4999.
        let values: Vec<f64> = ring.iter().map(pack_v).collect();
        assert!((values[0] - 1400.0).abs() < f64::EPSILON);
        assert!((pack_v(ring.latest().unwrap()) - 4999.0).abs() < f64::EPSILON);
        assert!(values.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn drops_samples_older_than_an_hour_after_a_gap() {
        let t0 = Instant::now();
        let mut ring = RingBuffer::new();
        for s in 0..100 {
            ring.push(entry(t0, s, 1.0));
        }
        // 2 h later (link down meanwhile): the old samples are gone.
        ring.push(entry(t0, 7300, 2.0));
        assert_eq!(ring.len(), 1);
        // A sample exactly one hour newer than the oldest keeps it.
        ring.push(entry(t0, 7300 + 3600, 3.0));
        assert_eq!(ring.len(), 2);
    }

    #[test]
    fn since_gives_the_window_before_a_fault() {
        let t0 = Instant::now();
        let mut ring = RingBuffer::new();
        for s in 0..300_u32 {
            ring.push(entry(t0, u64::from(s), f64::from(s)));
        }
        // Fault at 200 s: the window starts 60 s before it.
        let before: Vec<f64> = ring
            .since(t0 + Duration::from_secs(200 - 60))
            .map(pack_v)
            .take_while(|v| *v <= 200.0)
            .collect();
        assert_eq!(before.len(), 61); // 140 ..= 200
        assert!((before[0] - 140.0).abs() < f64::EPSILON);
        // Before the oldest entry: everything.
        assert_eq!(ring.since(t0).count(), 300);
        // After the newest: nothing.
        assert_eq!(ring.since(t0 + Duration::from_secs(301)).count(), 0);
    }

    #[test]
    fn keeps_the_utc_time_when_the_clock_is_valid() {
        let t0 = Instant::now();
        let mut ring = RingBuffer::new();
        let utc: Timestamp = "2026-11-24T10:00:00Z".parse().unwrap();
        ring.push(RingEntry {
            utc: Some(utc),
            ..entry(t0, 0, 1.0)
        });
        ring.push(entry(t0, 1, 1.0));
        let times: Vec<Option<Timestamp>> = ring.iter().map(|e| e.utc).collect();
        assert_eq!(times, vec![Some(utc), None]);
    }
}

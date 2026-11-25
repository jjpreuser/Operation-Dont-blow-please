//! Stream parser for one receive direction (UART draft §3, DD-40).
//!
//! Accepts partial reads and several frames per read. A candidate frame is
//! checked in this order: header, version, known id for this direction, exact
//! length for that id, CRC. On any failure the parser drops one byte and searches
//! for the next `AA 55`, so it resynchronises after bad input. Inside a valid
//! frame, `AA 55` in the payload is ordinary data.
//!
//! The buffer has a fixed size. Input is taken in pieces that fit, and after each
//! piece every complete frame is removed, so at most one partial frame stays
//! behind: the buffer cannot overflow and no input is ever dropped unread.

use crate::crc::crc16;
use crate::frame::{
    Direction, Frame, HEADER, MAX_FRAME_LEN, MessageId, OVERHEAD, PREFIX_LEN, PROTOCOL_VERSION,
};
use std::fmt;

const BUF_CAP: usize = 2 * MAX_FRAME_LEN;

/// Communication statistics (S5.9). Counters only grow.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ParserStats {
    /// Frames that passed every check.
    pub frames: u64,
    /// Header found but the protocol version is not 0x03.
    pub version_errors: u64,
    /// Id unknown, or a known id that this side never receives.
    pub unknown_id: u64,
    /// Payload length does not match the id.
    pub length_errors: u64,
    pub crc_errors: u64,
    /// Bytes skipped while searching for a header (garbage and bad frames).
    pub discarded_bytes: u64,
}

impl ParserStats {
    /// Every rejected candidate frame, of any kind.
    #[must_use]
    pub fn errors(&self) -> u64 {
        self.version_errors + self.unknown_id + self.length_errors + self.crc_errors
    }
}

pub struct FrameParser {
    accept: Direction,
    buf: [u8; BUF_CAP],
    len: usize,
    stats: ParserStats,
}

impl FrameParser {
    /// A parser for frames travelling in `accept` direction: the Pi uses
    /// `Direction::McuToPi`, an MCU simulator `Direction::PiToMcu`.
    #[must_use]
    pub fn new(accept: Direction) -> FrameParser {
        FrameParser {
            accept,
            buf: [0; BUF_CAP],
            len: 0,
            stats: ParserStats::default(),
        }
    }

    #[must_use]
    pub fn stats(&self) -> ParserStats {
        self.stats
    }

    /// Bytes waiting for the rest of a frame.
    #[must_use]
    pub fn buffered(&self) -> usize {
        self.len
    }

    /// Feeds received bytes and returns the complete valid frames, in order.
    pub fn push(&mut self, mut data: &[u8]) -> Vec<Frame> {
        let mut frames = Vec::new();
        while !data.is_empty() {
            let take = data.len().min(BUF_CAP - self.len);
            self.buf[self.len..self.len + take].copy_from_slice(&data[..take]);
            self.len += take;
            data = &data[take..];
            self.extract(&mut frames);
        }
        frames
    }

    fn extract(&mut self, frames: &mut Vec<Frame>) {
        loop {
            match self.buf[..self.len].windows(2).position(|w| w == HEADER) {
                Some(0) => {}
                Some(start) => self.discard(start),
                None => {
                    // A trailing 0xAA may be the first half of the next header.
                    let keep = usize::from(self.len > 0 && self.buf[self.len - 1] == HEADER[0]);
                    self.discard(self.len - keep);
                    return;
                }
            }
            if self.len < PREFIX_LEN {
                return;
            }
            if self.buf[2] != PROTOCOL_VERSION {
                self.stats.version_errors += 1;
                self.discard(1);
                continue;
            }
            let Some(id) = MessageId::try_from(self.buf[3])
                .ok()
                .filter(|id| id.direction() == self.accept)
            else {
                self.stats.unknown_id += 1;
                self.discard(1);
                continue;
            };
            let n = usize::from(self.buf[4]);
            if n != id.payload_len() {
                self.stats.length_errors += 1;
                self.discard(1);
                continue;
            }
            let total = n + OVERHEAD;
            if self.len < total {
                return; // wait for the rest
            }
            let received =
                u16::from_le_bytes([self.buf[PREFIX_LEN + n], self.buf[PREFIX_LEN + n + 1]]);
            if crc16(&self.buf[2..PREFIX_LEN + n]) != received {
                self.stats.crc_errors += 1;
                self.discard(1);
                continue;
            }
            frames.push(Frame {
                id,
                sequence: self.buf[5],
                payload: self.buf[PREFIX_LEN..PREFIX_LEN + n].to_vec(),
            });
            self.stats.frames += 1;
            self.consume(total);
        }
    }

    /// Skips bytes that are not part of a valid frame.
    fn discard(&mut self, count: usize) {
        self.stats.discarded_bytes += count as u64;
        self.consume(count);
    }

    fn consume(&mut self, count: usize) {
        self.buf.copy_within(count..self.len, 0);
        self.len -= count;
    }
}

/// Shows only the buffered bytes, not the whole fixed-size buffer.
impl fmt::Debug for FrameParser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameParser")
            .field("accept", &self.accept)
            .field("buffered", &&self.buf[..self.len])
            .field("stats", &self.stats)
            .finish()
    }
}

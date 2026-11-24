//! The `uart_rx` and `uart_tx` tasks (docs/design/architecture.md §2; DD-26,
//! DD-70).
//!
//! - `uart_rx` takes the bytes the port reader thread read, splits them into
//!   frames and checks the CRC (the protocol crate's parser), decodes them,
//!   stamps the receive time and hands every valid frame to
//!   `link_supervisor` in arrival order.
//! - `uart_tx` is the only writer to the port: it numbers and encodes the
//!   messages `link_supervisor` (and later `api_server`) send it and passes
//!   the bytes to the port writer thread. It holds back HEARTBEAT while
//!   `fan_controller` has not run for [`FAN_ALIVE_WITHIN`]; the heartbeats
//!   themselves come from `link_supervisor`, so they also stop when that task
//!   hangs. Either way the MCU raises E11 and stops charging (DD-26).

use super::hub::{now, Clock, RxFrame};
use protocol::frame::{Direction, SequenceCounter};
use protocol::messages::Message;
use protocol::parser::{FrameParser, ParserStats};
use std::future::Future;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};

/// HEARTBEAT is held back once `fan_controller` has not run for this long
/// (its timer runs every 1 s, DD-51).
pub const FAN_ALIVE_WITHIN: Duration = Duration::from_secs(3);

/// The `uart_rx` logic: bytes in, frames out.
#[derive(Debug)]
pub struct UartRx {
    parser: FrameParser,
}

impl Default for UartRx {
    fn default() -> UartRx {
        UartRx::new()
    }
}

impl UartRx {
    #[must_use]
    pub fn new() -> UartRx {
        UartRx {
            parser: FrameParser::new(Direction::McuToPi),
        }
    }

    /// Bytes received at `at` (`utc` while the clock is valid).
    pub fn on_bytes(
        &mut self,
        at: Instant,
        utc: Option<jiff::Timestamp>,
        bytes: &[u8],
    ) -> Vec<RxFrame> {
        self.parser
            .push(bytes)
            .iter()
            .map(|frame| RxFrame {
                at,
                utc,
                sequence: frame.sequence,
                message: Message::try_from(frame).ok(),
            })
            .collect()
    }

    /// Frame parser counters (S5.9, S9.11).
    #[must_use]
    pub fn stats(&self) -> ParserStats {
        self.parser.stats()
    }
}

/// Runs `uart_rx` until `shutdown` resolves or the port reader stops.
/// Returning drops the queue to `link_supervisor`, which then ends, and so
/// on down the chain: that is how the core stops.
pub async fn run_rx<S>(
    mut rx: UartRx,
    mut from_port: mpsc::Receiver<Vec<u8>>,
    to_link: mpsc::Sender<RxFrame>,
    clock: Clock,
    shutdown: S,
) -> ParserStats
where
    S: Future<Output = ()>,
{
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            bytes = from_port.recv() => {
                let Some(bytes) = bytes else { break };
                for frame in rx.on_bytes(now(), clock(), &bytes) {
                    if to_link.send(frame).await.is_err() {
                        return rx.stats();
                    }
                }
            }
            () = &mut shutdown => break,
        }
    }
    rx.stats()
}

/// The `uart_tx` logic: numbers and encodes, and gates the heartbeat.
#[derive(Debug)]
pub struct UartTx {
    sequence: SequenceCounter,
    held: bool,
}

impl Default for UartTx {
    fn default() -> UartTx {
        UartTx::new()
    }
}

impl UartTx {
    #[must_use]
    pub fn new() -> UartTx {
        UartTx {
            sequence: SequenceCounter::new(0),
            held: false,
        }
    }

    /// The bytes for `message` at `now`, or `None` for a HEARTBEAT while
    /// `fan_controller` last ran before `now - FAN_ALIVE_WITHIN` (DD-26).
    pub fn encode(
        &mut self,
        now: Instant,
        fan_alive: Instant,
        message: &Message,
    ) -> Option<Vec<u8>> {
        if matches!(message, Message::Heartbeat(_)) {
            self.held = now.saturating_duration_since(fan_alive) > FAN_ALIVE_WITHIN;
            if self.held {
                return None;
            }
        }
        Some(message.encode(self.sequence.next_sequence()))
    }

    /// The last HEARTBEAT was held back.
    #[must_use]
    pub fn holding_heartbeat(&self) -> bool {
        self.held
    }
}

/// Runs `uart_tx` until `link_supervisor` (and later `api_server`) drop
/// their senders; returning drops the queue to the port writer thread.
pub async fn run_tx(
    mut tx: UartTx,
    mut from_tasks: mpsc::Receiver<Message>,
    to_port: mpsc::Sender<Vec<u8>>,
    fan_alive: watch::Receiver<Instant>,
) {
    while let Some(message) = from_tasks.recv().await {
        let was_held = tx.holding_heartbeat();
        let alive = *fan_alive.borrow();
        let bytes = tx.encode(now(), alive, &message);
        if tx.holding_heartbeat() != was_held {
            eprintln!(
                "uart_tx: fan_controller {}; HEARTBEAT {} (DD-26)",
                if was_held {
                    "runs again"
                } else {
                    "has stopped"
                },
                if was_held { "resumed" } else { "held back" }
            );
        }
        if let Some(bytes) = bytes {
            // A closed port shows up as a link timeout; nothing else to do.
            let _ = to_port.send(bytes).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::messages::{Heartbeat, CONFIG_LEN};

    fn decode(bytes: &[u8]) -> Vec<Message> {
        FrameParser::new(Direction::PiToMcu)
            .push(bytes)
            .iter()
            .filter_map(|f| Message::try_from(f).ok())
            .collect()
    }

    #[test]
    fn tx_numbers_the_frames() {
        let t0 = Instant::now();
        let mut tx = UartTx::new();
        let a = tx.encode(t0, t0, &Message::ConfigRequest).unwrap();
        let b = tx.encode(t0, t0, &Message::ConfigRequest).unwrap();
        let seq = |bytes: &[u8]| FrameParser::new(Direction::PiToMcu).push(bytes)[0].sequence;
        assert_eq!(seq(&b), seq(&a).wrapping_add(1));
        assert_eq!(decode(&a), vec![Message::ConfigRequest]);
    }

    #[test]
    fn tx_holds_the_heartbeat_while_the_fan_task_is_not_alive() {
        let t0 = Instant::now();
        let hb = Message::Heartbeat(Heartbeat::WAITING);
        let mut tx = UartTx::new();
        let at = |s: u64| t0 + Duration::from_secs(s);
        assert!(tx.encode(at(3), t0, &hb).is_some(), "3 s is still alive");
        assert!(tx.encode(at(4), t0, &hb).is_none(), "DD-26: no heartbeat");
        assert!(tx.holding_heartbeat());
        // Other messages still go out (the CONFIG handshake, FAULT_RESET).
        assert!(tx.encode(at(4), t0, &Message::ConfigRequest).is_some());
        // The fan task runs again: heartbeats resume.
        assert!(tx.encode(at(5), at(5), &hb).is_some());
        assert!(!tx.holding_heartbeat());
    }

    #[test]
    fn rx_stamps_and_decodes_every_frame_in_order() {
        let t0 = Instant::now();
        let utc: jiff::Timestamp = "2026-11-24T10:00:00Z".parse().unwrap();
        let mut rx = UartRx::new();
        let config = Message::Config(Box::new([0; CONFIG_LEN]));
        let mut bytes = config.encode(7);
        bytes.extend([0x00, 0x13]); // noise between frames
        bytes.extend(config.encode(8));
        // Split mid-frame, as the port delivers it.
        let (a, b) = bytes.split_at(10);
        assert!(rx.on_bytes(t0, Some(utc), a).is_empty());
        let frames = rx.on_bytes(t0, Some(utc), b);
        assert_eq!(
            frames.iter().map(|f| f.sequence).collect::<Vec<_>>(),
            vec![7, 8]
        );
        assert!(frames
            .iter()
            .all(|f| f.message == Some(config.clone()) && f.utc == Some(utc) && f.at == t0));
        // Pi → MCU frames are not accepted in this direction.
        assert!(rx
            .on_bytes(t0, None, &Message::ConfigRequest.encode(9))
            .is_empty());
    }
}

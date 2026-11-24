//! The tokio tasks of `bess-core`, one module per box of
//! docs/design/architecture.md §2 (D1; DD-70).
//!
//! | Task | Reads | Writes |
//! |---|---|---|
//! | [`uart`] `uart_rx` | bytes from the port reader thread | frames → `link_supervisor` (`mpsc`) |
//! | [`link`] `link_supervisor` | frames, fan duty | link `watch`, fault-frame and sample `broadcast`s, ring buffer, messages → `uart_tx`, events → `db_writer` |
//! | [`uart`] `uart_tx` | messages, fan liveness | bytes → port writer thread |
//! | [`fan`] `fan_controller` | frames, link | duty `watch`, liveness `watch` |
//! | [`live`] `live_state` | frames, link, duty | live state `watch` (for `api_server`) |
//! | [`aggregator`] `aggregator` | samples, link | 10-min rows, daily job → `db_writer` |
//! | [`detector`] `event_detector` | frames, ring buffer | events, snapshots → `db_writer`; "snapshot complete" → `maintenance` |
//! | [`maintenance`] `maintenance` | "snapshot complete" | size guard, `last_alive_ts` → `db_writer` |
//!
//! `db_writer` is a thread ([`crate::writer`]). `clock_manager`,
//! `api_server` and `cloud_uploader` are still to do; [`crate::runtime`]
//! starts the tasks and stops them in order.
//!
//! Each task is a struct with the logic and no I/O, so it is tested on a PC
//! (S1.5), and a `run` function: the loop that moves its channels.

pub mod aggregator;
pub mod detector;
pub mod fan;
pub mod hub;
pub mod link;
pub mod live;
pub mod maintenance;
pub mod uart;

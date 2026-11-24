//! The Pi monitor core service, `bess-core` (docs/design/architecture.md §2,
//! DD-02, DD-70, DD-71).
//!
//! For now this is the decision logic and the database layer, written so it
//! runs in `cargo test` on a PC (S1.5): each logic module takes inputs and the
//! current time and returns what to send, log or show, and [`db::Store`] is
//! what the `db_writer` thread will call. The tokio tasks that own the serial
//! port, the channels and the database thread come later and call into these
//! modules; the `bess-core` binary starts with them.

pub mod aggregate;
pub mod colour;
pub mod db;
pub mod fan;
pub mod faults;
pub mod link;
pub mod live;
pub mod ring;
pub mod sample;
pub mod signals;
pub mod state;
pub mod wallclock;

//! The Pi monitor core service, `bess-core` (docs/design/architecture.md §2,
//! DD-02, DD-70, DD-71).
//!
//! The logic modules have no I/O, so they run in `cargo test` on a PC
//! (S1.5): each takes inputs and the current time and returns what to send,
//! log or show. [`tasks`] runs each of them as the tokio task D1 draws, with
//! the D1 channels between them; [`runtime`] starts and stops the tasks
//! between the serial port threads and the [`writer`] (`db_writer`) thread
//! over [`db::Store`]; the `bess-core` binary (`main.rs`) starts it all
//! (DD-99).

pub mod aggregate;
pub mod colour;
pub mod db;
pub mod detector;
pub mod fan;
pub mod faults;
pub mod link;
pub mod live;
pub mod ring;
pub mod runtime;
pub mod sample;
pub mod signals;
pub mod state;
pub mod tasks;
pub mod wallclock;
pub mod writer;

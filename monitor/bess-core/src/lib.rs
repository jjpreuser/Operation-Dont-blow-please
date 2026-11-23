//! The Pi monitor core service, `bess-core` (docs/design/architecture.md §2,
//! DD-02, DD-70, DD-71).
//!
//! For now this is the decision logic only, written without I/O so it runs
//! in `cargo test` on a PC (S1.5): each module takes inputs and the current
//! monotonic time and returns what to send and what to log. The tokio tasks
//! that own the serial port, the channels and the database come later and
//! call into these modules; the `bess-core` binary starts with them.

pub mod fan;
pub mod link;

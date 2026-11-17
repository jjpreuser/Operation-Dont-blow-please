//! MCU ↔ Pi UART protocol, as specified in the firmware team's interface draft V2
//! (`Research/STM32_Pi_UART_Interface_Draft_V2.docx`, protocol version 0x03), DD-40.
//!
//! Bytes in, typed messages out, and the reverse. No serial I/O and no async:
//! the `bess-core` app (docs/design/architecture.md §2, not written yet) owns
//! the port, and an MCU simulator can use the same crate.
//! Values stay in wire units (10 mA, 0.1 °C, negative current = charging);
//! `bess-core` converts them to SI units once when it builds a sample (DD-72).

pub mod crc;
pub mod frame;
pub mod parser;

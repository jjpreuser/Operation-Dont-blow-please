//! CRC-16/CCITT-FALSE (UART draft §3): polynomial 0x1021, init 0xFFFF,
//! no reflection, XOR-out 0. Check value: ASCII `123456789` → 0x29B1.

const POLY: u16 = 0x1021;
const INIT: u16 = 0xFFFF;

/// CRC over `data`. In a frame this covers version through payload (frame offsets 2..=5+N).
pub fn crc16(data: &[u8]) -> u16 {
    data.iter().fold(INIT, |mut crc, &byte| {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ POLY
            } else {
                crc << 1
            };
        }
        crc
    })
}

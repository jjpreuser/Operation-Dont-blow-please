//! The reference module: the CONFIG payload is the frame's, and it is a
//! CONFIG the Pi accepts.

mod common;

use common::{hex, CONFIG};
use protocol::messages::CONFIG_LEN;
use protocol::reference;

#[test]
fn the_payload_is_the_frame_without_header_and_crc() {
    assert_eq!(
        reference::CONFIG_PAYLOAD.as_slice(),
        &hex(CONFIG)[6..6 + CONFIG_LEN]
    );
}

#[test]
fn the_reference_config_passes_the_checks() {
    let config = reference::config();
    assert_eq!(config.boot_counter, 1);
    assert_eq!(config.config_revision, 1);
    let checked = config.check(config.identity());
    assert!(checked.is_ok(), "{checked:?}");
}

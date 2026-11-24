//! The `mcu_config` table: every CONFIG the Pi validates, with its raw bytes
//! (docs/design/database.md §2; S2.4, S9.11; DD-43).

use super::{Result, Store};
use crate::wallclock::utc_text;
use jiff::Timestamp;
use protocol::config::Config;
use protocol::messages::{AckStatus, CONFIG_LEN};
use rusqlite::OptionalExtension;

impl Store {
    /// Stores a validated CONFIG with the `CONFIG_ACK` status it got. Only
    /// the first CONFIG of an identity is stored (the MCU repeats it until
    /// acknowledged); returns `false` for a repeat.
    ///
    /// # Errors
    ///
    /// SQLite errors.
    pub fn insert_mcu_config(
        &mut self,
        received: Timestamp,
        config: &Config,
        status: AckStatus,
    ) -> Result<bool> {
        let inserted = self
            .conn
            .prepare_cached(
                "INSERT INTO mcu_config
                     (boot_counter, config_revision, received_ts, config_schema, fw_version, status, raw)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (boot_counter, config_revision) DO NOTHING",
            )?
            .execute((
                config.boot_counter,
                config.config_revision,
                utc_text(received),
                config.schema,
                config.firmware.to_string(),
                u8::from(status),
                config.to_bytes().as_slice(),
            ))?;
        Ok(inserted > 0)
    }

    /// The newest accepted CONFIG, decoded from its raw bytes: what the daily
    /// record uses for `CAP_AH` when no CONFIG is installed (D3 §4).
    ///
    /// # Errors
    ///
    /// SQLite errors.
    pub fn latest_accepted_config(&self) -> Result<Option<Config>> {
        let raw: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT raw FROM mcu_config WHERE status = 0
                 ORDER BY received_ts DESC, rowid DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(raw
            .and_then(|raw| <[u8; CONFIG_LEN]>::try_from(raw.as_slice()).ok())
            .map(|bytes| Config::from_bytes(&bytes)))
    }
}

/// `CAP_AH` from a CONFIG: `capacity_mAh` / 1000.
#[must_use]
pub fn capacity_ah(config: &Config) -> f64 {
    f64::from(config.capacity_mah) / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_util::ts;

    /// A CONFIG for storage tests; the content rules do not matter here,
    /// only that the bytes come back unchanged.
    fn reference_config() -> Config {
        let mut config = Config::from_bytes(&[0; CONFIG_LEN]);
        config.schema = 1;
        config.boot_counter = 7;
        config.config_revision = 1;
        config.firmware = protocol::config::FirmwareVersion {
            major: 1,
            minor: 4,
            patch: 2,
        };
        config.capacity_mah = 9600;
        config
    }

    #[test]
    fn a_config_is_stored_once_per_identity_with_its_raw_bytes() {
        let mut store = Store::open_in_memory().unwrap();
        let config = reference_config();
        assert!(store
            .insert_mcu_config(ts("2026-11-24T10:00:00Z"), &config, AckStatus::Accepted)
            .unwrap());
        assert!(!store
            .insert_mcu_config(ts("2026-11-24T10:00:01Z"), &config, AckStatus::Accepted)
            .unwrap());
        let (received, schema, fw, status, raw): (String, i64, String, i64, Vec<u8>) = store
            .conn()
            .query_row(
                "SELECT received_ts, config_schema, fw_version, status, raw FROM mcu_config",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(received, "2026-11-24T10:00:00.000Z");
        assert_eq!(schema, i64::from(config.schema));
        assert_eq!(fw, config.firmware.to_string());
        assert_eq!(status, 0);
        assert_eq!(raw, config.to_bytes().to_vec());
    }

    #[test]
    fn the_newest_accepted_config_gives_the_capacity() {
        let mut store = Store::open_in_memory().unwrap();
        assert_eq!(store.latest_accepted_config().unwrap(), None);

        let mut old = reference_config();
        old.capacity_mah = 9000;
        store
            .insert_mcu_config(ts("2026-11-20T10:00:00Z"), &old, AckStatus::Accepted)
            .unwrap();
        let mut new = reference_config();
        new.config_revision += 1;
        new.capacity_mah = 12_000;
        store
            .insert_mcu_config(ts("2026-11-24T10:00:00Z"), &new, AckStatus::Accepted)
            .unwrap();
        // A newer rejected CONFIG does not count.
        let mut rejected = reference_config();
        rejected.config_revision += 2;
        rejected.capacity_mah = 1;
        store
            .insert_mcu_config(
                ts("2026-11-24T11:00:00Z"),
                &rejected,
                AckStatus::InvalidContent,
            )
            .unwrap();

        let latest = store.latest_accepted_config().unwrap().unwrap();
        assert_eq!(latest, new);
        assert!((capacity_ah(&latest) - 12.0).abs() < 1e-12);
    }
}

//! XDG configuration (`config.toml`) and atomic state persistence (`state.json`) for `tayassu`.

mod atomic;
pub mod config;
pub mod error;
pub mod paths;
pub mod state;

pub use config::AppConfig;
pub use error::StoreError;
pub use paths::{APP_DIR_NAME, CONFIG_FILE_NAME, PROFILES_DIR_NAME, STATE_FILE_NAME, XdgPaths};
pub use state::{RuntimeState, STATE_SCHEMA_VERSION};

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeDelta, Utc};
    use tayassu_core::{AccountConfig, CooldownEntry, ModelBucket, ProviderKind};
    use tempfile::TempDir;

    /// Cooldown duration in hours used for persistence round-trip tests.
    const TEST_COOLDOWN_HOURS: i64 = 5;

    #[test]
    fn config_round_trips_atomically() {
        let dir = TempDir::new().expect("temp dir");
        let paths = XdgPaths::under(dir.path());

        let mut cfg = AppConfig::default();
        cfg.upsert_account(AccountConfig::new(
            "agy-student".parse().expect("id"),
            ProviderKind::Agy,
            "Antigravity Student",
        ));
        cfg.save_atomic(&paths.config_file).expect("save config");

        let loaded = AppConfig::load(&paths.config_file).expect("load config");
        assert_eq!(loaded, cfg);
    }

    #[test]
    fn state_round_trips_atomically() {
        let dir = TempDir::new().expect("temp dir");
        let paths = XdgPaths::under(dir.path());
        let account_id = "agy-pro".parse().expect("id");

        let mut state = RuntimeState::default();
        state
            .status_for_mut(&account_id)
            .set_cooldown(CooldownEntry {
                bucket: ModelBucket::default_bucket(),
                resets_at: Utc::now() + TimeDelta::hours(TEST_COOLDOWN_HOURS),
                reason: "manual".to_owned(),
            });
        state.save_atomic(&paths.state_file).expect("save state");

        let loaded = RuntimeState::load(&paths.state_file).expect("load state");
        assert_eq!(loaded.schema_version, STATE_SCHEMA_VERSION);
        assert!(loaded.status_for(&account_id).is_some());
    }
}

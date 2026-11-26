use crate::error::StoreError;
use std::env;
use std::path::{Path, PathBuf};

/// Application directory name used under XDG base directories.
pub const APP_DIR_NAME: &str = "tayassu";

/// Configuration filename stored under `$XDG_CONFIG_HOME/tayassu/`.
pub const CONFIG_FILE_NAME: &str = "config.toml";

/// Runtime state filename stored under `$XDG_STATE_HOME/tayassu/`.
pub const STATE_FILE_NAME: &str = "state.json";

/// Profiles subdirectory stored under `$XDG_DATA_HOME/tayassu/`.
pub const PROFILES_DIR_NAME: &str = "profiles";

const ENV_HOME: &str = "HOME";
const ENV_XDG_CONFIG_HOME: &str = "XDG_CONFIG_HOME";
const ENV_XDG_STATE_HOME: &str = "XDG_STATE_HOME";
const ENV_XDG_DATA_HOME: &str = "XDG_DATA_HOME";

/// Resolved XDG filesystem layout for `tayassu`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XdgPaths {
    /// Real user `$HOME` directory.
    pub home_dir: PathBuf,
    /// Path to `config.toml`.
    pub config_file: PathBuf,
    /// Path to `state.json`.
    pub state_file: PathBuf,
    /// Root directory containing per-account isolated profiles.
    pub profiles_dir: PathBuf,
}

impl XdgPaths {
    /// Resolves XDG paths from the current process environment.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::MissingHomeDir`] if `HOME` is unset or empty.
    pub fn from_env() -> Result<Self, StoreError> {
        let home_dir = env::var_os(ENV_HOME)
            .filter(|val| !val.is_empty())
            .map(PathBuf::from)
            .ok_or(StoreError::MissingHomeDir)?;

        let config_root = env_path_or(ENV_XDG_CONFIG_HOME, &home_dir.join(".config"));
        let state_root = env_path_or(ENV_XDG_STATE_HOME, &home_dir.join(".local/state"));
        let data_root = env_path_or(ENV_XDG_DATA_HOME, &home_dir.join(".local/share"));

        Ok(Self {
            home_dir,
            config_file: config_root.join(APP_DIR_NAME).join(CONFIG_FILE_NAME),
            state_file: state_root.join(APP_DIR_NAME).join(STATE_FILE_NAME),
            profiles_dir: data_root.join(APP_DIR_NAME).join(PROFILES_DIR_NAME),
        })
    }

    /// Builds an isolated [`XdgPaths`] rooted at `base_dir` (useful for tests).
    #[must_use]
    pub fn under(base_dir: &Path) -> Self {
        Self {
            home_dir: base_dir.join("home"),
            config_file: base_dir.join("config").join(CONFIG_FILE_NAME),
            state_file: base_dir.join("state").join(STATE_FILE_NAME),
            profiles_dir: base_dir.join(PROFILES_DIR_NAME),
        }
    }
}

fn env_path_or(var: &str, fallback: &Path) -> PathBuf {
    env::var_os(var)
        .filter(|val| !val.is_empty())
        .map_or_else(|| fallback.to_path_buf(), PathBuf::from)
}

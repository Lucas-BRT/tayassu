use crate::atomic::{FileLock, write_atomic};
use crate::error::StoreError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use tayassu_core::{AccountConfig, AccountId, FallbackTarget, ProviderKind, SelectionPolicy};

/// Sibling lock filename for configuration read-modify-write operations.
const CONFIG_LOCK_FILE_NAME: &str = "config.lock";

/// Returns `true` for serde defaults where a feature is enabled unless explicitly disabled.
const fn default_true() -> bool {
    true
}

/// Top-level user configuration persisted in `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppConfig {
    /// Default routing policy when `--policy` is not passed on the CLI.
    #[serde(default)]
    pub default_policy: SelectionPolicy,
    /// Whether background update checks and terminal notices are enabled.
    #[serde(default = "default_true")]
    pub update_check: bool,
    /// Named cross-provider fallback chains (e.g. `claude = ["agy:claude", "claude:default"]`).
    #[serde(default)]
    pub fallback_chains: BTreeMap<String, Vec<FallbackTarget>>,
    /// Configured subscription accounts.
    #[serde(default)]
    pub accounts: Vec<AccountConfig>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            default_policy: SelectionPolicy::default(),
            update_check: true,
            fallback_chains: BTreeMap::new(),
            accounts: Vec::new(),
        }
    }
}

impl AppConfig {
    /// Loads `config.toml` from `path`, returning an empty default config if the file does not exist.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] or [`StoreError::ConfigParse`] on read/parse failures.
    pub fn load(path: &Path) -> Result<Self, StoreError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path).map_err(|source| StoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        toml::from_str(&raw).map_err(|source| StoreError::ConfigParse {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Atomically writes this configuration to `path`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::ConfigSerialize`] or [`StoreError::Io`].
    pub fn save_atomic(&self, path: &Path) -> Result<(), StoreError> {
        let serialized = toml::to_string_pretty(self)?;
        write_atomic(path, serialized.as_bytes())
    }

    /// Loads the latest config and runs an operation while holding its exclusive lock.
    ///
    /// # Errors
    ///
    /// Returns lock, load or operation errors.
    pub fn with_current_locked<T, E>(
        path: &Path,
        operation: impl FnOnce(Self) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<StoreError>,
    {
        let _lock = FileLock::acquire(path.with_file_name(CONFIG_LOCK_FILE_NAME))?;
        operation(Self::load(path)?)
    }

    /// Returns the first unused account id for `provider` (`claude`, `claude-2`, `claude-3`, ...).
    ///
    /// # Panics
    ///
    /// Never in practice: the candidate sequence is unbounded and ids are short valid slugs.
    #[must_use]
    pub fn next_free_id(&self, provider: ProviderKind) -> AccountId {
        let base = provider.as_str();
        (1_u32..)
            .map(|n| {
                if n == 1 {
                    base.to_owned()
                } else {
                    format!("{base}-{n}")
                }
            })
            .find_map(|candidate| {
                candidate
                    .parse::<AccountId>()
                    .ok()
                    .filter(|id| self.find_account(id).is_none())
            })
            .expect("unbounded candidate sequence yields a free id")
    }

    /// Looks up a configured account by `id`.
    #[must_use]
    pub fn find_account(&self, id: &AccountId) -> Option<&AccountConfig> {
        self.accounts.iter().find(|account| &account.id == id)
    }

    /// Looks up a configured account for editing.
    pub fn find_account_mut(&mut self, id: &AccountId) -> Option<&mut AccountConfig> {
        self.accounts.iter_mut().find(|account| &account.id == id)
    }

    /// Inserts or replaces an account configuration by its [`AccountId`].
    pub fn upsert_account(&mut self, account: AccountConfig) {
        self.accounts.retain(|existing| existing.id != account.id);
        self.accounts.push(account);
    }

    /// Removes an account configuration by `id`. Returns `true` if removed.
    pub fn remove_account(&mut self, id: &AccountId) -> bool {
        let before = self.accounts.len();
        self.accounts.retain(|existing| &existing.id != id);
        self.accounts.len() < before
    }
}

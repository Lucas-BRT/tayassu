use crate::atomic::write_atomic;
use crate::error::StoreError;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use tayassu_core::{AccountConfig, AccountId, SelectionPolicy};

/// Top-level user configuration persisted in `config.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppConfig {
    /// Default routing policy when `--policy` is not passed on the CLI.
    #[serde(default)]
    pub default_policy: SelectionPolicy,
    /// Configured subscription accounts.
    #[serde(default)]
    pub accounts: Vec<AccountConfig>,
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

    /// Looks up a configured account by `id`.
    #[must_use]
    pub fn find_account(&self, id: &AccountId) -> Option<&AccountConfig> {
        self.accounts.iter().find(|account| &account.id == id)
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

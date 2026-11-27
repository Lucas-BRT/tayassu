use crate::atomic::write_atomic;
use crate::error::StoreError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use tayassu_core::{AccountId, ProviderKind, SelectionPolicy};

/// Per-repository policy filename discovered upward from the working directory.
pub const PROJECT_CONFIG_FILE_NAME: &str = ".tayassu.toml";

/// Optional per-project overrides loaded from `.tayassu.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectConfig {
    /// Optional default routing policy for this repository.
    #[serde(default)]
    pub default_policy: Option<SelectionPolicy>,
    /// Optional default fallback chain name for this repository.
    #[serde(default)]
    pub fallback_chain: Option<String>,
    /// Preferred account id per provider (e.g. `claude = "claude-work"`).
    #[serde(default)]
    pub preferred_accounts: BTreeMap<String, AccountId>,
}

impl ProjectConfig {
    /// Walks upward from `start_dir` looking for `.tayassu.toml` and parses the nearest one.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] or [`StoreError::ConfigParse`] if a file is found but invalid.
    pub fn discover(start_dir: &Path) -> Result<Option<(PathBuf, Self)>, StoreError> {
        let mut current = Some(start_dir);
        while let Some(dir) = current {
            let candidate = dir.join(PROJECT_CONFIG_FILE_NAME);
            if candidate.is_file() {
                let raw = fs::read_to_string(&candidate).map_err(|source| StoreError::Io {
                    path: candidate.clone(),
                    source,
                })?;
                let parsed: Self =
                    toml::from_str(&raw).map_err(|source| StoreError::ConfigParse {
                        path: candidate.clone(),
                        source,
                    })?;
                return Ok(Some((candidate, parsed)));
            }
            current = dir.parent();
        }
        Ok(None)
    }

    /// Returns the preferred account id configured for `provider`, if any.
    #[must_use]
    pub fn preferred_account_for(&self, provider: ProviderKind) -> Option<&AccountId> {
        self.preferred_accounts.get(provider.as_str())
    }

    /// Atomically writes this project configuration to `path`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::ConfigSerialize`] or [`StoreError::Io`].
    pub fn save_atomic(&self, path: &Path) -> Result<(), StoreError> {
        let serialized = toml::to_string_pretty(self)?;
        write_atomic(path, serialized.as_bytes())
    }
}

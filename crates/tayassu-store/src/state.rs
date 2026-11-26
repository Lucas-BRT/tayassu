use crate::atomic::write_atomic;
use crate::error::StoreError;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use tayassu_core::{AccountId, AccountStatus};

/// Current schema version written to `state.json`.
pub const STATE_SCHEMA_VERSION: u32 = 1;

/// Persisted quota and cooldown runtime state (`state.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeState {
    /// Forward-compatibility schema version.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// Per-account quota snapshots and active cooldowns.
    #[serde(default)]
    pub statuses: Vec<AccountStatus>,
}

const fn default_schema_version() -> u32 {
    STATE_SCHEMA_VERSION
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            statuses: Vec::new(),
        }
    }
}

impl RuntimeState {
    /// Loads `state.json` from `path`, returning an empty state if the file does not exist.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] or [`StoreError::StateParse`] on read/parse failures.
    pub fn load(path: &Path) -> Result<Self, StoreError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path).map_err(|source| StoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        serde_json::from_str(&raw).map_err(|source| StoreError::StateParse {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Atomically writes this runtime state to `path`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::StateSerialize`] or [`StoreError::Io`].
    pub fn save_atomic(&self, path: &Path) -> Result<(), StoreError> {
        let serialized = serde_json::to_vec_pretty(self)?;
        write_atomic(path, &serialized)
    }

    /// Looks up the runtime status for `account_id`.
    #[must_use]
    pub fn status_for(&self, account_id: &AccountId) -> Option<&AccountStatus> {
        self.statuses
            .iter()
            .find(|status| &status.account_id == account_id)
    }

    /// Returns a mutable reference to `account_id`'s status, creating an empty entry if absent.
    pub fn status_for_mut(&mut self, account_id: &AccountId) -> &mut AccountStatus {
        if let Some(pos) = self
            .statuses
            .iter()
            .position(|status| &status.account_id == account_id)
        {
            return &mut self.statuses[pos];
        }
        self.statuses.push(AccountStatus::empty(account_id.clone()));
        let last = self.statuses.len() - 1;
        &mut self.statuses[last]
    }
}

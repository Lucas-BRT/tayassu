use crate::atomic::{FileLock, write_atomic};
use crate::error::StoreError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use tayassu_core::{AccountId, AccountStatus, ProviderKind};

/// Current schema version written to `state.json`.
pub const STATE_SCHEMA_VERSION: u32 = 1;

/// Lock filename sibling to `state.json` guarding concurrent read-modify-write updates.
const STATE_LOCK_FILE_NAME: &str = "state.lock";

/// Persisted quota, cooldown, and per-directory session state (`state.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeState {
    /// Forward-compatibility schema version.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// Per-account quota snapshots, active cooldowns, and active session counts.
    #[serde(default)]
    pub statuses: Vec<AccountStatus>,
    /// Last account used per `<cwd>::<provider>` key to support `--continue`/`--resume`.
    #[serde(default)]
    pub last_account_by_cwd: BTreeMap<String, AccountId>,
}

const fn default_schema_version() -> u32 {
    STATE_SCHEMA_VERSION
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            statuses: Vec::new(),
            last_account_by_cwd: BTreeMap::new(),
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

    /// Acquires an exclusive OS file lock on `state.lock`, reloads `state.json`, applies `mutate`,
    /// and atomically writes the updated state back to disk.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if locking, loading, or saving fails.
    pub fn update_locked<T, F>(path: &Path, mutate: F) -> Result<(Self, T), StoreError>
    where
        F: FnOnce(&mut Self) -> T,
    {
        let _lock = FileLock::acquire(path.with_file_name(STATE_LOCK_FILE_NAME))?;
        let mut state = Self::load(path)?;
        let result = mutate(&mut state);
        state.save_atomic(path)?;
        Ok((state, result))
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

    /// Records the last account used in `cwd` for `provider`.
    pub fn record_cwd_account(
        &mut self,
        cwd: &Path,
        provider: ProviderKind,
        account_id: AccountId,
    ) {
        let key = cwd_provider_key(cwd, provider);
        self.last_account_by_cwd.insert(key, account_id);
    }

    /// Looks up the last account used in `cwd` for `provider`.
    #[must_use]
    pub fn last_cwd_account(&self, cwd: &Path, provider: ProviderKind) -> Option<&AccountId> {
        let key = cwd_provider_key(cwd, provider);
        self.last_account_by_cwd.get(&key)
    }
}

fn cwd_provider_key(cwd: &Path, provider: ProviderKind) -> String {
    format!("{}::{}", cwd.display(), provider)
}

use crate::session::process_is_alive;
use anyhow::{Context, Result};
use std::io::Write;
use tayassu_core::{AccountConfig, AccountId};
use tayassu_store::{AppConfig, RuntimeState, XdgPaths};

/// A CLI command that runs against the loaded [`App`] and writes its report to `out`.
pub trait Execute {
    /// Runs the command.
    ///
    /// # Errors
    ///
    /// Returns any failure of the command itself, with context attached.
    fn execute(self, app: &mut App, out: &mut dyn Write) -> Result<()>;
}

/// Loaded configuration, runtime state and resolved XDG paths shared by every command.
pub struct App {
    pub paths: XdgPaths,
    pub config: AppConfig,
    pub state: RuntimeState,
}

impl App {
    pub fn load() -> Result<Self> {
        let paths = XdgPaths::from_env().context("failed to resolve XDG directories")?;
        let config = AppConfig::load(&paths.config_file)
            .with_context(|| format!("failed to load {}", paths.config_file.display()))?;
        let state = RuntimeState::load(&paths.state_file)
            .with_context(|| format!("failed to load {}", paths.state_file.display()))?;
        Ok(Self {
            paths,
            config,
            state,
        })
    }

    /// Drops sessions left behind by `tayassu` processes that were killed before cleanup.
    pub fn prune_stale_sessions(&mut self) -> Result<()> {
        if !self
            .state
            .statuses
            .iter()
            .any(|status| status.sessions_stale(process_is_alive))
        {
            return Ok(());
        }
        let (updated, ()) = RuntimeState::update_locked(&self.paths.state_file, |state| {
            for status in &mut state.statuses {
                status.prune_sessions(process_is_alive);
            }
        })
        .with_context(|| format!("failed to save {}", self.paths.state_file.display()))?;
        self.state = updated;
        Ok(())
    }

    /// Atomically persists `config.toml`.
    pub fn save_config(&self) -> Result<()> {
        self.config
            .save_atomic(&self.paths.config_file)
            .with_context(|| format!("failed to save {}", self.paths.config_file.display()))
    }

    /// Looks up a configured account or fails with a uniform message.
    pub fn account(&self, id: &AccountId) -> Result<&AccountConfig> {
        self.config
            .find_account(id)
            .with_context(|| format!("account '{id}' not found"))
    }
}

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

    /// Runs `operation` against the latest `config.toml` loaded under an exclusive lock.
    pub fn with_current_config(
        &mut self,
        operation: impl FnOnce(&mut Self) -> Result<()>,
    ) -> Result<()> {
        AppConfig::with_current_locked(&self.paths.config_file.clone(), |config| {
            self.config = config;
            operation(self)
        })
    }
}

use crate::app::{App, Execute};
use crate::shell::account_candidates;
use anyhow::{Result, bail};
use clap::Args;
use clap_complete::ArgValueCandidates;
use std::io::Write;
use tayassu_core::AccountId;

/// Remove an account from `config.toml` (profile data stays on disk).
#[derive(Debug, Args)]
pub struct Rm {
    /// Account identifier to remove.
    #[arg(add = ArgValueCandidates::new(account_candidates))]
    id: AccountId,
}

impl Execute for Rm {
    fn execute(self, app: &mut App, out: &mut dyn Write) -> Result<()> {
        if !app.config.remove_account(&self.id) {
            bail!("account '{}' not found", self.id);
        }
        app.save_config()?;
        writeln!(out, "Removed account '{}' from config", self.id)?;
        Ok(())
    }
}

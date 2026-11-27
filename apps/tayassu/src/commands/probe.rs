use crate::app::{App, Execute};
use crate::probing::probe_accounts;
use crate::shell::account_candidates;
use anyhow::Result;
use clap::Args;
use clap_complete::ArgValueCandidates;
use std::io::Write;
use tayassu_core::{AccountConfig, AccountId};

/// Refresh quota windows and cooldowns by probing each provider CLI.
#[derive(Debug, Args)]
pub struct Probe {
    /// Limit probing to a single account id.
    #[arg(add = ArgValueCandidates::new(account_candidates))]
    id: Option<AccountId>,
    /// Send a desktop notification (`notify-send`) when a previously active cooldown clears.
    #[arg(long)]
    notify: bool,
}

impl Execute for Probe {
    fn execute(self, app: &mut App, out: &mut dyn Write) -> Result<()> {
        let targets: Vec<AccountConfig> = match self.id.as_ref() {
            Some(target_id) => vec![app.account(target_id)?.clone()],
            None => app.config.accounts.clone(),
        };

        if targets.is_empty() {
            writeln!(out, "No accounts configured to probe.")?;
            return Ok(());
        }

        probe_accounts(
            &targets,
            &app.config.accounts,
            &app.paths,
            &mut app.state,
            self.notify,
            Some(out),
        )?;
        Ok(())
    }
}

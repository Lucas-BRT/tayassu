use crate::app::{App, Execute};
use crate::completion;
use crate::probing::probe_accounts;
use anyhow::{Context, Result};
use clap::Args;
use clap_complete::engine::ArgValueCandidates;
use std::io::Write;
use tayassu_core::AccountConfig;
use tayassu_core::AccountId;

/// Refresh quota windows and cooldowns by probing each provider CLI.
#[derive(Debug, Args)]
pub struct Probe {
    /// Limit probing to a single account id.
    #[arg(add = ArgValueCandidates::new(completion::account_ids))]
    id: Option<AccountId>,
    /// Send a desktop notification (`notify-send`) when a previously active cooldown clears.
    #[arg(long)]
    notify: bool,
}

impl Execute for Probe {
    fn execute(self, app: &mut App, out: &mut dyn Write) -> Result<()> {
        let id = self.id.as_ref();
        let notify = self.notify;
        let targets: Vec<AccountConfig> = match id {
            Some(target_id) => vec![
                app.config
                    .find_account(target_id)
                    .with_context(|| format!("account '{target_id}' not found"))?
                    .clone(),
            ],
            None => app.config.accounts.clone(),
        };

        if targets.is_empty() {
            writeln!(out, "No accounts configured to probe.")?;
            return Ok(());
        }

        probe_accounts(&targets, &app.paths, &mut app.state, notify, Some(out))?;
        Ok(())
    }
}

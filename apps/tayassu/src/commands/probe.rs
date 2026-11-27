use crate::app::{App, Execute};
use crate::probing::probe_accounts;
use crate::shell::probe_target_candidates;
use anyhow::{Result, bail};
use clap::Args;
use clap_complete::ArgValueCandidates;
use std::io::Write;
use tayassu_core::{AccountConfig, AccountId, ProviderKind};

/// Refresh quota windows and cooldowns by probing each provider CLI.
#[derive(Debug, Args)]
pub struct Probe {
    /// Limit probing to a single account id or provider.
    #[arg(value_name = "ACCOUNT|PROVIDER", add = ArgValueCandidates::new(probe_target_candidates))]
    target: Option<String>,
    /// Send a desktop notification (`notify-send`) when a previously active cooldown clears.
    #[arg(long)]
    notify: bool,
}

impl Execute for Probe {
    fn execute(self, app: &mut App, out: &mut dyn Write) -> Result<()> {
        let targets: Vec<AccountConfig> = match self.target.as_deref() {
            Some(raw) => {
                if let Ok(id) = raw.parse::<AccountId>()
                    && let Some(account) = app.config.find_account(&id)
                {
                    vec![account.clone()]
                } else if let Ok(provider) = raw.parse::<ProviderKind>() {
                    let matching: Vec<_> = app
                        .config
                        .accounts
                        .iter()
                        .filter(|account| account.provider == provider)
                        .cloned()
                        .collect();
                    if matching.is_empty() {
                        bail!("no accounts configured for provider '{provider}'");
                    }
                    matching
                } else {
                    bail!("account or provider '{raw}' not found");
                }
            }
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

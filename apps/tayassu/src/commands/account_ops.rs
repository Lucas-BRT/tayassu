use crate::app::{App, Execute};
use crate::shell::account_candidates;
use anyhow::{Context, Result};
use clap::Args;
use clap_complete::ArgValueCandidates;
use std::io::Write;
use tayassu_core::AccountId;

/// Edit an account's label or selection priority.
#[derive(Debug, Args)]
pub struct Edit {
    /// Account identifier.
    #[arg(add = ArgValueCandidates::new(account_candidates))]
    id: AccountId,
    /// New display label.
    #[arg(long, required_unless_present = "priority")]
    label: Option<String>,
    /// New selection priority (lower values are preferred).
    #[arg(long)]
    priority: Option<u8>,
}

impl Execute for Edit {
    fn execute(self, app: &mut App, out: &mut dyn Write) -> Result<()> {
        app.with_current_config(|locked| {
            let account = locked
                .config
                .find_account_mut(&self.id)
                .with_context(|| format!("account '{}' not found", self.id))?;
            if let Some(label) = self.label {
                account.label = label;
            }
            if let Some(priority) = self.priority {
                account.priority = priority;
            }
            locked.save_config()?;
            writeln!(out, "Updated account '{}'", self.id)?;
            Ok(())
        })
    }
}

/// Include an account in automatic routing.
#[derive(Debug, Args)]
pub struct Enable {
    /// Account identifier.
    #[arg(add = ArgValueCandidates::new(account_candidates))]
    id: AccountId,
}

impl Execute for Enable {
    fn execute(self, app: &mut App, out: &mut dyn Write) -> Result<()> {
        set_enabled(app, &self.id, true, out)
    }
}

/// Pause automatic routing to an account while keeping its profile.
#[derive(Debug, Args)]
pub struct Disable {
    /// Account identifier.
    #[arg(add = ArgValueCandidates::new(account_candidates))]
    id: AccountId,
}

impl Execute for Disable {
    fn execute(self, app: &mut App, out: &mut dyn Write) -> Result<()> {
        set_enabled(app, &self.id, false, out)
    }
}

fn set_enabled(app: &mut App, id: &AccountId, enabled: bool, out: &mut dyn Write) -> Result<()> {
    app.with_current_config(|locked| {
        locked
            .config
            .find_account_mut(id)
            .with_context(|| format!("account '{id}' not found"))?
            .enabled = enabled;
        locked.save_config()?;
        let verb = if enabled { "Enabled" } else { "Disabled" };
        writeln!(out, "{verb} account '{id}'")?;
        Ok(())
    })
}

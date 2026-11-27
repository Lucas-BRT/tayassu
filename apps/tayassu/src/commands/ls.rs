use crate::app::{App, Execute};
use crate::completion;
use crate::render::StatusTable;
use anyhow::{Context, Result};
use chrono::Utc;
use clap::Args;
use clap_complete::engine::ArgValueCandidates;
use std::io::Write;
use tayassu_core::{ModelBucket, ProviderKind};

/// List accounts with remaining quota and cooldowns (default command).
#[derive(Debug, Default, Args)]
pub struct Ls {
    /// Show only this provider.
    #[arg(add = ArgValueCandidates::new(completion::configured_providers))]
    provider: Option<ProviderKind>,
    /// Model bucket to evaluate availability against.
    #[arg(long)]
    model: Option<ModelBucket>,
    /// Print machine-readable JSON instead of a table.
    #[arg(long)]
    json: bool,
}

impl Execute for Ls {
    fn execute(self, app: &mut App, mut out: &mut dyn Write) -> Result<()> {
        if self.json {
            let accounts: Vec<_> = app
                .config
                .accounts
                .iter()
                .filter(|account| self.provider.is_none_or(|p| account.provider == p))
                .collect();
            let now = Utc::now();
            let quota: Vec<_> = app
                .state
                .statuses
                .iter()
                .flat_map(|status| {
                    status.windows.iter().map(move |window| {
                        serde_json::json!({
                            "account": status.account_id,
                            "bucket": window.bucket,
                            "window": window.window_name,
                            "remaining": window.remaining,
                            "resets_at": window.resets_at,
                            "probed_at": status.last_probed_at,
                            "state": window.state(status.last_probed_at, now),
                        })
                    })
                })
                .collect();
            let json = serde_json::json!({
                "accounts": accounts,
                "statuses": app.state.statuses,
                "quota": quota,
            });
            return writeln!(out, "{json:#}").context("failed to write JSON");
        }
        StatusTable::new(
            &app.config.accounts,
            &app.state.statuses,
            self.provider,
            self.model.as_ref(),
        )
        .write_to(&mut out, Utc::now())
        .context("failed to write status table")
    }
}

use crate::app::{App, Execute};
use crate::render::StatusTable;
use crate::shell::{model_candidates, provider_candidates};
use anyhow::{Context, Result};
use chrono::Utc;
use clap::Args;
use clap_complete::ArgValueCandidates;
use std::io::Write;
use tayassu_core::{ModelBucket, ProviderKind};

/// List accounts with remaining quota and cooldowns (default command).
#[derive(Debug, Default, Args)]
pub struct Ls {
    /// Show only this provider.
    #[arg(add = ArgValueCandidates::new(provider_candidates))]
    provider: Option<ProviderKind>,
    /// Model bucket to evaluate availability against.
    #[arg(long, add = ArgValueCandidates::new(model_candidates))]
    model: Option<ModelBucket>,
    /// List discovered `OpenCode` models and their measured quota.
    #[arg(long, conflicts_with = "model")]
    list_models: bool,
    /// Print machine-readable JSON instead of a table.
    #[arg(long)]
    json: bool,
}

impl Execute for Ls {
    fn execute(self, app: &mut App, mut out: &mut dyn Write) -> Result<()> {
        let now = Utc::now();
        if self.json {
            let accounts: Vec<_> = app
                .config
                .accounts
                .iter()
                .filter(|account| self.provider.is_none_or(|p| account.provider == p))
                .collect();
            let quota: Vec<_> = app
                .state
                .statuses
                .iter()
                .filter(|status| {
                    accounts
                        .iter()
                        .any(|account| account.id == status.account_id)
                })
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
                "statuses": app.state.statuses.iter().filter(|status| accounts.iter().any(|account| account.id == status.account_id)).collect::<Vec<_>>(),
                "quota": quota,
            });
            return writeln!(out, "{json:#}").context("failed to write JSON");
        }
        let table = StatusTable::new(
            &app.config.accounts,
            &app.state.statuses,
            self.provider,
            self.model.as_ref(),
        );
        if self.list_models {
            table
                .write_models(&mut out, now)
                .context("failed to write model quota")
        } else {
            table
                .write_to(&mut out, now)
                .context("failed to write status table")
        }
    }
}

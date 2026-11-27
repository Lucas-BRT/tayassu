use crate::app::{App, Execute};
use crate::shell::account_candidates;
use anyhow::{Context, Result, bail};
use chrono::{TimeDelta, Utc};
use clap::{Args, Subcommand};
use clap_complete::ArgValueCandidates;
use std::io::Write;
use tayassu_core::{AccountId, CooldownEntry, ModelBucket};
use tayassu_store::RuntimeState;

/// Default manual cooldown length in minutes (5 hours).
pub const DEFAULT_MANUAL_COOLDOWN_MINUTES: u64 = 300;

/// Inspect or edit cooldowns by hand (normally managed by `run` and `probe`).
#[derive(Debug, Args)]
pub struct Cooldown {
    #[command(subcommand)]
    action: CooldownAction,
}

/// Manual cooldown management subcommands.
#[derive(Debug, Subcommand)]
enum CooldownAction {
    /// List active cooldowns.
    Ls,
    /// Mark an account (and optional model bucket) as cooling down.
    Set {
        /// Account identifier.
        #[arg(add = ArgValueCandidates::new(account_candidates))]
        id: AccountId,
        /// Cooldown duration in minutes.
        #[arg(long, default_value_t = DEFAULT_MANUAL_COOLDOWN_MINUTES)]
        minutes: u64,
        /// Model bucket to block (defaults to `default`).
        #[arg(long)]
        model: Option<ModelBucket>,
        /// Short reason stored in `state.json`.
        #[arg(long, default_value = "manual")]
        reason: String,
    },
    /// Clear a recorded cooldown for an account.
    Clear {
        /// Account identifier.
        #[arg(add = ArgValueCandidates::new(account_candidates))]
        id: AccountId,
        /// Model bucket to clear (defaults to `default`).
        #[arg(long)]
        model: Option<ModelBucket>,
    },
}

impl Execute for Cooldown {
    fn execute(self, app: &mut App, out: &mut dyn Write) -> Result<()> {
        match self.action {
            CooldownAction::Ls => {
                let mut any = false;
                for status in &app.state.statuses {
                    for entry in &status.cooldowns {
                        any = true;
                        writeln!(
                            out,
                            "{} (bucket '{}') until {} [{}]",
                            status.account_id,
                            entry.bucket,
                            entry.resets_at.to_rfc3339(),
                            entry.reason
                        )?;
                    }
                }
                if !any {
                    writeln!(out, "No active cooldowns.")?;
                }
            }
            CooldownAction::Set {
                id,
                minutes,
                model,
                reason,
            } => {
                if app.config.find_account(&id).is_none() {
                    bail!("account '{id}' not found");
                }
                let resets_at = i64::try_from(minutes)
                    .ok()
                    .and_then(TimeDelta::try_minutes)
                    .and_then(|delta| Utc::now().checked_add_signed(delta))
                    .context("cooldown duration exceeds supported range")?;
                let bucket = model.unwrap_or_else(ModelBucket::default_bucket);
                let bucket_for_lock = bucket.clone();
                let (updated, ()) = RuntimeState::update_locked(&app.paths.state_file, |state| {
                    state.status_for_mut(&id).set_cooldown(CooldownEntry {
                        bucket: bucket_for_lock,
                        resets_at,
                        reason,
                    });
                })
                .with_context(|| format!("failed to save {}", app.paths.state_file.display()))?;
                app.state = updated;
                writeln!(
                    out,
                    "Cooldown set for '{id}' (bucket '{bucket}') until {}",
                    resets_at.to_rfc3339()
                )?;
            }
            CooldownAction::Clear { id, model } => {
                let bucket = model.unwrap_or_else(ModelBucket::default_bucket);
                let bucket_for_lock = bucket.clone();
                let (updated, removed) =
                    RuntimeState::update_locked(&app.paths.state_file, |state| {
                        state.status_for_mut(&id).clear_cooldown(&bucket_for_lock)
                    })
                    .with_context(|| {
                        format!("failed to save {}", app.paths.state_file.display())
                    })?;
                app.state = updated;
                if removed {
                    writeln!(out, "Cleared cooldown for '{id}' (bucket '{bucket}')")?;
                } else {
                    writeln!(out, "No active cooldown for '{id}' (bucket '{bucket}')")?;
                }
            }
        }
        Ok(())
    }
}

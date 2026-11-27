use crate::app::{App, Execute};
use crate::shell::provider_candidates;
use crate::spawn::transfer_opencode_auth;
use anyhow::{Context, Result};
use clap::Args;
use clap_complete::ArgValueCandidates;
use std::env;
use std::io::Write;
use tayassu_core::{AccountConfig, DEFAULT_ACCOUNT_PRIORITY, ProviderKind};
use tayassu_providers::{
    AdapterRegistry, DiscoveredAgent, FAKE_HOME_SUBDIR, HostDiscovery, IsolationStrategy,
    MigrationMode, ProfileEnvironment,
};
use tracing::warn;

/// Auto-discover installed AI CLIs and safely import their host credentials into isolated profiles.
#[derive(Debug, Args)]
#[command(alias = "discover")]
pub struct Import {
    /// Include only specific provider(s) (repeatable). Defaults to all discovered providers.
    #[arg(long = "provider", add = ArgValueCandidates::new(provider_candidates))]
    include: Vec<ProviderKind>,
    /// Exclude specific provider(s) from discovery/import (repeatable).
    #[arg(long = "exclude", add = ArgValueCandidates::new(provider_candidates))]
    exclude: Vec<ProviderKind>,
    /// Preview discovered agents, files, and update-safety notes without modifying disk.
    #[arg(long)]
    dry_run: bool,
    /// Refresh credentials in profiles that are already registered.
    #[arg(long)]
    refresh: bool,
    /// Remove original host auth files after copying (never removes binaries or updater dirs).
    #[arg(long)]
    move_auth: bool,
    /// Selection priority assigned to imported accounts.
    #[arg(long, default_value_t = DEFAULT_ACCOUNT_PRIORITY)]
    priority: u8,
}

impl Execute for Import {
    fn execute(self, app: &mut App, out: &mut dyn Write) -> Result<()> {
        if self.dry_run {
            return self.run_with(app, out);
        }
        app.with_current_config(|locked| self.run_with(locked, out))
    }
}

impl Import {
    fn run_with(&self, app: &mut App, out: &mut dyn Write) -> Result<()> {
        let discovered: Vec<_> = HostDiscovery
            .discover_all(&app.paths.home_dir, env::var_os("PATH").as_deref())
            .into_iter()
            .filter(|agent| self.include.is_empty() || self.include.contains(&agent.provider))
            .filter(|agent| !self.exclude.contains(&agent.provider))
            .collect();

        if discovered.is_empty() {
            writeln!(
                out,
                "No matching installed AI CLI agents found. Check PATH or add an account with `tayassu add <provider> [id]`."
            )?;
            return Ok(());
        }

        let mode = if self.move_auth {
            MigrationMode::MoveAuth
        } else {
            MigrationMode::Copy
        };

        for agent in &discovered {
            let existing = app.config.find_account(&agent.default_account_id);
            if let Some(existing) = existing {
                if existing.provider != agent.provider {
                    writeln!(
                        out,
                        "Skipping '{}': ID already belongs to {}",
                        existing.id, existing.provider
                    )?;
                    continue;
                }
                if !self.refresh {
                    writeln!(
                        out,
                        "Skipping '{}': already configured (use --refresh to copy credentials again)",
                        existing.id
                    )?;
                    continue;
                }
            }
            let account = existing.cloned().unwrap_or_else(|| {
                let mut account = AccountConfig::new(
                    agent.default_account_id.clone(),
                    agent.provider,
                    format!("{} (imported)", agent.provider),
                );
                account.priority = self.priority;
                account
            });

            let adapter = AdapterRegistry.adapter_for(agent.provider);
            let profile_env = adapter.build_environment(&account, &app.paths.profiles_dir);

            if self.dry_run {
                write_preview(agent, &profile_env, mode, out)?;
                continue;
            }

            let summary = agent
                .apply(&profile_env, &app.paths.home_dir, mode)
                .with_context(|| format!("failed to import {}", agent.provider))?;

            if existing.is_none() {
                app.config.upsert_account(account);
                app.save_config()?;
            }

            writeln!(
                out,
                "Imported '{}' ({}) -> {} ({} file(s) copied, {} host auth file(s) moved)",
                agent.default_account_id,
                agent.provider,
                profile_env.profile_dir.display(),
                summary.copied_files,
                summary.removed_source_files
            )?;
            writeln!(out, "  update-safety: {}", agent.update_safety_note)?;
            let credentials_transferred = agent.provider == ProviderKind::Opencode
                && match transfer_opencode_auth(&agent.binary_path, &profile_env) {
                    Ok(()) => {
                        writeln!(out, "  credentials: exported from host and imported")?;
                        true
                    }
                    Err(err) => {
                        warn!(error = %err, "opencode credential transfer failed");
                        false
                    }
                };
            if summary.requires_relogin && !credentials_transferred {
                writeln!(
                    out,
                    "  next: run `tayassu login {}`",
                    agent.default_account_id
                )?;
            }
        }
        Ok(())
    }
}

fn write_preview(
    agent: &DiscoveredAgent,
    profile: &ProfileEnvironment,
    mode: MigrationMode,
    out: &mut dyn Write,
) -> Result<()> {
    writeln!(
        out,
        "[dry-run] {} -> account '{}' (binary: {}, {} credential/config file(s) found)",
        agent.provider,
        agent.default_account_id,
        agent.binary_path.display(),
        agent.existing_file_count()
    )?;
    let root = match profile.strategy {
        IsolationStrategy::NativeEnvVar { .. } => profile.profile_dir.clone(),
        IsolationStrategy::FakeHome { .. } => profile.profile_dir.join(FAKE_HOME_SUBDIR),
    };
    for file in agent.files.iter().filter(|file| file.exists) {
        let action = if mode == MigrationMode::MoveAuth && file.removable_on_move {
            "copy, then remove source"
        } else {
            "copy; keep source"
        };
        writeln!(
            out,
            "  {} -> {} ({action})",
            file.source_path.display(),
            root.join(&file.dest_rel).display()
        )?;
    }
    writeln!(out, "  update-safety: {}", agent.update_safety_note)?;
    if agent.requires_relogin {
        writeln!(
            out,
            "  note: run `tayassu login {}` after import",
            agent.default_account_id
        )?;
    }
    Ok(())
}

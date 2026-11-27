use crate::app::{App, Execute};
use crate::spawn::transfer_opencode_auth;
use anyhow::{Context, Result};
use clap::Args;
use std::env;
use std::io::Write;
use tayassu_core::{AccountConfig, DEFAULT_ACCOUNT_PRIORITY, ProviderKind};
use tayassu_providers::{AdapterRegistry, HostDiscovery, MigrationMode};
use tracing::warn;

/// Discover installed AI CLIs and copy their credentials into isolated profiles.
#[derive(Debug, Args)]
pub struct Import {
    /// Include only specific provider(s) (repeatable). Defaults to all discovered providers.
    #[arg(long = "provider")]
    include: Vec<ProviderKind>,
    /// Exclude specific provider(s) from discovery/import (repeatable).
    #[arg(long = "exclude")]
    exclude: Vec<ProviderKind>,
    /// Show what would be imported without writing anything.
    #[arg(long)]
    dry_run: bool,
    /// Remove original host auth files after verified copy (never removes binaries or updater dirs).
    #[arg(long)]
    move_auth: bool,
    /// Selection priority assigned to imported accounts.
    #[arg(long, default_value_t = DEFAULT_ACCOUNT_PRIORITY)]
    priority: u8,
}

impl Execute for Import {
    fn execute(self, app: &mut App, out: &mut dyn Write) -> Result<()> {
        let (include, exclude) = (&self.include, &self.exclude);
        let (dry_run, move_auth, priority) = (self.dry_run, self.move_auth, self.priority);
        let discovered: Vec<_> = HostDiscovery
            .discover_all(&app.paths.home_dir, env::var_os("PATH").as_deref())
            .into_iter()
            .filter(|agent| include.is_empty() || include.contains(&agent.provider))
            .filter(|agent| !exclude.contains(&agent.provider))
            .collect();

        if discovered.is_empty() {
            writeln!(out, "No matching installed AI CLI agents found.")?;
            return Ok(());
        }

        let mode = if move_auth {
            MigrationMode::MoveAuth
        } else {
            MigrationMode::Copy
        };

        let mut imported_any = false;
        for agent in &discovered {
            let mut account = AccountConfig::new(
                agent.default_account_id.clone(),
                agent.provider,
                format!("{} (imported)", agent.provider),
            );
            account.priority = priority;

            let adapter = AdapterRegistry.adapter_for(agent.provider);
            let profile_env = adapter.build_environment(&account, &app.paths.profiles_dir);

            if dry_run {
                writeln!(
                    out,
                    "[dry-run] {} -> account '{}' (binary: {}, {} credential/config file(s) found)",
                    agent.provider,
                    agent.default_account_id,
                    agent.binary_path.display(),
                    agent.existing_file_count()
                )?;
                writeln!(out, "  update-safety: {}", agent.update_safety_note)?;
                if agent.requires_relogin {
                    writeln!(
                        out,
                        "  note: run `tayassu login {}` after import",
                        agent.default_account_id
                    )?;
                }
                continue;
            }

            let summary = agent
                .apply(&profile_env, &app.paths.home_dir, mode)
                .with_context(|| format!("failed to import {}", agent.provider))?;

            app.config.upsert_account(account);
            imported_any = true;

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
                && match transfer_opencode_auth(&agent.binary_path, &profile_env.env_vars) {
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

        if imported_any {
            app.save_config()?;
        }

        Ok(())
    }
}

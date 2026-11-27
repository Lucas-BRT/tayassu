use crate::app::{App, Execute};
use anyhow::{Context, Result};
use clap::Args;
use std::env;
use std::io::Write;
use std::process::Command as ProcessCommand;
use tayassu_core::ProviderKind;
use tayassu_providers::{AdapterRegistry, HostDiscovery};
use tracing::warn;

/// Run each installed CLI's native self-updater using the real host `$HOME`.
#[derive(Debug, Args)]
pub struct UpgradeClis {
    /// Restrict updates to a single provider.
    provider: Option<ProviderKind>,
    /// Print updater commands without executing them.
    #[arg(long)]
    dry_run: bool,
}

impl Execute for UpgradeClis {
    fn execute(self, app: &mut App, out: &mut dyn Write) -> Result<()> {
        let (provider_filter, dry_run) = (self.provider, self.dry_run);
        let discovered: Vec<_> = HostDiscovery
            .discover_all(&app.paths.home_dir, env::var_os("PATH").as_deref())
            .into_iter()
            .filter(|agent| provider_filter.is_none_or(|p| agent.provider == p))
            .collect();

        if discovered.is_empty() {
            writeln!(out, "No installed AI CLI agents found to update.")?;
            return Ok(());
        }

        for agent in &discovered {
            let adapter = AdapterRegistry.adapter_for(agent.provider);
            let Some(args) = adapter.update_args() else {
                writeln!(
                    out,
                    "{}: no built-in self-updater (update via your package manager)",
                    agent.provider
                )?;
                continue;
            };

            if dry_run {
                writeln!(
                    out,
                    "[dry-run] {} -> {} {}",
                    agent.provider,
                    agent.binary_path.display(),
                    args.join(" ")
                )?;
                continue;
            }

            writeln!(
                out,
                "Updating {} via {} {}...",
                agent.provider,
                agent.binary_path.display(),
                args.join(" ")
            )?;
            out.flush()?;

            let status = ProcessCommand::new(&agent.binary_path)
                .args(&args)
                .env("HOME", &app.paths.home_dir)
                .status()
                .with_context(|| format!("failed to run updater for {}", agent.provider))?;

            if !status.success() {
                warn!(provider = %agent.provider, code = ?status.code(), "updater returned non-zero");
                writeln!(
                    out,
                    "{}: updater exited with status {:?}",
                    agent.provider,
                    status.code()
                )?;
            }
        }
        Ok(())
    }
}

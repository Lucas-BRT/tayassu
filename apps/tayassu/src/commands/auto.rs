use crate::app::{App, Execute};
use crate::probing::probe_accounts;
use crate::routing::RunRequest;
use crate::shell::chain_candidates;
use crate::systemd::{
    DEFAULT_PROBE_INTERVAL_MINUTES, SYSTEMD_SERVICE_NAME, SYSTEMD_TIMER_NAME, SystemdUnits,
};
use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use clap_complete::ArgValueCandidates;
use std::env;
use std::io::Write;
use std::path::Path;
use std::process::Command as ProcessCommand;
use tayassu_core::{FallbackTarget, ModelBucket};
use tayassu_store::ProjectConfig;

/// Automatic routing and background probing.
#[derive(Debug, Args)]
pub struct Auto {
    #[command(subcommand)]
    action: AutoAction,
}

#[derive(Debug, Subcommand)]
enum AutoAction {
    /// Select a configured fallback target with confirmed quota and launch it.
    Run {
        /// Override the configured fallback chain.
        #[arg(long, add = ArgValueCandidates::new(chain_candidates))]
        chain: Option<String>,
        /// Show the selected target using the last recorded quota.
        #[arg(long)]
        dry_run: bool,
        /// Arguments forwarded to the selected agent.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Enable periodic probing and refresh quotas now.
    Probe,
}

impl Auto {
    fn resolve_chain(app: &mut App, chain: Option<String>) -> Result<Option<String>> {
        let project_chain = ProjectConfig::discover(&env::current_dir()?)?
            .and_then(|(_, config)| config.fallback_chain);
        let chain = chain.or(project_chain).or_else(|| {
            app.config
                .fallback_chains
                .contains_key("auto")
                .then(|| "auto".to_owned())
        });
        Ok(chain.or_else(|| {
            let mut accounts: Vec<_> = app
                .config
                .accounts
                .iter()
                .filter(|account| account.enabled)
                .collect();
            accounts.sort_by_key(|account| account.priority);
            let mut targets = Vec::new();
            for account in accounts {
                if !targets
                    .iter()
                    .any(|target: &FallbackTarget| target.provider == account.provider)
                {
                    targets.push(FallbackTarget::new(
                        account.provider,
                        ModelBucket::default_bucket(),
                    ));
                }
            }
            if targets.is_empty() {
                return None;
            }
            let name = "__automatic".to_owned();
            app.config.fallback_chains.insert(name.clone(), targets);
            Some(name)
        }))
    }

    fn probe(app: &mut App, out: &mut dyn Write) -> Result<()> {
        let targets = app.config.accounts.clone();
        if targets.is_empty() {
            writeln!(out, "No accounts configured to probe.")?;
        } else {
            probe_accounts(
                &targets,
                &app.config.accounts,
                &app.paths,
                &mut app.state,
                false,
                Some(&mut *out),
            )?;
        }
        let enabled = ProcessCommand::new("systemctl")
            .args(["--user", "is-enabled", "--quiet", SYSTEMD_TIMER_NAME])
            .status()
            .context("failed to check systemd user timer")?
            .success();
        if enabled {
            anyhow::ensure!(
                ProcessCommand::new("systemctl")
                    .args(["--user", "start", SYSTEMD_TIMER_NAME])
                    .status()
                    .context("failed to start timer")?
                    .success(),
                "failed to start background probe timer"
            );
            writeln!(out, "Background probe timer already enabled.")?;
        } else {
            Self::install_timer(app)?;
            writeln!(out, "Background probe timer enabled and started.")?;
        }
        Ok(())
    }

    fn install_timer(app: &App) -> Result<()> {
        let config_base = app
            .paths
            .config_file
            .parent()
            .and_then(Path::parent)
            .context("failed to resolve XDG config root")?;
        let dir = config_base.join("systemd/user");
        let service = dir.join(SYSTEMD_SERVICE_NAME);
        let timer = dir.join(SYSTEMD_TIMER_NAME);
        if !service.exists() && !timer.exists() {
            SystemdUnits::new(
                env::current_exe().context("failed to resolve current binary")?,
                DEFAULT_PROBE_INTERVAL_MINUTES,
            )
            .install_to(&dir)?;
        } else {
            anyhow::ensure!(
                service.exists() && timer.exists(),
                "partial systemd probe units exist in {}; remove them or run `tayassu setup`",
                dir.display()
            );
        }
        anyhow::ensure!(
            ProcessCommand::new("systemctl")
                .args(["--user", "daemon-reload"])
                .status()
                .context("failed to run systemctl daemon-reload")?
                .success(),
            "could not reload systemd user units; run `tayassu probe` for a manual refresh"
        );
        anyhow::ensure!(
            ProcessCommand::new("systemctl")
                .args(["--user", "enable", "--now", SYSTEMD_TIMER_NAME])
                .status()
                .context("failed to run systemctl enable --now")?
                .success(),
            "could not enable and start the background probe timer; check `systemctl --user status {SYSTEMD_TIMER_NAME}`"
        );
        Ok(())
    }
}

impl Execute for Auto {
    fn execute(self, app: &mut App, mut out: &mut dyn Write) -> Result<()> {
        match self.action {
            AutoAction::Run {
                chain,
                dry_run,
                args,
            } => {
                let chain = Self::resolve_chain(app, chain)?;
                app.run_agent(
                    &RunRequest {
                        account: None,
                        provider: None,
                        chain,
                        model: None,
                        policy: None,
                        dry_run,
                        require_quota: true,
                        failover: false,
                        args,
                    },
                    &mut out,
                )
            }
            AutoAction::Probe => Self::probe(app, out),
        }
    }
}

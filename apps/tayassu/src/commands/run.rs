use crate::app::{App, Execute};
use crate::completion;
use crate::session::SessionGuard;
use crate::spawn::{SIGNAL_FALLBACK_EXIT_CODE, spawn_capturing_stderr};
use anyhow::{Context, Result};
use chrono::Utc;
use clap::Args;
use clap_complete::engine::ArgValueCandidates;
use std::env;
use std::io::{self, Write};
use std::path::Path;
use std::process;
use tayassu_core::{
    AccountConfig, AccountId, AccountRouter, ModelBucket, ProviderKind, SelectionPolicy,
};
use tayassu_providers::AdapterRegistry;
use tayassu_store::{ProjectConfig, RuntimeState};
use tracing::{info, warn};

/// Maximum attempts allowed when failover is enabled (primary + 1 fallback account).
const MAX_FAILOVER_ATTEMPTS: usize = 2;

/// CLI flags indicating the user is resuming an existing conversation in the current directory.
const RESUME_FLAGS: &[&str] = &["-c", "--continue", "-r", "--resume", "--conversation"];

/// How to pick the account: explicit id, fallback chain, or provider routing.
#[derive(Debug, Args)]
pub struct Target {
    /// Provider to use (`agy`, `claude`, `codex`, `copilot`, `opencode`, `custom`).
    /// Optional when `--account` or `--chain` (or `.tayassu.toml` `fallback_chain`) is used.
    #[arg(add = ArgValueCandidates::new(completion::configured_providers))]
    provider: Option<ProviderKind>,
    /// Force a specific account id instead of routing automatically.
    #[arg(long, add = ArgValueCandidates::new(completion::account_ids))]
    account: Option<AccountId>,
    /// Use a named cross-provider fallback chain from `config.toml`.
    #[arg(long, add = ArgValueCandidates::new(completion::chain_names))]
    chain: Option<String>,
    /// Model bucket to check cooldowns against (defaults to `default`).
    #[arg(long)]
    model: Option<ModelBucket>,
    /// Override the configured account selection policy.
    #[arg(long, add = ArgValueCandidates::new(completion::policies))]
    policy: Option<SelectionPolicy>,
}

impl Target {
    fn into_request(self, dry_run: bool, failover: bool, args: Vec<String>) -> RunRequest {
        RunRequest {
            target: self,
            dry_run,
            failover,
            args,
        }
    }
}

/// Select the best account for a provider (or chain) and invoke its CLI, failing over on rate limits.
#[derive(Debug, Args)]
pub struct Run {
    #[command(flatten)]
    target: Target,
    /// Print the chosen account and isolated environment without spawning the CLI.
    #[arg(long = "dry-run")]
    dry: bool,
    /// Do not retry on the next available account when the first hits a rate limit.
    #[arg(long)]
    no_failover: bool,
    /// Extra arguments passed verbatim to the underlying agent CLI.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

impl Execute for Run {
    fn execute(self, app: &mut App, out: &mut dyn Write) -> Result<()> {
        self.target
            .into_request(self.dry, !self.no_failover, self.args)
            .launch(app, out)
    }
}

/// Show which account would be used right now, with its executable and isolated environment.
#[derive(Debug, Args)]
pub struct Where {
    #[command(flatten)]
    target: Target,
}

impl Execute for Where {
    fn execute(self, app: &mut App, out: &mut dyn Write) -> Result<()> {
        self.target
            .into_request(true, false, Vec::new())
            .launch(app, out)
    }
}

/// A fully resolved request to run an agent CLI.
struct RunRequest {
    target: Target,
    dry_run: bool,
    failover: bool,
    args: Vec<String>,
}

impl RunRequest {
    fn is_resume(&self) -> bool {
        self.args.iter().any(|arg| {
            RESUME_FLAGS
                .iter()
                .any(|flag| arg == flag || arg.starts_with(&format!("{flag}=")))
        })
    }

    fn launch(&self, app: &mut App, out: &mut dyn Write) -> Result<()> {
        let cwd = env::current_dir().context("failed to resolve current directory")?;
        let project_cfg = ProjectConfig::discover(&cwd)
            .context("failed to load .tayassu.toml")?
            .map(|(_, cfg)| cfg);

        let effective_policy = self
            .target
            .policy
            .or_else(|| project_cfg.as_ref().and_then(|cfg| cfg.default_policy))
            .unwrap_or(app.config.default_policy);
        let router = AccountRouter::new(effective_policy);
        let max_attempts = if self.failover {
            MAX_FAILOVER_ATTEMPTS
        } else {
            1
        };

        app.prune_stale_sessions()?;

        for attempt in 1..=max_attempts {
            let (account, bucket) = self.resolve_target(app, project_cfg.as_ref(), &cwd, router)?;

            let adapter = AdapterRegistry.adapter_for(account.provider);
            let env_profile = adapter.build_environment(&account, &app.paths.profiles_dir);
            env_profile
                .prepare(&app.paths.home_dir)
                .with_context(|| format!("failed to prepare profile for {}", account.id))?;

            if self.dry_run {
                writeln!(out, "account: {}", account.id)?;
                writeln!(out, "executable: {}", account.executable())?;
                for (key, value) in &env_profile.env_vars {
                    writeln!(out, "env: {key}={value}")?;
                }
                if !self.args.is_empty() {
                    writeln!(out, "args: {}", self.args.join(" "))?;
                }
                return Ok(());
            }

            let (updated_state, ()) = RuntimeState::update_locked(&app.paths.state_file, |state| {
                state.record_cwd_account(&cwd, account.provider, account.id.clone());
            })
            .with_context(|| format!("failed to save {}", app.paths.state_file.display()))?;
            app.state = updated_state;

            let session_guard =
                SessionGuard::acquire(app.paths.state_file.clone(), account.id.clone())?;

            info!(account = %account.id, executable = account.executable(), "launching agent CLI");
            let outcome = spawn_capturing_stderr(&account, &env_profile.env_vars, &self.args)?;

            // A clean exit never signals a limit; stderr noise must not cool the account down.
            let rate_limit_hit = if outcome.status.success() {
                None
            } else {
                adapter.detect_rate_limit(&account, &bucket, &outcome.captured_stderr, Utc::now())
            };

            if let Some(cooldown) = rate_limit_hit {
                let resets_at = cooldown.resets_at;
                let account_id = account.id.clone();
                let (updated, ()) = RuntimeState::update_locked(&app.paths.state_file, |state| {
                    state.status_for_mut(&account_id).set_cooldown(cooldown);
                })
                .with_context(|| format!("failed to save {}", app.paths.state_file.display()))?;
                app.state = updated;

                warn!(
                    account = %account.id,
                    bucket = %bucket,
                    resets_at = %resets_at.to_rfc3339(),
                    "rate limit detected; cooldown recorded"
                );
                let _ = writeln!(
                    io::stderr(),
                    "tayassu: rate limit detected on '{}' (bucket '{}'); cooldown set until {}",
                    account.id,
                    bucket,
                    resets_at.to_rfc3339()
                );

                if self.failover && attempt < max_attempts && !outcome.status.success() {
                    let _ = writeln!(
                        io::stderr(),
                        "tayassu: failing over to next available account..."
                    );
                    continue;
                }
            }

            if !outcome.status.success() {
                let code = outcome.status.code().unwrap_or(SIGNAL_FALLBACK_EXIT_CODE);
                // Release the session counter first: `exit` skips destructors.
                drop(session_guard);
                // Propagate the agent's own status so shell wrappers and scripts see it.
                process::exit(code);
            }
            return Ok(());
        }

        Ok(())
    }

    fn resolve_target(
        &self,
        app: &App,
        project_cfg: Option<&ProjectConfig>,
        cwd: &Path,
        router: AccountRouter,
    ) -> Result<(AccountConfig, ModelBucket)> {
        let now = Utc::now();

        if let Some(ref forced_id) = self.target.account {
            let account = app
                .config
                .find_account(forced_id)
                .with_context(|| format!("account '{forced_id}' not found"))?
                .clone();
            return Ok((account, self.target.model.clone().unwrap_or_default()));
        }

        let chain_name = self.target.chain.as_deref().or_else(|| {
            if self.target.provider.is_none() {
                project_cfg.and_then(|cfg| cfg.fallback_chain.as_deref())
            } else {
                None
            }
        });

        if let Some(name) = chain_name {
            let chain = app
                .config
                .fallback_chains
                .get(name)
                .with_context(|| format!("fallback chain '{name}' not found in config.toml"))?;
            let (account, bucket) = router
                .select_from_chain(&app.config.accounts, &app.state.statuses, chain, now)
                .with_context(|| format!("no available account in fallback chain '{name}'"))?;
            return Ok((account.clone(), bucket));
        }

        let provider = self
            .target
            .provider
            .context("specify a provider, `--account <id>`, or `--chain <name>`")?;
        let bucket = self.target.model.clone().unwrap_or_default();

        if self.is_resume()
            && let Some(last_id) = app.state.last_cwd_account(cwd, provider)
            && let Some(account) = app.config.find_account(last_id)
        {
            if let Some(status) = app.state.status_for(last_id)
                && !status.is_available(&bucket, now)
            {
                warn!(
                    account = %last_id,
                    "resuming conversation on account currently in cooldown"
                );
            }
            return Ok((account.clone(), bucket));
        }

        if let Some(preferred_id) = project_cfg.and_then(|cfg| cfg.preferred_account_for(provider))
            && let Some(account) = app.config.find_account(preferred_id)
            && account.enabled
            && account.provider == provider
            && app
                .state
                .status_for(preferred_id)
                .is_none_or(|st| st.is_available(&bucket, now))
        {
            return Ok((account.clone(), bucket));
        }

        let account = router
            .select(
                &app.config.accounts,
                &app.state.statuses,
                provider,
                &bucket,
                now,
            )
            .context("cannot select an account for this run")?
            .clone();
        Ok((account, bucket))
    }
}

use crate::app::{App, Execute};
use crate::routing::RunRequest;
use crate::shell::{account_candidates, chain_candidates, runnable_provider_candidates};
use anyhow::Result;
use clap::Args;
use clap_complete::ArgValueCandidates;
use std::io::Write;
use tayassu_core::{AccountId, ModelBucket, ProviderKind, SelectionPolicy};

/// How to pick the account: explicit id, fallback chain, or provider routing.
#[derive(Debug, Args)]
pub struct Target {
    /// Provider to use (`agy`, `claude`, `codex`, `copilot`, `opencode`, `custom`).
    /// Optional when `--account` or `--chain` (or `.tayassu.toml` `fallback_chain`) is used.
    #[arg(add = ArgValueCandidates::new(runnable_provider_candidates))]
    provider: Option<ProviderKind>,
    /// Force a specific account id instead of routing automatically.
    #[arg(long, add = ArgValueCandidates::new(account_candidates))]
    account: Option<AccountId>,
    /// Use a named cross-provider fallback chain from `config.toml`.
    #[arg(long, add = ArgValueCandidates::new(chain_candidates))]
    chain: Option<String>,
    /// Model bucket to check cooldowns against (defaults to `default`).
    #[arg(long)]
    model: Option<ModelBucket>,
    /// Override the configured account selection policy.
    #[arg(long)]
    policy: Option<SelectionPolicy>,
}

/// Select the best account for a provider (or chain) and invoke its CLI, failing over on rate limits.
#[derive(Debug, Args)]
pub struct Run {
    #[command(flatten)]
    target: Target,
    /// Print the chosen account and isolated environment without spawning the CLI.
    #[arg(long = "dry-run")]
    dry: bool,
    /// Refresh usage before launch and select only an account with measured quota remaining.
    #[arg(long, conflicts_with = "account")]
    require_quota: bool,
    /// Do not retry on the next available account when the first hits a rate limit.
    #[arg(long, conflicts_with = "account")]
    no_failover: bool,
    /// Extra arguments passed verbatim to the underlying agent CLI.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

impl Execute for Run {
    fn execute(self, app: &mut App, mut out: &mut dyn Write) -> Result<()> {
        app.run_agent(
            &RunRequest {
                account: self.target.account,
                provider: self.target.provider,
                chain: self.target.chain,
                model: self.target.model,
                policy: self.target.policy,
                dry_run: self.dry,
                require_quota: self.require_quota,
                failover: !self.no_failover,
                args: self.args,
            },
            &mut out,
        )
    }
}

/// Show which account would be used right now, with its executable and isolated environment.
#[derive(Debug, Args)]
pub struct Where {
    #[command(flatten)]
    target: Target,
}

impl Execute for Where {
    fn execute(self, app: &mut App, mut out: &mut dyn Write) -> Result<()> {
        app.run_agent(
            &RunRequest {
                account: self.target.account,
                provider: self.target.provider,
                chain: self.target.chain,
                model: self.target.model,
                policy: self.target.policy,
                dry_run: true,
                require_quota: false,
                failover: false,
                args: Vec::new(),
            },
            &mut out,
        )
    }
}

use crate::app::{App, Execute};
use crate::routing::RunRequest;
use crate::shell::{
    chain_candidates, model_candidates, run_account_candidates, run_target_candidates,
};
use anyhow::{Result, bail};
use clap::Args;
use clap_complete::ArgValueCandidates;
use std::io::Write;
use tayassu_core::{AccountId, ModelBucket, ProviderKind, SelectionPolicy};

/// How to pick the account: explicit id, fallback chain, or provider routing.
#[derive(Debug, Args)]
pub struct Target {
    /// Provider (`agy`, `claude`, `codex`, `copilot`, `opencode`, `custom`) or account id.
    /// Optional when `--account` or `--chain` (or `.tayassu.toml` `fallback_chain`) is used.
    #[arg(
        value_name = "PROVIDER|ACCOUNT",
        add = ArgValueCandidates::new(run_target_candidates)
    )]
    provider_or_account: Option<String>,
    /// Account id for the selected provider (shorthand for `--account <id>`).
    #[arg(
        value_name = "ACCOUNT",
        conflicts_with = "account",
        add = ArgValueCandidates::new(run_account_candidates)
    )]
    account_arg: Option<AccountId>,
    /// Force a specific account id instead of routing automatically.
    #[arg(long, add = ArgValueCandidates::new(run_account_candidates))]
    account: Option<AccountId>,
    /// Use a named cross-provider fallback chain from `config.toml`.
    #[arg(long, add = ArgValueCandidates::new(chain_candidates))]
    chain: Option<String>,
    /// Model bucket to check cooldowns against (defaults to `default`).
    #[arg(long, add = ArgValueCandidates::new(model_candidates))]
    model: Option<ModelBucket>,
    /// Override the configured account selection policy.
    #[arg(long)]
    policy: Option<SelectionPolicy>,
}

impl Target {
    fn resolve(self, app: &App) -> Result<(Option<ProviderKind>, Option<AccountId>)> {
        let Some(raw) = self.provider_or_account else {
            return Ok((None, self.account.or(self.account_arg)));
        };
        if let Ok(provider) = raw.parse::<ProviderKind>() {
            return Ok((Some(provider), self.account.or(self.account_arg)));
        }
        if let Some(extra) = self.account_arg {
            bail!(
                "'{raw}' is not a provider; unexpected extra argument '{extra}' (pass CLI arguments after `--`)"
            );
        }
        if self.account.is_some() {
            bail!("'{raw}' is not a provider; pass a provider name or omit it with `--account`");
        }
        let account_id: AccountId = raw
            .parse()
            .map_err(|_| anyhow::anyhow!("unknown provider or account '{raw}'"))?;
        if app.config.find_account(&account_id).is_none() {
            bail!("unknown provider or account '{raw}'");
        }
        Ok((None, Some(account_id)))
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
    /// Refresh usage before launch and select only an account with measured quota remaining.
    #[arg(long, conflicts_with_all = ["account", "account_arg"])]
    require_quota: bool,
    /// Do not retry on the next available account when the first hits a rate limit.
    #[arg(long, conflicts_with_all = ["account", "account_arg"])]
    no_failover: bool,
    /// Extra arguments passed verbatim to the underlying agent CLI (after `--`).
    #[arg(last = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

impl Execute for Run {
    fn execute(self, app: &mut App, mut out: &mut dyn Write) -> Result<()> {
        let chain = self.target.chain.clone();
        let model = self.target.model.clone();
        let policy = self.target.policy;
        let (provider, account) = self.target.resolve(app)?;
        if account.is_some() && (self.require_quota || self.no_failover) {
            bail!("`--require-quota` and `--no-failover` cannot be used with an explicit account");
        }
        app.run_agent(
            &RunRequest {
                account,
                provider,
                chain,
                model,
                policy,
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
        let chain = self.target.chain.clone();
        let model = self.target.model.clone();
        let policy = self.target.policy;
        let (provider, account) = self.target.resolve(app)?;
        app.run_agent(
            &RunRequest {
                account,
                provider,
                chain,
                model,
                policy,
                dry_run: true,
                require_quota: false,
                failover: false,
                args: Vec::new(),
            },
            &mut out,
        )
    }
}

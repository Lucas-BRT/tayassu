use crate::shell::ShellKind;
use crate::shell::{account_candidates, chain_candidates, provider_candidates};
use crate::systemd::DEFAULT_PROBE_INTERVAL_MINUTES;
use clap::{Args, Parser, Subcommand};
use clap_complete::ArgValueCandidates;
use tayassu_core::{
    AccountId, DEFAULT_ACCOUNT_PRIORITY, ModelBucket, ProviderKind, SelectionPolicy,
};

/// Default manual cooldown length in minutes (5 hours).
pub const DEFAULT_MANUAL_COOLDOWN_MINUTES: u64 = 300;

/// Multi-account session and cooldown manager for subscription-authenticated AI CLIs.
#[derive(Debug, Parser)]
#[command(name = "tayassu", version, about)]
pub struct Cli {
    /// Subcommand to execute.
    #[command(subcommand)]
    pub command: Command,
}

/// Top-level CLI subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Display a compact status table for all configured accounts and cooldowns.
    Status {
        /// Filter output to a single provider.
        #[arg(long)]
        #[arg(add = ArgValueCandidates::new(provider_candidates))]
        provider: Option<ProviderKind>,
        /// Model bucket to evaluate availability against.
        #[arg(long)]
        model: Option<ModelBucket>,
        /// List discovered `OpenCode` models and their measured quota.
        #[arg(long, conflicts_with = "model")]
        list_models: bool,
    },

    /// Automatic routing and background probing.
    Auto {
        /// Automatic operation.
        #[command(subcommand)]
        action: AutoAction,
    },

    /// Open an interactive live terminal dashboard for accounts, quotas, and cooldowns.
    Dash,

    /// Select an available account for a provider (or fallback chain) and invoke its CLI.
    Run {
        /// Provider to invoke (`agy`, `claude`, `codex`, `copilot`, `opencode`, `custom`).
        /// Optional when `--account` or `--chain` (or `.tayassu.toml` `fallback_chain`) is used.
        #[arg(add = ArgValueCandidates::new(provider_candidates))]
        provider: Option<ProviderKind>,
        /// Force a specific account id instead of routing automatically.
        #[arg(long)]
        #[arg(add = ArgValueCandidates::new(account_candidates))]
        account: Option<AccountId>,
        /// Use a named cross-provider fallback chain from `config.toml`.
        #[arg(long)]
        #[arg(add = ArgValueCandidates::new(chain_candidates))]
        chain: Option<String>,
        /// Model bucket to check cooldowns against (defaults to `default`).
        #[arg(long)]
        model: Option<ModelBucket>,
        /// Override the configured account selection policy.
        #[arg(long)]
        policy: Option<SelectionPolicy>,
        /// Print the chosen account and isolated environment without spawning the CLI.
        #[arg(long)]
        dry_run: bool,
        /// Refresh usage before launch and select only an account with measured quota remaining.
        #[arg(long, conflicts_with = "account")]
        require_quota: bool,
        /// Retry once on the next available account if the first run hits a rate limit.
        #[arg(long, conflicts_with = "account")]
        failover: bool,
        /// Extra arguments passed verbatim to the underlying agent CLI.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Manage configured accounts and their isolated profile directories.
    Account {
        /// Account subcommand.
        #[command(subcommand)]
        action: AccountAction,
    },

    /// Record or clear cooldowns manually.
    Cooldown {
        /// Cooldown subcommand.
        #[command(subcommand)]
        action: CooldownAction,
    },

    /// Probe usage/cooldown state across configured accounts.
    Probe {
        /// Limit probing to a single account id.
        #[arg(long)]
        #[arg(add = ArgValueCandidates::new(account_candidates))]
        id: Option<AccountId>,
        /// Send a desktop notification (`notify-send`) when a previously active cooldown clears.
        #[arg(long)]
        notify: bool,
    },

    /// Auto-discover installed AI CLIs and safely import their host credentials into isolated profiles.
    #[command(alias = "discover")]
    Import(ImportArgs),

    /// Print shell wrapper functions so `agy`, `claude`, `codex`, etc. route through `tayassu`.
    ShellInit {
        /// Target shell (`bash`, `zsh`, `fish`).
        shell: ShellKind,
        /// Restrict wrappers to specific provider(s) (repeatable). Defaults to all built-in agents.
        #[arg(long = "provider")]
        #[arg(add = ArgValueCandidates::new(provider_candidates))]
        providers: Vec<ProviderKind>,
        /// Include `--failover` in generated wrapper functions.
        #[arg(long)]
        failover: bool,
    },

    /// Generate or install `systemd --user` service and timer units for background probing.
    SystemdUnit {
        /// Background probe interval in minutes.
        #[arg(long, default_value_t = DEFAULT_PROBE_INTERVAL_MINUTES)]
        interval_minutes: u64,
        /// Write units into `$XDG_CONFIG_HOME/systemd/user/`.
        #[arg(long)]
        install: bool,
    },

    /// Validate installed binaries, fake-HOME symlinks, and credential file permissions.
    Doctor {
        /// Exit non-zero on warnings as well as errors.
        #[arg(long)]
        strict: bool,
    },

    /// Run each installed CLI's native self-updater using the real host `$HOME`.
    Update {
        /// Restrict updates to a single provider.
        #[arg(long)]
        #[arg(add = ArgValueCandidates::new(provider_candidates))]
        provider: Option<ProviderKind>,
        /// Print updater commands without executing them.
        #[arg(long)]
        dry_run: bool,
    },
}

/// Automatic operations.
#[derive(Debug, Subcommand)]
pub enum AutoAction {
    /// Select a configured fallback target with confirmed quota and launch it.
    Run {
        /// Override the configured fallback chain.
        #[arg(long)]
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

/// Options for discovering and importing host accounts.
#[derive(Debug, Args)]
pub struct ImportArgs {
    /// Include only specific provider(s) (repeatable). Defaults to all discovered providers.
    #[arg(long = "provider")]
    #[arg(add = ArgValueCandidates::new(provider_candidates))]
    pub include: Vec<ProviderKind>,
    /// Exclude specific provider(s) from discovery/import (repeatable).
    #[arg(long = "exclude")]
    #[arg(add = ArgValueCandidates::new(provider_candidates))]
    pub exclude: Vec<ProviderKind>,
    /// Preview discovered agents, files, and update-safety notes without modifying disk.
    #[arg(long)]
    pub dry_run: bool,
    /// Refresh credentials in profiles that are already registered.
    #[arg(long)]
    pub refresh: bool,
    /// Remove original host auth files after verified copy (never removes binaries or updater dirs).
    #[arg(long)]
    pub move_auth: bool,
    /// Selection priority assigned to imported accounts.
    #[arg(long, default_value_t = DEFAULT_ACCOUNT_PRIORITY)]
    pub priority: u8,
}

/// Account management subcommands.
#[derive(Debug, Subcommand)]
pub enum AccountAction {
    /// Register a new account and prepare its isolated profile directory.
    Add {
        /// Unique account slug (e.g. `agy-student`, `agy-pro`).
        id: AccountId,
        /// Provider kind (`agy`, `claude`, `codex`, `copilot`, `opencode`, `custom`).
        #[arg(add = ArgValueCandidates::new(provider_candidates))]
        provider: ProviderKind,
        /// Human-readable label shown in status output.
        #[arg(long, default_value = "")]
        label: String,
        /// Selection priority (lower value is preferred first).
        #[arg(long, default_value_t = DEFAULT_ACCOUNT_PRIORITY)]
        priority: u8,
        /// Optional custom binary path or executable name.
        #[arg(long)]
        binary: Option<String>,
    },

    /// Edit an account's label or selection priority.
    Edit {
        /// Account identifier.
        #[arg(add = ArgValueCandidates::new(account_candidates))]
        id: AccountId,
        /// New display label.
        #[arg(long, required_unless_present = "priority")]
        label: Option<String>,
        /// New selection priority (lower values are preferred).
        #[arg(long)]
        priority: Option<u8>,
    },

    /// Include an account in automatic routing.
    Enable {
        /// Account identifier.
        #[arg(add = ArgValueCandidates::new(account_candidates))]
        id: AccountId,
    },

    /// Pause automatic routing to an account while keeping its profile.
    Disable {
        /// Account identifier.
        #[arg(add = ArgValueCandidates::new(account_candidates))]
        id: AccountId,
    },

    /// Remove an account from `config.toml` (leaves profile data on disk untouched).
    Remove {
        /// Account identifier to remove.
        #[arg(add = ArgValueCandidates::new(account_candidates))]
        id: AccountId,
    },

    /// Print the isolated profile directory and environment overrides for an account.
    Env {
        /// Account identifier to inspect.
        #[arg(add = ArgValueCandidates::new(account_candidates))]
        id: AccountId,
    },

    /// Launch the provider CLI interactively inside the account's isolated profile to log in.
    Login {
        /// Account identifier to authenticate.
        #[arg(add = ArgValueCandidates::new(account_candidates))]
        id: AccountId,
    },
}

/// Manual cooldown management subcommands.
#[derive(Debug, Subcommand)]
pub enum CooldownAction {
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

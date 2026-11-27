use crate::completion;
use crate::shell::ShellKind;
use crate::systemd::DEFAULT_PROBE_INTERVAL_MINUTES;
use clap::{Parser, Subcommand};
use clap_complete::engine::ArgValueCandidates;
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
        #[arg(long, add = ArgValueCandidates::new(completion::configured_providers))]
        provider: Option<ProviderKind>,
        /// Model bucket to evaluate availability against.
        #[arg(long)]
        model: Option<ModelBucket>,
    },

    /// Open an interactive live terminal dashboard for accounts, quotas, and cooldowns.
    Dash,

    /// Select an available account for a provider (or fallback chain) and invoke its CLI.
    Run {
        /// Provider to invoke (`agy`, `claude`, `codex`, `copilot`, `opencode`, `custom`).
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
        /// Print the chosen account and isolated environment without spawning the CLI.
        #[arg(long)]
        dry_run: bool,
        /// Retry once on the next available account if the first run hits a rate limit.
        #[arg(long)]
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
        #[arg(long, add = ArgValueCandidates::new(completion::account_ids))]
        id: Option<AccountId>,
        /// Send a desktop notification (`notify-send`) when a previously active cooldown clears.
        #[arg(long)]
        notify: bool,
    },

    /// Auto-discover installed AI CLIs and safely import their host credentials into isolated profiles.
    #[command(alias = "discover")]
    Import {
        /// Include only specific provider(s) (repeatable). Defaults to all discovered providers.
        #[arg(long = "provider")]
        include: Vec<ProviderKind>,
        /// Exclude specific provider(s) from discovery/import (repeatable).
        #[arg(long = "exclude")]
        exclude: Vec<ProviderKind>,
        /// Preview discovered agents, files, and update-safety notes without modifying disk.
        #[arg(long)]
        dry_run: bool,
        /// Remove original host auth files after verified copy (never removes binaries or updater dirs).
        #[arg(long)]
        move_auth: bool,
        /// Selection priority assigned to imported accounts.
        #[arg(long, default_value_t = DEFAULT_ACCOUNT_PRIORITY)]
        priority: u8,
    },

    /// Print shell wrapper functions so `agy`, `claude`, `codex`, etc. route through `tayassu`.
    ShellInit {
        /// Target shell (`bash`, `zsh`, `fish`).
        shell: ShellKind,
        /// Restrict wrappers to specific provider(s) (repeatable). Defaults to all built-in agents.
        #[arg(long = "provider")]
        providers: Vec<ProviderKind>,
        /// Include `--failover` in generated wrapper functions.
        #[arg(long)]
        failover: bool,
    },

    /// Configure tab completion for bash, zsh and fish (idempotent; run by `install.sh`).
    InstallCompletions,

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
        #[arg(long, add = ArgValueCandidates::new(completion::configured_providers))]
        provider: Option<ProviderKind>,
        /// Print updater commands without executing them.
        #[arg(long)]
        dry_run: bool,
    },
}

/// Account management subcommands.
#[derive(Debug, Subcommand)]
pub enum AccountAction {
    /// Register a new account and prepare its isolated profile directory.
    Add {
        /// Unique account slug (e.g. `agy-student`, `agy-pro`).
        id: AccountId,
        /// Provider kind (`agy`, `claude`, `codex`, `copilot`, `opencode`, `custom`).
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

    /// Remove an account from `config.toml` (leaves profile data on disk untouched).
    Remove {
        /// Account identifier to remove.
        #[arg(add = ArgValueCandidates::new(completion::account_ids))]
        id: AccountId,
    },

    /// Print the isolated profile directory and environment overrides for an account.
    Env {
        /// Account identifier to inspect.
        #[arg(add = ArgValueCandidates::new(completion::account_ids))]
        id: AccountId,
    },

    /// Launch the provider CLI interactively inside the account's isolated profile to log in.
    Login {
        /// Account identifier to authenticate.
        #[arg(add = ArgValueCandidates::new(completion::account_ids))]
        id: AccountId,
    },
}

/// Manual cooldown management subcommands.
#[derive(Debug, Subcommand)]
pub enum CooldownAction {
    /// Mark an account (and optional model bucket) as cooling down.
    Set {
        /// Account identifier.
        #[arg(add = ArgValueCandidates::new(completion::account_ids))]
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
        #[arg(add = ArgValueCandidates::new(completion::account_ids))]
        id: AccountId,
        /// Model bucket to clear (defaults to `default`).
        #[arg(long)]
        model: Option<ModelBucket>,
    },
}

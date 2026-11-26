use clap::{Parser, Subcommand};
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
        provider: Option<ProviderKind>,
        /// Model bucket to evaluate availability against.
        #[arg(long)]
        model: Option<ModelBucket>,
    },

    /// Select an available account for a provider and invoke its CLI in an isolated profile.
    Run {
        /// Provider to invoke (`agy`, `claude`, `codex`, `custom`).
        provider: ProviderKind,
        /// Model bucket to check cooldowns against (defaults to `default`).
        #[arg(long)]
        model: Option<ModelBucket>,
        /// Override the configured account selection policy.
        #[arg(long)]
        policy: Option<SelectionPolicy>,
        /// Print the chosen account and isolated environment without spawning the CLI.
        #[arg(long)]
        dry_run: bool,
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
        id: Option<AccountId>,
    },
}

/// Account management subcommands.
#[derive(Debug, Subcommand)]
pub enum AccountAction {
    /// Register a new account and prepare its isolated profile directory.
    Add {
        /// Unique account slug (e.g. `agy-student`, `agy-pro`).
        id: AccountId,
        /// Provider kind (`agy`, `claude`, `codex`, `custom`).
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
        id: AccountId,
    },

    /// Print the isolated profile directory and environment overrides for an account.
    Env {
        /// Account identifier to inspect.
        id: AccountId,
    },
}

/// Manual cooldown management subcommands.
#[derive(Debug, Subcommand)]
pub enum CooldownAction {
    /// Mark an account (and optional model bucket) as cooling down.
    Set {
        /// Account identifier.
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
        id: AccountId,
        /// Model bucket to clear (defaults to `default`).
        #[arg(long)]
        model: Option<ModelBucket>,
    },
}

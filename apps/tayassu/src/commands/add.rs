use super::login::Login;
use crate::app::{App, Execute};
use anyhow::{Context, Result, bail};
use clap::Args;
use std::io::Write;
use tayassu_core::{AccountConfig, AccountId, DEFAULT_ACCOUNT_PRIORITY, ProviderKind};
use tayassu_providers::AdapterRegistry;

/// Register an account, prepare its isolated profile and log in.
#[derive(Debug, Args)]
pub struct Add {
    /// Provider kind (`agy`, `claude`, `codex`, `copilot`, `opencode`, `custom`).
    provider: ProviderKind,
    /// Account slug; defaults to the provider name, then `<provider>-2`, `-3`, ...
    id: Option<AccountId>,
    /// Human-readable label shown in status output.
    #[arg(long, default_value = "")]
    label: String,
    /// Selection priority (lower value is preferred first).
    #[arg(long, default_value_t = DEFAULT_ACCOUNT_PRIORITY)]
    priority: u8,
    /// Optional custom binary path or executable name.
    #[arg(long)]
    binary: Option<String>,
    /// Register only; skip the interactive login.
    #[arg(long)]
    no_login: bool,
}

impl Execute for Add {
    fn execute(self, app: &mut App, out: &mut dyn Write) -> Result<()> {
        let provider = self.provider;
        let id = self.id.unwrap_or_else(|| app.config.next_free_id(provider));
        if app.config.find_account(&id).is_some() {
            bail!("account '{id}' already exists; run `tayassu rm {id}` first to re-register");
        }
        let label = if self.label.is_empty() {
            format!("{provider} ({id})")
        } else {
            self.label
        };
        let mut account = AccountConfig::new(id.clone(), provider, label);
        account.priority = self.priority;
        account.binary_override = self.binary;

        let adapter = AdapterRegistry.adapter_for(provider);
        let env_profile = adapter.build_environment(&account, &app.paths.profiles_dir);
        env_profile
            .prepare(&app.paths.home_dir)
            .with_context(|| format!("failed to prepare profile for {id}"))?;

        app.config.upsert_account(account.clone());
        app.save_config()?;
        writeln!(
            out,
            "Added account '{id}' ({provider}) at {}",
            env_profile.profile_dir.display()
        )?;
        if self.no_login {
            writeln!(out, "next: run `tayassu login {id}`")?;
            return Ok(());
        }
        Login::run(app, &account, out)
    }
}

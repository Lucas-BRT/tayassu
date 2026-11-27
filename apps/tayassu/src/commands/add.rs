use super::login::Login;
use crate::app::{App, Execute};
use crate::shell::provider_candidates;
use anyhow::{Context, Result, bail};
use clap::Args;
use clap_complete::ArgValueCandidates;
use std::io::Write;
use tayassu_core::{AccountConfig, AccountId, DEFAULT_ACCOUNT_PRIORITY, ProviderKind};
use tayassu_providers::AdapterRegistry;

/// Register an account, prepare its isolated profile and log in.
#[derive(Debug, Args)]
pub struct Add {
    /// Provider kind (`agy`, `claude`, `codex`, `copilot`, `opencode`, `custom`).
    #[arg(add = ArgValueCandidates::new(provider_candidates))]
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
    /// Share the desktop Secret Service instead of an isolated keyring (`agy` on Linux).
    #[arg(long)]
    shared_keyring: bool,
}

impl Execute for Add {
    fn execute(self, app: &mut App, out: &mut dyn Write) -> Result<()> {
        let provider = self.provider;
        let requested_id = self.id;
        let label_arg = self.label;
        let priority = self.priority;
        let binary = self.binary;
        let no_login = self.no_login;
        let shared_keyring = self.shared_keyring;
        let mut added_account = None;

        app.with_current_config(|locked| {
            let id = requested_id.unwrap_or_else(|| locked.config.next_free_id(provider));
            if locked.config.find_account(&id).is_some() {
                bail!("account '{id}' already exists; run `tayassu rm {id}` first to re-register");
            }
            let label = if label_arg.is_empty() {
                format!("{provider} ({id})")
            } else {
                label_arg
            };
            let mut account = AccountConfig::new(id.clone(), provider, label);
            account.priority = priority;
            account.binary_override = binary;
            #[cfg(target_os = "linux")]
            if provider == ProviderKind::Agy && !shared_keyring {
                account.agy_private_keyring = true;
            }
            #[cfg(not(target_os = "linux"))]
            let _ = shared_keyring;

            let adapter = AdapterRegistry.adapter_for(provider);
            let env_profile = adapter.build_environment(&account, &locked.paths.profiles_dir);
            crate::agy_keyring::reset(&account, &env_profile)
                .with_context(|| format!("failed to reset private keyring for {id}"))?;
            adapter
                .clear_auth(&env_profile)
                .with_context(|| format!("failed to clear stale credentials for {id}"))?;
            env_profile
                .prepare(&locked.paths.home_dir)
                .with_context(|| format!("failed to prepare profile for {id}"))?;

            locked.config.upsert_account(account.clone());
            locked.save_config()?;
            writeln!(
                out,
                "Added account '{id}' ({provider}) at {}",
                env_profile.profile_dir.display()
            )?;
            added_account = Some(account);
            Ok(())
        })?;

        if let Some(account) = added_account {
            if no_login {
                writeln!(out, "next: run `tayassu login {}`", account.id)?;
            } else {
                Login::run(app, &account, out)?;
            }
        }
        Ok(())
    }
}

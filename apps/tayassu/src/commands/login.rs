use crate::app::{App, Execute};
use crate::shell::account_candidates;
use crate::spawn::{account_executable, spawn_interactive};
use anyhow::{Context, Result};
use clap::Args;
use clap_complete::ArgValueCandidates;
use std::io::Write;
use tayassu_core::{AccountConfig, AccountId};
use tayassu_providers::AdapterRegistry;

/// Authenticate an account by launching its provider CLI inside the isolated profile.
#[derive(Debug, Args)]
pub struct Login {
    /// Account identifier to authenticate.
    #[arg(add = ArgValueCandidates::new(account_candidates))]
    id: AccountId,
}

impl Login {
    /// Prepares the profile of `account` and runs its interactive login flow.
    pub fn run(app: &App, account: &AccountConfig, out: &mut dyn Write) -> Result<()> {
        let adapter = AdapterRegistry.adapter_for(account.provider);
        let env_profile = adapter.build_environment(account, &app.paths.profiles_dir);
        env_profile
            .prepare(&app.paths.home_dir)
            .with_context(|| format!("failed to prepare profile for {}", account.id))?;
        writeln!(
            out,
            "Logging in '{}' ({}) using isolated profile {}",
            account.id,
            account.provider,
            env_profile.profile_dir.display()
        )?;
        out.flush()?;
        let executable = account_executable(account, &app.paths.home_dir);
        spawn_interactive(
            account,
            &executable,
            &env_profile.env_vars,
            &adapter.login_args(),
        )
    }
}

impl Execute for Login {
    fn execute(self, app: &mut App, out: &mut dyn Write) -> Result<()> {
        let account = app.account(&self.id)?.clone();
        Self::run(app, &account, out)
    }
}

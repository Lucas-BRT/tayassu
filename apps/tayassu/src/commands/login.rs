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
        let env_profile = crate::spawn::prepare_profile(
            account,
            &app.paths,
            crate::agy_keyring::Operation::Login,
        )?;
        writeln!(
            out,
            "Logging in '{}' ({}) using isolated profile {}",
            account.id,
            account.provider,
            env_profile.profile_dir.display()
        )?;
        out.flush()?;
        let executable = account_executable(account, &app.paths.home_dir);
        let args = if account.uses_private_agy_keyring() {
            Vec::new()
        } else {
            adapter.login_args()
        };
        let outcome = spawn_interactive(account, &executable, &env_profile, &args);
        crate::agy_keyring::check_after_launch(account, &env_profile)?;
        outcome?;
        if account.uses_private_agy_keyring() {
            crate::probing::run_single_probe(
                account,
                &app.config.accounts,
                &app.paths,
                chrono::Utc::now(),
            )
            .context("could not confirm Antigravity authentication in a fresh process")?;
            writeln!(
                out,
                "Confirmed Antigravity authentication for '{}'",
                account.id
            )?;
        }
        Ok(())
    }
}

impl Execute for Login {
    fn execute(self, app: &mut App, out: &mut dyn Write) -> Result<()> {
        let account = app.account(&self.id)?.clone();
        Self::run(app, &account, out)
    }
}

use crate::app::{App, Execute};
use crate::shell::account_candidates;
use anyhow::{Context, Result, bail};
use clap::Args;
use clap_complete::ArgValueCandidates;
use std::fs;
use std::io::{ErrorKind, Write};
use tayassu_core::AccountId;
use tayassu_providers::AdapterRegistry;

/// Remove an account from `config.toml` (pass `--purge` to also delete profile data on disk).
#[derive(Debug, Args)]
pub struct Rm {
    /// Account identifier to remove.
    #[arg(add = ArgValueCandidates::new(account_candidates))]
    id: AccountId,
    /// Also delete the isolated profile directory and stop any private keyring daemon.
    #[arg(long)]
    purge: bool,
}

impl Execute for Rm {
    fn execute(self, app: &mut App, out: &mut dyn Write) -> Result<()> {
        let id = self.id;
        let purge = self.purge;
        let mut removed = None;

        app.with_current_config(|locked| {
            let Some(account) = locked.config.find_account(&id).cloned() else {
                bail!("account '{id}' not found");
            };
            locked.config.remove_account(&id);
            locked.save_config()?;
            removed = Some(account);
            Ok(())
        })?;

        let Some(account) = removed else {
            return Ok(());
        };
        let adapter = AdapterRegistry.adapter_for(account.provider);
        let profile = adapter.build_environment(&account, &app.paths.profiles_dir);
        if purge {
            crate::agy_keyring::reset(&account, &profile)?;
            if let Err(err) = fs::remove_dir_all(&profile.profile_dir)
                && err.kind() != ErrorKind::NotFound
            {
                return Err(err).with_context(|| {
                    format!(
                        "failed to delete profile directory {}",
                        profile.profile_dir.display()
                    )
                });
            }
            writeln!(
                out,
                "Removed account '{id}' from config and deleted {}",
                profile.profile_dir.display()
            )?;
        } else {
            writeln!(
                out,
                "Removed account '{id}' from config (profile kept at {}; pass --purge to delete)",
                profile.profile_dir.display()
            )?;
        }
        Ok(())
    }
}

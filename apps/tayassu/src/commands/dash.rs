use crate::app::{App, Execute};
use crate::probing::probe_accounts;
use crate::tui;
use anyhow::{Context, Result};
use std::io::Write;
use tayassu_store::RuntimeState;

/// Open an interactive live terminal dashboard for accounts, quotas, and cooldowns.
#[derive(Debug, clap::Args)]
pub struct Dash;

impl Execute for Dash {
    fn execute(self, app: &mut App, _out: &mut dyn Write) -> Result<()> {
        let paths = app.paths.clone();
        let accounts = app.config.accounts.clone();
        let probe_accounts_list = accounts.clone();
        let probe_paths = paths.clone();

        tui::run_dashboard(
            &accounts,
            &mut app.state,
            &paths.state_file,
            move |state| {
                let (ok_count, err_count) = probe_accounts(
                    &probe_accounts_list,
                    &probe_accounts_list,
                    &probe_paths,
                    state,
                    false,
                    None,
                )?;
                Ok(format!("Probe complete: {ok_count} ok, {err_count} failed"))
            },
            |state, account_id| {
                let (updated, cleared) = RuntimeState::update_locked(&paths.state_file, |st| {
                    let status = st.status_for_mut(account_id);
                    let count = status.cooldowns.len();
                    status.cooldowns.clear();
                    count
                })
                .with_context(|| format!("failed to save {}", paths.state_file.display()))?;
                *state = updated;
                Ok(format!("Cleared {cleared} cooldown(s) for '{account_id}'"))
            },
        )
    }
}

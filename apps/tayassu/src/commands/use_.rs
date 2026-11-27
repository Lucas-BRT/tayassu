use crate::app::{App, Execute};
use crate::shell::{account_candidates, chain_candidates};
use anyhow::{Context, Result, bail};
use clap::Args;
use clap_complete::ArgValueCandidates;
use std::env;
use std::io::Write;
use tayassu_core::{AccountId, SelectionPolicy};
use tayassu_store::{PROJECT_CONFIG_FILE_NAME, ProjectConfig};

/// Pin routing for this project (`.tayassu.toml`) or globally with `--global`.
#[derive(Debug, Args)]
pub struct Use {
    /// Prefer this account for its provider in the current project.
    #[arg(add = ArgValueCandidates::new(account_candidates))]
    account: Option<AccountId>,
    /// Default fallback chain for the current project.
    #[arg(long, add = ArgValueCandidates::new(chain_candidates))]
    chain: Option<String>,
    /// Default selection policy.
    #[arg(long)]
    policy: Option<SelectionPolicy>,
    /// Write the policy to the global `config.toml` instead of the project file.
    #[arg(long)]
    global: bool,
}

impl Execute for Use {
    fn execute(self, app: &mut App, out: &mut dyn Write) -> Result<()> {
        if self.account.is_none() && self.chain.is_none() && self.policy.is_none() {
            bail!("nothing to set: pass an account, `--chain <name>` or `--policy <policy>`");
        }
        if self.global {
            if self.account.is_some() || self.chain.is_some() {
                bail!("`--global` only supports `--policy`");
            }
            if let Some(policy) = self.policy {
                app.config.default_policy = policy;
                app.save_config()?;
                writeln!(out, "Global default policy set to '{policy}'")?;
            }
            return Ok(());
        }

        let cwd = env::current_dir().context("failed to resolve current directory")?;
        let (path, mut project) = ProjectConfig::discover(&cwd)
            .context("failed to load .tayassu.toml")?
            .unwrap_or_else(|| (cwd.join(PROJECT_CONFIG_FILE_NAME), ProjectConfig::default()));

        if let Some(id) = &self.account {
            let provider = app.account(id)?.provider;
            project
                .preferred_accounts
                .insert(provider.as_str().to_owned(), id.clone());
            writeln!(out, "Prefer '{id}' for {provider}")?;
        }
        if let Some(chain) = self.chain {
            if !app.config.fallback_chains.contains_key(&chain) {
                bail!("fallback chain '{chain}' not found in config.toml");
            }
            writeln!(out, "Default chain set to '{chain}'")?;
            project.fallback_chain = Some(chain);
        }
        if let Some(policy) = self.policy {
            project.default_policy = Some(policy);
            writeln!(out, "Project policy set to '{policy}'")?;
        }
        project
            .save_atomic(&path)
            .with_context(|| format!("failed to write {}", path.display()))?;
        writeln!(out, "Updated {}", path.display())?;
        Ok(())
    }
}

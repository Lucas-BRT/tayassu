use crate::app::{App, Execute};
use crate::shell::{DEFAULT_SHELL_PROVIDERS, ShellKind};
use anyhow::{Context, Result};
use clap::Args;
use std::io::Write;
use tayassu_core::ProviderKind;

/// Print shell code that routes `claude`, `agy`, ... through tayassu and enables completion.
///
/// Use as `eval "$(tayassu activate bash)"`; `tayassu setup` adds this to your rc file.
#[derive(Debug, Args)]
pub struct Activate {
    /// Target shell.
    shell: ShellKind,
    /// Restrict wrappers to specific provider(s) (repeatable). Defaults to all built-in agents.
    #[arg(long = "provider")]
    providers: Vec<ProviderKind>,
    /// Generate wrappers that disable failover.
    #[arg(long)]
    no_failover: bool,
}

impl Execute for Activate {
    fn execute(self, _app: &mut App, out: &mut dyn Write) -> Result<()> {
        let providers = if self.providers.is_empty() {
            DEFAULT_SHELL_PROVIDERS
        } else {
            &self.providers
        };
        let script = self.shell.shell().activation(providers, !self.no_failover);
        write!(out, "{script}").context("failed to write activation script")
    }
}

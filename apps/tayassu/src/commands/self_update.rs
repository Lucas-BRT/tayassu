use crate::app::{App, Execute};
use anyhow::{Context, Result, bail};
use clap::Args;
use std::io::Write;
use std::process::Command as ProcessCommand;

/// Installer script fetched by `self-update`.
const INSTALL_SCRIPT_URL: &str =
    "https://raw.githubusercontent.com/Lucas-BRT/tayassu/main/install.sh";

/// Update tayassu itself to the latest release.
#[derive(Debug, Args)]
pub struct SelfUpdate;

impl Execute for SelfUpdate {
    fn execute(self, _app: &mut App, out: &mut dyn Write) -> Result<()> {
        writeln!(out, "Updating tayassu via {INSTALL_SCRIPT_URL}...")?;
        out.flush()?;
        let status = ProcessCommand::new("sh")
            .arg("-c")
            .arg(format!("curl -fsSL {INSTALL_SCRIPT_URL} | bash"))
            .status()
            .context("failed to run the installer")?;
        if !status.success() {
            bail!("installer exited with status {:?}", status.code());
        }
        Ok(())
    }
}

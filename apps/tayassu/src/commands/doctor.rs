use crate::app::{App, Execute};
use crate::doctor::DoctorReport;
use anyhow::{Context, Result, bail};
use clap::Args;
use std::io::Write;

/// Validate installed binaries, fake-HOME symlinks, and credential file permissions.
#[derive(Debug, Args)]
pub struct Doctor {
    /// Exit non-zero on warnings as well as errors.
    #[arg(long)]
    strict: bool,
}

impl Execute for Doctor {
    fn execute(self, app: &mut App, mut out: &mut dyn Write) -> Result<()> {
        let strict = self.strict;
        let report = DoctorReport::run(&app.config.accounts, &app.paths);
        report
            .write_to(&mut out)
            .context("failed to write doctor report")?;
        if report.has_errors() || (strict && report.has_warnings_or_errors()) {
            bail!("doctor reported issues");
        }
        Ok(())
    }
}

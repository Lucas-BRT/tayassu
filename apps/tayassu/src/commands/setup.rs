use crate::app::{App, Execute};
use crate::artifacts::{ManagedArtifact, ShellActivation, SystemdTimer};
use crate::systemd::DEFAULT_PROBE_INTERVAL_MINUTES;
use anyhow::Result;
use clap::Args;
use std::io::Write;

/// Configure shell activation, completion and background probing (safe to re-run).
#[derive(Debug, Args)]
pub struct Setup {
    /// Background probe interval in minutes.
    #[arg(
        long,
        default_value_t = DEFAULT_PROBE_INTERVAL_MINUTES,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    interval_minutes: u64,
    /// Skip installing the systemd probe timer.
    #[arg(long)]
    no_systemd: bool,
}

impl Execute for Setup {
    fn execute(self, app: &mut App, out: &mut dyn Write) -> Result<()> {
        let mut steps: Vec<Box<dyn ManagedArtifact>> = vec![Box::new(ShellActivation)];
        if !self.no_systemd {
            steps.push(Box::new(SystemdTimer {
                interval_minutes: self.interval_minutes,
            }));
        }
        for step in steps {
            let changes = step.install(app)?;
            if changes.is_empty() {
                writeln!(out, "{}: already configured", step.label())?;
            }
            for line in changes {
                writeln!(out, "{}: {line}", step.label())?;
            }
        }
        Ok(())
    }
}

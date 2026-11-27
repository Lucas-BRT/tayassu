use crate::app::{App, Execute};
use crate::artifacts::{ManagedArtifact, ShellActivation, ShellCompletion, SystemdTimer};
use crate::systemd::DEFAULT_PROBE_INTERVAL_MINUTES;
use crate::systemd::SYSTEMD_TIMER_NAME;
use anyhow::Result;
use clap::{Args, Subcommand};
use std::io::Write;
use std::process::Command;

/// Configure background probing; shell wrappers require an explicit subcommand.
#[derive(Debug, Args)]
pub struct Setup {
    #[command(subcommand)]
    action: Option<SetupAction>,
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

#[derive(Debug, Subcommand)]
enum SetupAction {
    /// Install shell wrappers and completion into shell startup files.
    Shell,
    /// Install shell completion without enabling provider wrappers.
    Completion,
}

impl Execute for Setup {
    fn execute(self, app: &mut App, out: &mut dyn Write) -> Result<()> {
        let shell = matches!(self.action, Some(SetupAction::Shell));
        let completion = matches!(self.action, Some(SetupAction::Completion));
        let mut steps: Vec<Box<dyn ManagedArtifact>> = Vec::new();
        if shell {
            steps.push(Box::new(ShellActivation));
        } else if completion {
            steps.push(Box::new(ShellCompletion));
        } else if !self.no_systemd {
            steps.push(Box::new(SystemdTimer {
                interval_minutes: self.interval_minutes,
            }));
        }
        for step in steps {
            let changes = step.install(app)?;
            if changes.is_empty() {
                if step.planned_removals(app)?.is_empty() {
                    writeln!(
                        out,
                        "{}: no supported shell configuration found",
                        step.label()
                    )?;
                } else {
                    writeln!(out, "{}: already configured", step.label())?;
                }
            }
            for line in changes {
                writeln!(out, "{}: {line}", step.label())?;
            }
        }
        if !shell && !completion && !self.no_systemd {
            match Command::new("systemctl")
                .args(["--user", "is-active", SYSTEMD_TIMER_NAME])
                .output()
            {
                Ok(output) if output.status.success() => writeln!(out, "systemd: timer is active")?,
                Ok(output) if output.stdout.trim_ascii() == b"inactive" => {
                    writeln!(out, "systemd: timer is inactive")?;
                }
                _ => writeln!(
                    out,
                    "systemd: timer status could not be confirmed; check with `systemctl --user status {SYSTEMD_TIMER_NAME}`"
                )?,
            }
        }
        if shell {
            writeln!(
                out,
                "shell: to activate in this session, run `eval \"$(tayassu activate bash)\"` for bash (use `zsh` for zsh; fish: `tayassu activate fish | source`)"
            )?;
        } else if completion {
            writeln!(out, "completion: restart your shell to load completion")?;
        } else {
            writeln!(
                out,
                "shell: run `tayassu setup shell` to enable provider wrappers and completion"
            )?;
        }
        Ok(())
    }
}

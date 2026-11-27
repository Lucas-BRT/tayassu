use crate::app::{App, Execute};
use crate::artifacts::{
    BinaryFile, ManagedArtifact, ShellActivation, ShellCompletion, SystemdTimer, UserData,
};
use crate::systemd::DEFAULT_PROBE_INTERVAL_MINUTES;
use anyhow::{Context, Result, bail};
use clap::Args;
use std::io::{self, BufRead, Write};

/// Remove tayassu shell hooks, systemd timer, and binary (and optionally user data with `--purge`).
#[derive(Debug, Args)]
pub struct Uninstall {
    /// Skip confirmation prompts.
    #[arg(long, short = 'y')]
    yes: bool,
    /// Show what would be removed without deleting anything.
    #[arg(long)]
    dry_run: bool,
    /// Also delete config, state, and isolated account profiles (contains credentials).
    #[arg(long)]
    purge: bool,
}

impl Uninstall {
    fn artifacts(&self) -> Vec<Box<dyn ManagedArtifact>> {
        let mut items: Vec<Box<dyn ManagedArtifact>> = vec![
            Box::new(ShellActivation),
            Box::new(ShellCompletion),
            Box::new(SystemdTimer {
                interval_minutes: DEFAULT_PROBE_INTERVAL_MINUTES,
            }),
            Box::new(BinaryFile),
        ];
        if self.purge {
            items.push(Box::new(UserData));
        }
        items
    }
}

impl Execute for Uninstall {
    fn execute(self, app: &mut App, out: &mut dyn Write) -> Result<()> {
        let artifacts = self.artifacts();
        let mut plan = Vec::new();
        for artifact in &artifacts {
            for item in artifact.planned_removals(app)? {
                plan.push((artifact.label(), item));
            }
        }

        if plan.is_empty() {
            writeln!(out, "Nothing to uninstall.")?;
            return Ok(());
        }

        for (label, item) in &plan {
            let prefix = if self.dry_run { "[dry-run] " } else { "" };
            writeln!(out, "{prefix}{label}: {item}")?;
        }

        if self.dry_run {
            return Ok(());
        }

        if !self.yes {
            let prompt = if self.purge {
                "Proceed with uninstall AND delete all credentials/profiles? [y/N] "
            } else {
                "Proceed with uninstall? [y/N] "
            };
            write!(out, "{prompt}")?;
            out.flush()?;
            let mut answer = String::new();
            io::stdin()
                .lock()
                .read_line(&mut answer)
                .context("failed to read confirmation")?;
            if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                bail!("uninstall aborted");
            }
        }

        for artifact in &artifacts {
            for line in artifact.remove(app)? {
                writeln!(out, "{}: {line}", artifact.label())?;
            }
        }
        writeln!(
            out,
            "shell: restart your shell to remove active provider wrappers and refresh its command cache"
        )?;
        Ok(())
    }
}

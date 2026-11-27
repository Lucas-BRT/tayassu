use crate::app::{App, Execute};
use crate::shell::ShellKind;
use crate::systemd::{DEFAULT_PROBE_INTERVAL_MINUTES, SystemdUnits};
use anyhow::{Context, Result};
use clap::Args;
use std::env;
use std::io::Write;
use std::path::Path;

/// Relative path under `$XDG_CONFIG_HOME` where `systemd --user` units live.
const SYSTEMD_USER_REL_DIR: &str = "systemd/user";

/// One idempotent piece of first-time setup.
trait SetupStep {
    /// Short name printed before the step's report.
    fn label(&self) -> &'static str;

    /// Applies the step and returns one line per change (empty when already configured).
    fn apply(&self, app: &App) -> Result<Vec<String>>;
}

/// Hooks `tayassu activate` into every detected shell.
struct ShellActivation;

impl SetupStep for ShellActivation {
    fn label(&self) -> &'static str {
        "shell"
    }

    fn apply(&self, app: &App) -> Result<Vec<String>> {
        let mut changes = Vec::new();
        for kind in ShellKind::ALL {
            let change = kind
                .shell()
                .install(&app.paths.home_dir)
                .with_context(|| format!("failed to set up {}", kind.shell().name()))?;
            changes.extend(change);
        }
        Ok(changes)
    }
}

/// Installs the `systemd --user` background probe units.
struct SystemdTimer {
    interval_minutes: u64,
}

impl SetupStep for SystemdTimer {
    fn label(&self) -> &'static str {
        "systemd"
    }

    fn apply(&self, app: &App) -> Result<Vec<String>> {
        let exe = env::current_exe().context("failed to resolve current tayassu binary")?;
        let config_base = app
            .paths
            .config_file
            .parent()
            .and_then(Path::parent)
            .context("failed to resolve XDG config root")?;
        let (svc, tmr) = SystemdUnits::new(exe, self.interval_minutes)
            .install_to(&config_base.join(SYSTEMD_USER_REL_DIR))?;
        Ok(vec![
            format!("installed {}", svc.display()),
            format!("installed {}", tmr.display()),
            "enable with: systemctl --user daemon-reload && systemctl --user enable --now tayassu-probe.timer"
                .to_owned(),
        ])
    }
}

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
        let mut steps: Vec<Box<dyn SetupStep>> = vec![Box::new(ShellActivation)];
        if !self.no_systemd {
            steps.push(Box::new(SystemdTimer {
                interval_minutes: self.interval_minutes,
            }));
        }
        for step in steps {
            let changes = step.apply(app)?;
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

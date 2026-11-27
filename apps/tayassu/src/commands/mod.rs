mod activate;
mod add;
mod config;
mod cooldown;
mod dash;
mod doctor;
mod import;
mod login;
mod ls;
mod probe;
mod providers;
mod rm;
mod run;
mod self_update;
mod setup;
mod uninstall;
mod use_;

use crate::app::{App, Execute};
use anyhow::Result;
use clap::Subcommand;
use std::io::Write;

/// Top-level CLI subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// List accounts with remaining quota and cooldowns (default).
    Ls(ls::Ls),
    /// Open an interactive live dashboard.
    Dash(dash::Dash),
    /// Register an account and log in.
    Add(add::Add),
    /// Remove an account.
    Rm(rm::Rm),
    /// Authenticate an existing account.
    Login(login::Login),
    /// Discover installed CLIs and import their credentials.
    Import(import::Import),
    /// Pin routing for this project, or the global policy with `--global`.
    Use(use_::Use),
    /// Route to the best account and run its CLI.
    #[command(visible_aliases = ["r", "x"])]
    Run(run::Run),
    /// Show which account would be used right now.
    Where(run::Where),
    /// Refresh quota windows by probing each provider CLI.
    Probe(probe::Probe),
    /// Validate binaries, profiles and credential permissions.
    Doctor(doctor::Doctor),
    /// Print shell code for wrappers and completion (`eval "$(tayassu activate bash)"`).
    Activate(activate::Activate),
    /// Configure shell activation and the background probe timer.
    Setup(setup::Setup),
    /// Read or write global settings.
    Config(config::Config),
    /// Update tayassu itself.
    SelfUpdate(self_update::SelfUpdate),
    /// Manage installed provider CLIs.
    Providers(providers::Providers),
    /// Remove tayassu hooks, timer, and binary.
    Uninstall(uninstall::Uninstall),
    /// Manage cooldowns by hand.
    #[command(hide = true)]
    Cooldown(cooldown::Cooldown),
}

impl Default for Command {
    fn default() -> Self {
        Self::Ls(ls::Ls::default())
    }
}

impl Execute for Command {
    fn execute(self, app: &mut App, out: &mut dyn Write) -> Result<()> {
        match self {
            Self::Ls(c) => c.execute(app, out),
            Self::Dash(c) => c.execute(app, out),
            Self::Add(c) => c.execute(app, out),
            Self::Rm(c) => c.execute(app, out),
            Self::Login(c) => c.execute(app, out),
            Self::Import(c) => c.execute(app, out),
            Self::Use(c) => c.execute(app, out),
            Self::Run(c) => c.execute(app, out),
            Self::Where(c) => c.execute(app, out),
            Self::Probe(c) => c.execute(app, out),
            Self::Doctor(c) => c.execute(app, out),
            Self::Activate(c) => c.execute(app, out),
            Self::Setup(c) => c.execute(app, out),
            Self::Config(c) => c.execute(app, out),
            Self::SelfUpdate(c) => c.execute(app, out),
            Self::Providers(c) => c.execute(app, out),
            Self::Uninstall(c) => c.execute(app, out),
            Self::Cooldown(c) => c.execute(app, out),
        }
    }
}

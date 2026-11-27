use crate::commands::Command;
use clap::Parser;

/// Multi-account session and cooldown manager for subscription-authenticated AI CLIs.
#[derive(Debug, Parser)]
#[command(name = "tayassu", version, about)]
pub struct Cli {
    /// Subcommand to execute (defaults to `ls`).
    #[command(subcommand)]
    pub command: Option<Command>,
}

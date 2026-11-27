mod app;
mod artifacts;
mod claude_usage;
mod cli;
mod commands;
mod doctor;
mod opencode_usage;
mod probing;
mod quota_rpc;
mod release;
mod render;
mod routing;
mod session;
mod shell;
mod spawn;
mod systemd;
mod tui;
mod update_notifier;

use anyhow::Result;
use app::{App, Execute};
use chrono::Utc;
use clap::{CommandFactory, Parser};
use clap_complete::CompleteEnv;
use cli::Cli;
use session::{RECEIVED_INT, install_ctrlc_handler, process_is_alive};
use shell::COMPLETION_ENV;
use std::io::{self, IsTerminal};
use std::sync::atomic::Ordering;
use tayassu_store::RuntimeState;
use tracing_subscriber::EnvFilter;
use update_notifier::UpdateNotifier;

/// Default tracing filter when `RUST_LOG` is unset.
const DEFAULT_LOG_FILTER: &str = "warn";

fn main() -> Result<()> {
    init_tracing();
    install_ctrlc_handler();
    CompleteEnv::with_factory(Cli::command)
        .var(COMPLETION_ENV)
        .complete();
    let cli = Cli::parse();
    let mut app = App::load()?;
    let mut stdout = io::stdout().lock();

    let skip_update_check = UpdateNotifier::should_skip(cli.command.as_ref(), &app.config);
    if !skip_update_check {
        UpdateNotifier::maybe_spawn_background(&app.paths.state_file, Utc::now());
    }

    // Run the CLI; if SIGINT was received, perform explicit session cleanup before exiting.
    let result = cli
        .command
        .unwrap_or_default()
        .execute(&mut app, &mut stdout);
    if RECEIVED_INT.load(Ordering::SeqCst) {
        let _ = RuntimeState::update_locked(&app.paths.state_file, |state| {
            for status in &mut state.statuses {
                status.prune_sessions(process_is_alive);
            }
        });
    }
    if !skip_update_check && result.is_ok() {
        let mut stderr = io::stderr().lock();
        let is_tty = stderr.is_terminal();
        UpdateNotifier::maybe_notify(&app.paths.state_file, Utc::now(), is_tty, &mut stderr);
    }
    result
}

fn init_tracing() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .init();
}

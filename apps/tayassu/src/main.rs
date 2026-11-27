mod app;
mod cli;
mod commands;
mod completion;
mod doctor;
mod probing;
mod render;
mod session;
mod shell;
mod spawn;
mod systemd;
mod tui;

use anyhow::Result;
use app::{App, Execute};
use clap::{CommandFactory, Parser};
use clap_complete::CompleteEnv;
use cli::Cli;
use session::{RECEIVED_INT, install_ctrlc_handler, process_is_alive};
use std::io;
use std::sync::atomic::Ordering;
use tayassu_store::RuntimeState;
use tracing_subscriber::EnvFilter;

/// Default tracing filter when `RUST_LOG` is unset.
const DEFAULT_LOG_FILTER: &str = "warn";

fn main() -> Result<()> {
    CompleteEnv::with_factory(Cli::command).complete();
    init_tracing();
    install_ctrlc_handler();
    let cli = Cli::parse();
    let mut app = App::load()?;
    let mut stdout = io::stdout().lock();

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

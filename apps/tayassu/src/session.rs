use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use tayassu_core::AccountId;
use tayassu_store::RuntimeState;

/// Atomic flag set when SIGINT (Ctrl-C) is received, to trigger explicit session cleanup.
pub static RECEIVED_INT: AtomicBool = AtomicBool::new(false);

/// Installs a Ctrl-C handler that sets `RECEIVED_INT`.
/// On systems where ctrlc isn't available (e.g. some Windows configs), it gracefully no-ops.
pub fn install_ctrlc_handler() {
    let _ = ctrlc::set_handler(move || {
        RECEIVED_INT.store(true, Ordering::SeqCst);
    });
}

/// Returns whether a process with `pid` currently exists (Linux `/proc`).
pub fn process_is_alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

/// RAII guard tracking an active `tayassu run` session in `state.json` under an exclusive lock.
pub struct SessionGuard {
    state_file: PathBuf,
    account_id: AccountId,
}

impl SessionGuard {
    pub fn acquire(state_file: PathBuf, account_id: AccountId) -> Result<Self> {
        let id = account_id.clone();
        RuntimeState::update_locked(&state_file, |state| {
            state.status_for_mut(&id).add_session(process::id());
        })
        .with_context(|| format!("failed to record active session for '{account_id}'"))?;
        Ok(Self {
            state_file,
            account_id,
        })
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        let id = self.account_id.clone();
        let _ = RuntimeState::update_locked(&self.state_file, |state| {
            state.status_for_mut(&id).remove_session(process::id());
        });
    }
}

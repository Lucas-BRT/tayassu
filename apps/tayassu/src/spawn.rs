use anyhow::{Context, Result, bail};
use std::env;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitStatus, Stdio};
use std::thread;
use tayassu_core::AccountConfig;
use tayassu_providers::HostDiscovery;

/// Fallback non-zero exit code when a child process is terminated by a signal.
pub const SIGNAL_FALLBACK_EXIT_CODE: i32 = 1;

/// Maximum stderr bytes buffered in memory during `tayassu run` for passive rate-limit detection.
const MAX_CAPTURED_STDERR_BYTES: usize = 64 * 1024;

/// Chunk size in bytes used when forwarding child stderr to the user's terminal.
const STDERR_READ_CHUNK_BYTES: usize = 4096;

/// Arguments for dumping the host `opencode` credentials as JSON on stdout.
const OPENCODE_AUTH_EXPORT_ARGS: &[&str] = &["auth", "export"];

/// Arguments for reading exported `opencode` credentials from stdin.
const OPENCODE_AUTH_IMPORT_ARGS: &[&str] = &["auth", "import"];

pub struct RunOutcome {
    pub status: ExitStatus,
    pub captured_stderr: String,
}

/// Transfers `opencode` credentials through a pipe from `auth export` to `auth import`.
pub fn transfer_opencode_auth(binary: &Path, profile_env: &[(String, String)]) -> Result<()> {
    let exported = ProcessCommand::new(binary)
        .args(OPENCODE_AUTH_EXPORT_ARGS)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .context("failed to run `opencode auth export` on the host")?;
    if !exported.status.success() {
        bail!("`opencode auth export` exited with {}", exported.status);
    }

    let mut cmd = ProcessCommand::new(binary);
    cmd.args(OPENCODE_AUTH_IMPORT_ARGS)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (key, value) in profile_env {
        cmd.env(key, value);
    }
    let mut child = cmd
        .spawn()
        .context("failed to run `opencode auth import` in the profile")?;
    child
        .stdin
        .take()
        .context("opencode import stdin unavailable")?
        .write_all(&exported.stdout)
        .context("failed to pipe credentials into `opencode auth import`")?;
    let status = child
        .wait()
        .context("failed waiting on `opencode auth import`")?;
    if !status.success() {
        bail!("`opencode auth import` exited with {status}");
    }
    Ok(())
}

pub fn account_executable(account: &AccountConfig, home_dir: &Path) -> PathBuf {
    let executable = account.executable();
    HostDiscovery
        .resolve_executable(executable, home_dir, env::var_os("PATH").as_deref())
        .unwrap_or_else(|| PathBuf::from(executable))
}

pub fn spawn_interactive(
    account: &AccountConfig,
    executable: &Path,
    env_vars: &[(String, String)],
    args: &[String],
) -> Result<()> {
    let mut cmd = ProcessCommand::new(executable);
    cmd.args(args);
    for (key, value) in env_vars {
        cmd.env(key, value);
    }

    let exit_status = cmd.status().with_context(|| {
        format!(
            "failed to spawn '{}' for account '{}'",
            executable.display(),
            account.id
        )
    })?;

    if !exit_status.success() {
        let code = exit_status.code().unwrap_or(SIGNAL_FALLBACK_EXIT_CODE);
        bail!(
            "'{}' (account '{}') exited with status {code}",
            executable.display(),
            account.id
        );
    }
    Ok(())
}

/// Spawns the agent CLI with inherited stdin/stdout and teed stderr for rate-limit detection.
pub fn spawn_capturing_stderr(
    account: &AccountConfig,
    executable: &Path,
    env_vars: &[(String, String)],
    args: &[String],
) -> Result<RunOutcome> {
    let mut cmd = ProcessCommand::new(executable);
    cmd.args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::piped());
    for (key, value) in env_vars {
        cmd.env(key, value);
    }

    let mut child = cmd.spawn().with_context(|| {
        format!(
            "failed to spawn '{}' for account '{}'",
            executable.display(),
            account.id
        )
    })?;

    let mut stderr_pipe = child.stderr.take().context("child stderr unavailable")?;
    let tee_handle = thread::spawn(move || -> String {
        let mut captured = Vec::new();
        let mut buf = [0u8; STDERR_READ_CHUNK_BYTES];
        let mut stderr = io::stderr();
        while let Ok(read_bytes) = stderr_pipe.read(&mut buf) {
            if read_bytes == 0 {
                break;
            }
            let _ = stderr.write_all(&buf[..read_bytes]);
            let _ = stderr.flush();
            if captured.len() < MAX_CAPTURED_STDERR_BYTES {
                let remaining = MAX_CAPTURED_STDERR_BYTES - captured.len();
                let take = remaining.min(read_bytes);
                captured.extend_from_slice(&buf[..take]);
            }
        }
        String::from_utf8_lossy(&captured).into_owned()
    });

    let status = child
        .wait()
        .with_context(|| format!("failed waiting on '{}'", executable.display()))?;
    let captured_stderr = tee_handle.join().unwrap_or_default();

    Ok(RunOutcome {
        status,
        captured_stderr,
    })
}

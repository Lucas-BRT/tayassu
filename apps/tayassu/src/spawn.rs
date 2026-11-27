use anyhow::{Context, Result, bail};
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Command as ProcessCommand, ExitStatus, Stdio};
use std::thread;
use tayassu_core::AccountConfig;

/// Maximum stderr bytes buffered in memory during `tayassu run` for passive rate-limit detection.
/// Fallback non-zero exit code when a child process is terminated by a signal.
pub const SIGNAL_FALLBACK_EXIT_CODE: i32 = 1;

pub const MAX_CAPTURED_STDERR_BYTES: usize = 64 * 1024;

/// Chunk size in bytes used when forwarding child stderr to the user's terminal.
pub const STDERR_READ_CHUNK_BYTES: usize = 4096;

/// Arguments for dumping the host `opencode` credentials as JSON on stdout.
pub const OPENCODE_AUTH_EXPORT_ARGS: &[&str] = &["auth", "export"];

/// Arguments for reading exported `opencode` credentials from stdin.
pub const OPENCODE_AUTH_IMPORT_ARGS: &[&str] = &["auth", "import"];

pub struct RunOutcome {
    pub status: ExitStatus,
    pub captured_stderr: String,
}

/// Copies host `opencode` credentials (stored in its database, not a file) into an isolated profile.
///
/// The secrets flow through a pipe from `auth export` to `auth import` and never touch disk.
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

pub fn spawn_interactive(
    account: &AccountConfig,
    env_vars: &[(String, String)],
    args: &[String],
) -> Result<()> {
    let mut cmd = ProcessCommand::new(account.executable());
    cmd.args(args);
    for (key, value) in env_vars {
        cmd.env(key, value);
    }

    let exit_status = cmd.status().with_context(|| {
        format!(
            "failed to spawn '{}' for account '{}'",
            account.executable(),
            account.id
        )
    })?;

    if !exit_status.success() {
        let code = exit_status.code().unwrap_or(SIGNAL_FALLBACK_EXIT_CODE);
        bail!(
            "'{}' (account '{}') exited with status {code}",
            account.executable(),
            account.id
        );
    }
    Ok(())
}

/// Spawns the agent CLI with inherited stdin/stdout and teed stderr for rate-limit detection.
pub fn spawn_capturing_stderr(
    account: &AccountConfig,
    env_vars: &[(String, String)],
    args: &[String],
) -> Result<RunOutcome> {
    let mut cmd = ProcessCommand::new(account.executable());
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
            account.executable(),
            account.id
        )
    })?;

    let stderr_handle = child.stderr.take().map(|mut child_stderr| {
        thread::spawn(move || {
            let mut captured = Vec::new();
            let mut buf = [0u8; STDERR_READ_CHUNK_BYTES];
            let mut host_stderr = io::stderr();
            while let Ok(read_len) = child_stderr.read(&mut buf) {
                if read_len == 0 {
                    break;
                }
                let chunk = &buf[..read_len];
                let _ = host_stderr.write_all(chunk);
                let _ = host_stderr.flush();
                captured.extend_from_slice(chunk);
                // Keep the newest bytes: a limit message appears at the end of a session.
                if captured.len() > MAX_CAPTURED_STDERR_BYTES {
                    let excess = captured.len() - MAX_CAPTURED_STDERR_BYTES;
                    captured.drain(..excess);
                }
            }
            String::from_utf8_lossy(&captured).into_owned()
        })
    });

    let status = child.wait().with_context(|| {
        format!(
            "failed waiting on '{}' for account '{}'",
            account.executable(),
            account.id
        )
    })?;

    let captured_stderr = stderr_handle
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();

    Ok(RunOutcome {
        status,
        captured_stderr,
    })
}

use anyhow::{Result, bail};
use std::fmt;
use tayassu_core::AccountConfig;
use tayassu_providers::ProfileEnvironment;

/// Typed error returned when an account's private keyring is locked or uninitialized
/// during a non-interactive operation.
#[derive(Debug)]
pub(crate) struct KeyringLockedError(pub(crate) String);

impl fmt::Display for KeyringLockedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for KeyringLockedError {}

#[derive(Clone, Copy)]
pub(crate) enum Operation {
    Login,
    Run,
    Probe,
}

pub(crate) fn prepare(
    account: &AccountConfig,
    profile: &mut ProfileEnvironment,
    operation: Operation,
) -> Result<()> {
    if !account.uses_private_agy_keyring() {
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        use anyhow::Context;
        linux::attach(profile, operation, account.id.as_str())
            .with_context(|| format!("private Antigravity keyring for '{}'", account.id))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (profile, operation);
        bail!("private Antigravity keyrings require Linux")
    }
}

pub(crate) fn reset(account: &AccountConfig, profile: &ProfileEnvironment) -> Result<()> {
    #[cfg(target_os = "linux")]
    if account.provider == tayassu_core::ProviderKind::Agy {
        linux::reset(profile)?;
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (account, profile);
    Ok(())
}

pub(crate) fn check_after_launch(
    account: &AccountConfig,
    profile: &ProfileEnvironment,
) -> Result<()> {
    #[cfg(target_os = "linux")]
    if account.uses_private_agy_keyring() {
        linux::check_after_launch(profile)?;
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (account, profile);
    Ok(())
}

/// Bound a private-keyring probe even if Antigravity waits for OAuth before starting its own timer.
pub(crate) fn probe_output(command: &mut std::process::Command) -> Result<std::process::Output> {
    use anyhow::{Context, ensure};
    use std::io::{Read, Seek};
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
    const POLL_INTERVAL: Duration = Duration::from_millis(50);
    const MAX_OUTPUT_BYTES: u64 = 1024 * 1024;

    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?)
        .spawn()
        .context("failed to start private Antigravity probe")?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL_INTERVAL),
            result => {
                let _ = child.kill();
                let _ = child.wait();
                result.context("failed waiting for private Antigravity probe")?;
                bail!("Antigravity probe timed out; check the profile's sign-in in a terminal");
            }
        }
    };

    let mut output = std::process::Output {
        status,
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    for (file, buffer) in [
        (&mut stdout, &mut output.stdout),
        (&mut stderr, &mut output.stderr),
    ] {
        file.rewind()?;
        let size = file.take(MAX_OUTPUT_BYTES + 1).read_to_end(buffer)?;
        ensure!(
            size as u64 <= MAX_OUTPUT_BYTES,
            "Antigravity probe output exceeded the capture limit"
        );
    }
    Ok(output)
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{Operation, ProfileEnvironment, bail};
    use anyhow::{Context, Result, ensure};
    use sha2::{Digest, Sha256};
    use std::env;
    use std::fs::{self, DirBuilder, OpenOptions};
    use std::io::{IsTerminal, Write};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    const BUS_ENV: &str = "DBUS_SESSION_BUS_ADDRESS";
    const SECRET_SERVICE: &str = "org.freedesktop.secrets";
    const SERVICE_PATH: &str = "/org/freedesktop/secrets";
    const LOGIN_COLLECTION: &str = "/org/freedesktop/secrets/collection/login";
    const FALLBACK_TOKEN: &str = "home/.gemini/antigravity-cli/antigravity-oauth-token";
    const PRIVATE_MODE: u32 = 0o700;
    const PERMISSION_BITS: u32 = 0o777;
    const PRIVATE_FILE_MODE: u32 = 0o600;
    const DBUS_TIMEOUT_SECONDS: &str = "5";
    const DBUS_PID_FILE: &str = "dbus.pid";
    const DBUS_DAEMON_COMM: &str = "dbus-daemon";

    const BUS_CONFIG: &str = r#"<busconfig>
      <type>session</type>
      <listen>SOCKET_ADDRESS</listen>
      <auth>EXTERNAL</auth>
      <servicedir>services</servicedir>
      <policy context="default">
        <allow send_destination="*"/>
        <allow receive_sender="*"/>
        <allow own="*"/>
      </policy>
    </busconfig>"#;
    const SERVICE_FILE: &str = "[D-BUS Service]\nName=org.freedesktop.secrets\nExec=/usr/bin/env gnome-keyring-daemon --start --foreground --components=secrets\n";

    pub(super) fn attach(
        profile: &mut ProfileEnvironment,
        operation: Operation,
        account: &str,
    ) -> Result<()> {
        check_fallback(profile)?;
        verify_storage(profile)?;

        let runtime_root = env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .context("XDG_RUNTIME_DIR must point to the current Linux user runtime directory")?;
        let profile_path = profile.profile_dir.canonicalize()?;
        let digest = Sha256::digest(profile_path.as_os_str().as_encoded_bytes());
        let runtime = runtime_root.join(format!("tayassu-agy-{digest:.16x}"));
        match DirBuilder::new().mode(PRIVATE_MODE).create(&runtime) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error).context("failed to create private keyring runtime directory");
            }
        }
        let metadata = runtime.symlink_metadata()?;
        ensure!(
            metadata.is_dir()
                && metadata.uid() == rustix::process::geteuid().as_raw()
                && metadata.mode() & PERMISSION_BITS == PRIVATE_MODE,
            "keyring runtime directory must be owned by the current user and have mode 0700"
        );

        let lock = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .mode(PRIVATE_FILE_MODE)
            .open(runtime.join("lock"))?;
        lock.lock()
            .context("failed to lock private keyring setup")?;

        let address = bus_address(profile, &runtime)?;
        let alias = call(
            &address,
            SERVICE_PATH,
            "org.freedesktop.Secret.Service.ReadAlias",
            &["default"],
        )?;
        let collection = alias_path(&alias)?;
        ensure!(
            matches!(collection, "/" | LOGIN_COLLECTION),
            "private Antigravity keyring has an unexpected default collection"
        );
        if collection == "/" || collection_locked(&address, collection)? {
            let can_unlock = match operation {
                Operation::Login => std::io::stdin().is_terminal(),
                Operation::Run => collection != "/" && std::io::stdin().is_terminal(),
                Operation::Probe => false,
            };
            if !can_unlock {
                return Err(super::KeyringLockedError(format!(
                    "keyring is locked or uninitialized; run 'tayassu login {account}' in a terminal"
                ))
                .into());
            }
            unlock(profile, &runtime, &address, account)?;
            ensure!(
                !collection_locked(&address, LOGIN_COLLECTION)?,
                "keyring is locked; check the password"
            );
            if collection == "/" {
                call(
                    &address,
                    SERVICE_PATH,
                    "org.freedesktop.Secret.Service.SetAlias",
                    &["default", LOGIN_COLLECTION],
                )?;
            }
        }
        profile.env_vars.push((BUS_ENV.to_owned(), address));
        Ok(())
    }

    pub(super) fn reset(profile: &ProfileEnvironment) -> Result<()> {
        let Some(runtime_root) = env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
        else {
            return Ok(());
        };
        let Ok(profile_path) = profile.profile_dir.canonicalize() else {
            return Ok(());
        };
        let digest = Sha256::digest(profile_path.as_os_str().as_encoded_bytes());
        let runtime = runtime_root.join(format!("tayassu-agy-{digest:.16x}"));
        if !runtime.is_dir() {
            return Ok(());
        }
        if let Ok(raw_pid) = fs::read_to_string(runtime.join(DBUS_PID_FILE))
            && let Ok(raw_pid) = raw_pid.trim().parse::<i32>()
            && let Some(pid) = rustix::process::Pid::from_raw(raw_pid)
            && fs::read_to_string(format!("/proc/{raw_pid}/comm"))
                .ok()
                .as_deref()
                .map(str::trim)
                == Some(DBUS_DAEMON_COMM)
        {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
        }
        for name in ["address", DBUS_PID_FILE, "control-root"] {
            let path = runtime.join(name);
            if let Err(error) = fs::remove_file(&path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                return Err(error).with_context(|| format!("failed to remove {}", path.display()));
            }
        }
        Ok(())
    }

    pub(super) fn check_after_launch(profile: &ProfileEnvironment) -> Result<()> {
        check_fallback(profile)?;
        let address = profile
            .env_vars
            .iter()
            .find_map(|(key, value)| (key == BUS_ENV).then_some(value.as_str()))
            .context("private D-Bus address missing from Antigravity launch")?;
        let alias = call(
            address,
            SERVICE_PATH,
            "org.freedesktop.Secret.Service.ReadAlias",
            &["default"],
        )?;
        let collection = alias_path(&alias)?;
        ensure!(
            collection == LOGIN_COLLECTION && !collection_locked(address, collection)?,
            "private keyring became unavailable during the Antigravity operation"
        );
        Ok(())
    }

    fn check_fallback(profile: &ProfileEnvironment) -> Result<()> {
        let fallback = profile.profile_dir.join(FALLBACK_TOKEN);
        match fallback.symlink_metadata() {
            Ok(_) => bail!(
                "Antigravity fallback credential exists at {}; move it out of the profile before using the private keyring",
                fallback.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .context("failed to check for an Antigravity fallback credential");
            }
        }

        Ok(())
    }

    fn verify_storage(profile: &ProfileEnvironment) -> Result<()> {
        for relative in [
            "",
            "home",
            "home/.local",
            "home/.local/share",
            "home/.local/share/keyrings",
            "home/.gemini",
            "home/.gemini/antigravity-cli",
        ] {
            let path = profile.profile_dir.join(relative);
            match path.symlink_metadata() {
                Ok(metadata) => ensure!(
                    metadata.is_dir(),
                    "private Antigravity authentication requires a local directory at {}",
                    path.display()
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("failed to inspect {}", path.display()));
                }
            }
        }
        Ok(())
    }

    fn bus_address(profile: &ProfileEnvironment, runtime: &Path) -> Result<String> {
        let address_file = runtime.join("address");
        if let Ok(address) = fs::read_to_string(&address_file)
            && call(
                &address,
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus.GetId",
                &[],
            )
            .is_ok()
        {
            return Ok(address);
        }

        fs::create_dir_all(runtime.join("services"))?;
        fs::write(
            runtime
                .join("services")
                .join(format!("{SECRET_SERVICE}.service")),
            SERVICE_FILE,
        )?;
        // A fresh socket directory also gives a restarted daemon a fresh control socket.
        let bus_runtime = tempfile::Builder::new()
            .prefix("bus-")
            .tempdir_in(runtime)?;
        let address = format!("unix:dir={}", percent_encode_address(bus_runtime.path()));
        fs::write(
            runtime.join("bus.conf"),
            BUS_CONFIG.replace("SOCKET_ADDRESS", &address),
        )?;
        let mut command = Command::new("dbus-daemon");
        command
            .arg(format!(
                "--config-file={}",
                runtime.join("bus.conf").display()
            ))
            .args(["--fork", "--print-address=1", "--print-pid=1"]);
        daemon_environment(&mut command, profile, bus_runtime.path());
        let output = command.output().context("failed to execute dbus-daemon")?;
        ensure!(
            output.status.success(),
            "dbus-daemon failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        let stdout = String::from_utf8(output.stdout)?;
        let mut lines = stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty());
        let address = lines
            .next()
            .context("dbus-daemon did not print a bus address")?
            .to_owned();
        if let Some(pid) = lines.next() {
            fs::write(runtime.join(DBUS_PID_FILE), pid)?;
        }
        fs::write(
            runtime.join("control-root"),
            bus_runtime.path().as_os_str().as_encoded_bytes(),
        )?;
        fs::write(address_file, &address)?;
        let _ = bus_runtime.keep();
        Ok(address)
    }

    fn percent_encode_address(path: &Path) -> String {
        use std::fmt::Write as _;
        let mut encoded = String::new();
        for &byte in path.as_os_str().as_encoded_bytes() {
            if byte.is_ascii_alphanumeric() || b"/-._".contains(&byte) {
                encoded.push(char::from(byte));
            } else {
                let _ = write!(encoded, "%{byte:02X}");
            }
        }
        encoded
    }

    fn daemon_environment(command: &mut Command, profile: &ProfileEnvironment, runtime: &Path) {
        profile.apply_to_command(command);
        command
            .env("XDG_RUNTIME_DIR", runtime)
            .env_remove("GNOME_KEYRING_CONTROL")
            .env_remove(BUS_ENV);
    }

    fn unlock(
        profile: &ProfileEnvironment,
        runtime: &Path,
        address: &str,
        account: &str,
    ) -> Result<()> {
        let control_root = fs::read_to_string(runtime.join("control-root"))?;
        let mut password = read_password_from_tty(&format!(
            "Password for private Antigravity keyring '{account}' (choose one on first use): "
        ))?;

        let mut command = Command::new("gnome-keyring-daemon");
        daemon_environment(&mut command, profile, Path::new(&control_root));
        // Replace the locked daemon on this private bus so startup can unlock the on-disk collection.
        let child = command
            .args([
                "--replace",
                "--unlock",
                "--components=secrets",
                "--control-directory",
            ])
            .arg(Path::new(&control_root).join("keyring"))
            .env(BUS_ENV, address)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let written = child.and_then(|mut child| {
            let written = child
                .stdin
                .take()
                .expect("piped stdin")
                .write_all(&password);
            let status = child.wait();
            written.and(status)
        });
        password.fill(0);
        ensure!(
            written
                .context("failed to run gnome-keyring-daemon --unlock")?
                .success(),
            "keyring unlock failed"
        );
        Ok(())
    }

    struct RawModeGuard;

    impl RawModeGuard {
        fn enter() -> Result<Self> {
            crossterm::terminal::enable_raw_mode()
                .context("failed to enable raw terminal mode for password prompt")?;
            Ok(Self)
        }
    }

    impl Drop for RawModeGuard {
        fn drop(&mut self) {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }

    fn read_password_from_tty(prompt: &str) -> Result<Vec<u8>> {
        use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, read};

        let mut stderr = std::io::stderr();
        stderr.write_all(prompt.as_bytes())?;
        stderr.flush()?;

        let _raw = RawModeGuard::enter()?;
        let mut chars = String::new();
        loop {
            let Event::Key(key) = read().context("failed to read keyring password")? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Enter => {
                    let _ = stderr.write_all(b"\r\n");
                    let _ = stderr.flush();
                    break;
                }
                KeyCode::Char('c' | 'd') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let _ = stderr.write_all(b"\r\n");
                    let _ = stderr.flush();
                    let mut bytes = chars.into_bytes();
                    bytes.fill(0);
                    bail!("keyring password prompt cancelled");
                }
                KeyCode::Backspace => {
                    chars.pop();
                }
                KeyCode::Char(ch)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    chars.push(ch);
                }
                _ => {}
            }
        }
        let mut bytes = chars.into_bytes();
        if bytes.is_empty() {
            bytes.fill(0);
            bail!("keyring password cannot be empty");
        }
        Ok(bytes)
    }

    fn alias_path(response: &str) -> Result<&str> {
        response
            .strip_prefix("(objectpath '")
            .and_then(|path| path.strip_suffix("',)"))
            .context("unexpected Secret Service alias response")
    }

    fn collection_locked(address: &str, collection: &str) -> Result<bool> {
        match call(
            address,
            collection,
            "org.freedesktop.DBus.Properties.Get",
            &["org.freedesktop.Secret.Collection", "Locked"],
        )?
        .as_str()
        {
            "(<true>,)" => Ok(true),
            "(<false>,)" => Ok(false),
            _ => bail!("unexpected Secret Service Locked property"),
        }
    }

    fn call(address: &str, path: &str, method: &str, args: &[&str]) -> Result<String> {
        let destination = if path == "/org/freedesktop/DBus" {
            "org.freedesktop.DBus"
        } else {
            SECRET_SERVICE
        };
        let output = Command::new("gdbus")
            .args([
                "call",
                "--address",
                address,
                "--timeout",
                DBUS_TIMEOUT_SECONDS,
                "--dest",
                destination,
                "--object-path",
                path,
                "--method",
                method,
            ])
            .args(args)
            .output()
            .context("failed to execute gdbus")?;
        ensure!(
            output.status.success(),
            "Secret Service call {method} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    }
}

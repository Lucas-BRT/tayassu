mod cli;
mod doctor;
mod render;
mod routing;
mod shell;
mod systemd;
mod tui;

use anyhow::{Context, Result, bail};
use chrono::{TimeDelta, Utc};
use clap::{CommandFactory, Parser};
use clap_complete::CompleteEnv;
use cli::{AccountAction, Cli, Command, CooldownAction, ImportArgs};
use doctor::DoctorReport;
use render::StatusTable;
use routing::RunRequest;
use shell::ShellInit;
use std::collections::BTreeSet;
use std::env;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{self, Command as ProcessCommand, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use systemd::SystemdUnits;
use tayassu_core::{
    AccountConfig, AccountId, CooldownEntry, ModelBucket, ProbeFailure, ProviderKind,
};
use tayassu_providers::{
    AdapterRegistry, HostDiscovery, MigrationMode, ProbeOutput, ProviderError,
};
use tayassu_store::{AppConfig, RuntimeState, XdgPaths};
use tracing::warn;
use tracing_subscriber::EnvFilter;

/// Default tracing filter when `RUST_LOG` is unset.
const DEFAULT_LOG_FILTER: &str = "warn";

/// Fallback non-zero exit code when a child process is terminated by a signal.
const SIGNAL_FALLBACK_EXIT_CODE: i32 = 1;

/// Atomic flag set when SIGINT (Ctrl-C) is received, to trigger explicit session cleanup.
static RECEIVED_INT: AtomicBool = AtomicBool::new(false);

/// Installs a Ctrl-C handler that sets `RECEIVED_INT`.
/// On systems where ctrlc isn't available (e.g. some Windows configs), it gracefully no-ops.
fn install_ctrlc_handler() {
    let _ = ctrlc::set_handler(move || {
        RECEIVED_INT.store(true, Ordering::SeqCst);
    });
}

/// Maximum stderr bytes buffered in memory during `tayassu run` for passive rate-limit detection.
const MAX_CAPTURED_STDERR_BYTES: usize = 64 * 1024;

/// Chunk size in bytes used when forwarding child stderr to the user's terminal.
const STDERR_READ_CHUNK_BYTES: usize = 4096;

/// Arguments for dumping the host `opencode` credentials as JSON on stdout.
const OPENCODE_AUTH_EXPORT_ARGS: &[&str] = &["auth", "export"];

/// Arguments for reading exported `opencode` credentials from stdin.
const OPENCODE_AUTH_IMPORT_ARGS: &[&str] = &["auth", "import"];

/// Relative path under `$XDG_CONFIG_HOME` where `systemd --user` units live.
const SYSTEMD_USER_REL_DIR: &str = "systemd/user";

struct RunOutcome {
    status: ExitStatus,
    captured_stderr: String,
}

/// Returns whether a process with `pid` currently exists (Linux `/proc`).
fn process_is_alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

/// RAII guard tracking an active `tayassu run` session in `state.json` under an exclusive lock.
struct SessionGuard {
    state_file: PathBuf,
    account_id: AccountId,
}

impl SessionGuard {
    fn acquire(state_file: PathBuf, account_id: AccountId) -> Result<Self> {
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

struct App {
    paths: XdgPaths,
    config: AppConfig,
    state: RuntimeState,
}

impl App {
    fn load() -> Result<Self> {
        let paths = XdgPaths::from_env().context("failed to resolve XDG directories")?;
        let config = AppConfig::load(&paths.config_file)
            .with_context(|| format!("failed to load {}", paths.config_file.display()))?;
        let state = RuntimeState::load(&paths.state_file)
            .with_context(|| format!("failed to load {}", paths.state_file.display()))?;
        Ok(Self {
            paths,
            config,
            state,
        })
    }

    fn execute(&mut self, command: Command, out: &mut impl Write) -> Result<()> {
        match command {
            Command::Status { provider, model } => {
                let table = StatusTable::new(
                    &self.config.accounts,
                    &self.state.statuses,
                    provider,
                    model.as_ref(),
                );
                table
                    .write_to(out, Utc::now())
                    .context("failed to write status table")
            }
            Command::Dash => self.handle_dash(),
            Command::Run {
                account,
                provider,
                chain,
                model,
                policy,
                dry_run,
                failover,
                args,
            } => self.run_agent(
                &RunRequest {
                    account,
                    provider,
                    chain,
                    model,
                    policy,
                    dry_run,
                    failover,
                    args,
                },
                out,
            ),
            Command::Account { action } => {
                if matches!(
                    action,
                    AccountAction::Env { .. } | AccountAction::Login { .. }
                ) {
                    self.handle_account(action, out)
                } else {
                    self.with_current_config(|app| app.handle_account(action, out))
                }
            }
            Command::Cooldown { action } => self.handle_cooldown(action, out),
            Command::Probe { id, notify } => self.handle_probe(id.as_ref(), notify, out),
            Command::Import(args) if args.dry_run => self.handle_import(&args, out),
            Command::Import(args) => self.with_current_config(|app| app.handle_import(&args, out)),
            Command::ShellInit {
                shell,
                providers,
                failover,
            } => {
                let snippet = ShellInit::new(shell, providers, failover).render();
                write!(out, "{snippet}").context("failed to write shell-init snippet")
            }
            Command::SystemdUnit {
                interval_minutes,
                install,
            } => self.handle_systemd_unit(interval_minutes, install, out),
            Command::Doctor { strict } => self.handle_doctor(strict, out),
            Command::Update { provider, dry_run } => self.handle_update(provider, dry_run, out),
        }
    }

    fn with_current_config(
        &mut self,
        operation: impl FnOnce(&mut Self) -> Result<()>,
    ) -> Result<()> {
        AppConfig::with_current_locked(&self.paths.config_file.clone(), |config| {
            self.config = config;
            operation(self)
        })
    }

    fn handle_dash(&mut self) -> Result<()> {
        let paths = self.paths.clone();
        let accounts = self.config.accounts.clone();

        tui::run_dashboard(
            &accounts,
            &mut self.state,
            |state| {
                let (ok_count, err_count) = probe_accounts(&accounts, &paths, state, false, None)?;
                Ok(format!("Probe complete: {ok_count} ok, {err_count} failed"))
            },
            |state, account_id| {
                let (updated, cleared) = RuntimeState::update_locked(&paths.state_file, |st| {
                    let status = st.status_for_mut(account_id);
                    let count = status.cooldowns.len();
                    status.cooldowns.clear();
                    count
                })
                .with_context(|| format!("failed to save {}", paths.state_file.display()))?;
                *state = updated;
                Ok(format!("Cleared {cleared} cooldown(s) for '{account_id}'"))
            },
        )
    }

    fn handle_account(&mut self, action: AccountAction, out: &mut impl Write) -> Result<()> {
        match action {
            AccountAction::Add {
                id,
                provider,
                label,
                priority,
                binary,
            } => {
                let display_label = if label.is_empty() {
                    format!("{provider} ({id})")
                } else {
                    label
                };
                if self.config.find_account(&id).is_some() {
                    bail!("account '{id}' already exists; remove it first to re-register");
                }
                let mut account = AccountConfig::new(id.clone(), provider, display_label);
                account.priority = priority;
                account.binary_override = binary;

                let adapter = AdapterRegistry.adapter_for(provider);
                let env_profile = adapter.build_environment(&account, &self.paths.profiles_dir);
                env_profile
                    .prepare(&self.paths.home_dir)
                    .with_context(|| format!("failed to prepare profile for {id}"))?;

                self.config.upsert_account(account);
                self.save_config()?;
                writeln!(
                    out,
                    "Added account '{id}' ({provider}) at {}",
                    env_profile.profile_dir.display()
                )?;
            }
            AccountAction::Edit {
                id,
                label,
                priority,
            } => {
                let account = self
                    .config
                    .find_account_mut(&id)
                    .with_context(|| format!("account '{id}' not found"))?;
                if let Some(label) = label {
                    account.label = label;
                }
                if let Some(priority) = priority {
                    account.priority = priority;
                }
                self.save_config()?;
                writeln!(out, "Updated account '{id}'")?;
            }
            AccountAction::Enable { id } => self.set_account_enabled(&id, true, out)?,
            AccountAction::Disable { id } => self.set_account_enabled(&id, false, out)?,
            AccountAction::Remove { id } => {
                if !self.config.remove_account(&id) {
                    bail!("account '{id}' not found");
                }
                self.save_config()?;
                writeln!(out, "Removed account '{id}' from config")?;
            }
            AccountAction::Env { id } => {
                let account = self
                    .config
                    .find_account(&id)
                    .with_context(|| format!("account '{id}' not found"))?;
                let adapter = AdapterRegistry.adapter_for(account.provider);
                let env_profile = adapter.build_environment(account, &self.paths.profiles_dir);
                writeln!(out, "profile_dir={}", env_profile.profile_dir.display())?;
                for (key, value) in &env_profile.env_vars {
                    writeln!(out, "export {key}={value}")?;
                }
            }
            AccountAction::Login { id } => self.login_account(&id, out)?,
        }
        Ok(())
    }

    fn login_account(&mut self, id: &AccountId, out: &mut impl Write) -> Result<()> {
        let account = self
            .config
            .find_account(id)
            .with_context(|| format!("account '{id}' not found"))?
            .clone();
        let adapter = AdapterRegistry.adapter_for(account.provider);
        let env_profile = adapter.build_environment(&account, &self.paths.profiles_dir);
        env_profile
            .prepare(&self.paths.home_dir)
            .with_context(|| format!("failed to prepare profile for {id}"))?;

        writeln!(
            out,
            "Logging in '{id}' ({}) using isolated profile {}",
            account.provider,
            env_profile.profile_dir.display()
        )?;
        out.flush()?;
        let login_args = adapter.login_args();
        let executable = account_executable(&account, &self.paths.home_dir);
        spawn_interactive(&account, &executable, &env_profile.env_vars, &login_args)?;
        if self
            .state
            .status_for(id)
            .is_some_and(tayassu_core::AccountStatus::needs_login)
        {
            let (updated, ()) = RuntimeState::update_locked(&self.paths.state_file, |state| {
                let status = state.status_for_mut(id);
                if status.needs_login() {
                    status.clear_probe_failure();
                    status.last_probed_at = None;
                    status.windows.clear();
                }
            })
            .context("failed to reset probe status after login")?;
            self.state = updated;
        }
        Ok(())
    }

    fn set_account_enabled(
        &mut self,
        id: &AccountId,
        enabled: bool,
        out: &mut impl Write,
    ) -> Result<()> {
        self.config
            .find_account_mut(id)
            .with_context(|| format!("account '{id}' not found"))?
            .enabled = enabled;
        self.save_config()?;
        let state = if enabled { "Enabled" } else { "Disabled" };
        writeln!(out, "{state} account '{id}'")?;
        Ok(())
    }

    fn handle_cooldown(&mut self, action: CooldownAction, out: &mut impl Write) -> Result<()> {
        match action {
            CooldownAction::Set {
                id,
                minutes,
                model,
                reason,
            } => {
                if self.config.find_account(&id).is_none() {
                    bail!("account '{id}' not found");
                }
                let resets_at = i64::try_from(minutes)
                    .ok()
                    .and_then(TimeDelta::try_minutes)
                    .and_then(|delta| Utc::now().checked_add_signed(delta))
                    .context("cooldown duration exceeds supported range")?;
                let bucket = model.unwrap_or_else(ModelBucket::default_bucket);
                let bucket_for_lock = bucket.clone();
                let (updated, ()) = RuntimeState::update_locked(&self.paths.state_file, |state| {
                    state.status_for_mut(&id).set_cooldown(CooldownEntry {
                        bucket: bucket_for_lock,
                        resets_at,
                        reason,
                    });
                })
                .with_context(|| format!("failed to save {}", self.paths.state_file.display()))?;
                self.state = updated;
                writeln!(
                    out,
                    "Cooldown set for '{id}' (bucket '{bucket}') until {}",
                    resets_at.to_rfc3339()
                )?;
            }
            CooldownAction::Clear { id, model } => {
                let bucket = model.unwrap_or_else(ModelBucket::default_bucket);
                let bucket_for_lock = bucket.clone();
                let (updated, removed) =
                    RuntimeState::update_locked(&self.paths.state_file, |state| {
                        state.status_for_mut(&id).clear_cooldown(&bucket_for_lock)
                    })
                    .with_context(|| {
                        format!("failed to save {}", self.paths.state_file.display())
                    })?;
                self.state = updated;
                if removed {
                    writeln!(out, "Cleared cooldown for '{id}' (bucket '{bucket}')")?;
                } else {
                    writeln!(out, "No active cooldown for '{id}' (bucket '{bucket}')")?;
                }
            }
        }
        Ok(())
    }

    fn handle_probe(
        &mut self,
        id: Option<&AccountId>,
        notify: bool,
        out: &mut impl Write,
    ) -> Result<()> {
        let targets: Vec<AccountConfig> = match id {
            Some(target_id) => vec![
                self.config
                    .find_account(target_id)
                    .with_context(|| format!("account '{target_id}' not found"))?
                    .clone(),
            ],
            None => self.config.accounts.clone(),
        };

        if targets.is_empty() {
            writeln!(out, "No accounts configured to probe.")?;
            return Ok(());
        }

        probe_accounts(&targets, &self.paths, &mut self.state, notify, Some(out))?;
        Ok(())
    }

    fn handle_import(&mut self, args: &ImportArgs, out: &mut impl Write) -> Result<()> {
        let discovered: Vec<_> = HostDiscovery
            .discover_all(&self.paths.home_dir, env::var_os("PATH").as_deref())
            .into_iter()
            .filter(|agent| args.include.is_empty() || args.include.contains(&agent.provider))
            .filter(|agent| !args.exclude.contains(&agent.provider))
            .collect();

        if discovered.is_empty() {
            writeln!(out, "No matching installed AI CLI agents found.")?;
            return Ok(());
        }

        let mode = if args.move_auth {
            MigrationMode::MoveAuth
        } else {
            MigrationMode::Copy
        };

        for agent in &discovered {
            let existing = self.config.find_account(&agent.default_account_id);
            if let Some(existing) = existing {
                if existing.provider != agent.provider {
                    writeln!(
                        out,
                        "Skipping '{}': ID already belongs to {}",
                        existing.id, existing.provider
                    )?;
                    continue;
                }
                if !args.refresh {
                    writeln!(
                        out,
                        "Skipping '{}': already configured (use --refresh to copy credentials again)",
                        existing.id
                    )?;
                    continue;
                }
            }
            let account = existing.cloned().unwrap_or_else(|| {
                let mut account = AccountConfig::new(
                    agent.default_account_id.clone(),
                    agent.provider,
                    format!("{} (imported)", agent.provider),
                );
                account.priority = args.priority;
                account
            });

            let adapter = AdapterRegistry.adapter_for(agent.provider);
            let profile_env = adapter.build_environment(&account, &self.paths.profiles_dir);

            if args.dry_run {
                Self::write_import_preview(agent, out)?;
                continue;
            }

            let summary = agent
                .apply(&profile_env, &self.paths.home_dir, mode)
                .with_context(|| format!("failed to import {}", agent.provider))?;

            if existing.is_none() {
                self.config.upsert_account(account);
                self.save_config()?;
            }

            writeln!(
                out,
                "Imported '{}' ({}) -> {} ({} file(s) copied, {} host auth file(s) moved)",
                agent.default_account_id,
                agent.provider,
                profile_env.profile_dir.display(),
                summary.copied_files,
                summary.removed_source_files
            )?;
            writeln!(out, "  update-safety: {}", agent.update_safety_note)?;
            let credentials_transferred = agent.provider == ProviderKind::Opencode
                && match transfer_opencode_auth(&agent.binary_path, &profile_env.env_vars) {
                    Ok(()) => {
                        writeln!(out, "  credentials: exported from host and imported")?;
                        true
                    }
                    Err(err) => {
                        warn!(error = %err, "opencode credential transfer failed");
                        false
                    }
                };
            if summary.requires_relogin && !credentials_transferred {
                writeln!(
                    out,
                    "  next: run `tayassu account login {}`",
                    agent.default_account_id
                )?;
            }
        }

        Ok(())
    }

    fn write_import_preview(
        agent: &tayassu_providers::DiscoveredAgent,
        out: &mut impl Write,
    ) -> Result<()> {
        writeln!(
            out,
            "[dry-run] {} -> account '{}' (binary: {}, {} credential/config file(s) found)",
            agent.provider,
            agent.default_account_id,
            agent.binary_path.display(),
            agent.existing_file_count()
        )?;
        writeln!(out, "  update-safety: {}", agent.update_safety_note)?;
        if agent.requires_relogin {
            writeln!(
                out,
                "  note: run `tayassu account login {}` after import",
                agent.default_account_id
            )?;
        }
        Ok(())
    }

    fn handle_systemd_unit(
        &self,
        interval_minutes: u64,
        install: bool,
        out: &mut impl Write,
    ) -> Result<()> {
        let exe_path = env::current_exe().context("failed to resolve current tayassu binary")?;
        let units = SystemdUnits::new(exe_path, interval_minutes);

        if install {
            let config_base = self
                .paths
                .config_file
                .parent()
                .and_then(Path::parent)
                .context("failed to resolve XDG config root")?;
            let systemd_dir = config_base.join(SYSTEMD_USER_REL_DIR);
            let (svc, tmr) = units.install_to(&systemd_dir)?;
            writeln!(out, "Installed {}", svc.display())?;
            writeln!(out, "Installed {}", tmr.display())?;
            writeln!(
                out,
                "Enable with: systemctl --user daemon-reload && systemctl --user enable --now tayassu-probe.timer"
            )?;
        } else {
            let (svc, tmr) = units.render();
            writeln!(
                out,
                "# tayassu-probe.service\n{svc}\n# tayassu-probe.timer\n{tmr}"
            )?;
        }
        Ok(())
    }

    fn handle_doctor(&self, strict: bool, out: &mut impl Write) -> Result<()> {
        let report = DoctorReport::run(&self.config.accounts, &self.paths);
        report
            .write_to(out)
            .context("failed to write doctor report")?;
        if report.has_errors() || (strict && report.has_warnings_or_errors()) {
            bail!("doctor reported issues");
        }
        Ok(())
    }

    fn handle_update(
        &self,
        provider_filter: Option<ProviderKind>,
        dry_run: bool,
        out: &mut impl Write,
    ) -> Result<()> {
        let discovered: Vec<_> = HostDiscovery
            .discover_all(&self.paths.home_dir, env::var_os("PATH").as_deref())
            .into_iter()
            .filter(|agent| provider_filter.is_none_or(|p| agent.provider == p))
            .collect();

        if discovered.is_empty() {
            writeln!(out, "No installed AI CLI agents found to update.")?;
            return Ok(());
        }

        for agent in &discovered {
            let adapter = AdapterRegistry.adapter_for(agent.provider);
            let Some(args) = adapter.update_args() else {
                writeln!(
                    out,
                    "{}: no built-in self-updater (update via your package manager)",
                    agent.provider
                )?;
                continue;
            };

            if dry_run {
                writeln!(
                    out,
                    "[dry-run] {} -> {} {}",
                    agent.provider,
                    agent.binary_path.display(),
                    args.join(" ")
                )?;
                continue;
            }

            writeln!(
                out,
                "Updating {} via {} {}...",
                agent.provider,
                agent.binary_path.display(),
                args.join(" ")
            )?;
            out.flush()?;

            let status = ProcessCommand::new(&agent.binary_path)
                .args(&args)
                .env("HOME", &self.paths.home_dir)
                .status()
                .with_context(|| format!("failed to run updater for {}", agent.provider))?;

            if !status.success() {
                warn!(provider = %agent.provider, code = ?status.code(), "updater returned non-zero");
                writeln!(
                    out,
                    "{}: updater exited with status {:?}",
                    agent.provider,
                    status.code()
                )?;
            }
        }
        Ok(())
    }

    fn save_config(&self) -> Result<()> {
        self.config
            .save_atomic(&self.paths.config_file)
            .with_context(|| format!("failed to save {}", self.paths.config_file.display()))
    }
}

fn probe_accounts(
    targets: &[AccountConfig],
    paths: &XdgPaths,
    state: &mut RuntimeState,
    notify: bool,
    mut out: Option<&mut dyn Write>,
) -> Result<(usize, usize)> {
    let now = Utc::now();
    let profiles_dir = &paths.profiles_dir;
    let home_dir = &paths.home_dir;

    let previously_cooling: BTreeSet<(AccountId, ModelBucket)> = state
        .statuses
        .iter()
        .flat_map(|st| {
            st.cooldowns
                .iter()
                .map(move |cd| (st.account_id.clone(), cd.bucket.clone()))
        })
        .collect();

    let results: Vec<(AccountId, Result<ProbeOutput>)> = thread::scope(|scope| {
        let handles: Vec<_> = targets
            .iter()
            .map(|account| {
                let worker =
                    scope.spawn(move || run_single_probe(account, profiles_dir, home_dir, now));
                (account.id.clone(), worker)
            })
            .collect();

        handles
            .into_iter()
            .map(|(id, worker)| {
                (
                    id,
                    worker
                        .join()
                        .unwrap_or_else(|_| Err(anyhow::anyhow!("probe worker panicked"))),
                )
            })
            .collect()
    });

    let mut ok_count = 0usize;
    let mut err_count = 0usize;
    let mut cleared_notifications = Vec::new();

    let (updated_state, ()) = RuntimeState::update_locked(&paths.state_file, |locked_state| {
        for (account_id, outcome) in results {
            let status = locked_state.status_for_mut(&account_id);
            if status.last_probed_at.is_some_and(|last| last > now) {
                continue;
            }
            status.last_probed_at = Some(now);
            match outcome {
                Ok(parsed) => {
                    let window_count = parsed.windows.len();
                    let cooldown_count = parsed.cooldowns.len();
                    status.refresh_probe_windows(parsed.windows, parsed.cooldowns, now);
                    status.clear_probe_failure();
                    for (prev_id, prev_bucket) in &previously_cooling {
                        if prev_id == &account_id && status.active_cooldown(prev_bucket, now).is_none()
                        {
                            cleared_notifications.push((account_id.clone(), prev_bucket.clone()));
                        }
                    }
                    ok_count += 1;
                    if let Some(writer) = out.as_deref_mut() {
                        let _ = writeln!(
                            writer,
                            "{account_id}: ok ({window_count} windows, {cooldown_count} active cooldowns)"
                        );
                    }
                }
                Err(err) => {
                    err_count += 1;
                    let failure = if matches!(
                        err.downcast_ref::<ProviderError>(),
                        Some(ProviderError::AuthenticationRequired { .. })
                    ) {
                        ProbeFailure::authentication(err.to_string())
                    } else {
                        ProbeFailure::other(format!("{err:#}"))
                    };
                    status.set_probe_failure(failure);
                    warn!(account = %account_id, error = %format_args!("{err:#}"), "probe failed");
                    if let Some(writer) = out.as_deref_mut() {
                        let _ = writeln!(writer, "{account_id}: {err:#}");
                    }
                }
            }
        }
    })
    .with_context(|| format!("failed to save {}", paths.state_file.display()))?;

    *state = updated_state;

    if notify {
        for (account_id, bucket) in cleared_notifications {
            send_desktop_notification(
                "tayassu: cooldown cleared",
                &format!("Account '{account_id}' (bucket '{bucket}') is ready again."),
            );
        }
    }

    Ok((ok_count, err_count))
}

fn send_desktop_notification(title: &str, body: &str) {
    let _ = ProcessCommand::new("notify-send")
        .args([title, body])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Copies host `opencode` credentials (stored in its database, not a file) into an isolated profile.
///
/// The secrets flow through a pipe from `auth export` to `auth import` and never touch disk.
fn transfer_opencode_auth(binary: &Path, profile_env: &[(String, String)]) -> Result<()> {
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

fn account_executable(account: &AccountConfig, home_dir: &Path) -> PathBuf {
    let executable = account.executable();
    HostDiscovery
        .resolve_executable(executable, home_dir, env::var_os("PATH").as_deref())
        .unwrap_or_else(|| PathBuf::from(executable))
}

fn spawn_interactive(
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
fn spawn_capturing_stderr(
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
            executable.display(),
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

fn run_single_probe(
    account: &AccountConfig,
    profiles_dir: &Path,
    real_home: &Path,
    now: chrono::DateTime<Utc>,
) -> Result<ProbeOutput> {
    let adapter = AdapterRegistry.adapter_for(account.provider);
    let env_profile = adapter.build_environment(account, profiles_dir);
    env_profile
        .prepare(real_home)
        .with_context(|| format!("failed to prepare profile for {}", account.id))?;

    if let Some(local_result) = adapter.probe_local(account, &env_profile, now) {
        return local_result.map_err(Into::into);
    }

    let Some(spec) = adapter.probe_command() else {
        return adapter
            .parse_usage_output(account, "", now)
            .map_err(Into::into);
    };

    let executable = account_executable(account, real_home);
    let mut cmd = ProcessCommand::new(&executable);
    cmd.args(&spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in &env_profile.env_vars {
        cmd.env(key, value);
    }

    let output = cmd.output().with_context(|| {
        format!(
            "failed to run probe '{}' for account '{}'",
            executable.display(),
            account.id
        )
    })?;

    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.stderr.is_empty() {
        combined.push('\n');
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
    }

    let parsed = adapter.parse_usage_output(account, &combined, now);
    let reported_auth_failure = account.provider != ProviderKind::Opencode
        && matches!(parsed, Err(ProviderError::AuthenticationRequired { .. }));
    if !output.status.success() && !reported_auth_failure {
        bail!(
            "probe '{}' exited with {}",
            executable.display(),
            output.status
        );
    }
    parsed.map_err(Into::into)
}

fn main() -> Result<()> {
    init_tracing();
    install_ctrlc_handler();
    CompleteEnv::with_factory(Cli::command)
        .var(shell::COMPLETION_ENV)
        .complete();
    let cli = Cli::parse();
    let mut app = App::load()?;
    let mut stdout = io::stdout().lock();

    // Run the CLI; if SIGINT was received, perform explicit session cleanup before exiting.
    let result = app.execute(cli.command, &mut stdout);
    if RECEIVED_INT.load(Ordering::SeqCst) {
        // Attempt to clean up any pending session guard state.
        // The guard's Drop may have been skipped; we reset the session count here.
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

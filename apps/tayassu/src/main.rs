mod cli;
mod render;
mod tui;

use anyhow::{Context, Result, bail};
use chrono::{TimeDelta, Utc};
use clap::Parser;
use cli::{AccountAction, Cli, Command, CooldownAction};
use render::StatusTable;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Command as ProcessCommand, ExitStatus, Stdio};
use std::thread;
use tayassu_core::{
    AccountConfig, AccountId, AccountRouter, CooldownEntry, ModelBucket, ProviderKind,
    SelectionPolicy,
};
use tayassu_providers::{AdapterRegistry, ProbeOutput};
use tayassu_store::{AppConfig, RuntimeState, XdgPaths};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

/// Default tracing filter when `RUST_LOG` is unset.
const DEFAULT_LOG_FILTER: &str = "warn";

/// Fallback non-zero exit code when a child process is terminated by a signal.
const SIGNAL_FALLBACK_EXIT_CODE: i32 = 1;

/// Maximum stderr bytes buffered in memory during `tayassu run` for passive rate-limit detection.
const MAX_CAPTURED_STDERR_BYTES: usize = 64 * 1024;

/// Chunk size in bytes used when forwarding child stderr to the user's terminal.
const STDERR_READ_CHUNK_BYTES: usize = 4096;

/// Maximum attempts allowed when `--failover` is enabled (primary + 1 fallback account).
const MAX_FAILOVER_ATTEMPTS: usize = 2;

struct RunRequest {
    provider: ProviderKind,
    model: Option<ModelBucket>,
    policy: Option<SelectionPolicy>,
    dry_run: bool,
    failover: bool,
    args: Vec<String>,
}

struct RunOutcome {
    status: ExitStatus,
    captured_stderr: String,
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
                provider,
                model,
                policy,
                dry_run,
                failover,
                args,
            } => self.run_agent(
                &RunRequest {
                    provider,
                    model,
                    policy,
                    dry_run,
                    failover,
                    args,
                },
                out,
            ),
            Command::Account { action } => self.handle_account(action, out),
            Command::Cooldown { action } => self.handle_cooldown(action, out),
            Command::Probe { id } => self.handle_probe(id.as_ref(), out),
        }
    }

    fn handle_dash(&mut self) -> Result<()> {
        let paths = self.paths.clone();
        let accounts = self.config.accounts.clone();

        tui::run_dashboard(
            &accounts,
            &mut self.state,
            |state| {
                let (ok_count, err_count) = probe_accounts(&accounts, &paths, state, None)?;
                Ok(format!("Probe complete: {ok_count} ok, {err_count} failed"))
            },
            |state, account_id| {
                let status = state.status_for_mut(account_id);
                let cleared = status.cooldowns.len();
                status.cooldowns.clear();
                state
                    .save_atomic(&paths.state_file)
                    .with_context(|| format!("failed to save {}", paths.state_file.display()))?;
                Ok(format!("Cleared {cleared} cooldown(s) for '{account_id}'"))
            },
        )
    }

    fn run_agent(&mut self, req: &RunRequest, out: &mut impl Write) -> Result<()> {
        let bucket = req.model.clone().unwrap_or_default();
        let router = AccountRouter::new(req.policy.unwrap_or(self.config.default_policy));
        let max_attempts = if req.failover {
            MAX_FAILOVER_ATTEMPTS
        } else {
            1
        };

        for attempt in 1..=max_attempts {
            let account = router
                .select(
                    &self.config.accounts,
                    &self.state.statuses,
                    req.provider,
                    &bucket,
                    Utc::now(),
                )
                .context("cannot select an account for this run")?
                .clone();

            let adapter = AdapterRegistry.adapter_for(account.provider);
            let env = adapter.build_environment(&account, &self.paths.profiles_dir);
            env.prepare(&self.paths.home_dir)
                .with_context(|| format!("failed to prepare profile for {}", account.id))?;

            if req.dry_run {
                writeln!(out, "account: {}", account.id)?;
                writeln!(out, "executable: {}", account.executable())?;
                for (key, value) in &env.env_vars {
                    writeln!(out, "env: {key}={value}")?;
                }
                if !req.args.is_empty() {
                    writeln!(out, "args: {}", req.args.join(" "))?;
                }
                return Ok(());
            }

            info!(account = %account.id, executable = account.executable(), "launching agent CLI");
            let outcome = spawn_capturing_stderr(&account, &env.env_vars, &req.args)?;

            let rate_limit_hit =
                adapter.detect_rate_limit(&account, &bucket, &outcome.captured_stderr, Utc::now());

            if let Some(cooldown) = rate_limit_hit {
                let resets_at = cooldown.resets_at;
                self.state
                    .status_for_mut(&account.id)
                    .set_cooldown(cooldown);
                self.save_state()?;
                warn!(
                    account = %account.id,
                    bucket = %bucket,
                    resets_at = %resets_at.to_rfc3339(),
                    "rate limit detected; cooldown recorded"
                );
                let _ = writeln!(
                    io::stderr(),
                    "tayassu: rate limit detected on '{}' (bucket '{}'); cooldown set until {}",
                    account.id,
                    bucket,
                    resets_at.to_rfc3339()
                );

                if req.failover && attempt < max_attempts {
                    let _ = writeln!(
                        io::stderr(),
                        "tayassu: failing over to next available {} account...",
                        req.provider
                    );
                    continue;
                }
            }

            if !outcome.status.success() {
                let code = outcome.status.code().unwrap_or(SIGNAL_FALLBACK_EXIT_CODE);
                bail!(
                    "'{}' (account '{}') exited with status {code}",
                    account.executable(),
                    account.id
                );
            }
            return Ok(());
        }

        Ok(())
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
                let mut account = AccountConfig::new(id.clone(), provider, display_label);
                account.priority = priority;
                account.binary_override = binary;

                let adapter = AdapterRegistry.adapter_for(provider);
                let env = adapter.build_environment(&account, &self.paths.profiles_dir);
                env.prepare(&self.paths.home_dir)
                    .with_context(|| format!("failed to prepare profile for {id}"))?;

                self.config.upsert_account(account);
                self.save_config()?;
                writeln!(
                    out,
                    "Added account '{id}' ({provider}) at {}",
                    env.profile_dir.display()
                )?;
            }
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
                let env = adapter.build_environment(account, &self.paths.profiles_dir);
                writeln!(out, "profile_dir={}", env.profile_dir.display())?;
                for (key, value) in &env.env_vars {
                    writeln!(out, "export {key}={value}")?;
                }
            }
            AccountAction::Login { id } => {
                let account = self
                    .config
                    .find_account(&id)
                    .with_context(|| format!("account '{id}' not found"))?
                    .clone();
                let adapter = AdapterRegistry.adapter_for(account.provider);
                let env = adapter.build_environment(&account, &self.paths.profiles_dir);
                env.prepare(&self.paths.home_dir)
                    .with_context(|| format!("failed to prepare profile for {id}"))?;

                writeln!(
                    out,
                    "Logging in '{id}' ({}) using isolated profile {}",
                    account.provider,
                    env.profile_dir.display()
                )?;
                out.flush()?;
                let login_args = adapter.login_args();
                spawn_interactive(&account, &env.env_vars, &login_args)?;
            }
        }
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
                let minutes_i64 =
                    i64::try_from(minutes).context("cooldown duration exceeds supported range")?;
                let bucket = model.unwrap_or_else(ModelBucket::default_bucket);
                let resets_at = Utc::now() + TimeDelta::minutes(minutes_i64);
                self.state.status_for_mut(&id).set_cooldown(CooldownEntry {
                    bucket: bucket.clone(),
                    resets_at,
                    reason,
                });
                self.save_state()?;
                writeln!(
                    out,
                    "Cooldown set for '{id}' (bucket '{bucket}') until {}",
                    resets_at.to_rfc3339()
                )?;
            }
            CooldownAction::Clear { id, model } => {
                let bucket = model.unwrap_or_else(ModelBucket::default_bucket);
                if self.state.status_for_mut(&id).clear_cooldown(&bucket) {
                    self.save_state()?;
                    writeln!(out, "Cleared cooldown for '{id}' (bucket '{bucket}')")?;
                } else {
                    writeln!(out, "No active cooldown for '{id}' (bucket '{bucket}')")?;
                }
            }
        }
        Ok(())
    }

    fn handle_probe(&mut self, id: Option<&AccountId>, out: &mut impl Write) -> Result<()> {
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

        probe_accounts(&targets, &self.paths, &mut self.state, Some(out))?;
        Ok(())
    }

    fn save_config(&self) -> Result<()> {
        self.config
            .save_atomic(&self.paths.config_file)
            .with_context(|| format!("failed to save {}", self.paths.config_file.display()))
    }

    fn save_state(&self) -> Result<()> {
        self.state
            .save_atomic(&self.paths.state_file)
            .with_context(|| format!("failed to save {}", self.paths.state_file.display()))
    }
}

fn probe_accounts(
    targets: &[AccountConfig],
    paths: &XdgPaths,
    state: &mut RuntimeState,
    mut out: Option<&mut dyn Write>,
) -> Result<(usize, usize)> {
    let now = Utc::now();
    let profiles_dir = &paths.profiles_dir;
    let home_dir = &paths.home_dir;

    let results: Vec<(AccountId, Result<ProbeOutput>)> = thread::scope(|scope| {
        let handles: Vec<_> = targets
            .iter()
            .map(|account| {
                scope.spawn(move || {
                    let res = run_single_probe(account, profiles_dir, home_dir, now);
                    (account.id.clone(), res)
                })
            })
            .collect();

        handles
            .into_iter()
            .map(|handle| handle.join().unwrap_or_else(|_| bail_unknown_probe_panic()))
            .collect()
    });

    let mut ok_count = 0usize;
    let mut err_count = 0usize;
    for (account_id, outcome) in results {
        match outcome {
            Ok(parsed) => {
                let window_count = parsed.windows.len();
                let cooldown_count = parsed.cooldowns.len();
                let status = state.status_for_mut(&account_id);
                status.last_probed_at = Some(now);
                status.windows = parsed.windows;
                status.prune_expired(now);
                for cooldown in parsed.cooldowns {
                    status.set_cooldown(cooldown);
                }
                ok_count += 1;
                if let Some(writer) = out.as_deref_mut() {
                    writeln!(
                        writer,
                        "{account_id}: ok ({window_count} windows, {cooldown_count} active cooldowns)"
                    )?;
                }
            }
            Err(err) => {
                err_count += 1;
                warn!(account = %account_id, error = %err, "probe failed");
                if let Some(writer) = out.as_deref_mut() {
                    writeln!(writer, "{account_id}: {err}")?;
                }
            }
        }
    }

    if ok_count > 0 {
        state
            .save_atomic(&paths.state_file)
            .with_context(|| format!("failed to save {}", paths.state_file.display()))?;
    }

    Ok((ok_count, err_count))
}

fn spawn_interactive(
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
fn spawn_capturing_stderr(
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
                if captured.len() < MAX_CAPTURED_STDERR_BYTES {
                    let remaining = MAX_CAPTURED_STDERR_BYTES - captured.len();
                    let take = read_len.min(remaining);
                    captured.extend_from_slice(&chunk[..take]);
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

fn run_single_probe(
    account: &AccountConfig,
    profiles_dir: &Path,
    real_home: &Path,
    now: chrono::DateTime<Utc>,
) -> Result<ProbeOutput> {
    let adapter = AdapterRegistry.adapter_for(account.provider);
    let env = adapter.build_environment(account, profiles_dir);
    env.prepare(real_home)
        .with_context(|| format!("failed to prepare profile for {}", account.id))?;

    if let Some(local_result) = adapter.probe_local(account, &env, now) {
        return local_result.map_err(Into::into);
    }

    let Some(spec) = adapter.probe_command() else {
        return adapter
            .parse_usage_output(account, "", now)
            .map_err(Into::into);
    };

    let mut cmd = ProcessCommand::new(account.executable());
    cmd.args(&spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in &env.env_vars {
        cmd.env(key, value);
    }

    let output = cmd.output().with_context(|| {
        format!(
            "failed to run probe '{}' for account '{}'",
            account.executable(),
            account.id
        )
    })?;

    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.stderr.is_empty() {
        combined.push('\n');
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
    }

    adapter
        .parse_usage_output(account, &combined, now)
        .map_err(Into::into)
}

fn bail_unknown_probe_panic() -> (AccountId, Result<ProbeOutput>) {
    let fallback_id: AccountId = "unknown".parse().unwrap_or_else(|_| unreachable!());
    (fallback_id, Err(anyhow::anyhow!("probe worker panicked")))
}

fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();
    let mut app = App::load()?;
    let mut stdout = io::stdout().lock();
    app.execute(cli.command, &mut stdout)
}

fn init_tracing() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .init();
}

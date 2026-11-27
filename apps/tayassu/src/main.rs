mod cli;
mod doctor;
mod render;
mod shell;
mod systemd;
mod tui;

use anyhow::{Context, Result, bail};
use chrono::{TimeDelta, Utc};
use clap::Parser;
use cli::{AccountAction, Cli, Command, CooldownAction};
use doctor::DoctorReport;
use render::StatusTable;
use shell::ShellInit;
use std::collections::BTreeSet;
use std::env;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitStatus, Stdio};
use std::thread;
use systemd::SystemdUnits;
use tayassu_core::{
    AccountConfig, AccountId, AccountRouter, CooldownEntry, ModelBucket, ProviderKind,
    SelectionPolicy,
};
use tayassu_providers::{AdapterRegistry, HostDiscovery, MigrationMode, ProbeOutput};
use tayassu_store::{AppConfig, ProjectConfig, RuntimeState, XdgPaths};
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

/// CLI flags indicating the user is resuming an existing conversation in the current directory.
const RESUME_FLAGS: &[&str] = &["-c", "--continue", "-r", "--resume", "--conversation"];

/// Relative path under `$XDG_CONFIG_HOME` where `systemd --user` units live.
const SYSTEMD_USER_REL_DIR: &str = "systemd/user";

struct RunRequest {
    provider: Option<ProviderKind>,
    account: Option<AccountId>,
    chain: Option<String>,
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

/// RAII guard tracking an active `tayassu run` session in `state.json` under an exclusive lock.
struct SessionGuard {
    state_file: PathBuf,
    account_id: AccountId,
}

impl SessionGuard {
    fn acquire(state_file: PathBuf, account_id: AccountId) -> Result<Self> {
        let id = account_id.clone();
        RuntimeState::update_locked(&state_file, |state| {
            let entry = state.status_for_mut(&id);
            entry.active_sessions = entry.active_sessions.saturating_add(1);
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
            let entry = state.status_for_mut(&id);
            entry.active_sessions = entry.active_sessions.saturating_sub(1);
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
                provider,
                account,
                chain,
                model,
                policy,
                dry_run,
                failover,
                args,
            } => self.run_agent(
                &RunRequest {
                    provider,
                    account,
                    chain,
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
            Command::Probe { id, notify } => self.handle_probe(id.as_ref(), notify, out),
            Command::Import {
                include,
                exclude,
                dry_run,
                move_auth,
                priority,
            } => self.handle_import(&include, &exclude, dry_run, move_auth, priority, out),
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

    fn run_agent(&mut self, req: &RunRequest, out: &mut impl Write) -> Result<()> {
        let cwd = env::current_dir().context("failed to resolve current directory")?;
        let project_cfg = ProjectConfig::discover(&cwd)
            .context("failed to load .tayassu.toml")?
            .map(|(_, cfg)| cfg);

        let effective_policy = req
            .policy
            .or_else(|| project_cfg.as_ref().and_then(|cfg| cfg.default_policy))
            .unwrap_or(self.config.default_policy);
        let router = AccountRouter::new(effective_policy);
        let max_attempts = if req.failover {
            MAX_FAILOVER_ATTEMPTS
        } else {
            1
        };

        for attempt in 1..=max_attempts {
            let (account, bucket) =
                self.resolve_run_target(req, project_cfg.as_ref(), &cwd, router)?;

            let adapter = AdapterRegistry.adapter_for(account.provider);
            let env_profile = adapter.build_environment(&account, &self.paths.profiles_dir);
            env_profile
                .prepare(&self.paths.home_dir)
                .with_context(|| format!("failed to prepare profile for {}", account.id))?;

            if req.dry_run {
                writeln!(out, "account: {}", account.id)?;
                writeln!(out, "executable: {}", account.executable())?;
                for (key, value) in &env_profile.env_vars {
                    writeln!(out, "env: {key}={value}")?;
                }
                if !req.args.is_empty() {
                    writeln!(out, "args: {}", req.args.join(" "))?;
                }
                return Ok(());
            }

            let (updated_state, ()) =
                RuntimeState::update_locked(&self.paths.state_file, |state| {
                    state.record_cwd_account(&cwd, account.provider, account.id.clone());
                })
                .with_context(|| format!("failed to save {}", self.paths.state_file.display()))?;
            self.state = updated_state;

            let _session_guard =
                SessionGuard::acquire(self.paths.state_file.clone(), account.id.clone())?;

            info!(account = %account.id, executable = account.executable(), "launching agent CLI");
            let outcome = spawn_capturing_stderr(&account, &env_profile.env_vars, &req.args)?;

            let rate_limit_hit =
                adapter.detect_rate_limit(&account, &bucket, &outcome.captured_stderr, Utc::now());

            if let Some(cooldown) = rate_limit_hit {
                let resets_at = cooldown.resets_at;
                let account_id = account.id.clone();
                let (updated, ()) = RuntimeState::update_locked(&self.paths.state_file, |state| {
                    state.status_for_mut(&account_id).set_cooldown(cooldown);
                })
                .with_context(|| format!("failed to save {}", self.paths.state_file.display()))?;
                self.state = updated;

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
                        "tayassu: failing over to next available account..."
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

    fn resolve_run_target(
        &self,
        req: &RunRequest,
        project_cfg: Option<&ProjectConfig>,
        cwd: &Path,
        router: AccountRouter,
    ) -> Result<(AccountConfig, ModelBucket)> {
        let now = Utc::now();

        if let Some(ref forced_id) = req.account {
            let account = self
                .config
                .find_account(forced_id)
                .with_context(|| format!("account '{forced_id}' not found"))?
                .clone();
            return Ok((account, req.model.clone().unwrap_or_default()));
        }

        let chain_name = req.chain.as_deref().or_else(|| {
            if req.provider.is_none() {
                project_cfg.and_then(|cfg| cfg.fallback_chain.as_deref())
            } else {
                None
            }
        });

        if let Some(name) = chain_name {
            let chain = self
                .config
                .fallback_chains
                .get(name)
                .with_context(|| format!("fallback chain '{name}' not found in config.toml"))?;
            let (account, bucket) = router
                .select_from_chain(&self.config.accounts, &self.state.statuses, chain, now)
                .with_context(|| format!("no available account in fallback chain '{name}'"))?;
            return Ok((account.clone(), bucket));
        }

        let provider = req
            .provider
            .context("specify a provider, `--account <id>`, or `--chain <name>`")?;
        let bucket = req.model.clone().unwrap_or_default();

        if is_resume_invocation(&req.args)
            && let Some(last_id) = self.state.last_cwd_account(cwd, provider)
            && let Some(account) = self.config.find_account(last_id)
        {
            if let Some(status) = self.state.status_for(last_id)
                && !status.is_available(&bucket, now)
            {
                warn!(
                    account = %last_id,
                    "resuming conversation on account currently in cooldown"
                );
            }
            return Ok((account.clone(), bucket));
        }

        if let Some(preferred_id) = project_cfg.and_then(|cfg| cfg.preferred_account_for(provider))
            && let Some(account) = self.config.find_account(preferred_id)
            && account.enabled
            && account.provider == provider
            && self
                .state
                .status_for(preferred_id)
                .is_none_or(|st| st.is_available(&bucket, now))
        {
            return Ok((account.clone(), bucket));
        }

        let account = router
            .select(
                &self.config.accounts,
                &self.state.statuses,
                provider,
                &bucket,
                now,
            )
            .context("cannot select an account for this run")?
            .clone();
        Ok((account, bucket))
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
            AccountAction::Login { id } => {
                let account = self
                    .config
                    .find_account(&id)
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
                spawn_interactive(&account, &env_profile.env_vars, &login_args)?;
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

    fn handle_import(
        &mut self,
        include: &[ProviderKind],
        exclude: &[ProviderKind],
        dry_run: bool,
        move_auth: bool,
        priority: u8,
        out: &mut impl Write,
    ) -> Result<()> {
        let discovered: Vec<_> = HostDiscovery
            .discover_all(&self.paths.home_dir, env::var_os("PATH").as_deref())
            .into_iter()
            .filter(|agent| include.is_empty() || include.contains(&agent.provider))
            .filter(|agent| !exclude.contains(&agent.provider))
            .collect();

        if discovered.is_empty() {
            writeln!(out, "No matching installed AI CLI agents found.")?;
            return Ok(());
        }

        let mode = if move_auth {
            MigrationMode::MoveAuth
        } else {
            MigrationMode::Copy
        };

        let mut imported_any = false;
        for agent in &discovered {
            let mut account = AccountConfig::new(
                agent.default_account_id.clone(),
                agent.provider,
                format!("{} (imported)", agent.provider),
            );
            account.priority = priority;

            let adapter = AdapterRegistry.adapter_for(agent.provider);
            let profile_env = adapter.build_environment(&account, &self.paths.profiles_dir);

            if dry_run {
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
                continue;
            }

            let summary = agent
                .apply(&profile_env, &self.paths.home_dir, mode)
                .with_context(|| format!("failed to import {}", agent.provider))?;

            self.config.upsert_account(account);
            imported_any = true;

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
            if summary.requires_relogin {
                writeln!(
                    out,
                    "  next: run `tayassu account login {}`",
                    agent.default_account_id
                )?;
            }
        }

        if imported_any {
            self.save_config()?;
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

fn is_resume_invocation(args: &[String]) -> bool {
    args.iter().any(|arg| {
        RESUME_FLAGS
            .iter()
            .any(|flag| arg == flag || arg.starts_with(&format!("{flag}=")))
    })
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
    let mut cleared_notifications = Vec::new();

    let (updated_state, ()) = RuntimeState::update_locked(&paths.state_file, |locked_state| {
        for (account_id, outcome) in results {
            match outcome {
                Ok(parsed) => {
                    let window_count = parsed.windows.len();
                    let cooldown_count = parsed.cooldowns.len();
                    let status = locked_state.status_for_mut(&account_id);
                    status.last_probed_at = Some(now);
                    status.windows = parsed.windows;
                    status.prune_expired(now);
                    for cooldown in parsed.cooldowns {
                        status.set_cooldown(cooldown);
                    }
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
                    warn!(account = %account_id, error = %err, "probe failed");
                    if let Some(writer) = out.as_deref_mut() {
                        let _ = writeln!(writer, "{account_id}: {err}");
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

    let mut cmd = ProcessCommand::new(account.executable());
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

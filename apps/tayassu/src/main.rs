mod cli;
mod render;

use anyhow::{Context, Result, bail};
use chrono::{TimeDelta, Utc};
use clap::Parser;
use cli::{AccountAction, Cli, Command, CooldownAction};
use render::StatusTable;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command as ProcessCommand, Stdio};
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
            Command::Run {
                provider,
                model,
                policy,
                dry_run,
                args,
            } => self.run_agent(provider, model, policy, dry_run, &args, out),
            Command::Account { action } => self.handle_account(action, out),
            Command::Cooldown { action } => self.handle_cooldown(action, out),
            Command::Probe { id } => self.handle_probe(id.as_ref(), out),
        }
    }

    fn run_agent(
        &self,
        provider: ProviderKind,
        model: Option<ModelBucket>,
        policy: Option<SelectionPolicy>,
        dry_run: bool,
        args: &[String],
        out: &mut impl Write,
    ) -> Result<()> {
        let bucket = model.unwrap_or_default();
        let router = AccountRouter::new(policy.unwrap_or(self.config.default_policy));
        let account = router
            .select(
                &self.config.accounts,
                &self.state.statuses,
                provider,
                &bucket,
                Utc::now(),
            )
            .context("cannot select an account for this run")?;

        let adapter = AdapterRegistry.adapter_for(account.provider);
        let env = adapter.build_environment(account, &self.paths.profiles_dir);
        env.prepare(&self.paths.home_dir)
            .with_context(|| format!("failed to prepare profile for {}", account.id))?;

        if dry_run {
            writeln!(out, "account: {}", account.id)?;
            writeln!(out, "executable: {}", account.executable())?;
            for (key, value) in &env.env_vars {
                writeln!(out, "env: {key}={value}")?;
            }
            if !args.is_empty() {
                writeln!(out, "args: {}", args.join(" "))?;
            }
            return Ok(());
        }

        info!(account = %account.id, executable = account.executable(), "launching agent CLI");
        spawn_interactive(account, &env.env_vars, args)
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

        let now = Utc::now();
        let profiles_dir = &self.paths.profiles_dir;
        let home_dir = &self.paths.home_dir;

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

        let mut updated_any = false;
        for (account_id, outcome) in results {
            match outcome {
                Ok(parsed) => {
                    let window_count = parsed.windows.len();
                    let cooldown_count = parsed.cooldowns.len();
                    let status = self.state.status_for_mut(&account_id);
                    status.last_probed_at = Some(now);
                    status.windows = parsed.windows;
                    status.prune_expired(now);
                    for cooldown in parsed.cooldowns {
                        status.set_cooldown(cooldown);
                    }
                    updated_any = true;
                    writeln!(
                        out,
                        "{account_id}: ok ({window_count} windows, {cooldown_count} active cooldowns)"
                    )?;
                }
                Err(err) => {
                    warn!(account = %account_id, error = %err, "probe failed");
                    writeln!(out, "{account_id}: {err}")?;
                }
            }
        }

        if updated_any {
            self.save_state()?;
        }

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

fn run_single_probe(
    account: &AccountConfig,
    profiles_dir: &Path,
    real_home: &Path,
    now: chrono::DateTime<Utc>,
) -> Result<ProbeOutput> {
    let adapter = AdapterRegistry.adapter_for(account.provider);
    let Some(spec) = adapter.probe_command() else {
        return adapter
            .parse_usage_output(account, "", now)
            .map_err(Into::into);
    };

    let env = adapter.build_environment(account, profiles_dir);
    env.prepare(real_home)
        .with_context(|| format!("failed to prepare profile for {}", account.id))?;

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

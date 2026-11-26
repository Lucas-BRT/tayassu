mod cli;
mod render;

use anyhow::{Context, Result, bail};
use chrono::{TimeDelta, Utc};
use clap::Parser;
use cli::{AccountAction, Cli, Command, CooldownAction};
use render::StatusTable;
use std::io::{self, Write};
use std::process::Command as ProcessCommand;
use tayassu_core::{
    AccountConfig, AccountId, AccountRouter, CooldownEntry, ModelBucket, ProviderKind,
    SelectionPolicy,
};
use tayassu_providers::AdapterRegistry;
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
                let bucket = model.unwrap_or_default();
                let table = StatusTable::new(
                    &self.config.accounts,
                    &self.state.statuses,
                    provider,
                    &bucket,
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
        let mut cmd = ProcessCommand::new(account.executable());
        cmd.args(args);
        for (key, value) in &env.env_vars {
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

    fn handle_probe(&self, id: Option<&AccountId>, out: &mut impl Write) -> Result<()> {
        let targets: Vec<&AccountConfig> = match id {
            Some(target_id) => vec![
                self.config
                    .find_account(target_id)
                    .with_context(|| format!("account '{target_id}' not found"))?,
            ],
            None => self.config.accounts.iter().collect(),
        };
        if targets.is_empty() {
            writeln!(out, "No accounts configured to probe.")?;
            return Ok(());
        }
        for account in targets {
            let adapter = AdapterRegistry.adapter_for(account.provider);
            match adapter.parse_usage_output(account, "", Utc::now()) {
                Ok(_) => writeln!(out, "{}: ok", account.id)?,
                Err(err) => {
                    warn!(account = %account.id, error = %err, "probe skipped");
                    writeln!(out, "{}: {err}", account.id)?;
                }
            }
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

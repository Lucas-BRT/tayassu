use crate::app::App;
use crate::probing::probe_accounts;
use crate::session::{SessionGuard, process_is_alive};
use crate::spawn::{SIGNAL_FALLBACK_EXIT_CODE, account_executable, spawn_capturing_stderr};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::env;
use std::io::{self, Write};
use std::path::Path;
use std::process;
use tayassu_core::{
    AccountConfig, AccountId, AccountRouter, CooldownEntry, FallbackTarget, ModelBucket,
    ProviderKind, SelectionPolicy,
};
use tayassu_providers::AdapterRegistry;
use tayassu_store::{ProjectConfig, RuntimeState};
use tracing::{info, warn};

/// Maximum attempts allowed when `--failover` is enabled (primary + 1 fallback account).
const MAX_FAILOVER_ATTEMPTS: usize = 2;

/// CLI flags indicating the user is resuming an existing conversation in the current directory.
const RESUME_FLAGS: &[&str] = &["-c", "--continue", "-r", "--resume", "--conversation"];

pub(super) struct RunRequest {
    pub(super) account: Option<AccountId>,
    pub(super) provider: Option<ProviderKind>,
    pub(super) chain: Option<String>,
    pub(super) model: Option<ModelBucket>,
    pub(super) policy: Option<SelectionPolicy>,
    pub(super) dry_run: bool,
    pub(super) require_quota: bool,
    pub(super) failover: bool,
    pub(super) args: Vec<String>,
}

impl RunRequest {
    fn arguments_for(&self, provider: ProviderKind, bucket: &ModelBucket) -> Result<Vec<String>> {
        let use_model = self.require_quota
            && match provider {
                ProviderKind::Opencode => true,
                ProviderKind::Claude => matches!(bucket.as_str(), "opus" | "sonnet" | "haiku"),
                _ => false,
            };
        if !use_model {
            return Ok(self.args.clone());
        }
        anyhow::ensure!(
            !self.args.iter().any(|arg| arg == "-m"
                || arg == "--model"
                || arg.starts_with("--model=")
                || arg.starts_with("-m=")),
            "an explicit agent model conflicts with the model selected by --require-quota"
        );
        let mut args = self.args.clone();
        let index = usize::from(
            provider == ProviderKind::Opencode && args.first().is_some_and(|arg| arg == "run"),
        );
        args.splice(index..index, ["--model".to_owned(), bucket.to_string()]);
        Ok(args)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionReason {
    Forced,
    Chain,
    Resume,
    ProjectPreference,
    Policy,
}

impl std::fmt::Display for SelectionReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Forced => "explicit account",
            Self::Chain => "first available fallback target, ranked by policy",
            Self::Resume => "previous account for this conversation",
            Self::ProjectPreference => "preferred account from .tayassu.toml",
            Self::Policy => "highest-ranked available account",
        })
    }
}

impl App {
    pub(crate) fn run_agent(&mut self, req: &RunRequest, out: &mut impl Write) -> Result<()> {
        let cwd = env::current_dir().context("failed to resolve current directory")?;
        let project_cfg = ProjectConfig::discover(&cwd)
            .context("failed to load .tayassu.toml")?
            .map(|(_, cfg)| cfg);

        let effective_policy = req
            .policy
            .or_else(|| project_cfg.as_ref().and_then(|cfg| cfg.default_policy))
            .unwrap_or(self.config.default_policy);
        let router = AccountRouter::new(effective_policy).require_quota(req.require_quota);
        let max_attempts = if req.failover {
            MAX_FAILOVER_ATTEMPTS
        } else {
            1
        };

        if req.dry_run {
            return self.preview_run(req, project_cfg.as_ref(), &cwd, out, effective_policy);
        }
        if req.require_quota {
            self.refresh_routing_quota(req, project_cfg.as_ref())?;
        }
        self.prune_stale_sessions()?;

        let mut failed_account = None;
        for attempt in 1..=max_attempts {
            let candidates;
            let accounts = if let Some((id, _)) = &failed_account {
                candidates = self
                    .config
                    .accounts
                    .iter()
                    .filter(|account| &account.id != id)
                    .cloned()
                    .collect::<Vec<_>>();
                candidates.as_slice()
            } else {
                self.config.accounts.as_slice()
            };

            let selected =
                self.resolve_run_target(req, project_cfg.as_ref(), &cwd, router, accounts);
            let (account, bucket, reason) = match (selected, &failed_account) {
                (Err(error), Some((_, code))) => {
                    let _ = writeln!(io::stderr(), "tayassu: no fallback account: {error:#}");
                    process::exit(*code);
                }
                (result, _) => result?,
            };

            let adapter = AdapterRegistry.adapter_for(account.provider);
            let env_profile = adapter.build_environment(&account, &self.paths.profiles_dir);
            env_profile
                .prepare(&self.paths.home_dir)
                .with_context(|| format!("failed to prepare profile for {}", account.id))?;

            let (updated_state, ()) =
                RuntimeState::update_locked(&self.paths.state_file, |state| {
                    state.record_cwd_account(&cwd, account.provider, account.id.clone());
                })
                .with_context(|| format!("failed to save {}", self.paths.state_file.display()))?;
            self.state = updated_state;

            let session_guard =
                SessionGuard::acquire(self.paths.state_file.clone(), account.id.clone())?;

            let executable = account_executable(&account, &self.paths.home_dir);
            info!(account = %account.id, executable = %executable.display(), "launching agent CLI");
            let arguments = req.arguments_for(account.provider, &bucket)?;
            let outcome =
                spawn_capturing_stderr(&account, &executable, &env_profile.env_vars, &arguments)?;

            let exit_code = outcome.status.code().unwrap_or(SIGNAL_FALLBACK_EXIT_CODE);
            // A clean exit never signals a limit; stderr noise must not cool the account down.
            let rate_limit_hit = if outcome.status.success() {
                None
            } else {
                adapter.detect_rate_limit(&account, &bucket, &outcome.captured_stderr, Utc::now())
            };

            if let Some(cooldown) = rate_limit_hit {
                self.record_rate_limit(&account.id, cooldown)?;

                if req.failover && attempt < max_attempts && reason != SelectionReason::Resume {
                    failed_account = Some((account.id.clone(), exit_code));
                    let _ = writeln!(
                        io::stderr(),
                        "tayassu: failing over to next available account..."
                    );
                    continue;
                }
            }

            if !outcome.status.success() {
                // Release the session counter first: `exit` skips destructors.
                drop(session_guard);
                // Propagate the agent's own status so shell wrappers and scripts see it.
                process::exit(exit_code);
            }
            return Ok(());
        }

        Ok(())
    }

    /// Drops sessions left behind by `tayassu` processes that were killed before cleanup.
    fn refresh_routing_quota(
        &mut self,
        req: &RunRequest,
        project_cfg: Option<&ProjectConfig>,
    ) -> Result<()> {
        let targets = self.run_targets(req, project_cfg)?;
        let accounts: Vec<_> = self
            .config
            .accounts
            .iter()
            .filter(|account| {
                account.enabled
                    && targets
                        .iter()
                        .any(|target| target.provider == account.provider)
            })
            .cloned()
            .collect();
        if !accounts.is_empty() {
            probe_accounts(
                &accounts,
                &self.config.accounts,
                &self.paths,
                &mut self.state,
                false,
                None,
            )?;
        }
        Ok(())
    }

    fn record_rate_limit(&mut self, account: &AccountId, cooldown: CooldownEntry) -> Result<()> {
        let resets_at = cooldown.resets_at;
        let bucket = cooldown.bucket.clone();
        let (updated, ()) = RuntimeState::update_locked(&self.paths.state_file, |state| {
            state.status_for_mut(account).set_cooldown(cooldown);
        })
        .with_context(|| format!("failed to save {}", self.paths.state_file.display()))?;
        self.state = updated;

        warn!(%account, %bucket, resets_at = %resets_at.to_rfc3339(), "rate limit detected; cooldown recorded");
        let _ = writeln!(
            io::stderr(),
            "tayassu: rate limit detected on '{account}' (bucket '{bucket}'); cooldown set until {}",
            resets_at.to_rfc3339()
        );
        Ok(())
    }

    fn prune_stale_sessions(&mut self) -> Result<()> {
        if !self
            .state
            .statuses
            .iter()
            .any(|status| status.sessions_stale(process_is_alive))
        {
            return Ok(());
        }
        let (updated, ()) = RuntimeState::update_locked(&self.paths.state_file, |state| {
            for status in &mut state.statuses {
                status.prune_sessions(process_is_alive);
            }
        })
        .with_context(|| format!("failed to save {}", self.paths.state_file.display()))?;
        self.state = updated;
        Ok(())
    }

    fn preview_run(
        &self,
        req: &RunRequest,
        project_cfg: Option<&ProjectConfig>,
        cwd: &Path,
        out: &mut impl Write,
        policy: SelectionPolicy,
    ) -> Result<()> {
        let (account, bucket, reason) = self.resolve_run_target(
            req,
            project_cfg,
            cwd,
            AccountRouter::new(policy).require_quota(req.require_quota),
            &self.config.accounts,
        )?;
        self.write_routing_explanation(req, out, &account, (policy, reason), project_cfg)?;
        writeln!(out, "target: {}:{bucket}", account.provider)?;
        let executable = account_executable(&account, &self.paths.home_dir);
        writeln!(out, "executable: {}", executable.display())?;
        let adapter = AdapterRegistry.adapter_for(account.provider);
        let profile = adapter.build_environment(&account, &self.paths.profiles_dir);
        for (key, value) in &profile.env_vars {
            writeln!(out, "env: {key}={value}")?;
        }
        let arguments = req.arguments_for(account.provider, &bucket)?;
        if !arguments.is_empty() {
            writeln!(out, "args: {arguments:?}")?;
        }
        Ok(())
    }

    fn write_routing_explanation(
        &self,
        req: &RunRequest,
        out: &mut impl Write,
        account: &AccountConfig,
        (effective_policy, reason): (SelectionPolicy, SelectionReason),
        project_cfg: Option<&ProjectConfig>,
    ) -> Result<()> {
        writeln!(out, "account: {}", account.id)?;
        if req.require_quota {
            writeln!(out, "quota: last recorded measurement (dry-run)")?;
        }
        writeln!(out, "policy: {effective_policy}")?;
        writeln!(out, "selection: {reason}")?;
        if reason == SelectionReason::Chain
            && let Some(chain) = Self::chain_name(req, project_cfg)
        {
            writeln!(out, "chain: {chain}")?;
        }
        if let Some(preferred) = project_cfg
            .as_ref()
            .and_then(|cfg| cfg.preferred_account_for(account.provider))
        {
            writeln!(
                out,
                "project preference for {}: {preferred}",
                account.provider
            )?;
        }
        let targets = self.run_targets(req, project_cfg)?;
        for alternative in self
            .config
            .accounts
            .iter()
            .filter(|other| other.id != account.id)
        {
            let mut matching = targets
                .iter()
                .filter(|target| target.provider == alternative.provider)
                .peekable();
            if matching.peek().is_none() {
                writeln!(out, "skipped {}: outside routing targets", alternative.id)?;
            }
            for target in matching {
                let explanation = self
                    .routing_status(alternative, &target.bucket, Utc::now(), req.require_quota)
                    .unwrap_or_else(|| {
                        "eligible; selection precedence or policy favored the chosen account"
                            .to_owned()
                    });
                writeln!(out, "skipped {} ({target}): {explanation}", alternative.id)?;
            }
        }
        Ok(())
    }

    fn resolve_run_target(
        &self,
        req: &RunRequest,
        project_cfg: Option<&ProjectConfig>,
        cwd: &Path,
        router: AccountRouter,
        accounts: &[AccountConfig],
    ) -> Result<(AccountConfig, ModelBucket, SelectionReason)> {
        let now = Utc::now();

        if let Some(ref forced_id) = req.account {
            let account = self
                .config
                .find_account(forced_id)
                .with_context(|| format!("account '{forced_id}' not found"))?
                .clone();
            return Ok((
                account,
                req.model.clone().unwrap_or_default(),
                SelectionReason::Forced,
            ));
        }

        let targets = self.run_targets(req, project_cfg)?;
        if let Some(name) = Self::chain_name(req, project_cfg) {
            let (account, bucket) = router
                .select_from_chain(accounts, &self.state.statuses, &targets, now)
                .with_context(|| {
                    format!(
                        "no available account in fallback chain '{name}'; {}",
                        self.unavailable_summary(accounts, &targets, now, req.require_quota)
                    )
                })?;
            return Ok((account.clone(), bucket, SelectionReason::Chain));
        }

        let target = &targets[0];
        let provider = target.provider;
        let bucket = target.bucket.clone();

        if is_resume_invocation(&req.args)
            && let Some(last_id) = self.state.last_cwd_account(cwd, provider)
            && let Some(account) = accounts.iter().find(|account| &account.id == last_id)
        {
            if let Some(status) = self.state.status_for(last_id)
                && !status.is_available(&bucket, now)
            {
                warn!(
                    account = %last_id,
                    "resuming conversation on account currently in cooldown"
                );
            }
            if req.require_quota
                && (router
                    .resolve_bucket(provider, self.state.status_for(last_id), &bucket, now)
                    .as_ref()
                    != Some(&bucket))
            {
                anyhow::bail!(
                    "account '{last_id}' owns this session but has no confirmed available quota"
                );
            }
            if !req.dry_run {
                let _ = writeln!(io::stderr(), "tayassu: resuming on account '{last_id}'");
            }
            return Ok((account.clone(), bucket, SelectionReason::Resume));
        }

        if let Some(preferred_id) = project_cfg.and_then(|cfg| cfg.preferred_account_for(provider))
            && let Some(account) = accounts.iter().find(|account| &account.id == preferred_id)
            && account.enabled
            && account.provider == provider
            && let Some(bucket) =
                router.resolve_bucket(provider, self.state.status_for(preferred_id), &bucket, now)
        {
            return Ok((account.clone(), bucket, SelectionReason::ProjectPreference));
        }

        let account = router
            .select(accounts, &self.state.statuses, provider, &bucket, now)
            .with_context(|| self.unavailable_summary(accounts, &targets, now, req.require_quota))?
            .clone();
        let bucket = router
            .resolve_bucket(provider, self.state.status_for(&account.id), &bucket, now)
            .context("selected model has no confirmed available quota")?;
        Ok((account, bucket, SelectionReason::Policy))
    }

    fn chain_name<'a>(
        req: &'a RunRequest,
        project_cfg: Option<&'a ProjectConfig>,
    ) -> Option<&'a str> {
        req.chain.as_deref().or_else(|| {
            req.provider
                .is_none()
                .then(|| project_cfg.and_then(|cfg| cfg.fallback_chain.as_deref()))
                .flatten()
        })
    }

    fn run_targets(
        &self,
        req: &RunRequest,
        project_cfg: Option<&ProjectConfig>,
    ) -> Result<Vec<FallbackTarget>> {
        let bucket = req.model.clone().unwrap_or_default();
        if let Some(id) = &req.account {
            let account = self
                .config
                .find_account(id)
                .with_context(|| format!("account '{id}' not found"))?;
            return Ok(vec![FallbackTarget::new(account.provider, bucket)]);
        }
        if let Some(name) = Self::chain_name(req, project_cfg) {
            return self
                .config
                .fallback_chains
                .get(name)
                .cloned()
                .with_context(|| format!("fallback chain '{name}' not found in config.toml"));
        }
        let provider = req
            .provider
            .context("specify a provider, `--account <id>`, or `--chain <name>`")?;
        Ok(vec![FallbackTarget::new(provider, bucket)])
    }

    fn routing_status(
        &self,
        account: &AccountConfig,
        bucket: &ModelBucket,
        now: DateTime<Utc>,
        require_quota: bool,
    ) -> Option<String> {
        if !account.enabled {
            return Some("disabled".to_owned());
        }
        let status = self.state.status_for(&account.id);
        let router = AccountRouter::new(SelectionPolicy::default()).require_quota(require_quota);
        let unknown = || "available quota could not be confirmed".to_owned();
        let Some(status) = status else {
            return require_quota.then(unknown);
        };
        if status.needs_login() {
            return Some(format!(
                "login required (tayassu account login {})",
                account.id
            ));
        }
        if status.is_available(bucket, now) {
            return router
                .resolve_bucket(account.provider, Some(status), bucket, now)
                .is_none()
                .then(unknown);
        }
        Some(status.next_available_at(bucket, now).map_or_else(
            || "quota exhausted; reset time unknown".to_owned(),
            |reset| format!("blocked until {}", reset.to_rfc3339()),
        ))
    }

    fn unavailable_summary(
        &self,
        accounts: &[AccountConfig],
        targets: &[FallbackTarget],
        now: DateTime<Utc>,
        require_quota: bool,
    ) -> String {
        let mut reasons = Vec::new();
        let mut next_release = None;
        for target in targets {
            for account in accounts
                .iter()
                .filter(|account| account.provider == target.provider)
            {
                if let Some(reason) =
                    self.routing_status(account, &target.bucket, now, require_quota)
                {
                    reasons.push(format!("{} ({target}): {reason}", account.id));
                }
                if account.enabled
                    && let Some(reset) = self
                        .state
                        .status_for(&account.id)
                        .filter(|status| !status.needs_login())
                        .and_then(|status| status.next_available_at(&target.bucket, now))
                {
                    next_release =
                        Some(next_release.map_or(reset, |known: DateTime<Utc>| known.min(reset)));
                }
            }
        }
        if reasons.is_empty() {
            return "no accounts configured for the requested targets".to_owned();
        }
        if let Some(reset) = next_release {
            reasons.push(format!("next known availability: {}", reset.to_rfc3339()));
        }
        reasons.join("; ")
    }
}

fn is_resume_invocation(args: &[String]) -> bool {
    args.iter().any(|arg| {
        RESUME_FLAGS
            .iter()
            .any(|flag| arg == flag || arg.starts_with(&format!("{flag}=")))
    })
}

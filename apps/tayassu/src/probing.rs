use crate::claude_usage;
use crate::opencode_usage;
use crate::quota_rpc;
use crate::spawn::account_executable;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use std::collections::BTreeSet;
use std::io::Write;
use std::process::{Command as ProcessCommand, Stdio};
use std::thread;
use tayassu_core::{
    AccountConfig, AccountId, AccountStatus, ModelBucket, ProbeFailure, ProviderKind,
};
use tayassu_providers::{AdapterRegistry, ProbeOutput, ProviderError};
use tayassu_store::{RuntimeState, XdgPaths};
use tracing::warn;

fn auth_only_replaces_measurements(parsed: &ProbeOutput, status: &AccountStatus) -> bool {
    parsed.windows.iter().all(|w| w.remaining.is_none())
        && parsed.model_buckets.is_empty()
        && parsed.key_budget.is_none()
        && status.windows.iter().any(|w| w.remaining.is_some())
}

fn record_auth_only_failure(
    status: &mut AccountStatus,
    account_id: &AccountId,
    out: &mut Option<&mut dyn Write>,
) {
    let msg = "probe returned auth-only without measurements; keeping prior quota windows";
    status.set_probe_failure(ProbeFailure::other(msg));
    warn!(account = %account_id, "{msg}");
    if let Some(writer) = out.as_deref_mut() {
        let _ = writeln!(writer, "{account_id}: {msg}");
    }
}

fn apply_probe_success(
    status: &mut AccountStatus,
    parsed: ProbeOutput,
    account_id: &AccountId,
    previously_cooling: &BTreeSet<(AccountId, ModelBucket)>,
    now: DateTime<Utc>,
    out: &mut Option<&mut dyn Write>,
    cleared_notifications: &mut Vec<(AccountId, ModelBucket)>,
) {
    let window_count = parsed.windows.len();
    let cooldown_count = parsed.cooldowns.len();
    let assumed_resets: Vec<ModelBucket> = status
        .windows
        .iter()
        .filter(|window| window.has_reset_at(now))
        .map(|window| window.bucket.clone())
        .collect();
    status.last_probed_at = Some(now);
    status.refresh_probe_windows(parsed.windows, parsed.cooldowns, now);
    status.model_buckets = parsed.model_buckets;
    status.key_budget = parsed.key_budget;
    status.clear_probe_failure();
    if let Some(writer) = out.as_deref_mut() {
        for bucket in assumed_resets {
            let still_exhausted = status
                .windows
                .iter()
                .any(|w| w.bucket == bucket && w.is_exhausted_at(now));
            if still_exhausted {
                let _ = writeln!(
                    writer,
                    "{account_id}: estimated reset for '{bucket}' was wrong; provider reports it exhausted"
                );
            }
        }
    }
    for (prev_id, prev_bucket) in previously_cooling {
        if prev_id == account_id && status.active_cooldown(prev_bucket, now).is_none() {
            cleared_notifications.push((account_id.clone(), prev_bucket.clone()));
        }
    }
    if let Some(writer) = out.as_deref_mut() {
        let _ = writeln!(
            writer,
            "{account_id}: ok ({window_count} windows, {cooldown_count} active cooldowns)"
        );
    }
}

pub fn probe_accounts(
    targets: &[AccountConfig],
    configured_accounts: &[AccountConfig],
    paths: &XdgPaths,
    state: &mut RuntimeState,
    notify: bool,
    mut out: Option<&mut dyn Write>,
) -> Result<(usize, usize)> {
    let now = Utc::now();

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
                    scope.spawn(move || run_single_probe(account, configured_accounts, paths, now));
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
    let mut auth_failures = 0usize;
    let mut cleared_notifications = Vec::new();

    let (updated_state, ()) = RuntimeState::update_locked(&paths.state_file, |locked_state| {
        for (account_id, outcome) in results {
            let status = locked_state.status_for_mut(&account_id);
            if status.last_probed_at.is_some_and(|last| last > now) {
                continue;
            }
            match outcome {
                Ok(parsed) if auth_only_replaces_measurements(&parsed, status) => {
                    err_count += 1;
                    record_auth_only_failure(status, &account_id, &mut out);
                }
                Ok(parsed) => {
                    apply_probe_success(
                        status,
                        parsed,
                        &account_id,
                        &previously_cooling,
                        now,
                        &mut out,
                        &mut cleared_notifications,
                    );
                    ok_count += 1;
                }
                Err(err) => {
                    err_count += 1;
                    let failure = classify_failure(&err);
                    if failure.is_authentication() || failure.is_locked() {
                        auth_failures += 1;
                    }
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

    if ok_count == 0
        && err_count > 0
        && auth_failures == 0
        && let Some(writer) = out
    {
        let _ = writeln!(
            writer,
            "all probes failed (offline?); keeping cached quota, run `tayassu ls` to review it"
        );
    }

    if notify {
        notify_cleared(&cleared_notifications);
    }

    Ok((ok_count, err_count))
}

fn classify_failure(err: &anyhow::Error) -> ProbeFailure {
    if matches!(
        err.downcast_ref::<ProviderError>(),
        Some(ProviderError::AuthenticationRequired { .. })
    ) {
        ProbeFailure::authentication(err.to_string())
    } else if err.chain().any(|cause| {
        cause
            .downcast_ref::<crate::agy_keyring::KeyringLockedError>()
            .is_some()
    }) {
        ProbeFailure::locked(format!("{err:#}"))
    } else {
        ProbeFailure::other(format!("{err:#}"))
    }
}

fn notify_cleared(cleared: &[(AccountId, ModelBucket)]) {
    for (account_id, bucket) in cleared {
        send_desktop_notification(
            "tayassu: cooldown cleared",
            &format!("Account '{account_id}' (bucket '{bucket}') is ready again."),
        );
    }
}

fn send_desktop_notification(title: &str, body: &str) {
    let _ = ProcessCommand::new("notify-send")
        .arg(title)
        .arg(body)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

pub(crate) fn run_single_probe(
    account: &AccountConfig,
    configured_accounts: &[AccountConfig],
    paths: &XdgPaths,
    now: DateTime<Utc>,
) -> Result<ProbeOutput> {
    let adapter = AdapterRegistry.adapter_for(account.provider);
    let env_profile =
        crate::spawn::prepare_profile(account, paths, crate::agy_keyring::Operation::Probe)?;

    let executable = account_executable(account, &paths.home_dir);
    if account.provider == ProviderKind::Opencode {
        match opencode_usage::probe(
            account,
            &executable,
            &env_profile,
            configured_accounts,
            paths,
        ) {
            Ok(probe) if !probe.windows.is_empty() || !probe.model_buckets.is_empty() => {
                return Ok(probe);
            }
            Ok(_) => {}
            Err(error) => {
                tracing::debug!(account = %account.id, %error, "structured OpenCode probe unavailable");
            }
        }
    }
    if matches!(
        account.provider,
        ProviderKind::Codex | ProviderKind::Copilot
    ) {
        let mut command = ProcessCommand::new(&executable);
        env_profile.apply_to_command(&mut command);
        match quota_rpc::read_quota(command, account.provider) {
            Ok(snapshot) => {
                let mut probe =
                    tayassu_providers::parse_quota_snapshot(account.provider, &snapshot)?;
                if probe.windows.is_empty() {
                    probe.windows.push(tayassu_core::QuotaWindow {
                        window_name: "auth".to_owned(),
                        bucket: ModelBucket::default_bucket(),
                        remaining: None,
                        resets_at: None,
                    });
                }
                return Ok(probe);
            }
            Err(error) => {
                tracing::debug!(account = %account.id, %error, "quota RPC unavailable; checking authentication");
            }
        }
    }

    if let Some(local_result) = adapter.probe_local(account, &env_profile, now) {
        return local_result.map_err(Into::into);
    }

    let Some(spec) = adapter.probe_command() else {
        return adapter
            .parse_usage_output(account, "", now)
            .map_err(Into::into);
    };

    let output = probe_process(account, &executable, &env_profile, &spec.args)?;
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
    let mut parsed = parsed?;
    if account.provider == ProviderKind::Claude {
        match claude_usage::probe(&executable, &env_profile, now) {
            Ok(windows) => parsed.windows.extend(windows),
            Err(error) => {
                tracing::debug!(account = %account.id, %error, "Claude plan usage unavailable");
            }
        }
    }
    Ok(parsed)
}

fn probe_process(
    account: &AccountConfig,
    executable: &std::path::Path,
    env_profile: &tayassu_providers::ProfileEnvironment,
    args: &[String],
) -> Result<std::process::Output> {
    let mut cmd = ProcessCommand::new(executable);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    env_profile.apply_to_command(&mut cmd);

    let output = if account.uses_private_agy_keyring() {
        crate::agy_keyring::probe_output(&mut cmd)
    } else {
        cmd.output().map_err(Into::into)
    };
    crate::agy_keyring::check_after_launch(account, env_profile)?;
    output.with_context(|| {
        format!(
            "failed to run probe '{}' for account '{}'",
            executable.display(),
            account.id
        )
    })
}

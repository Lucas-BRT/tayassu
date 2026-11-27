use anyhow::{Context, Result};
use chrono::Utc;
use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;
use std::process::{Command as ProcessCommand, Stdio};
use std::thread;
use tayassu_core::{AccountConfig, AccountId, ModelBucket, ProbeFailure};
use tayassu_providers::{AdapterRegistry, ProbeOutput, ProviderError};
use tayassu_store::{RuntimeState, XdgPaths};
use tracing::warn;

pub fn probe_accounts(
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
    let mut auth_failures = 0usize;
    let mut cleared_notifications = Vec::new();

    let (updated_state, ()) = RuntimeState::update_locked(&paths.state_file, |locked_state| {
        for (account_id, outcome) in results {
            match outcome {
                Ok(parsed) => {
                    let window_count = parsed.windows.len();
                    let cooldown_count = parsed.cooldowns.len();
                    let status = locked_state.status_for_mut(&account_id);
                    let contradicted = status.replace_windows(parsed.windows, now);
                    if let Some(writer) = out.as_deref_mut() {
                        for bucket in contradicted {
                            let _ = writeln!(
                                writer,
                                "{account_id}: estimated reset for '{bucket}' was wrong; provider reports it exhausted"
                            );
                        }
                    }
                    status.prune_expired(now);
                    status.clear_probe_failure();
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
                    let failure = classify_failure(&err);
                    if failure.is_authentication() {
                        auth_failures += 1;
                    }
                    locked_state
                        .status_for_mut(&account_id)
                        .set_probe_failure(failure);
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
    } else {
        ProbeFailure::other(err.to_string())
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
        .args([title, body])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
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

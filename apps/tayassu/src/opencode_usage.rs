use crate::{account_executable, quota_rpc};
use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::sync_channel;
use std::time::Duration;
use tayassu_core::{
    AccountConfig, KeyBudget, MAX_REMAINING_PERCENT, ModelBucket, ProviderKind, QuotaWindow,
    RemainingPercent,
};
use tayassu_providers::{AdapterRegistry, ProbeOutput, ProfileEnvironment};
use tayassu_store::XdgPaths;

/// Deadline for each CLI or HTTP query.
const TIMEOUT: Duration = Duration::from_secs(8);
/// Maximum accepted catalog, credential export or usage response size.
const RESPONSE_LIMIT: u64 = 4 * 1024 * 1024;
const COPILOT: &str = "github-copilot";
const OPENROUTER: &str = "openrouter";
const GO: &str = "opencode-go";
const OPENROUTER_FREE: &str = "openrouter-free";

pub(crate) fn probe(
    account: &AccountConfig,
    executable: &Path,
    profile: &ProfileEnvironment,
    configured_accounts: &[AccountConfig],
    paths: &XdgPaths,
) -> Result<ProbeOutput> {
    let auth = opencode_output(executable, profile, &["auth", "list", "--format", "json"])?;
    let auth: Value = serde_json::from_slice(&auth).context("invalid OpenCode connection list")?;
    let integrations = auth
        .as_array()
        .context("invalid OpenCode connection list")?;
    let models = opencode_output(executable, profile, &["models"])?;
    let models = String::from_utf8(models).context("invalid OpenCode catalog encoding")?;
    let mut result = ProbeOutput::default();
    for raw in models.lines().map(str::trim) {
        let Some((integration, model)) = raw.split_once('/') else {
            continue;
        };
        if integration.is_empty() || model.is_empty() || integration.contains(':') {
            continue;
        }
        let scope = if integration == OPENROUTER && (model.ends_with(":free") || model == "free") {
            OPENROUTER_FREE
        } else {
            integration
        };
        if let (Ok(model), Ok(bucket)) = (raw.parse::<ModelBucket>(), scope.parse::<ModelBucket>())
            && model.as_str() == raw
        {
            result.model_buckets.insert(model, bucket);
        }
    }

    for integration in integrations {
        let (Some(id), Some(connections)) = (
            integration["id"].as_str(),
            integration["connections"].as_array(),
        ) else {
            continue;
        };
        if connections.is_empty() {
            continue;
        }
        let Ok(bucket) = id.parse::<ModelBucket>() else {
            continue;
        };
        result.windows.push(QuotaWindow {
            window_name: "auth".to_owned(),
            bucket,
            remaining: None,
            resets_at: None,
        });

        let adapter_result = match id {
            COPILOT if connections.len() == 1 => {
                linked_copilot(account, configured_accounts, paths)
                    .map(|windows| result.windows.extend(windows))
            }
            GO | OPENROUTER => api_key_usage(id, executable, profile, &mut result),
            _ => Ok(()),
        };
        if let Err(error) = adapter_result {
            tracing::debug!(account = %account.id, integration = id, %error, "OpenCode quota unavailable");
        }
    }
    Ok(result)
}

fn linked_copilot(
    account: &AccountConfig,
    configured_accounts: &[AccountConfig],
    paths: &XdgPaths,
) -> Result<Vec<QuotaWindow>> {
    let Some(link) = &account.opencode_copilot_account else {
        return Ok(Vec::new());
    };
    let linked = configured_accounts
        .iter()
        .find(|item| &item.id == link && item.provider == ProviderKind::Copilot)
        .context("linked Copilot profile missing")?;
    let executable = account_executable(linked, &paths.home_dir);
    let profile = AdapterRegistry
        .adapter_for(ProviderKind::Copilot)
        .build_environment(linked, &paths.profiles_dir);
    profile.prepare(&paths.home_dir)?;
    let mut command = Command::new(executable);
    command.envs(profile.env_vars.iter().map(|(key, value)| (key, value)));
    // Keep the link tied to the isolated Copilot profile instead of an ambient token.
    for variable in ["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"] {
        command.env_remove(variable);
    }
    let snapshot = quota_rpc::read_quota(command, ProviderKind::Copilot)?;
    let mut windows =
        tayassu_providers::parse_quota_snapshot(ProviderKind::Copilot, &snapshot)?.windows;
    let bucket: ModelBucket = COPILOT.parse()?;
    for window in &mut windows {
        window.bucket = bucket.clone();
    }
    Ok(windows)
}

fn api_key_usage(
    id: &str,
    executable: &Path,
    profile: &ProfileEnvironment,
    result: &mut ProbeOutput,
) -> Result<()> {
    let export = opencode_output(executable, profile, &["auth", "export", id])?;
    let export: Value = serde_json::from_slice(&export)
        .map_err(|_| anyhow::anyhow!("invalid OpenCode credential export"))?;
    let key = active_key(&export, id).context("OpenCode did not export an active API key")?;
    let endpoint = if id == GO {
        "https://opencode.ai/inference/go/v1/usage"
    } else {
        "https://openrouter.ai/api/v1/key"
    };
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .max_redirects(0)
        .build()
        .into();
    let snapshot = agent
        .get(endpoint)
        .header("Authorization", &format!("Bearer {key}"))
        .call()
        .map_err(|_| anyhow::anyhow!("integration usage request failed"))?
        .body_mut()
        .with_config()
        .limit(RESPONSE_LIMIT)
        .read_json::<Value>()
        .map_err(|_| anyhow::anyhow!("invalid integration usage response"))?;
    if id == GO {
        result.windows.extend(go_windows(&snapshot)?);
    } else {
        read_openrouter(&snapshot, result)?;
    }
    Ok(())
}

fn active_key<'a>(export: &'a Value, integration: &str) -> Option<&'a str> {
    let mut active = export.as_array()?.iter().filter(|entry| {
        entry["active"].as_bool() == Some(true)
            && entry["integrationID"].as_str() == Some(integration)
    });
    let entry = active.next()?;
    if active.next().is_some() {
        return None;
    }
    entry["value"]["key"].as_str().filter(|key| !key.is_empty())
}

fn go_windows(snapshot: &Value) -> Result<Vec<QuotaWindow>> {
    let usage = snapshot["usage"]
        .as_object()
        .context("OpenCode Go usage missing")?;
    let bucket: ModelBucket = GO.parse()?;
    ["rolling", "weekly", "monthly"]
        .into_iter()
        .filter_map(|name| usage.get(name).map(|meter| (name, meter)))
        .map(|(name, meter)| {
            let used = meter["percent"]
                .as_u64()
                .and_then(|value| u8::try_from(value).ok())
                .filter(|value| *value <= MAX_REMAINING_PERCENT)
                .context("invalid Go quota percentage")?;
            let resets_at = meter["resetsAt"]
                .as_str()
                .context("Go quota reset missing")?
                .parse::<DateTime<Utc>>()?;
            let remaining = if meter["status"] == "rate-limited" {
                0
            } else {
                MAX_REMAINING_PERCENT - used
            };
            Ok(QuotaWindow {
                window_name: name.to_owned(),
                bucket: bucket.clone(),
                remaining: Some(RemainingPercent::try_from(remaining)?),
                resets_at: Some(resets_at),
            })
        })
        .collect()
}

fn read_openrouter(snapshot: &Value, result: &mut ProbeOutput) -> Result<()> {
    let data = &snapshot["data"];
    let key_budget = data["limit_remaining"]
        .as_f64()
        .map(|amount| (amount, data["limit_reset"].as_str()));
    let effective = &data["effective_budget"];
    let workspace_budget = effective["remaining_usd"]
        .as_f64()
        .map(|amount| (amount, effective["reset_interval"].as_str()));
    if let Some((amount, reset)) = key_budget
        .into_iter()
        .chain(workspace_budget)
        .min_by(|a, b| a.0.total_cmp(&b.0))
    {
        result.key_budget = Some(KeyBudget {
            remaining_usd: format!("{amount:.2}"),
            reset_interval: reset.map(str::to_owned),
        });
    }
    let free = &data["free_model_daily_requests"];
    if let (Some(remaining), Some(limit)) = (free["remaining"].as_u64(), free["limit"].as_u64())
        && limit > 0
        && remaining <= limit
    {
        let percent =
            (u128::from(remaining) * u128::from(MAX_REMAINING_PERCENT)).div_ceil(u128::from(limit));
        result.windows.push(QuotaWindow {
            window_name: "free-requests-daily".to_owned(),
            bucket: OPENROUTER_FREE.parse()?,
            remaining: Some(RemainingPercent::try_from(u8::try_from(percent)?)?),
            resets_at: Utc::now()
                .date_naive()
                .succ_opt()
                .map(|date| date.and_time(chrono::NaiveTime::MIN).and_utc()),
        });
    }
    Ok(())
}

fn opencode_output(
    executable: &Path,
    profile: &ProfileEnvironment,
    args: &[&str],
) -> Result<Vec<u8>> {
    let mut child = Command::new(executable)
        .args(args)
        .envs(profile.env_vars.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to start OpenCode")?;
    let mut stdout = child
        .stdout
        .take()
        .context("OpenCode stdout unavailable")?
        .take(RESPONSE_LIMIT + 1);
    let (sender, receiver) = sync_channel(1);
    std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = sender.send(stdout.read_to_end(&mut output).map(|_| output));
    });
    let start = std::time::Instant::now();
    let output = (|| {
        let output = receiver
            .recv_timeout(TIMEOUT)
            .context("OpenCode response timed out")??;
        ensure!(
            output.len() as u64 <= RESPONSE_LIMIT,
            "OpenCode response exceeded the size limit"
        );
        while start.elapsed() < TIMEOUT {
            if let Some(status) = child.try_wait()? {
                ensure!(status.success(), "OpenCode command failed with {status}");
                return Ok(output);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        bail!("OpenCode command did not finish")
    })();
    if output.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn takes_only_the_active_key_for_the_exact_integration() {
        let export = json!([
            {"integrationID": "openrouter", "active": false, "value": {"key": "old"}},
            {"integrationID": "opencode-go", "active": true, "value": {"key": "different"}},
            {"integrationID": "openrouter", "active": true, "value": {"key": "valid"}}
        ]);
        assert_eq!(active_key(&export, "openrouter"), Some("valid"));
        assert_eq!(active_key(&export, "google"), None);
    }

    #[test]
    fn keeps_openrouter_budget_separate_from_free_request_quota() {
        let mut result = ProbeOutput::default();
        read_openrouter(
            &json!({"data": {
                "limit_remaining": 20,
                "effective_budget": {"remaining_usd": 6.06, "reset_interval": "weekly"},
                "free_model_daily_requests": {"remaining": 18, "limit": 50}
            }}),
            &mut result,
        )
        .expect("parse budget");
        assert_eq!(result.key_budget.expect("key budget").remaining_usd, "6.06");
        assert_eq!(
            result.windows[0].remaining.map(RemainingPercent::value),
            Some(36)
        );
        assert_eq!(result.windows[0].bucket.as_str(), OPENROUTER_FREE);
    }

    #[test]
    fn uses_go_reported_window_exhaustion() {
        let windows = go_windows(&json!({"usage": {
            "rolling": {"percent": 20, "resetsAt": "2099-01-01T00:00:00Z"},
            "weekly": {"status": "rate-limited", "percent": 99, "resetsAt": "2099-01-01T00:00:00Z"},
            "monthly": {"percent": 40, "resetsAt": "2099-01-01T00:00:00Z"}
        }}))
        .expect("Go meters");
        assert_eq!(windows[0].remaining.map(RemainingPercent::value), Some(80));
        assert_eq!(windows[1].remaining.map(RemainingPercent::value), Some(0));
        assert_eq!(windows[2].bucket.as_str(), GO);
    }
}

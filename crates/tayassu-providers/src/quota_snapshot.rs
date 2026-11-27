use crate::{ProbeOutput, ProviderError};
use chrono::{DateTime, Utc};
use serde_json::Value;
use tayassu_core::{
    MAX_REMAINING_PERCENT, ModelBucket, ProviderKind, QuotaWindow, RemainingPercent,
};

/// Converts a native quota RPC response to quota windows for routing.
///
/// # Errors
///
/// Returns a parsing error when the response has no snapshot structure or an invalid meter.
pub fn parse_quota_snapshot(
    provider: ProviderKind,
    snapshot: &Value,
) -> Result<ProbeOutput, ProviderError> {
    let windows = match provider {
        ProviderKind::Codex => codex_windows(snapshot),
        ProviderKind::Copilot => copilot_windows(snapshot),
        _ => Err("provider has no quota RPC"),
    }
    .map_err(|reason| ProviderError::ParseUsage {
        provider,
        reason: reason.to_owned(),
    })?;
    Ok(ProbeOutput {
        windows,
        ..ProbeOutput::default()
    })
}

fn codex_windows(snapshot: &Value) -> Result<Vec<QuotaWindow>, &'static str> {
    let mut windows = Vec::new();
    let by_limit = snapshot["rateLimitsByLimitId"].as_object();
    if let Some(limits) = by_limit {
        for (id, limit) in limits {
            let bucket = if id == "codex" {
                ModelBucket::default_bucket()
            } else {
                id.parse().map_err(|_| "invalid Codex limit identifier")?
            };
            append_codex_windows(&mut windows, &bucket, limit)?;
        }
    }
    if by_limit.is_none_or(|limits| !limits.contains_key("codex")) {
        let limit = snapshot["rateLimits"]
            .as_object()
            .ok_or("missing Codex rate limits")?;
        append_codex_windows(
            &mut windows,
            &ModelBucket::default_bucket(),
            &Value::Object(limit.clone()),
        )?;
    }
    Ok(windows)
}

fn append_codex_windows(
    windows: &mut Vec<QuotaWindow>,
    bucket: &ModelBucket,
    limit: &Value,
) -> Result<(), &'static str> {
    for name in ["primary", "secondary"] {
        let meter = &limit[name];
        if meter.is_null() {
            continue;
        }
        let used = meter["usedPercent"]
            .as_f64()
            .ok_or("invalid Codex usedPercent")?;
        let remaining = percent_left(f64::from(MAX_REMAINING_PERCENT) - used, used)?;
        let resets_at = match &meter["resetsAt"] {
            Value::Null => None,
            value => Some(
                value
                    .as_i64()
                    .and_then(|seconds| DateTime::from_timestamp(seconds, 0))
                    .ok_or("invalid Codex reset timestamp")?,
            ),
        };
        let window_name = meter["windowDurationMins"]
            .as_u64()
            .map_or_else(|| name.to_owned(), |minutes| format!("{minutes}m"));
        windows.push(QuotaWindow {
            window_name,
            bucket: bucket.clone(),
            remaining: Some(remaining),
            resets_at,
        });
    }
    Ok(())
}

fn copilot_windows(snapshot: &Value) -> Result<Vec<QuotaWindow>, &'static str> {
    let snapshots = snapshot["quotaSnapshots"]
        .as_object()
        .ok_or("missing Copilot quota snapshots")?;
    // An entitlement of zero indicates this plan does not use that meter.
    // Completion entitlement applies to editor completions rather than CLI conversations.
    let meter = ["premium_interactions", "chat"]
        .into_iter()
        .find_map(|name| {
            let meter = snapshots.get(name)?;
            (meter["entitlementRequests"].as_i64() != Some(0)).then_some((name, meter))
        });
    let Some((name, meter)) = meter else {
        return Ok(Vec::new());
    };
    if meter["entitlementRequests"].as_i64() == Some(-1) || meter["remainingPercentage"].is_null() {
        return Ok(Vec::new());
    }
    let percent = meter["remainingPercentage"]
        .as_f64()
        .ok_or("invalid Copilot remainingPercentage")?;
    let resets_at = match &meter["resetDate"] {
        Value::Null => None,
        value => {
            let date = value.as_str().ok_or("invalid Copilot reset date")?;
            Some(
                DateTime::parse_from_rfc3339(date)
                    .map_err(|_| "invalid Copilot reset date")?
                    .with_timezone(&Utc),
            )
        }
    };
    Ok(vec![QuotaWindow {
        window_name: name.replace('_', "-"),
        bucket: ModelBucket::default_bucket(),
        remaining: Some(percent_left(percent, percent)?),
        resets_at,
    }])
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the validated remainder fits in u8"
)]
fn percent_left(remaining: f64, reported: f64) -> Result<RemainingPercent, &'static str> {
    if !(0.0..=f64::from(MAX_REMAINING_PERCENT)).contains(&reported) {
        return Err("percentage outside 0..=100");
    }
    // Keep a fractional nonzero remainder distinct from exhaustion.
    RemainingPercent::try_from(remaining.ceil() as u8).map_err(|_| "invalid remaining percentage")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn codex_inverts_used_percent_and_keeps_named_limits() {
        let result = parse_quota_snapshot(ProviderKind::Codex, &json!({
            "rateLimitsByLimitId": {
                "codex": {"primary": {"usedPercent": 20, "windowDurationMins": 300, "resetsAt": 2_000_000_000}},
                "gpt-test": {"secondary": {"usedPercent": 100, "resetsAt": null}}
            }
        })).expect("valid snapshots");
        assert_eq!(result.windows.len(), 2);
        assert_eq!(result.windows[0].bucket, ModelBucket::default_bucket());
        assert_eq!(
            result.windows[0].remaining.map(RemainingPercent::value),
            Some(80)
        );
        assert_eq!(
            result.windows[0].resets_at.map(|date| date.timestamp()),
            Some(2_000_000_000)
        );
        assert_eq!(result.windows[1].bucket.as_str(), "gpt-test");
        assert_eq!(
            result.windows[1].remaining.map(RemainingPercent::value),
            Some(0)
        );
    }

    #[test]
    fn copilot_retains_remaining_percent_and_does_not_invent_unlimited_quota() {
        let result = parse_quota_snapshot(ProviderKind::Copilot, &json!({
            "quotaSnapshots": {
                "premium_interactions": {
                    "remainingPercentage": 0.2, "entitlementRequests": 300, "resetDate": "2099-01-01T00:00:00Z"
                }
            }
        })).expect("valid quota");
        assert_eq!(
            result.windows[0].remaining.map(RemainingPercent::value),
            Some(1)
        );
        let unlimited = parse_quota_snapshot(ProviderKind::Copilot, &json!({
            "quotaSnapshots": { "premium_interactions": { "remainingPercentage": 100, "entitlementRequests": -1 } }
        })).expect("unlimited quota");
        assert_eq!(unlimited.windows, []);
    }
    #[test]
    fn copilot_uses_chat_when_premium_entitlement_is_zero() {
        let result = parse_quota_snapshot(
            ProviderKind::Copilot,
            &json!({
                "quotaSnapshots": {
                    "premium_interactions": {"remainingPercentage": 0, "entitlementRequests": 0},
                    "chat": {"remainingPercentage": 25, "entitlementRequests": 200},
                    "completions": {"remainingPercentage": 100, "entitlementRequests": 2000}
                }
            }),
        )
        .expect("valid snapshots");
        assert_eq!(result.windows[0].window_name, "chat");
        assert_eq!(
            result.windows[0].remaining.map(RemainingPercent::value),
            Some(25)
        );
    }
}

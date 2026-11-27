use chrono::{DateTime, TimeDelta, Utc};
use std::collections::BTreeSet;
use std::io::{self, Write};
use tayassu_core::{AccountConfig, AccountStatus, ModelBucket, ProviderKind};

/// Seconds per minute used for human-readable countdown formatting.
const SECS_PER_MINUTE: i64 = 60;

/// Minutes per hour used for human-readable countdown formatting.
const MINUTES_PER_HOUR: i64 = 60;

/// Compact CLI table renderer for account availability and cooldowns.
pub struct StatusTable<'a> {
    accounts: &'a [AccountConfig],
    statuses: &'a [AccountStatus],
    provider_filter: Option<ProviderKind>,
    bucket_filter: Option<&'a ModelBucket>,
}

impl<'a> StatusTable<'a> {
    /// Creates a new status table view over `accounts` and `statuses`.
    #[must_use]
    pub const fn new(
        accounts: &'a [AccountConfig],
        statuses: &'a [AccountStatus],
        provider_filter: Option<ProviderKind>,
        bucket_filter: Option<&'a ModelBucket>,
    ) -> Self {
        Self {
            accounts,
            statuses,
            provider_filter,
            bucket_filter,
        }
    }

    /// Prints the provider model catalog with each model's applicable quota.
    pub fn write_models(&self, writer: &mut impl Write, now: DateTime<Utc>) -> io::Result<()> {
        let mut found = false;
        for account in self.accounts.iter().filter(|account| {
            self.provider_filter
                .is_none_or(|provider| provider == account.provider)
        }) {
            let Some(status) = self
                .statuses
                .iter()
                .find(|status| status.account_id == account.id)
            else {
                continue;
            };
            for (model, scope) in &status.model_buckets {
                if !found {
                    writeln!(writer, "ACCOUNT MODEL QUOTA SCOPE")?;
                    found = true;
                }
                let remaining = status
                    .lowest_remaining_percent(model, now)
                    .map_or_else(|| "unknown".to_owned(), |value| value.to_string());
                let tag = if status.last_probe_error.is_some() {
                    " (stale)"
                } else if !status.is_available(model, now) {
                    " (blocked)"
                } else {
                    ""
                };
                writeln!(writer, "{} {model} {remaining}{tag} {scope}", account.id)?;
            }
        }
        if !found {
            writeln!(
                writer,
                "No model catalog is available. Run tayassu probe first."
            )?;
        }
        Ok(())
    }

    /// Writes the formatted table to `writer` evaluated at `now`.
    ///
    /// # Errors
    ///
    /// Returns any underlying I/O error from `writer`.
    pub fn write_to(&self, writer: &mut impl Write, now: DateTime<Utc>) -> io::Result<()> {
        let filtered: Vec<&AccountConfig> = self
            .accounts
            .iter()
            .filter(|account| {
                self.provider_filter
                    .is_none_or(|provider| account.provider == provider)
            })
            .collect();

        if filtered.is_empty() {
            return writeln!(
                writer,
                "No accounts configured. Add one with: tayassu account add <id> <provider>"
            );
        }

        writeln!(
            writer,
            "{:<18} {:<10} {:<4} {:<18} {:<13} {:<28} {:<20} {:<8} LABEL",
            "ACCOUNT", "PROVIDER", "PRI", "STATE", "LOGIN", "QUOTA", "LAST PROBE (UTC)", "SESSIONS"
        )?;

        let default_bucket = ModelBucket::default_bucket();

        for account in filtered {
            let status = self
                .statuses
                .iter()
                .find(|entry| entry.account_id == account.id);

            let eval_bucket = self.bucket_filter.unwrap_or(&default_bucket);
            let state_text = render_state(account, status, eval_bucket, now);
            let mut quota_text = render_quota(status, self.bucket_filter, now);
            if status.is_some_and(|st| st.last_probe_error.is_some()) && quota_text.contains('%') {
                quota_text.push_str(" (stale)");
            }

            let login = render_login(status);
            let last_probe = status.and_then(|st| st.last_probed_at).map_or_else(
                || "never".to_owned(),
                |time| time.format("%Y-%m-%d %H:%M:%S").to_string(),
            );
            let sessions = status.map_or(0, |st| st.active_sessions);

            writeln!(
                writer,
                "{:<18} {:<10} {:<4} {:<18} {:<13} {:<28} {:<20} {sessions:<8} {}",
                account.id,
                account.provider,
                account.priority,
                state_text,
                login,
                quota_text,
                last_probe,
                account.label
            )?;
            if let Some(budget) = status.and_then(|st| st.key_budget.as_ref()) {
                let freshness = if status.is_some_and(|st| st.last_probe_error.is_some()) {
                    " (stale)"
                } else {
                    ""
                };
                let interval = budget
                    .reset_interval
                    .as_deref()
                    .map_or_else(String::new, |value| format!(" ({value})"));
                writeln!(
                    writer,
                    "  {}: OpenRouter budget ${} remaining{interval}{freshness}",
                    account.id, budget.remaining_usd
                )?;
            }
            if let Some(failure) = status.and_then(|st| st.last_probe_error.as_ref()) {
                let id = &account.id;
                let reason = failure
                    .message
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                let action = if failure.is_authentication() {
                    format!("tayassu login {id}")
                } else {
                    format!("tayassu doctor, then tayassu probe --id {id}")
                };
                writeln!(writer, "  {id}: {reason}. Next: {action}")?;
            }
        }

        Ok(())
    }
}

fn render_state(
    account: &AccountConfig,
    status: Option<&AccountStatus>,
    bucket: &ModelBucket,
    now: DateTime<Utc>,
) -> String {
    if !account.enabled {
        return "DISABLED".to_owned();
    }
    if let Some(failure) = status.and_then(|st| st.last_probe_error.as_ref()) {
        return if failure.is_authentication() {
            "AUTH".to_owned()
        } else {
            "ERROR".to_owned()
        };
    }
    if let Some(cooldown) = status.and_then(|st| st.active_cooldown(bucket, now)) {
        let rem = cooldown
            .remaining(now)
            .map_or_else(|| "0m".to_owned(), format_countdown);
        return format!("COOLDOWN ({rem})");
    }
    if status.is_some_and(|st| !st.is_available(bucket, now)) {
        return "EXHAUSTED".to_owned();
    }
    "READY".to_owned()
}

pub(crate) fn render_login(status: Option<&AccountStatus>) -> &'static str {
    match status {
        Some(st)
            if st
                .last_probe_error
                .as_ref()
                .is_some_and(tayassu_core::ProbeFailure::is_authentication) =>
        {
            "required"
        }
        Some(st) if st.last_probe_error.is_none() && st.last_probed_at.is_some() => "ok",
        _ => "unverified",
    }
}

fn render_quota(
    status: Option<&AccountStatus>,
    bucket_filter: Option<&ModelBucket>,
    now: DateTime<Utc>,
) -> String {
    let Some(st) = status else {
        return "unknown".to_owned();
    };

    if let Some(bucket) = bucket_filter {
        return st
            .lowest_remaining_percent(bucket, now)
            .map_or_else(|| "unknown".to_owned(), |pct| pct.to_string());
    }

    let mut buckets = BTreeSet::new();
    for window in st
        .windows
        .iter()
        .filter(|window| window.remaining.is_some())
    {
        buckets.insert(window.bucket.clone());
    }
    for cooldown in &st.cooldowns {
        if cooldown.is_active(now) {
            buckets.insert(cooldown.bucket.clone());
        }
    }

    if buckets.is_empty() {
        return "unknown".to_owned();
    }

    let parts: Vec<String> = buckets
        .iter()
        .map(|bucket| {
            let pct = st.quota_label(bucket, now);
            if let Some(cd) = st.active_cooldown(bucket, now) {
                let rem = cd
                    .remaining(now)
                    .map_or_else(|| "0m".to_owned(), format_countdown);
                format!("{bucket}:{pct}({rem})")
            } else {
                format!("{bucket}:{pct}")
            }
        })
        .collect();

    parts.join(" ")
}

/// Formats a positive duration into a compact `XhYYm` or `Ym` countdown.
#[must_use]
pub fn format_countdown(delta: TimeDelta) -> String {
    let total_minutes = (delta.num_seconds() + SECS_PER_MINUTE - 1) / SECS_PER_MINUTE;
    let hours = total_minutes / MINUTES_PER_HOUR;
    let minutes = total_minutes % MINUTES_PER_HOUR;
    if hours > 0 {
        format!("{hours}h{minutes:02}m")
    } else {
        format!("{minutes}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tayassu_core::{CooldownEntry, QuotaWindow, RemainingPercent};

    /// Cooldown duration in minutes used to verify countdown formatting.
    const TEST_COOLDOWN_MINUTES: i64 = 125;

    /// Gemini remaining quota percentage fixture.
    const GEMINI_PCT: u8 = 43;

    #[test]
    fn renders_auth_failure_state() {
        let now = Utc::now();
        let account = AccountConfig::new(
            "claude-work".parse().expect("id"),
            ProviderKind::Claude,
            "Work",
        );
        let mut status = AccountStatus::empty(account.id.clone());
        status.last_probed_at = Some(now);
        status.set_probe_failure(tayassu_core::ProbeFailure::authentication("not logged in"));

        let accounts = [account];
        let statuses = [status];
        let table = StatusTable::new(&accounts, &statuses, None, None);

        let mut buf = Vec::new();
        table.write_to(&mut buf, now).expect("render table");
        let output = String::from_utf8(buf).expect("utf8");

        assert!(output.contains("AUTH"));
    }

    #[test]
    fn renders_ready_and_cooldown_rows() {
        let now = Utc::now();
        let ready = AccountConfig::new(
            "agy-pro".parse().expect("id"),
            ProviderKind::Agy,
            "Personal Pro",
        );
        let cooling = AccountConfig::new(
            "agy-student".parse().expect("id"),
            ProviderKind::Agy,
            "Student",
        );

        let mut cooling_status = AccountStatus::empty(cooling.id.clone());

        cooling_status.last_probed_at = Some(now);
        cooling_status.windows.push(QuotaWindow {
            window_name: "weekly".to_owned(),
            bucket: "gemini".parse().expect("bucket"),
            remaining: Some(RemainingPercent::try_from(GEMINI_PCT).expect("pct")),
            resets_at: None,
        });
        cooling_status.set_cooldown(CooldownEntry {
            bucket: ModelBucket::default_bucket(),
            resets_at: now + TimeDelta::minutes(TEST_COOLDOWN_MINUTES),
            reason: "manual".to_owned(),
        });

        let accounts = [ready, cooling];
        let statuses = [cooling_status];
        let table = StatusTable::new(&accounts, &statuses, None, None);

        let mut buf = Vec::new();
        table.write_to(&mut buf, now).expect("render table");
        let output = String::from_utf8(buf).expect("utf8");

        assert!(output.contains("agy-pro"));
        assert!(output.contains("READY"));
        assert!(output.contains("agy-student"));
        assert!(output.contains("COOLDOWN (2h05m)"));
        assert!(output.contains("gemini:43%"));
    }
}

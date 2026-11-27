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
                "No accounts configured. Add one with: tayassu add <provider>"
            );
        }

        writeln!(
            writer,
            "{:<18} {:<10} {:<4} {:<18} {:<28} {:<10} LABEL",
            "ACCOUNT", "PROVIDER", "PRI", "STATE", "QUOTA", "CHECKED"
        )?;

        let default_bucket = ModelBucket::default_bucket();

        for account in filtered {
            let status = self
                .statuses
                .iter()
                .find(|entry| entry.account_id == account.id);

            let eval_bucket = self.bucket_filter.unwrap_or(&default_bucket);
            let state_text = render_state(account, status, eval_bucket, now);
            let quota_text = render_quota(status, self.bucket_filter, now);

            writeln!(
                writer,
                "{:<18} {:<10} {:<4} {:<18} {:<28} {:<10} {}",
                account.id,
                account.provider,
                account.priority,
                state_text,
                quota_text,
                render_checked(status, now),
                account.label
            )?;
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

fn render_quota(
    status: Option<&AccountStatus>,
    bucket_filter: Option<&ModelBucket>,
    now: DateTime<Utc>,
) -> String {
    let Some(st) = status else {
        return "-".to_owned();
    };

    if let Some(bucket) = bucket_filter {
        return st
            .bucket_state(bucket, now)
            .map_or_else(|| "-".to_owned(), |_| st.quota_label(bucket, now));
    }

    let mut buckets = BTreeSet::new();
    for window in &st.windows {
        buckets.insert(window.bucket.clone());
    }
    for cooldown in &st.cooldowns {
        if cooldown.is_active(now) {
            buckets.insert(cooldown.bucket.clone());
        }
    }

    if buckets.is_empty() {
        return "-".to_owned();
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

/// Age of the last probe (`never`, `3m ago`, `2h05m ago`).
fn render_checked(status: Option<&AccountStatus>, now: DateTime<Utc>) -> String {
    status.and_then(|st| st.last_probed_at).map_or_else(
        || "never".to_owned(),
        |at| {
            format!(
                "{} ago",
                format_countdown(now.signed_duration_since(at).max(TimeDelta::zero()))
            )
        },
    )
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

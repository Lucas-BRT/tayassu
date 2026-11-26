use chrono::{DateTime, TimeDelta, Utc};
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
    bucket: &'a ModelBucket,
}

impl<'a> StatusTable<'a> {
    /// Creates a new status table view over `accounts` and `statuses`.
    #[must_use]
    pub const fn new(
        accounts: &'a [AccountConfig],
        statuses: &'a [AccountStatus],
        provider_filter: Option<ProviderKind>,
        bucket: &'a ModelBucket,
    ) -> Self {
        Self {
            accounts,
            statuses,
            provider_filter,
            bucket,
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
                "No accounts configured. Add one with: tayassu account add <id> <provider>"
            );
        }

        writeln!(
            writer,
            "{:<18} {:<10} {:<4} {:<18} {:<8} LABEL",
            "ACCOUNT", "PROVIDER", "PRI", "STATE", "QUOTA"
        )?;

        for account in filtered {
            let status = self
                .statuses
                .iter()
                .find(|entry| entry.account_id == account.id);

            let state_text = if !account.enabled {
                "DISABLED".to_owned()
            } else if let Some(cooldown) =
                status.and_then(|st| st.active_cooldown(self.bucket, now))
            {
                let rem = cooldown
                    .remaining(now)
                    .map_or_else(|| "0m".to_owned(), format_countdown);
                format!("COOLDOWN ({rem})")
            } else if status.is_some_and(|st| !st.is_available(self.bucket, now)) {
                "EXHAUSTED".to_owned()
            } else {
                "READY".to_owned()
            };

            let quota_text = status
                .and_then(|st| st.lowest_remaining_percent(self.bucket))
                .map_or_else(|| "-".to_owned(), |pct| pct.to_string());

            writeln!(
                writer,
                "{:<18} {:<10} {:<4} {:<18} {:<8} {}",
                account.id,
                account.provider,
                account.priority,
                state_text,
                quota_text,
                account.label
            )?;
        }

        Ok(())
    }
}

fn format_countdown(delta: TimeDelta) -> String {
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
    use tayassu_core::CooldownEntry;

    /// Cooldown duration in minutes used to verify countdown formatting.
    const TEST_COOLDOWN_MINUTES: i64 = 125;

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
        cooling_status.set_cooldown(CooldownEntry {
            bucket: ModelBucket::default_bucket(),
            resets_at: now + TimeDelta::minutes(TEST_COOLDOWN_MINUTES),
            reason: "manual".to_owned(),
        });

        let accounts = [ready, cooling];
        let statuses = [cooling_status];
        let bucket = ModelBucket::default_bucket();
        let table = StatusTable::new(&accounts, &statuses, None, &bucket);

        let mut buf = Vec::new();
        table.write_to(&mut buf, now).expect("render table");
        let output = String::from_utf8(buf).expect("utf8");

        assert!(output.contains("agy-pro"));
        assert!(output.contains("READY"));
        assert!(output.contains("agy-student"));
        assert!(output.contains("COOLDOWN (2h05m)"));
    }
}

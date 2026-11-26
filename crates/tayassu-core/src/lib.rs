//! Core domain model, quota/cooldown state, and account routing for `tayassu`.

pub mod account;
pub mod error;
pub mod quota;
pub mod router;

pub use account::{
    AccountConfig, AccountId, DEFAULT_ACCOUNT_PRIORITY, DEFAULT_MODEL_BUCKET, MAX_ACCOUNT_ID_LEN,
    MAX_MODEL_BUCKET_LEN, ModelBucket, ProviderKind,
};
pub use error::CoreError;
pub use quota::{
    AccountStatus, CooldownEntry, DEFAULT_FALLBACK_COOLDOWN_SECS, MAX_REMAINING_PERCENT,
    MIN_REMAINING_PERCENT, QuotaWindow, RemainingPercent,
};
pub use router::{AccountRouter, SelectionPolicy};

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeDelta, Utc};

    /// Priority assigned to the preferred student account in routing tests.
    const STUDENT_PRIORITY: u8 = 10;

    /// Priority assigned to the secondary pro account in routing tests.
    const PRO_PRIORITY: u8 = 20;

    /// Cooldown duration in hours used in routing tests.
    const TEST_COOLDOWN_HOURS: i64 = 5;

    /// High remaining quota percentage fixture.
    const HIGH_QUOTA_PCT: u8 = 85;

    /// Low remaining quota percentage fixture.
    const LOW_QUOTA_PCT: u8 = 25;

    #[test]
    fn account_id_normalizes_and_rejects_invalid_chars() {
        let valid: AccountId = "Agy_Student-1".parse().expect("valid account id");
        assert_eq!(valid.as_str(), "agy_student-1");

        let invalid = "bad/id".parse::<AccountId>();
        assert!(matches!(invalid, Err(CoreError::InvalidAccountId { .. })));
    }

    #[test]
    fn router_skips_cooling_down_account_and_picks_next() {
        let now = Utc::now();
        let mut student = AccountConfig::new(
            "agy-student".parse().expect("id"),
            ProviderKind::Agy,
            "Student plan",
        );
        student.priority = STUDENT_PRIORITY;

        let mut pro = AccountConfig::new(
            "agy-pro".parse().expect("id"),
            ProviderKind::Agy,
            "Personal Pro",
        );
        pro.priority = PRO_PRIORITY;

        let mut student_status = AccountStatus::empty(student.id.clone());
        student_status.set_cooldown(CooldownEntry {
            bucket: ModelBucket::default_bucket(),
            resets_at: now + TimeDelta::hours(TEST_COOLDOWN_HOURS),
            reason: "probe".to_owned(),
        });

        let accounts = [student, pro.clone()];
        let router = AccountRouter::new(SelectionPolicy::Priority);
        let picked = router
            .select(
                &accounts,
                &[student_status],
                ProviderKind::Agy,
                &ModelBucket::default_bucket(),
                now,
            )
            .expect("pro account should be selected");

        assert_eq!(picked.id, pro.id);
    }

    #[test]
    fn router_most_quota_prefers_higher_remaining_percent() {
        let now = Utc::now();
        let mut first = AccountConfig::new(
            "claude-work".parse().expect("id"),
            ProviderKind::Claude,
            "Work",
        );
        first.priority = STUDENT_PRIORITY;

        let mut second = AccountConfig::new(
            "claude-personal".parse().expect("id"),
            ProviderKind::Claude,
            "Personal",
        );
        second.priority = PRO_PRIORITY;

        let mut first_status = AccountStatus::empty(first.id.clone());
        first_status.windows.push(QuotaWindow {
            window_name: "5h".to_owned(),
            bucket: ModelBucket::default_bucket(),
            remaining: Some(RemainingPercent::try_from(LOW_QUOTA_PCT).expect("pct")),
            resets_at: Some(now + TimeDelta::hours(TEST_COOLDOWN_HOURS)),
        });

        let mut second_status = AccountStatus::empty(second.id.clone());
        second_status.windows.push(QuotaWindow {
            window_name: "5h".to_owned(),
            bucket: ModelBucket::default_bucket(),
            remaining: Some(RemainingPercent::try_from(HIGH_QUOTA_PCT).expect("pct")),
            resets_at: Some(now + TimeDelta::hours(TEST_COOLDOWN_HOURS)),
        });

        let accounts = [first, second.clone()];
        let router = AccountRouter::new(SelectionPolicy::MostQuota);
        let picked = router
            .select(
                &accounts,
                &[first_status, second_status],
                ProviderKind::Claude,
                &ModelBucket::default_bucket(),
                now,
            )
            .expect("higher quota account should be selected");

        assert_eq!(picked.id, second.id);
    }

    #[test]
    fn router_soonest_reset_and_expired_cooldown_handling() {
        let now = Utc::now();
        let mut soon = AccountConfig::new(
            "copilot-a".parse().expect("id"),
            ProviderKind::Copilot,
            "Copilot A",
        );
        soon.priority = PRO_PRIORITY;

        let mut later = AccountConfig::new(
            "copilot-b".parse().expect("id"),
            ProviderKind::Copilot,
            "Copilot B",
        );
        later.priority = STUDENT_PRIORITY;

        let mut soon_status = AccountStatus::empty(soon.id.clone());
        soon_status.windows.push(QuotaWindow {
            window_name: "5h".to_owned(),
            bucket: ModelBucket::default_bucket(),
            remaining: Some(RemainingPercent::try_from(LOW_QUOTA_PCT).expect("pct")),
            resets_at: Some(now + TimeDelta::hours(1)),
        });
        // Expired cooldown must not block selection.
        soon_status.set_cooldown(CooldownEntry {
            bucket: ModelBucket::default_bucket(),
            resets_at: now - TimeDelta::minutes(1),
            reason: "expired".to_owned(),
        });

        let mut later_status = AccountStatus::empty(later.id.clone());
        later_status.windows.push(QuotaWindow {
            window_name: "5h".to_owned(),
            bucket: ModelBucket::default_bucket(),
            remaining: Some(RemainingPercent::try_from(HIGH_QUOTA_PCT).expect("pct")),
            resets_at: Some(now + TimeDelta::hours(TEST_COOLDOWN_HOURS)),
        });

        let accounts = [soon.clone(), later];
        let router = AccountRouter::new(SelectionPolicy::SoonestReset);
        let picked = router
            .select(
                &accounts,
                &[soon_status, later_status],
                ProviderKind::Copilot,
                &ModelBucket::default_bucket(),
                now,
            )
            .expect("soonest resetting account should be selected");

        assert_eq!(picked.id, soon.id);
    }

    #[test]
    fn provider_kind_parses_all_supported_agents_and_display_pads() {
        assert_eq!(
            "copilot".parse::<ProviderKind>().expect("copilot"),
            ProviderKind::Copilot
        );
        assert_eq!(
            "github-copilot".parse::<ProviderKind>().expect("alias"),
            ProviderKind::Copilot
        );
        assert_eq!(
            "opencode".parse::<ProviderKind>().expect("opencode"),
            ProviderKind::Opencode
        );

        let id: AccountId = "agy-pro".parse().expect("id");
        assert_eq!(format!("{id:<12}"), "agy-pro     ");
        assert_eq!(format!("{:<10}", ProviderKind::Agy), "agy       ");
    }
}

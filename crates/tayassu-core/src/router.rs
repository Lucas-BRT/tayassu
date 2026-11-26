use crate::account::{AccountConfig, ModelBucket, ProviderKind};
use crate::error::CoreError;
use crate::quota::{AccountStatus, MAX_REMAINING_PERCENT, RemainingPercent};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::fmt;
use std::str::FromStr;

/// Strategy used by [`AccountRouter`] to rank available accounts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SelectionPolicy {
    /// Prefer lowest configured `priority`, breaking ties by highest remaining quota.
    #[default]
    Priority,
    /// Prefer highest remaining quota percentage, breaking ties by `priority`.
    MostQuota,
    /// Prefer the account whose current quota window resets soonest (use-it-or-lose-it).
    SoonestReset,
}

impl SelectionPolicy {
    /// Canonical lowercase name of this policy.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Priority => "priority",
            Self::MostQuota => "most-quota",
            Self::SoonestReset => "soonest-reset",
        }
    }
}

impl FromStr for SelectionPolicy {
    type Err = CoreError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "priority" => Ok(Self::Priority),
            "most-quota" => Ok(Self::MostQuota),
            "soonest-reset" => Ok(Self::SoonestReset),
            _ => Err(CoreError::UnknownSelectionPolicy {
                value: raw.to_owned(),
            }),
        }
    }
}

impl fmt::Display for SelectionPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Selects the best available account for a `(provider, model_bucket)` request.
#[derive(Debug, Clone, Copy)]
pub struct AccountRouter {
    policy: SelectionPolicy,
}

impl AccountRouter {
    /// Creates a router using `policy`.
    #[must_use]
    pub const fn new(policy: SelectionPolicy) -> Self {
        Self { policy }
    }

    /// Selects the highest-ranked available account for `provider` and `bucket` at `now`.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::NoAvailableAccount`] if no enabled account for `provider` is
    /// outside cooldown for `bucket`.
    pub fn select<'a>(
        &self,
        accounts: &'a [AccountConfig],
        statuses: &[AccountStatus],
        provider: ProviderKind,
        bucket: &ModelBucket,
        now: DateTime<Utc>,
    ) -> Result<&'a AccountConfig, CoreError> {
        let mut candidates: Vec<(&'a AccountConfig, Option<&AccountStatus>)> = accounts
            .iter()
            .filter(|account| account.enabled && account.provider == provider)
            .map(|account| {
                let status = statuses
                    .iter()
                    .find(|status| status.account_id == account.id);
                (account, status)
            })
            .filter(|(_, status)| status.is_none_or(|st| st.is_available(bucket, now)))
            .collect();

        match self.policy {
            SelectionPolicy::Priority => {
                candidates.sort_by_key(|(account, status)| {
                    let remaining = status
                        .and_then(|st| st.lowest_remaining_percent(bucket))
                        .map_or(MAX_REMAINING_PERCENT, RemainingPercent::value);
                    (account.priority, Reverse(remaining), account.id.clone())
                });
            }
            SelectionPolicy::MostQuota => {
                candidates.sort_by_key(|(account, status)| {
                    let remaining = status
                        .and_then(|st| st.lowest_remaining_percent(bucket))
                        .map_or(MAX_REMAINING_PERCENT, RemainingPercent::value);
                    (Reverse(remaining), account.priority, account.id.clone())
                });
            }
            SelectionPolicy::SoonestReset => {
                candidates.sort_by_key(|(account, status)| {
                    let reset_ts = status
                        .and_then(|st| st.soonest_reset(bucket))
                        .map_or(i64::MAX, |dt| dt.timestamp());
                    (reset_ts, account.priority, account.id.clone())
                });
            }
        }

        candidates
            .first()
            .map(|(account, _)| *account)
            .ok_or_else(|| CoreError::NoAvailableAccount {
                provider,
                bucket: bucket.clone(),
            })
    }
}

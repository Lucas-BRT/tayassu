use crate::account::{AccountConfig, ModelBucket, ProviderKind};
use crate::error::CoreError;
use crate::quota::AccountStatus;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::fmt;
use std::str::FromStr;

/// Strategy used by [`AccountRouter`] to rank available accounts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SelectionPolicy {
    /// Prefer lowest configured `priority`, breaking ties by fewer active sessions and highest quota.
    #[default]
    Priority,
    /// Prefer highest known remaining quota percentage, then fewer sessions and lower `priority`.
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
        f.pad(self.as_str())
    }
}

/// One `(provider, bucket)` step inside a cross-provider fallback chain (e.g. `agy:claude`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct FallbackTarget {
    /// Target CLI provider.
    pub provider: ProviderKind,
    /// Target model bucket within that provider.
    pub bucket: ModelBucket,
}

impl FallbackTarget {
    /// Creates a new [`FallbackTarget`].
    #[must_use]
    pub const fn new(provider: ProviderKind, bucket: ModelBucket) -> Self {
        Self { provider, bucket }
    }
}

impl FromStr for FallbackTarget {
    type Err = CoreError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let trimmed = raw.trim();
        let (provider_raw, bucket_raw) = match trimmed.split_once(':') {
            Some((p, b)) => (p, Some(b)),
            None => (trimmed, None),
        };
        let provider = provider_raw.parse::<ProviderKind>().map_err(|err| {
            CoreError::InvalidFallbackTarget {
                value: raw.to_owned(),
                reason: err.to_string(),
            }
        })?;
        let bucket = match bucket_raw {
            Some(b) => {
                b.parse::<ModelBucket>()
                    .map_err(|err| CoreError::InvalidFallbackTarget {
                        value: raw.to_owned(),
                        reason: err.to_string(),
                    })?
            }
            None => ModelBucket::default_bucket(),
        };
        Ok(Self { provider, bucket })
    }
}

impl TryFrom<String> for FallbackTarget {
    type Error = CoreError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<FallbackTarget> for String {
    fn from(target: FallbackTarget) -> Self {
        target.to_string()
    }
}

impl fmt::Display for FallbackTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.provider, self.bucket)
    }
}

/// Selects the best available account for a `(provider, model_bucket)` request.
#[derive(Debug, Clone, Copy)]
pub struct AccountRouter {
    policy: SelectionPolicy,
    require_quota: bool,
}

impl AccountRouter {
    /// Creates a router using `policy`.
    #[must_use]
    pub const fn new(policy: SelectionPolicy) -> Self {
        Self {
            policy,
            require_quota: false,
        }
    }

    /// Requires a current, positive quota measurement for automatic selection.
    #[must_use]
    pub const fn require_quota(mut self, required: bool) -> Self {
        self.require_quota = required;
        self
    }

    /// Returns whether the latest account status satisfies this router.
    #[must_use]
    pub fn accepts(
        &self,
        status: Option<&AccountStatus>,
        bucket: &ModelBucket,
        now: DateTime<Utc>,
    ) -> bool {
        if self.require_quota {
            return status.is_some_and(|st| {
                st.last_probe_error.is_none()
                    && st.can_route(bucket, now)
                    && st
                        .lowest_remaining_percent(bucket, now)
                        .is_some_and(|quota| !quota.is_exhausted())
            });
        }
        status.is_none_or(|st| st.can_route(bucket, now))
    }

    /// Resolves the concrete bucket eligible for the request.
    #[must_use]
    pub fn resolve_bucket(
        &self,
        provider: ProviderKind,
        status: Option<&AccountStatus>,
        requested: &ModelBucket,
        now: DateTime<Utc>,
    ) -> Option<ModelBucket> {
        if self.require_quota && provider == ProviderKind::Opencode {
            let status = status?;
            if requested == &ModelBucket::default_bucket() {
                return status
                    .model_buckets
                    .keys()
                    .filter(|model| self.accepts(Some(status), model, now))
                    .max_by_key(|model| {
                        (status.lowest_remaining_percent(model, now), Reverse(*model))
                    })
                    .cloned();
            }
            return (status.model_buckets.contains_key(requested)
                && self.accepts(Some(status), requested, now))
            .then(|| requested.clone());
        }
        if self.require_quota
            && provider == ProviderKind::Claude
            && requested == &ModelBucket::default_bucket()
            && let Some(status) = status
            && status
                .windows
                .iter()
                .any(|window| window.remaining.is_some() && window.bucket != *requested)
        {
            // A family-specific limit requires an explicit model to avoid running the wrong family.
            return None;
        }
        self.accepts(status, requested, now)
            .then(|| requested.clone())
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
        let mut candidates: Vec<_> = accounts
            .iter()
            .filter(|account| account.enabled && account.provider == provider)
            .filter_map(|account| {
                let status = statuses
                    .iter()
                    .find(|status| status.account_id == account.id);
                let resolved = self.resolve_bucket(provider, status, bucket, now)?;
                Some((account, status, resolved))
            })
            .collect();

        match self.policy {
            SelectionPolicy::Priority => {
                candidates.sort_by_key(|(account, status, bucket)| {
                    let active = status.map_or(0, |st| st.active_sessions);
                    let remaining = status.and_then(|st| st.lowest_remaining_percent(bucket, now));
                    (
                        account.priority,
                        active,
                        Reverse(remaining),
                        account.id.clone(),
                    )
                });
            }
            SelectionPolicy::MostQuota => {
                candidates.sort_by_key(|(account, status, bucket)| {
                    let active = status.map_or(0, |st| st.active_sessions);
                    let remaining = status.and_then(|st| st.lowest_remaining_percent(bucket, now));
                    (
                        Reverse(remaining),
                        active,
                        account.priority,
                        account.id.clone(),
                    )
                });
            }
            SelectionPolicy::SoonestReset => {
                candidates.sort_by_key(|(account, status, bucket)| {
                    let active = status.map_or(0, |st| st.active_sessions);
                    let reset_ts = status
                        .and_then(|st| st.soonest_reset(bucket, now))
                        .map_or(i64::MAX, |dt| dt.timestamp());
                    (reset_ts, active, account.priority, account.id.clone())
                });
            }
        }

        candidates
            .first()
            .map(|(account, _, _)| *account)
            .ok_or_else(|| CoreError::NoAvailableAccount {
                provider,
                bucket: bucket.clone(),
            })
    }

    /// Evaluates a cross-provider fallback chain in order and returns the first available account.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::NoAvailableAccountInChain`] if every target in `chain` is exhausted.
    pub fn select_from_chain<'a>(
        &self,
        accounts: &'a [AccountConfig],
        statuses: &[AccountStatus],
        chain: &[FallbackTarget],
        now: DateTime<Utc>,
    ) -> Result<(&'a AccountConfig, ModelBucket), CoreError> {
        for target in chain {
            if let Ok(account) =
                self.select(accounts, statuses, target.provider, &target.bucket, now)
                && let Some(bucket) = self.resolve_bucket(
                    target.provider,
                    statuses
                        .iter()
                        .find(|status| status.account_id == account.id),
                    &target.bucket,
                    now,
                )
            {
                return Ok((account, bucket));
            }
        }
        Err(CoreError::NoAvailableAccountInChain)
    }
}

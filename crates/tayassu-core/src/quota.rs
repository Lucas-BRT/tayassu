use crate::account::{AccountId, ModelBucket};
use crate::error::CoreError;
use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Minimum remaining quota percentage.
pub const MIN_REMAINING_PERCENT: u8 = 0;

/// Maximum remaining quota percentage.
pub const MAX_REMAINING_PERCENT: u8 = 100;

/// Default fallback cooldown duration (5 hours) when a CLI omits an explicit reset timestamp.
pub const DEFAULT_FALLBACK_COOLDOWN_SECS: i64 = 5 * 60 * 60;

/// Percentage of quota remaining in a window (`0..=100`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct RemainingPercent(u8);

impl RemainingPercent {
    /// Returns the raw integer value (`0..=100`).
    #[must_use]
    pub const fn value(self) -> u8 {
        self.0
    }

    /// Returns `true` when zero quota remains.
    #[must_use]
    pub const fn is_exhausted(self) -> bool {
        self.0 == MIN_REMAINING_PERCENT
    }
}

impl TryFrom<u8> for RemainingPercent {
    type Error = CoreError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value > MAX_REMAINING_PERCENT {
            return Err(CoreError::InvalidRemainingPercent {
                value,
                min: MIN_REMAINING_PERCENT,
                max: MAX_REMAINING_PERCENT,
            });
        }
        Ok(Self(value))
    }
}

impl From<RemainingPercent> for u8 {
    fn from(percent: RemainingPercent) -> Self {
        percent.0
    }
}

impl fmt::Display for RemainingPercent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}%", self.0)
    }
}

/// Snapshot of a single rolling or weekly quota window reported by a provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaWindow {
    /// Human-readable window label (e.g. `5h`, `weekly`, `claude-5h`).
    pub window_name: String,
    /// Model bucket this window applies to.
    #[serde(default)]
    pub bucket: ModelBucket,
    /// Remaining quota percentage if known.
    pub remaining: Option<RemainingPercent>,
    /// Next reset instant if reported by the provider.
    pub resets_at: Option<DateTime<Utc>>,
}

impl QuotaWindow {
    /// Returns `true` when this window is exhausted and has not reset yet.
    #[must_use]
    pub fn is_exhausted_at(&self, now: DateTime<Utc>) -> bool {
        let exhausted = self.remaining.is_some_and(RemainingPercent::is_exhausted);
        let not_reset = self.resets_at.is_none_or(|reset| reset > now);
        exhausted && not_reset
    }
}

/// Active or historical cooldown recorded for an `(account, bucket)` pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CooldownEntry {
    /// Model bucket affected by the cooldown.
    pub bucket: ModelBucket,
    /// Instant when the cooldown expires.
    pub resets_at: DateTime<Utc>,
    /// Source or reason for the cooldown (e.g. `manual`, `probe`, `stderr-429`).
    pub reason: String,
}

impl CooldownEntry {
    /// Returns `true` if the cooldown is still active at `now`.
    #[must_use]
    pub fn is_active(&self, now: DateTime<Utc>) -> bool {
        self.resets_at > now
    }

    /// Returns the remaining duration until reset, or `None` if already expired.
    #[must_use]
    pub fn remaining(&self, now: DateTime<Utc>) -> Option<TimeDelta> {
        let delta = self.resets_at.signed_duration_since(now);
        if delta > TimeDelta::zero() {
            Some(delta)
        } else {
            None
        }
    }
}

/// Mutable runtime state tracked locally for one account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountStatus {
    /// Account this status belongs to.
    pub account_id: AccountId,
    /// Last time an active probe refreshed this account.
    pub last_probed_at: Option<DateTime<Utc>>,
    /// Latest quota windows observed from probes.
    #[serde(default)]
    pub windows: Vec<QuotaWindow>,
    /// Recorded cooldown entries per model bucket.
    #[serde(default)]
    pub cooldowns: Vec<CooldownEntry>,
    /// Number of concurrent `tayassu run` sessions currently using this account.
    #[serde(default)]
    pub active_sessions: u32,
}

impl AccountStatus {
    /// Creates an empty runtime status for `account_id`.
    #[must_use]
    pub fn empty(account_id: AccountId) -> Self {
        Self {
            account_id,
            last_probed_at: None,
            windows: Vec::new(),
            cooldowns: Vec::new(),
            active_sessions: 0,
        }
    }

    /// Returns the active cooldown blocking `bucket` (or the default bucket) at `now`.
    #[must_use]
    pub fn active_cooldown(
        &self,
        bucket: &ModelBucket,
        now: DateTime<Utc>,
    ) -> Option<&CooldownEntry> {
        let default_bucket = ModelBucket::default_bucket();
        self.cooldowns
            .iter()
            .filter(|entry| entry.is_active(now))
            .find(|entry| &entry.bucket == bucket || entry.bucket == default_bucket)
    }

    /// Returns `true` if no active cooldown or exhausted window blocks `bucket` at `now`.
    #[must_use]
    pub fn is_available(&self, bucket: &ModelBucket, now: DateTime<Utc>) -> bool {
        if self.active_cooldown(bucket, now).is_some() {
            return false;
        }
        let default_bucket = ModelBucket::default_bucket();
        !self.windows.iter().any(|window| {
            (&window.bucket == bucket || window.bucket == default_bucket)
                && window.is_exhausted_at(now)
        })
    }

    /// Returns the minimum known remaining percentage across windows covering `bucket`.
    #[must_use]
    pub fn lowest_remaining_percent(&self, bucket: &ModelBucket) -> Option<RemainingPercent> {
        let default_bucket = ModelBucket::default_bucket();
        self.windows
            .iter()
            .filter(|window| &window.bucket == bucket || window.bucket == default_bucket)
            .filter_map(|window| window.remaining)
            .min()
    }

    /// Returns the earliest known reset timestamp among windows covering `bucket`.
    #[must_use]
    pub fn soonest_reset(&self, bucket: &ModelBucket) -> Option<DateTime<Utc>> {
        let default_bucket = ModelBucket::default_bucket();
        self.windows
            .iter()
            .filter(|window| &window.bucket == bucket || window.bucket == default_bucket)
            .filter_map(|window| window.resets_at)
            .min()
    }

    /// Replaces or inserts a cooldown entry for its model bucket.
    pub fn set_cooldown(&mut self, entry: CooldownEntry) {
        self.cooldowns
            .retain(|existing| existing.bucket != entry.bucket);
        self.cooldowns.push(entry);
    }

    /// Removes any recorded cooldown for `bucket`. Returns `true` if an entry was removed.
    pub fn clear_cooldown(&mut self, bucket: &ModelBucket) -> bool {
        let before = self.cooldowns.len();
        self.cooldowns.retain(|entry| &entry.bucket != bucket);
        self.cooldowns.len() < before
    }

    /// Drops cooldown entries that have already expired at `now`.
    pub fn prune_expired(&mut self, now: DateTime<Utc>) {
        self.cooldowns.retain(|entry| entry.is_active(now));
    }
}

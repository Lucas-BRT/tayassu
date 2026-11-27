use crate::account::{AccountId, DEFAULT_MODEL_BUCKET, ModelBucket};
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

/// Reason prefix marking cooldowns inferred from CLI output rather than reported by a provider.
pub const PASSIVE_COOLDOWN_REASON_PREFIX: &str = "detected-rate-limit";

/// Reason prefix for cooldowns derived from AGY quota probes.
pub const AGY_PROBE_COOLDOWN_REASON_PREFIX: &str = "agy-probe:";

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

/// Classification of the most recent failed probe for an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProbeFailureKind {
    /// The isolated profile has no valid credentials.
    Authentication,
    /// Any other probe failure.
    Other,
}

/// Detail about the most recent failed probe, persisted in `state.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeFailure {
    /// Failure category used by the UI.
    pub kind: ProbeFailureKind,
    /// Human-readable failure message.
    pub message: String,
}

impl ProbeFailure {
    /// Creates an authentication failure.
    #[must_use]
    pub fn authentication(message: impl Into<String>) -> Self {
        Self {
            kind: ProbeFailureKind::Authentication,
            message: message.into(),
        }
    }

    /// Creates a non-authentication failure.
    #[must_use]
    pub fn other(message: impl Into<String>) -> Self {
        Self {
            kind: ProbeFailureKind::Other,
            message: message.into(),
        }
    }

    /// Returns `true` when the profile needs a fresh login.
    #[must_use]
    pub const fn is_authentication(&self) -> bool {
        matches!(self.kind, ProbeFailureKind::Authentication)
    }
}

/// A measured `OpenRouter` key budget, kept separate from request quota.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyBudget {
    /// USD remaining, formatted as a decimal amount.
    pub remaining_usd: String,
    /// Reset interval when reported by the key service.
    pub reset_interval: Option<String>,
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
    /// Discovered model IDs and the integration quota bucket that applies to each.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub model_buckets: std::collections::BTreeMap<ModelBucket, ModelBucket>,
    /// `OpenRouter` key budget, when provided by the current probe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_budget: Option<KeyBudget>,
    /// Recorded cooldown entries per model bucket.
    #[serde(default)]
    pub cooldowns: Vec<CooldownEntry>,
    /// Most recent failed probe, cleared on the next successful probe.
    #[serde(default)]
    pub last_probe_error: Option<ProbeFailure>,
    /// Number of concurrent `tayassu run` sessions currently using this account.
    #[serde(default)]
    pub active_sessions: u32,
    /// Process ids of the sessions counted in [`Self::active_sessions`], used to drop sessions
    /// whose `tayassu` process was killed without running its destructors.
    #[serde(default)]
    pub session_pids: Vec<u32>,
}

impl AccountStatus {
    /// Creates an empty runtime status for `account_id`.
    #[must_use]
    pub fn empty(account_id: AccountId) -> Self {
        Self {
            account_id,
            last_probed_at: None,
            windows: Vec::new(),
            model_buckets: std::collections::BTreeMap::new(),
            key_budget: None,
            cooldowns: Vec::new(),
            last_probe_error: None,
            active_sessions: 0,
            session_pids: Vec::new(),
        }
    }

    /// Registers a live session owned by process `pid`.
    pub fn add_session(&mut self, pid: u32) {
        if !self.session_pids.contains(&pid) {
            self.session_pids.push(pid);
        }
        self.sync_session_count();
    }

    /// Unregisters the session owned by process `pid`.
    pub fn remove_session(&mut self, pid: u32) {
        self.session_pids.retain(|&owner| owner != pid);
        self.sync_session_count();
    }

    /// Returns `true` when the stored count disagrees with the sessions still alive.
    #[must_use]
    pub fn sessions_stale(&self, is_alive: impl Fn(u32) -> bool) -> bool {
        let live = self
            .session_pids
            .iter()
            .filter(|&&pid| is_alive(pid))
            .count();
        u32::try_from(live) != Ok(self.active_sessions)
    }

    /// Drops sessions whose owning process is gone and resyncs the counter.
    pub fn prune_sessions(&mut self, is_alive: impl Fn(u32) -> bool) {
        self.session_pids.retain(|&pid| is_alive(pid));
        self.sync_session_count();
    }

    fn sync_session_count(&mut self) {
        self.active_sessions = u32::try_from(self.session_pids.len()).unwrap_or(u32::MAX);
    }

    /// Records a failed probe.
    pub fn set_probe_failure(&mut self, failure: ProbeFailure) {
        self.last_probe_error = Some(failure);
    }

    /// Clears any recorded probe failure after a successful probe.
    pub fn clear_probe_failure(&mut self) {
        self.last_probe_error = None;
    }

    /// Returns whether the last probe explicitly reported missing or expired credentials.
    #[must_use]
    pub fn needs_login(&self) -> bool {
        self.last_probe_error
            .as_ref()
            .is_some_and(ProbeFailure::is_authentication)
    }

    /// Returns whether automatic routing can use this account and model.
    #[must_use]
    pub fn can_route(&self, bucket: &ModelBucket, now: DateTime<Utc>) -> bool {
        !self.needs_login() && self.is_available(bucket, now)
    }

    fn covers(&self, scope: &ModelBucket, target: &ModelBucket) -> bool {
        scope == target
            || scope.as_str() == DEFAULT_MODEL_BUCKET
            || self.model_buckets.get(target) == Some(scope)
    }

    /// Returns the active cooldown blocking `bucket` (or the default bucket) at `now`.
    #[must_use]
    pub fn active_cooldown(
        &self,
        bucket: &ModelBucket,
        now: DateTime<Utc>,
    ) -> Option<&CooldownEntry> {
        self.cooldowns
            .iter()
            .filter(|entry| entry.is_active(now))
            .filter(|entry| self.covers(&entry.bucket, bucket))
            .max_by_key(|entry| entry.resets_at)
    }

    /// Returns `true` if no active cooldown or exhausted window blocks `bucket` at `now`.
    #[must_use]
    pub fn is_available(&self, bucket: &ModelBucket, now: DateTime<Utc>) -> bool {
        if self.active_cooldown(bucket, now).is_some() {
            return false;
        }
        !self
            .windows
            .iter()
            .any(|window| self.covers(&window.bucket, bucket) && window.is_exhausted_at(now))
    }

    /// Returns the time at which all currently blocking limits will have lifted, if known.
    #[must_use]
    pub fn next_available_at(
        &self,
        bucket: &ModelBucket,
        now: DateTime<Utc>,
    ) -> Option<DateTime<Utc>> {
        let mut latest = self
            .active_cooldown(bucket, now)
            .map(|entry| entry.resets_at);
        for window in self
            .windows
            .iter()
            .filter(|window| self.covers(&window.bucket, bucket) && window.is_exhausted_at(now))
        {
            let reset = window.resets_at?;
            latest = Some(latest.map_or(reset, |known| known.max(reset)));
        }
        latest
    }

    /// Returns the minimum known remaining percentage across windows covering `bucket`.
    #[must_use]
    pub fn lowest_remaining_percent(
        &self,
        bucket: &ModelBucket,
        now: DateTime<Utc>,
    ) -> Option<RemainingPercent> {
        self.windows
            .iter()
            .filter(|window| self.covers(&window.bucket, bucket))
            .filter(|window| window.resets_at.is_none_or(|reset| reset > now))
            .filter_map(|window| window.remaining)
            .min()
    }

    /// Returns the earliest known reset timestamp among windows covering `bucket`.
    #[must_use]
    pub fn soonest_reset(&self, bucket: &ModelBucket, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.windows
            .iter()
            .filter(|window| self.covers(&window.bucket, bucket))
            .filter(|window| window.remaining.is_some())
            .filter_map(|window| window.resets_at)
            .filter(|reset| *reset > now)
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

    /// Reconciles probe-derived cooldowns with a new successful provider snapshot.
    pub fn refresh_probe_windows(
        &mut self,
        windows: Vec<QuotaWindow>,
        cooldowns: Vec<CooldownEntry>,
        now: DateTime<Utc>,
    ) {
        self.windows = windows;
        self.prune_expired(now);
        self.cooldowns
            .retain(|entry| !entry.reason.starts_with(AGY_PROBE_COOLDOWN_REASON_PREFIX));
        self.clear_contradicted_passive_cooldowns();
        for cooldown in cooldowns {
            match self
                .cooldowns
                .iter_mut()
                .find(|entry| entry.bucket == cooldown.bucket)
            {
                None => self.cooldowns.push(cooldown),
                Some(entry)
                    if entry.reason.starts_with(AGY_PROBE_COOLDOWN_REASON_PREFIX)
                        && entry.resets_at < cooldown.resets_at =>
                {
                    *entry = cooldown;
                }
                Some(_) => {}
            }
        }
    }

    /// Removes passively detected cooldowns contradicted by fresh windows that still show quota.
    pub fn clear_contradicted_passive_cooldowns(&mut self) {
        let default_bucket = ModelBucket::default_bucket();
        let windows = &self.windows;
        self.cooldowns.retain(|entry| {
            if !entry.reason.starts_with(PASSIVE_COOLDOWN_REASON_PREFIX) {
                return true;
            }
            let covering = windows
                .iter()
                .filter(|window| window.bucket == entry.bucket || window.bucket == default_bucket)
                .filter_map(|window| window.remaining);
            // Keep the cooldown unless every covering window reports quota left.
            let mut seen = false;
            for remaining in covering {
                seen = true;
                if remaining.is_exhausted() {
                    return true;
                }
            }
            !seen
        });
    }

    /// Drops cooldown entries that have already expired at `now`.
    pub fn prune_expired(&mut self, now: DateTime<Utc>) {
        self.cooldowns.retain(|entry| entry.is_active(now));
    }
}

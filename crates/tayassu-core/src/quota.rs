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

/// Minutes a probe stays "fresh" before its readings are shown as estimates.
pub const FRESH_WINDOW_MINUTES: i64 = 15;

/// How trustworthy a cached quota reading is at a given instant (derived, never persisted).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WindowState {
    /// Probed recently; the percentage is a live reading.
    Fresh,
    /// Probed a while ago; the percentage is the last known value.
    Stale,
    /// The recorded reset instant has passed; the quota has presumably been restored.
    Reset,
    /// Never probed.
    Unknown,
}

impl WindowState {
    /// Lowercase name used in JSON and text output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Stale => "stale",
            Self::Reset => "reset",
            Self::Unknown => "unknown",
        }
    }
}

/// Reason prefix marking cooldowns inferred from CLI output rather than reported by a provider.
pub const PASSIVE_COOLDOWN_REASON_PREFIX: &str = "detected-rate-limit";

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
    /// Returns `true` when the recorded reset instant has already passed at `now`.
    #[must_use]
    pub fn has_reset_at(&self, now: DateTime<Utc>) -> bool {
        self.resets_at.is_some_and(|reset| reset <= now)
    }

    /// Classifies this reading given when its account was last probed.
    #[must_use]
    pub fn state(&self, probed_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> WindowState {
        if self.has_reset_at(now) {
            return WindowState::Reset;
        }
        match probed_at {
            None => WindowState::Unknown,
            Some(at)
                if now.signed_duration_since(at) < TimeDelta::minutes(FRESH_WINDOW_MINUTES) =>
            {
                WindowState::Fresh
            }
            Some(_) => WindowState::Stale,
        }
    }

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
    ///
    /// Windows whose reset instant has passed are skipped: their percentage is outdated.
    #[must_use]
    pub fn lowest_remaining_percent(
        &self,
        bucket: &ModelBucket,
        now: DateTime<Utc>,
    ) -> Option<RemainingPercent> {
        self.covering(bucket)
            .filter(|window| !window.has_reset_at(now))
            .filter_map(|window| window.remaining)
            .min()
    }

    /// Windows that apply to `bucket` (its own and the shared default bucket).
    fn covering<'a>(&'a self, bucket: &'a ModelBucket) -> impl Iterator<Item = &'a QuotaWindow> {
        let default_bucket = ModelBucket::default_bucket();
        self.windows
            .iter()
            .filter(move |window| &window.bucket == bucket || window.bucket == default_bucket)
    }

    /// Worst reading state across windows covering `bucket` (`Reset` > `Stale` > `Unknown` > `Fresh`).
    #[must_use]
    pub fn bucket_state(&self, bucket: &ModelBucket, now: DateTime<Utc>) -> Option<WindowState> {
        let probed_at = self.last_probed_at;
        let states: Vec<_> = self
            .covering(bucket)
            .map(|window| window.state(probed_at, now))
            .collect();
        [
            WindowState::Reset,
            WindowState::Stale,
            WindowState::Unknown,
            WindowState::Fresh,
        ]
        .into_iter()
        .find(|candidate| states.contains(candidate))
    }

    /// Compact quota text for `bucket`: `43%`, `~43%` (stale), `reset✓` or `ok` (no percentage).
    #[must_use]
    pub fn quota_label(&self, bucket: &ModelBucket, now: DateTime<Utc>) -> String {
        match self.bucket_state(bucket, now) {
            Some(WindowState::Reset) => "reset✓".to_owned(),
            state => self.lowest_remaining_percent(bucket, now).map_or_else(
                || "ok".to_owned(),
                |pct| match state {
                    Some(WindowState::Fresh) => pct.to_string(),
                    _ => format!("~{pct}"),
                },
            ),
        }
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

    /// Stores freshly probed `windows` as the source of truth.
    ///
    /// Returns the buckets whose cached reset had passed (so they were assumed restored) but
    /// which the provider now reports as exhausted again.
    pub fn replace_windows(
        &mut self,
        windows: Vec<QuotaWindow>,
        now: DateTime<Utc>,
    ) -> Vec<ModelBucket> {
        let assumed_restored: Vec<ModelBucket> = self
            .windows
            .iter()
            .filter(|window| window.has_reset_at(now))
            .map(|window| window.bucket.clone())
            .collect();
        self.last_probed_at = Some(now);
        self.windows = windows;
        assumed_restored
            .into_iter()
            .filter(|bucket| {
                self.windows
                    .iter()
                    .any(|window| &window.bucket == bucket && window.is_exhausted_at(now))
            })
            .collect()
    }

    /// Drops cooldown entries that have already expired at `now`.
    pub fn prune_expired(&mut self, now: DateTime<Utc>) {
        self.cooldowns.retain(|entry| entry.is_active(now));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hours between the probe and "now" in the stale fixture.
    const STALE_HOURS: i64 = 2;

    fn window(remaining: u8, resets_in: TimeDelta, now: DateTime<Utc>) -> QuotaWindow {
        QuotaWindow {
            window_name: "5h".to_owned(),
            bucket: ModelBucket::default_bucket(),
            remaining: RemainingPercent::try_from(remaining).ok(),
            resets_at: Some(now + resets_in),
        }
    }

    fn status_with(window: QuotaWindow, probed_at: Option<DateTime<Utc>>) -> AccountStatus {
        let mut status = AccountStatus::empty("a".parse().expect("id"));
        status.last_probed_at = probed_at;
        status.windows = vec![window];
        status
    }

    #[test]
    fn window_state_follows_probe_age_and_reset_instant() {
        let now = Utc::now();
        let future = TimeDelta::hours(1);
        let bucket = ModelBucket::default_bucket();

        let fresh = status_with(window(40, future, now), Some(now));
        assert_eq!(fresh.bucket_state(&bucket, now), Some(WindowState::Fresh));
        assert_eq!(fresh.quota_label(&bucket, now), "40%");

        let probed = now - TimeDelta::hours(STALE_HOURS);
        let stale = status_with(window(40, future, now), Some(probed));
        assert_eq!(stale.bucket_state(&bucket, now), Some(WindowState::Stale));
        assert_eq!(stale.quota_label(&bucket, now), "~40%");

        let never = status_with(window(40, future, now), None);
        assert_eq!(never.bucket_state(&bucket, now), Some(WindowState::Unknown));

        let reset = status_with(window(0, -TimeDelta::minutes(1), now), Some(probed));
        assert_eq!(reset.bucket_state(&bucket, now), Some(WindowState::Reset));
        assert_eq!(reset.quota_label(&bucket, now), "reset✓");
    }

    #[test]
    fn reset_window_is_available_and_has_no_known_percentage() {
        let now = Utc::now();
        let bucket = ModelBucket::default_bucket();
        let probed = now - TimeDelta::hours(STALE_HOURS);
        let status = status_with(window(0, -TimeDelta::minutes(1), now), Some(probed));

        assert!(status.is_available(&bucket, now));
        assert_eq!(status.lowest_remaining_percent(&bucket, now), None);
    }
}

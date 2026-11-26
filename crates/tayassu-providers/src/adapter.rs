use crate::error::ProviderError;
use crate::isolation::{DEFAULT_SHARED_HOME_LINKS, IsolationStrategy, ProfileEnvironment};
use chrono::{DateTime, NaiveDateTime, TimeDelta, Utc};
use std::path::Path;
use tayassu_core::{
    AccountConfig, CooldownEntry, DEFAULT_FALLBACK_COOLDOWN_SECS, ModelBucket, ProviderKind,
    QuotaWindow, RemainingPercent,
};

/// Native config directory env var supported by Claude Code CLI.
pub const CLAUDE_CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

/// Native home directory env var supported by `OpenAI` Codex CLI.
pub const CODEX_HOME_ENV: &str = "CODEX_HOME";

/// Timeout in seconds passed to `agy --print-timeout` during usage probes.
pub const AGY_PROBE_TIMEOUT_SECS: u64 = 15;

/// Substring markers indicating a CLI hit a rate or usage limit.
const RATE_LIMIT_MARKERS: &[&str] = &["rate limit", "usage limit", "quota exceeded", "429"];

/// Substring marker emitted by `agy` when a profile has no stored OAuth credentials.
const AGY_AUTH_REQUIRED_MARKER: &str = "authentication required";

/// Minimum number of whitespace-separated columns in an `agy /usage` quota row.
const AGY_MIN_ROW_COLUMNS: usize = 3;

/// Expected length of a short timezone offset like `-03` or `+05`.
const SHORT_TZ_OFFSET_LEN: usize = 3;

/// Expected length of a colon-separated timezone offset like `-03:00`.
const COLON_TZ_OFFSET_LEN: usize = 6;

/// Expected length of a compact timezone offset like `-0300`.
const COMPACT_TZ_OFFSET_LEN: usize = 5;

/// Command arguments used to probe usage non-interactively for a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeCommand {
    /// Arguments passed to the account executable.
    pub args: Vec<String>,
}

/// Structured result produced by probing a provider's usage state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProbeOutput {
    /// Quota windows reported by the provider.
    pub windows: Vec<QuotaWindow>,
    /// Cooldown entries derived from the probe output.
    pub cooldowns: Vec<CooldownEntry>,
}

/// Contract implemented by each AI CLI provider adapter.
pub trait ProviderAdapter: Send + Sync {
    /// Provider kind handled by this adapter.
    fn kind(&self) -> ProviderKind;

    /// Isolation strategy used for accounts of this provider.
    fn isolation(&self, account: &AccountConfig) -> IsolationStrategy;

    /// Arguments passed to the CLI executable for interactive account login.
    fn login_args(&self) -> Vec<String> {
        Vec::new()
    }

    /// Non-interactive probe command specification, if supported by this provider.
    fn probe_command(&self) -> Option<ProbeCommand> {
        None
    }

    /// Builds the concrete [`ProfileEnvironment`] for `account` under `profiles_root`.
    fn build_environment(
        &self,
        account: &AccountConfig,
        profiles_root: &Path,
    ) -> ProfileEnvironment {
        let profile_dir = profiles_root
            .join(self.kind().as_str())
            .join(account.id.as_str());
        ProfileEnvironment::new(profile_dir, self.isolation(account))
    }

    /// Parses raw usage output (e.g. from `/usage`) into quota windows and cooldowns.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::ProbeNotImplemented`] until the provider parser is wired.
    fn parse_usage_output(
        &self,
        _account: &AccountConfig,
        _raw_output: &str,
        _now: DateTime<Utc>,
    ) -> Result<ProbeOutput, ProviderError> {
        Err(ProviderError::ProbeNotImplemented {
            provider: self.kind(),
        })
    }

    /// Passively inspects CLI output for rate-limit signals and builds a fallback cooldown.
    fn detect_rate_limit(
        &self,
        _account: &AccountConfig,
        bucket: &ModelBucket,
        output: &str,
        now: DateTime<Utc>,
    ) -> Option<CooldownEntry> {
        let lowered = output.to_ascii_lowercase();
        let matched = RATE_LIMIT_MARKERS
            .iter()
            .any(|marker| lowered.contains(marker));
        if !matched {
            return None;
        }
        Some(CooldownEntry {
            bucket: bucket.clone(),
            resets_at: now + TimeDelta::seconds(DEFAULT_FALLBACK_COOLDOWN_SECS),
            reason: format!("detected-rate-limit:{}", self.kind()),
        })
    }
}

/// Adapter for Antigravity CLI (`agy`).
#[derive(Debug, Default, Clone, Copy)]
pub struct AgyAdapter;

impl AgyAdapter {
    fn normalize_bucket(group: &str) -> Result<ModelBucket, ProviderError> {
        let lowered = group.trim().to_ascii_lowercase();
        let slug = if lowered.contains("gemini") {
            "gemini".to_owned()
        } else if lowered.contains("claude") {
            "claude".to_owned()
        } else {
            lowered
                .chars()
                .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
                .collect::<String>()
                .trim_matches('-')
                .to_owned()
        };

        slug.parse().map_err(|err| ProviderError::ParseUsage {
            provider: ProviderKind::Agy,
            reason: format!("invalid model group '{group}': {err}"),
        })
    }

    fn normalize_window_name(raw_label: &str) -> String {
        let lowered = raw_label.trim().to_ascii_lowercase();
        if lowered.contains("five hour") || lowered.contains("5 hour") || lowered.contains("5h") {
            "5h".to_owned()
        } else if lowered.contains("weekly") || lowered.contains("week") {
            "weekly".to_owned()
        } else {
            lowered.replace(' ', "-")
        }
    }

    fn parse_reset_timestamp(raw: &str) -> Result<DateTime<Utc>, ProviderError> {
        let trimmed = raw.trim();
        let normalized = normalize_offset_suffix(trimmed);

        if let Ok(dt) = DateTime::parse_from_str(&normalized, "%Y-%m-%d %H:%M %z") {
            return Ok(dt.with_timezone(&Utc));
        }
        if let Ok(dt) = DateTime::parse_from_rfc3339(&normalized) {
            return Ok(dt.with_timezone(&Utc));
        }
        if let Ok(naive) = NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M") {
            return Ok(naive.and_utc());
        }

        Err(ProviderError::ParseUsage {
            provider: ProviderKind::Agy,
            reason: format!("unrecognized reset timestamp '{trimmed}'"),
        })
    }
}

impl ProviderAdapter for AgyAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Agy
    }

    fn isolation(&self, account: &AccountConfig) -> IsolationStrategy {
        IsolationStrategy::FakeHome {
            shared_links: resolved_shared_links(account),
        }
    }

    fn login_args(&self) -> Vec<String> {
        vec!["--print=/usage".to_owned()]
    }

    fn probe_command(&self) -> Option<ProbeCommand> {
        Some(ProbeCommand {
            args: vec![
                "--print-timeout".to_owned(),
                format!("{AGY_PROBE_TIMEOUT_SECS}s"),
                "--print=/usage".to_owned(),
            ],
        })
    }

    fn parse_usage_output(
        &self,
        account: &AccountConfig,
        raw_output: &str,
        now: DateTime<Utc>,
    ) -> Result<ProbeOutput, ProviderError> {
        if raw_output
            .to_ascii_lowercase()
            .contains(AGY_AUTH_REQUIRED_MARKER)
        {
            return Err(ProviderError::AuthenticationRequired {
                provider: ProviderKind::Agy,
                account_id: account.id.clone(),
            });
        }

        let mut output = ProbeOutput::default();

        for line in raw_output.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("quota:") {
                continue;
            }

            let columns = split_table_columns(trimmed);
            if columns.len() < AGY_MIN_ROW_COLUMNS {
                continue;
            }

            let bucket = Self::normalize_bucket(columns[0])?;
            let window_name = Self::normalize_window_name(columns[1]);

            let pct_raw = columns[2].trim_end_matches('%');
            let pct_value: u8 = pct_raw.parse().map_err(|err| ProviderError::ParseUsage {
                provider: ProviderKind::Agy,
                reason: format!(
                    "invalid percentage '{}' in line '{trimmed}': {err}",
                    columns[2]
                ),
            })?;
            let remaining =
                RemainingPercent::try_from(pct_value).map_err(|err| ProviderError::ParseUsage {
                    provider: ProviderKind::Agy,
                    reason: format!("out-of-range percentage in line '{trimmed}': {err}"),
                })?;

            let resets_at = columns
                .get(3)
                .map(|raw_ts| Self::parse_reset_timestamp(raw_ts))
                .transpose()?;

            if remaining.is_exhausted()
                && let Some(reset_instant) = resets_at
                && reset_instant > now
            {
                output.cooldowns.push(CooldownEntry {
                    bucket: bucket.clone(),
                    resets_at: reset_instant,
                    reason: format!("agy-probe:{window_name}"),
                });
            }

            output.windows.push(QuotaWindow {
                window_name,
                bucket,
                remaining: Some(remaining),
                resets_at,
            });
        }

        if output.windows.is_empty() {
            return Err(ProviderError::ParseUsage {
                provider: ProviderKind::Agy,
                reason: "no quota rows found in /usage output".to_owned(),
            });
        }

        Ok(output)
    }
}

/// Adapter for Claude Code CLI (`claude`).
#[derive(Debug, Default, Clone, Copy)]
pub struct ClaudeAdapter;

impl ProviderAdapter for ClaudeAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Claude
    }

    fn isolation(&self, _account: &AccountConfig) -> IsolationStrategy {
        IsolationStrategy::NativeEnvVar {
            var_name: CLAUDE_CONFIG_DIR_ENV,
        }
    }
}

/// Adapter for `OpenAI` Codex CLI (`codex`).
#[derive(Debug, Default, Clone, Copy)]
pub struct CodexAdapter;

impl ProviderAdapter for CodexAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Codex
    }

    fn isolation(&self, _account: &AccountConfig) -> IsolationStrategy {
        IsolationStrategy::NativeEnvVar {
            var_name: CODEX_HOME_ENV,
        }
    }
}

/// Generic adapter for custom subscription-authenticated CLIs.
#[derive(Debug, Default, Clone, Copy)]
pub struct CustomAdapter;

impl ProviderAdapter for CustomAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Custom
    }

    fn isolation(&self, account: &AccountConfig) -> IsolationStrategy {
        IsolationStrategy::FakeHome {
            shared_links: resolved_shared_links(account),
        }
    }
}

/// Registry resolving a [`ProviderKind`] to its concrete [`ProviderAdapter`].
#[derive(Debug, Default, Clone, Copy)]
pub struct AdapterRegistry;

impl AdapterRegistry {
    /// Returns the adapter responsible for `kind`.
    #[must_use]
    pub fn adapter_for(self, kind: ProviderKind) -> Box<dyn ProviderAdapter> {
        match kind {
            ProviderKind::Agy => Box::new(AgyAdapter),
            ProviderKind::Claude => Box::new(ClaudeAdapter),
            ProviderKind::Codex => Box::new(CodexAdapter),
            ProviderKind::Custom => Box::new(CustomAdapter),
        }
    }
}

fn resolved_shared_links(account: &AccountConfig) -> Vec<String> {
    if account.shared_home_links.is_empty() {
        DEFAULT_SHARED_HOME_LINKS
            .iter()
            .map(|entry| (*entry).to_owned())
            .collect()
    } else {
        account.shared_home_links.clone()
    }
}

/// Splits a whitespace-aligned table row on runs of two or more ASCII spaces.
fn split_table_columns(line: &str) -> Vec<&str> {
    let mut columns = Vec::new();
    let bytes = line.as_bytes();
    let mut start = 0;
    let mut idx = 0;

    while idx < bytes.len() {
        if bytes[idx] == b' ' && idx + 1 < bytes.len() && bytes[idx + 1] == b' ' {
            let segment = line[start..idx].trim();
            if !segment.is_empty() {
                columns.push(segment);
            }
            while idx < bytes.len() && bytes[idx] == b' ' {
                idx += 1;
            }
            start = idx;
        } else {
            idx += 1;
        }
    }

    let tail = line[start..].trim();
    if !tail.is_empty() {
        columns.push(tail);
    }
    columns
}

/// Expands short timezone offsets (`-03`, `+05`) into `+HHMM` / `-HHMM` for `chrono`.
fn normalize_offset_suffix(raw: &str) -> String {
    let Some((prefix, tz)) = raw.rsplit_once(' ') else {
        return raw.to_owned();
    };
    let tz_bytes = tz.as_bytes();
    if tz_bytes.len() == SHORT_TZ_OFFSET_LEN
        && matches!(tz_bytes[0], b'+' | b'-')
        && tz_bytes[1].is_ascii_digit()
        && tz_bytes[2].is_ascii_digit()
    {
        return format!("{prefix} {tz}00");
    }
    if tz_bytes.len() == COLON_TZ_OFFSET_LEN
        && matches!(tz_bytes[0], b'+' | b'-')
        && tz_bytes[3] == b':'
    {
        let compact: String = tz.chars().filter(|&ch| ch != ':').collect();
        if compact.len() == COMPACT_TZ_OFFSET_LEN {
            return format!("{prefix} {compact}");
        }
    }
    raw.to_owned()
}

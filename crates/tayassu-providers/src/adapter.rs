use crate::error::ProviderError;
use crate::isolation::{DEFAULT_SHARED_HOME_LINKS, IsolationStrategy, ProfileEnvironment};
use chrono::{DateTime, NaiveDateTime, TimeDelta, Utc};
use serde::Deserialize;
use std::env;
use std::path::Path;
use tayassu_core::{
    AccountConfig, CooldownEntry, DEFAULT_FALLBACK_COOLDOWN_SECS, ModelBucket, ProviderKind,
    QuotaWindow, RemainingPercent,
};

/// Native config directory env var supported by Claude Code CLI.
pub const CLAUDE_CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

/// Native home directory env var supported by `OpenAI` Codex CLI.
pub const CODEX_HOME_ENV: &str = "CODEX_HOME";

/// Native home directory env var supported by GitHub Copilot CLI.
pub const COPILOT_HOME_ENV: &str = "COPILOT_HOME";

/// Timeout in seconds passed to `agy --print-timeout` during usage probes.
pub const AGY_PROBE_TIMEOUT_SECS: u64 = 15;

/// Substring markers indicating a CLI hit a rate or usage limit.
const RATE_LIMIT_MARKERS: &[&str] = &[
    "rate limit",
    "usage limit",
    "quota exceeded",
    "too many requests",
    "resource_exhausted",
    "limit reached",
];

/// Phrases that introduce the time to wait inside a rate-limit message.
const RETRY_ANCHORS: &[&[&str]] = &[
    &["try", "again", "in"],
    &["retry", "after"],
    &["retry", "in"],
    &["resets", "in"],
    &["reset", "in"],
    &["wait"],
];

/// HTTP status text that only counts as a rate-limit marker when it stands alone.
const HTTP_TOO_MANY_REQUESTS: &str = "429";

/// Seconds per day used when parsing textual retry-after durations.
const SECS_PER_DAY: i64 = 24 * SECS_PER_HOUR;

/// Seconds per minute used when parsing textual retry-after durations.
const SECS_PER_MINUTE: i64 = 60;

/// Seconds per hour used when parsing textual retry-after durations.
const SECS_PER_HOUR: i64 = 3600;

/// Maximum retry-after duration (7 days in seconds) accepted from unstructured CLI stderr.
const MAX_PARSED_COOLDOWN_SECS: i64 = 7 * SECS_PER_DAY;

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

/// Environment variables checked by GitHub Copilot CLI for token authentication.
const COPILOT_TOKEN_ENV_VARS: &[&str] = &["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"];

/// Credential/config filenames written under `COPILOT_HOME` after `copilot login`.
const COPILOT_CREDENTIAL_FILES: &[&str] = &["config.json", "hosts.json"];

/// Shared links added for `agy` fake `HOME` so `agy update`, skills, and MCP configs stay unified.
const AGY_EXTRA_SHARED_LINKS: &[&str] = &[
    ".local/bin",
    ".gemini/antigravity-cli/skills",
    ".gemini/antigravity-cli/mcp",
    ".gemini/GEMINI.md",
];

/// First port of the range used for per-account `opencode` managed services (host default is 49374).
const OPENCODE_SERVICE_PORT_BASE: u16 = 50_000;

/// Number of ports in the per-account `opencode` managed-service range.
const OPENCODE_SERVICE_PORT_SPAN: u32 = 10_000;

/// FNV-1a 32-bit offset basis.
const FNV_OFFSET_BASIS: u32 = 0x811c_9dc5;

/// FNV-1a 32-bit prime.
const FNV_PRIME: u32 = 0x0100_0193;

/// Shared links added for `opencode` fake `HOME` so `opencode upgrade`, skills, and plugins stay unified.
const OPENCODE_EXTRA_SHARED_LINKS: &[&str] = &[
    ".opencode/bin",
    ".config/opencode/skills",
    ".config/opencode/plugins",
    ".config/opencode/AGENTS.md",
];

/// Command arguments used to probe usage or auth status non-interactively for a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeCommand {
    /// Arguments passed to the account executable.
    pub args: Vec<String>,
}

/// Structured result produced by probing a provider's usage or auth state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProbeOutput {
    /// Quota or auth windows reported by the provider.
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

    /// Arguments passed to the CLI executable to self-update in the real host `$HOME`.
    fn update_args(&self) -> Option<Vec<String>> {
        None
    }

    /// Non-interactive probe command specification, if supported by this provider.
    fn probe_command(&self) -> Option<ProbeCommand> {
        None
    }

    /// Optional local profile probe for providers without a non-interactive status subcommand.
    fn probe_local(
        &self,
        _account: &AccountConfig,
        _env: &ProfileEnvironment,
        _now: DateTime<Utc>,
    ) -> Option<Result<ProbeOutput, ProviderError>> {
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

    /// Parses raw usage or auth status output into quota windows and cooldowns.
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

    /// Passively inspects CLI output for rate-limit signals and builds a cooldown entry.
    fn detect_rate_limit(
        &self,
        _account: &AccountConfig,
        bucket: &ModelBucket,
        output: &str,
        now: DateTime<Utc>,
    ) -> Option<CooldownEntry> {
        let lowered = output.to_ascii_lowercase();
        let matched = mentions_status_429(&lowered)
            || RATE_LIMIT_MARKERS
                .iter()
                .any(|marker| lowered.contains(marker));
        if !matched {
            return None;
        }
        let duration_secs =
            parse_retry_after_secs(&lowered).unwrap_or(DEFAULT_FALLBACK_COOLDOWN_SECS);
        Some(CooldownEntry {
            bucket: bucket.clone(),
            resets_at: now + TimeDelta::seconds(duration_secs),
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
            slugify(group)
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
            shared_links: resolved_shared_links(account, AGY_EXTRA_SHARED_LINKS),
        }
    }

    fn login_args(&self) -> Vec<String> {
        vec!["--print=/usage".to_owned()]
    }

    fn update_args(&self) -> Option<Vec<String>> {
        Some(vec!["update".to_owned()])
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

/// Subset of JSON fields returned by `claude auth status`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeAuthStatus {
    logged_in: bool,
    #[serde(default)]
    login_expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    plan_tier: Option<String>,
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

    fn login_args(&self) -> Vec<String> {
        vec!["auth".to_owned(), "login".to_owned()]
    }

    fn update_args(&self) -> Option<Vec<String>> {
        Some(vec!["update".to_owned()])
    }

    fn probe_command(&self) -> Option<ProbeCommand> {
        Some(ProbeCommand {
            args: vec!["auth".to_owned(), "status".to_owned()],
        })
    }

    fn parse_usage_output(
        &self,
        account: &AccountConfig,
        raw_output: &str,
        now: DateTime<Utc>,
    ) -> Result<ProbeOutput, ProviderError> {
        let json_slice =
            extract_json_object(raw_output).ok_or_else(|| ProviderError::ParseUsage {
                provider: ProviderKind::Claude,
                reason: "no JSON object found in `claude auth status` output".to_owned(),
            })?;

        let status: ClaudeAuthStatus =
            serde_json::from_str(json_slice).map_err(|err| ProviderError::ParseUsage {
                provider: ProviderKind::Claude,
                reason: format!("invalid `claude auth status` JSON: {err}"),
            })?;

        if !status.logged_in || status.login_expires_at.is_some_and(|exp| exp <= now) {
            return Err(ProviderError::AuthenticationRequired {
                provider: ProviderKind::Claude,
                account_id: account.id.clone(),
            });
        }

        let window_name = status
            .plan_tier
            .as_deref()
            .filter(|tier| !tier.trim().is_empty())
            .map_or_else(
                || "auth".to_owned(),
                |tier| format!("auth:{}", tier.trim().to_ascii_lowercase()),
            );

        Ok(ProbeOutput {
            windows: vec![QuotaWindow {
                window_name,
                bucket: ModelBucket::default_bucket(),
                remaining: None,
                resets_at: status.login_expires_at,
            }],
            cooldowns: Vec::new(),
        })
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

    fn login_args(&self) -> Vec<String> {
        vec!["login".to_owned()]
    }

    fn probe_command(&self) -> Option<ProbeCommand> {
        Some(ProbeCommand {
            args: vec!["login".to_owned(), "status".to_owned()],
        })
    }

    fn parse_usage_output(
        &self,
        account: &AccountConfig,
        raw_output: &str,
        _now: DateTime<Utc>,
    ) -> Result<ProbeOutput, ProviderError> {
        let lowered = raw_output.to_ascii_lowercase();
        if lowered.contains("not logged in") || lowered.contains(AGY_AUTH_REQUIRED_MARKER) {
            return Err(ProviderError::AuthenticationRequired {
                provider: ProviderKind::Codex,
                account_id: account.id.clone(),
            });
        }

        if lowered.contains("logged in") {
            return Ok(ProbeOutput {
                windows: vec![QuotaWindow {
                    window_name: "auth".to_owned(),
                    bucket: ModelBucket::default_bucket(),
                    remaining: None,
                    resets_at: None,
                }],
                cooldowns: Vec::new(),
            });
        }

        Err(ProviderError::ParseUsage {
            provider: ProviderKind::Codex,
            reason: "unrecognized `codex login status` output".to_owned(),
        })
    }
}

/// Adapter for GitHub Copilot CLI (`copilot`).
#[derive(Debug, Default, Clone, Copy)]
pub struct CopilotAdapter;

impl ProviderAdapter for CopilotAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Copilot
    }

    fn isolation(&self, _account: &AccountConfig) -> IsolationStrategy {
        IsolationStrategy::NativeEnvVar {
            var_name: COPILOT_HOME_ENV,
        }
    }

    fn login_args(&self) -> Vec<String> {
        vec!["login".to_owned()]
    }

    fn update_args(&self) -> Option<Vec<String>> {
        Some(vec!["update".to_owned()])
    }

    fn probe_local(
        &self,
        account: &AccountConfig,
        env_profile: &ProfileEnvironment,
        _now: DateTime<Utc>,
    ) -> Option<Result<ProbeOutput, ProviderError>> {
        let has_profile_cred = COPILOT_CREDENTIAL_FILES
            .iter()
            .any(|name| env_profile.profile_dir.join(name).is_file());
        let has_env_token = COPILOT_TOKEN_ENV_VARS
            .iter()
            .any(|var| env::var_os(var).is_some_and(|val| !val.is_empty()));

        if has_profile_cred || has_env_token {
            Some(Ok(ProbeOutput {
                windows: vec![QuotaWindow {
                    window_name: "auth".to_owned(),
                    bucket: ModelBucket::default_bucket(),
                    remaining: None,
                    resets_at: None,
                }],
                cooldowns: Vec::new(),
            }))
        } else {
            Some(Err(ProviderError::AuthenticationRequired {
                provider: ProviderKind::Copilot,
                account_id: account.id.clone(),
            }))
        }
    }
}

/// Adapter for `OpenCode` CLI (`opencode`).
#[derive(Debug, Default, Clone, Copy)]
pub struct OpencodeAdapter;

impl ProviderAdapter for OpencodeAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Opencode
    }

    fn isolation(&self, account: &AccountConfig) -> IsolationStrategy {
        IsolationStrategy::FakeHome {
            shared_links: resolved_shared_links(account, OPENCODE_EXTRA_SHARED_LINKS),
        }
    }

    fn build_environment(
        &self,
        account: &AccountConfig,
        profiles_root: &Path,
    ) -> ProfileEnvironment {
        let profile_dir = profiles_root
            .join(self.kind().as_str())
            .join(account.id.as_str());
        ProfileEnvironment::new(profile_dir, self.isolation(account))
            .with_service_port(opencode_service_port(account.id.as_str()))
    }

    fn login_args(&self) -> Vec<String> {
        vec!["auth".to_owned(), "login".to_owned()]
    }

    fn update_args(&self) -> Option<Vec<String>> {
        Some(vec!["upgrade".to_owned()])
    }

    fn probe_command(&self) -> Option<ProbeCommand> {
        Some(ProbeCommand {
            args: vec!["auth".to_owned(), "list".to_owned()],
        })
    }

    fn parse_usage_output(
        &self,
        account: &AccountConfig,
        raw_output: &str,
        _now: DateTime<Utc>,
    ) -> Result<ProbeOutput, ProviderError> {
        let mut windows = Vec::new();
        let mut seen_buckets = std::collections::BTreeSet::new();

        for line in raw_output.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let columns = split_table_columns(trimmed);
            let is_stored = columns
                .last()
                .is_some_and(|col| col.eq_ignore_ascii_case("stored"));
            if !is_stored {
                continue;
            }
            let raw_provider = columns[0];
            let slug = slugify(raw_provider);
            if let Ok(bucket) = slug.parse::<ModelBucket>()
                && seen_buckets.insert(bucket.clone())
            {
                windows.push(QuotaWindow {
                    window_name: "auth".to_owned(),
                    bucket,
                    remaining: None,
                    resets_at: None,
                });
            }
        }

        if windows.is_empty() {
            return Err(ProviderError::AuthenticationRequired {
                provider: ProviderKind::Opencode,
                account_id: account.id.clone(),
            });
        }

        Ok(ProbeOutput {
            windows,
            cooldowns: Vec::new(),
        })
    }
}

/// Derives a stable per-account managed-service port so isolated `opencode` homes never share one.
fn opencode_service_port(account_id: &str) -> u16 {
    let hash = account_id.bytes().fold(FNV_OFFSET_BASIS, |acc, byte| {
        (acc ^ u32::from(byte)).wrapping_mul(FNV_PRIME)
    });
    let offset = u16::try_from(hash % OPENCODE_SERVICE_PORT_SPAN).unwrap_or_default();
    OPENCODE_SERVICE_PORT_BASE + offset
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
            shared_links: resolved_shared_links(account, &[]),
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
            ProviderKind::Copilot => Box::new(CopilotAdapter),
            ProviderKind::Opencode => Box::new(OpencodeAdapter),
            ProviderKind::Custom => Box::new(CustomAdapter),
        }
    }
}

fn resolved_shared_links(account: &AccountConfig, extra_defaults: &[&str]) -> Vec<String> {
    if account.shared_home_links.is_empty() {
        DEFAULT_SHARED_HOME_LINKS
            .iter()
            .chain(extra_defaults.iter())
            .map(|entry| (*entry).to_owned())
            .collect()
    } else {
        account.shared_home_links.clone()
    }
}

fn slugify(raw: &str) -> String {
    let mut result = String::with_capacity(raw.len());
    let mut prev_dash = false;
    for ch in raw.trim().to_ascii_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            result.push(ch);
            prev_dash = false;
        } else if !prev_dash && !result.is_empty() {
            result.push('-');
            prev_dash = true;
        }
    }
    result.trim_end_matches('-').to_owned()
}

fn extract_json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end >= start {
        Some(&raw[start..=end])
    } else {
        None
    }
}

/// Splits a whitespace-aligned table row on runs of two or more ASCII spaces.
fn split_table_columns(line: &str) -> Vec<&str> {
    if line.contains('\t') {
        return line
            .split('\t')
            .map(str::trim)
            .filter(|segment| !segment.is_empty())
            .collect();
    }

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

/// Extracts an approximate cooldown duration in seconds from rate-limit stderr messages
/// such as `"try again in 45 minutes"`, `"resets in 2h 30m"`, or `"retry-after: 120s"`.
fn parse_retry_after_secs(lowered: &str) -> Option<i64> {
    let tokens: Vec<&str> = lowered
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|tok| !tok.is_empty())
        .collect();

    let start = RETRY_ANCHORS
        .iter()
        .filter_map(|anchor| {
            tokens
                .windows(anchor.len())
                .position(|window| window == *anchor)
                .map(|pos| pos + anchor.len())
        })
        .min()
        .unwrap_or(0);

    let mut total_secs: i64 = 0;
    let mut idx = start;
    while idx < tokens.len() {
        let token = tokens[idx];
        if let Some(secs) = parse_compact_duration_token(token) {
            total_secs = total_secs.saturating_add(secs);
            idx += 1;
        } else if let Ok(value) = token.parse::<i64>()
            && value > 0
        {
            let multiplier = tokens.get(idx + 1).copied().and_then(unit_multiplier_secs);
            match multiplier {
                Some(multiplier) => {
                    total_secs = total_secs.saturating_add(value.saturating_mul(multiplier));
                    idx += 2;
                }
                // A bare number right after "retry after" is a count of seconds.
                None if total_secs == 0 && idx == start && start > 0 => {
                    total_secs = value;
                    break;
                }
                None => break,
            }
        } else if total_secs > 0 && token == "and" {
            idx += 1;
        } else if total_secs > 0 {
            break;
        } else {
            idx += 1;
        }
    }

    (total_secs > 0 && total_secs <= MAX_PARSED_COOLDOWN_SECS).then_some(total_secs)
}

/// Returns `true` when `text` mentions HTTP status 429 as a standalone number.
fn mentions_status_429(text: &str) -> bool {
    text.match_indices(HTTP_TOO_MANY_REQUESTS)
        .any(|(pos, found)| {
            let before = text[..pos].chars().next_back();
            let after = text[pos + found.len()..].chars().next();
            !before.is_some_and(|ch| ch.is_ascii_alphanumeric())
                && !after.is_some_and(|ch| ch.is_ascii_alphanumeric())
        })
}

fn parse_compact_duration_token(token: &str) -> Option<i64> {
    if token.len() < 2 {
        return None;
    }
    let (digits, suffix) = token.split_at(token.len() - 1);
    let value: i64 = digits.parse().ok()?;
    if value <= 0 {
        return None;
    }
    match suffix {
        "h" => Some(value.saturating_mul(SECS_PER_HOUR)),
        "m" => Some(value.saturating_mul(SECS_PER_MINUTE)),
        "s" => Some(value),
        "d" => Some(value.saturating_mul(SECS_PER_DAY)),
        _ => None,
    }
}

fn unit_multiplier_secs(unit: &str) -> Option<i64> {
    match unit {
        "h" | "hr" | "hrs" | "hour" | "hours" => Some(SECS_PER_HOUR),
        "m" | "min" | "mins" | "minute" | "minutes" => Some(SECS_PER_MINUTE),
        "s" | "sec" | "secs" | "second" | "seconds" => Some(1),
        "d" | "day" | "days" => Some(SECS_PER_DAY),
        _ => None,
    }
}

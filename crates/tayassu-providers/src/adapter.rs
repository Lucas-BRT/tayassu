use crate::error::ProviderError;
use crate::isolation::{DEFAULT_SHARED_HOME_LINKS, IsolationStrategy, ProfileEnvironment};
use chrono::{DateTime, TimeDelta, Utc};
use std::path::Path;
use tayassu_core::{
    AccountConfig, CooldownEntry, DEFAULT_FALLBACK_COOLDOWN_SECS, ModelBucket, ProviderKind,
    QuotaWindow,
};

/// Native config directory env var supported by Claude Code CLI.
pub const CLAUDE_CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

/// Native home directory env var supported by `OpenAI` Codex CLI.
pub const CODEX_HOME_ENV: &str = "CODEX_HOME";

/// Substring markers indicating a CLI hit a rate or usage limit.
const RATE_LIMIT_MARKERS: &[&str] = &["rate limit", "usage limit", "quota exceeded", "429"];

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

impl ProviderAdapter for AgyAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Agy
    }

    fn isolation(&self, account: &AccountConfig) -> IsolationStrategy {
        IsolationStrategy::FakeHome {
            shared_links: resolved_shared_links(account),
        }
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

use crate::error::CoreError;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Maximum length allowed for an [`AccountId`] slug.
pub const MAX_ACCOUNT_ID_LEN: usize = 64;

/// Maximum length allowed for a [`ModelBucket`] identifier.
pub const MAX_MODEL_BUCKET_LEN: usize = 64;

/// Default model bucket used when no specific sub-model is requested.
pub const DEFAULT_MODEL_BUCKET: &str = "default";

/// Default account selection priority (lower value = higher preference).
pub const DEFAULT_ACCOUNT_PRIORITY: u8 = 50;

/// Validated, filesystem-safe identifier for a configured account.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AccountId(String);

impl AccountId {
    /// Returns the underlying string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for AccountId {
    type Err = CoreError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.len() > MAX_ACCOUNT_ID_LEN {
            return Err(CoreError::InvalidAccountId {
                value: raw.to_owned(),
                reason: format!("length must be 1..={MAX_ACCOUNT_ID_LEN}"),
            });
        }
        let valid_chars = trimmed
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
        if !valid_chars {
            return Err(CoreError::InvalidAccountId {
                value: raw.to_owned(),
                reason: "only ASCII letters, digits, '-' and '_' are allowed".to_owned(),
            });
        }
        Ok(Self(trimmed.to_ascii_lowercase()))
    }
}

impl TryFrom<String> for AccountId {
    type Error = CoreError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<AccountId> for String {
    fn from(id: AccountId) -> Self {
        id.0
    }
}

impl AsRef<str> for AccountId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(&self.0)
    }
}

/// Supported AI CLI agent providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    /// Antigravity CLI (`agy`).
    Agy,
    /// Claude Code CLI (`claude`).
    Claude,
    /// `OpenAI` Codex CLI (`codex`).
    Codex,
    /// GitHub Copilot CLI (`copilot`).
    Copilot,
    /// `OpenCode` CLI (`opencode`).
    Opencode,
    /// User-defined CLI binary with explicit configuration.
    Custom,
}

impl ProviderKind {
    /// Default executable name for this provider.
    #[must_use]
    pub const fn default_binary(self) -> &'static str {
        match self {
            Self::Agy => "agy",
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Copilot => "copilot",
            Self::Opencode => "opencode",
            Self::Custom => "custom-agent",
        }
    }

    /// Canonical lowercase identifier used in config and profile paths.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Agy => "agy",
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Copilot => "copilot",
            Self::Opencode => "opencode",
            Self::Custom => "custom",
        }
    }
}

impl FromStr for ProviderKind {
    type Err = CoreError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "agy" | "antigravity" => Ok(Self::Agy),
            "claude" | "claude-code" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            "copilot" | "github-copilot" => Ok(Self::Copilot),
            "opencode" => Ok(Self::Opencode),
            "custom" => Ok(Self::Custom),
            _ => Err(CoreError::UnknownProvider {
                value: raw.to_owned(),
            }),
        }
    }
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.as_str())
    }
}

/// Quota bucket inside an account (for example `default` or `claude` inside `agy`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ModelBucket(String);

impl ModelBucket {
    /// Returns the default model bucket.
    #[must_use]
    pub fn default_bucket() -> Self {
        Self(DEFAULT_MODEL_BUCKET.to_owned())
    }

    /// Returns the normalized bucket name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for ModelBucket {
    fn default() -> Self {
        Self::default_bucket()
    }
}

impl FromStr for ModelBucket {
    type Err = CoreError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.len() > MAX_MODEL_BUCKET_LEN {
            return Err(CoreError::InvalidModelBucket {
                value: raw.to_owned(),
                reason: format!("length must be 1..={MAX_MODEL_BUCKET_LEN}"),
            });
        }
        let valid_chars = trimmed
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/'));
        if !valid_chars {
            return Err(CoreError::InvalidModelBucket {
                value: raw.to_owned(),
                reason: "only ASCII letters, digits, '-', '_', '.', and '/' are allowed".to_owned(),
            });
        }
        Ok(Self(trimmed.to_ascii_lowercase()))
    }
}

impl TryFrom<String> for ModelBucket {
    type Error = CoreError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<ModelBucket> for String {
    fn from(bucket: ModelBucket) -> Self {
        bucket.0
    }
}

impl fmt::Display for ModelBucket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(&self.0)
    }
}

/// Declarative configuration for one subscription account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountConfig {
    /// Stable local identifier (e.g. `agy-student`, `agy-pro`).
    pub id: AccountId,
    /// CLI provider this account belongs to.
    pub provider: ProviderKind,
    /// Human-friendly description shown in status output.
    pub label: String,
    /// Selection preference (lower number is preferred first).
    #[serde(default = "default_priority")]
    pub priority: u8,
    /// Optional binary path override instead of the provider default.
    #[serde(default)]
    pub binary_override: Option<String>,
    /// Whether this account participates in automatic routing.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Relative paths under `$HOME` to symlink into a fake-HOME profile.
    #[serde(default)]
    pub shared_home_links: Vec<String>,
}

const fn default_priority() -> u8 {
    DEFAULT_ACCOUNT_PRIORITY
}

const fn default_enabled() -> bool {
    true
}

impl AccountConfig {
    /// Creates a new enabled account with default priority and no overrides.
    #[must_use]
    pub fn new(id: AccountId, provider: ProviderKind, label: impl Into<String>) -> Self {
        Self {
            id,
            provider,
            label: label.into(),
            priority: DEFAULT_ACCOUNT_PRIORITY,
            binary_override: None,
            enabled: true,
            shared_home_links: Vec::new(),
        }
    }

    /// Returns the executable name or path to invoke for this account.
    #[must_use]
    pub fn executable(&self) -> &str {
        self.binary_override
            .as_deref()
            .unwrap_or_else(|| self.provider.default_binary())
    }
}

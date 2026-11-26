use crate::account::{AccountId, ModelBucket, ProviderKind};
use thiserror::Error;

/// Domain errors returned by `tayassu-core`.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CoreError {
    /// Account identifier failed validation.
    #[error("invalid account id '{value}': {reason}")]
    InvalidAccountId {
        /// Raw rejected input.
        value: String,
        /// Human-readable validation reason.
        reason: String,
    },

    /// Provider name is not recognized.
    #[error(
        "unknown provider '{value}'; expected one of: agy, claude, codex, copilot, opencode, custom"
    )]
    UnknownProvider {
        /// Raw rejected input.
        value: String,
    },

    /// Model bucket name failed validation.
    #[error("invalid model bucket '{value}': {reason}")]
    InvalidModelBucket {
        /// Raw rejected input.
        value: String,
        /// Human-readable validation reason.
        reason: String,
    },

    /// Remaining quota percentage is outside the valid `0..=100` range.
    #[error("invalid remaining percentage {value}: must be between {min} and {max}")]
    InvalidRemainingPercent {
        /// Raw rejected value.
        value: u8,
        /// Minimum allowed percentage.
        min: u8,
        /// Maximum allowed percentage.
        max: u8,
    },

    /// Selection policy name is not recognized.
    #[error(
        "unknown selection policy '{value}'; expected one of: priority, most-quota, soonest-reset"
    )]
    UnknownSelectionPolicy {
        /// Raw rejected input.
        value: String,
    },

    /// No enabled account for the requested provider and model bucket is outside cooldown.
    #[error("no available {provider} account for model bucket '{bucket}'")]
    NoAvailableAccount {
        /// Target provider.
        provider: ProviderKind,
        /// Target model bucket.
        bucket: ModelBucket,
    },

    /// Account was not found in the active registry.
    #[error("account '{id}' not found")]
    AccountNotFound {
        /// Missing account identifier.
        id: AccountId,
    },

    /// Fallback target string failed parsing (`<provider>` or `<provider>:<bucket>`).
    #[error("invalid fallback target '{value}': {reason}")]
    InvalidFallbackTarget {
        /// Raw rejected input.
        value: String,
        /// Validation reason.
        reason: String,
    },

    /// No account in any target of a fallback chain is available.
    #[error("no available account across fallback chain")]
    NoAvailableAccountInChain,
}

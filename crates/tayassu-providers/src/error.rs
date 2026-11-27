use std::path::PathBuf;
use tayassu_core::{AccountId, ProviderKind};
use thiserror::Error;

/// Errors returned by provider isolation and probing adapters.
#[derive(Debug, Error)]
pub enum ProviderError {
    /// Copied credentials differ from their source.
    #[error("copied file differs from source: {path}; source was preserved")]
    CopyVerification { path: PathBuf },
    /// Filesystem operation failed while preparing an isolated profile.
    #[error("profile filesystem error at {path}: {source}")]
    Io {
        /// Path involved in the failed operation.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// Provider does not yet implement automatic usage probing.
    #[error("automatic usage probe is not implemented yet for provider '{provider}'")]
    ProbeNotImplemented {
        /// Provider lacking probe support.
        provider: ProviderKind,
    },

    /// Isolated profile has not been authenticated yet.
    #[error(
        "account '{account_id}' ({provider}) is not logged in; run `tayassu login {account_id}`"
    )]
    AuthenticationRequired {
        /// Provider requiring authentication.
        provider: ProviderKind,
        /// Unauthenticated account identifier.
        account_id: AccountId,
    },

    /// Local configuration cannot establish which account the CLI authenticated.
    #[error("unable to verify Copilot authentication for account '{account_id}'")]
    AuthenticationUnverified {
        /// Profile whose authentication could not be verified.
        account_id: AccountId,
    },

    /// Provider usage output could not be parsed.
    #[error("failed to parse usage output for provider '{provider}': {reason}")]
    ParseUsage {
        /// Provider whose output failed parsing.
        provider: ProviderKind,
        /// Parsing failure detail.
        reason: String,
    },
}

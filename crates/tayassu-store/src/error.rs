use std::path::PathBuf;
use thiserror::Error;

/// Errors returned while loading or persisting `tayassu` configuration and state.
#[derive(Debug, Error)]
pub enum StoreError {
    /// Could not determine the user's home directory from environment variables.
    #[error("unable to resolve HOME directory from environment")]
    MissingHomeDir,

    /// Filesystem read, write, or rename failed.
    #[error("storage I/O error at {path}: {source}")]
    Io {
        /// Path involved in the failed operation.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// TOML config deserialization failed.
    #[error("failed to parse config at {path}: {source}")]
    ConfigParse {
        /// Path to the invalid config file.
        path: PathBuf,
        /// Underlying TOML parser error.
        #[source]
        source: toml::de::Error,
    },

    /// TOML config serialization failed.
    #[error("failed to serialize config: {0}")]
    ConfigSerialize(#[from] toml::ser::Error),

    /// JSON state deserialization failed.
    #[error("failed to parse state at {path}: {source}")]
    StateParse {
        /// Path to the invalid state file.
        path: PathBuf,
        /// Underlying JSON parser error.
        #[source]
        source: serde_json::Error,
    },

    /// JSON state serialization failed.
    #[error("failed to serialize state: {0}")]
    StateSerialize(#[from] serde_json::Error),
}

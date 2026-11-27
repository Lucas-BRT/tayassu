//! Dynamic shell completion candidates sourced from the user's `config.toml`.

use clap_complete::engine::CompletionCandidate;
use std::collections::BTreeSet;
use tayassu_core::SelectionPolicy;
use tayassu_store::{AppConfig, XdgPaths};

/// Loads the config, falling back to an empty one so completion never errors.
fn load_config() -> AppConfig {
    XdgPaths::from_env()
        .ok()
        .and_then(|paths| AppConfig::load(&paths.config_file).ok())
        .unwrap_or_default()
}

/// Providers that have at least one configured account.
pub fn configured_providers() -> Vec<CompletionCandidate> {
    load_config()
        .accounts
        .iter()
        .map(|account| account.provider.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(CompletionCandidate::new)
        .collect()
}

/// Ids of all configured accounts.
pub fn account_ids() -> Vec<CompletionCandidate> {
    load_config()
        .accounts
        .iter()
        .map(|account| CompletionCandidate::new(account.id.as_str()))
        .collect()
}

/// Names of configured fallback chains.
pub fn chain_names() -> Vec<CompletionCandidate> {
    load_config()
        .fallback_chains
        .keys()
        .map(CompletionCandidate::new)
        .collect()
}

/// All selection policy names.
pub fn policies() -> Vec<CompletionCandidate> {
    [
        SelectionPolicy::Priority,
        SelectionPolicy::MostQuota,
        SelectionPolicy::SoonestReset,
    ]
    .into_iter()
    .map(|policy| CompletionCandidate::new(policy.as_str()))
    .collect()
}

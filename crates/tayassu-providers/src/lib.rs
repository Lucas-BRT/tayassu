//! Provider adapters and Linux profile isolation for `tayassu`.

pub mod adapter;
pub mod error;
pub mod isolation;

pub use adapter::{
    AdapterRegistry, AgyAdapter, CLAUDE_CONFIG_DIR_ENV, CODEX_HOME_ENV, ClaudeAdapter,
    CodexAdapter, CustomAdapter, ProbeOutput, ProviderAdapter,
};
pub use error::ProviderError;
pub use isolation::{
    DEFAULT_SHARED_HOME_LINKS, ENV_HOME, FAKE_HOME_SUBDIR, IsolationStrategy, ProfileEnvironment,
};

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::fs;
    use tayassu_core::{AccountConfig, ModelBucket, ProviderKind};
    use tempfile::TempDir;

    #[test]
    fn claude_and_codex_use_native_env_vars() {
        let profiles_root = TempDir::new().expect("temp dir");
        let registry = AdapterRegistry;

        let claude = AccountConfig::new(
            "claude-pro".parse().expect("id"),
            ProviderKind::Claude,
            "Claude Pro",
        );
        let claude_env = registry
            .adapter_for(ProviderKind::Claude)
            .build_environment(&claude, profiles_root.path());
        assert_eq!(claude_env.env_vars[0].0, CLAUDE_CONFIG_DIR_ENV);

        let codex = AccountConfig::new(
            "codex-work".parse().expect("id"),
            ProviderKind::Codex,
            "Codex Work",
        );
        let codex_env = registry
            .adapter_for(ProviderKind::Codex)
            .build_environment(&codex, profiles_root.path());
        assert_eq!(codex_env.env_vars[0].0, CODEX_HOME_ENV);
    }

    #[test]
    fn agy_prepares_fake_home_with_shared_symlinks() {
        let real_home = TempDir::new().expect("real home");
        let profiles_root = TempDir::new().expect("profiles root");
        fs::write(real_home.path().join(".gitconfig"), "[user]\nname = test\n")
            .expect("write gitconfig");

        let account = AccountConfig::new(
            "agy-student".parse().expect("id"),
            ProviderKind::Agy,
            "Student",
        );
        let env = AdapterRegistry
            .adapter_for(ProviderKind::Agy)
            .build_environment(&account, profiles_root.path());

        env.prepare(real_home.path()).expect("prepare profile");

        let fake_gitconfig = env.profile_dir.join(FAKE_HOME_SUBDIR).join(".gitconfig");
        assert!(fake_gitconfig.symlink_metadata().is_ok());
        assert_eq!(env.env_vars[0].0, ENV_HOME);
    }

    #[test]
    fn passive_rate_limit_detection_records_cooldown() {
        let account = AccountConfig::new("agy-pro".parse().expect("id"), ProviderKind::Agy, "Pro");
        let now = Utc::now();
        let entry = AgyAdapter
            .detect_rate_limit(
                &account,
                &ModelBucket::default_bucket(),
                "Error: HTTP 429 rate limit exceeded",
                now,
            )
            .expect("rate limit should be detected");

        assert!(entry.is_active(now));
    }
}

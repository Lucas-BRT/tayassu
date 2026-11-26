//! Provider adapters and Linux profile isolation for `tayassu`.

pub mod adapter;
pub mod error;
pub mod isolation;

pub use adapter::{
    AGY_PROBE_TIMEOUT_SECS, AdapterRegistry, AgyAdapter, CLAUDE_CONFIG_DIR_ENV, CODEX_HOME_ENV,
    COPILOT_HOME_ENV, ClaudeAdapter, CodexAdapter, CopilotAdapter, CustomAdapter, OpencodeAdapter,
    ProbeCommand, ProbeOutput, ProviderAdapter,
};
pub use error::ProviderError;
pub use isolation::{
    DEFAULT_SHARED_HOME_LINKS, ENV_HOME, ENV_XDG_CACHE_HOME, ENV_XDG_CONFIG_HOME,
    ENV_XDG_DATA_HOME, ENV_XDG_STATE_HOME, FAKE_HOME_SUBDIR, IsolationStrategy, ProfileEnvironment,
};

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use std::fs;
    use tayassu_core::{AccountConfig, ModelBucket, ProviderKind, RemainingPercent};
    use tempfile::TempDir;

    /// Real `agy --print=/usage` output captured on 2026-11-26.
    const AGY_USAGE_FIXTURE: &str = "\
Quota:
Gemini Models          Weekly Limit Remaining     43%  2026-12-03 06:02 -03
Gemini Models          Five Hour Limit Remaining  47%  2026-11-26 23:00 -03
Claude and GPT models  Weekly Limit Remaining     50%  2026-12-03 16:38 -03
Claude and GPT models  Five Hour Limit Remaining  0%   2026-11-26 21:38 -03
";

    /// Expected number of quota rows parsed from [`AGY_USAGE_FIXTURE`].
    const EXPECTED_AGY_WINDOWS: usize = 4;

    /// Expected Gemini weekly remaining percentage in [`AGY_USAGE_FIXTURE`].
    const EXPECTED_GEMINI_WEEKLY_PCT: u8 = 43;

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
    fn agy_parses_real_usage_output_and_extracts_claude_cooldown() {
        let account = AccountConfig::new(
            "agy-student".parse().expect("id"),
            ProviderKind::Agy,
            "Student",
        );
        let now = Utc
            .with_ymd_and_hms(2026, 11, 26, 22, 30, 0)
            .single()
            .expect("valid utc instant");

        let parsed = AgyAdapter
            .parse_usage_output(&account, AGY_USAGE_FIXTURE, now)
            .expect("fixture should parse");

        assert_eq!(parsed.windows.len(), EXPECTED_AGY_WINDOWS);
        assert_eq!(parsed.windows[0].bucket.as_str(), "gemini");
        assert_eq!(parsed.windows[0].window_name, "weekly");
        assert_eq!(
            parsed.windows[0].remaining.map(RemainingPercent::value),
            Some(EXPECTED_GEMINI_WEEKLY_PCT)
        );

        assert_eq!(parsed.cooldowns.len(), 1);
        assert_eq!(parsed.cooldowns[0].bucket.as_str(), "claude");
        assert_eq!(parsed.cooldowns[0].reason, "agy-probe:5h");
        assert!(parsed.cooldowns[0].is_active(now));
    }

    #[test]
    fn agy_detects_unauthenticated_profile() {
        let account = AccountConfig::new(
            "agy-new".parse().expect("id"),
            ProviderKind::Agy,
            "New Account",
        );
        let err = AgyAdapter
            .parse_usage_output(
                &account,
                "Authentication required. Please visit the URL to log in:",
                Utc::now(),
            )
            .expect_err("unauthenticated output must error");

        assert!(matches!(
            err,
            ProviderError::AuthenticationRequired {
                provider: ProviderKind::Agy,
                ..
            }
        ));
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

    #[test]
    fn copilot_uses_native_env_and_opencode_isolates_home_and_xdg() {
        let profiles_root = TempDir::new().expect("temp dir");
        let registry = AdapterRegistry;

        let copilot = AccountConfig::new(
            "copilot-work".parse().expect("id"),
            ProviderKind::Copilot,
            "Copilot Work",
        );
        let copilot_adapter = registry.adapter_for(ProviderKind::Copilot);
        let copilot_env = copilot_adapter.build_environment(&copilot, profiles_root.path());
        assert_eq!(copilot_env.env_vars.len(), 1);
        assert_eq!(copilot_env.env_vars[0].0, COPILOT_HOME_ENV);
        assert_eq!(copilot_adapter.login_args(), vec!["login"]);

        let opencode = AccountConfig::new(
            "opencode-main".parse().expect("id"),
            ProviderKind::Opencode,
            "OpenCode",
        );
        let opencode_adapter = registry.adapter_for(ProviderKind::Opencode);
        let opencode_env = opencode_adapter.build_environment(&opencode, profiles_root.path());
        let env_keys: Vec<&str> = opencode_env
            .env_vars
            .iter()
            .map(|(key, _)| key.as_str())
            .collect();
        assert_eq!(
            env_keys,
            vec![
                ENV_HOME,
                ENV_XDG_CONFIG_HOME,
                ENV_XDG_DATA_HOME,
                ENV_XDG_STATE_HOME,
                ENV_XDG_CACHE_HOME
            ]
        );
        assert_eq!(opencode_adapter.login_args(), vec!["auth", "login"]);
    }

    #[test]
    fn agy_parser_rejects_empty_or_malformed_usage_output() {
        let account = AccountConfig::new("agy-bad".parse().expect("id"), ProviderKind::Agy, "Bad");
        let empty_err = AgyAdapter
            .parse_usage_output(&account, "Quota:\n", Utc::now())
            .expect_err("empty table must fail");
        assert!(matches!(empty_err, ProviderError::ParseUsage { .. }));

        let bad_pct = AgyAdapter
            .parse_usage_output(
                &account,
                "Gemini Models  Weekly Limit Remaining  150%  2026-12-03 06:02 -03\n",
                Utc::now(),
            )
            .expect_err("percentage > 100 must fail");
        assert!(matches!(bad_pct, ProviderError::ParseUsage { .. }));
    }
}

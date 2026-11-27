//! Provider adapters and Linux profile isolation for `tayassu`.

pub mod adapter;
pub mod error;
pub mod isolation;
pub mod migration;

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
pub use migration::{
    DiscoveredAgent, HostDiscovery, MigrationFile, MigrationMode, MigrationSummary,
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

    /// Tab-separated `agy --print=/usage` output captured on 2026-11-27 (agy 1.5.18).
    const AGY_TAB_USAGE_FIXTURE: &str = "\
Gemini Models\tWeekly Limit Remaining\t31%\t2026-12-03T09:02:27Z
Gemini Models\tFive Hour Limit Remaining\t0%\t2026-11-27T02:00:14Z
Claude and GPT models\tWeekly Limit Remaining\t50%\t2026-12-03T19:38:57Z
Claude and GPT models\tFive Hour Limit Remaining\t0%\t2026-11-27T00:38:57Z
";

    /// Expected number of quota rows parsed from [`AGY_USAGE_FIXTURE`].
    const EXPECTED_AGY_WINDOWS: usize = 4;

    /// Expected Gemini weekly remaining percentage in [`AGY_USAGE_FIXTURE`].
    const EXPECTED_GEMINI_WEEKLY_PCT: u8 = 43;

    /// Expected Gemini weekly remaining percentage in [`AGY_TAB_USAGE_FIXTURE`].
    const EXPECTED_AGY_TAB_GEMINI_PCT: u8 = 31;

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
        fs::create_dir_all(real_home.path().join(".local/bin")).expect("mkdir .local/bin");
        fs::create_dir_all(real_home.path().join(".gemini/antigravity-cli/skills"))
            .expect("mkdir skills");

        let account = AccountConfig::new(
            "agy-student".parse().expect("id"),
            ProviderKind::Agy,
            "Student",
        );
        let env = AdapterRegistry
            .adapter_for(ProviderKind::Agy)
            .build_environment(&account, profiles_root.path());

        env.prepare(real_home.path()).expect("prepare profile");

        let fake_home = env.profile_dir.join(FAKE_HOME_SUBDIR);
        assert!(fake_home.join(".gitconfig").symlink_metadata().is_ok());
        assert!(fake_home.join(".local/bin").symlink_metadata().is_ok());
        assert!(
            fake_home
                .join(".gemini/antigravity-cli/skills")
                .symlink_metadata()
                .is_ok()
        );
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
    fn agy_parses_tab_separated_usage_output() {
        let account =
            AccountConfig::new("agy-host".parse().expect("id"), ProviderKind::Agy, "Host");
        let now = Utc
            .with_ymd_and_hms(2026, 11, 27, 0, 10, 0)
            .single()
            .expect("valid utc instant");

        let parsed = AgyAdapter
            .parse_usage_output(&account, AGY_TAB_USAGE_FIXTURE, now)
            .expect("tab fixture should parse");

        assert_eq!(parsed.windows.len(), EXPECTED_AGY_WINDOWS);
        assert_eq!(parsed.windows[0].bucket.as_str(), "gemini");
        assert_eq!(
            parsed.windows[0].remaining.map(RemainingPercent::value),
            Some(EXPECTED_AGY_TAB_GEMINI_PCT)
        );
        assert_eq!(parsed.windows[2].bucket.as_str(), "claude");
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
        assert_eq!(
            entry.resets_at.signed_duration_since(now).num_seconds(),
            tayassu_core::DEFAULT_FALLBACK_COOLDOWN_SECS
        );

        let parsed_duration = ClaudeAdapter
            .detect_rate_limit(
                &account,
                &ModelBucket::default_bucket(),
                "Usage limit reached. Please try again in 2h 30m.",
                now,
            )
            .expect("duration hint should be parsed");
        assert_eq!(
            parsed_duration
                .resets_at
                .signed_duration_since(now)
                .num_minutes(),
            150
        );
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

    #[test]
    fn claude_parses_auth_status_json_and_detects_logged_out() {
        let account = AccountConfig::new(
            "claude-pro".parse().expect("id"),
            ProviderKind::Claude,
            "Claude Pro",
        );
        let now = Utc
            .with_ymd_and_hms(2026, 11, 26, 22, 0, 0)
            .single()
            .expect("valid utc");

        let fixture = r#"
{
  "loggedIn": true,
  "authMethod": "claude.ai",
  "loginExpiresAt": "2026-12-06T03:29:58.541Z",
  "planTier": "pro"
}
"#;
        let parsed = ClaudeAdapter
            .parse_usage_output(&account, fixture, now)
            .expect("claude json should parse");
        assert_eq!(parsed.windows.len(), 1);
        assert_eq!(parsed.windows[0].window_name, "auth:pro");
        assert!(parsed.windows[0].resets_at.is_some());

        let logged_out = ClaudeAdapter
            .parse_usage_output(&account, r#"{"loggedIn": false}"#, now)
            .expect_err("logged out must fail");
        assert!(matches!(
            logged_out,
            ProviderError::AuthenticationRequired {
                provider: ProviderKind::Claude,
                ..
            }
        ));
    }

    #[test]
    fn codex_parses_login_status_logged_in_and_out() {
        let account = AccountConfig::new(
            "codex-main".parse().expect("id"),
            ProviderKind::Codex,
            "Codex",
        );
        let now = Utc::now();

        let parsed = CodexAdapter
            .parse_usage_output(&account, "Logged in using ChatGPT\n", now)
            .expect("codex logged in should parse");
        assert_eq!(parsed.windows.len(), 1);
        assert_eq!(parsed.windows[0].window_name, "auth");

        let logged_out = CodexAdapter
            .parse_usage_output(&account, "Not logged in\n", now)
            .expect_err("not logged in must fail");
        assert!(matches!(
            logged_out,
            ProviderError::AuthenticationRequired {
                provider: ProviderKind::Codex,
                ..
            }
        ));
    }

    #[test]
    fn opencode_parses_stored_providers_and_rejects_empty() {
        let account = AccountConfig::new(
            "opencode-main".parse().expect("id"),
            ProviderKind::Opencode,
            "OpenCode",
        );
        let fixture = "\
GitHub Copilot  GitHub Copilot              stored
Google          Google                      stored
OpenRouter      OpenRouter 2                stored
OpenRouter      OpenRouter                  stored
";
        let parsed = OpencodeAdapter
            .parse_usage_output(&account, fixture, Utc::now())
            .expect("opencode auth list should parse");
        let buckets: Vec<&str> = parsed
            .windows
            .iter()
            .map(|win| win.bucket.as_str())
            .collect();
        assert_eq!(buckets, vec!["github-copilot", "google", "openrouter"]);

        let empty_err = OpencodeAdapter
            .parse_usage_output(&account, "", Utc::now())
            .expect_err("empty auth list must require auth");
        assert!(matches!(
            empty_err,
            ProviderError::AuthenticationRequired {
                provider: ProviderKind::Opencode,
                ..
            }
        ));
    }

    #[test]
    fn copilot_local_probe_checks_profile_credentials() {
        let profiles_root = TempDir::new().expect("temp dir");
        let real_home = TempDir::new().expect("real home");
        let account = AccountConfig::new(
            "copilot-work".parse().expect("id"),
            ProviderKind::Copilot,
            "Copilot",
        );
        let env = CopilotAdapter.build_environment(&account, profiles_root.path());
        env.prepare(real_home.path()).expect("prepare");

        fs::write(env.profile_dir.join("config.json"), "{}").expect("write config");
        let parsed = CopilotAdapter
            .probe_local(&account, &env, Utc::now())
            .expect("local probe should be supported")
            .expect("config.json should authenticate profile");
        assert_eq!(parsed.windows.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn host_discovery_and_move_auth_preserves_binaries_and_settings() {
        use std::os::unix::fs::PermissionsExt;

        let real_home = TempDir::new().expect("real home");
        let profiles_root = TempDir::new().expect("profiles root");

        let bin_dir = real_home.path().join(".local/bin");
        fs::create_dir_all(&bin_dir).expect("mkdir bin");
        let codex_bin = bin_dir.join("codex");
        fs::write(&codex_bin, "#!/bin/sh\n").expect("write codex bin");
        fs::set_permissions(&codex_bin, fs::Permissions::from_mode(0o755)).expect("chmod");

        let codex_dir = real_home.path().join(".codex");
        let packages_dir = codex_dir.join("packages/standalone");
        fs::create_dir_all(&packages_dir).expect("mkdir packages");
        fs::write(codex_dir.join("auth.json"), "{\"token\":\"ok\"}").expect("write auth");
        fs::write(codex_dir.join("config.toml"), "model = \"o3\"\n").expect("write cfg");
        fs::write(packages_dir.join("marker"), "keep").expect("write marker");

        let discovered = HostDiscovery.discover_all(real_home.path(), None);
        assert_eq!(discovered.len(), 1);
        let agent = &discovered[0];
        assert_eq!(agent.provider, ProviderKind::Codex);
        assert!(!agent.requires_relogin);

        let account = AccountConfig::new(
            agent.default_account_id.clone(),
            agent.provider,
            "Codex Imported",
        );
        let env = CodexAdapter.build_environment(&account, profiles_root.path());
        let summary = agent
            .apply(&env, real_home.path(), MigrationMode::MoveAuth)
            .expect("apply migration");

        assert_eq!(summary.copied_files, 2);
        assert_eq!(summary.removed_source_files, 1);
        // Auth moved into profile, but config.toml and updater packages dir remain intact on host!
        assert!(env.profile_dir.join("auth.json").is_file());
        assert!(env.profile_dir.join("config.toml").is_file());
        assert!(!codex_dir.join("auth.json").exists());
        assert!(codex_dir.join("config.toml").is_file());
        assert!(packages_dir.join("marker").is_file());
    }
}

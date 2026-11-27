use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// Executable file mode for mock CLI scripts in integration tests.
#[cfg(unix)]
const SCRIPT_EXECUTABLE_MODE: u32 = 0o755;

/// Isolated test harness executing the compiled `tayassu` binary against a temp XDG root.
struct TestEnv {
    dir: TempDir,
}

impl TestEnv {
    fn new() -> Self {
        let dir = TempDir::new().expect("temp dir");
        fs::create_dir_all(dir.path().join("home")).expect("create fake real home");
        Self { dir }
    }

    fn run_cli(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_tayassu"))
            .args(args)
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.dir.path().join("home"))
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env("XDG_STATE_HOME", self.dir.path().join("state"))
            .env("XDG_DATA_HOME", self.dir.path().join("data"))
            .output()
            .expect("spawn tayassu")
    }

    fn run_cli_in(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_tayassu"))
            .current_dir(cwd)
            .args(args)
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.dir.path().join("home"))
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env("XDG_STATE_HOME", self.dir.path().join("state"))
            .env("XDG_DATA_HOME", self.dir.path().join("data"))
            .output()
            .expect("spawn tayassu")
    }

    fn run_ok(&self, args: &[&str]) -> String {
        let output = self.run_cli(args);
        assert!(
            output.status.success(),
            "tayassu {:?} failed:\nstdout: {}\nstderr: {}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("utf8 stdout")
    }

    #[cfg(unix)]
    fn write_mock_script(&self, name: &str, body: &str) -> String {
        let path = self.dir.path().join(name);
        fs::write(&path, body).expect("write mock script");
        let mut perms = fs::metadata(&path).expect("metadata").permissions();
        perms.set_mode(SCRIPT_EXECUTABLE_MODE);
        fs::set_permissions(&path, perms).expect("chmod");
        path.display().to_string()
    }
}

#[test]
fn account_lifecycle_and_aligned_status_table() {
    let env = TestEnv::new();

    let added = env.run_ok(&[
        "add",
        "agy",
        "agy-student",
        "--no-login",
        "--label",
        "Student Plan",
        "--priority",
        "10",
    ]);
    assert!(added.contains("Added account 'agy-student' (agy)"));

    let env_out = env.run_ok(&["where", "--account", "agy-student"]);
    assert!(env_out.contains("env: HOME="));
    assert!(env_out.contains("env: XDG_CONFIG_HOME="));

    let status_out = env.run_ok(&["ls"]);
    assert!(status_out.contains("agy-student        agy        10"));
    assert!(status_out.contains("READY"));

    let removed = env.run_ok(&["rm", "agy-student"]);
    assert!(removed.contains("Removed account 'agy-student'"));
}

#[test]
fn manual_cooldown_routes_run_to_fallback_account() {
    let env = TestEnv::new();

    env.run_ok(&[
        "add",
        "claude",
        "claude-work",
        "--no-login",
        "--priority",
        "10",
    ]);
    env.run_ok(&[
        "add",
        "claude",
        "claude-personal",
        "--no-login",
        "--priority",
        "20",
    ]);

    env.run_ok(&[
        "cooldown",
        "set",
        "claude-work",
        "--minutes",
        "180",
        "--reason",
        "hit-5h-cap",
    ]);

    let dry_run = env.run_ok(&["run", "claude", "--dry-run"]);
    assert!(dry_run.contains("account: claude-personal"));
    assert!(dry_run.contains("CLAUDE_CONFIG_DIR="));

    env.run_ok(&["cooldown", "clear", "claude-work"]);
    let dry_run_after = env.run_ok(&["run", "claude", "--dry-run"]);
    assert!(dry_run_after.contains("account: claude-work"));
}

#[cfg(unix)]
#[test]
fn live_probe_updates_buckets_and_routes_per_model_cooldown() {
    let env = TestEnv::new();

    let student_script = env.write_mock_script(
        "mock-agy-student.sh",
        "#!/usr/bin/env bash\ncat <<'EOF'\nQuota:\nGemini Models          Weekly Limit Remaining     43%  2099-12-03 06:02 -03\nGemini Models          Five Hour Limit Remaining  47%  2099-11-26 23:00 -03\nClaude and GPT models  Weekly Limit Remaining     50%  2099-12-03 16:38 -03\nClaude and GPT models  Five Hour Limit Remaining  0%   2099-11-26 21:38 -03\nEOF\n",
    );
    let pro_script = env.write_mock_script(
        "mock-agy-pro.sh",
        "#!/usr/bin/env bash\ncat <<'EOF'\nQuota:\nGemini Models          Weekly Limit Remaining     90%  2099-12-03 06:02 -03\nClaude and GPT models  Five Hour Limit Remaining  80%  2099-11-26 21:38 -03\nEOF\n",
    );

    env.run_ok(&[
        "add",
        "agy",
        "agy-student",
        "--no-login",
        "--priority",
        "10",
        "--binary",
        &student_script,
    ]);
    env.run_ok(&[
        "add",
        "agy",
        "agy-pro",
        "--no-login",
        "--priority",
        "20",
        "--binary",
        &pro_script,
    ]);

    let probe_out = env.run_ok(&["probe"]);
    assert!(probe_out.contains("agy-student: ok (4 windows, 1 active cooldowns)"));
    assert!(probe_out.contains("agy-pro: ok (2 windows, 0 active cooldowns)"));

    let status_out = env.run_ok(&["ls"]);
    assert!(status_out.contains("gemini:43%"));
    assert!(status_out.contains("claude:0%("));

    // Explicit priority policy keeps the higher-priority agy-student.
    let gemini_run = env.run_ok(&[
        "run",
        "agy",
        "--model",
        "gemini",
        "--policy",
        "priority",
        "--dry-run",
    ]);
    assert!(gemini_run.contains("account: agy-student"));

    // Default policy is most-quota: agy-pro has more gemini quota left (90% vs 43%).
    let gemini_default = env.run_ok(&["run", "agy", "--model", "gemini", "--dry-run"]);
    assert!(gemini_default.contains("account: agy-pro"));

    // Claude bucket is at 0% on agy-student, so router must fail over to agy-pro.
    let claude_run = env.run_ok(&["run", "agy", "--model", "claude", "--dry-run"]);
    assert!(claude_run.contains("account: agy-pro"));
}

#[cfg(unix)]
#[test]
fn run_injects_isolated_env_into_spawned_copilot_and_codex() {
    let env = TestEnv::new();

    let echo_copilot = env.write_mock_script(
        "mock-copilot.sh",
        "#!/usr/bin/env bash\necho \"COPILOT_HOME=$COPILOT_HOME\"\n",
    );
    let echo_codex = env.write_mock_script(
        "mock-codex.sh",
        "#!/usr/bin/env bash\necho \"CODEX_HOME=$CODEX_HOME\"\n",
    );

    env.run_ok(&[
        "add",
        "copilot",
        "gh-main",
        "--no-login",
        "--binary",
        &echo_copilot,
    ]);
    env.run_ok(&[
        "add",
        "codex",
        "oai-main",
        "--no-login",
        "--binary",
        &echo_codex,
    ]);

    let copilot_out = env.run_ok(&["run", "copilot"]);
    let expected_copilot = Path::new("profiles").join("copilot").join("gh-main");
    assert!(copilot_out.contains(&expected_copilot.display().to_string()));

    let codex_out = env.run_ok(&["run", "codex"]);
    let expected_codex = Path::new("profiles").join("codex").join("oai-main");
    assert!(codex_out.contains(&expected_codex.display().to_string()));
}

#[cfg(unix)]
#[test]
fn probe_supports_claude_codex_copilot_and_opencode_in_parallel() {
    let env = TestEnv::new();

    let mock_claude = env.write_mock_script(
        "mock-claude.sh",
        "#!/usr/bin/env bash\necho '{\"loggedIn\":true,\"planTier\":\"pro\",\"loginExpiresAt\":\"2099-12-06T03:29:58.541Z\"}'\n",
    );
    let mock_codex = env.write_mock_script(
        "mock-codex.sh",
        "#!/usr/bin/env bash\necho 'Logged in using ChatGPT'\n",
    );
    let mock_opencode = env.write_mock_script(
        "mock-opencode.sh",
        "#!/usr/bin/env bash\ncat <<'EOF'\nGitHub Copilot  GitHub Copilot              stored\nGoogle          Google                      stored\nEOF\n",
    );

    env.run_ok(&[
        "add",
        "claude",
        "claude-main",
        "--no-login",
        "--binary",
        &mock_claude,
    ]);
    env.run_ok(&[
        "add",
        "codex",
        "codex-main",
        "--no-login",
        "--binary",
        &mock_codex,
    ]);
    env.run_ok(&[
        "add",
        "opencode",
        "opencode-main",
        "--no-login",
        "--binary",
        &mock_opencode,
    ]);
    env.run_ok(&["add", "copilot", "copilot-main", "--no-login"]);

    let copilot_cfg = env
        .dir
        .path()
        .join("data")
        .join("tayassu")
        .join("profiles")
        .join("copilot")
        .join("copilot-main")
        .join("config.json");
    fs::write(copilot_cfg, "{}").expect("seed copilot config");

    let probe_out = env.run_ok(&["probe"]);
    assert!(probe_out.contains("claude-main: ok (1 windows, 0 active cooldowns)"));
    assert!(probe_out.contains("codex-main: ok (1 windows, 0 active cooldowns)"));
    assert!(probe_out.contains("opencode-main: ok (2 windows, 0 active cooldowns)"));
    assert!(probe_out.contains("copilot-main: ok (1 windows, 0 active cooldowns)"));

    let status_out = env.run_ok(&["ls"]);
    assert!(status_out.contains("claude-main"));
    assert!(status_out.contains("default:ok"));
    assert!(status_out.contains("github-copilot:ok google:ok"));
}

#[cfg(unix)]
#[test]
fn run_captures_stderr_rate_limit_and_failover_switches_account() {
    let env = TestEnv::new();

    let rate_limited_script = env.write_mock_script(
        "mock-rate-limited.sh",
        "#!/usr/bin/env bash\necho 'Error 429: usage limit reached, try again in 45 minutes' >&2\nexit 1\n",
    );
    let healthy_script = env.write_mock_script(
        "mock-healthy.sh",
        "#!/usr/bin/env bash\necho 'served-by-fallback'\n",
    );

    env.run_ok(&[
        "add",
        "claude",
        "claude-primary",
        "--no-login",
        "--priority",
        "10",
        "--binary",
        &rate_limited_script,
    ]);
    env.run_ok(&[
        "add",
        "claude",
        "claude-backup",
        "--no-login",
        "--priority",
        "20",
        "--binary",
        &healthy_script,
    ]);

    // With --no-failover: command exits non-zero, but cooldown is recorded automatically.
    let first_run = env.run_cli(&["run", "claude", "--no-failover"]);
    assert!(!first_run.status.success());
    let first_stderr = String::from_utf8_lossy(&first_run.stderr);
    assert!(first_stderr.contains("rate limit detected on 'claude-primary'"));

    let status_after = env.run_ok(&["ls"]);
    assert!(status_after.contains("claude-primary"));
    assert!(status_after.contains("COOLDOWN (45m)"));

    // Clear cooldown and verify default failover automatically retries on claude-backup.
    env.run_ok(&["cooldown", "clear", "claude-primary"]);
    let failover_out = env.run_ok(&["run", "claude"]);
    assert!(failover_out.contains("served-by-fallback"));

    let status_final = env.run_ok(&["ls"]);
    assert!(status_final.contains("COOLDOWN (45m)"));
}

#[cfg(unix)]
#[test]
fn import_discovers_filters_and_migrates_update_safely() {
    let env = TestEnv::new();
    let home = env.dir.path().join("home");
    let local_bin = home.join(".local/bin");
    fs::create_dir_all(&local_bin).expect("mkdir local bin");

    for bin_name in ["claude", "codex", "agy"] {
        let bin_path = local_bin.join(bin_name);
        fs::write(&bin_path, "#!/usr/bin/env bash\n").expect("write stub bin");
        let mut perms = fs::metadata(&bin_path).expect("meta").permissions();
        perms.set_mode(SCRIPT_EXECUTABLE_MODE);
        fs::set_permissions(&bin_path, perms).expect("chmod");
    }

    fs::create_dir_all(home.join(".claude")).expect("mkdir .claude");
    fs::write(home.join(".claude/.credentials.json"), "{}").expect("write claude cred");
    fs::create_dir_all(home.join(".codex")).expect("mkdir .codex");
    fs::write(home.join(".codex/auth.json"), "{}").expect("write codex auth");

    // Dry-run excluding agy must list claude and codex without modifying config.toml.
    let dry = env.run_ok(&["import", "--dry-run", "--exclude", "agy"]);
    assert!(dry.contains("[dry-run] claude -> account 'claude-host'"));
    assert!(dry.contains("[dry-run] codex -> account 'codex-host'"));
    assert!(!dry.contains("agy-host"));
    assert!(!env.dir.path().join("config/tayassu/config.toml").exists());

    // Real import with --provider claude --move-auth moves only .credentials.json.
    let imported = env.run_ok(&["import", "--provider", "claude", "--move-auth"]);
    assert!(imported.contains("Imported 'claude-host' (claude)"));
    assert!(!home.join(".claude/.credentials.json").exists());
    assert!(
        env.dir
            .path()
            .join("data/tayassu/profiles/claude/claude-host/.credentials.json")
            .is_file()
    );
    // Host binary and codex auth (excluded) remain untouched.
    assert!(local_bin.join("claude").is_file());
    assert!(home.join(".codex/auth.json").is_file());
}

#[test]
fn activate_emits_valid_wrappers_for_bash_and_fish() {
    let env = TestEnv::new();

    let bash_out = env.run_ok(&["activate", "bash"]);
    assert!(bash_out.contains("agy() { tayassu run agy -- \"$@\"; }"));
    assert!(bash_out.contains("claude() { tayassu run claude -- \"$@\"; }"));
    assert!(bash_out.contains("source <(COMPLETE=bash tayassu)"));

    let no_failover = env.run_ok(&["activate", "bash", "--no-failover"]);
    assert!(no_failover.contains("codex() { tayassu run codex --no-failover -- \"$@\"; }"));

    let fish_out = env.run_ok(&["activate", "fish", "--provider", "copilot"]);
    assert!(fish_out.contains("function copilot --wraps copilot"));
    assert!(!fish_out.contains("function agy"));
}

#[cfg(unix)]
#[test]
fn project_policy_resume_memory_and_fallback_chain() {
    let env = TestEnv::new();
    let repo_dir = env.dir.path().join("my-work-repo");
    fs::create_dir_all(&repo_dir).expect("mkdir repo");
    let mock_ok = env.write_mock_script("mock-ok.sh", "#!/usr/bin/env bash\necho ok\n");

    for (id, provider, pri) in [
        ("agy-student", "agy", "10"),
        ("claude-work", "claude", "20"),
        ("claude-personal", "claude", "10"),
    ] {
        env.run_ok(&[
            "add",
            provider,
            id,
            "--no-login",
            "--priority",
            pri,
            "--binary",
            &mock_ok,
        ]);
    }

    fs::write(
        repo_dir.join(".tayassu.toml"),
        "[preferred_accounts]\nclaude = \"claude-work\"\n",
    )
    .expect("write .tayassu.toml");

    let project_run = env.run_cli_in(&repo_dir, &["run", "claude", "--dry-run"]);
    assert!(project_run.status.success());
    assert!(String::from_utf8_lossy(&project_run.stdout).contains("account: claude-work"));

    let forced_run = env.run_cli_in(
        &repo_dir,
        &["run", "claude", "--account", "claude-personal"],
    );
    assert!(forced_run.status.success());

    let resume_dry = env.run_cli_in(
        &repo_dir,
        &["run", "claude", "--dry-run", "--", "--continue"],
    );
    assert!(String::from_utf8_lossy(&resume_dry.stdout).contains("account: claude-personal"));

    let cfg_path = env.dir.path().join("config/tayassu/config.toml");
    let cfg_text = fs::read_to_string(&cfg_path).expect("read config");
    let updated_cfg = cfg_text.replace(
        "[fallback_chains]",
        "[fallback_chains]\nclaude-any = [\"agy:claude\", \"claude:default\"]",
    );
    fs::write(&cfg_path, updated_cfg).expect("write config with chain");

    env.run_ok(&[
        "cooldown",
        "set",
        "agy-student",
        "--model",
        "claude",
        "--minutes",
        "60",
    ]);
    let chain_dry = env.run_ok(&["run", "--chain", "claude-any", "--dry-run"]);
    assert!(chain_dry.contains("account: claude-personal"));
}

#[cfg(unix)]
#[test]
fn systemd_unit_install_and_doctor_diagnostics() {
    let env = TestEnv::new();
    let mock_ok = env.write_mock_script("mock-ok.sh", "#!/usr/bin/env bash\necho ok\n");
    env.run_ok(&[
        "add",
        "claude",
        "claude-work",
        "--no-login",
        "--binary",
        &mock_ok,
    ]);

    let unit_out = env.run_ok(&["setup", "--interval-minutes", "10"]);
    assert!(unit_out.contains("tayassu-probe.timer"));
    assert!(
        env.run_ok(&["setup", "--no-systemd"])
            .contains("already configured")
    );
    assert!(
        env.dir
            .path()
            .join("config/systemd/user/tayassu-probe.service")
            .is_file()
    );

    let cred_file = env
        .dir
        .path()
        .join("data/tayassu/profiles/claude/claude-work/.credentials.json");
    fs::write(&cred_file, "{}").expect("write cred");
    fs::set_permissions(&cred_file, fs::Permissions::from_mode(0o644)).expect("chmod 644");
    let doctor_out = env.run_ok(&["doctor"]);
    assert!(doctor_out.contains("[OK  ]"));
    assert!(doctor_out.contains("chmod 600"));
}

#[test]
fn bare_command_lists_accounts_and_json_is_machine_readable() {
    let env = TestEnv::new();
    env.run_ok(&["add", "claude", "--no-login"]);
    env.run_ok(&["add", "claude", "--no-login"]);

    let bare = env.run_ok(&[]);
    assert!(bare.contains("claude-2"));

    let json = env.run_ok(&["ls", "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
    assert_eq!(parsed["accounts"].as_array().map(Vec::len), Some(2));
}

#[test]
fn use_and_config_persist_routing_settings() {
    let env = TestEnv::new();
    let repo = env.dir.path().join("repo");
    fs::create_dir_all(&repo).expect("mkdir repo");
    env.run_ok(&["add", "claude", "work", "--no-login"]);

    let out = env.run_cli_in(&repo, &["use", "work", "--policy", "priority"]);
    assert!(out.status.success());
    let project = fs::read_to_string(repo.join(".tayassu.toml")).expect("project file");
    assert!(project.contains("claude = \"work\""));
    assert!(project.contains("default_policy = \"priority\""));

    env.run_ok(&["config", "set", "default-policy", "soonest-reset"]);
    assert_eq!(
        env.run_ok(&["config", "get", "default-policy"]).trim(),
        "soonest-reset"
    );
}

#[cfg(unix)]
#[test]
fn cached_window_past_its_reset_is_shown_as_reset_and_routable() {
    let env = TestEnv::new();
    let script = env.write_mock_script(
        "mock-agy-old.sh",
        "#!/usr/bin/env bash\ncat <<'EOF'\nQuota:\nGemini Models          Five Hour Limit Remaining  0%  2000-01-01 00:00 -03\nEOF\n",
    );
    env.run_ok(&["add", "agy", "old", "--binary", &script, "--no-login"]);
    env.run_ok(&["probe"]);

    let table = env.run_ok(&["ls"]);
    assert!(table.contains("gemini:reset✓"), "{table}");
    assert!(table.contains("READY"));
    assert!(!table.contains("EXHAUSTED"));

    let json = env.run_ok(&["ls", "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("json");
    assert_eq!(parsed["quota"][0]["state"], "reset");

    let routed = env.run_ok(&["where", "agy", "--model", "gemini"]);
    assert!(routed.contains("account: old"));
}

#[cfg(unix)]
#[test]
fn failed_probe_keeps_cached_windows() {
    let env = TestEnv::new();
    let ok = env.write_mock_script(
        "mock-agy-ok.sh",
        "#!/usr/bin/env bash\ncat <<'EOF'\nQuota:\nGemini Models          Weekly Limit Remaining  43%  2099-12-03 06:02 -03\nEOF\n",
    );
    env.run_ok(&["add", "agy", "cached", "--binary", &ok, "--no-login"]);
    env.run_ok(&["probe"]);

    let broken = env.write_mock_script("mock-agy-broken.sh", "#!/usr/bin/env bash\nexit 1\n");
    let config = env.dir.path().join("config/tayassu/config.toml");
    let raw = fs::read_to_string(&config).expect("config");
    fs::write(&config, raw.replace(&ok, &broken)).expect("rewrite config");

    let out = env.run_ok(&["probe"]);
    assert!(out.contains("all probes failed"), "{out}");
    assert!(env.run_ok(&["ls"]).contains("gemini:43%"));
}

#[cfg(unix)]
#[test]
fn uninstall_removes_hooks_units_and_binary_while_keeping_data_unless_purged() {
    let env = TestEnv::new();
    let home = env.dir.path().join("home");
    fs::write(home.join(".bashrc"), "export KEEP=1\n").expect("write bashrc");
    env.run_ok(&["add", "claude", "work", "--no-login"]);
    env.run_ok(&["setup", "--interval-minutes", "15"]);

    let fake_bin = env.dir.path().join("tayassu-copy");
    fs::write(&fake_bin, "bin").expect("write fake bin");

    let dry = Command::new(env!("CARGO_BIN_EXE_tayassu"))
        .args(["uninstall", "--dry-run"])
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", env.dir.path().join("config"))
        .env("XDG_STATE_HOME", env.dir.path().join("state"))
        .env("XDG_DATA_HOME", env.dir.path().join("data"))
        .env("TAYASSU_UNINSTALL_BIN", &fake_bin)
        .output()
        .expect("dry uninstall");
    assert!(dry.status.success());
    let dry_out = String::from_utf8_lossy(&dry.stdout);
    assert!(dry_out.contains("[dry-run] shell:"));
    assert!(dry_out.contains("[dry-run] binary:"));
    assert!(fake_bin.exists());

    let run = Command::new(env!("CARGO_BIN_EXE_tayassu"))
        .args(["uninstall", "--yes"])
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", env.dir.path().join("config"))
        .env("XDG_STATE_HOME", env.dir.path().join("state"))
        .env("XDG_DATA_HOME", env.dir.path().join("data"))
        .env("TAYASSU_UNINSTALL_BIN", &fake_bin)
        .output()
        .expect("uninstall");
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(!fake_bin.exists());
    assert_eq!(
        fs::read_to_string(home.join(".bashrc")).expect("read bashrc"),
        "export KEEP=1\n"
    );
    assert!(
        !env.dir
            .path()
            .join("config/systemd/user/tayassu-probe.timer")
            .exists()
    );
    // User data stays untouched without --purge.
    assert!(env.dir.path().join("config/tayassu/config.toml").exists());

    let purge = Command::new(env!("CARGO_BIN_EXE_tayassu"))
        .args(["uninstall", "--yes", "--purge"])
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", env.dir.path().join("config"))
        .env("XDG_STATE_HOME", env.dir.path().join("state"))
        .env("XDG_DATA_HOME", env.dir.path().join("data"))
        .env("TAYASSU_UNINSTALL_BIN", &fake_bin)
        .output()
        .expect("purge");
    assert!(purge.status.success());
    assert!(!env.dir.path().join("config/tayassu").exists());
    assert!(!env.dir.path().join("data/tayassu").exists());
}

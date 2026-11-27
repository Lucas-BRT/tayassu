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

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tayassu"));
        command
            .current_dir(self.dir.path())
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.dir.path().join("home"))
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env("XDG_STATE_HOME", self.dir.path().join("state"))
            .env("XDG_DATA_HOME", self.dir.path().join("data"))
            .env_remove("TAYASSU_COMPLETE");
        command
    }

    fn run_cli(&self, args: &[&str]) -> Output {
        self.command().args(args).output().expect("spawn tayassu")
    }

    fn run_cli_in(&self, cwd: &Path, args: &[&str]) -> Output {
        self.command()
            .current_dir(cwd)
            .args(args)
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
        "account",
        "add",
        "agy-student",
        "agy",
        "--label",
        "Student Plan",
        "--priority",
        "10",
    ]);
    assert!(added.contains("Added account 'agy-student' (agy)"));

    let env_out = env.run_ok(&["account", "env", "agy-student"]);
    assert!(env_out.contains("export HOME="));
    assert!(env_out.contains("export XDG_CONFIG_HOME="));

    let status_out = env.run_ok(&["status"]);
    assert!(status_out.contains("agy-student        agy        10"));
    assert!(status_out.contains("READY"));

    let removed = env.run_ok(&["account", "remove", "agy-student"]);
    assert!(removed.contains("Removed account 'agy-student'"));
}

#[test]
fn manual_cooldown_routes_run_to_fallback_account() {
    let env = TestEnv::new();

    env.run_ok(&[
        "account",
        "add",
        "claude-work",
        "claude",
        "--priority",
        "10",
    ]);
    env.run_ok(&[
        "account",
        "add",
        "claude-personal",
        "claude",
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
        "account",
        "add",
        "agy-student",
        "agy",
        "--priority",
        "10",
        "--binary",
        &student_script,
    ]);
    env.run_ok(&[
        "account",
        "add",
        "agy-pro",
        "agy",
        "--priority",
        "20",
        "--binary",
        &pro_script,
    ]);

    let probe_out = env.run_ok(&["probe"]);
    assert!(probe_out.contains("agy-student: ok (4 windows, 1 active cooldowns)"));
    assert!(probe_out.contains("agy-pro: ok (2 windows, 0 active cooldowns)"));

    let status_out = env.run_ok(&["status"]);
    assert!(status_out.contains("gemini:43%"));
    assert!(status_out.contains("claude:0%("));

    // Gemini is still available on higher-priority agy-student.
    let gemini_run = env.run_ok(&["run", "agy", "--model", "gemini", "--dry-run"]);
    assert!(gemini_run.contains("account: agy-student"));

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
        "account",
        "add",
        "gh-main",
        "copilot",
        "--binary",
        &echo_copilot,
    ]);
    env.run_ok(&[
        "account",
        "add",
        "oai-main",
        "codex",
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
        "account",
        "add",
        "claude-main",
        "claude",
        "--binary",
        &mock_claude,
    ]);
    env.run_ok(&[
        "account",
        "add",
        "codex-main",
        "codex",
        "--binary",
        &mock_codex,
    ]);
    env.run_ok(&[
        "account",
        "add",
        "opencode-main",
        "opencode",
        "--binary",
        &mock_opencode,
    ]);
    env.run_ok(&["account", "add", "copilot-main", "copilot"]);

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

    let status_out = env.run_ok(&["status"]);
    assert!(status_out.contains("claude-main"));
    assert!(status_out.contains("LOGIN"));
    assert!(status_out.contains("ok            unknown"));
    assert!(status_out.contains("LAST PROBE (UTC)"));
}

#[cfg(unix)]
#[test]
fn run_captures_stderr_rate_limit_and_failover_switches_account() {
    let env = TestEnv::new();

    let rate_limited_script = env.write_mock_script(
        "mock-rate-limited.sh",
        "#!/usr/bin/env bash\necho 'Error 429: usage limit reached, try again in 45 minutes' >&2\nexit 7\n",
    );
    let healthy_script = env.write_mock_script(
        "mock-healthy.sh",
        "#!/usr/bin/env bash\necho 'served-by-fallback'\n",
    );

    env.run_ok(&[
        "account",
        "add",
        "claude-primary",
        "claude",
        "--priority",
        "10",
        "--binary",
        &rate_limited_script,
    ]);
    env.run_ok(&[
        "account",
        "add",
        "claude-backup",
        "claude",
        "--priority",
        "20",
        "--binary",
        &healthy_script,
    ]);

    // Without --failover: command exits non-zero, but cooldown is recorded automatically.
    let first_run = env.run_cli(&["run", "claude"]);
    assert!(!first_run.status.success());
    let first_stderr = String::from_utf8_lossy(&first_run.stderr);
    assert!(first_stderr.contains("rate limit detected on 'claude-primary'"));

    let status_after = env.run_ok(&["status"]);
    assert!(status_after.contains("claude-primary"));
    assert!(status_after.contains("COOLDOWN (45m)"));

    env.run_ok(&["account", "disable", "claude-backup"]);
    env.run_ok(&["cooldown", "clear", "claude-primary"]);
    let no_fallback = env.run_cli(&["run", "claude", "--failover"]);
    assert_eq!(no_fallback.status.code(), Some(7));
    assert!(String::from_utf8_lossy(&no_fallback.stderr).contains("no fallback account"));
    env.run_ok(&["account", "enable", "claude-backup"]);

    // Clear cooldown and verify --failover automatically retries on claude-backup.
    env.run_ok(&["cooldown", "clear", "claude-primary"]);
    let failover_out = env.run_ok(&["run", "claude", "--failover"]);
    assert!(failover_out.contains("served-by-fallback"));

    let status_final = env.run_ok(&["status"]);
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
    let profile_auth = env
        .dir
        .path()
        .join("data/tayassu/profiles/claude/claude-host/.credentials.json");
    env.run_ok(&[
        "account",
        "edit",
        "claude-host",
        "--priority",
        "20",
        "--label",
        "Work",
    ]);
    env.run_ok(&["account", "disable", "claude-host"]);
    fs::write(home.join(".claude/.credentials.json"), "updated").expect("new host credentials");
    assert!(
        env.run_ok(&["import", "--provider", "claude"])
            .contains("already configured")
    );
    assert_eq!(
        fs::read_to_string(&profile_auth).expect("original auth"),
        "{}"
    );
    env.run_ok(&["import", "--provider", "claude", "--refresh"]);
    assert_eq!(
        fs::read_to_string(&profile_auth).expect("refreshed auth"),
        "updated"
    );
    let status = env.run_ok(&["status"]);
    assert!(status.contains("claude     20"));
    assert!(status.contains("DISABLED"));
    assert!(status.contains("Work"));
    env.run_ok(&["account", "enable", "claude-host"]);
    assert!(env.run_ok(&["status"]).contains("READY"));
    fs::remove_file(home.join(".claude/.credentials.json")).expect("remove test credentials");
    // Host binary and codex auth (excluded) remain untouched.
    assert!(local_bin.join("claude").is_file());
    assert!(home.join(".codex/auth.json").is_file());
}

#[test]
fn shell_init_emits_valid_wrappers_for_bash_and_fish() {
    let env = TestEnv::new();

    let bash_out = env.run_ok(&["shell-init", "bash", "--failover"]);
    assert!(bash_out.contains("agy() { tayassu run agy --failover -- \"$@\"; }"));
    assert!(bash_out.contains("claude() { tayassu run claude --failover -- \"$@\"; }"));
    assert!(bash_out.contains("codex() { tayassu run codex --failover -- \"$@\"; }"));

    let fish_out = env.run_ok(&["shell-init", "fish", "--provider", "copilot"]);
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
            "account",
            "add",
            id,
            provider,
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
        "account",
        "add",
        "claude-work",
        "claude",
        "--binary",
        &mock_ok,
    ]);

    let unit_out = env.run_ok(&["systemd-unit", "--interval-minutes", "10", "--install"]);
    assert!(unit_out.contains("tayassu-probe.timer"));
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
fn dry_run_explains_routing_without_writes_and_reports_next_release() {
    let env = TestEnv::new();
    env.run_ok(&["account", "add", "work", "codex", "--priority", "10"]);
    env.run_ok(&["account", "add", "personal", "codex"]);
    env.run_ok(&["account", "disable", "personal"]);

    let profile_root = env.dir.path().join("data/tayassu/profiles");
    fs::remove_dir_all(&profile_root).expect("remove test profiles");
    let preview = env.run_ok(&["run", "codex", "--dry-run"]);
    assert!(preview.contains("account: work"));
    assert!(preview.contains("policy: priority"));
    assert!(preview.contains("skipped personal (codex:default): disabled"));
    assert!(!profile_root.exists());
    assert!(!env.dir.path().join("state").exists());

    env.run_ok(&["cooldown", "set", "work"]);
    let blocked = env.run_cli(&["run", "codex", "--dry-run"]);
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("next known availability:"));

    let incompatible = env.run_cli(&["run", "--account", "work", "--failover"]);
    assert!(!incompatible.status.success());
    assert!(String::from_utf8_lossy(&incompatible.stderr).contains("cannot be used with"));
}

#[cfg(unix)]
#[test]
fn failed_probe_does_not_report_login_ok_or_lose_the_account_id() {
    let env = TestEnv::new();
    let mock = env.write_mock_script(
        "codex.sh",
        "#!/bin/sh
printf 'Logged in using ChatGPT\n'
exit 7
",
    );
    env.run_ok(&["account", "add", "work", "codex", "--binary", &mock]);
    let probe = env.run_ok(&["probe", "--id", "work"]);
    assert!(probe.contains("exit status: 7"));
    let status = env.run_ok(&["status"]);
    assert!(status.contains("ERROR"));
    assert!(status.contains("unverified"));
    assert!(status.contains("tayassu doctor, then tayassu probe --id work"));

    env.write_mock_script(
        "codex.sh",
        "#!/bin/sh
printf 'Not logged in\n' >&2
exit 1
",
    );
    env.run_ok(&["probe", "--id", "work"]);
    let status = env.run_ok(&["status"]);
    assert!(status.contains("AUTH"));
    assert!(status.contains("tayassu account login work"));
    let automatic = env.run_cli(&["run", "codex", "--dry-run"]);
    assert!(!automatic.status.success());
    assert!(String::from_utf8_lossy(&automatic.stderr).contains("login required"));
    assert!(
        env.run_ok(&["run", "--account", "work", "--dry-run"])
            .contains("explicit account")
    );
}

#[test]
fn shell_completion_reads_current_accounts_providers_and_chains() {
    let env = TestEnv::new();
    env.run_ok(&["account", "add", "work-codex", "codex"]);
    let config = env.dir.path().join("config/tayassu/config.toml");
    let contents = fs::read_to_string(&config).expect("read config");
    fs::write(
        &config,
        contents.replace(
            "[fallback_chains]",
            "[fallback_chains]\nwork-chain = [\"codex\"]",
        ),
    )
    .expect("add chain");

    for (index, args, expected) in [
        ("2", vec!["run", "co"], "codex"),
        ("3", vec!["run", "--account", "wo"], "work-codex"),
        ("3", vec!["run", "--chain", "wo"], "work-chain"),
        ("2", vec!["run", "--dry"], "--dry-run"),
        ("3", vec!["account", "disable", "wo"], "work-codex"),
    ] {
        let output = env
            .command()
            .env("TAYASSU_COMPLETE", "bash")
            .env("_CLAP_COMPLETE_INDEX", index)
            .args(["--", "tayassu"])
            .args(&args)
            .output()
            .expect("get candidates");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(expected),
            "{args:?}"
        );
    }
}

#[test]
fn concurrent_account_registrations_do_not_lose_updates() {
    let env = TestEnv::new();
    let children: Vec<_> = (0..8)
        .map(|index| {
            env.command()
                .args(["account", "add", &format!("worker-{index}"), "codex"])
                .stdout(std::process::Stdio::null())
                .spawn()
                .expect("start registration")
        })
        .collect();
    for mut child in children {
        assert!(child.wait().expect("wait for registration").success());
    }
    let status = env.run_ok(&["status"]);
    for index in 0..8 {
        assert!(status.contains(&format!("worker-{index}")));
    }
}

#[cfg(unix)]
#[test]
fn agy_disabled_window_clears_previous_probe_cooldown_and_keeps_other_quota() {
    let env = TestEnv::new();
    let mock = env.write_mock_script(
        "agy.sh",
        "#!/bin/sh\nprintf 'Claude and GPT models\tFHour Limit Remaining\t0%%\t2099-01-01T00:00:00Z\n'\n",
    );
    env.run_ok(&["account", "add", "agy-host", "agy", "--binary", &mock]);
    env.run_ok(&["probe", "--id", "agy-host"]);
    assert!(
        !env.run_cli(&["run", "agy", "--model", "claude", "--dry-run"])
            .status
            .success()
    );

    env.write_mock_script(
        "agy.sh",
        "#!/bin/sh\nprintf 'Claude and GPT models\tFHour Limit Remaining\tdisabled\nClaude and GPT models\tWeekly Limit Remaining\t35%%\n'\n",
    );
    assert!(
        env.run_ok(&["probe", "--id", "agy-host"])
            .contains("ok (1 windows")
    );
    assert!(env.run_ok(&["status"]).contains("claude:35%"));
    assert!(
        env.run_ok(&["run", "agy", "--model", "claude", "--dry-run"])
            .contains("account: agy-host")
    );
}

#[cfg(unix)]
#[test]
fn opencode_outside_path_works_for_doctor_probe_and_run() {
    let env = TestEnv::new();
    let user_bin = env.dir.path().join("home/.opencode/bin");
    fs::create_dir_all(&user_bin).expect("create provider bin directory");
    let mock = env.write_mock_script(
        "mock-opencode",
        "#!/bin/sh\nif [ \"$1\" = auth ]; then printf 'Google  Google  stored\n'; else printf 'ran opencode\n'; fi\n",
    );
    fs::rename(mock, user_bin.join("opencode")).expect("install mock outside PATH");

    env.run_ok(&["account", "add", "opencode-host", "opencode"]);
    assert!(env.run_ok(&["doctor"]).contains("binary 'opencode' found"));
    assert!(
        env.run_ok(&["probe", "--id", "opencode-host"])
            .contains("opencode-host: ok (1 windows")
    );
    assert!(
        env.run_ok(&["run", "opencode", "--dry-run"])
            .contains(".opencode/bin/opencode")
    );
    assert!(env.run_ok(&["run", "opencode"]).contains("ran opencode"));
}

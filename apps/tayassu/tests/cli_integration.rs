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
            .env_remove("SHELL")
            .env_remove("ZDOTDIR")
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
fn filtered_empty_status_names_the_requested_provider() {
    let env = TestEnv::new();
    env.run_ok(&["add", "agy", "work", "--no-login"]);
    let output = env.run_ok(&["ls", "codex"]);
    assert!(output.contains("No accounts configured for codex"));
    assert!(output.contains("tayassu add codex"));
}

#[test]
fn setup_without_shell_configuration_does_not_claim_it_is_configured() {
    let env = TestEnv::new();
    let output = env.run_ok(&["setup", "shell"]);
    assert!(output.contains("no supported shell configuration found"));
    assert!(!output.contains("already configured"));
}

#[test]
fn setup_requires_explicit_shell_opt_in() {
    let env = TestEnv::new();
    let rc = env.dir.path().join("home/.bashrc");
    fs::write(&rc, "# original\n").expect("write shell config");
    env.run_ok(&["setup", "--no-systemd"]);
    assert_eq!(
        fs::read_to_string(&rc).expect("read shell config"),
        "# original\n"
    );
    env.run_ok(&["setup", "shell"]);
    assert!(
        fs::read_to_string(rc)
            .expect("read shell config")
            .contains("tayassu activate bash")
    );
}

#[test]
fn completion_setup_is_idempotent_and_does_not_enable_wrappers() {
    let env = TestEnv::new();
    let rc = env.dir.path().join("home/.bashrc");
    fs::write(&rc, "# original\n").expect("write rc");
    env.run_ok(&["setup", "completion"]);
    env.run_ok(&["setup", "completion"]);
    let contents = fs::read_to_string(&rc).expect("read rc");
    assert_eq!(contents.matches("# tayassu completion").count(), 1);
    assert!(contents.contains("TAYASSU_COMPLETE=bash"));
    assert!(!contents.contains("tayassu activate"));
    let bin = env.dir.path().join("binary");
    fs::write(&bin, "binary").expect("write binary");
    let output = env
        .command()
        .args(["uninstall", "--yes"])
        .env("TAYASSU_UNINSTALL_BIN", bin)
        .output()
        .expect("uninstall");
    assert!(output.status.success());
    assert_eq!(fs::read_to_string(rc).expect("read rc"), "# original\n");
}

#[test]
fn completion_uninstall_preserves_unrelated_fish_configuration() {
    let env = TestEnv::new();
    let directory = env.dir.path().join("home/.config/fish/conf.d");
    fs::create_dir_all(&directory).expect("create fish config");
    let path = directory.join("tayassu-completion.fish");
    fs::write(&path, "set -g user_setting yes\n").expect("write user setting");
    env.run_ok(&["setup", "completion"]);
    let bin = env.dir.path().join("binary");
    fs::write(&bin, "binary").expect("write binary");
    let output = env
        .command()
        .args(["uninstall", "--yes"])
        .env("TAYASSU_UNINSTALL_BIN", bin)
        .output()
        .expect("uninstall");
    assert!(output.status.success());
    assert_eq!(
        fs::read_to_string(path).expect("read user setting"),
        "set -g user_setting yes\n"
    );
}

#[cfg(unix)]
fn test_shell(shell: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let executable = std::env::split_paths(&path)
        .map(|directory| directory.join(shell))
        .find(|candidate| candidate.is_file());
    assert!(
        executable.is_some() || std::env::var_os("TAYASSU_REQUIRE_TEST_SHELLS").is_none(),
        "{shell} is required for shell integration tests"
    );
    executable
}

#[cfg(unix)]
#[test]
fn installed_completion_loads_without_provider_wrappers() {
    for (shell, flags, script) in [
        (
            "bash",
            "-c",
            "source ~/.bashrc && complete -p tayassu && ! declare -F agy",
        ),
        (
            "zsh",
            "-ic",
            "[[ ${_comps[tayassu]} == _clap_dynamic_completer_tayassu ]] && (( ! $+functions[agy] ))",
        ),
        (
            "fish",
            "-c",
            "contains -- setup (complete -C 'tayassu set' | cut -f1); and not functions -q agy",
        ),
    ] {
        let Some(executable) = test_shell(shell) else {
            continue;
        };
        let env = TestEnv::new();
        let bin = env.dir.path().join("bin");
        fs::create_dir_all(&bin).expect("create bin");
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_tayassu"), bin.join("tayassu"))
            .expect("link tayassu");
        let mut setup = env.command();
        let result = setup
            .env("SHELL", shell)
            .args(["setup", "completion"])
            .output()
            .expect("setup completion");
        assert!(result.status.success(), "{shell}: {result:?}");
        let mut run = Command::new(executable);
        run.current_dir(env.dir.path())
            .envs(
                setup
                    .get_envs()
                    .filter_map(|(key, value)| value.map(|value| (key, value))),
            )
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .args([flags, script]);
        let output = run.output().expect("run configured shell");
        assert!(output.status.success(), "{shell}: {output:?}");
    }
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

    // Explicit priority policy keeps higher-priority agy-student.
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
    assert!(
        probe_out.contains("unable to verify Copilot authentication for account 'copilot-main'")
    );

    let status_out = env.run_ok(&["ls"]);
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

    // Without --failover: command exits non-zero, but cooldown is recorded automatically.
    let first_run = env.run_cli(&["run", "claude", "--no-failover"]);
    assert!(!first_run.status.success());
    let first_stderr = String::from_utf8_lossy(&first_run.stderr);
    assert!(first_stderr.contains("rate limit detected on 'claude-primary'"));

    let status_after = env.run_ok(&["ls"]);
    assert!(status_after.contains("claude-primary"));
    assert!(status_after.contains("COOLDOWN (45m)"));

    env.run_ok(&["disable", "claude-backup"]);
    env.run_ok(&["cooldown", "clear", "claude-primary"]);
    let no_fallback = env.run_cli(&["run", "claude"]);
    assert_eq!(no_fallback.status.code(), Some(7));
    assert!(String::from_utf8_lossy(&no_fallback.stderr).contains("no fallback account"));
    env.run_ok(&["enable", "claude-backup"]);

    // Clear cooldown and verify --failover automatically retries on claude-backup.
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
    assert!(dry.contains(".claude/.credentials.json"));
    assert!(dry.contains("profiles/claude/claude-host/.credentials.json"));
    assert!(dry.contains("copy; keep source"));
    assert!(!dry.contains("agy-host"));
    assert!(!env.dir.path().join("config/tayassu/config.toml").exists());
    let move_preview = env.run_ok(&["import", "--dry-run", "--provider", "claude", "--move-auth"]);
    assert!(move_preview.contains("copy, then remove source"));
    assert!(home.join(".claude/.credentials.json").is_file());

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
    env.run_ok(&["edit", "claude-host", "--priority", "20", "--label", "Work"]);
    env.run_ok(&["disable", "claude-host"]);
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
    let status = env.run_ok(&["ls"]);
    assert!(status.contains("claude     20"));
    assert!(status.contains("DISABLED"));
    assert!(status.contains("Work"));
    env.run_ok(&["enable", "claude-host"]);
    assert!(env.run_ok(&["ls"]).contains("READY"));
    fs::remove_file(home.join(".claude/.credentials.json")).expect("remove test credentials");
    // Host binary and codex auth (excluded) remain untouched.
    assert!(local_bin.join("claude").is_file());
    assert!(home.join(".codex/auth.json").is_file());
}

#[test]
fn activate_emits_valid_wrappers_for_bash_and_fish() {
    let env = TestEnv::new();

    let bash_out = env.run_ok(&["activate", "bash"]);
    assert!(bash_out.contains("command tayassu __has-account agy"));
    assert!(bash_out.contains("command tayassu __has-account claude"));
    assert!(bash_out.contains("TAYASSU_COMPLETE=bash"));

    let no_failover = env.run_ok(&["activate", "bash", "--no-failover"]);
    assert!(no_failover.contains("command tayassu run codex --no-failover -- \"$@\""));

    let fish_out = env.run_ok(&["activate", "fish", "--provider", "copilot"]);
    assert!(fish_out.contains("function copilot --wraps copilot"));
    assert!(!fish_out.contains("function agy"));
}

#[cfg(unix)]
#[test]
fn bash_wrapper_falls_back_without_an_account_and_after_uninstall() {
    let env = TestEnv::new();
    let bin = env.dir.path().join("bin");
    fs::create_dir_all(&bin).expect("create bin");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_tayassu"), bin.join("tayassu"))
        .expect("link tayassu");
    let host = bin.join("agy");
    fs::write(&host, "#!/bin/sh\nprintf 'host:%s\\n' \"$*\"\n").expect("write host");
    fs::set_permissions(&host, fs::Permissions::from_mode(SCRIPT_EXECUTABLE_MODE))
        .expect("chmod host");
    let script = "eval \"$(tayassu activate bash --provider agy)\"; agy --help; rm \"$BIN_DIR/tayassu\"; agy after-uninstall";
    let output = env
        .command()
        .get_envs()
        .fold(Command::new("bash"), |mut cmd, (key, value)| {
            if let Some(value) = value {
                cmd.env(key, value);
            } else {
                cmd.env_remove(key);
            }
            cmd
        })
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("BIN_DIR", &bin)
        .args(["-c", script])
        .output()
        .expect("run shell");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "host:--help\nhost:after-uninstall\n"
    );
}

#[cfg(unix)]
#[test]
fn zsh_and_fish_wrappers_fall_back_when_tayassu_is_removed() {
    for (shell, script) in [
        (
            "zsh",
            "eval \"$(tayassu activate zsh --provider agy)\"; agy 'hello world'; rm \"$BIN_DIR/tayassu\"; agy after",
        ),
        (
            "fish",
            "tayassu activate fish --provider agy | source; agy 'hello world'; rm $BIN_DIR/tayassu; agy after",
        ),
    ] {
        let Some(executable) = test_shell(shell) else {
            continue;
        };
        let env = TestEnv::new();
        let bin = env.dir.path().join("bin");
        fs::create_dir_all(&bin).expect("create bin");
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_tayassu"), bin.join("tayassu"))
            .expect("link tayassu");
        let host = bin.join("agy");
        fs::write(&host, "#!/bin/sh\nprintf 'host:%s\\n' \"$*\"\n").expect("write host");
        fs::set_permissions(&host, fs::Permissions::from_mode(SCRIPT_EXECUTABLE_MODE))
            .expect("chmod host");
        let output = env
            .command()
            .get_envs()
            .fold(Command::new(executable), |mut cmd, (key, value)| {
                if let Some(value) = value {
                    cmd.env(key, value);
                } else {
                    cmd.env_remove(key);
                }
                cmd
            })
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("BIN_DIR", &bin)
            .args(["-c", script])
            .output()
            .expect("run shell");
        assert!(
            output.status.success(),
            "{shell}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "host:hello world\nhost:after\n",
            "{shell}"
        );
    }
}

#[test]
fn wrapper_account_check_keeps_disabled_accounts_in_routing() {
    let env = TestEnv::new();
    assert_eq!(env.run_ok(&["__has-account", "agy"]).trim(), "false");
    env.run_ok(&["add", "agy", "work", "--no-login"]);
    assert_eq!(env.run_ok(&["__has-account", "agy"]).trim(), "true");
    env.run_ok(&["disable", "work"]);
    assert_eq!(env.run_ok(&["__has-account", "agy"]).trim(), "true");
    env.run_ok(&["rm", "work"]);
    assert_eq!(env.run_ok(&["__has-account", "agy"]).trim(), "false");
}

#[cfg(unix)]
#[test]
fn bash_wrapper_routes_configured_accounts_and_does_not_bypass_disabled_accounts() {
    let env = TestEnv::new();
    let bin = env.dir.path().join("bin");
    fs::create_dir_all(&bin).expect("create bin");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_tayassu"), bin.join("tayassu"))
        .expect("link tayassu");
    let host = bin.join("agy");
    fs::write(&host, "#!/bin/sh\nprintf 'host invoked\\n'\n").expect("write host");
    fs::set_permissions(&host, fs::Permissions::from_mode(SCRIPT_EXECUTABLE_MODE))
        .expect("chmod host");
    let routed = env.write_mock_script("routed", "#!/bin/sh\nprintf 'routed:%s\\n' \"$*\"\n");
    env.run_ok(&["add", "agy", "work", "--no-login", "--binary", &routed]);
    let run_shell = || {
        env.command()
            .get_envs()
            .fold(Command::new("bash"), |mut cmd, (key, value)| {
                if let Some(value) = value {
                    cmd.env(key, value);
                } else {
                    cmd.env_remove(key);
                }
                cmd
            })
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .args([
                "-c",
                "eval \"$(tayassu activate bash --provider agy)\"; agy --example",
            ])
            .output()
            .expect("run wrapper")
    };
    let output = run_shell();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "routed:--example\n"
    );
    env.run_ok(&["disable", "work"]);
    let output = run_shell();
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("host invoked"));
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
    env.run_ok(&["add", "codex", "work", "--no-login", "--priority", "10"]);
    env.run_ok(&["add", "codex", "personal", "--no-login"]);
    env.run_ok(&["disable", "personal"]);

    let profile_root = env.dir.path().join("data/tayassu/profiles");
    fs::remove_dir_all(&profile_root).expect("remove test profiles");
    let preview = env.run_ok(&["run", "codex", "--dry-run"]);
    assert!(preview.contains("account: work"));
    assert!(preview.contains("policy: most-quota"));
    assert!(preview.contains("skipped personal (codex:default): disabled"));
    assert!(!profile_root.exists());
    assert!(!env.dir.path().join("state").exists());

    env.run_ok(&["cooldown", "set", "work"]);
    let blocked = env.run_cli(&["run", "codex", "--dry-run"]);
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("next known availability:"));

    let incompatible = env.run_cli(&["run", "--account", "work", "--no-failover"]);
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
    env.run_ok(&["add", "codex", "work", "--no-login", "--binary", &mock]);
    let probe = env.run_ok(&["probe", "work"]);
    assert!(probe.contains("exit status: 7"));
    let status = env.run_ok(&["ls"]);
    assert!(status.contains("ERROR"));
    assert!(status.contains("unverified"));
    assert!(status.contains("tayassu doctor, then tayassu probe work"));

    env.write_mock_script(
        "codex.sh",
        "#!/bin/sh
printf 'Not logged in\n' >&2
exit 1
",
    );
    env.run_ok(&["probe", "work"]);
    let status = env.run_ok(&["ls"]);
    assert!(status.contains("AUTH"));
    assert!(status.contains("tayassu login work"));
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
    env.run_ok(&["add", "codex", "work-codex", "--no-login"]);
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
        ("1", vec!["act"], "activate"),
        ("2", vec!["run", "co"], "codex"),
        ("4", vec!["auto", "run", "--chain", "wo"], "work-chain"),
        ("3", vec!["run", "--account", "wo"], "work-codex"),
        ("3", vec!["run", "--chain", "wo"], "work-chain"),
        ("2", vec!["run", "--dry"], "--dry-run"),
        ("2", vec!["disable", "wo"], "work-codex"),
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
        if args == ["run", "co"] {
            assert!(!String::from_utf8_lossy(&output.stdout).contains("copilot"));
        }
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
                .args(["add", "codex", &format!("worker-{index}"), "--no-login"])
                .stdout(std::process::Stdio::null())
                .spawn()
                .expect("start registration")
        })
        .collect();
    for mut child in children {
        assert!(child.wait().expect("wait for registration").success());
    }
    let status = env.run_ok(&["ls"]);
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
    env.run_ok(&["add", "agy", "agy-host", "--no-login", "--binary", &mock]);
    env.run_ok(&["probe", "agy-host"]);
    assert!(
        !env.run_cli(&["run", "agy", "--model", "claude", "--dry-run"])
            .status
            .success()
    );

    env.write_mock_script(
        "agy.sh",
        "#!/bin/sh\nprintf 'Claude and GPT models\tFHour Limit Remaining\tdisabled\nClaude and GPT models\tWeekly Limit Remaining\t35%%\n'\n",
    );
    assert!(env.run_ok(&["probe", "agy-host"]).contains("ok (1 windows"));
    assert!(env.run_ok(&["ls"]).contains("claude:35%"));
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

    env.run_ok(&["add", "opencode", "opencode-host", "--no-login"]);
    assert!(env.run_ok(&["doctor"]).contains("binary 'opencode' found"));
    assert!(
        env.run_ok(&["probe", "opencode-host"])
            .contains("opencode-host: ok (1 windows")
    );
    assert!(
        env.run_ok(&["run", "opencode", "--dry-run"])
            .contains(".opencode/bin/opencode")
    );
    assert!(env.run_ok(&["run", "opencode"]).contains("ran opencode"));
}

#[cfg(unix)]
#[test]
fn strict_routing_probes_each_profile_and_skips_unknown_or_exhausted_quota() {
    let env = TestEnv::new();
    let mock_codex = env.write_mock_script(
        "codex-quota",
        r#"#!/bin/bash
set -eu
if [[ "${1:-}" == app-server ]]; then
  IFS= read -r initialize
  [[ "$initialize" == *'"method":"initialize"'* ]]
  printf '%s\n' '{"id":1,"result":{}}'
  IFS= read -r initialized
  [[ "$initialized" == *'"method":"initialized"'* ]]
  IFS= read -r quota
  [[ "$quota" == *'"method":"account/rateLimits/read"'* ]]
  case "$CODEX_HOME" in
    */codex-full) used=100 ;;
    */codex-available) used=27 ;;
    *) exit 2 ;;
  esac
  printf '{"id":2,"result":{"rateLimits":{"primary":{"usedPercent":%s,"resetsAt":4102444800}}}}\n' "$used"
else
  printf 'ran %s\n' "${CODEX_HOME##*/}"
fi
"#,
    );
    let mock_claude =
        env.write_mock_script("claude-auth", "#!/bin/sh\nprintf '{\"loggedIn\":true}\n'\n");
    for (id, provider, priority, binary) in [
        ("codex-full", "codex", "10", &mock_codex),
        ("codex-available", "codex", "20", &mock_codex),
        ("claude-unknown", "claude", "1", &mock_claude),
    ] {
        env.run_ok(&[
            "add",
            provider,
            id,
            "--no-login",
            "--priority",
            priority,
            "--binary",
            binary,
        ]);
    }
    let config = env.dir.path().join("config/tayassu/config.toml");
    let contents = fs::read_to_string(&config).expect("read config");
    fs::write(
        &config,
        contents.replace(
            "[fallback_chains]",
            "[fallback_chains]\nauto = [\"claude\", \"codex\"]",
        ),
    )
    .expect("configure chain");

    let launch = env.run_ok(&["run", "--chain", "auto", "--require-quota"]);
    assert!(launch.contains("ran codex-available"));
    let automatic = env.run_ok(&["auto", "run"]);
    assert!(automatic.contains("ran codex-available"));
    fs::write(&config, contents).expect("remove configured chain");
    let automatic = env.run_ok(&["auto", "run"]);
    assert!(automatic.contains("ran codex-available"));
    fs::write(
        &config,
        fs::read_to_string(&config).expect("config").replace(
            "[fallback_chains]",
            "[fallback_chains]\nauto = [\"claude\", \"codex\"]",
        ),
    )
    .expect("restore chain");
    let status = env.run_ok(&["ls"]);
    assert!(status.contains("default:73%"));
    assert!(status.contains("EXHAUSTED"));

    let preview = env.run_ok(&["run", "--chain", "auto", "--require-quota", "--dry-run"]);
    assert!(preview.contains("account: codex-available"));
}

#[cfg(unix)]
#[test]
fn opencode_strict_routing_uses_the_catalog_and_passes_the_measured_model() {
    let env = TestEnv::new();
    let mock_opencode = env.write_mock_script(
        "opencode-catalog",
        r#"#!/bin/sh
case "$1" in
  auth) printf '%s\n' '[{"id":"github-copilot","connections":[{"id":"linked"}]},{"id":"google","connections":[{"id":"unknown"}]}]' ;;
  models) printf '%s\n' 'github-copilot/gpt-test' 'google/gemini-test' ;;
  *) printf '%s\n' "$*" ;;
esac
"#,
    );
    let mock_copilot = env.write_mock_script(
        "copilot-quota",
        r#"#!/bin/bash
IFS= read -r header
IFS= read -r empty
length="${header#Content-Length: }"
IFS= read -r -N "${length//$'\r'/}" request
[[ "$request" == *'account.getQuota'* ]] || exit 2
response='{"jsonrpc":"2.0","id":2,"result":{"quotaSnapshots":{"premium_interactions":{"entitlementRequests":300,"remainingPercentage":42,"resetDate":"2099-01-01T00:00:00Z"}}}}'
printf 'Content-Length: %s\r\n\r\n%s' "${#response}" "$response"
"#,
    );
    env.run_ok(&[
        "add",
        "opencode",
        "open",
        "--no-login",
        "--binary",
        &mock_opencode,
    ]);
    env.run_ok(&[
        "add",
        "copilot",
        "copilot",
        "--no-login",
        "--binary",
        &mock_copilot,
    ]);
    let config = env.dir.path().join("config/tayassu/config.toml");
    let contents = fs::read_to_string(&config).expect("read config");
    let contents = contents.replace(
        "provider = \"opencode\"",
        "provider = \"opencode\"\nopencode_copilot_account = \"copilot\"",
    );
    fs::write(config, contents).expect("link matching Copilot profile");

    let launch = env.run_ok(&["run", "opencode", "--require-quota"]);
    assert!(launch.contains("--model github-copilot/gpt-test"));
    let models = env.run_ok(&["ls", "opencode", "--list-models"]);
    assert!(models.contains("github-copilot/gpt-test 42% github-copilot"));
    assert!(models.contains("google/gemini-test unknown google"));
    let unavailable = env.run_cli(&[
        "run",
        "opencode",
        "--require-quota",
        "--model",
        "google/gemini-test",
        "--dry-run",
    ]);
    assert!(!unavailable.status.success());
}

#[cfg(unix)]
#[test]
fn auto_probe_installs_and_enables_timer_without_overwriting_existing_units() {
    let env = TestEnv::new();
    env.write_mock_script(
        "systemctl",
        "#!/bin/sh\ncase \"$*\" in *is-enabled*) exit 1;; esac\nexit 0\n",
    );
    let path = format!("{}:/usr/bin:/bin", env.dir.path().display());
    let run = || {
        env.command()
            .env("PATH", &path)
            .args(["auto", "probe"])
            .output()
            .expect("run auto probe")
    };
    let first = run();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let timer = env
        .dir
        .path()
        .join("config/systemd/user/tayassu-probe.timer");
    assert!(timer.exists());
    fs::write(&timer, "custom timer").expect("customize timer");
    let second = run();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(
        fs::read_to_string(timer).expect("read timer"),
        "custom timer"
    );
}

#[test]
fn shared_token_does_not_confirm_two_copilot_profiles() {
    let env = TestEnv::new();
    let ids = ["copilot-work", "copilot-personal"];
    for id in ids {
        env.run_ok(&["add", "copilot", id, "--no-login", "--binary", "/bin/false"]);
    }
    let output = env
        .command()
        .env("GH_TOKEN", "synthetic")
        .arg("probe")
        .output()
        .expect("probe");
    assert!(output.status.success());
    let report = String::from_utf8(output.stdout).expect("text report");
    for id in ids {
        assert!(report.contains(&format!(
            "unable to verify Copilot authentication for account '{id}'"
        )));
    }
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
    env.run_ok(&["setup", "shell"]);

    let fake_bin = env.dir.path().join("tayassu-copy");
    fs::write(&fake_bin, "bin").expect("write fake bin");

    let dry = env
        .command()
        .args(["uninstall", "--dry-run"])
        .env("TAYASSU_UNINSTALL_BIN", &fake_bin)
        .output()
        .expect("dry uninstall");
    assert!(dry.status.success());
    let dry_out = String::from_utf8_lossy(&dry.stdout);
    assert!(dry_out.contains("[dry-run] shell:"));
    assert!(dry_out.contains("[dry-run] binary:"));
    assert!(fake_bin.exists());

    let run = env
        .command()
        .args(["uninstall", "--yes"])
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
    assert!(env.dir.path().join("config/tayassu/config.toml").exists());

    let purge = env
        .command()
        .args(["uninstall", "--yes", "--purge"])
        .env("TAYASSU_UNINSTALL_BIN", &fake_bin)
        .output()
        .expect("purge");
    assert!(purge.status.success());
    assert!(!env.dir.path().join("config/tayassu").exists());
    assert!(!env.dir.path().join("data/tayassu").exists());
}

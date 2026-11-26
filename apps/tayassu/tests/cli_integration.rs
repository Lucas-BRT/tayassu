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

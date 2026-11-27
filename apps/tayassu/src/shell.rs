use clap::ValueEnum;
use clap_complete::CompletionCandidate;
use std::collections::BTreeSet;
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use tayassu_core::{AccountConfig, AccountId, ProviderKind};
use tayassu_store::{AppConfig, RuntimeState, XdgPaths};

/// Default providers wrapped by `tayassu activate` when `--provider` is omitted.
pub const DEFAULT_SHELL_PROVIDERS: &[ProviderKind] = &[
    ProviderKind::Agy,
    ProviderKind::Claude,
    ProviderKind::Codex,
    ProviderKind::Copilot,
    ProviderKind::Opencode,
];

/// Environment variable used by the generated shell completers.
pub const COMPLETION_ENV: &str = "TAYASSU_COMPLETE";

/// Marker line that makes rc-file edits idempotent.
const RC_MARKER: &str = "# tayassu activate";

/// Fish configuration snippet directory, relative to `$HOME`.
const FISH_CONF_DIR: &str = ".config/fish";

/// Fish snippet filename under `conf.d/` (autoloaded by fish).
const FISH_CONF_FILE: &str = "conf.d/tayassu.fish";

/// Environment variable set by `clap_complete` with the active completion word index.
const COMPLETE_INDEX_ENV: &str = "_CLAP_COMPLETE_INDEX";

/// Flags that consume a following value token in `run`, `where`, and `ls`.
const VALUE_FLAGS: &[&str] = &["--account", "--chain", "--model", "--policy"];

fn load_completion_config() -> Option<AppConfig> {
    let paths = XdgPaths::from_env().ok()?;
    AppConfig::load(&paths.config_file).ok()
}

fn load_completion_state() -> Option<RuntimeState> {
    let paths = XdgPaths::from_env().ok()?;
    RuntimeState::load(&paths.state_file).ok()
}

#[derive(Debug, Default, PartialEq, Eq)]
struct CompletionContext {
    provider: Option<ProviderKind>,
    account: Option<AccountId>,
    first_positional_is_account: bool,
}

impl CompletionContext {
    fn from_env(config: &AppConfig) -> Self {
        let args: Vec<String> = env::args_os()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let index = env::var(COMPLETE_INDEX_ENV)
            .ok()
            .and_then(|raw| raw.parse::<usize>().ok());
        Self::from_args(&args, index, config)
    }

    fn from_args(args: &[String], index: Option<usize>, config: &AppConfig) -> Self {
        let Some(sep) = args.iter().position(|arg| arg == "--") else {
            return Self::default();
        };
        let words = &args[sep + 1..];
        if words.len() < 2 {
            return Self::default();
        }
        let cursor = index
            .unwrap_or_else(|| words.len().saturating_sub(1))
            .min(words.len());
        // Skip binary name (`words[0]`) and subcommand (`words[1]`).
        if cursor <= 2 {
            return Self::default();
        }
        let committed = &words[2..cursor];
        let mut ctx = Self::default();
        let mut idx = 0usize;
        let mut positional_seen = 0usize;

        while idx < committed.len() {
            let token = committed[idx].as_str();
            if token == "--" {
                break;
            }
            if let Some(value) = token.strip_prefix("--account=") {
                if let Ok(id) = value.parse::<AccountId>() {
                    ctx.account = Some(id);
                }
                idx += 1;
                continue;
            }
            if VALUE_FLAGS.contains(&token) {
                if token == "--account"
                    && let Some(value) = committed.get(idx + 1)
                    && let Ok(id) = value.parse::<AccountId>()
                {
                    ctx.account = Some(id);
                }
                idx += 2;
                continue;
            }
            if token.starts_with('-') {
                idx += 1;
                continue;
            }
            if positional_seen == 0 {
                if let Ok(provider) = token.parse::<ProviderKind>() {
                    ctx.provider = Some(provider);
                } else if let Ok(id) = token.parse::<AccountId>()
                    && config.find_account(&id).is_some()
                {
                    ctx.account = Some(id);
                    ctx.first_positional_is_account = true;
                }
            }
            positional_seen += 1;
            idx += 1;
        }

        ctx
    }

    fn effective_provider(&self, config: &AppConfig) -> Option<ProviderKind> {
        self.provider.or_else(|| {
            self.account
                .as_ref()
                .and_then(|id| config.find_account(id))
                .map(|account| account.provider)
        })
    }
}

fn account_candidate(account: &AccountConfig) -> CompletionCandidate {
    let help = if account.label.is_empty() {
        account.provider.to_string()
    } else {
        account.label.clone()
    };
    CompletionCandidate::new(account.id.to_string()).help(Some(help.into()))
}

pub fn account_candidates() -> Vec<CompletionCandidate> {
    load_completion_config().map_or_else(Vec::new, |config| {
        config.accounts.iter().map(account_candidate).collect()
    })
}

pub fn enabled_account_candidates() -> Vec<CompletionCandidate> {
    load_completion_config().map_or_else(Vec::new, |config| {
        config
            .accounts
            .iter()
            .filter(|account| account.enabled)
            .map(account_candidate)
            .collect()
    })
}

pub fn disabled_account_candidates() -> Vec<CompletionCandidate> {
    load_completion_config().map_or_else(Vec::new, |config| {
        config
            .accounts
            .iter()
            .filter(|account| !account.enabled)
            .map(account_candidate)
            .collect()
    })
}

/// First positional target for `run` and `where`: enabled providers plus configured accounts.
pub fn run_target_candidates() -> Vec<CompletionCandidate> {
    load_completion_config().map_or_else(Vec::new, |config| {
        let mut seen = BTreeSet::new();
        let mut candidates = Vec::new();
        for &provider in DEFAULT_SHELL_PROVIDERS
            .iter()
            .chain(&[ProviderKind::Custom])
        {
            if config
                .accounts
                .iter()
                .any(|account| account.enabled && account.provider == provider)
            {
                let name = provider.as_str().to_owned();
                if seen.insert(name.clone()) {
                    candidates.push(
                        CompletionCandidate::new(name).help(Some("provider (auto-route)".into())),
                    );
                }
            }
        }
        for account in &config.accounts {
            if seen.insert(account.id.as_str().to_owned()) {
                candidates.push(account_candidate(account));
            }
        }
        candidates
    })
}

/// Account candidates for `run` / `where`, filtered by the provider already typed on the command line.
pub fn run_account_candidates() -> Vec<CompletionCandidate> {
    load_completion_config().map_or_else(Vec::new, |config| {
        let ctx = CompletionContext::from_env(&config);
        if ctx.first_positional_is_account {
            return Vec::new();
        }
        config
            .accounts
            .iter()
            .filter(|account| ctx.provider.is_none_or(|p| account.provider == p))
            .map(account_candidate)
            .collect()
    })
}

/// Targets for `tayassu probe`: configured accounts and providers with at least one account.
pub fn probe_target_candidates() -> Vec<CompletionCandidate> {
    load_completion_config().map_or_else(Vec::new, |config| {
        let mut seen = BTreeSet::new();
        let mut candidates = Vec::new();
        for account in &config.accounts {
            if seen.insert(account.id.as_str().to_owned()) {
                candidates.push(account_candidate(account));
            }
        }
        for &provider in DEFAULT_SHELL_PROVIDERS
            .iter()
            .chain(&[ProviderKind::Custom])
        {
            if config
                .accounts
                .iter()
                .any(|account| account.provider == provider)
                && seen.insert(provider.as_str().to_owned())
            {
                candidates.push(
                    CompletionCandidate::new(provider.as_str())
                        .help(Some("all accounts for provider".into())),
                );
            }
        }
        candidates
    })
}

/// Model buckets discovered in `state.json`, filtered by the provider or account on the command line.
pub fn model_candidates() -> Vec<CompletionCandidate> {
    let Some(config) = load_completion_config() else {
        return Vec::new();
    };
    let Some(state) = load_completion_state() else {
        return Vec::new();
    };
    let ctx = CompletionContext::from_env(&config);
    let provider_filter = ctx.effective_provider(&config);
    let mut buckets = BTreeSet::new();
    for status in &state.statuses {
        let Some(account) = config.find_account(&status.account_id) else {
            continue;
        };
        if let Some(account_filter) = &ctx.account
            && &account.id != account_filter
        {
            continue;
        }
        if let Some(provider) = provider_filter
            && account.provider != provider
        {
            continue;
        }
        for window in &status.windows {
            buckets.insert(window.bucket.as_str().to_owned());
        }
        for model in status.model_buckets.keys() {
            buckets.insert(model.as_str().to_owned());
        }
    }
    buckets.into_iter().map(CompletionCandidate::new).collect()
}

pub fn chain_candidates() -> Vec<CompletionCandidate> {
    load_completion_config().map_or_else(Vec::new, |config| {
        config
            .fallback_chains
            .into_keys()
            .map(CompletionCandidate::new)
            .collect()
    })
}

pub fn provider_candidates() -> Vec<CompletionCandidate> {
    DEFAULT_SHELL_PROVIDERS
        .iter()
        .copied()
        .chain([ProviderKind::Custom])
        .map(|provider| CompletionCandidate::new(provider.as_str()))
        .collect()
}

/// A shell tayassu can activate: wrapper functions, completion and one-time installation.
pub trait Shell {
    /// Shell name as used in `TAYASSU_COMPLETE=<name>` and `tayassu activate <name>`.
    fn name(&self) -> &'static str;

    /// Wrapper function source routing `provider`'s CLI through `tayassu run`.
    fn wrapper(&self, provider: ProviderKind, failover: bool) -> String;

    /// Line registering dynamic tab completion.
    fn completion(&self) -> String;

    /// Idempotently hooks `tayassu activate` into this shell's startup under `home`.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when a startup file cannot be written.
    fn install(&self, home: &Path) -> io::Result<Option<String>>;

    /// Removes the `tayassu activate` hook from this shell's startup under `home`.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when a startup file cannot be read or updated.
    fn uninstall(&self, home: &Path) -> io::Result<Option<String>>;

    /// Startup file path that would be modified or removed if currently configured.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when a startup file cannot be inspected.
    fn installed_path(&self, home: &Path) -> io::Result<Option<PathBuf>>;

    /// Full activation script: wrappers for `providers` followed by completion.
    fn activation(&self, providers: &[ProviderKind], failover: bool) -> String {
        let mut out = String::from(
            "# Generated by `tayassu activate`. Bypass any wrapper with `command <cli> ...`.\n",
        );
        for &provider in providers {
            let _ = writeln!(out, "{}", self.wrapper(provider, failover));
        }
        let _ = writeln!(out, "{}", self.completion());
        out
    }
}

/// A [`Shell`] configured through a single rc file that sources `tayassu activate`.
pub trait RcShell: Shell {
    /// rc filename relative to `$HOME`.
    fn rc_file(&self) -> &'static str;

    /// Line appended to the rc file.
    fn activate_line(&self) -> String {
        format!(
            "command -v tayassu >/dev/null 2>&1 && eval \"$(tayassu activate {})\"",
            self.name()
        )
    }

    /// Appends the marked activation block to an existing rc file, once.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the rc file cannot be read or appended to.
    fn install_rc(&self, home: &Path) -> io::Result<Option<String>> {
        use std::io::Write as _;
        let path = home.join(self.rc_file());
        if !path.exists() {
            return Ok(None);
        }
        let existing = fs::read_to_string(&path)?;
        if existing.contains(RC_MARKER) {
            return Ok(None);
        }
        let sep = if existing.is_empty() || existing.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        let mut file = fs::OpenOptions::new().append(true).open(&path)?;
        write!(file, "{sep}\n{RC_MARKER}\n{}\n", self.activate_line())?;
        Ok(Some(format!(
            "{}: added activation to {}",
            self.name(),
            path.display()
        )))
    }

    /// Returns the rc file path if it contains the `tayassu activate` marker.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the rc file cannot be read.
    fn installed_rc(&self, home: &Path) -> io::Result<Option<PathBuf>> {
        let path = home.join(self.rc_file());
        if !path.is_file() {
            return Ok(None);
        }
        let existing = fs::read_to_string(&path)?;
        Ok(existing.contains(RC_MARKER).then_some(path))
    }

    /// Removes the marked activation block from the rc file, if present.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the rc file cannot be read or written.
    fn uninstall_rc(&self, home: &Path) -> io::Result<Option<String>> {
        let path = home.join(self.rc_file());
        if !path.is_file() {
            return Ok(None);
        }
        let existing = fs::read_to_string(&path)?;
        if !existing.contains(RC_MARKER) {
            return Ok(None);
        }
        let mut kept: Vec<&str> = Vec::new();
        let mut skip_next = false;
        for line in existing.lines() {
            if skip_next {
                skip_next = false;
                if line.contains("command -v tayassu") && line.contains("tayassu activate") {
                    continue;
                }
            }
            if line.trim() == RC_MARKER {
                if kept.last().is_some_and(|prev| prev.is_empty()) {
                    kept.pop();
                }
                skip_next = true;
                continue;
            }
            kept.push(line);
        }
        let mut updated = kept.join("\n");
        if existing.ends_with('\n') && !updated.is_empty() {
            updated.push('\n');
        }
        fs::write(&path, updated)?;
        Ok(Some(format!(
            "{}: removed activation from {}",
            self.name(),
            path.display()
        )))
    }
}

fn posix_wrapper(provider: ProviderKind, failover: bool) -> String {
    let flag = if failover { "" } else { " --no-failover" };
    format!(
        "{}() {{ if [ ! -x \"$(command -v tayassu)\" ] || [ \"$(command tayassu __has-account {})\" = false ]; then command {} \"$@\"; else command tayassu run {}{flag} -- \"$@\"; fi; }}",
        provider.default_binary(),
        provider.as_str(),
        provider.default_binary(),
        provider.as_str()
    )
}

struct Bash;
struct Zsh;
struct Fish;

impl Shell for Bash {
    fn name(&self) -> &'static str {
        "bash"
    }
    fn wrapper(&self, provider: ProviderKind, failover: bool) -> String {
        posix_wrapper(provider, failover)
    }
    fn completion(&self) -> String {
        format!("eval \"$({COMPLETION_ENV}=bash tayassu)\"")
    }
    fn install(&self, home: &Path) -> io::Result<Option<String>> {
        self.install_rc(home)
    }
    fn uninstall(&self, home: &Path) -> io::Result<Option<String>> {
        self.uninstall_rc(home)
    }
    fn installed_path(&self, home: &Path) -> io::Result<Option<PathBuf>> {
        self.installed_rc(home)
    }
}

impl RcShell for Bash {
    fn rc_file(&self) -> &'static str {
        ".bashrc"
    }
}

impl Shell for Zsh {
    fn name(&self) -> &'static str {
        "zsh"
    }
    fn wrapper(&self, provider: ProviderKind, failover: bool) -> String {
        posix_wrapper(provider, failover)
    }
    fn completion(&self) -> String {
        format!("eval \"$({COMPLETION_ENV}=zsh tayassu)\"")
    }
    fn install(&self, home: &Path) -> io::Result<Option<String>> {
        self.install_rc(home)
    }
    fn uninstall(&self, home: &Path) -> io::Result<Option<String>> {
        self.uninstall_rc(home)
    }
    fn installed_path(&self, home: &Path) -> io::Result<Option<PathBuf>> {
        self.installed_rc(home)
    }
}

impl RcShell for Zsh {
    fn rc_file(&self) -> &'static str {
        ".zshrc"
    }
}

impl Shell for Fish {
    fn name(&self) -> &'static str {
        "fish"
    }
    fn wrapper(&self, provider: ProviderKind, failover: bool) -> String {
        let cmd = provider.default_binary();
        let flag = if failover { "" } else { " --no-failover" };
        format!(
            "function {cmd} --wraps {cmd}\n    if not command -q tayassu; or test (command tayassu __has-account {}) = false\n        command {cmd} $argv\n    else\n        command tayassu run {}{flag} -- $argv\n    end\nend",
            provider.as_str(),
            provider.as_str()
        )
    }
    fn completion(&self) -> String {
        format!("env {COMPLETION_ENV}=fish tayassu | source")
    }
    fn install(&self, home: &Path) -> io::Result<Option<String>> {
        if !home.join(FISH_CONF_DIR).is_dir() {
            return Ok(None);
        }
        let path = home.join(FISH_CONF_DIR).join(FISH_CONF_FILE);
        let script = "command -q tayassu; and tayassu activate fish | source\n";
        if fs::read_to_string(&path).ok().as_deref() == Some(script) {
            return Ok(None);
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, script)?;
        Ok(Some(format!("fish: wrote {}", path.display())))
    }
    fn uninstall(&self, home: &Path) -> io::Result<Option<String>> {
        let path = home.join(FISH_CONF_DIR).join(FISH_CONF_FILE);
        if !path.exists() {
            return Ok(None);
        }
        fs::remove_file(&path)?;
        Ok(Some(format!("fish: removed {}", path.display())))
    }
    fn installed_path(&self, home: &Path) -> io::Result<Option<PathBuf>> {
        let path = home.join(FISH_CONF_DIR).join(FISH_CONF_FILE);
        Ok(path.exists().then_some(path))
    }
}

/// Target shell syntax supported by `tayassu activate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ShellKind {
    /// GNU Bash (`~/.bashrc`).
    Bash,
    /// Zsh (`~/.zshrc`).
    Zsh,
    /// Fish (`~/.config/fish`).
    Fish,
}

impl ShellKind {
    /// Every supported shell.
    pub const ALL: [Self; 3] = [Self::Bash, Self::Zsh, Self::Fish];

    /// Behaviour implementation for this shell.
    #[must_use]
    pub fn shell(self) -> &'static dyn Shell {
        match self {
            Self::Bash => &Bash,
            Self::Zsh => &Zsh,
            Self::Fish => &Fish,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_wrappers_with_optional_failover() {
        let bash = ShellKind::Bash
            .shell()
            .activation(&[ProviderKind::Agy], true);
        assert!(bash.contains("command tayassu __has-account agy"));
        assert!(bash.contains("eval \"$(TAYASSU_COMPLETE=bash tayassu)\""));

        let fish = ShellKind::Fish
            .shell()
            .activation(&[ProviderKind::Codex], false);
        assert!(fish.contains("function codex --wraps codex"));
        assert!(fish.contains("tayassu run codex --no-failover -- $argv"));
    }

    #[test]
    fn install_and_uninstall_are_idempotent() {
        let home = tempfile::tempdir().expect("tempdir");
        fs::write(home.path().join(".bashrc"), "export A=1").expect("write");
        fs::create_dir_all(home.path().join(FISH_CONF_DIR)).expect("mkdir");

        for kind in ShellKind::ALL {
            kind.shell().install(home.path()).expect("install");
        }
        let bashrc = fs::read_to_string(home.path().join(".bashrc")).expect("read");
        assert!(bashrc.contains("tayassu activate bash"));
        assert!(!home.path().join(".zshrc").exists());
        assert!(
            home.path()
                .join(".config/fish/conf.d/tayassu.fish")
                .exists()
        );

        for kind in ShellKind::ALL {
            kind.shell().uninstall(home.path()).expect("uninstall");
        }
        assert_eq!(
            fs::read_to_string(home.path().join(".bashrc")).expect("read"),
            "export A=1\n"
        );
        assert!(
            !home
                .path()
                .join(".config/fish/conf.d/tayassu.fish")
                .exists()
        );
    }
}

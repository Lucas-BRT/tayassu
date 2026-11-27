//! Managed artifacts shared between `setup` and `uninstall`.

use crate::app::App;
use crate::shell::ShellKind;
use crate::systemd::SystemdUnits;
use anyhow::{Context, Result};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Relative path under `$XDG_CONFIG_HOME` where `systemd --user` units live.
const SYSTEMD_USER_REL_DIR: &str = "systemd/user";

/// Environment override used in tests to point binary removal at a disposable copy.
const UNINSTALL_BIN_ENV: &str = "TAYASSU_UNINSTALL_BIN";

/// An artifact installed and/or removed by `tayassu setup` and `tayassu uninstall`.
pub trait ManagedArtifact {
    /// Short category label printed in reports.
    fn label(&self) -> &'static str;

    /// Installs or updates the artifact; returns one line per change (empty if already current).
    ///
    /// # Errors
    ///
    /// Returns an error if the artifact cannot be written or configured.
    fn install(&self, app: &App) -> Result<Vec<String>>;

    /// Lists paths or targets that `remove` would touch without modifying anything.
    ///
    /// # Errors
    ///
    /// Returns an error if inspection fails.
    fn planned_removals(&self, app: &App) -> Result<Vec<String>>;

    /// Removes the artifact; returns one line per removal.
    ///
    /// # Errors
    ///
    /// Returns an error if an existing artifact cannot be removed.
    fn remove(&self, app: &App) -> Result<Vec<String>>;
}

/// Shell activation hooks in `~/.bashrc`, `~/.zshrc`, and `~/.config/fish/conf.d/tayassu.fish`.
pub struct ShellActivation;

/// Shell completion hooks, independent of provider wrappers.
pub struct ShellCompletion;

const COMPLETION_MARKER: &str = "# tayassu completion";
const FISH_COMPLETION_PATH: &str = "fish/conf.d/tayassu-completion.fish";

impl ShellCompletion {
    fn entries(app: &App) -> Vec<(PathBuf, &'static str)> {
        let home = &app.paths.home_dir;
        let login_shell = env::var_os("SHELL").map(PathBuf::from);
        let uses = |shell: &str| {
            login_shell
                .as_deref()
                .and_then(Path::file_name)
                .is_some_and(|name| name == shell)
        };
        let zsh_dir = env::var_os("ZDOTDIR")
            .filter(|dir| !dir.is_empty())
            .map_or_else(|| home.clone(), PathBuf::from);
        let mut entries = Vec::new();
        for (path, shell) in [
            (home.join(".bashrc"), "bash"),
            (zsh_dir.join(".zshrc"), "zsh"),
        ] {
            if path.is_file() || uses(shell) {
                entries.push((path, shell));
            }
        }
        let default_config = home.join(".config");
        let config = env::var_os("XDG_CONFIG_HOME")
            .filter(|dir| !dir.is_empty())
            .map_or_else(|| default_config.clone(), PathBuf::from);
        if config.join("fish").is_dir() || uses("fish") {
            entries.push((config.join(FISH_COMPLETION_PATH), "fish"));
        }
        let legacy = default_config.join(FISH_COMPLETION_PATH);
        if config != default_config && legacy.is_file() {
            entries.push((legacy, "fish"));
        }
        entries
    }

    fn script(shell: &str) -> String {
        match shell {
            "fish" => "command -q tayassu; and env TAYASSU_COMPLETE=fish tayassu | source".to_owned(),
            // Zsh's -i skips insecure completion directories without prompting.
            "zsh" => "if command -v tayassu >/dev/null 2>&1 && { (( $+functions[compdef] )) || { autoload -Uz compinit && compinit -i -D; }; }; then eval \"$(TAYASSU_COMPLETE=zsh tayassu)\"; fi".to_owned(),
            _ => format!(
                "command -v tayassu >/dev/null 2>&1 && eval \"$(TAYASSU_COMPLETE={shell} tayassu)\""
            ),
        }
    }
}

impl ManagedArtifact for ShellCompletion {
    fn label(&self) -> &'static str {
        "completion"
    }
    fn install(&self, app: &App) -> Result<Vec<String>> {
        use std::io::Write as _;
        let mut changes = Vec::new();
        for (path, shell) in Self::entries(app) {
            let existing = match fs::read_to_string(&path) {
                Ok(contents) => contents,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("failed to read {}", path.display()));
                }
            };
            let line = Self::script(shell);
            if shell == "zsh" {
                let previous = format!(
                    "{COMPLETION_MARKER}\ncommand -v tayassu >/dev/null 2>&1 && eval \"$(TAYASSU_COMPLETE=zsh tayassu)\"\n"
                );
                if existing.contains(&previous) {
                    fs::write(
                        &path,
                        existing.replace(&previous, &format!("{COMPLETION_MARKER}\n{line}\n")),
                    )?;
                    changes.push(format!("{shell}: updated completion in {}", path.display()));
                    continue;
                }
            }
            if existing.lines().any(|entry| entry == COMPLETION_MARKER) {
                continue;
            }
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;
            if !existing.is_empty() && !existing.ends_with('\n') {
                writeln!(file)?;
            }
            writeln!(file, "{COMPLETION_MARKER}\n{line}")?;
            changes.push(format!("{shell}: added completion to {}", path.display()));
        }
        Ok(changes)
    }
    fn planned_removals(&self, app: &App) -> Result<Vec<String>> {
        Ok(Self::entries(app)
            .into_iter()
            .filter(|(path, _)| {
                fs::read_to_string(path).is_ok_and(|s| s.contains(COMPLETION_MARKER))
            })
            .map(|(path, _)| path.display().to_string())
            .collect())
    }
    fn remove(&self, app: &App) -> Result<Vec<String>> {
        let mut removed = Vec::new();
        for (path, shell) in Self::entries(app) {
            let Ok(existing) = fs::read_to_string(&path) else {
                continue;
            };
            if !existing.contains(COMPLETION_MARKER) {
                continue;
            }
            {
                let mut lines = existing.lines();
                let mut kept = Vec::new();
                while let Some(line) = lines.next() {
                    if line == COMPLETION_MARKER {
                        if let Some(next) = lines.next() {
                            if !next.contains("TAYASSU_COMPLETE=") {
                                kept.push(line);
                                kept.push(next);
                            }
                        } else {
                            kept.push(line);
                        }
                    } else {
                        kept.push(line);
                    }
                }
                let mut updated = kept.join("\n");
                if existing.ends_with('\n') && !updated.is_empty() {
                    updated.push('\n');
                }
                if shell == "fish" && updated.is_empty() {
                    fs::remove_file(&path)?;
                } else {
                    fs::write(&path, updated)?;
                }
            }
            removed.push(format!("removed completion from {}", path.display()));
        }
        Ok(removed)
    }
}

impl ManagedArtifact for ShellActivation {
    fn label(&self) -> &'static str {
        "shell"
    }

    fn install(&self, app: &App) -> Result<Vec<String>> {
        let mut changes = Vec::new();
        for kind in ShellKind::ALL {
            let change = kind
                .shell()
                .install(&app.paths.home_dir)
                .with_context(|| format!("failed to set up {}", kind.shell().name()))?;
            changes.extend(change);
        }
        Ok(changes)
    }

    fn planned_removals(&self, app: &App) -> Result<Vec<String>> {
        let mut items = Vec::new();
        for kind in ShellKind::ALL {
            if let Some(path) = kind
                .shell()
                .installed_path(&app.paths.home_dir)
                .with_context(|| format!("failed to inspect {}", kind.shell().name()))?
            {
                items.push(format!("{}: {}", kind.shell().name(), path.display()));
            }
        }
        Ok(items)
    }

    fn remove(&self, app: &App) -> Result<Vec<String>> {
        let mut removed = Vec::new();
        for kind in ShellKind::ALL {
            let change = kind
                .shell()
                .uninstall(&app.paths.home_dir)
                .with_context(|| format!("failed to remove {}", kind.shell().name()))?;
            removed.extend(change);
        }
        Ok(removed)
    }
}

/// `systemd --user` probe service and timer units.
pub struct SystemdTimer {
    /// Background probe interval in minutes.
    pub interval_minutes: u64,
}

impl SystemdTimer {
    fn systemd_dir(app: &App) -> Result<PathBuf> {
        let config_base = app
            .paths
            .config_file
            .parent()
            .and_then(Path::parent)
            .context("failed to resolve XDG config root")?;
        Ok(config_base.join(SYSTEMD_USER_REL_DIR))
    }
}

impl ManagedArtifact for SystemdTimer {
    fn label(&self) -> &'static str {
        "systemd"
    }

    fn install(&self, app: &App) -> Result<Vec<String>> {
        let exe = env::current_exe().context("failed to resolve current tayassu binary")?;
        let dir = Self::systemd_dir(app)?;
        let (svc, tmr) = SystemdUnits::new(exe, self.interval_minutes).install_to(&dir)?;
        Ok(vec![
            format!("installed {}", svc.display()),
            format!("installed {}", tmr.display()),
            "installation does not enable the timer; enable with: systemctl --user daemon-reload && systemctl --user enable --now tayassu-probe.timer"
                .to_owned(),
        ])
    }

    fn planned_removals(&self, app: &App) -> Result<Vec<String>> {
        let dir = Self::systemd_dir(app)?;
        Ok(SystemdUnits::installed_in(&dir)
            .into_iter()
            .map(|path| path.display().to_string())
            .collect())
    }

    fn remove(&self, app: &App) -> Result<Vec<String>> {
        let dir = Self::systemd_dir(app)?;
        Ok(SystemdUnits::uninstall_from(&dir)?
            .into_iter()
            .map(|path| format!("removed {}", path.display()))
            .collect())
    }
}

/// The `tayassu` executable itself.
pub struct BinaryFile;

impl BinaryFile {
    fn target_path() -> Result<PathBuf> {
        if let Some(overridden) = env::var_os(UNINSTALL_BIN_ENV).filter(|v| !v.is_empty()) {
            return Ok(PathBuf::from(overridden));
        }
        env::current_exe().context("failed to resolve current tayassu binary")
    }
}

impl ManagedArtifact for BinaryFile {
    fn label(&self) -> &'static str {
        "binary"
    }

    fn install(&self, _app: &App) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    fn planned_removals(&self, _app: &App) -> Result<Vec<String>> {
        let path = Self::target_path()?;
        Ok(path
            .exists()
            .then(|| path.display().to_string())
            .into_iter()
            .collect())
    }

    fn remove(&self, _app: &App) -> Result<Vec<String>> {
        let path = Self::target_path()?;
        if !path.exists() {
            return Ok(Vec::new());
        }
        fs::remove_file(&path).with_context(|| format!("failed to remove {}", path.display()))?;
        Ok(vec![format!("removed {}", path.display())])
    }
}

/// User configuration, runtime state, and isolated account profiles (only removed with `--purge`).
pub struct UserData;

impl UserData {
    fn data_dirs(app: &App) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if let Some(dir) = app.paths.config_file.parent() {
            dirs.push(dir.to_path_buf());
        }
        if let Some(dir) = app.paths.state_file.parent()
            && !dirs.iter().any(|existing| existing == dir)
        {
            dirs.push(dir.to_path_buf());
        }
        if let Some(dir) = app.paths.profiles_dir.parent()
            && !dirs.iter().any(|existing| existing == dir)
        {
            dirs.push(dir.to_path_buf());
        }
        dirs.into_iter().filter(|dir| dir.exists()).collect()
    }
}

impl ManagedArtifact for UserData {
    fn label(&self) -> &'static str {
        "data"
    }

    fn install(&self, _app: &App) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    fn planned_removals(&self, app: &App) -> Result<Vec<String>> {
        Ok(Self::data_dirs(app)
            .into_iter()
            .map(|dir| dir.display().to_string())
            .collect())
    }

    fn remove(&self, app: &App) -> Result<Vec<String>> {
        let mut removed = Vec::new();
        for dir in Self::data_dirs(app) {
            fs::remove_dir_all(&dir)
                .with_context(|| format!("failed to remove {}", dir.display()))?;
            removed.push(format!("removed {}", dir.display()));
        }
        Ok(removed)
    }
}

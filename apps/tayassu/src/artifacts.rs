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
            "enable with: systemctl --user daemon-reload && systemctl --user enable --now tayassu-probe.timer"
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

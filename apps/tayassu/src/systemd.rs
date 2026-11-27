use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Service unit filename installed under `$XDG_CONFIG_HOME/systemd/user/`.
pub const SYSTEMD_SERVICE_NAME: &str = "tayassu-probe.service";

/// Timer unit filename installed under `$XDG_CONFIG_HOME/systemd/user/`.
pub const SYSTEMD_TIMER_NAME: &str = "tayassu-probe.timer";

/// Default background probe interval in minutes.
pub const DEFAULT_PROBE_INTERVAL_MINUTES: u64 = 15;

/// Delay after user login before the first background probe runs.
const BOOT_DELAY_MINUTES: u64 = 2;

/// Generator for `systemd --user` background probe units.
#[derive(Debug, Clone)]
pub struct SystemdUnits {
    binary_path: PathBuf,
    interval_minutes: u64,
}

impl SystemdUnits {
    /// Creates a unit generator for `binary_path` running every `interval_minutes`.
    #[must_use]
    pub const fn new(binary_path: PathBuf, interval_minutes: u64) -> Self {
        Self {
            binary_path,
            interval_minutes,
        }
    }

    /// Renders `(service_unit, timer_unit)` contents.
    #[must_use]
    pub fn render(&self) -> (String, String) {
        let bin = format!("\"{}\"", self.binary_path.display());
        let service = format!(
            "[Unit]\n\
             Description=Refresh tayassu AI CLI account quotas and cooldowns\n\
             After=network-online.target\n\
             \n\
             [Service]\n\
             Type=oneshot\n\
             ExecStart={bin} probe --notify\n"
        );
        let timer = format!(
            "[Unit]\n\
             Description=Periodic tayassu quota probe timer\n\
             \n\
             [Timer]\n\
             OnBootSec={BOOT_DELAY_MINUTES}m\n\
             OnUnitActiveSec={}m\n\
             Persistent=true\n\
             \n\
             [Install]\n\
             WantedBy=timers.target\n",
            self.interval_minutes
        );
        (service, timer)
    }

    /// Writes both unit files into `systemd_user_dir`.
    ///
    /// # Errors
    ///
    /// Returns an error if creating the directory or writing either unit file fails.
    pub fn install_to(&self, systemd_user_dir: &Path) -> Result<(PathBuf, PathBuf)> {
        fs::create_dir_all(systemd_user_dir).with_context(|| {
            format!(
                "failed to create systemd user directory {}",
                systemd_user_dir.display()
            )
        })?;

        let (service_body, timer_body) = self.render();
        let service_path = systemd_user_dir.join(SYSTEMD_SERVICE_NAME);
        let timer_path = systemd_user_dir.join(SYSTEMD_TIMER_NAME);

        fs::write(&service_path, service_body)
            .with_context(|| format!("failed to write {}", service_path.display()))?;
        fs::write(&timer_path, timer_body)
            .with_context(|| format!("failed to write {}", timer_path.display()))?;

        Ok((service_path, timer_path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Custom probe interval in minutes used in unit rendering tests.
    const TEST_INTERVAL_MINUTES: u64 = 10;

    #[test]
    fn renders_and_installs_systemd_user_units() {
        let dir = TempDir::new().expect("temp dir");
        let units = SystemdUnits::new(
            PathBuf::from("/usr/local/bin/tayassu"),
            TEST_INTERVAL_MINUTES,
        );
        let (service, timer) = units.render();

        assert!(service.contains(r#"ExecStart="/usr/local/bin/tayassu" probe --notify"#));
        assert!(timer.contains("OnUnitActiveSec=10m"));

        let (svc_path, tmr_path) = units.install_to(dir.path()).expect("install units");
        assert!(svc_path.is_file());
        assert!(tmr_path.is_file());
    }
}

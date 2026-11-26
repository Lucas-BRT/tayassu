use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use tayassu_core::AccountConfig;
use tayassu_providers::{AdapterRegistry, FAKE_HOME_SUBDIR, IsolationStrategy};
use tayassu_store::XdgPaths;

/// Sensitive credential filenames checked for overly permissive Unix file modes.
const SENSITIVE_CRED_FILES: &[&str] = &[
    ".credentials.json",
    ".claude.json",
    "auth.json",
    "service.json",
    "hosts.json",
];

/// Mask matching group/other permission bits on Unix (`0o077`).
#[cfg(unix)]
const GROUP_OTHER_PERM_MASK: u32 = 0o077;

/// Severity of one diagnostic check produced by `tayassu doctor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckLevel {
    /// Check passed cleanly.
    Ok,
    /// Non-fatal warning worth reviewing.
    Warn,
    /// Broken configuration or missing dependency.
    Error,
}

/// One diagnostic line emitted by [`DoctorReport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckItem {
    /// Severity level.
    pub level: CheckLevel,
    /// Account or subsystem checked.
    pub subject: String,
    /// Human-readable finding.
    pub message: String,
}

/// Aggregated health diagnostics for all configured `tayassu` accounts and profiles.
#[derive(Debug, Default, Clone)]
pub struct DoctorReport {
    items: Vec<CheckItem>,
}

impl DoctorReport {
    /// Runs health diagnostics across `accounts` and `paths`.
    #[must_use]
    pub fn run(accounts: &[AccountConfig], paths: &XdgPaths) -> Self {
        let mut report = Self::default();
        let path_env = env::var_os("PATH");

        if accounts.is_empty() {
            report.push(
                CheckLevel::Warn,
                "config",
                "no accounts configured; run `tayassu import` or `tayassu account add`",
            );
            return report;
        }

        for account in accounts {
            let subject = account.id.to_string();
            let exe = account.executable();
            if executable_available(exe, &paths.home_dir, path_env.as_deref()) {
                report.push(CheckLevel::Ok, &subject, format!("binary '{exe}' found"));
            } else {
                report.push(
                    CheckLevel::Error,
                    &subject,
                    format!("binary '{exe}' not found in PATH"),
                );
            }

            let adapter = AdapterRegistry.adapter_for(account.provider);
            let env_profile = adapter.build_environment(account, &paths.profiles_dir);

            if !env_profile.profile_dir.is_dir() {
                report.push(
                    CheckLevel::Warn,
                    &subject,
                    format!(
                        "profile directory {} does not exist yet",
                        env_profile.profile_dir.display()
                    ),
                );
                continue;
            }

            if let IsolationStrategy::FakeHome { shared_links } = &env_profile.strategy {
                let fake_home = env_profile.profile_dir.join(FAKE_HOME_SUBDIR);
                for rel in shared_links {
                    let link_path = fake_home.join(rel);
                    if link_path.symlink_metadata().is_ok() && !link_path.exists() {
                        report.push(
                            CheckLevel::Error,
                            &subject,
                            format!("broken symlink in fake HOME: {}", link_path.display()),
                        );
                    }
                }
            }

            check_credential_permissions(&env_profile.profile_dir, &subject, &mut report);
        }

        report
    }

    /// Returns `true` if any [`CheckLevel::Error`] item was recorded.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.items
            .iter()
            .any(|item| item.level == CheckLevel::Error)
    }

    /// Returns `true` if any [`CheckLevel::Warn`] or [`CheckLevel::Error`] item was recorded.
    #[must_use]
    pub fn has_warnings_or_errors(&self) -> bool {
        self.items.iter().any(|item| item.level != CheckLevel::Ok)
    }

    /// Writes the formatted diagnostic report to `writer`.
    ///
    /// # Errors
    ///
    /// Returns any underlying I/O error from `writer`.
    pub fn write_to(&self, writer: &mut impl Write) -> io::Result<()> {
        for item in &self.items {
            let tag = match item.level {
                CheckLevel::Ok => "OK",
                CheckLevel::Warn => "WARN",
                CheckLevel::Error => "ERR",
            };
            writeln!(writer, "[{tag:<4}] {:<18} {}", item.subject, item.message)?;
        }
        Ok(())
    }

    fn push(&mut self, level: CheckLevel, subject: impl Into<String>, message: impl Into<String>) {
        self.items.push(CheckItem {
            level,
            subject: subject.into(),
            message: message.into(),
        });
    }
}

fn executable_available(exe: &str, home_dir: &Path, path_env: Option<&std::ffi::OsStr>) -> bool {
    let as_path = Path::new(exe);
    if as_path.components().count() > 1 {
        return as_path.is_file();
    }
    if let Some(raw_path) = path_env {
        for dir in env::split_paths(raw_path) {
            if dir.join(exe).is_file() {
                return true;
            }
        }
    }
    home_dir.join(".local/bin").join(exe).is_file()
        || home_dir.join(".opencode/bin").join(exe).is_file()
}

#[cfg(unix)]
fn check_credential_permissions(profile_dir: &Path, subject: &str, report: &mut DoctorReport) {
    use std::os::unix::fs::PermissionsExt;

    for cred in SENSITIVE_CRED_FILES {
        let candidate = profile_dir.join(cred);
        if let Ok(meta) = fs::metadata(&candidate)
            && meta.is_file()
            && (meta.permissions().mode() & GROUP_OTHER_PERM_MASK != 0)
        {
            report.push(
                CheckLevel::Warn,
                subject,
                format!(
                    "credential file {} is readable by group/others; consider `chmod 600`",
                    candidate.display()
                ),
            );
        }
    }
}

#[cfg(not(unix))]
fn check_credential_permissions(_profile_dir: &Path, _subject: &str, _report: &mut DoctorReport) {}

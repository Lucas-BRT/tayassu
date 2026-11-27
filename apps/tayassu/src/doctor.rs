use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use tayassu_core::AccountConfig;
use tayassu_providers::{AdapterRegistry, FAKE_HOME_SUBDIR, HostDiscovery, IsolationStrategy};
use tayassu_store::XdgPaths;

/// Sensitive credential filenames checked for overly permissive Unix file modes.
const SENSITIVE_CRED_FILES: &[&str] = &[
    ".credentials.json",
    ".claude.json",
    "auth.json",
    "service.json",
    "hosts.json",
];

/// Binaries required on Linux for private Antigravity keyrings.
#[cfg(target_os = "linux")]
const PRIVATE_KEYRING_BINARIES: &[&str] = &[
    "dbus-daemon",
    "gdbus",
    "gnome-keyring-daemon",
    "systemd-ask-password",
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
                "no accounts configured; run `tayassu import` or `tayassu add`",
            );
            return report;
        }

        for account in accounts {
            let subject = account.id.to_string();
            let exe = account.executable();
            if let Some(path) =
                HostDiscovery.resolve_executable(exe, &paths.home_dir, path_env.as_deref())
            {
                report.push(
                    CheckLevel::Ok,
                    &subject,
                    format!("binary '{exe}' found at {}", path.display()),
                );
            } else {
                report.push(
                    CheckLevel::Error,
                    &subject,
                    format!("binary '{exe}' not found or not executable"),
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

        check_private_keyring_deps(accounts, &paths.home_dir, path_env.as_deref(), &mut report);
        check_orphaned_profiles(accounts, &paths.profiles_dir, &mut report);

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

#[cfg(target_os = "linux")]
fn check_private_keyring_deps(
    accounts: &[AccountConfig],
    home_dir: &Path,
    path_env: Option<&std::ffi::OsStr>,
    report: &mut DoctorReport,
) {
    if !accounts.iter().any(AccountConfig::uses_private_agy_keyring) {
        return;
    }
    let has_runtime = env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .is_some_and(|dir| dir.is_absolute());
    if has_runtime {
        report.push(
            CheckLevel::Ok,
            "agy-keyring",
            "XDG_RUNTIME_DIR is configured for private keyrings",
        );
    } else {
        report.push(
            CheckLevel::Error,
            "agy-keyring",
            "XDG_RUNTIME_DIR must point to an absolute runtime directory for private agy keyrings",
        );
    }
    let missing: Vec<&str> = PRIVATE_KEYRING_BINARIES
        .iter()
        .copied()
        .filter(|bin| {
            HostDiscovery
                .resolve_executable(bin, home_dir, path_env)
                .is_none()
        })
        .collect();
    if missing.is_empty() {
        report.push(
            CheckLevel::Ok,
            "agy-keyring",
            "all private keyring dependencies found on PATH",
        );
    } else {
        report.push(
            CheckLevel::Error,
            "agy-keyring",
            format!("missing private keyring tool(s): {}", missing.join(", ")),
        );
    }
}

#[cfg(not(target_os = "linux"))]
fn check_private_keyring_deps(
    _accounts: &[AccountConfig],
    _home_dir: &Path,
    _path_env: Option<&std::ffi::OsStr>,
    _report: &mut DoctorReport,
) {
}

fn check_orphaned_profiles(
    accounts: &[AccountConfig],
    profiles_dir: &Path,
    report: &mut DoctorReport,
) {
    let Ok(provider_dirs) = fs::read_dir(profiles_dir) else {
        return;
    };
    let mut orphans = Vec::new();
    for provider_entry in provider_dirs.flatten() {
        let provider_path = provider_entry.path();
        if !provider_path.is_dir() {
            continue;
        }
        let provider_name = provider_entry.file_name();
        let Ok(account_dirs) = fs::read_dir(&provider_path) else {
            continue;
        };
        for account_entry in account_dirs.flatten() {
            let account_path = account_entry.path();
            if !account_path.is_dir() {
                continue;
            }
            let account_name = account_entry.file_name();
            let configured = accounts.iter().any(|account| {
                provider_name == account.provider.as_str() && account_name == account.id.as_str()
            });
            if !configured {
                orphans.push(account_path);
            }
        }
    }
    orphans.sort();
    for orphan in orphans {
        report.push(
            CheckLevel::Warn,
            "profiles",
            format!(
                "orphaned profile directory {} (not in config.toml)",
                orphan.display()
            ),
        );
    }
}

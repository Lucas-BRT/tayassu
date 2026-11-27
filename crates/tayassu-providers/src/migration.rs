use crate::error::ProviderError;
use crate::isolation::{FAKE_HOME_SUBDIR, IsolationStrategy, ProfileEnvironment};
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use tayassu_core::{AccountId, ProviderKind};

/// Extra standard user binary directories checked in addition to `PATH`.
const EXTRA_USER_BIN_DIRS: &[&str] = &[".local/bin", ".opencode/bin"];

/// Suffix appended to provider names when generating default imported account ids.
const IMPORTED_ACCOUNT_SUFFIX: &str = "host";

/// Built-in providers scanned during host agent discovery.
const DISCOVERABLE_PROVIDERS: &[ProviderKind] = &[
    ProviderKind::Agy,
    ProviderKind::Claude,
    ProviderKind::Codex,
    ProviderKind::Copilot,
    ProviderKind::Opencode,
];

/// Controls whether host credential files are copied or moved into isolated profiles.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MigrationMode {
    /// Copy credential/config files into the isolated profile, leaving host files untouched.
    #[default]
    Copy,
    /// Copy files into the isolated profile, then remove original auth-only host files.
    /// Shared binaries, updater directories, and settings files are never removed.
    MoveAuth,
}

/// One host file eligible for safe migration into an isolated profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationFile {
    /// Absolute path of the file on the host.
    pub source_path: PathBuf,
    /// Relative destination path inside the isolated profile root (or fake `HOME`).
    pub dest_rel: PathBuf,
    /// Whether `source_path` currently exists as a regular file.
    pub exists: bool,
    /// Whether removing `source_path` after a verified copy is safe in [`MigrationMode::MoveAuth`].
    pub removable_on_move: bool,
}

/// Result of executing a [`DiscoveredAgent`] migration into a profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrationSummary {
    /// Number of existing host files copied into the isolated profile.
    pub copied_files: usize,
    /// Number of original host auth files removed (only non-zero under [`MigrationMode::MoveAuth`]).
    pub removed_source_files: usize,
    /// Whether the user still needs to run `tayassu account login <id>` for this profile.
    pub requires_relogin: bool,
}

/// An installed AI CLI agent discovered on the host along with its update-safe migration plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredAgent {
    /// Discovered provider kind.
    pub provider: ProviderKind,
    /// Resolved executable path on the host.
    pub binary_path: PathBuf,
    /// Default account id assigned when importing this host agent.
    pub default_account_id: AccountId,
    /// Credential/config files considered for migration (never binaries or updater dirs).
    pub files: Vec<MigrationFile>,
    /// Human-readable explanation of how auto-updates remain safe for this provider.
    pub update_safety_note: &'static str,
    /// Whether an interactive `tayassu account login` is still required after import.
    pub requires_relogin: bool,
}

impl DiscoveredAgent {
    /// Returns the number of host migration files that currently exist on disk.
    #[must_use]
    pub fn existing_file_count(&self) -> usize {
        self.files.iter().filter(|file| file.exists).count()
    }

    /// Executes this migration plan into `profile_env`.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Io`] if profile preparation, file copy, or source removal fails.
    pub fn apply(
        &self,
        profile_env: &ProfileEnvironment,
        real_home: &Path,
        mode: MigrationMode,
    ) -> Result<MigrationSummary, ProviderError> {
        profile_env.prepare(real_home)?;

        let dest_root = match profile_env.strategy {
            IsolationStrategy::NativeEnvVar { .. } => profile_env.profile_dir.clone(),
            IsolationStrategy::FakeHome { .. } => profile_env.profile_dir.join(FAKE_HOME_SUBDIR),
        };

        let mut copied_files = 0usize;
        let mut removed_source_files = 0usize;

        for file in &self.files {
            if !file.exists {
                continue;
            }
            let dest_path = dest_root.join(&file.dest_rel);
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent).map_err(|source| ProviderError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            fs::copy(&file.source_path, &dest_path).map_err(|source| ProviderError::Io {
                path: dest_path.clone(),
                source,
            })?;
            copied_files += 1;

            if mode == MigrationMode::MoveAuth && file.removable_on_move && dest_path.is_file() {
                fs::remove_file(&file.source_path).map_err(|source| ProviderError::Io {
                    path: file.source_path.clone(),
                    source,
                })?;
                removed_source_files += 1;
            }
        }

        Ok(MigrationSummary {
            copied_files,
            removed_source_files,
            requires_relogin: self.requires_relogin,
        })
    }
}

/// Discovers installed AI CLI agents on the host and constructs update-safe migration plans.
#[derive(Debug, Default, Clone, Copy)]
pub struct HostDiscovery;

impl HostDiscovery {
    /// Scans `path_env` and standard user bin directories under `real_home` for known AI CLIs.
    #[must_use]
    pub fn discover_all(self, real_home: &Path, path_env: Option<&OsStr>) -> Vec<DiscoveredAgent> {
        let search_dirs = build_search_dirs(real_home, path_env);

        DISCOVERABLE_PROVIDERS
            .iter()
            .filter_map(|&provider| {
                let binary_path = find_executable(provider.default_binary(), &search_dirs)?;
                Some(build_discovered_agent(provider, binary_path, real_home))
            })
            .collect()
    }
}

fn build_search_dirs(real_home: &Path, path_env: Option<&OsStr>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(raw_path) = path_env {
        dirs.extend(env::split_paths(raw_path));
    }
    for rel in EXTRA_USER_BIN_DIRS {
        let candidate = real_home.join(rel);
        if !dirs.contains(&candidate) {
            dirs.push(candidate);
        }
    }
    dirs
}

fn find_executable(binary_name: &str, search_dirs: &[PathBuf]) -> Option<PathBuf> {
    for dir in search_dirs {
        let candidate = dir.join(binary_name);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).is_ok_and(|meta| meta.is_file() && (meta.permissions().mode() & 0o111 != 0))
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

fn build_discovered_agent(
    provider: ProviderKind,
    binary_path: PathBuf,
    real_home: &Path,
) -> DiscoveredAgent {
    let id_str = format!("{}-{IMPORTED_ACCOUNT_SUFFIX}", provider.as_str());
    let default_account_id: AccountId = id_str.parse().unwrap_or_else(|_| unreachable!());
    let files = host_files_for(provider, real_home);
    let requires_relogin = match provider {
        ProviderKind::Agy | ProviderKind::Custom => true,
        _ => !files
            .iter()
            .any(|file| file.removable_on_move && file.exists),
    };

    DiscoveredAgent {
        provider,
        binary_path,
        default_account_id,
        files,
        update_safety_note: update_safety_note_for(provider),
        requires_relogin,
    }
}

fn host_files_for(provider: ProviderKind, real_home: &Path) -> Vec<MigrationFile> {
    match provider {
        ProviderKind::Claude => vec![
            mig_file(
                real_home,
                ".claude/.credentials.json",
                ".credentials.json",
                true,
            ),
            mig_file(real_home, ".claude/settings.json", "settings.json", false),
            mig_file(real_home, ".claude.json", ".claude.json", true),
        ],
        ProviderKind::Codex => vec![
            mig_file(real_home, ".codex/auth.json", "auth.json", true),
            mig_file(real_home, ".codex/config.toml", "config.toml", false),
        ],
        ProviderKind::Copilot => vec![
            mig_file(real_home, ".copilot/config.json", "config.json", true),
            mig_file(real_home, ".copilot/hosts.json", "hosts.json", true),
            mig_file(
                real_home,
                ".copilot/mcp-config.json",
                "mcp-config.json",
                false,
            ),
        ],
        ProviderKind::Agy => vec![mig_file(
            real_home,
            ".gemini/antigravity-cli/settings.json",
            ".gemini/antigravity-cli/settings.json",
            false,
        )],
        ProviderKind::Opencode => vec![
            mig_file(
                real_home,
                ".config/opencode/opencode.json",
                ".config/opencode/opencode.json",
                false,
            ),
            mig_file(
                real_home,
                ".config/opencode/service.json",
                ".config/opencode/service.json",
                false,
            ),
            mig_file(
                real_home,
                ".local/share/opencode/auth.json",
                ".local/share/opencode/auth.json",
                true,
            ),
        ],
        ProviderKind::Custom => Vec::new(),
    }
}

const fn update_safety_note_for(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Claude => {
            "Uses CLAUDE_CONFIG_DIR; ~/.local/share/claude/versions and auto-updater stay untouched"
        }
        ProviderKind::Codex => {
            "Uses CODEX_HOME; ~/.codex/packages/standalone binary and auto-updater stay untouched"
        }
        ProviderKind::Copilot => {
            "Uses COPILOT_HOME; ~/.local/bin/copilot and auto-updater stay untouched"
        }
        ProviderKind::Agy => {
            "Uses fake HOME with ~/.local/bin symlinked so agy update updates the shared host binary; run tayassu account login to authenticate"
        }
        ProviderKind::Opencode => {
            "Uses fake HOME with ~/.opencode/bin symlinked so opencode upgrade updates the shared host binary"
        }
        ProviderKind::Custom => "Custom provider; manage updates and credentials manually",
    }
}

fn mig_file(
    real_home: &Path,
    source_rel: &str,
    dest_rel: &str,
    removable_on_move: bool,
) -> MigrationFile {
    let source_path = real_home.join(source_rel);
    let exists = source_path.is_file();
    MigrationFile {
        source_path,
        dest_rel: PathBuf::from(dest_rel),
        exists,
        removable_on_move,
    }
}

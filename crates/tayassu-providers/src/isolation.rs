use crate::error::ProviderError;
use std::fs;
use std::path::{Path, PathBuf};

/// Subdirectory created inside a profile directory when isolating via fake `HOME`.
pub const FAKE_HOME_SUBDIR: &str = "home";

/// Default dotfile entries symlinked from the real `$HOME` into a fake `HOME` profile.
pub const DEFAULT_SHARED_HOME_LINKS: &[&str] = &[".gitconfig", ".ssh"];

/// Config file (relative to the fake `HOME`) where `opencode` stores its managed-service port.
pub const OPENCODE_SERVICE_CONFIG: &str = ".config/opencode/service.json";

/// JSON key holding the managed-service port inside [`OPENCODE_SERVICE_CONFIG`].
const SERVICE_PORT_KEY: &str = "port";

/// Environment variable name for the user home directory.
pub const ENV_HOME: &str = "HOME";

/// Environment variable name for the XDG config directory.
pub const ENV_XDG_CONFIG_HOME: &str = "XDG_CONFIG_HOME";

/// Environment variable name for the XDG data directory.
pub const ENV_XDG_DATA_HOME: &str = "XDG_DATA_HOME";

/// Environment variable name for the XDG state directory.
pub const ENV_XDG_STATE_HOME: &str = "XDG_STATE_HOME";

/// Environment variable name for the XDG cache directory.
pub const ENV_XDG_CACHE_HOME: &str = "XDG_CACHE_HOME";

/// How a provider isolates one account's credentials and session state on Linux.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IsolationStrategy {
    /// Provider natively supports a dedicated config/home environment variable.
    NativeEnvVar {
        /// Environment variable set to the profile directory.
        var_name: &'static str,
    },
    /// Provider reads credentials from `$HOME`; isolate via a dedicated fake `HOME` directory.
    FakeHome {
        /// Relative paths under the real `$HOME` to symlink into the fake `HOME`.
        shared_links: Vec<String>,
    },
}

/// Concrete filesystem path and environment overrides prepared for one account run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileEnvironment {
    /// Root directory owned by this account profile.
    pub profile_dir: PathBuf,
    /// Strategy used to isolate this profile.
    pub strategy: IsolationStrategy,
    /// Environment key/value overrides injected into the spawned CLI process.
    pub env_vars: Vec<(String, String)>,
    /// Dedicated managed-service port written into the fake `HOME` when the CLI binds a fixed port.
    pub service_port: Option<u16>,
}

impl ProfileEnvironment {
    /// Builds a [`ProfileEnvironment`] for `profile_dir` and `strategy`.
    #[must_use]
    pub fn new(profile_dir: PathBuf, strategy: IsolationStrategy) -> Self {
        let env_vars = match &strategy {
            IsolationStrategy::NativeEnvVar { var_name } => {
                vec![((*var_name).to_owned(), profile_dir.display().to_string())]
            }
            IsolationStrategy::FakeHome { .. } => {
                let fake_home = profile_dir.join(FAKE_HOME_SUBDIR);
                vec![
                    (ENV_HOME.to_owned(), fake_home.display().to_string()),
                    (
                        ENV_XDG_CONFIG_HOME.to_owned(),
                        fake_home.join(".config").display().to_string(),
                    ),
                    (
                        ENV_XDG_DATA_HOME.to_owned(),
                        fake_home.join(".local/share").display().to_string(),
                    ),
                    (
                        ENV_XDG_STATE_HOME.to_owned(),
                        fake_home.join(".local/state").display().to_string(),
                    ),
                    (
                        ENV_XDG_CACHE_HOME.to_owned(),
                        fake_home.join(".cache").display().to_string(),
                    ),
                ]
            }
        };

        Self {
            profile_dir,
            strategy,
            env_vars,
            service_port: None,
        }
    }

    /// Sets the managed-service port persisted into the fake `HOME` by [`Self::prepare`].
    #[must_use]
    pub fn with_service_port(mut self, port: u16) -> Self {
        self.service_port = Some(port);
        self
    }

    /// Ensures the profile directory exists and creates any configured symlinks for fake `HOME`.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Io`] if directory or symlink creation fails.
    pub fn prepare(&self, real_home: &Path) -> Result<(), ProviderError> {
        fs::create_dir_all(&self.profile_dir).map_err(|source| ProviderError::Io {
            path: self.profile_dir.clone(),
            source,
        })?;

        if let IsolationStrategy::FakeHome { shared_links } = &self.strategy {
            let fake_home = self.profile_dir.join(FAKE_HOME_SUBDIR);
            fs::create_dir_all(&fake_home).map_err(|source| ProviderError::Io {
                path: fake_home.clone(),
                source,
            })?;

            for relative in shared_links {
                let source_path = real_home.join(relative);
                if !source_path.exists() {
                    continue;
                }
                let link_path = fake_home.join(relative);
                if link_path.exists() || link_path.symlink_metadata().is_ok() {
                    continue;
                }
                if let Some(parent) = link_path.parent() {
                    fs::create_dir_all(parent).map_err(|source| ProviderError::Io {
                        path: parent.to_path_buf(),
                        source,
                    })?;
                }
                create_symlink(&source_path, &link_path)?;
            }

            if let Some(port) = self.service_port {
                write_service_port(&fake_home.join(OPENCODE_SERVICE_CONFIG), port)?;
            }
        }

        Ok(())
    }
}

/// Writes `port` into the service config at `path`, keeping any existing port or other keys.
fn write_service_port(path: &Path, port: u16) -> Result<(), ProviderError> {
    let io_err = |source| ProviderError::Io {
        path: path.to_path_buf(),
        source,
    };
    let mut config: serde_json::Map<String, serde_json::Value> = match fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw).map_err(|err| io_err(err.into()))?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
        Err(err) => return Err(io_err(err)),
    };
    if config.contains_key(SERVICE_PORT_KEY) {
        return Ok(());
    }
    config.insert(SERVICE_PORT_KEY.to_owned(), port.into());
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_err)?;
    }
    let body = serde_json::to_string_pretty(&config).map_err(|e| io_err(e.into()))?;
    fs::write(path, body).map_err(io_err)
}

#[cfg(unix)]
fn create_symlink(source: &Path, link: &Path) -> Result<(), ProviderError> {
    std::os::unix::fs::symlink(source, link).map_err(|err| ProviderError::Io {
        path: link.to_path_buf(),
        source: err,
    })
}

#[cfg(not(unix))]
fn create_symlink(_source: &Path, _link: &Path) -> Result<(), ProviderError> {
    Ok(())
}

#[cfg(test)]
mod service_config_tests {
    use super::*;

    /// Managed port used in configuration fixtures.
    const TEST_PORT: u16 = 50_000;

    #[test]
    fn invalid_service_config_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("service.json");
        for invalid in ["{", "[]", "null"] {
            fs::write(&path, invalid).unwrap();
            assert!(write_service_port(&path, TEST_PORT).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), invalid);
        }
    }

    #[test]
    fn adding_port_preserves_existing_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("service.json");
        fs::write(&path, r#"{"enabled":false}"#).unwrap();
        write_service_port(&path, TEST_PORT).unwrap();
        let saved: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(
            saved,
            serde_json::json!({"enabled": false, "port": TEST_PORT})
        );
    }
}

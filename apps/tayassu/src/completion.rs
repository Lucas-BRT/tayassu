//! Dynamic shell completion candidates sourced from the user's `config.toml`.

use clap_complete::engine::CompletionCandidate;
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tayassu_core::SelectionPolicy;
use tayassu_store::{AppConfig, XdgPaths};

/// Loads the config, falling back to an empty one so completion never errors.
fn load_config() -> AppConfig {
    XdgPaths::from_env()
        .ok()
        .and_then(|paths| AppConfig::load(&paths.config_file).ok())
        .unwrap_or_default()
}

/// Providers that have at least one configured account.
pub fn configured_providers() -> Vec<CompletionCandidate> {
    load_config()
        .accounts
        .iter()
        .map(|account| account.provider.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(CompletionCandidate::new)
        .collect()
}

/// Ids of all configured accounts.
pub fn account_ids() -> Vec<CompletionCandidate> {
    load_config()
        .accounts
        .iter()
        .map(|account| CompletionCandidate::new(account.id.as_str()))
        .collect()
}

/// Names of configured fallback chains.
pub fn chain_names() -> Vec<CompletionCandidate> {
    load_config()
        .fallback_chains
        .keys()
        .map(CompletionCandidate::new)
        .collect()
}

/// All selection policy names.
pub fn policies() -> Vec<CompletionCandidate> {
    [
        SelectionPolicy::Priority,
        SelectionPolicy::MostQuota,
        SelectionPolicy::SoonestReset,
    ]
    .into_iter()
    .map(|policy| CompletionCandidate::new(policy.as_str()))
    .collect()
}

/// Marker line that makes rc-file edits idempotent.
const RC_MARKER: &str = "# tayassu shell completion";

/// Registers dynamic completion for bash and zsh via a guarded rc-file block.
const RC_BLOCK_TEMPLATE: &str =
    "command -v tayassu >/dev/null 2>&1 && source <(COMPLETE={shell} tayassu)";

/// Fish autoloads completion scripts from this directory under `$HOME`.
const FISH_COMPLETIONS_REL_DIR: &str = ".config/fish/completions";

/// Fish completion registration script.
const FISH_SCRIPT: &str = "COMPLETE=fish tayassu | source\n";

/// Installs shell completion registration for the user's shells under a home directory.
pub struct CompletionInstaller {
    home: PathBuf,
}

impl CompletionInstaller {
    /// Creates an installer rooted at `home`.
    pub fn new(home: PathBuf) -> Self {
        Self { home }
    }

    /// Configures every detected shell; returns human-readable lines describing what changed.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when an rc or completion file cannot be written.
    pub fn install(&self) -> io::Result<Vec<String>> {
        let mut report = Vec::new();
        for (shell, rc) in [("bash", ".bashrc"), ("zsh", ".zshrc")] {
            let path = self.home.join(rc);
            if path.exists() && Self::append_block(&path, shell)? {
                report.push(format!("{shell}: added completion to {}", path.display()));
            }
        }
        let fish_dir = self.home.join(FISH_COMPLETIONS_REL_DIR);
        if self.home.join(".config/fish").is_dir() {
            fs::create_dir_all(&fish_dir)?;
            let path = fish_dir.join("tayassu.fish");
            if fs::read_to_string(&path).ok().as_deref() != Some(FISH_SCRIPT) {
                fs::write(&path, FISH_SCRIPT)?;
                report.push(format!("fish: wrote {}", path.display()));
            }
        }
        Ok(report)
    }

    /// Appends the marked block unless already present; returns whether the file changed.
    fn append_block(path: &Path, shell: &str) -> io::Result<bool> {
        let existing = fs::read_to_string(path)?;
        if existing.contains(RC_MARKER) {
            return Ok(false);
        }
        let block = RC_BLOCK_TEMPLATE.replace("{shell}", shell);
        let sep = if existing.ends_with('\n') || existing.is_empty() {
            ""
        } else {
            "\n"
        };
        let mut file = fs::OpenOptions::new().append(true).open(path)?;
        write!(file, "{sep}\n{RC_MARKER}\n{block}\n")?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_once_for_detected_shells() {
        let home = tempfile::tempdir().expect("tempdir");
        fs::write(home.path().join(".bashrc"), "export A=1").expect("write");
        fs::create_dir_all(home.path().join(".config/fish")).expect("mkdir");
        let installer = CompletionInstaller::new(home.path().to_path_buf());

        let first = installer.install().expect("install");
        assert_eq!(first.len(), 2);
        let bashrc = fs::read_to_string(home.path().join(".bashrc")).expect("read");
        assert!(bashrc.contains("COMPLETE=bash tayassu"));
        assert!(!home.path().join(".zshrc").exists());
        assert!(
            home.path()
                .join(".config/fish/completions/tayassu.fish")
                .exists()
        );

        let again = installer.install().expect("reinstall");
        assert_eq!(again.len(), 0);
    }
}

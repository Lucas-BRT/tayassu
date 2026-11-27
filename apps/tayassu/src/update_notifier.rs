//! Asynchronous background release check and once-per-day terminal notice.

use crate::commands::Command;
use crate::release::{ReleaseSource, current_version};
use anyhow::{Context, Result};
use chrono::{DateTime, TimeDelta, Utc};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use tayassu_store::{AppConfig, write_atomic};

/// Cache filename inside `$XDG_STATE_HOME/tayassu/`.
const CACHE_FILE_NAME: &str = "update-check.json";

/// Minimum hours between background release fetches and between terminal notices.
const CHECK_INTERVAL_HOURS: i64 = 24;

/// Environment variable disabling background release checks and notices.
const NO_UPDATE_CHECK_ENV: &str = "TAYASSU_NO_UPDATE_CHECK";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UpdateCheckCache {
    checked_at: DateTime<Utc>,
    latest_tag: String,
    latest_version: String,
    #[serde(default)]
    last_notified_at: Option<DateTime<Utc>>,
    #[serde(default)]
    last_notified_version: Option<String>,
}

/// Coordinates background release checks and terminal update notices.
pub struct UpdateNotifier;

impl UpdateNotifier {
    /// Returns `true` if update checks/notices must stay silent for this invocation.
    #[must_use]
    pub fn should_skip(command: Option<&Command>, config: &AppConfig) -> bool {
        if !config.update_check {
            return true;
        }
        if env::var_os(NO_UPDATE_CHECK_ENV).is_some_and(|v| v == "1" || v == "true") {
            return true;
        }
        if env::var_os("COMPLETE").is_some() {
            return true;
        }
        matches!(
            command,
            Some(
                Command::Run(_)
                    | Command::Activate(_)
                    | Command::Update(_)
                    | Command::Uninstall(_)
                    | Command::CheckUpdate
            )
        )
    }

    /// Spawns a detached `tayassu __check-update` child when the cache is missing or older than 24h.
    pub fn maybe_spawn_background(state_file: &Path, now: DateTime<Utc>) {
        let Some(cache_path) = cache_path_from_state(state_file) else {
            return;
        };
        if let Some(cache) = load_cache(&cache_path)
            && now.signed_duration_since(cache.checked_at) < TimeDelta::hours(CHECK_INTERVAL_HOURS)
        {
            return;
        }
        let Ok(exe) = env::current_exe() else {
            return;
        };
        let _ = ProcessCommand::new(exe)
            .arg("__check-update")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }

    /// Prints a one-line notice to `stderr` if `is_tty`, a newer stable version is cached, and we
    /// have not already notified for that version within 24h.
    pub fn maybe_notify(
        state_file: &Path,
        now: DateTime<Utc>,
        is_tty: bool,
        stderr: &mut dyn Write,
    ) {
        if !is_tty {
            return;
        }
        let Some(cache_path) = cache_path_from_state(state_file) else {
            return;
        };
        let Some(mut cache) = load_cache(&cache_path) else {
            return;
        };
        let Ok(current) = current_version() else {
            return;
        };
        let Ok(latest) = Version::parse(&cache.latest_version) else {
            return;
        };
        if latest <= current {
            return;
        }
        let recently_notified = cache.last_notified_version.as_deref()
            == Some(cache.latest_version.as_str())
            && cache.last_notified_at.is_some_and(|at| {
                now.signed_duration_since(at) < TimeDelta::hours(CHECK_INTERVAL_HOURS)
            });
        if recently_notified {
            return;
        }

        let _ = writeln!(
            stderr,
            "tayassu {latest} available (you have {current}). Run: tayassu update"
        );
        cache.last_notified_at = Some(now);
        cache.last_notified_version = Some(cache.latest_version.clone());
        let _ = save_cache(&cache_path, &cache);
    }

    /// Queries `source` for the latest stable release and updates the cache file.
    ///
    /// # Errors
    ///
    /// Returns an error if the source fails or the cache cannot be written.
    pub fn refresh_cache(
        source: &dyn ReleaseSource,
        state_file: &Path,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let cache_path = cache_path_from_state(state_file)
            .context("cannot resolve state directory for update cache")?;
        let release = source.latest(false)?;
        let existing = load_cache(&cache_path);
        let cache = UpdateCheckCache {
            checked_at: now,
            latest_tag: release.tag,
            latest_version: release.version.to_string(),
            last_notified_at: existing.as_ref().and_then(|c| c.last_notified_at),
            last_notified_version: existing.and_then(|c| c.last_notified_version),
        };
        save_cache(&cache_path, &cache)
    }
}

fn cache_path_from_state(state_file: &Path) -> Option<PathBuf> {
    state_file.parent().map(|dir| dir.join(CACHE_FILE_NAME))
}

fn load_cache(path: &Path) -> Option<UpdateCheckCache> {
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn save_cache(path: &Path, cache: &UpdateCheckCache) -> Result<()> {
    let json = serde_json::to_string_pretty(cache).context("failed to serialize update cache")?;
    write_atomic(path, json.as_bytes()).context("failed to write update cache")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::release::Release;

    struct FakeSource(Release);

    impl ReleaseSource for FakeSource {
        fn fetch_releases(&self) -> Result<Vec<Release>> {
            Ok(vec![self.0.clone()])
        }
        fn download(&self, _url: &str) -> Result<Vec<u8>> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn refresh_and_notify_throttles_to_once_per_day_on_tty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_file = dir.path().join("state.json");
        let now = Utc::now();
        let source = FakeSource(Release {
            tag: "v99.0.0".to_owned(),
            version: Version::new(99, 0, 0),
            prerelease: false,
            assets: Vec::new(),
        });

        UpdateNotifier::refresh_cache(&source, &state_file, now).expect("refresh");

        let mut non_tty = Vec::new();
        UpdateNotifier::maybe_notify(&state_file, now, false, &mut non_tty);
        assert_eq!(non_tty.len(), 0);

        let mut first = Vec::new();
        UpdateNotifier::maybe_notify(&state_file, now, true, &mut first);
        let msg = String::from_utf8(first).expect("utf8");
        assert!(msg.contains("99.0.0 available"));

        let mut second = Vec::new();
        UpdateNotifier::maybe_notify(&state_file, now + TimeDelta::hours(1), true, &mut second);
        assert_eq!(second.len(), 0);

        let mut next_day = Vec::new();
        UpdateNotifier::maybe_notify(
            &state_file,
            now + TimeDelta::hours(CHECK_INTERVAL_HOURS + 1),
            true,
            &mut next_day,
        );
        assert_ne!(next_day.len(), 0);
    }
}

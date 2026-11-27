//! GitHub release fetching, SHA-256 verification, and atomic binary replacement.

use anyhow::{Context, Result, bail};
use flate2::read::GzDecoder;
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::Duration;
use tar::Archive;

/// GitHub repository slug (`owner/repo`).
const GITHUB_REPO: &str = "Lucas-BRT/tayassu";

/// Binary filename inside release archives.
pub const BIN_NAME: &str = "tayassu";

/// Checksum manifest filename uploaded with every release.
pub const CHECKSUMS_ASSET_NAME: &str = "SHA256SUMS";

/// HTTP request timeout for release metadata and asset downloads.
const HTTP_TIMEOUT_SECS: u64 = 10;

/// Metadata for a single release asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseAsset {
    /// Asset filename (e.g. `tayassu-x86_64-unknown-linux-gnu.tar.gz`).
    pub name: String,
    /// Direct download URL.
    pub download_url: String,
}

/// A parsed release entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Git tag (e.g. `v0.2.0`).
    pub tag: String,
    /// Parsed semver version (without leading `v`).
    pub version: Version,
    /// Whether GitHub marked this release as a pre-release.
    pub prerelease: bool,
    /// Attached release assets.
    pub assets: Vec<ReleaseAsset>,
}

/// Abstraction over release registries so update logic is testable offline.
pub trait ReleaseSource {
    /// Lists releases sorted or unsorted; callers pick by semver.
    ///
    /// # Errors
    ///
    /// Returns an error if the registry cannot be reached or returns invalid JSON.
    fn fetch_releases(&self) -> Result<Vec<Release>>;

    /// Downloads raw bytes from `url`.
    ///
    /// # Errors
    ///
    /// Returns an error on network failure or non-success HTTP status.
    fn download(&self, url: &str) -> Result<Vec<u8>>;

    /// Returns the highest-version release, filtering out pre-releases unless `include_pre` is set.
    ///
    /// # Errors
    ///
    /// Returns an error if no matching release is available.
    fn latest(&self, include_pre: bool) -> Result<Release> {
        self.fetch_releases()?
            .into_iter()
            .filter(|rel| include_pre || (!rel.prerelease && rel.version.pre.is_empty()))
            .max_by(|a, b| a.version.cmp(&b.version))
            .context("no matching release found")
    }

    /// Finds a release matching `tag` (with or without leading `v`).
    ///
    /// # Errors
    ///
    /// Returns an error if no release matches `tag`.
    fn by_tag(&self, tag: &str) -> Result<Release> {
        let normalized = tag.strip_prefix('v').unwrap_or(tag);
        self.fetch_releases()?
            .into_iter()
            .find(|rel| rel.tag == tag || rel.tag.strip_prefix('v') == Some(normalized))
            .with_context(|| format!("release '{tag}' not found"))
    }
}

/// GitHub Releases API client backed by `ureq`.
pub struct GitHubReleases {
    agent: ureq::Agent,
}

impl GitHubReleases {
    /// Creates a new GitHub Releases client with a bounded timeout.
    #[must_use]
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(HTTP_TIMEOUT_SECS)))
            .build();
        Self {
            agent: config.into(),
        }
    }
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    prerelease: bool,
    draft: bool,
    assets: Vec<GhAsset>,
}

impl ReleaseSource for GitHubReleases {
    fn fetch_releases(&self) -> Result<Vec<Release>> {
        let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases?per_page=20");
        let user_agent = format!("{BIN_NAME}/{}", env!("CARGO_PKG_VERSION"));
        let mut resp = self
            .agent
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", &user_agent)
            .call()
            .context("failed to query GitHub releases")?;
        let raw_json = resp
            .body_mut()
            .read_to_string()
            .context("failed to read GitHub releases body")?;
        let raw: Vec<GhRelease> =
            serde_json::from_str(&raw_json).context("failed to parse GitHub releases JSON")?;
        let mut parsed = Vec::new();
        for item in raw {
            if item.draft {
                continue;
            }
            let ver_str = item.tag_name.strip_prefix('v').unwrap_or(&item.tag_name);
            let Ok(version) = Version::parse(ver_str) else {
                continue;
            };
            parsed.push(Release {
                tag: item.tag_name,
                version,
                prerelease: item.prerelease,
                assets: item
                    .assets
                    .into_iter()
                    .map(|asset| ReleaseAsset {
                        name: asset.name,
                        download_url: asset.browser_download_url,
                    })
                    .collect(),
            });
        }
        Ok(parsed)
    }

    fn download(&self, url: &str) -> Result<Vec<u8>> {
        let user_agent = format!("{BIN_NAME}/{}", env!("CARGO_PKG_VERSION"));
        let mut resp = self
            .agent
            .get(url)
            .header("User-Agent", &user_agent)
            .call()
            .with_context(|| format!("failed to download {url}"))?;
        let mut bytes = Vec::new();
        resp.body_mut()
            .as_reader()
            .read_to_end(&mut bytes)
            .with_context(|| format!("failed to read body from {url}"))?;
        Ok(bytes)
    }
}

/// Returns the target triple matching this build.
///
/// # Errors
///
/// Returns an error on unsupported OS or CPU architectures.
pub fn current_target() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        (os, arch) => bail!("unsupported platform: {os}/{arch}"),
    }
}

/// Expected release archive filename for `target`.
#[must_use]
pub fn archive_name(target: &str) -> String {
    format!("{BIN_NAME}-{target}.tar.gz")
}

/// Parses the currently compiled crate version as semver.
///
/// # Errors
///
/// Returns an error if `CARGO_PKG_VERSION` is not valid semver.
pub fn current_version() -> Result<Version> {
    Version::parse(env!("CARGO_PKG_VERSION")).context("invalid compiled package version")
}

/// Returns `true` when `exe` lives inside a Cargo bin directory (`~/.cargo/bin`).
#[must_use]
pub fn is_cargo_install(exe: &Path) -> bool {
    exe.to_string_lossy().contains("/.cargo/bin/")
}

/// Verifies `bytes` against the SHA-256 entry for `filename` inside `sums_txt`.
///
/// # Errors
///
/// Returns an error if `filename` is missing from `sums_txt` or the digest mismatches.
pub fn verify_sha256(bytes: &[u8], sums_txt: &str, filename: &str) -> Result<()> {
    let expected = sums_txt
        .lines()
        .find_map(|line| {
            let mut parts = line.split_whitespace();
            let hash = parts.next()?;
            let name = parts.next()?.trim_start_matches('*');
            (name == filename).then(|| hash.to_ascii_lowercase())
        })
        .with_context(|| {
            format!("checksum for '{filename}' not found in {CHECKSUMS_ASSET_NAME}")
        })?;

    let digest = Sha256::digest(bytes);
    let actual = digest.iter().fold(String::with_capacity(64), |mut acc, b| {
        use std::fmt::Write as _;
        let _ = write!(acc, "{b:02x}");
        acc
    });
    if actual != expected {
        bail!("checksum mismatch for '{filename}': expected {expected}, got {actual}");
    }
    Ok(())
}

/// Extracts `bin_name` from a `.tar.gz` archive held in memory.
///
/// # Errors
///
/// Returns an error if the archive is corrupt or does not contain `bin_name`.
pub fn extract_binary(archive_bytes: &[u8], bin_name: &str) -> Result<Vec<u8>> {
    let gz = GzDecoder::new(archive_bytes);
    let mut archive = Archive::new(gz);
    for entry in archive.entries().context("failed to read tar entries")? {
        let mut entry = entry.context("corrupt tar entry")?;
        let path = entry.path().context("invalid tar entry path")?;
        if path.file_name().and_then(|n| n.to_str()) == Some(bin_name) {
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .context("failed to extract binary from archive")?;
            return Ok(buf);
        }
    }
    bail!("binary '{bin_name}' not found inside archive")
}

/// Atomically replaces `target` by writing a sibling temp file and renaming over it.
///
/// # Errors
///
/// Returns an error if the directory is not writable or permissions/rename fail.
pub fn replace_binary_atomically(target: &Path, new_bytes: &[u8]) -> Result<()> {
    let parent = target
        .parent()
        .with_context(|| format!("cannot determine parent directory of {}", target.display()))?;
    let tmp = parent.join(format!(".{BIN_NAME}.update.tmp.{}", std::process::id()));
    fs::write(&tmp, new_bytes)
        .with_context(|| format!("failed to write temporary binary at {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o755);
        fs::set_permissions(&tmp, perms)
            .with_context(|| format!("failed to set executable mode on {}", tmp.display()))?;
    }
    fs::rename(&tmp, target).with_context(|| {
        let _ = fs::remove_file(&tmp);
        format!("failed to replace {}", target.display())
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;

    struct FakeSource(Vec<Release>);

    impl ReleaseSource for FakeSource {
        fn fetch_releases(&self) -> Result<Vec<Release>> {
            Ok(self.0.clone())
        }
        fn download(&self, _url: &str) -> Result<Vec<u8>> {
            Ok(Vec::new())
        }
    }

    fn rel(tag: &str, prerelease: bool) -> Release {
        Release {
            tag: tag.to_owned(),
            version: Version::parse(tag.strip_prefix('v').unwrap_or(tag)).expect("ver"),
            prerelease,
            assets: Vec::new(),
        }
    }

    #[test]
    fn latest_respects_prerelease_flag() {
        let src = FakeSource(vec![
            rel("v0.1.0", false),
            rel("v0.2.0", false),
            rel("v0.3.0-rc.1", true),
        ]);
        assert_eq!(src.latest(false).expect("stable").tag, "v0.2.0");
        assert_eq!(src.latest(true).expect("pre").tag, "v0.3.0-rc.1");
        assert_eq!(src.by_tag("0.1.0").expect("by_tag").tag, "v0.1.0");
    }

    #[test]
    fn sha256_verification_accepts_match_and_rejects_mismatch() {
        let payload = b"hello tayassu";
        let digest = Sha256::digest(payload);
        let hex = digest.iter().fold(String::new(), |mut acc, b| {
            use std::fmt::Write as _;
            let _ = write!(acc, "{b:02x}");
            acc
        });
        let sums = format!("{hex}  tayassu-x86_64-unknown-linux-gnu.tar.gz\n");
        verify_sha256(payload, &sums, "tayassu-x86_64-unknown-linux-gnu.tar.gz").expect("ok");
        assert!(
            verify_sha256(
                b"tampered",
                &sums,
                "tayassu-x86_64-unknown-linux-gnu.tar.gz"
            )
            .is_err()
        );
        assert!(verify_sha256(payload, &sums, "missing.tar.gz").is_err());
    }

    #[test]
    fn extracts_binary_from_tar_gz_and_replaces_atomically() {
        let mut tar_buf = Vec::new();
        {
            let gz = GzEncoder::new(&mut tar_buf, Compression::default());
            let mut builder = tar::Builder::new(gz);
            let data = b"#!/bin/sh\necho new\n";
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, "tayassu", &data[..])
                .expect("append");
            builder.into_inner().expect("gz").finish().expect("finish");
        }
        let extracted = extract_binary(&tar_buf, "tayassu").expect("extract");
        assert_eq!(extracted, b"#!/bin/sh\necho new\n");

        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("tayassu");
        fs::write(&target, b"old").expect("write old");
        replace_binary_atomically(&target, &extracted).expect("replace");
        assert_eq!(fs::read(&target).expect("read"), extracted);
    }
}

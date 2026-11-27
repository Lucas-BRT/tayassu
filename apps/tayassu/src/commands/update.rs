use crate::app::{App, Execute};
use crate::artifacts::{ManagedArtifact, ShellActivation, SystemdTimer};
use crate::release::{
    BIN_NAME, CHECKSUMS_ASSET_NAME, GitHubReleases, Release, ReleaseSource, archive_name,
    asset_checksum_name, current_target, current_version, extract_binary, is_cargo_install,
    replace_binary_atomically, verify_sha256,
};
use crate::systemd::{DEFAULT_PROBE_INTERVAL_MINUTES, SystemdUnits};
use anyhow::{Context, Result, bail};
use clap::Args;
use std::env;
use std::io::Write;
use std::path::Path;

/// Update tayassu itself to the latest release (with mandatory SHA-256 verification).
#[derive(Debug, Args)]
pub struct Update {
    /// Check if a newer release is available without downloading or replacing the binary.
    #[arg(long)]
    check: bool,
    /// Install a specific release tag (e.g. `v0.2.0`).
    #[arg(long)]
    version: Option<String>,
    /// Include pre-release versions when resolving the latest release.
    #[arg(long)]
    pre: bool,
    /// Print what would be updated without modifying the binary.
    #[arg(long)]
    dry_run: bool,
}

impl Execute for Update {
    fn execute(self, app: &mut App, out: &mut dyn Write) -> Result<()> {
        let exe = env::current_exe().context("failed to resolve current tayassu binary")?;
        let source = GitHubReleases::new();
        self.run_with(&source, &exe, app, out)
    }
}

impl Update {
    fn run_with(
        &self,
        source: &dyn ReleaseSource,
        exe: &Path,
        app: &App,
        out: &mut dyn Write,
    ) -> Result<()> {
        let current = current_version()?;
        let release = self.resolve_release(source)?;

        if self.check {
            if release.version > current {
                writeln!(
                    out,
                    "Update available: {} (current: v{current})",
                    release.tag
                )?;
            } else {
                writeln!(out, "tayassu v{current} is up to date.")?;
            }
            return Ok(());
        }

        if self.version.is_none() && release.version <= current {
            writeln!(out, "tayassu v{current} is already up to date.")?;
            return Ok(());
        }

        if is_cargo_install(exe) && !self.dry_run {
            bail!(
                "binary at {} looks managed by Cargo; update with `cargo install --locked --git https://github.com/Lucas-BRT/tayassu`",
                exe.display()
            );
        }

        let target = current_target()?;
        let asset_name = archive_name(target);
        let archive_asset = release
            .assets
            .iter()
            .find(|a| a.name == asset_name)
            .with_context(|| format!("release {} has no asset '{asset_name}'", release.tag))?;
        let per_asset_sums = asset_checksum_name(&asset_name);
        let sums_asset = release
            .assets
            .iter()
            .find(|a| a.name == per_asset_sums || a.name == CHECKSUMS_ASSET_NAME)
            .with_context(|| {
                format!(
                    "release {} is missing '{per_asset_sums}' / '{CHECKSUMS_ASSET_NAME}'; refusing unverified update",
                    release.tag
                )
            })?;

        if self.dry_run {
            writeln!(
                out,
                "[dry-run] would update {} from v{current} to {} ({asset_name})",
                exe.display(),
                release.tag
            )?;
            return Ok(());
        }

        writeln!(out, "Downloading {} ({asset_name})...", release.tag)?;
        out.flush()?;
        let sums_bytes = source.download(&sums_asset.download_url)?;
        let sums_txt = String::from_utf8(sums_bytes).context("SHA256SUMS is not valid UTF-8")?;
        let archive_bytes = source.download(&archive_asset.download_url)?;
        verify_sha256(&archive_bytes, &sums_txt, &asset_name)?;
        let binary_bytes = extract_binary(&archive_bytes, BIN_NAME)?;

        replace_binary_atomically(exe, &binary_bytes)?;
        writeln!(out, "Updated {} to {}.", exe.display(), release.tag)?;

        // Refresh shell activation and any existing systemd units.
        if !ShellActivation.planned_removals(app)?.is_empty() {
            for line in ShellActivation.install(app)? {
                writeln!(out, "shell: {line}")?;
            }
        }
        if let Some(config_base) = app.paths.config_file.parent().and_then(Path::parent) {
            let systemd_dir = config_base.join("systemd/user");
            if !SystemdUnits::installed_in(&systemd_dir).is_empty() {
                for line in (SystemdTimer {
                    interval_minutes: DEFAULT_PROBE_INTERVAL_MINUTES,
                })
                .install(app)?
                {
                    writeln!(out, "systemd: {line}")?;
                }
            }
        }
        Ok(())
    }

    fn resolve_release(&self, source: &dyn ReleaseSource) -> Result<Release> {
        match &self.version {
            Some(tag) => source.by_tag(tag),
            None => source.latest(self.pre),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::release::MockReleaseSource;
    use crate::release::ReleaseAsset;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use semver::Version;
    use sha2::{Digest, Sha256};
    use std::collections::HashMap;
    use std::fs;
    use tayassu_store::XdgPaths;

    struct FakeReleaseRegistry {
        releases: Vec<Release>,
        blobs: HashMap<String, Vec<u8>>,
    }

    impl ReleaseSource for FakeReleaseRegistry {
        fn fetch_releases(&self) -> Result<Vec<Release>> {
            Ok(self.releases.clone())
        }
        fn download(&self, url: &str) -> Result<Vec<u8>> {
            self.blobs
                .get(url)
                .cloned()
                .with_context(|| format!("missing fake blob {url}"))
        }
    }

    #[test]
    fn checksum_mismatch_preserves_installed_binary() {
        let dir = tempfile::tempdir().expect("tempdir");
        let exe = dir.path().join("tayassu");
        fs::write(&exe, b"original").expect("write original");
        let asset = archive_name(current_target().expect("target"));
        let release = Release {
            tag: "v99.0.0".to_owned(),
            version: Version::new(99, 0, 0),
            prerelease: false,
            assets: vec![
                ReleaseAsset {
                    name: asset.clone(),
                    download_url: "archive".to_owned(),
                },
                ReleaseAsset {
                    name: CHECKSUMS_ASSET_NAME.to_owned(),
                    download_url: "sums".to_owned(),
                },
            ],
        };
        let mut source = MockReleaseSource::new();
        source
            .expect_latest()
            .with(mockall::predicate::eq(false))
            .times(1)
            .return_once(move |_| Ok(release));
        let sums = format!("{}  {asset}\n", "0".repeat(64));
        source
            .expect_download()
            .with(mockall::predicate::eq("sums"))
            .times(1)
            .return_once(move |_| Ok(sums.into_bytes()));
        source
            .expect_download()
            .with(mockall::predicate::eq("archive"))
            .times(1)
            .return_once(|_| Ok(b"bad archive".to_vec()));
        let app = App {
            paths: XdgPaths::under(dir.path()),
            config: tayassu_store::AppConfig::default(),
            state: tayassu_store::RuntimeState::default(),
        };
        let cmd = Update {
            check: false,
            version: None,
            pre: false,
            dry_run: false,
        };
        let error = cmd
            .run_with(&source, &exe, &app, &mut Vec::new())
            .expect_err("reject invalid checksum");
        assert!(error.to_string().contains("checksum"));
        assert_eq!(fs::read(exe).expect("read original"), b"original");
    }

    #[test]
    fn verifies_checksum_and_replaces_binary_atomically() {
        let dir = tempfile::tempdir().expect("tempdir");
        let exe = dir.path().join("tayassu");
        fs::write(&exe, b"old-binary").expect("write old");

        let mut tar_gz = Vec::new();
        {
            let gz = GzEncoder::new(&mut tar_gz, Compression::default());
            let mut builder = tar::Builder::new(gz);
            let new_bin = b"new-binary-v99";
            let mut header = tar::Header::new_gnu();
            header.set_size(new_bin.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, "tayassu", &new_bin[..])
                .expect("append");
            builder.into_inner().expect("gz").finish().expect("finish");
        }
        let target = current_target().expect("target");
        let asset_name = archive_name(target);
        let hex = Sha256::digest(&tar_gz)
            .iter()
            .fold(String::new(), |mut acc, b| {
                use std::fmt::Write as _;
                let _ = write!(acc, "{b:02x}");
                acc
            });
        let sums = format!("{hex}  {asset_name}\n");

        let registry = FakeReleaseRegistry {
            releases: vec![Release {
                tag: "v99.0.0".to_owned(),
                version: Version::new(99, 0, 0),
                prerelease: false,
                assets: vec![
                    ReleaseAsset {
                        name: asset_name.clone(),
                        download_url: "mem://archive".to_owned(),
                    },
                    ReleaseAsset {
                        name: CHECKSUMS_ASSET_NAME.to_owned(),
                        download_url: "mem://sums".to_owned(),
                    },
                ],
            }],
            blobs: HashMap::from([
                ("mem://archive".to_owned(), tar_gz),
                ("mem://sums".to_owned(), sums.into_bytes()),
            ]),
        };

        let app = App {
            paths: XdgPaths::under(dir.path()),
            config: tayassu_store::AppConfig::default(),
            state: tayassu_store::RuntimeState::default(),
        };
        let cmd = Update {
            check: false,
            version: None,
            pre: false,
            dry_run: false,
        };
        let mut out = Vec::new();
        cmd.run_with(&registry, &exe, &app, &mut out)
            .expect("update");
        assert_eq!(fs::read(&exe).expect("read"), b"new-binary-v99");
        assert!(String::from_utf8(out).expect("utf8").contains("v99.0.0"));
    }
}

use rustix::net::{self, AddressFamily, SocketAddrUnix, SocketFlags, SocketType};
use rustix::process::{Uid, geteuid};
use sha2::{Digest, Sha256};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, symlink};
use std::path::{Path, PathBuf};

/// Relative name of the control socket published by Codex inside its profile.
const CONTROL_SOCKET: &str = "app-server-control/app-server-control.sock";
/// Permission bits required by Codex for its per-user socket directory.
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
/// Mask for the owner, group and other Unix permission bits.
const PERMISSIONS_MASK: u32 = 0o777;

pub(crate) fn restore_missing_link(profile: &Path) {
    let uid = geteuid();
    let directory = PathBuf::from(format!("/tmp/codex-daemon-{}", uid.as_raw()));
    if let Err(error) = restore(profile, &directory, uid) {
        tracing::debug!(%error, "could not restore Codex daemon control link");
    }
}

fn restore(profile: &Path, directory: &Path, uid: Uid) -> io::Result<()> {
    let link = profile.join(CONTROL_SOCKET);
    match link.symlink_metadata() {
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let metadata = directory.symlink_metadata()?;
    if !metadata.is_dir()
        || metadata.uid() != uid.as_raw()
        || metadata.mode() & PERMISSIONS_MASK != PRIVATE_DIRECTORY_MODE
    {
        return Ok(());
    }

    // Match the Unix socket layout used by Codex CLI 0.172.0.
    let parent = link.parent().expect("control socket has a parent");
    let canonical = parent
        .canonicalize()?
        .join(link.file_name().expect("control socket name"));
    let hash = Sha256::digest(canonical.as_os_str().as_bytes());
    let socket = directory.join(format!("{hash:x}"));
    let metadata = socket.symlink_metadata()?;
    if !metadata.file_type().is_socket() || metadata.uid() != uid.as_raw() {
        return Ok(());
    }

    let connection = net::socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
        None,
    )?;
    net::connect(&connection, &SocketAddrUnix::new(&socket)?)?;
    if net::sockopt::socket_peercred(&connection)?.uid != uid {
        return Ok(());
    }
    // Creating the symlink fails if a concurrent daemon startup published it first.
    symlink(socket, link)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    #[test]
    fn republishes_a_live_socket_without_replacing_an_existing_link() {
        let profile = tempfile::tempdir().expect("profile");
        let sockets = tempfile::tempdir().expect("socket directory");
        fs::set_permissions(
            sockets.path(),
            fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE),
        )
        .expect("restrict socket directory");
        let link = profile.path().join(CONTROL_SOCKET);
        fs::create_dir_all(link.parent().expect("parent")).expect("control directory");
        let hash = Sha256::digest(link.as_os_str().as_bytes());
        let socket = sockets.path().join(format!("{hash:x}"));

        let stale = UnixListener::bind(&socket).expect("bind socket");
        drop(stale);
        assert!(restore(profile.path(), sockets.path(), geteuid()).is_err());
        assert!(link.symlink_metadata().is_err());

        fs::remove_file(&socket).expect("remove stale socket");
        let _listener = UnixListener::bind(&socket).expect("bind live socket");
        restore(profile.path(), sockets.path(), geteuid()).expect("restore");
        assert_eq!(link.read_link().expect("link"), socket);

        fs::remove_file(&link).expect("remove link");
        let existing = Path::new("existing-target");
        symlink(existing, &link).expect("install another link");
        restore(profile.path(), sockets.path(), geteuid()).expect("preserve existing link");
        assert_eq!(link.read_link().expect("existing link"), existing);
    }
}

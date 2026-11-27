use crate::error::StoreError;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

/// Writes to a unique sibling file before publishing the completed contents.
/// Writes `bytes` to a sibling temp file, syncs it, and atomically renames it over `target`.
///
/// # Errors
///
/// Returns [`StoreError::Io`] if directory creation, file writing, sync, or rename fails.
pub fn write_atomic(target: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let parent = target
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let io_error = |source| StoreError::Io {
        path: target.to_path_buf(),
        source,
    };

    fs::create_dir_all(parent).map_err(io_error)?;
    let mut file = NamedTempFile::new_in(parent).map_err(io_error)?;
    file.write_all(bytes).map_err(io_error)?;
    file.as_file().sync_all().map_err(io_error)?;
    file.persist(target)
        .map_err(|error| io_error(error.error))?;
    Ok(())
}

/// Exclusive advisory file lock guarding concurrent modifications to a persisted file.
pub(crate) struct FileLock {
    _file: File,
}

impl FileLock {
    pub(crate) fn acquire(lock_path: PathBuf) -> Result<Self, StoreError> {
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent).map_err(|source| StoreError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|source| StoreError::Io {
                path: lock_path.clone(),
                source,
            })?;
        file.lock().map_err(|source| StoreError::Io {
            path: lock_path,
            source,
        })?;
        Ok(Self { _file: file })
    }
}

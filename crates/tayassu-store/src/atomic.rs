use crate::error::StoreError;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

/// Temporary filename suffix appended during atomic file replacement.
const TEMP_FILE_SUFFIX: &str = ".tmp";

/// Writes `bytes` to a sibling temp file, syncs it, and atomically renames it over `target`.
pub(crate) fn write_atomic(target: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|source| StoreError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let file_name = target.file_name().map_or_else(
        || "file".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    let temp_path = target.with_file_name(format!("{file_name}{TEMP_FILE_SUFFIX}"));

    let mut file = File::create(&temp_path).map_err(|source| StoreError::Io {
        path: temp_path.clone(),
        source,
    })?;
    file.write_all(bytes).map_err(|source| StoreError::Io {
        path: temp_path.clone(),
        source,
    })?;
    file.sync_all().map_err(|source| StoreError::Io {
        path: temp_path.clone(),
        source,
    })?;

    fs::rename(&temp_path, target).map_err(|source| StoreError::Io {
        path: target.to_path_buf(),
        source,
    })
}

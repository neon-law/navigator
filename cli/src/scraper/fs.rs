//! Filesystem helpers shared by the scraper's state files (session
//! cookies, download ledger, run state): every one persists atomically, so
//! an interrupted write never leaves a half-read file behind.

use std::path::{Path, PathBuf};

/// Create `path` (and its parents) if it does not exist yet.
///
/// # Errors
///
/// When the directory cannot be created.
pub(crate) fn ensure_dir(path: &Path) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(path)
}

/// Write `bytes` to `path` atomically: create parents, write beside the
/// target, rename into place.
///
/// # Errors
///
/// When any step of the write fails.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = PathBuf::from(format!(
        "{}.tmp-{}",
        path.display(),
        uuid::Uuid::now_v7()
    ));
    std::fs::write(&temp, bytes)?;
    std::fs::rename(&temp, path)?;
    Ok(())
}

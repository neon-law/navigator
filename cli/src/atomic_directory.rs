//! Atomic no-replacement publication of a verified directory.

use anyhow::{Context, Result};
use std::path::Path;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn publish_no_clobber(source: &Path, destination: &Path) -> Result<()> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        source,
        rustix::fs::CWD,
        destination,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .context("publish verified restore without replacing an existing path")?;
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn publish_no_clobber(_source: &Path, _destination: &Path) -> Result<()> {
    anyhow::bail!("no-clobber directory publication is unsupported on this platform")
}

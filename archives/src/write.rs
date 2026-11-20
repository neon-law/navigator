//! The archive destination must disclose its retention policy before receiving bytes.
use anyhow::{Context, Result};
use cloud::StorageService;

pub(crate) async fn policy(storage: &dyn StorageService) -> Result<bool> {
    storage
        .retention_locked()
        .await
        .context("probe the archive bucket's retention policy")
}

pub(crate) async fn object(
    storage: &dyn StorageService,
    key: &str,
    bytes: &[u8],
    content_type: &str,
) -> Result<()> {
    if policy(storage).await? {
        storage.put_immutable(key, bytes, content_type).await?;
    } else {
        storage.put(key, bytes, content_type).await?;
    }
    Ok(())
}

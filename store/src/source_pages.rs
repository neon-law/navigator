//! `store::source_pages` — page renderings of a stored PDF asset, with
//! the relied-on passage pinned to a normalised rectangle (#893).
//!
//! [`crate::assets`] holds the bytes; [`pdf::passage`] does the geometry.
//! This module joins them: fetch an asset, locate a quote in it, lift the
//! pages it falls on, and persist each page rendering through
//! [`cloud::StorageService`] — never a local path, never into the
//! repository.
//!
//! # Provenance is the storage key
//!
//! A rendering is keyed by the **SHA-256 of the exact asset bytes it was
//! cut from**, which is the revision identity `assets` already carries:
//!
//! ```text
//! source-pages/<sha256>/p<page-index>.pdf
//! ```
//!
//! Superseding a document produces different bytes, therefore a
//! different digest, therefore a different key. There is no way for a
//! superseded document to keep serving a stale highlight, because the
//! stale rendering is not reachable from the new revision. The returned
//! [`SourcePageRender`] carries the digest so a caller records what it
//! pinned rather than assuming.
//!
//! Because the key is content-addressed, writing is idempotent: the same
//! asset and page re-render to the same key, so a second request reuses
//! the stored object rather than uploading it again.
//!
//! # What it refuses
//!
//! An asset that is not a PDF, and every failure [`pdf::passage`] refuses
//! to guess at — an unfound quote, an out-of-range occurrence, an
//! unmeasurable font. None of them produce a rendering.

use std::sync::Arc;

use cloud::StorageService;
use pdf::{NormalisedRect, PassageError};
use uuid::Uuid;

use crate::application_store::ApplicationStore;

/// Errors from [`render_passage`].
#[derive(Debug, thiserror::Error)]
pub enum SourcePageError {
    #[error("storage: {0}")]
    Storage(#[from] cloud::StorageError),
    #[error("database: {0}")]
    Db(#[from] crate::assets::AssetError),
    #[error("no asset {0}")]
    NoAsset(Uuid),
    /// Only a PDF has pages to render and a text layer to locate in.
    #[error("asset {asset_id} is `{content_type}`, not a PDF")]
    NotAPdf {
        asset_id: Uuid,
        content_type: String,
    },
    #[error(transparent)]
    Passage(#[from] PassageError),
}

/// One page of a source document, rendered and stored, with the passage
/// marked on it.
#[derive(Debug, Clone, PartialEq)]
pub struct SourcePageRegion {
    /// Zero-based page index within the source asset.
    pub page_index: usize,
    /// Where the page rendering was written. Stable for a given asset
    /// revision and page.
    pub storage_key: String,
    /// The passage's region on that page, as page fractions.
    pub rect: NormalisedRect,
}

/// A stored page rendering set for one occurrence of one quote.
#[derive(Debug, Clone, PartialEq)]
pub struct SourcePageRender {
    /// The asset the rendering was cut from.
    pub asset_id: Uuid,
    /// The revision: the SHA-256 of the exact bytes rendered. A
    /// superseded document has a different digest and therefore
    /// different keys.
    pub asset_sha256_hex: String,
    /// Which occurrence of the quote was pinned, 1-based.
    pub ordinal: usize,
    /// How many times the quote appears in the asset.
    pub occurrences: usize,
    /// One region per line the passage covers; a passage broken across a
    /// page break carries regions with differing
    /// [`SourcePageRegion::page_index`] and therefore differing
    /// [`SourcePageRegion::storage_key`].
    pub regions: Vec<SourcePageRegion>,
}

impl SourcePageRender {
    /// The distinct page renderings this passage needs, in page order.
    #[must_use]
    pub fn storage_keys(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for region in &self.regions {
            if !out.contains(&region.storage_key) {
                out.push(region.storage_key.clone());
            }
        }
        out
    }
}

/// The key a page rendering of `sha256_hex`'s page `page_index` is
/// stored at. Content-addressed, so it pins the revision rather than the
/// asset row.
#[must_use]
pub fn storage_key(sha256_hex: &str, page_index: usize) -> String {
    format!("source-pages/{sha256_hex}/p{page_index}.pdf")
}

/// Locate the `ordinal`-th (1-based) occurrence of `quote` in the PDF
/// asset `asset_id`, render every page it falls on, store those
/// renderings, and return the normalised rects tied to the revision.
///
/// # Errors
/// [`SourcePageError::NoAsset`] for an unknown id,
/// [`SourcePageError::NotAPdf`] for an asset with no pages,
/// [`SourcePageError::Passage`] for anything [`pdf::locate`] refuses to
/// guess at, and [`SourcePageError::Storage`] /
/// [`SourcePageError::Db`] on an infrastructure failure.
pub async fn render_passage(
    db: &ApplicationStore,
    storage: &Arc<dyn StorageService>,
    asset_id: Uuid,
    quote: &str,
    ordinal: usize,
) -> Result<SourcePageRender, SourcePageError> {
    let row = crate::assets::find_by_id(db, asset_id)
        .await?
        .ok_or(SourcePageError::NoAsset(asset_id))?;
    if row.content_type != "application/pdf" {
        return Err(SourcePageError::NotAPdf {
            asset_id,
            content_type: row.content_type,
        });
    }
    let bytes = storage.get(&row.storage_key).await?.bytes;

    // Locate first: a quote that cannot be pinned must not leave a
    // rendering behind in storage.
    let found = pdf::locate(&bytes, quote, ordinal)?;

    let mut regions = Vec::with_capacity(found.rects.len());
    for hit in &found.rects {
        let key = storage_key(&row.sha256_hex, hit.page_index);
        if !storage.exists(&key).await? {
            let page = pdf::page_render(&bytes, hit.page_index)?;
            storage.put(&key, &page, "application/pdf").await?;
        }
        regions.push(SourcePageRegion {
            page_index: hit.page_index,
            storage_key: key,
            rect: hit.rect,
        });
    }

    Ok(SourcePageRender {
        asset_id,
        asset_sha256_hex: row.sha256_hex,
        ordinal: found.ordinal,
        occurrences: found.occurrences,
        regions,
    })
}

#[cfg(test)]
mod tests {}

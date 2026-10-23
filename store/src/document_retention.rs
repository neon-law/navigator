//! Soft-deleted document chains in the private archived-projects lane.
//!
//! A [Soft Deletion](../../docs/glossary/soft-deletion.md) hides a chain at
//! once. [`reconcile_soft_deleted`] then copies every revision of each
//! soft-deleted chain to the archived-projects lane, reads each copy back, and
//! checks its size and SHA-256 before it records a checkpoint. Only when every
//! revision of the chain has verified does it remove the active copies, and it
//! keeps any object another unpurged row still points at — a sibling chain on
//! the same matter sharing the content-addressed bytes, or a row on another
//! matter. A chain whose archive is still in progress keeps its active copies
//! and stays recoverable; a rerun after any interruption repeats only the
//! idempotent copy-and-verify.
//!
//! [`restore_chain`] puts the archived bytes back at their original keys,
//! verified, before it clears the Soft Deletion stamps, so a restored chain
//! has the same ids, revisions, checksums, and visibility. [`read_archived`]
//! serves a lawyer's read of a soft-deleted revision whose active copy is
//! gone. [`purge_archived_copies`] is the retention purge's half: archived
//! bytes leave with the active ones unless another live or held row on the
//! matter needs them, and each removal is recorded so recovery generations
//! that held them report a purge rather than corruption.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use cloud::{StorageError, StorageService};
use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

use crate::assets::{Asset, Restoration, RestoreError};
use crate::postgres::PostgresDb;
use crate::project_archives::object_key;
use crate::surreal::SurrealDb;

const OBJECT_CONTENT_TYPE: &str = "application/octet-stream";

/// The stores the retention lane reads and writes.
pub struct RetentionStores<'a> {
    pub surreal: &'a SurrealDb,
    pub pg: &'a PostgresDb,
    pub documents: &'a Arc<dyn StorageService>,
    pub archive: &'a Arc<dyn StorageService>,
}

/// A chain's stable identity on its matter: its slug, or the asset id of a
/// document that has none.
#[must_use]
pub fn chain_key(asset: &Asset) -> String {
    asset.slug.clone().unwrap_or_else(|| asset.id.to_string())
}

/// What one reconcile pass did. Keys and counts only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ArchiveReport {
    /// Chains whose every revision is now verified in the archive.
    pub archived: usize,
    /// Chains whose active copies were removed this pass.
    pub active_removed: usize,
    /// Chains on a Project with no Firm, which cannot be archived.
    pub without_firm: usize,
    /// Chains that stopped, with the reason; they keep their active copies.
    pub stalled: Vec<String>,
}

/// Why a retention command stopped. Messages carry ids and keys, never
/// document content.
#[derive(Debug, thiserror::Error)]
pub enum RetentionError {
    #[error("revision {0} has no readable source bytes")]
    SourceMissing(Uuid),
    #[error("revision {0}'s bytes do not match its recorded digest")]
    SourceCorrupt(Uuid),
    #[error("archived copy of revision {0} is missing or damaged")]
    ArchiveDamaged(Uuid),
    #[error("the archived-projects lane is needed to restore this chain")]
    ArchiveUnavailable,
    #[error("restore: {0}")]
    Restore(#[from] RestoreError),
    #[error("storage: {0}")]
    Storage(#[from] StorageError),
    #[error("database: {0}")]
    Database(String),
    #[error("store: {0}")]
    Store(String),
}

impl From<sqlx::Error> for RetentionError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error.to_string())
    }
}

fn store_error(error: impl std::fmt::Display) -> RetentionError {
    RetentionError::Store(error.to_string())
}

fn sha256_hex(bytes: &[u8]) -> String {
    crate::assets::sha256_hex(bytes)
}

/// One archive row's identity and progress.
#[derive(Debug, Clone)]
struct ChainArchive {
    id: Uuid,
    state: String,
    active_removed: bool,
}

async fn find_or_create_archive(
    pg: &PostgresDb,
    firm_id: Uuid,
    project_id: Uuid,
    key: &str,
    deleted_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<ChainArchive, RetentionError> {
    sqlx::query(
        "INSERT INTO document_chain_archive \
         (id, firm_id, project_id, chain_key, deleted_at, state, requested_at) \
         VALUES ($1, $2, $3, $4, $5, 'archiving', $6) \
         ON CONFLICT (project_id, chain_key, deleted_at) DO NOTHING",
    )
    .bind(Uuid::now_v7())
    .bind(firm_id)
    .bind(project_id)
    .bind(key)
    .bind(deleted_at)
    .bind(now)
    .execute(pg.pool())
    .await?;
    let row = sqlx::query(
        "SELECT id, state, active_removed_at FROM document_chain_archive \
         WHERE project_id = $1 AND chain_key = $2 AND deleted_at = $3",
    )
    .bind(project_id)
    .bind(key)
    .bind(deleted_at)
    .fetch_one(pg.pool())
    .await?;
    Ok(ChainArchive {
        id: row.try_get("id")?,
        state: row.try_get("state")?,
        active_removed: row
            .try_get::<Option<DateTime<Utc>>, _>("active_removed_at")?
            .is_some(),
    })
}

/// Archive every soft-deleted chain not yet archived, then remove the active
/// copies of every fully verified one. Bounded to `limit` soft-deleted rows.
///
/// # Errors
/// A database failure listing the chains. A chain that fails is reported in
/// [`ArchiveReport::stalled`] and keeps its active copies.
pub async fn reconcile_soft_deleted(
    stores: &RetentionStores<'_>,
    now: DateTime<Utc>,
    limit: usize,
) -> Result<ArchiveReport, RetentionError> {
    let rows = crate::assets::soft_deleted_unpurged(stores.surreal, limit)
        .await
        .map_err(store_error)?;
    let mut chains: BTreeMap<(Uuid, String), Asset> = BTreeMap::new();
    for row in rows {
        if let Some(project_id) = row.project_id {
            chains.entry((project_id, chain_key(&row))).or_insert(row);
        }
    }
    let mut report = ArchiveReport::default();
    for ((project_id, key), first) in chains {
        let project = crate::projects::find_by_id(stores.surreal, project_id)
            .await
            .map_err(store_error)?;
        let Some(firm_id) = project.and_then(|project| project.firm_id) else {
            report.without_firm += 1;
            continue;
        };
        match archive_chain(stores, firm_id, project_id, &key, &first, now).await {
            Ok((archived, removed)) => {
                report.archived += usize::from(archived);
                report.active_removed += usize::from(removed);
            }
            Err(error) => {
                tracing::warn!(%project_id, %error, "soft-deleted chain archive stalled");
                report.stalled.push(format!("{project_id}/{key}: {error}"));
            }
        }
    }
    Ok(report)
}

/// Archive one chain and, once it is verified, remove its active copies.
/// Returns whether it became archived this call and whether active copies
/// were removed.
async fn archive_chain(
    stores: &RetentionStores<'_>,
    firm_id: Uuid,
    project_id: Uuid,
    key: &str,
    first: &Asset,
    now: DateTime<Utc>,
) -> Result<(bool, bool), RetentionError> {
    let Some(deleted_at) = first.deleted_at else {
        return Ok((false, false));
    };
    let archive =
        find_or_create_archive(stores.pg, firm_id, project_id, key, deleted_at, now).await?;
    let chain = crate::assets::chain(stores.surreal, first)
        .await
        .map_err(store_error)?;
    let mut became_archived = false;
    if archive.state == "archiving" {
        sqlx::query("UPDATE document_chain_archive SET attempts = attempts + 1 WHERE id = $1")
            .bind(archive.id)
            .execute(stores.pg.pool())
            .await?;
        if let Err(error) = copy_chain(stores, firm_id, project_id, &archive, &chain, now).await {
            sqlx::query("UPDATE document_chain_archive SET last_error = $2 WHERE id = $1")
                .bind(archive.id)
                .bind(error.to_string())
                .execute(stores.pg.pool())
                .await?;
            return Err(error);
        }
        sqlx::query(
            "UPDATE document_chain_archive SET state = 'archived', archived_at = $2, \
             last_error = NULL WHERE id = $1 AND state = 'archiving'",
        )
        .bind(archive.id)
        .bind(now)
        .execute(stores.pg.pool())
        .await?;
        became_archived = true;
    } else if archive.state != "archived" || archive.active_removed {
        return Ok((false, false));
    }
    let removed = remove_active_copies(stores, &archive, &chain, deleted_at, now).await?;
    Ok((became_archived, removed))
}

async fn copy_chain(
    stores: &RetentionStores<'_>,
    firm_id: Uuid,
    project_id: Uuid,
    archive: &ChainArchive,
    chain: &[Asset],
    now: DateTime<Utc>,
) -> Result<(), RetentionError> {
    for asset in chain.iter().filter(|asset| asset.purged_at.is_none()) {
        let target = object_key(firm_id, project_id, &asset.sha256_hex);
        sqlx::query(
            "INSERT INTO document_chain_archive_item \
             (archive_id, asset_id, storage_key, object_key, sha256, size_bytes) \
             VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (archive_id, asset_id) DO NOTHING",
        )
        .bind(archive.id)
        .bind(asset.id)
        .bind(&asset.storage_key)
        .bind(&target)
        .bind(&asset.sha256_hex)
        .bind(asset.byte_size)
        .execute(stores.pg.pool())
        .await?;
        let verified: Option<DateTime<Utc>> = sqlx::query_scalar(
            "SELECT verified_at FROM document_chain_archive_item \
             WHERE archive_id = $1 AND asset_id = $2",
        )
        .bind(archive.id)
        .bind(asset.id)
        .fetch_one(stores.pg.pool())
        .await?;
        if verified.is_some() {
            continue;
        }
        let bytes = match stores.documents.get(&asset.storage_key).await {
            Ok(object) => object.bytes,
            // An earlier deletion of this chain may already have moved the
            // bytes; the archived copy is then the source.
            Err(StorageError::NotFound(_)) => match stores.archive.get(&target).await {
                Ok(object) => object.bytes,
                Err(StorageError::NotFound(_)) => {
                    return Err(RetentionError::SourceMissing(asset.id))
                }
                Err(error) => return Err(error.into()),
            },
            Err(error) => return Err(error.into()),
        };
        if sha256_hex(&bytes) != asset.sha256_hex {
            return Err(RetentionError::SourceCorrupt(asset.id));
        }
        crate::project_archives::write_verified(
            stores.archive,
            &target,
            &bytes,
            OBJECT_CONTENT_TYPE,
            &asset.id.to_string(),
        )
        .await
        .map_err(|_| RetentionError::ArchiveDamaged(asset.id))?;
        sqlx::query(
            "UPDATE document_chain_archive_item SET verified_at = $3 \
             WHERE archive_id = $1 AND asset_id = $2 AND verified_at IS NULL",
        )
        .bind(archive.id)
        .bind(asset.id)
        .bind(now)
        .execute(stores.pg.pool())
        .await?;
    }
    Ok(())
}

/// Remove a verified chain's active copies, keeping any object another
/// unpurged row needs. Rechecks that the chain is still the same soft
/// deletion first: a chain restored meanwhile keeps everything.
async fn remove_active_copies(
    stores: &RetentionStores<'_>,
    archive: &ChainArchive,
    chain: &[Asset],
    deleted_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<bool, RetentionError> {
    let ids: Vec<Uuid> = chain.iter().map(|asset| asset.id).collect();
    let fresh = crate::assets::find_by_ids(stores.surreal, &ids)
        .await
        .map_err(store_error)?;
    if fresh.is_empty()
        || fresh
            .iter()
            .any(|asset| asset.deleted_at != Some(deleted_at) || asset.purged_at.is_some())
    {
        return Ok(false);
    }
    for asset in &fresh {
        let target = sqlx::query_scalar::<_, String>(
            "SELECT object_key FROM document_chain_archive_item \
             WHERE archive_id = $1 AND asset_id = $2 AND verified_at IS NOT NULL",
        )
        .bind(archive.id)
        .bind(asset.id)
        .fetch_optional(stores.pg.pool())
        .await?
        .ok_or(RetentionError::ArchiveDamaged(asset.id))?;
        // Verify the archived bytes again immediately before the active copy
        // goes, so a damaged archive never becomes the only copy.
        match stores.archive.get(&target).await {
            Ok(object) if sha256_hex(&object.bytes) == asset.sha256_hex => {}
            Ok(_) | Err(StorageError::NotFound(_)) => {
                return Err(RetentionError::ArchiveDamaged(asset.id))
            }
            Err(error) => return Err(error.into()),
        }
    }
    for asset in &fresh {
        for key in std::iter::once(&asset.storage_key).chain(asset.secondary_storage_key.as_ref()) {
            if crate::assets::storage_key_needed_elsewhere(stores.surreal, key, &ids)
                .await
                .map_err(store_error)?
            {
                continue;
            }
            match stores.documents.delete(key).await {
                Ok(()) | Err(StorageError::NotFound(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    sqlx::query(
        "UPDATE document_chain_archive SET active_removed_at = $2 \
         WHERE id = $1 AND active_removed_at IS NULL",
    )
    .bind(archive.id)
    .bind(now)
    .execute(stores.pg.pool())
    .await?;
    Ok(true)
}

/// The verified archived bytes of one revision, if the retention lane holds
/// them. Returns `None` when no verified copy exists.
///
/// # Errors
/// [`RetentionError::ArchiveDamaged`] when the recorded copy is missing or its
/// digest differs, or a storage or database error.
pub async fn read_archived(
    pg: &PostgresDb,
    archive: &Arc<dyn StorageService>,
    asset: &Asset,
) -> Result<Option<Vec<u8>>, RetentionError> {
    let target = sqlx::query_scalar::<_, String>(
        "SELECT i.object_key FROM document_chain_archive_item i \
         JOIN document_chain_archive a ON a.id = i.archive_id \
         WHERE i.asset_id = $1 AND i.verified_at IS NOT NULL AND a.state <> 'purged' \
         ORDER BY a.requested_at DESC LIMIT 1",
    )
    .bind(asset.id)
    .fetch_optional(pg.pool())
    .await?;
    let Some(target) = target else {
        return Ok(None);
    };
    match archive.get(&target).await {
        Ok(object) if sha256_hex(&object.bytes) == asset.sha256_hex => Ok(Some(object.bytes)),
        Ok(_) | Err(StorageError::NotFound(_)) => Err(RetentionError::ArchiveDamaged(asset.id)),
        Err(error) => Err(error.into()),
    }
}

/// Restore the chain `asset_id` belongs to: return every revision's bytes to
/// its original key from the verified archive where the active copy is gone,
/// then clear the Soft Deletion stamps. Nothing is cleared until every
/// revision's bytes are back and verified; a purged chain cannot be restored.
///
/// # Errors
/// [`RetentionError`] for a foreign or purged chain, a missing archive lane
/// when bytes must come from it, damaged archived bytes, or a store failure.
pub async fn restore_chain(
    surreal: &SurrealDb,
    pg: Option<&PostgresDb>,
    documents: &Arc<dyn StorageService>,
    archive: Option<&Arc<dyn StorageService>>,
    project_id: Uuid,
    asset_id: Uuid,
    now: DateTime<Utc>,
) -> Result<Restoration, RetentionError> {
    let asset = crate::assets::find_by_id(surreal, asset_id)
        .await
        .map_err(store_error)?
        .filter(|asset| asset.project_id == Some(project_id))
        .ok_or(RetentionError::Restore(RestoreError::NotOnProject))?;
    let chain = crate::assets::chain(surreal, &asset)
        .await
        .map_err(store_error)?;
    if chain.iter().any(|row| row.purged_at.is_some()) {
        return Err(RestoreError::Purged.into());
    }
    for row in chain.iter().filter(|row| row.deleted_at.is_some()) {
        for key in std::iter::once(&row.storage_key).chain(row.secondary_storage_key.as_ref()) {
            let present = match documents.get(key).await {
                Ok(object) => sha256_hex(&object.bytes) == row.sha256_hex,
                Err(StorageError::NotFound(_)) => false,
                Err(error) => return Err(error.into()),
            };
            if present {
                continue;
            }
            let (Some(pg), Some(archive)) = (pg, archive) else {
                return Err(RetentionError::ArchiveUnavailable);
            };
            let bytes = read_archived(pg, archive, row)
                .await?
                .ok_or(RetentionError::ArchiveDamaged(row.id))?;
            crate::project_archives::write_verified(
                documents,
                key,
                &bytes,
                &row.content_type,
                &row.id.to_string(),
            )
            .await
            .map_err(|_| RetentionError::ArchiveDamaged(row.id))?;
        }
    }
    let deleted_at = chain.iter().find_map(|row| row.deleted_at);
    let restoration = crate::assets::restore(surreal, project_id, asset_id).await?;
    if let (Some(pg), Some(deleted_at)) = (pg, deleted_at) {
        sqlx::query(
            "UPDATE document_chain_archive SET state = 'restored', restored_at = $4 \
             WHERE project_id = $1 AND chain_key = $2 AND deleted_at = $3 \
             AND state IN ('archiving', 'archived')",
        )
        .bind(project_id)
        .bind(chain_key(&asset))
        .bind(deleted_at)
        .bind(now)
        .execute(pg.pool())
        .await?;
    }
    Ok(restoration)
}

/// The retention purge's archived half for one chain: delete each archived
/// copy no other unpurged row on the matter needs, record each removal, and
/// mark the chain's archives purged.
///
/// # Errors
/// A storage or database failure; a rerun converges.
pub async fn purge_archived_copies(
    stores: &RetentionStores<'_>,
    firm_id: Uuid,
    project_id: Uuid,
    chain: &[Asset],
    expunge_record_id: Option<Uuid>,
    now: DateTime<Utc>,
) -> Result<usize, RetentionError> {
    let (surreal, pg, archive) = (stores.surreal, stores.pg, stores.archive);
    let ids: Vec<Uuid> = chain.iter().map(|asset| asset.id).collect();
    let mut digests: Vec<&str> = chain
        .iter()
        .map(|asset| asset.sha256_hex.as_str())
        .collect();
    digests.sort_unstable();
    digests.dedup();
    let mut removed = 0;
    for digest in digests {
        if crate::assets::digest_needed_elsewhere(surreal, project_id, digest, &ids)
            .await
            .map_err(store_error)?
        {
            continue;
        }
        match archive
            .delete(&object_key(firm_id, project_id, digest))
            .await
        {
            Ok(()) | Err(StorageError::NotFound(_)) => {}
            Err(error) => return Err(error.into()),
        }
        sqlx::query(
            "INSERT INTO archived_object_purge (project_id, sha256, purged_at, expunge_record_id) \
             VALUES ($1, $2, $3, $4) ON CONFLICT (project_id, sha256) DO NOTHING",
        )
        .bind(project_id)
        .bind(digest)
        .bind(now)
        .bind(expunge_record_id)
        .execute(pg.pool())
        .await?;
        removed += 1;
    }
    if let Some(first) = chain.first() {
        sqlx::query(
            "UPDATE document_chain_archive SET state = 'purged', purged_at = $3 \
             WHERE project_id = $1 AND chain_key = $2 AND state <> 'purged'",
        )
        .bind(project_id)
        .bind(chain_key(first))
        .bind(now)
        .execute(pg.pool())
        .await?;
    }
    Ok(removed)
}

/// Archived digests the retention purge removed on a Project, with when.
///
/// # Errors
/// A database error.
pub async fn purged_digests(
    pg: &PostgresDb,
    project_id: Uuid,
) -> Result<BTreeMap<String, DateTime<Utc>>, RetentionError> {
    let rows =
        sqlx::query("SELECT sha256, purged_at FROM archived_object_purge WHERE project_id = $1")
            .bind(project_id)
            .fetch_all(pg.pool())
            .await?;
    rows.iter()
        .map(|row| Ok((row.try_get("sha256")?, row.try_get("purged_at")?)))
        .collect()
}

/// Record, or join, one chain's governed purge run. Returns the expunge record
/// an earlier attempt already wrote, so a retry does not expunge twice.
///
/// # Errors
/// A database error.
pub async fn start_purge_run(
    pg: &PostgresDb,
    project_id: Uuid,
    chain_key: &str,
    purge_after: DateTime<Utc>,
    firm_id: Uuid,
    authorized_by: Uuid,
    now: DateTime<Utc>,
) -> Result<Option<Uuid>, RetentionError> {
    sqlx::query(
        "INSERT INTO retention_purge \
         (project_id, chain_key, purge_after, firm_id, authorized_by_person_id, started_at) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (project_id, chain_key, purge_after) DO NOTHING",
    )
    .bind(project_id)
    .bind(chain_key)
    .bind(purge_after)
    .bind(firm_id)
    .bind(authorized_by)
    .bind(now)
    .execute(pg.pool())
    .await?;
    Ok(sqlx::query_scalar(
        "SELECT expunge_record_id FROM retention_purge \
         WHERE project_id = $1 AND chain_key = $2 AND purge_after = $3",
    )
    .bind(project_id)
    .bind(chain_key)
    .bind(purge_after)
    .fetch_one(pg.pool())
    .await?)
}

/// Record the expunge one purge run wrote.
///
/// # Errors
/// A database error.
pub async fn record_purge_expunge(
    pg: &PostgresDb,
    project_id: Uuid,
    chain_key: &str,
    purge_after: DateTime<Utc>,
    expunge_record_id: Uuid,
) -> Result<(), RetentionError> {
    sqlx::query(
        "UPDATE retention_purge SET expunge_record_id = $4 \
         WHERE project_id = $1 AND chain_key = $2 AND purge_after = $3 \
         AND expunge_record_id IS NULL",
    )
    .bind(project_id)
    .bind(chain_key)
    .bind(purge_after)
    .bind(expunge_record_id)
    .execute(pg.pool())
    .await?;
    Ok(())
}

/// Mark one purge run finished.
///
/// # Errors
/// A database error.
pub async fn finish_purge_run(
    pg: &PostgresDb,
    project_id: Uuid,
    chain_key: &str,
    purge_after: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), RetentionError> {
    sqlx::query(
        "UPDATE retention_purge SET finished_at = $4 \
         WHERE project_id = $1 AND chain_key = $2 AND purge_after = $3 AND finished_at IS NULL",
    )
    .bind(project_id)
    .bind(chain_key)
    .bind(purge_after)
    .bind(now)
    .execute(pg.pool())
    .await?;
    Ok(())
}

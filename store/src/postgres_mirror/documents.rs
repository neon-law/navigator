//! Asset rows in the PostgreSQL mirror: document assets and bare content
//! assets alike.
//!
//! The same contract as the tenancy mirror: SurrealDB is read back after its
//! write, and PostgreSQL's copy is upserted or deleted to match. Each row
//! keeps its Surreal record UUID as its PostgreSQL `id`, so a revision chain
//! (`project_id`, `slug`, newest id first) reads the same in both stores.
//!
//! The row is read raw rather than through [`crate::assets::Asset`]: the
//! JSON columns (`metadata`, `derived_from`, `signing`, `notarization`) are
//! copied exactly as SurrealDB stores them, never re-parsed into a shape
//! this build understands. Checksums, storage keys, visibility, mail
//! provenance, and the soft-deletion, Legal Hold, and purge stamps are
//! copied as stored.
//!
//! An asset names its Project and, once soft-deleted, the Person who deleted
//! it. Each one PostgreSQL does not hold is mirrored first; one SurrealDB
//! cannot supply is refused with SQLSTATE 23503 and reported by [`parity`].
//!
//! Reports name rows by key only, never by filename or storage key.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use serde_json::Value as Json;
use sqlx::types::Json as Jsonb;
use sqlx::Row;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use super::tenancy::{compare, remirror, select_sql, source, target, upsert_sql};
use super::{changed, identity, micros, MirrorError, MirrorKey, Mirrored, ParityReport};
use crate::postgres::PostgresDb;
use crate::surreal::{record_id, record_uuid, SurrealDb};

const ASSET_COLUMNS: &[&str] = &[
    "id",
    "storage_key",
    "secondary_storage_key",
    "content_type",
    "byte_size",
    "sha256_hex",
    "project_id",
    "filename",
    "kind",
    "source",
    "received_at",
    "description",
    "visibility",
    "slug",
    "published_at",
    "source_message_id",
    "source_sender",
    "source_received_at",
    "source_subject",
    "metadata",
    "derived_from",
    "transcript_quality",
    "signing",
    "notarization",
    "deleted_at",
    "purge_after",
    "deleted_by_person_id",
    "deleted_commit_sha",
    "legal_hold_at",
    "purged_at",
    "inserted_at",
    "updated_at",
];

static ASSET_UPSERT: LazyLock<String> = LazyLock::new(|| upsert_sql("asset", ASSET_COLUMNS));
static ASSET_SELECT: LazyLock<String> = LazyLock::new(|| select_sql("asset", ASSET_COLUMNS));

/// The Surreal projection, every column the mirror copies.
const ASSET_SOURCE_FIELDS: &str = "id, storage_key, secondary_storage_key, content_type, \
    byte_size, sha256_hex, project_id, filename, kind, source, received_at, description, \
    visibility, slug, published_at, source_message_id, source_sender, source_received_at, \
    source_subject, metadata, derived_from, transcript_quality, signing, notarization, \
    deleted_at, purge_after, deleted_by_person_id, deleted_commit_sha, legal_hold_at, purged_at, \
    inserted_at, updated_at";

/// The asset row as SurrealDB stores it.
#[derive(SurrealValue)]
struct AssetSource {
    id: surrealdb::types::RecordId,
    storage_key: String,
    secondary_storage_key: Option<String>,
    content_type: String,
    byte_size: i64,
    sha256_hex: String,
    project_id: Option<surrealdb::types::RecordId>,
    filename: Option<String>,
    kind: Option<String>,
    source: Option<String>,
    received_at: Option<String>,
    description: Option<String>,
    visibility: String,
    slug: Option<String>,
    published_at: Option<String>,
    source_message_id: Option<String>,
    source_sender: Option<String>,
    source_received_at: Option<String>,
    source_subject: Option<String>,
    metadata: Option<Json>,
    derived_from: Option<Json>,
    transcript_quality: Option<String>,
    signing: Option<Json>,
    notarization: Option<Json>,
    deleted_at: Option<surrealdb::types::Datetime>,
    purge_after: Option<surrealdb::types::Datetime>,
    deleted_by_person_id: Option<surrealdb::types::RecordId>,
    deleted_commit_sha: Option<String>,
    legal_hold_at: Option<surrealdb::types::Datetime>,
    purged_at: Option<surrealdb::types::Datetime>,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AssetImage {
    id: Uuid,
    storage_key: String,
    secondary_storage_key: Option<String>,
    content_type: String,
    byte_size: i64,
    sha256_hex: String,
    project_id: Option<Uuid>,
    filename: Option<String>,
    kind: Option<String>,
    source: Option<String>,
    received_at: Option<String>,
    description: Option<String>,
    visibility: String,
    slug: Option<String>,
    published_at: Option<String>,
    source_message_id: Option<String>,
    source_sender: Option<String>,
    source_received_at: Option<String>,
    source_subject: Option<String>,
    metadata: Option<Json>,
    derived_from: Option<Json>,
    transcript_quality: Option<String>,
    signing: Option<Json>,
    notarization: Option<Json>,
    deleted_at: Option<DateTime<Utc>>,
    purge_after: Option<DateTime<Utc>>,
    deleted_by_person_id: Option<Uuid>,
    deleted_commit_sha: Option<String>,
    legal_hold_at: Option<DateTime<Utc>>,
    purged_at: Option<DateTime<Utc>>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

/// A link that is set must name a native UUID key; one that does not has no
/// PostgreSQL identity and makes the row unmirrorable.
fn link(record: Option<&surrealdb::types::RecordId>) -> Result<Option<Uuid>, MirrorError> {
    record
        .map(|record| record_uuid(record).ok_or(MirrorError::Source))
        .transpose()
}

fn instant(at: Option<surrealdb::types::Datetime>) -> Option<DateTime<Utc>> {
    at.map(|at| micros(at.into()))
}

/// A JSON column whose value is JSON `null` reads as absent in both stores.
fn json(value: Option<Json>) -> Option<Json> {
    value.filter(|value| !value.is_null())
}

impl AssetImage {
    fn from_source(row: AssetSource) -> Result<Self, MirrorError> {
        Ok(Self {
            id: record_uuid(&row.id).ok_or(MirrorError::Source)?,
            storage_key: row.storage_key,
            secondary_storage_key: row.secondary_storage_key,
            content_type: row.content_type,
            byte_size: row.byte_size,
            sha256_hex: row.sha256_hex,
            project_id: link(row.project_id.as_ref())?,
            filename: row.filename,
            kind: row.kind,
            source: row.source,
            received_at: row.received_at,
            description: row.description,
            visibility: row.visibility,
            slug: row.slug,
            published_at: row.published_at,
            source_message_id: row.source_message_id,
            source_sender: row.source_sender,
            source_received_at: row.source_received_at,
            source_subject: row.source_subject,
            metadata: json(row.metadata),
            derived_from: json(row.derived_from),
            transcript_quality: row.transcript_quality,
            signing: json(row.signing),
            notarization: json(row.notarization),
            deleted_at: instant(row.deleted_at),
            purge_after: instant(row.purge_after),
            deleted_by_person_id: link(row.deleted_by_person_id.as_ref())?,
            deleted_commit_sha: row.deleted_commit_sha,
            legal_hold_at: instant(row.legal_hold_at),
            purged_at: instant(row.purged_at),
            inserted_at: micros(row.inserted_at.into()),
            updated_at: micros(row.updated_at.into()),
        })
    }

    fn from_row(row: &sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        let jsonb = |column: &str| -> Result<Option<Json>, sqlx::Error> {
            let value: Option<Jsonb<Json>> = row.try_get(column)?;
            Ok(json(value.map(|value| value.0)))
        };
        Ok(Self {
            id: row.try_get("id")?,
            storage_key: row.try_get("storage_key")?,
            secondary_storage_key: row.try_get("secondary_storage_key")?,
            content_type: row.try_get("content_type")?,
            byte_size: row.try_get("byte_size")?,
            sha256_hex: row.try_get("sha256_hex")?,
            project_id: row.try_get("project_id")?,
            filename: row.try_get("filename")?,
            kind: row.try_get("kind")?,
            source: row.try_get("source")?,
            received_at: row.try_get("received_at")?,
            description: row.try_get("description")?,
            visibility: row.try_get("visibility")?,
            slug: row.try_get("slug")?,
            published_at: row.try_get("published_at")?,
            source_message_id: row.try_get("source_message_id")?,
            source_sender: row.try_get("source_sender")?,
            source_received_at: row.try_get("source_received_at")?,
            source_subject: row.try_get("source_subject")?,
            metadata: jsonb("metadata")?,
            derived_from: jsonb("derived_from")?,
            transcript_quality: row.try_get("transcript_quality")?,
            signing: jsonb("signing")?,
            notarization: jsonb("notarization")?,
            deleted_at: row.try_get("deleted_at")?,
            purge_after: row.try_get("purge_after")?,
            deleted_by_person_id: row.try_get("deleted_by_person_id")?,
            deleted_commit_sha: row.try_get("deleted_commit_sha")?,
            legal_hold_at: row.try_get("legal_hold_at")?,
            purged_at: row.try_get("purged_at")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

async fn source_asset(surreal: &SurrealDb, id: Uuid) -> Result<Option<AssetSource>, MirrorError> {
    let mut response = surreal
        .query(format!(
            "SELECT {ASSET_SOURCE_FIELDS} FROM ONLY $id LIMIT 1"
        ))
        .bind(("id", record_id(crate::assets::TABLE, id)))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(source)?;
    response.take(0).map_err(source)
}

async fn source_assets(surreal: &SurrealDb) -> Result<Vec<AssetSource>, MirrorError> {
    let mut response = surreal
        .query(format!(
            "SELECT {ASSET_SOURCE_FIELDS} FROM {}",
            crate::assets::TABLE
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(source)?;
    response.take(0).map_err(source)
}

const PROJECT_PRESENT: &str = "SELECT EXISTS (SELECT 1 FROM project WHERE id = $1)";

/// Make PostgreSQL's copy of one asset row match SurrealDB's.
pub(super) async fn mirror(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    key: &MirrorKey,
) -> Result<Mirrored, MirrorError> {
    let MirrorKey::Asset(id) = key else {
        // `super::mirror` routes only asset keys here.
        return Err(MirrorError::Source);
    };
    let Some(row) = source_asset(surreal, *id).await? else {
        let deleted = sqlx::query("DELETE FROM asset WHERE id = $1")
            .bind(id)
            .execute(&pg.0)
            .await?;
        return Ok(changed(deleted.rows_affected(), Mirrored::Deleted));
    };
    let image = AssetImage::from_source(row)?;
    if let Some(project) = image.project_id {
        let held: bool = sqlx::query_scalar(PROJECT_PRESENT)
            .bind(project)
            .fetch_one(&pg.0)
            .await?;
        if !held {
            super::tenancy::mirror(surreal, pg, &MirrorKey::Project(project)).await?;
        }
    }
    if let Some(person) = image.deleted_by_person_id {
        identity::ensure(surreal, pg, MirrorKey::Person(person)).await?;
    }
    upsert_asset(pg, &image).await
}

async fn upsert_asset(pg: &PostgresDb, asset: &AssetImage) -> Result<Mirrored, MirrorError> {
    let written = sqlx::query(ASSET_UPSERT.as_str())
        .bind(asset.id)
        .bind(&asset.storage_key)
        .bind(&asset.secondary_storage_key)
        .bind(&asset.content_type)
        .bind(asset.byte_size)
        .bind(&asset.sha256_hex)
        .bind(asset.project_id)
        .bind(&asset.filename)
        .bind(&asset.kind)
        .bind(&asset.source)
        .bind(&asset.received_at)
        .bind(&asset.description)
        .bind(&asset.visibility)
        .bind(&asset.slug)
        .bind(&asset.published_at)
        .bind(&asset.source_message_id)
        .bind(&asset.source_sender)
        .bind(&asset.source_received_at)
        .bind(&asset.source_subject)
        .bind(asset.metadata.as_ref().map(Jsonb))
        .bind(asset.derived_from.as_ref().map(Jsonb))
        .bind(&asset.transcript_quality)
        .bind(asset.signing.as_ref().map(Jsonb))
        .bind(asset.notarization.as_ref().map(Jsonb))
        .bind(asset.deleted_at)
        .bind(asset.purge_after)
        .bind(asset.deleted_by_person_id)
        .bind(&asset.deleted_commit_sha)
        .bind(asset.legal_hold_at)
        .bind(asset.purged_at)
        .bind(asset.inserted_at)
        .bind(asset.updated_at)
        .execute(&pg.0)
        .await?;
    Ok(changed(written.rows_affected(), Mirrored::Written))
}

/// Compare every asset row in both stores.
///
/// # Errors
///
/// Either store could not be read.
pub async fn parity(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let mut assets: BTreeMap<MirrorKey, Option<AssetImage>> = BTreeMap::new();
    for row in source_assets(surreal).await? {
        // A record whose key is not a native UUID has no PostgreSQL identity.
        let Some(id) = record_uuid(&row.id) else {
            continue;
        };
        assets.insert(MirrorKey::Asset(id), AssetImage::from_source(row).ok());
    }
    let tables = vec![compare(
        "asset",
        &assets,
        &target(pg, ASSET_SELECT.as_str(), AssetImage::from_row, |row| {
            MirrorKey::Asset(row.id)
        })
        .await?,
    )];
    Ok(ParityReport { tables })
}

/// Re-mirror every divergent asset row from SurrealDB, then compare again.
/// A row that fails both passes stays in the returned report.
///
/// # Errors
///
/// Either store could not be read for the comparison.
pub async fn reconcile(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let before = parity(surreal, pg).await?;
    remirror(surreal, pg, &before).await;
    parity(surreal, pg).await
}

#[cfg(test)]
#[path = "documents_tests.rs"]
mod tests;

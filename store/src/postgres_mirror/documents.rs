//! Asset rows in the PostgreSQL mirror: document assets and bare content
//! assets alike.
//!
//! Existing PostgreSQL rows are preserved. The frozen scoped import inserts
//! source-only identities and compares all columns against the native union.
//! Independently verified onboarding metadata may retain its native visibility,
//! timestamp, and verification marker; unexplained differences block activation.
//! Completed enrollment refuses legacy imports before reading Source rows.
//!
//! The row is read raw rather than through [`crate::assets::Asset`]: the
//! JSON columns (`metadata`, `derived_from`, `signing`, `notarization`,
//! `evidence`) are
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

use super::tenancy::{compare, remirror, select_sql, source, target};
use super::{changed, identity, micros, MirrorError, MirrorKey, Mirrored, ParityReport};
use crate::postgres::PostgresDb;
use crate::postgres_backfill::family::{self, ImportMode};
use crate::surreal::{record_id, record_uuid, SurrealDb};

pub(super) const ASSET_COLUMNS: &[&str] = &[
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
    "evidence",
    "folder",
    "deleted_at",
    "purge_after",
    "deleted_by_person_id",
    "deleted_commit_sha",
    "legal_hold_at",
    "purged_at",
    "inserted_at",
    "updated_at",
];

static ASSET_INSERT: LazyLock<String> = LazyLock::new(|| {
    let values = (1..=ASSET_COLUMNS.len())
        .map(|index| format!("${index}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "INSERT INTO asset ({}) VALUES ({values}) ON CONFLICT (id) DO NOTHING",
        ASSET_COLUMNS.join(", ")
    )
});
static ASSET_SELECT: LazyLock<String> = LazyLock::new(|| select_sql("asset", ASSET_COLUMNS));
static COMPAT_ASSET_SELECT: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{} WHERE kind IS DISTINCT FROM 'onboarding'",
        ASSET_SELECT.as_str()
    )
});

/// The Surreal projection, every column the mirror copies.
const ASSET_SOURCE_FIELDS: &str = "id, storage_key, secondary_storage_key, content_type, \
    byte_size, sha256_hex, project_id, filename, kind, source, received_at, description, \
    visibility, slug, published_at, source_message_id, source_sender, source_received_at, \
    source_subject, metadata, derived_from, transcript_quality, signing, notarization, evidence, \
    folder, deleted_at, purge_after, deleted_by_person_id, deleted_commit_sha, legal_hold_at, purged_at, \
    inserted_at, updated_at, {metadata:metadata IS NONE, derived_from:derived_from IS NONE, \
    signing:signing IS NONE, notarization:notarization IS NONE, evidence:evidence IS NONE} AS json_none";

/// The asset row as SurrealDB stores it.
#[derive(SurrealValue)]
struct AssetSource {
    json_none: BTreeMap<String, bool>,
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
    evidence: Option<Json>,
    folder: Option<String>,
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
    evidence: Option<Json>,
    folder: Option<String>,
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

/// Surreal's typed optional projection collapses NULL and NONE; the raw
/// presence projection keeps explicit caller JSON null distinct from absence.
fn source_json(
    value: Option<Json>,
    presence: &BTreeMap<String, bool>,
    field: &str,
) -> Result<Option<Json>, MirrorError> {
    let absent = presence.get(field).ok_or(MirrorError::Source)?;
    Ok((!absent).then(|| value.unwrap_or(Json::Null)))
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
            metadata: source_json(row.metadata, &row.json_none, "metadata")?,
            derived_from: source_json(row.derived_from, &row.json_none, "derived_from")?,
            transcript_quality: row.transcript_quality,
            signing: source_json(row.signing, &row.json_none, "signing")?,
            notarization: source_json(row.notarization, &row.json_none, "notarization")?,
            evidence: source_json(row.evidence, &row.json_none, "evidence")?,
            folder: row.folder,
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
            Ok(value.map(|value| value.0))
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
            evidence: jsonb("evidence")?,
            folder: row.try_get("folder")?,
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
    let mode = family::import_mode(pg, "asset").await?;
    if mode == ImportMode::Refuse {
        return Err(MirrorError::MatterAuthority);
    }
    let row = source_asset(surreal, *id)
        .await?
        .ok_or(MirrorError::Source)?;
    let held: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM asset WHERE id=$1)")
        .bind(id)
        .fetch_one(pg.pool())
        .await?;
    if held {
        return Ok(Mirrored::Unchanged);
    }
    let mut image = AssetImage::from_source(row)?;
    if let Some(metadata) = image.metadata.as_mut().and_then(Json::as_object_mut) {
        metadata.remove(crate::assets::VERIFIED_ONBOARDING_ENVELOPE_KEY);
    }
    if mode == ImportMode::InsertMissing {
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
    }
    upsert_asset(pg, &image).await
}

async fn upsert_asset(pg: &PostgresDb, asset: &AssetImage) -> Result<Mirrored, MirrorError> {
    let written = sqlx::query(ASSET_INSERT.as_str())
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
        .bind(asset.evidence.as_ref().map(Jsonb))
        .bind(&asset.folder)
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
    if crate::postgres_backfill::family::active_for_table(pg, "asset").await? {
        return Ok(ParityReport { tables: Vec::new() });
    }
    if crate::postgres_backfill::family::importing("asset") {
        let (table, _) = scoped_parity(surreal, pg).await?;
        return Ok(ParityReport {
            tables: vec![table],
        });
    }
    let mut assets: BTreeMap<MirrorKey, Option<AssetImage>> = BTreeMap::new();
    for row in source_assets(surreal).await? {
        // A record whose key is not a native UUID has no PostgreSQL identity.
        let Some(id) = record_uuid(&row.id) else {
            continue;
        };
        if row.kind.as_deref() != Some("onboarding") {
            assets.insert(MirrorKey::Asset(id), AssetImage::from_source(row).ok());
        }
    }
    let tables = vec![compare(
        "asset",
        &assets,
        &target(
            pg,
            COMPAT_ASSET_SELECT.as_str(),
            AssetImage::from_row,
            |row| MirrorKey::Asset(row.id),
        )
        .await?,
    )];
    Ok(ParityReport { tables })
}

pub(crate) use crate::postgres_backfill::family::AssetUnionProof;

async fn native_onboarding(pg: &PostgresDb, row: &AssetImage) -> Result<bool, MirrorError> {
    if row.kind.as_deref() != Some("onboarding") {
        return Ok(false);
    }
    let Some(project) = row.project_id else {
        return Ok(false);
    };
    let Some(envelope) = crate::assets::verified_onboarding_envelope(row.metadata.as_ref()) else {
        return Ok(false);
    };
    let parents: std::collections::BTreeSet<_> = [
        Some(row.storage_key.as_str()),
        row.secondary_storage_key.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter_map(crate::postgres::assets::notation_parent)
    .collect();
    // A receipt that names only the Project cannot prove a Note-scoped Asset.
    // Each scoped key must resolve to the completed envelope on that same Note.
    if !parents.is_empty() {
        for notation in parents {
            let proven: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM signature s JOIN notation n ON n.id=s.notation_id \
                 JOIN template t ON t.id=n.template_id JOIN project p ON p.id=n.project_id \
                 WHERE n.id=$1 AND n.project_id=$2 AND t.kind='onboarding' \
                 AND s.provider='docusign' AND s.provider_id=$3 AND s.state='completed' \
                 AND s.signed_at IS NOT NULL)",
            )
            .bind(notation)
            .bind(project)
            .bind(envelope.to_string())
            .fetch_one(pg.pool())
            .await?;
            if !proven {
                return Ok(false);
            }
        }
        return Ok(true);
    }
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM signature s JOIN notation n ON n.id=s.notation_id \
         JOIN template t ON t.id=n.template_id JOIN project p ON p.id=n.project_id \
         WHERE n.project_id=$1 AND t.kind='onboarding' AND s.provider='docusign' \
         AND s.provider_id=$2 AND s.state='completed' AND s.signed_at IS NOT NULL) \
         OR EXISTS(SELECT 1 FROM docusign_completion c JOIN project p ON p.id=c.project_id \
         WHERE c.project_id=$1 AND c.envelope_id=$3 AND c.status='filed' AND c.firm_id=p.firm_id)",
    )
    .bind(project)
    .bind(envelope.to_string())
    .bind(envelope)
    .fetch_one(pg.pool())
    .await?)
}

/// Whether independently native Asset ownership explains a target-only reset fact.
pub(crate) async fn native_protects(pg: &PostgresDb, notation: Uuid) -> Result<bool, MirrorError> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "{} WHERE starts_with(storage_key,$1) OR starts_with(secondary_storage_key,$1)",
        ASSET_SELECT.as_str()
    )))
    .bind(format!("notations/{notation}/"))
    .fetch_all(pg.pool())
    .await?;
    for row in rows {
        if native_onboarding(pg, &AssetImage::from_row(&row)?).await? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn resolved_native_difference(source: &AssetImage, target: &AssetImage) -> bool {
    let mut retained = target.clone();
    retained.visibility.clone_from(&source.visibility);
    retained.updated_at = source.updated_at;
    if let Some(metadata) = retained.metadata.as_mut().and_then(Json::as_object_mut) {
        metadata.remove(crate::assets::VERIFIED_ONBOARDING_ENVELOPE_KEY);
    }
    // Source verification markers are never trusted or imported.
    let mut source = source.clone();
    if let Some(metadata) = source.metadata.as_mut().and_then(Json::as_object_mut) {
        metadata.remove(crate::assets::VERIFIED_ONBOARDING_ENVELOPE_KEY);
    }
    retained == source
}

async fn compare_union(
    pg: &PostgresDb,
    source: BTreeMap<Uuid, AssetImage>,
    target: BTreeMap<Uuid, AssetImage>,
) -> Result<(super::TableParity, AssetUnionProof), MirrorError> {
    let mut proof = AssetUnionProof {
        full_metadata_compared: true,
        ..Default::default()
    };
    let mut divergent = Vec::new();
    for (id, row) in &source {
        match target.get(id) {
            None => {
                proof.source_only_remaining += 1;
                divergent.push(MirrorKey::Asset(*id));
            }
            Some(held) => {
                proof.compared_overlaps += 1;
                if row != held {
                    if resolved_native_difference(row, held) && native_onboarding(pg, held).await? {
                        proof.resolved_native_overlaps += 1;
                    } else {
                        proof.unresolved_overlaps += 1;
                        divergent.push(MirrorKey::Asset(*id));
                    }
                }
            }
        }
    }
    for (id, row) in &target {
        if !source.contains_key(id) {
            if native_onboarding(pg, row).await? {
                proof.proven_native_target_only += 1;
            } else {
                proof.unknown_target_only += 1;
                divergent.push(MirrorKey::Asset(*id));
            }
        }
    }
    divergent.sort();
    Ok((
        super::TableParity {
            table: "asset",
            source_rows: source.len(),
            target_rows: target.len(),
            divergent,
        },
        proof,
    ))
}

/// Asset references share the existing preservation proof without enrolling
/// Asset in the generic row writer, which cannot apply its native union policy.
pub(crate) const REFERENCES: &[(&str, &str)] = &[
    ("project_id", "project"),
    ("deleted_by_person_id", "person"),
    ("storage_key", "notation"),
    ("secondary_storage_key", "notation"),
];

pub(crate) async fn scoped_references(
    surreal: &SurrealDb,
    pg: &PostgresDb,
) -> Result<Vec<super::records::UnresolvedReference>, MirrorError> {
    let mut source_parents = BTreeMap::new();
    let mut target_parents = BTreeMap::new();
    for parent in ["project", "person", "notation"] {
        let source =
            super::select_table(surreal, parent, format!("SELECT VALUE id FROM `{parent}`"))
                .await?;
        let source = source
            .iter()
            .map(|id| {
                // Decode the RecordId itself rather than trusting a printable key.
                let surrealdb::types::Value::RecordId(id) = id else {
                    return Err(MirrorError::Source);
                };
                record_uuid(id).ok_or(MirrorError::Source)
            })
            .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
        source_parents.insert(parent, source);
        let target: Vec<Uuid> =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT id FROM {parent}")))
                .fetch_all(pg.pool())
                .await?;
        target_parents.insert(
            parent,
            target
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>(),
        );
    }
    let mut counts: BTreeMap<&str, super::records::UnresolvedReference> = BTreeMap::new();
    for row in source_assets(surreal).await? {
        let row = AssetImage::from_source(row)?;
        for (field, parent, id) in [
            ("project_id", "project", row.project_id),
            ("deleted_by_person_id", "person", row.deleted_by_person_id),
            (
                "storage_key",
                "notation",
                crate::postgres::assets::notation_parent(&row.storage_key),
            ),
            (
                "secondary_storage_key",
                "notation",
                row.secondary_storage_key
                    .as_deref()
                    .and_then(crate::postgres::assets::notation_parent),
            ),
        ] {
            let Some(id) = id else { continue };
            let source_has = source_parents[parent].contains(&id);
            let target_has = target_parents[parent].contains(&id);
            if !source_has || !target_has {
                counts
                    .entry(field)
                    .or_insert(super::records::UnresolvedReference {
                        table: "asset",
                        field,
                        source_orphans: 0,
                        postgres_missing: 0,
                        newly_target_missing: 0,
                    })
                    .observe(source_has, target_has);
            }
        }
    }
    Ok(counts.into_values().collect())
}

/// Compare the complete frozen Source against the preserved native Asset union.
/// Malformed identities and decoding errors fail the handoff rather than vanish.
pub(crate) async fn scoped_parity(
    surreal: &SurrealDb,
    pg: &PostgresDb,
) -> Result<(super::TableParity, AssetUnionProof), MirrorError> {
    let mut images = BTreeMap::new();
    for row in source_assets(surreal).await? {
        let row = AssetImage::from_source(row)?;
        if images.insert(row.id, row).is_some() {
            return Err(MirrorError::Source);
        }
    }
    let targets = sqlx::query(ASSET_SELECT.as_str())
        .fetch_all(pg.pool())
        .await?
        .iter()
        .map(AssetImage::from_row)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|row| (row.id, row))
        .collect();
    compare_union(pg, images, targets).await
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

#[cfg(test)]
#[path = "asset_union_tests.rs"]
mod union_tests;

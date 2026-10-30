//! Versioned Template metadata in the PostgreSQL mirror.
//!
//! The markdown body stays in its content-addressed Asset. This table keeps
//! each version's identity, scope, provenance, and pins. `current_key` is
//! copied exactly from SurrealDB: it is present only for the current version
//! and its unique index distinguishes shared, Firm Library, and
//! Project-scoped codes.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use chrono::{DateTime, SubsecRound, Utc};
use sqlx::Row;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use super::tenancy::{compare, select_sql, target, upsert_sql};
use super::{changed, MirrorError, MirrorKey, Mirrored, ParityReport};
use crate::postgres::PostgresDb;
use crate::surreal::{record_id, record_uuid, SurrealDb};

const COLUMNS: &[&str] = &[
    "id",
    "code",
    "title",
    "respondent_type",
    "project_id",
    "firm_id",
    "is_current",
    "asset_id",
    "form_code",
    "kind",
    "source_commit_sha",
    "body_sha256",
    "current_key",
    "inserted_at",
    "updated_at",
];
const SOURCE_FIELDS: &str = "id, code, title, respondent_type, project_id, firm_id, is_current, \
    asset_id, form_code, kind, source_commit_sha, body_sha256, current_key, inserted_at, updated_at";
static UPSERT: LazyLock<String> = LazyLock::new(|| upsert_sql("template", COLUMNS));
static SELECT: LazyLock<String> = LazyLock::new(|| select_sql("template", COLUMNS));

#[derive(SurrealValue)]
struct TemplateSource {
    id: surrealdb::types::RecordId,
    code: String,
    title: String,
    respondent_type: String,
    project_id: Option<surrealdb::types::RecordId>,
    firm_id: Option<surrealdb::types::RecordId>,
    is_current: bool,
    asset_id: Option<surrealdb::types::RecordId>,
    form_code: Option<String>,
    kind: Option<String>,
    source_commit_sha: Option<String>,
    body_sha256: Option<String>,
    current_key: Option<String>,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TemplateImage {
    id: Uuid,
    code: String,
    title: String,
    respondent_type: String,
    project_id: Option<Uuid>,
    firm_id: Option<Uuid>,
    is_current: bool,
    asset_id: Option<Uuid>,
    form_code: Option<String>,
    kind: Option<String>,
    source_commit_sha: Option<String>,
    body_sha256: Option<String>,
    current_key: Option<String>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

fn optional_id(id: Option<surrealdb::types::RecordId>) -> Result<Option<Uuid>, MirrorError> {
    id.map(|record| record_uuid(&record).ok_or(MirrorError::Source))
        .transpose()
}

impl TemplateImage {
    fn from_source(row: TemplateSource) -> Result<Self, MirrorError> {
        let inserted_at: DateTime<Utc> = row.inserted_at.into();
        let updated_at: DateTime<Utc> = row.updated_at.into();
        Ok(Self {
            id: record_uuid(&row.id).ok_or(MirrorError::Source)?,
            code: row.code,
            title: row.title,
            respondent_type: row.respondent_type,
            project_id: optional_id(row.project_id)?,
            firm_id: optional_id(row.firm_id)?,
            is_current: row.is_current,
            asset_id: optional_id(row.asset_id)?,
            form_code: row.form_code,
            kind: row.kind,
            source_commit_sha: row.source_commit_sha,
            body_sha256: row.body_sha256,
            current_key: row.current_key,
            inserted_at: inserted_at.trunc_subsecs(6),
            updated_at: updated_at.trunc_subsecs(6),
        })
    }

    fn from_row(row: &sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            code: row.try_get("code")?,
            title: row.try_get("title")?,
            respondent_type: row.try_get("respondent_type")?,
            project_id: row.try_get("project_id")?,
            firm_id: row.try_get("firm_id")?,
            is_current: row.try_get("is_current")?,
            asset_id: row.try_get("asset_id")?,
            form_code: row.try_get("form_code")?,
            kind: row.try_get("kind")?,
            source_commit_sha: row.try_get("source_commit_sha")?,
            body_sha256: row.try_get("body_sha256")?,
            current_key: row.try_get("current_key")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

async fn source_one(surreal: &SurrealDb, id: Uuid) -> Result<Option<TemplateImage>, MirrorError> {
    let mut response = surreal
        .query(format!("SELECT {SOURCE_FIELDS} FROM ONLY $id LIMIT 1"))
        .bind(("id", record_id(crate::templates::TABLE, id)))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?;
    let row: Option<TemplateSource> = response.take(0).map_err(|_| MirrorError::Source)?;
    row.map(TemplateImage::from_source).transpose()
}

async fn source_all(surreal: &SurrealDb) -> Result<Vec<TemplateImage>, MirrorError> {
    let mut response = surreal
        .query(format!("SELECT {SOURCE_FIELDS} FROM template ORDER BY id"))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?;
    let rows: Vec<TemplateSource> = response.take(0).map_err(|_| MirrorError::Source)?;
    rows.into_iter().map(TemplateImage::from_source).collect()
}

async fn ensure_parent(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    exists_sql: &'static str,
    id: Uuid,
    key: MirrorKey,
) -> Result<(), MirrorError> {
    let exists: bool = sqlx::query_scalar(exists_sql)
        .bind(id)
        .fetch_one(&pg.0)
        .await?;
    if !exists {
        Box::pin(super::mirror(surreal, pg, &key)).await?;
    }
    Ok(())
}

async fn upsert(pg: &PostgresDb, image: &TemplateImage) -> Result<Mirrored, MirrorError> {
    let result = sqlx::query(UPSERT.as_str())
        .bind(image.id)
        .bind(&image.code)
        .bind(&image.title)
        .bind(&image.respondent_type)
        .bind(image.project_id)
        .bind(image.firm_id)
        .bind(image.is_current)
        .bind(image.asset_id)
        .bind(&image.form_code)
        .bind(&image.kind)
        .bind(&image.source_commit_sha)
        .bind(&image.body_sha256)
        .bind(&image.current_key)
        .bind(image.inserted_at)
        .bind(image.updated_at)
        .execute(&pg.0)
        .await?;
    Ok(changed(result.rows_affected(), Mirrored::Written))
}

/// Make PostgreSQL's Template row match SurrealDB, mirroring its parents on
/// demand. The source row and its key are read again after every invocation.
pub async fn mirror(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    id: Uuid,
) -> Result<Mirrored, MirrorError> {
    let Some(image) = source_one(surreal, id).await? else {
        let deleted = sqlx::query("DELETE FROM template WHERE id = $1")
            .bind(id)
            .execute(&pg.0)
            .await?;
        return Ok(changed(deleted.rows_affected(), Mirrored::Deleted));
    };
    if let Some(project_id) = image.project_id {
        ensure_parent(
            surreal,
            pg,
            "SELECT EXISTS (SELECT 1 FROM project WHERE id = $1)",
            project_id,
            MirrorKey::Project(project_id),
        )
        .await?;
    }
    if let Some(firm_id) = image.firm_id {
        ensure_parent(
            surreal,
            pg,
            "SELECT EXISTS (SELECT 1 FROM firm WHERE id = $1)",
            firm_id,
            MirrorKey::Firm(firm_id),
        )
        .await?;
    }
    if let Some(asset_id) = image.asset_id {
        ensure_parent(
            surreal,
            pg,
            "SELECT EXISTS (SELECT 1 FROM asset WHERE id = $1)",
            asset_id,
            MirrorKey::Asset(asset_id),
        )
        .await?;
    }
    upsert(pg, &image).await
}

/// Compare every Template version in both stores.
pub async fn parity(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let source: BTreeMap<_, _> = source_all(surreal)
        .await?
        .into_iter()
        .map(|image| (MirrorKey::Template(image.id), Some(image)))
        .collect();
    let target: BTreeMap<_, _> = target(pg, SELECT.as_str(), TemplateImage::from_row, |image| {
        MirrorKey::Template(image.id)
    })
    .await?;
    Ok(ParityReport {
        tables: vec![compare("template", &source, &target)],
    })
}

/// Repair template rows which differ, then return the remaining parity gaps.
pub async fn reconcile(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let before = parity(surreal, pg).await?;
    let source_images: BTreeMap<_, _> = source_all(surreal)
        .await?
        .into_iter()
        .map(|image| (MirrorKey::Template(image.id), image))
        .collect();
    let target_images = target(pg, SELECT.as_str(), TemplateImage::from_row, |image| {
        MirrorKey::Template(image.id)
    })
    .await?;

    // A target-only current row can occupy the current key of a source row.
    // Leave both rows visible as drift instead of deleting either one.
    let mut key_conflicts = std::collections::BTreeSet::new();
    for (target_key, target_image) in &target_images {
        if source_images.contains_key(target_key) {
            continue;
        }
        let Some(current_key) = &target_image.current_key else {
            continue;
        };
        if let Some((source_key, _)) = source_images
            .iter()
            .find(|(_, image)| image.is_current && image.current_key.as_ref() == Some(current_key))
        {
            key_conflicts.insert(target_key.clone());
            key_conflicts.insert(source_key.clone());
        }
    }

    // Retired versions first: they free a source-owned `current_key` before
    // its replacement is upserted, while keeping both version rows intact.
    let mut divergent: Vec<_> = before.divergent().cloned().collect();
    divergent.sort_by_key(|key| {
        let rank = match source_images.get(key) {
            Some(image) if !image.is_current => 0,
            Some(_) => 1,
            None => 2,
        };
        (rank, key.clone())
    });
    for key in &divergent {
        if key_conflicts.contains(key) {
            tracing::warn!(key = %key, "PostgreSQL reconcile left a current-key conflict untouched for review");
            continue;
        }
        if let MirrorKey::Template(id) = key {
            if let Err(error) = mirror(surreal, pg, *id).await {
                tracing::warn!(key = %key, error = %error, "PostgreSQL reconcile could not mirror the row");
            }
        }
    }
    parity(surreal, pg).await
}

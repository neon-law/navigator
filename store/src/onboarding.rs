//! PostgreSQL execution evidence and document filing for onboarding.
use std::{collections::HashSet, sync::Arc};

use cloud::StorageService;
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use crate::{
    assets::{Asset, AssetError, ExpectedHead, Filed, RevisionError},
    documents::{DocumentIdentity, IngestArgs, IngestedDocument},
    postgres::PostgresDb,
};

fn database_error(error: impl std::fmt::Display) -> AssetError {
    AssetError::Postgres(error.to_string())
}

/// Projects with a completed DocuSign UUID in authoritative PostgreSQL evidence.
pub async fn official_projects(
    pg: &PostgresDb,
    projects: &[Uuid],
) -> Result<HashSet<Uuid>, sqlx::Error> {
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT n.project_id, s.provider_id FROM signature s
         JOIN notation n ON n.id=s.notation_id JOIN template t ON t.id=n.template_id
         WHERE n.project_id=ANY($1) AND t.kind='onboarding'
           AND s.provider='docusign' AND s.state='completed' AND s.signed_at IS NOT NULL
         UNION
         SELECT project_id, metadata->>'docusign_envelope_id' FROM asset
         WHERE project_id=ANY($1) AND kind='onboarding' AND deleted_at IS NULL
           AND metadata->>'verified_onboarding_envelope_id'=metadata->>'docusign_envelope_id'",
    )
    .bind(projects)
    .fetch_all(pg.pool())
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(project, envelope)| {
            Uuid::parse_str(&envelope)
                .ok()
                .filter(|id| !id.is_nil())
                .map(|_| project)
        })
        .collect())
}

/// Filed opening and closing documents for batched lifecycle reads.
pub async fn filed_documents(
    pg: &PostgresDb,
    projects: &[Uuid],
) -> Result<(HashSet<Uuid>, HashSet<Uuid>), sqlx::Error> {
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT project_id,kind FROM asset
         WHERE project_id=ANY($1) AND deleted_at IS NULL AND kind IN ('onboarding','offboarding')
         UNION SELECT n.project_id,t.kind FROM notation n JOIN template t ON t.id=n.template_id
         JOIN notation_document d ON d.notation_id=n.id
         WHERE n.project_id=ANY($1) AND t.kind IN ('onboarding','offboarding')",
    )
    .bind(projects)
    .fetch_all(pg.pool())
    .await?;
    let mut opening = HashSet::new();
    let mut closing = HashSet::new();
    for (project, kind) in rows {
        if kind == "onboarding" {
            opening.insert(project);
        } else {
            closing.insert(project);
        }
    }
    Ok((opening, closing))
}

fn decode(row: &sqlx::postgres::PgRow) -> Result<Asset, AssetError> {
    let mut value: Value = row.try_get("row").map_err(database_error)?;
    if value["notarization"].is_null() {
        value["notarization"] = serde_json::json!([]);
    }
    serde_json::from_value(value).map_err(database_error)
}

/// PostgreSQL revisions, ordered from the current head.
pub async fn revisions(
    pg: &PostgresDb,
    project: Uuid,
    slug: &str,
) -> Result<Vec<Asset>, AssetError> {
    sqlx::query(
        "SELECT to_jsonb(a) AS row FROM asset a WHERE project_id=$1 AND slug=$2 ORDER BY id DESC",
    )
    .bind(project)
    .bind(slug)
    .fetch_all(pg.pool())
    .await
    .map_err(database_error)?
    .iter()
    .map(decode)
    .collect()
}

/// The Project's live PostgreSQL documents.
pub async fn documents(pg: &PostgresDb, project: Uuid) -> Result<Vec<Asset>, AssetError> {
    sqlx::query("SELECT to_jsonb(a) AS row FROM asset a WHERE project_id=$1 AND deleted_at IS NULL ORDER BY id DESC")
        .bind(project).fetch_all(pg.pool()).await.map_err(database_error)?
        .iter().map(decode).collect()
}

/// File onboarding bytes and execution evidence without a compatibility-store write.
pub async fn file_document(
    pg: &PostgresDb,
    storage: &Arc<dyn StorageService>,
    args: &IngestArgs<'_>,
    identity: &DocumentIdentity<'_>,
    expected: Option<&ExpectedHead>,
    bytes: &[u8],
) -> Result<Filed, RevisionError> {
    crate::document_pointers::MatterFolder::parse_checked_folder(identity.folder)
        .map_err(crate::documents::IngestError::from)?;
    let slug = identity.slug.unwrap_or(args.filename);
    let sha = crate::documents::sha256_hex(bytes);
    let mut tx = pg.pool().begin().await.map_err(database_error)?;
    // The Project row serializes concurrent appends, including an empty chain.
    let code: String = sqlx::query_scalar("SELECT code FROM project WHERE id=$1 FOR UPDATE")
        .bind(args.project_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(database_error)?;
    let rows = sqlx::query(
        "SELECT to_jsonb(a) AS row FROM asset a WHERE project_id=$1 AND slug=$2 ORDER BY id DESC",
    )
    .bind(args.project_id)
    .bind(slug)
    .fetch_all(&mut *tx)
    .await
    .map_err(database_error)?;
    let chain = rows.iter().map(decode).collect::<Result<Vec<_>, _>>()?;
    if let Some(head) = chain.first() {
        if head.kind.as_deref().is_some_and(|kind| kind != args.kind) {
            return Err(RevisionError::KindChanged {
                slug: slug.into(),
                existing: head.kind.clone().unwrap_or_default(),
                attempted: args.kind.into(),
            });
        }
        let evidence = crate::assets::verified_onboarding_envelope(identity.metadata.as_ref());
        if head.deleted_at.is_none()
            && head.sha256_hex == sha
            && (evidence.is_none()
                || crate::assets::verified_onboarding_envelope(head.metadata.as_ref()) == evidence)
        {
            return Ok(Filed::Unchanged { asset_id: head.id });
        }
    }
    if let Some(expected) = expected {
        let current = chain.first().map(|head| ExpectedHead {
            asset_id: head.id,
            version: u32::try_from(chain.len()).unwrap_or(u32::MAX),
            sha256: head.sha256_hex.clone(),
        });
        if current.as_ref() != Some(expected) {
            return Err(RevisionError::HeadConflict {
                slug: slug.into(),
                current,
            });
        }
    }
    let storage_key = format!("{}/{sha}", cloud::workspace::documents_prefix(&code));
    storage
        .put(&storage_key, bytes, args.content_type)
        .await
        .map_err(crate::documents::IngestError::from)?;
    let id = Uuid::now_v7();
    let size = i64::try_from(bytes.len()).unwrap_or(i64::MAX);
    let document = IngestedDocument {
        asset_id: id,
        storage_key,
        sha256_hex: sha,
        byte_size: size,
        reused: false,
    };
    insert_document(&mut tx, args, identity, &document).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(Filed::Revision(document))
}

async fn insert_document(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    args: &IngestArgs<'_>,
    identity: &DocumentIdentity<'_>,
    document: &IngestedDocument,
) -> Result<(), AssetError> {
    sqlx::query(
        "INSERT INTO asset
         (id,project_id,storage_key,content_type,byte_size,sha256_hex,filename,kind,source,
          description,visibility,slug,metadata,signing,notarization,evidence,folder,received_at,
          secondary_storage_key,published_at,derived_from)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21)",
    )
    .bind(document.asset_id)
    .bind(args.project_id)
    .bind(&document.storage_key)
    .bind(args.content_type)
    .bind(document.byte_size)
    .bind(&document.sha256_hex)
    .bind(args.filename)
    .bind(args.kind)
    .bind(args.source)
    .bind(args.description)
    .bind(args.visibility)
    .bind(identity.slug.unwrap_or(args.filename))
    .bind(&identity.metadata)
    .bind(
        identity
            .signing
            .map(serde_json::to_value)
            .transpose()
            .map_err(database_error)?,
    )
    .bind(serde_json::to_value(&identity.notarization).map_err(database_error)?)
    .bind(
        identity
            .evidence
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(database_error)?,
    )
    .bind(identity.folder)
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(args.secondary_storage_key)
    .bind(identity.published_at)
    .bind(&identity.derived_from)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    sqlx::query("UPDATE asset SET visibility='internal',updated_at=now() WHERE project_id=$1 AND slug=$2 AND id<>$3 AND visibility<>'internal'")
        .bind(args.project_id).bind(identity.slug.unwrap_or(args.filename)).bind(document.asset_id).execute(&mut **tx).await.map_err(database_error)?;
    Ok(())
}

#[cfg(test)]
mod tests;

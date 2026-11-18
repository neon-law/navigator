//! PostgreSQL execution evidence and document filing for onboarding.
use std::{collections::HashSet, sync::Arc};

use cloud::StorageService;
use uuid::Uuid;

use crate::{
    assets::{Asset, AssetError, ExpectedHead, Filed, RevisionError},
    documents::{DocumentIdentity, IngestArgs},
    postgres::PostgresDb,
};

/// Projects with a completed DocuSign UUID in authoritative PostgreSQL evidence.
pub async fn official_projects(
    pg: &PostgresDb,
    projects: &[Uuid],
) -> Result<HashSet<Uuid>, sqlx::Error> {
    crate::postgres_backfill::family::require_scope(
        pg,
        crate::postgres_backfill::family::Family::Notation,
    )
    .await
    .map_err(|error| sqlx::Error::Configuration(Box::new(error)))?;
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
    crate::postgres_backfill::family::require_scope(
        pg,
        crate::postgres_backfill::family::Family::Notation,
    )
    .await
    .map_err(|error| sqlx::Error::Configuration(Box::new(error)))?;
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

/// PostgreSQL revisions, ordered from the current head.
pub async fn revisions(
    pg: &PostgresDb,
    project: Uuid,
    slug: &str,
) -> Result<Vec<Asset>, AssetError> {
    crate::postgres::assets::rows(
        pg,
        "AND r.project_id=($1->>'project')::uuid AND r.slug=$1->>'slug' ORDER BY id DESC",
        serde_json::json!({"project":project,"slug":slug}),
    )
    .await
}

/// The Project's live PostgreSQL documents.
pub async fn documents(pg: &PostgresDb, project: Uuid) -> Result<Vec<Asset>, AssetError> {
    crate::postgres::assets::rows(
        pg,
        "AND r.project_id=($1->>'project')::uuid AND r.deleted_at IS NULL ORDER BY id DESC",
        serde_json::json!({"project":project}),
    )
    .await
}

/// File verified onboarding bytes through the canonical Asset transaction.
pub async fn file_document(
    pg: &PostgresDb,
    storage: &Arc<dyn StorageService>,
    args: &IngestArgs<'_>,
    identity: &DocumentIdentity<'_>,
    expected: Option<&ExpectedHead>,
    bytes: &[u8],
) -> Result<Filed, RevisionError> {
    crate::postgres::assets::file_revision(pg, storage, args, identity, None, expected, bytes).await
}

#[cfg(test)]
mod tests;

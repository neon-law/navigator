//! A private, live parent lease for a verified archive's restore rows.

use super::{store_error, ArchiveError, RestoreMetadata};
use crate::postgres::PostgresDb;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub(crate) struct RestoreParents<'a> {
    assets: BTreeMap<Uuid, &'a crate::assets::Asset>,
    documents: BTreeMap<Uuid, &'a crate::notation_documents::NotationDocument>,
    versions: BTreeMap<Uuid, &'a crate::notation_documents::NotationDocumentVersion>,
    pg: PostgresDb,
    transaction: sqlx::Transaction<'static, sqlx::Postgres>,
    protected: BTreeSet<Uuid>,
    missing_notes: BTreeSet<Uuid>,
    missing_documents: BTreeSet<Uuid>,
}

impl RestoreParents<'_> {
    pub(crate) fn verifies_asset(&self, row: &crate::assets::Asset) -> bool {
        self.assets
            .get(&row.id)
            .is_some_and(|verified| *verified == row)
    }
    pub(crate) fn verifies_document(
        &self,
        row: &crate::notation_documents::NotationDocument,
    ) -> bool {
        self.documents
            .get(&row.id)
            .is_some_and(|verified| *verified == row)
    }
    pub(crate) fn verifies_version(
        &self,
        row: &crate::notation_documents::NotationDocumentVersion,
    ) -> bool {
        self.versions
            .get(&row.id)
            .is_some_and(|verified| *verified == row)
    }
    pub(crate) fn database(&self) -> &PostgresDb {
        &self.pg
    }
    pub(crate) fn permits_note(&self, id: Uuid) -> bool {
        self.protected.contains(&id) || self.missing_notes.contains(&id)
    }
    pub(crate) fn permits_missing_document(&self, id: Uuid) -> bool {
        self.missing_documents.contains(&id)
    }
    pub(super) async fn finish(self) -> Result<(), ArchiveError> {
        self.transaction.commit().await.map_err(store_error)
    }
}

/// Called only after the archive bytes and row set have been verified.
pub(super) async fn acquire<'a>(
    pg: &PostgresDb,
    metadata: &'a RestoreMetadata,
) -> Result<RestoreParents<'a>, ArchiveError> {
    let required: BTreeSet<_> = metadata.notations.iter().map(|row| row.id).collect();
    let mut candidates = required.clone();
    for asset in &metadata.assets {
        for key in [
            Some(asset.storage_key.as_str()),
            asset.secondary_storage_key.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(parent) = crate::postgres::assets::notation_parent(key) {
                candidates.insert(parent);
            }
        }
    }
    candidates.extend(
        metadata
            .notation_documents
            .iter()
            .map(|row| row.notation_id),
    );
    let documents: BTreeSet<_> = metadata
        .notation_documents
        .iter()
        .map(|row| row.id)
        .collect();
    let historical_documents: BTreeSet<_> = metadata
        .notation_document_versions
        .iter()
        .map(|row| row.document_id)
        .filter(|id| !documents.contains(id))
        .collect();
    let held: Vec<(Uuid, Uuid)> =
        sqlx::query_as("SELECT id,notation_id FROM notation_document WHERE id=ANY($1)")
            .bind(historical_documents.iter().copied().collect::<Vec<_>>())
            .fetch_all(pg.pool())
            .await
            .map_err(store_error)?;
    let held_ids: BTreeSet<_> = held.iter().map(|row| row.0).collect();
    candidates.extend(held.into_iter().map(|row| row.1));
    let missing_documents = historical_documents
        .difference(&held_ids)
        .copied()
        .collect();
    acquire_set(pg, metadata, &required, &candidates, missing_documents).await
}

async fn acquire_set<'a>(
    pg: &PostgresDb,
    metadata: &'a RestoreMetadata,
    required: &BTreeSet<Uuid>,
    candidates: &BTreeSet<Uuid>,
    missing_documents: BTreeSet<Uuid>,
) -> Result<RestoreParents<'a>, ArchiveError> {
    let mut transaction = pg.pool().begin().await.map_err(store_error)?;
    let mut protected = BTreeSet::new();
    let mut missing_notes = BTreeSet::new();
    for id in candidates {
        let found: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM notation WHERE id=$1 FOR KEY SHARE")
                .bind(id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(store_error)?;
        if found.is_some() {
            protected.insert(*id);
        } else if required.contains(id) {
            return Err(ArchiveError::RestorePrerequisite("a restored Notation"));
        } else {
            missing_notes.insert(*id);
        }
    }
    Ok(RestoreParents {
        assets: metadata.assets.iter().map(|row| (row.id, row)).collect(),
        documents: metadata
            .notation_documents
            .iter()
            .map(|row| (row.id, row))
            .collect(),
        versions: metadata
            .notation_document_versions
            .iter()
            .map(|row| (row.id, row))
            .collect(),
        pg: pg.clone(),
        transaction,
        protected,
        missing_notes,
        missing_documents,
    })
}

#[cfg(test)]
#[path = "restore_parents_tests.rs"]
mod tests;

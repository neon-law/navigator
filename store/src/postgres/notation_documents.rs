//! Canonical Notation Document writes under the reset's parent lock.

use chrono::{SubsecRound, Utc};
use serde::de::DeserializeOwned;
use sqlx::Row;
use uuid::Uuid;

use super::{notations::lock_parent, PostgresDb};
use crate::assets::RowRestore;
use crate::notation_documents::{NotationDocument, NotationDocumentError, NotationDocumentVersion};

fn decode<T: DeserializeOwned>(row: &sqlx::postgres::PgRow) -> Result<T, NotationDocumentError> {
    serde_json::from_value(row.try_get("row")?)
        .map_err(|_| NotationDocumentError::WriteReturnedNothing)
}

pub(crate) async fn create(
    pg: &PostgresDb,
    notation_id: Uuid,
    original_asset_id: Uuid,
) -> Result<NotationDocument, NotationDocumentError> {
    let mut tx = pg.pool().begin().await?;
    lock_parent(&mut tx, notation_id).await?;
    sqlx::query("INSERT INTO notation_document(id,notation_id,original_asset_id,inserted_at,updated_at) VALUES($1,$2,$3,now(),now()) ON CONFLICT(notation_id) DO NOTHING")
        .bind(Uuid::now_v7()).bind(notation_id).bind(original_asset_id).execute(&mut *tx).await?;
    let row =
        sqlx::query("SELECT to_jsonb(d) AS row FROM notation_document d WHERE notation_id=$1")
            .bind(notation_id)
            .fetch_one(&mut *tx)
            .await?;
    let document: NotationDocument = decode(&row)?;
    if document.original_asset_id != original_asset_id {
        return Err(NotationDocumentError::OriginalAssetMismatch {
            notation_id,
            existing: document.original_asset_id,
            attempted: original_asset_id,
        });
    }
    tx.commit().await?;
    Ok(document)
}

pub(crate) async fn restore_document(
    pg: &PostgresDb,
    archived: &NotationDocument,
) -> Result<RowRestore, NotationDocumentError> {
    restore_document_inner(pg, archived, None).await
}

pub(crate) async fn restore_document_history(
    parents: &crate::project_archives::restore_parents::RestoreParents<'_>,
    archived: &NotationDocument,
) -> Result<RowRestore, NotationDocumentError> {
    if !parents.verifies_document(archived) {
        return Err(NotationDocumentError::WriteReturnedNothing);
    }
    restore_document_inner(parents.database(), archived, Some(parents)).await
}

async fn restore_document_inner(
    pg: &PostgresDb,
    archived: &NotationDocument,
    parents: Option<&crate::project_archives::restore_parents::RestoreParents<'_>>,
) -> Result<RowRestore, NotationDocumentError> {
    let mut expected = archived.clone();
    expected.inserted_at = expected.inserted_at.trunc_subsecs(6);
    expected.updated_at = expected.updated_at.trunc_subsecs(6);
    let mut tx = pg.pool().begin().await?;
    if let Some(parents) = parents {
        if !parents.permits_note(expected.notation_id) {
            return Err(NotationDocumentError::WriteReturnedNothing);
        }
    } else {
        lock_parent(&mut tx, expected.notation_id).await?;
    }

    let created=sqlx::query("INSERT INTO notation_document SELECT * FROM jsonb_populate_record(NULL::notation_document,$1) ON CONFLICT DO NOTHING RETURNING id")
        .bind(sqlx::types::Json(&expected)).fetch_optional(&mut *tx).await?.is_some();
    let result = if created {
        RowRestore::Created
    } else {
        let row = sqlx::query("SELECT to_jsonb(d) AS row FROM notation_document d WHERE id=$1")
            .bind(expected.id)
            .fetch_optional(&mut *tx)
            .await?;
        if row
            .as_ref()
            .map(decode::<NotationDocument>)
            .transpose()?
            .as_ref()
            == Some(&expected)
        {
            RowRestore::Identical
        } else {
            RowRestore::Conflict
        }
    };
    tx.commit().await?;
    Ok(result)
}

pub(crate) async fn restore_version(
    pg: &PostgresDb,
    archived: &NotationDocumentVersion,
) -> Result<RowRestore, NotationDocumentError> {
    restore_version_inner(pg, archived, None).await
}

pub(crate) async fn restore_version_history(
    parents: &crate::project_archives::restore_parents::RestoreParents<'_>,
    archived: &NotationDocumentVersion,
) -> Result<RowRestore, NotationDocumentError> {
    if !parents.verifies_version(archived) {
        return Err(NotationDocumentError::WriteReturnedNothing);
    }
    restore_version_inner(parents.database(), archived, Some(parents)).await
}

async fn restore_version_inner(
    pg: &PostgresDb,
    archived: &NotationDocumentVersion,
    parents: Option<&crate::project_archives::restore_parents::RestoreParents<'_>>,
) -> Result<RowRestore, NotationDocumentError> {
    let mut expected = archived.clone();
    expected.inserted_at = expected.inserted_at.trunc_subsecs(6);
    expected.updated_at = expected.updated_at.trunc_subsecs(6);
    let mut tx = pg.pool().begin().await?;
    if let Some(parents) = parents {
        if !parents.permits_missing_document(expected.document_id) {
            let notation: Uuid =
                sqlx::query_scalar("SELECT notation_id FROM notation_document WHERE id=$1")
                    .bind(expected.document_id)
                    .fetch_one(&mut *tx)
                    .await?;
            if !parents.permits_note(notation) {
                return Err(NotationDocumentError::WriteReturnedNothing);
            }
        }
    } else {
        let notation: Uuid =
            sqlx::query_scalar("SELECT notation_id FROM notation_document WHERE id=$1")
                .bind(expected.document_id)
                .fetch_one(&mut *tx)
                .await?;
        lock_parent(&mut tx, notation).await?;
    }
    let created=sqlx::query("INSERT INTO notation_document_version SELECT * FROM jsonb_populate_record(NULL::notation_document_version,$1) ON CONFLICT(id) DO NOTHING RETURNING id")
        .bind(sqlx::types::Json(&expected)).fetch_optional(&mut *tx).await?.is_some();
    let result = if created {
        RowRestore::Created
    } else {
        let row =
            sqlx::query("SELECT to_jsonb(v) AS row FROM notation_document_version v WHERE id=$1")
                .bind(expected.id)
                .fetch_one(&mut *tx)
                .await?;
        if decode::<NotationDocumentVersion>(&row)? == expected {
            RowRestore::Identical
        } else {
            RowRestore::Conflict
        }
    };
    tx.commit().await?;
    Ok(result)
}

/// The losing CAS keeps its immutable version; only the current pointer is conditional.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn append(
    pg: &PostgresDb,
    document: &NotationDocument,
    expected_parent: Option<Uuid>,
    markdown_asset_id: Uuid,
    anchor_manifest_asset_id: Uuid,
    parser_version: &str,
    schema_version: i64,
    authored_by_person_id: Uuid,
    status: &str,
) -> Result<NotationDocumentVersion, NotationDocumentError> {
    let mut tx = pg.pool().begin().await?;
    lock_parent(&mut tx, document.notation_id).await?;
    let now = Utc::now();
    let version = NotationDocumentVersion {
        id: Uuid::now_v7(),
        document_id: document.id,
        parent_version_id: expected_parent,
        markdown_asset_id,
        anchor_manifest_asset_id,
        parser_version: parser_version.into(),
        schema_version,
        authored_by_person_id,
        status: status.into(),
        inserted_at: now,
        updated_at: now,
    };
    let row=sqlx::query("INSERT INTO notation_document_version SELECT * FROM jsonb_populate_record(NULL::notation_document_version,$1) RETURNING to_jsonb(notation_document_version) AS row")
        .bind(sqlx::types::Json(&version)).fetch_one(&mut *tx).await?;
    let version: NotationDocumentVersion = decode(&row)?;
    let changed=sqlx::query("UPDATE notation_document SET current_version_id=$2,updated_at=now() WHERE id=$1 AND notation_id=$4 AND current_version_id IS NOT DISTINCT FROM $3")
        .bind(document.id).bind(version.id).bind(expected_parent).bind(document.notation_id).execute(&mut *tx).await?.rows_affected();
    let current: Option<Uuid> =
        sqlx::query_scalar("SELECT current_version_id FROM notation_document WHERE id=$1")
            .bind(document.id)
            .fetch_one(&mut *tx)
            .await?;
    tx.commit().await?;
    if changed == 0 {
        return Err(NotationDocumentError::Conflict {
            expected: expected_parent,
            actual: current,
        });
    }
    Ok(version)
}

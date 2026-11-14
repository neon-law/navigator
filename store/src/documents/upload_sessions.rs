//! The PostgreSQL `document_upload_session` table, the Upload Session's system
//! of record whenever PostgreSQL is configured. Chunk bytes stay in object
//! storage; the row holds the declared identity and the resume point.

use chrono::{DateTime, Utc};
use sqlx::{postgres::PgRow, Row};
use uuid::Uuid;

use super::{
    UploadSession, UploadSessionArgs, UploadSessionError, UploadSessionRow, UPLOAD_SESSION_SELECT,
    UPLOAD_SESSION_TABLE as TABLE,
};
use crate::postgres::PostgresDb;
use crate::postgres_backfill::Copied;
use crate::surreal::SurrealDb;

fn session(row: &PgRow) -> Result<UploadSession, sqlx::Error> {
    Ok(UploadSession {
        id: row.try_get("id")?,
        project_id: row.try_get("project_id")?,
        filename: row.try_get("filename")?,
        kind: row.try_get("kind")?,
        content_type: row.try_get("content_type")?,
        visibility: row.try_get("visibility")?,
        description: row.try_get("description")?,
        slug: row.try_get("slug")?,
        transcript_quality: row.try_get("transcript_quality")?,
        metadata: row.try_get("metadata")?,
        derived_from: row.try_get("derived_from")?,
        signing: row.try_get("signing")?,
        notarization: row.try_get("notarization")?,
        folder: row.try_get("folder")?,
        declared_size: row.try_get("declared_size")?,
        declared_sha256: row.try_get("declared_sha256")?,
        chunk_size: row.try_get("chunk_size")?,
        received_bytes: row.try_get("received_bytes")?,
        created_at: row.try_get("created_at")?,
        expires_at: row.try_get("expires_at")?,
    })
}

/// Insert a session as written, keeping its id. `ON CONFLICT (id) DO NOTHING`
/// makes a copy rerun inert; a fresh session's id is always new.
async fn insert(pg: &PostgresDb, row: &UploadSession) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO {TABLE} (id, project_id, filename, kind, content_type, visibility, \
         description, slug, transcript_quality, metadata, derived_from, signing, notarization, \
         folder, declared_size, declared_sha256, chunk_size, received_bytes, created_at, \
         expires_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, \
         $16, $17, $18, $19, $20) ON CONFLICT (id) DO NOTHING"
    )))
    .bind(row.id)
    .bind(row.project_id)
    .bind(&row.filename)
    .bind(&row.kind)
    .bind(&row.content_type)
    .bind(&row.visibility)
    .bind(&row.description)
    .bind(&row.slug)
    .bind(&row.transcript_quality)
    .bind(&row.metadata)
    .bind(&row.derived_from)
    .bind(&row.signing)
    .bind(&row.notarization)
    .bind(&row.folder)
    .bind(row.declared_size)
    .bind(&row.declared_sha256)
    .bind(row.chunk_size)
    .bind(row.received_bytes)
    .bind(row.created_at)
    .bind(row.expires_at)
    .execute(&pg.0)
    .await?
    .rows_affected())
}

/// Store a new session.
pub(super) async fn create(pg: &PostgresDb, row: &UploadSession) -> Result<(), UploadSessionError> {
    insert(pg, row).await?;
    Ok(())
}

/// The live session this exact upload already has, newest first.
pub(super) async fn find_resumable(
    pg: &PostgresDb,
    args: &UploadSessionArgs<'_>,
    now: DateTime<Utc>,
) -> Result<Option<UploadSession>, UploadSessionError> {
    Ok(sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {UPLOAD_SESSION_SELECT} FROM {TABLE} WHERE project_id = $1 AND filename = $2 \
         AND declared_size = $3 AND declared_sha256 = $4 AND expires_at > $5 \
         ORDER BY created_at DESC LIMIT 1"
    )))
    .bind(args.project_id)
    .bind(args.filename.trim())
    .bind(i64::try_from(args.declared_size).unwrap_or(i64::MAX))
    .bind(args.declared_sha256)
    .bind(now)
    .fetch_optional(&pg.0)
    .await?
    .as_ref()
    .map(session)
    .transpose()?)
}

/// One session by id, scoped to its matter.
pub(super) async fn find(
    pg: &PostgresDb,
    project_id: Uuid,
    session_id: Uuid,
) -> Result<Option<UploadSession>, UploadSessionError> {
    Ok(sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {UPLOAD_SESSION_SELECT} FROM {TABLE} WHERE id = $1 AND project_id = $2"
    )))
    .bind(session_id)
    .bind(project_id)
    .fetch_optional(&pg.0)
    .await?
    .as_ref()
    .map(session)
    .transpose()?)
}

/// Move the resume point from `offset` to `received_bytes`; `false` when
/// another append already moved it.
pub(super) async fn advance(
    pg: &PostgresDb,
    session_id: Uuid,
    offset: i64,
    received_bytes: i64,
) -> Result<bool, UploadSessionError> {
    Ok(sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE {TABLE} SET received_bytes = $3 WHERE id = $1 AND received_bytes = $2"
    )))
    .bind(session_id)
    .bind(offset)
    .bind(received_bytes)
    .execute(&pg.0)
    .await?
    .rows_affected()
        == 1)
}

/// Remove a finished session's row.
pub(super) async fn remove(pg: &PostgresDb, session_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DELETE FROM {TABLE} WHERE id = $1"
    )))
    .bind(session_id)
    .execute(&pg.0)
    .await?;
    Ok(())
}

/// Copy every SurrealDB session PostgreSQL does not hold, keeping its id and
/// resume point, so an in-flight upload resumes after the switch.
pub(crate) async fn copy_from_surreal(
    surreal: &SurrealDb,
    pg: &PostgresDb,
) -> Result<Copied, surrealdb::Error> {
    let rows: Vec<UploadSessionRow> = surreal
        .query(format!("SELECT {UPLOAD_SESSION_SELECT} FROM {TABLE}"))
        .await
        .and_then(surrealdb::IndexedResults::check)?
        .take(0)?;
    let mut copied = Copied::default();
    for row in rows {
        let Some(row) = row.into_upload_session() else {
            copied.unrepresentable(TABLE);
            continue;
        };
        copied.record(TABLE, Some(row.id), insert(pg, &row).await);
    }
    Ok(copied)
}

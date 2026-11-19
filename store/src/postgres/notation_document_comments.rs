//! Version-scoped comments and atomic review decisions under the Notation lock.
use super::{matter, PostgresDb};
use crate::notation_documents::{
    DecideCommentError, NotationDocumentComment, NotationDocumentError,
};
use chrono::Utc;
use uuid::Uuid;

type Transaction<'a> = sqlx::Transaction<'a, sqlx::Postgres>;

async fn lock_scope(
    tx: &mut Transaction<'_>,
    project: Uuid,
    note: Uuid,
) -> Result<(), NotationDocumentError> {
    let actual: Option<Uuid> =
        sqlx::query_scalar("SELECT project_id FROM notation WHERE id=$1 FOR KEY SHARE")
            .bind(note)
            .fetch_optional(&mut **tx)
            .await?;
    let actual = actual.ok_or(crate::notations::NotationError::NotFound(note))?;
    let belongs = actual == project;
    if !belongs {
        return Err(NotationDocumentError::ProjectMismatch {
            notation_id: note,
            project_id: project,
        });
    }
    Ok(())
}
async fn version_parent(
    tx: &mut Transaction<'_>,
    note: Uuid,
    version: Uuid,
) -> Result<Option<Option<Uuid>>, NotationDocumentError> {
    Ok(sqlx::query_scalar("SELECT v.parent_version_id FROM notation_document_version v JOIN notation_document d ON d.id=v.document_id WHERE v.id=$1 AND d.notation_id=$2")
        .bind(version).bind(note).fetch_optional(&mut **tx).await?)
}
fn decode(value: serde_json::Value) -> Result<NotationDocumentComment, NotationDocumentError> {
    serde_json::from_value(value)
        .map_err(matter::MatterAuthorityError::from)
        .map_err(Into::into)
}
async fn comments_on(
    tx: &mut Transaction<'_>,
    note: Uuid,
    version: Uuid,
) -> Result<Vec<NotationDocumentComment>, NotationDocumentError> {
    let rows:Vec<serde_json::Value>=sqlx::query_scalar("SELECT to_jsonb(c) FROM notation_document_comment c JOIN notation_document_version v ON v.id=c.version_id JOIN notation_document d ON d.id=v.document_id WHERE d.notation_id=$1 AND v.id=$2 ORDER BY c.inserted_at,c.id")
        .bind(note).bind(version).fetch_all(&mut **tx).await?;
    rows.into_iter().map(decode).collect()
}
async fn insert(
    tx: &mut Transaction<'_>,
    comment: &NotationDocumentComment,
) -> Result<NotationDocumentComment, NotationDocumentError> {
    let row=sqlx::query_scalar("INSERT INTO notation_document_comment AS c(id,version_id,anchor,person_id,body,decision,resolved,inserted_at,updated_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING to_jsonb(c)")
        .bind(comment.id).bind(comment.version_id).bind(&comment.anchor).bind(comment.person_id).bind(&comment.body).bind(&comment.decision).bind(comment.resolved).bind(comment.inserted_at).bind(comment.updated_at).fetch_one(&mut **tx).await?;
    decode(row)
}
pub(crate) async fn add(
    pg: &PostgresDb,
    project: Uuid,
    note: Uuid,
    mut comment: NotationDocumentComment,
) -> Result<Uuid, NotationDocumentError> {
    let mut tx = pg.pool().begin().await?;
    lock_scope(&mut tx, project, note).await?;
    version_parent(&mut tx, note, comment.version_id)
        .await?
        .ok_or(NotationDocumentError::VersionNotFound(comment.version_id))?;
    comment.inserted_at = Utc::now();
    comment.updated_at = comment.inserted_at;
    let id = insert(&mut tx, &comment).await?.id;
    tx.commit().await?;
    Ok(id)
}
pub(crate) async fn list(
    pg: &PostgresDb,
    project: Uuid,
    note: Uuid,
    version: Uuid,
) -> Result<Vec<NotationDocumentComment>, NotationDocumentError> {
    let mut tx = pg.pool().begin().await?;
    lock_scope(&mut tx, project, note).await?;
    let rows = comments_on(&mut tx, note, version).await?;
    tx.commit().await?;
    Ok(rows)
}
pub(crate) async fn carry(
    pg: &PostgresDb,
    project: Uuid,
    note: Uuid,
    parent: Uuid,
    child: Uuid,
    anchors: &[String],
) -> Result<Vec<NotationDocumentComment>, NotationDocumentError> {
    let mut tx = pg.pool().begin().await?;
    lock_scope(&mut tx, project, note).await?;
    let actual = version_parent(&mut tx, note, child)
        .await?
        .ok_or(NotationDocumentError::VersionNotFound(child))?;
    if actual != Some(parent) {
        return Err(NotationDocumentError::Conflict {
            expected: Some(parent),
            actual,
        });
    }
    version_parent(&mut tx, note, parent)
        .await?
        .ok_or(NotationDocumentError::VersionNotFound(parent))?;
    let mut carried = Vec::new();
    for mut comment in comments_on(&mut tx, note, parent).await? {
        comment.id = Uuid::now_v7();
        comment.version_id = child;
        comment.resolved = anchors.contains(&comment.anchor);
        if !comment.resolved {
            comment.decision = None;
        }
        comment.inserted_at = Utc::now();
        comment.updated_at = comment.inserted_at;
        carried.push(insert(&mut tx, &comment).await?);
    }
    tx.commit().await?;
    Ok(carried)
}
pub(crate) async fn decide(
    pg: &PostgresDb,
    project: Uuid,
    record: crate::notation_events::TransitionRecord<'_>,
    comment_id: Uuid,
) -> Result<(), DecideCommentError> {
    let mut tx = pg
        .pool()
        .begin()
        .await
        .map_err(NotationDocumentError::from)?;
    lock_scope(&mut tx, project, record.notation_id).await?;
    let row:Option<serde_json::Value>=sqlx::query_scalar("SELECT to_jsonb(c) FROM notation_document_comment c JOIN notation_document_version v ON v.id=c.version_id JOIN notation_document d ON d.id=v.document_id WHERE c.id=$1 AND d.notation_id=$2 FOR UPDATE OF c")
        .bind(comment_id).bind(record.notation_id).fetch_optional(&mut *tx).await.map_err(NotationDocumentError::from)?;
    let mut comment = row
        .map(decode)
        .transpose()?
        .ok_or(DecideCommentError::NotFound(comment_id))?;
    if comment.decision.is_some() {
        return Err(DecideCommentError::AlreadyDecided(comment_id));
    }
    comment.decision = Some(record.to_state.to_owned());
    comment.resolved = true;
    comment.updated_at = Utc::now();
    let changed=sqlx::query("UPDATE notation_document_comment SET decision=$2,resolved=true,updated_at=$3 WHERE id=$1 AND decision IS NULL").bind(comment.id).bind(&comment.decision).bind(comment.updated_at).execute(&mut *tx).await.map_err(NotationDocumentError::from)?.rows_affected();
    if changed != 1 {
        return Err(DecideCommentError::AlreadyDecided(comment_id));
    }
    let payload=serde_json::json!({"comment_id":comment.id,"version_id":comment.version_id,"anchor":comment.anchor,"decision":record.to_state}).to_string();
    crate::notation_events::append_on(
        &mut tx,
        crate::notation_events::TransitionRecord {
            payload_json: Some(payload),
            ..record
        },
    )
    .await
    .map_err(NotationDocumentError::from)?;
    tx.commit().await.map_err(NotationDocumentError::from)?;
    Ok(())
}
#[cfg(test)]
#[path = "notation_document_comments_tests.rs"]
mod tests;

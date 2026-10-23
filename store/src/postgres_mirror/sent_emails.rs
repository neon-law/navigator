//! Append-only PostgreSQL projection of outbound email audit rows.
//!
//! Reconciliation inserts missing records. Changed or extra PostgreSQL rows
//! remain visible as drift; audit history is never overwritten or removed.

use super::{changed, micros, Columns, MirrorError, MirrorKey, Mirrored, ParityReport};
use crate::{
    postgres::PostgresDb,
    surreal::{record_uuid, SurrealDb},
};
use chrono::{DateTime, Utc};
use sqlx::{postgres::PgRow, Row};
use std::collections::BTreeMap;
use surrealdb::types::{Datetime, RecordId, SurrealValue};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SentEmailImage {
    id: Uuid,
    recipient: String,
    subject: String,
    sender: String,
    project_id: Option<Uuid>,
    template_slug: Option<String>,
    body: String,
    outcome: String,
    sg_message_id: Option<String>,
    sent_at: DateTime<Utc>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}
#[derive(SurrealValue)]
struct Source {
    id: RecordId,
    recipient: String,
    subject: String,
    sender: String,
    project_id: Option<RecordId>,
    template_slug: Option<String>,
    body: String,
    outcome: String,
    sg_message_id: Option<String>,
    sent_at: Datetime,
    inserted_at: Datetime,
    updated_at: Datetime,
}
impl TryFrom<Source> for SentEmailImage {
    type Error = MirrorError;
    fn try_from(row: Source) -> Result<Self, Self::Error> {
        Ok(Self {
            id: record_uuid(&row.id).ok_or(MirrorError::Source)?,
            recipient: row.recipient,
            subject: row.subject,
            sender: row.sender,
            project_id: row
                .project_id
                .as_ref()
                .map(|id| record_uuid(id).ok_or(MirrorError::Source))
                .transpose()?,
            template_slug: row.template_slug,
            body: row.body,
            outcome: row.outcome,
            sg_message_id: row.sg_message_id,
            sent_at: micros(row.sent_at.into()),
            inserted_at: micros(row.inserted_at.into()),
            updated_at: micros(row.updated_at.into()),
        })
    }
}
const SELECT: &str = "id, recipient, subject, sender, project_id, template_slug, body, outcome, sg_message_id, sent_at, inserted_at, updated_at";
async fn source(surreal: &SurrealDb) -> Result<BTreeMap<MirrorKey, Columns>, MirrorError> {
    let rows: Vec<Source> = surreal
        .query(format!("SELECT {SELECT} FROM sent_email"))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?
        .take(0)
        .map_err(|_| MirrorError::Source)?;
    rows.into_iter()
        .map(|row| {
            let image = SentEmailImage::try_from(row)?;
            Ok((
                MirrorKey::SentEmail(image.id),
                Columns::SentEmail(Box::new(image)),
            ))
        })
        .collect()
}
async fn source_one(surreal: &SurrealDb, id: Uuid) -> Result<Option<SentEmailImage>, MirrorError> {
    let mut response = surreal
        .query(format!("SELECT {SELECT} FROM ONLY $id LIMIT 1"))
        .bind(("id", crate::surreal::record_id("sent_email", id)))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?;
    let row: Option<Source> = response.take(0).map_err(|_| MirrorError::Source)?;
    row.map(SentEmailImage::try_from).transpose()
}
fn from_row(row: &PgRow) -> Result<SentEmailImage, sqlx::Error> {
    Ok(SentEmailImage {
        id: row.try_get("id")?,
        recipient: row.try_get("recipient")?,
        subject: row.try_get("subject")?,
        sender: row.try_get("sender")?,
        project_id: row.try_get("project_id")?,
        template_slug: row.try_get("template_slug")?,
        body: row.try_get("body")?,
        outcome: row.try_get("outcome")?,
        sg_message_id: row.try_get("sg_message_id")?,
        sent_at: row.try_get("sent_at")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}
async fn target(pg: &PostgresDb) -> Result<BTreeMap<MirrorKey, Columns>, MirrorError> {
    sqlx::query("SELECT id, recipient, subject, sender, project_id, template_slug, body, outcome, sg_message_id, sent_at, inserted_at, updated_at FROM sent_email")
        .fetch_all(&pg.0)
        .await?
        .into_iter()
        .map(|row| {
            let image = from_row(&row)?;
            Ok((
                MirrorKey::SentEmail(image.id),
                Columns::SentEmail(Box::new(image)),
            ))
        })
        .collect()
}
/// Insert a missing audit row without changing any existing history.
pub async fn mirror(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    id: Uuid,
) -> Result<Mirrored, MirrorError> {
    let Some(row) = source_one(surreal, id).await? else {
        return Ok(Mirrored::Unchanged);
    };
    if let Some(project_id) = row.project_id {
        let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM project WHERE id=$1)")
            .bind(project_id)
            .fetch_one(&pg.0)
            .await?;
        if !exists {
            super::tenancy::mirror(surreal, pg, &MirrorKey::Project(project_id)).await?;
        }
    }
    let result = sqlx::query("INSERT INTO sent_email (id, recipient, subject, sender, project_id, template_slug, body, outcome, sg_message_id, sent_at, inserted_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) ON CONFLICT (id) DO NOTHING")
        .bind(row.id)
        .bind(row.recipient)
        .bind(row.subject)
        .bind(row.sender)
        .bind(row.project_id)
        .bind(row.template_slug)
        .bind(row.body)
        .bind(row.outcome)
        .bind(row.sg_message_id)
        .bind(row.sent_at)
        .bind(row.inserted_at)
        .bind(row.updated_at)
        .execute(&pg.0).await?;
    Ok(changed(result.rows_affected(), Mirrored::Written))
}
/// Compare all audit fields without exposing message values in the report.
pub async fn parity(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    Ok(ParityReport {
        tables: vec![super::compare(
            "sent_email",
            &source(surreal).await?,
            &target(pg).await?,
        )],
    })
}
/// Fill absent audit records and report all remaining divergence.
pub async fn reconcile(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let source = source(surreal).await?;
    let target = target(pg).await?;
    for key in source.keys().filter(|key| !target.contains_key(*key)) {
        if let MirrorKey::SentEmail(id) = key {
            mirror(surreal, pg, *id).await?;
        }
    }
    parity(surreal, pg).await
}
#[cfg(test)]
#[path = "sent_emails_tests.rs"]
mod tests;

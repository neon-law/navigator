//! PostgreSQL projection for Project-scoped Deadlines.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use sqlx::{postgres::PgRow, Row};
use surrealdb::types::{RecordId, SurrealValue};
use uuid::Uuid;

use super::{changed, micros, Columns, MirrorError, MirrorKey, Mirrored, ParityReport};
use crate::{
    postgres::PostgresDb,
    surreal::{record_uuid, SurrealDb},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DeadlineImage {
    id: Uuid,
    project_id: Uuid,
    title: Option<String>,
    description: Option<String>,
    due_at: DateTime<Utc>,
    authority_kind: String,
    authority: String,
    source: String,
    source_document_id: Option<Uuid>,
    owner_person_id: Option<Uuid>,
    status: String,
    calendar_role: String,
    internal_lead_days: Option<i64>,
    client_lead_days: Option<i64>,
    replay_key: Option<String>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    calendar_sequence: Option<i64>,
}

#[derive(SurrealValue)]
struct DeadlineSource {
    id: RecordId,
    project_id: RecordId,
    title: Option<String>,
    description: Option<String>,
    due_at: String,
    authority_kind: String,
    authority: String,
    source: String,
    source_document_id: Option<RecordId>,
    owner_person_id: Option<RecordId>,
    status: String,
    calendar_role: String,
    internal_lead_days: Option<i64>,
    client_lead_days: Option<i64>,
    replay_key: Option<String>,
    inserted_at: String,
    updated_at: String,
    calendar_sequence: Option<i64>,
}

fn uuid(id: &RecordId) -> Result<Uuid, MirrorError> {
    record_uuid(id).ok_or(MirrorError::Source)
}
fn optional_uuid(id: Option<&RecordId>) -> Result<Option<Uuid>, MirrorError> {
    id.map(uuid).transpose()
}
fn date(value: &str) -> Result<DateTime<Utc>, MirrorError> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| micros(dt.with_timezone(&Utc)))
        .map_err(|_| MirrorError::SourceTimestamp)
}
impl TryFrom<DeadlineSource> for DeadlineImage {
    type Error = MirrorError;
    fn try_from(row: DeadlineSource) -> Result<Self, Self::Error> {
        Ok(Self {
            id: uuid(&row.id)?,
            project_id: uuid(&row.project_id)?,
            title: row.title,
            description: row.description,
            due_at: date(&row.due_at)?,
            authority_kind: row.authority_kind,
            authority: row.authority,
            source: row.source,
            source_document_id: optional_uuid(row.source_document_id.as_ref())?,
            owner_person_id: optional_uuid(row.owner_person_id.as_ref())?,
            status: row.status,
            calendar_role: row.calendar_role,
            internal_lead_days: row.internal_lead_days,
            client_lead_days: row.client_lead_days,
            replay_key: row.replay_key,
            inserted_at: date(&row.inserted_at)?,
            updated_at: date(&row.updated_at)?,
            calendar_sequence: row.calendar_sequence,
        })
    }
}

const SELECT: &str = "id, project_id, title, description, due_at, authority_kind, authority, source, source_document_id, owner_person_id, status, calendar_role, internal_lead_days, client_lead_days, replay_key, inserted_at, updated_at, calendar_sequence";
async fn source(surreal: &SurrealDb) -> Result<BTreeMap<MirrorKey, Columns>, MirrorError> {
    let rows: Vec<DeadlineSource> = surreal
        .query(format!("SELECT {SELECT} FROM project_deadlines"))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?
        .take(0)
        .map_err(|_| MirrorError::Source)?;
    rows.into_iter()
        .map(|row| {
            let image = DeadlineImage::try_from(row)?;
            Ok((
                MirrorKey::ProjectDeadline(image.id),
                Columns::ProjectDeadline(Box::new(image)),
            ))
        })
        .collect()
}
async fn source_one(surreal: &SurrealDb, id: Uuid) -> Result<Option<DeadlineImage>, MirrorError> {
    let mut response = surreal
        .query(format!("SELECT {SELECT} FROM ONLY $id LIMIT 1"))
        .bind(("id", crate::surreal::record_id("project_deadlines", id)))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?;
    let row: Option<DeadlineSource> = response.take(0).map_err(|_| MirrorError::Source)?;
    row.map(DeadlineImage::try_from).transpose()
}

async fn ensure_parent(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    table: &'static str,
    id: Uuid,
    key: MirrorKey,
) -> Result<(), MirrorError> {
    let exists: bool = match table {
        "project" => {
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM project WHERE id=$1)")
                .bind(id)
                .fetch_one(&pg.0)
                .await?
        }
        "asset" => {
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM asset WHERE id=$1)")
                .bind(id)
                .fetch_one(&pg.0)
                .await?
        }
        "person" => {
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM person WHERE id=$1)")
                .bind(id)
                .fetch_one(&pg.0)
                .await?
        }
        _ => return Err(MirrorError::Source),
    };
    if !exists {
        match key {
            MirrorKey::Project(_) => super::tenancy::mirror(surreal, pg, &key).await?,
            MirrorKey::Asset(_) => super::documents::mirror(surreal, pg, &key).await?,
            MirrorKey::Person(_) => super::identity::mirror(surreal, pg, &key).await?,
            _ => return Err(MirrorError::Source),
        };
    }
    Ok(())
}
async fn target(pg: &PostgresDb) -> Result<BTreeMap<MirrorKey, Columns>, MirrorError> {
    let rows = sqlx::query("SELECT id, project_id, title, description, due_at, authority_kind, authority, source, source_document_id, owner_person_id, status, calendar_role, internal_lead_days, client_lead_days, replay_key, inserted_at, updated_at, calendar_sequence FROM project_deadlines")
        .fetch_all(&pg.0)
        .await?;
    rows.into_iter()
        .map(|row| {
            let image = from_row(&row)?;
            Ok((
                MirrorKey::ProjectDeadline(image.id),
                Columns::ProjectDeadline(Box::new(image)),
            ))
        })
        .collect()
}
fn from_row(row: &PgRow) -> Result<DeadlineImage, sqlx::Error> {
    Ok(DeadlineImage {
        id: row.try_get("id")?,
        project_id: row.try_get("project_id")?,
        title: row.try_get("title")?,
        description: row.try_get("description")?,
        due_at: row.try_get("due_at")?,
        authority_kind: row.try_get("authority_kind")?,
        authority: row.try_get("authority")?,
        source: row.try_get("source")?,
        source_document_id: row.try_get("source_document_id")?,
        owner_person_id: row.try_get("owner_person_id")?,
        status: row.try_get("status")?,
        calendar_role: row.try_get("calendar_role")?,
        internal_lead_days: row.try_get("internal_lead_days")?,
        client_lead_days: row.try_get("client_lead_days")?,
        replay_key: row.try_get("replay_key")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
        calendar_sequence: row.try_get("calendar_sequence")?,
    })
}
async fn upsert(pg: &PostgresDb, row: &DeadlineImage) -> Result<Mirrored, MirrorError> {
    let result = sqlx::query("INSERT INTO project_deadlines (id,project_id,title,due_at,authority_kind,authority,source,source_document_id,owner_person_id,status,calendar_role,internal_lead_days,client_lead_days,replay_key,inserted_at,updated_at,description,calendar_sequence) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18) ON CONFLICT (id) DO UPDATE SET project_id=EXCLUDED.project_id,title=EXCLUDED.title,description=EXCLUDED.description,calendar_sequence=EXCLUDED.calendar_sequence,due_at=EXCLUDED.due_at,authority_kind=EXCLUDED.authority_kind,authority=EXCLUDED.authority,source=EXCLUDED.source,source_document_id=EXCLUDED.source_document_id,owner_person_id=EXCLUDED.owner_person_id,status=EXCLUDED.status,calendar_role=EXCLUDED.calendar_role,internal_lead_days=EXCLUDED.internal_lead_days,client_lead_days=EXCLUDED.client_lead_days,replay_key=EXCLUDED.replay_key,inserted_at=EXCLUDED.inserted_at,updated_at=EXCLUDED.updated_at WHERE (project_deadlines.project_id,project_deadlines.title,project_deadlines.due_at,project_deadlines.authority_kind,project_deadlines.authority,project_deadlines.source,project_deadlines.source_document_id,project_deadlines.owner_person_id,project_deadlines.status,project_deadlines.calendar_role,project_deadlines.internal_lead_days,project_deadlines.client_lead_days,project_deadlines.replay_key,project_deadlines.inserted_at,project_deadlines.updated_at,project_deadlines.description,project_deadlines.calendar_sequence) IS DISTINCT FROM (EXCLUDED.project_id,EXCLUDED.title,EXCLUDED.due_at,EXCLUDED.authority_kind,EXCLUDED.authority,EXCLUDED.source,EXCLUDED.source_document_id,EXCLUDED.owner_person_id,EXCLUDED.status,EXCLUDED.calendar_role,EXCLUDED.internal_lead_days,EXCLUDED.client_lead_days,EXCLUDED.replay_key,EXCLUDED.inserted_at,EXCLUDED.updated_at,EXCLUDED.description,EXCLUDED.calendar_sequence)")
        .bind(row.id).bind(row.project_id).bind(&row.title).bind(row.due_at).bind(&row.authority_kind).bind(&row.authority).bind(&row.source)
        .bind(row.source_document_id).bind(row.owner_person_id).bind(&row.status).bind(&row.calendar_role)
        .bind(row.internal_lead_days).bind(row.client_lead_days).bind(&row.replay_key).bind(micros(row.inserted_at)).bind(micros(row.updated_at)).bind(&row.description).bind(row.calendar_sequence)
        .execute(&pg.0).await?;
    Ok(changed(result.rows_affected(), Mirrored::Written))
}
pub async fn mirror(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    key: &MirrorKey,
) -> Result<Mirrored, MirrorError> {
    let MirrorKey::ProjectDeadline(id) = key else {
        return Err(MirrorError::Source);
    };
    let Some(row) = source_one(surreal, *id).await? else {
        return Ok(changed(
            sqlx::query("DELETE FROM project_deadlines WHERE id=$1")
                .bind(id)
                .execute(&pg.0)
                .await?
                .rows_affected(),
            Mirrored::Deleted,
        ));
    };
    ensure_parent(
        surreal,
        pg,
        "project",
        row.project_id,
        MirrorKey::Project(row.project_id),
    )
    .await?;
    if let Some(asset) = row.source_document_id {
        ensure_parent(surreal, pg, "asset", asset, MirrorKey::Asset(asset)).await?;
    }
    if let Some(person) = row.owner_person_id {
        ensure_parent(surreal, pg, "person", person, MirrorKey::Person(person)).await?;
    }
    upsert(pg, &row).await
}
pub async fn parity(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    Ok(ParityReport {
        tables: vec![super::compare(
            "project_deadlines",
            &source(surreal).await?,
            &target(pg).await?,
        )],
    })
}
pub async fn reconcile(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let a = source(surreal).await?;
    let b = target(pg).await?;
    let keys: Vec<_> = a
        .iter()
        .filter(|(k, v)| b.get(*k) != Some(*v))
        .map(|(k, _)| k.clone())
        .chain(b.keys().filter(|k| !a.contains_key(*k)).cloned())
        .collect();
    for key in keys {
        mirror(surreal, pg, &key).await?;
    }
    parity(surreal, pg).await
}

#[cfg(test)]
#[path = "deadlines_tests.rs"]
mod tests;

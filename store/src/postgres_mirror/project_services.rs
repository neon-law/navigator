//! PostgreSQL mirror for the Project-scoped Service recorded on a served Document.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{postgres::PgRow, Row};
use surrealdb::types::{RecordId, SurrealValue};
use uuid::Uuid;

use super::{changed, micros, Columns, MirrorError, MirrorKey, Mirrored, ParityReport};
use crate::{
    postgres::PostgresDb,
    surreal::{record_uuid, SurrealDb},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProjectServiceImage {
    id: Uuid,
    project_id: Uuid,
    document_id: Uuid,
    recipient: String,
    recipient_person_id: Option<Uuid>,
    recipient_entity_id: Option<Uuid>,
    counsel_for: Option<String>,
    method: String,
    served_on: NaiveDate,
    address: String,
    declarant: String,
    declarant_person_id: Option<Uuid>,
    letter_id: Option<Uuid>,
    sent_email_id: Option<Uuid>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(SurrealValue)]
struct SourceRow {
    id: RecordId,
    project_id: RecordId,
    document_id: RecordId,
    recipient: String,
    recipient_person_id: Option<RecordId>,
    recipient_entity_id: Option<RecordId>,
    counsel_for: Option<String>,
    method: String,
    served_on: String,
    address: String,
    declarant: String,
    declarant_person_id: Option<RecordId>,
    letter_id: Option<RecordId>,
    sent_email_id: Option<RecordId>,
    inserted_at: String,
    updated_at: String,
}

fn uuid(id: &RecordId) -> Result<Uuid, MirrorError> {
    record_uuid(id).ok_or(MirrorError::Source)
}

fn optional_uuid(id: Option<&RecordId>) -> Result<Option<Uuid>, MirrorError> {
    id.map(uuid).transpose()
}

fn timestamp(value: &str) -> Result<DateTime<Utc>, MirrorError> {
    DateTime::parse_from_rfc3339(value)
        .map(|date| micros(date.with_timezone(&Utc)))
        .map_err(|_| MirrorError::SourceTimestamp)
}

impl TryFrom<SourceRow> for ProjectServiceImage {
    type Error = MirrorError;

    fn try_from(row: SourceRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: uuid(&row.id)?,
            project_id: uuid(&row.project_id)?,
            document_id: uuid(&row.document_id)?,
            recipient: row.recipient,
            recipient_person_id: optional_uuid(row.recipient_person_id.as_ref())?,
            recipient_entity_id: optional_uuid(row.recipient_entity_id.as_ref())?,
            counsel_for: row.counsel_for,
            method: row.method,
            served_on: NaiveDate::parse_from_str(&row.served_on, "%Y-%m-%d")
                .map_err(|_| MirrorError::Source)?,
            address: row.address,
            declarant: row.declarant,
            declarant_person_id: optional_uuid(row.declarant_person_id.as_ref())?,
            letter_id: optional_uuid(row.letter_id.as_ref())?,
            sent_email_id: optional_uuid(row.sent_email_id.as_ref())?,
            inserted_at: timestamp(&row.inserted_at)?,
            updated_at: timestamp(&row.updated_at)?,
        })
    }
}

const SELECT: &str = "id, project_id, document_id, recipient, recipient_person_id, \
    recipient_entity_id, counsel_for, method, served_on, address, declarant, \
    declarant_person_id, letter_id, sent_email_id, inserted_at, updated_at";

async fn source(surreal: &SurrealDb) -> Result<BTreeMap<MirrorKey, Columns>, MirrorError> {
    let rows: Vec<SourceRow> = surreal
        .query(format!("SELECT {SELECT} FROM project_service"))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?
        .take(0)
        .map_err(|_| MirrorError::Source)?;
    rows.into_iter()
        .map(|row| {
            let image = ProjectServiceImage::try_from(row)?;
            Ok((
                MirrorKey::ProjectService(image.id),
                Columns::ProjectService(Box::new(image)),
            ))
        })
        .collect()
}

async fn source_one(
    surreal: &SurrealDb,
    id: Uuid,
) -> Result<Option<ProjectServiceImage>, MirrorError> {
    let mut response = surreal
        .query(format!("SELECT {SELECT} FROM ONLY $id LIMIT 1"))
        .bind(("id", crate::surreal::record_id("project_service", id)))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?;
    let row: Option<SourceRow> = response.take(0).map_err(|_| MirrorError::Source)?;
    row.map(ProjectServiceImage::try_from).transpose()
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
        "entity" => {
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM entity WHERE id=$1)")
                .bind(id)
                .fetch_one(&pg.0)
                .await?
        }
        "sent_email" => {
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM sent_email WHERE id=$1)")
                .bind(id)
                .fetch_one(&pg.0)
                .await?
        }
        _ => return Err(MirrorError::Source),
    };
    if !exists {
        match key {
            MirrorKey::Project(_) => {
                super::tenancy::mirror(surreal, pg, &key).await?;
            }
            MirrorKey::Asset(_) => {
                super::documents::mirror(surreal, pg, &key).await?;
            }
            MirrorKey::Person(_) | MirrorKey::Entity(_) => {
                super::identity::mirror(surreal, pg, &key).await?;
            }
            MirrorKey::SentEmail(_) => {
                super::sent_emails::mirror(surreal, pg, id).await?;
            }
            _ => return Err(MirrorError::Source),
        }
    }
    Ok(())
}

async fn target(pg: &PostgresDb) -> Result<BTreeMap<MirrorKey, Columns>, MirrorError> {
    let rows = sqlx::query(
        "SELECT id, project_id, document_id, recipient, recipient_person_id, \
         recipient_entity_id, counsel_for, method, served_on, address, declarant, \
         declarant_person_id, letter_id, sent_email_id, inserted_at, updated_at \
         FROM project_service",
    )
    .fetch_all(&pg.0)
    .await?;
    rows.into_iter()
        .map(|row| {
            let image = from_row(&row)?;
            Ok((
                MirrorKey::ProjectService(image.id),
                Columns::ProjectService(Box::new(image)),
            ))
        })
        .collect()
}

fn from_row(row: &PgRow) -> Result<ProjectServiceImage, sqlx::Error> {
    Ok(ProjectServiceImage {
        id: row.try_get("id")?,
        project_id: row.try_get("project_id")?,
        document_id: row.try_get("document_id")?,
        recipient: row.try_get("recipient")?,
        recipient_person_id: row.try_get("recipient_person_id")?,
        recipient_entity_id: row.try_get("recipient_entity_id")?,
        counsel_for: row.try_get("counsel_for")?,
        method: row.try_get("method")?,
        served_on: row.try_get("served_on")?,
        address: row.try_get("address")?,
        declarant: row.try_get("declarant")?,
        declarant_person_id: row.try_get("declarant_person_id")?,
        letter_id: row.try_get("letter_id")?,
        sent_email_id: row.try_get("sent_email_id")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

async fn upsert(pg: &PostgresDb, row: &ProjectServiceImage) -> Result<Mirrored, MirrorError> {
    let result = sqlx::query(
        "INSERT INTO project_service (id,project_id,document_id,recipient,recipient_person_id,\
         recipient_entity_id,counsel_for,method,served_on,address,declarant,declarant_person_id,\
         letter_id,sent_email_id,inserted_at,updated_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16) \
         ON CONFLICT (id) DO UPDATE SET project_id=EXCLUDED.project_id,\
         document_id=EXCLUDED.document_id,recipient=EXCLUDED.recipient,\
         recipient_person_id=EXCLUDED.recipient_person_id,\
         recipient_entity_id=EXCLUDED.recipient_entity_id,counsel_for=EXCLUDED.counsel_for,\
         method=EXCLUDED.method,served_on=EXCLUDED.served_on,address=EXCLUDED.address,\
         declarant=EXCLUDED.declarant,declarant_person_id=EXCLUDED.declarant_person_id,\
         letter_id=EXCLUDED.letter_id,sent_email_id=EXCLUDED.sent_email_id,\
         inserted_at=EXCLUDED.inserted_at,updated_at=EXCLUDED.updated_at \
         WHERE (project_service.project_id,project_service.document_id,project_service.recipient,\
         project_service.recipient_person_id,project_service.recipient_entity_id,\
         project_service.counsel_for,project_service.method,project_service.served_on,\
         project_service.address,project_service.declarant,project_service.declarant_person_id,\
         project_service.letter_id,project_service.sent_email_id,project_service.inserted_at,\
         project_service.updated_at) IS DISTINCT FROM \
         (EXCLUDED.project_id,EXCLUDED.document_id,EXCLUDED.recipient,\
         EXCLUDED.recipient_person_id,EXCLUDED.recipient_entity_id,EXCLUDED.counsel_for,\
         EXCLUDED.method,EXCLUDED.served_on,EXCLUDED.address,EXCLUDED.declarant,\
         EXCLUDED.declarant_person_id,EXCLUDED.letter_id,EXCLUDED.sent_email_id,\
         EXCLUDED.inserted_at,EXCLUDED.updated_at)",
    )
    .bind(row.id)
    .bind(row.project_id)
    .bind(row.document_id)
    .bind(&row.recipient)
    .bind(row.recipient_person_id)
    .bind(row.recipient_entity_id)
    .bind(&row.counsel_for)
    .bind(&row.method)
    .bind(row.served_on)
    .bind(&row.address)
    .bind(&row.declarant)
    .bind(row.declarant_person_id)
    .bind(row.letter_id)
    .bind(row.sent_email_id)
    .bind(micros(row.inserted_at))
    .bind(micros(row.updated_at))
    .execute(&pg.0)
    .await?;
    Ok(changed(result.rows_affected(), Mirrored::Written))
}

pub async fn mirror(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    key: &MirrorKey,
) -> Result<Mirrored, MirrorError> {
    let MirrorKey::ProjectService(id) = key else {
        return Err(MirrorError::Source);
    };
    let Some(row) = source_one(surreal, *id).await? else {
        return Ok(changed(
            sqlx::query("DELETE FROM project_service WHERE id=$1")
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
    ensure_parent(
        surreal,
        pg,
        "asset",
        row.document_id,
        MirrorKey::Asset(row.document_id),
    )
    .await?;
    let document_project: Option<Uuid> =
        sqlx::query_scalar("SELECT project_id FROM asset WHERE id=$1")
            .bind(row.document_id)
            .fetch_optional(&pg.0)
            .await?;
    if document_project != Some(row.project_id) {
        return Err(MirrorError::Source);
    }
    for person in [row.recipient_person_id, row.declarant_person_id]
        .into_iter()
        .flatten()
    {
        ensure_parent(surreal, pg, "person", person, MirrorKey::Person(person)).await?;
    }
    if let Some(entity) = row.recipient_entity_id {
        ensure_parent(surreal, pg, "entity", entity, MirrorKey::Entity(entity)).await?;
    }
    if let Some(sent_email) = row.sent_email_id {
        ensure_parent(
            surreal,
            pg,
            "sent_email",
            sent_email,
            MirrorKey::SentEmail(sent_email),
        )
        .await?;
    }
    upsert(pg, &row).await
}

pub async fn parity(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    Ok(ParityReport {
        tables: vec![super::compare(
            "project_service",
            &source(surreal).await?,
            &target(pg).await?,
        )],
    })
}

pub async fn reconcile(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let source = source(surreal).await?;
    let target = target(pg).await?;
    let keys: Vec<_> = source
        .iter()
        .filter(|(key, value)| target.get(*key) != Some(*value))
        .map(|(key, _)| key.clone())
        .chain(
            target
                .keys()
                .filter(|key| !source.contains_key(*key))
                .cloned(),
        )
        .collect();
    for key in keys {
        mirror(surreal, pg, &key).await?;
    }
    parity(surreal, pg).await
}

#[cfg(test)]
#[path = "project_services_tests.rs"]
mod tests;

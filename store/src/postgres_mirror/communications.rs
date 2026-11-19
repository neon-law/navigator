//! PostgreSQL projection for firm summary-sender authorization and audit.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use sqlx::{postgres::PgRow, Row};
use surrealdb::types::{Datetime, RecordId, SurrealValue};
use uuid::Uuid;

use super::{changed, micros, Columns, MirrorError, MirrorKey, Mirrored, ParityReport};
use crate::{
    postgres::PostgresDb,
    surreal::{record_uuid, SurrealDb},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ApprovalImage {
    id: Uuid,
    firm_id: Uuid,
    project_id: Option<Uuid>,
    provider_account: String,
    person_id: Uuid,
    sender_email: String,
    status: String,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ApprovalEventImage {
    id: Uuid,
    firm_id: Uuid,
    actor_person_id: Uuid,
    project_id: Option<Uuid>,
    provider_account: String,
    sender_email: String,
    person_id: Uuid,
    action: String,
    inserted_at: DateTime<Utc>,
}

#[derive(SurrealValue)]
struct ApprovalSource {
    id: RecordId,
    firm_id: RecordId,
    project_id: Option<RecordId>,
    provider_account: String,
    person_id: RecordId,
    sender_email: String,
    status: String,
    updated_at: Datetime,
}
#[derive(SurrealValue)]
struct EventSource {
    id: RecordId,
    firm_id: RecordId,
    actor_person_id: RecordId,
    project_id: Option<RecordId>,
    provider_account: String,
    sender_email: String,
    person_id: RecordId,
    action: String,
    inserted_at: Datetime,
}

fn link(id: &RecordId) -> Result<Uuid, MirrorError> {
    record_uuid(id).ok_or(MirrorError::Source)
}
fn optional_link(id: Option<&RecordId>) -> Result<Option<Uuid>, MirrorError> {
    id.map(link).transpose()
}
fn timestamp(value: Datetime) -> DateTime<Utc> {
    micros(value.into())
}

impl TryFrom<ApprovalSource> for ApprovalImage {
    type Error = MirrorError;
    fn try_from(row: ApprovalSource) -> Result<Self, Self::Error> {
        Ok(Self {
            id: link(&row.id)?,
            firm_id: link(&row.firm_id)?,
            project_id: optional_link(row.project_id.as_ref())?,
            provider_account: row.provider_account,
            person_id: link(&row.person_id)?,
            sender_email: row.sender_email,
            status: row.status,
            updated_at: timestamp(row.updated_at),
        })
    }
}
impl TryFrom<EventSource> for ApprovalEventImage {
    type Error = MirrorError;
    fn try_from(row: EventSource) -> Result<Self, Self::Error> {
        Ok(Self {
            id: link(&row.id)?,
            firm_id: link(&row.firm_id)?,
            actor_person_id: link(&row.actor_person_id)?,
            project_id: optional_link(row.project_id.as_ref())?,
            provider_account: row.provider_account,
            sender_email: row.sender_email,
            person_id: link(&row.person_id)?,
            action: row.action,
            inserted_at: timestamp(row.inserted_at),
        })
    }
}

async fn source_approvals(
    surreal: &SurrealDb,
) -> Result<BTreeMap<MirrorKey, Columns>, MirrorError> {
    let rows: Vec<ApprovalSource> = surreal.query("SELECT id, firm_id, project_id, provider_account, person_id, sender_email, status, updated_at FROM summary_sender_approval")
        .await.and_then(surrealdb::IndexedResults::check).map_err(|_| MirrorError::Source)?.take(0).map_err(|_| MirrorError::Source)?;
    rows.into_iter()
        .map(|row| {
            let image = ApprovalImage::try_from(row)?;
            Ok((
                MirrorKey::SummarySenderApproval(image.id),
                Columns::SummarySenderApproval(Box::new(image)),
            ))
        })
        .collect()
}
async fn source_events(surreal: &SurrealDb) -> Result<BTreeMap<MirrorKey, Columns>, MirrorError> {
    let rows: Vec<EventSource> = surreal.query("SELECT id, firm_id, actor_person_id, project_id, provider_account, sender_email, person_id, action, inserted_at FROM summary_sender_approval_event")
        .await.and_then(surrealdb::IndexedResults::check).map_err(|_| MirrorError::Source)?.take(0).map_err(|_| MirrorError::Source)?;
    rows.into_iter()
        .map(|row| {
            let image = ApprovalEventImage::try_from(row)?;
            Ok((
                MirrorKey::SummarySenderApprovalEvent(image.id),
                Columns::SummarySenderApprovalEvent(Box::new(image)),
            ))
        })
        .collect()
}

async fn source_approval(
    surreal: &SurrealDb,
    id: Uuid,
) -> Result<Option<ApprovalImage>, MirrorError> {
    let mut response = surreal.query("SELECT id, firm_id, project_id, provider_account, person_id, sender_email, status, updated_at FROM ONLY $id LIMIT 1")
        .bind(("id", crate::surreal::record_id("summary_sender_approval", id)))
        .await.and_then(surrealdb::IndexedResults::check).map_err(|_| MirrorError::Source)?;
    let row: Option<ApprovalSource> = response.take(0).map_err(|_| MirrorError::Source)?;
    row.map(ApprovalImage::try_from).transpose()
}

async fn source_event(
    surreal: &SurrealDb,
    id: Uuid,
) -> Result<Option<ApprovalEventImage>, MirrorError> {
    let mut response = surreal.query("SELECT id, firm_id, actor_person_id, project_id, provider_account, sender_email, person_id, action, inserted_at FROM ONLY $id LIMIT 1")
        .bind(("id", crate::surreal::record_id("summary_sender_approval_event", id)))
        .await.and_then(surrealdb::IndexedResults::check).map_err(|_| MirrorError::Source)?;
    let row: Option<EventSource> = response.take(0).map_err(|_| MirrorError::Source)?;
    row.map(ApprovalEventImage::try_from).transpose()
}

async fn target_approvals(pg: &PostgresDb) -> Result<BTreeMap<MirrorKey, Columns>, MirrorError> {
    let rows = sqlx::query("SELECT id, firm_id, project_id, provider_account, person_id, sender_email, status, updated_at FROM summary_sender_approval").fetch_all(&pg.0).await?;
    rows.into_iter()
        .map(|row| {
            Ok::<_, MirrorError>((
                MirrorKey::SummarySenderApproval(row.try_get("id")?),
                Columns::SummarySenderApproval(Box::new(approval_from_row(&row)?)),
            ))
        })
        .collect()
}
async fn target_events(pg: &PostgresDb) -> Result<BTreeMap<MirrorKey, Columns>, MirrorError> {
    let rows = sqlx::query("SELECT id, firm_id, actor_person_id, project_id, provider_account, sender_email, person_id, action, inserted_at FROM summary_sender_approval_event").fetch_all(&pg.0).await?;
    rows.into_iter()
        .map(|row| {
            Ok::<_, MirrorError>((
                MirrorKey::SummarySenderApprovalEvent(row.try_get("id")?),
                Columns::SummarySenderApprovalEvent(Box::new(event_from_row(&row)?)),
            ))
        })
        .collect()
}
fn approval_from_row(row: &PgRow) -> Result<ApprovalImage, sqlx::Error> {
    Ok(ApprovalImage {
        id: row.try_get("id")?,
        firm_id: row.try_get("firm_id")?,
        project_id: row.try_get("project_id")?,
        provider_account: row.try_get("provider_account")?,
        person_id: row.try_get("person_id")?,
        sender_email: row.try_get("sender_email")?,
        status: row.try_get("status")?,
        updated_at: row.try_get("updated_at")?,
    })
}
fn event_from_row(row: &PgRow) -> Result<ApprovalEventImage, sqlx::Error> {
    Ok(ApprovalEventImage {
        id: row.try_get("id")?,
        firm_id: row.try_get("firm_id")?,
        actor_person_id: row.try_get("actor_person_id")?,
        project_id: row.try_get("project_id")?,
        provider_account: row.try_get("provider_account")?,
        sender_email: row.try_get("sender_email")?,
        person_id: row.try_get("person_id")?,
        action: row.try_get("action")?,
        inserted_at: row.try_get("inserted_at")?,
    })
}

async fn upsert_approval(pg: &PostgresDb, row: &ApprovalImage) -> Result<Mirrored, MirrorError> {
    let result = sqlx::query("INSERT INTO summary_sender_approval (id, firm_id, project_id, provider_account, person_id, sender_email, status, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT (id) DO NOTHING")
        .bind(row.id).bind(row.firm_id).bind(row.project_id).bind(&row.provider_account).bind(row.person_id).bind(&row.sender_email).bind(&row.status).bind(micros(row.updated_at)).execute(&pg.0).await?;
    Ok(changed(result.rows_affected(), Mirrored::Written))
}
async fn upsert_event(pg: &PostgresDb, row: &ApprovalEventImage) -> Result<Mirrored, MirrorError> {
    let result = sqlx::query("INSERT INTO summary_sender_approval_event (id, firm_id, actor_person_id, project_id, provider_account, sender_email, person_id, action, inserted_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT (id) DO NOTHING")
        .bind(row.id).bind(row.firm_id).bind(row.actor_person_id).bind(row.project_id).bind(&row.provider_account).bind(&row.sender_email).bind(row.person_id).bind(&row.action).bind(micros(row.inserted_at)).execute(&pg.0).await?;
    Ok(changed(result.rows_affected(), Mirrored::Written))
}

async fn ensure_parent(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    table: &'static str,
    id: Uuid,
    key: MirrorKey,
) -> Result<(), MirrorError> {
    let exists: bool = match table {
        "firm" => {
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM firm WHERE id=$1)")
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
        "project" => {
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM project WHERE id=$1)")
                .bind(id)
                .fetch_one(&pg.0)
                .await?
        }
        _ => return Err(MirrorError::Source),
    };
    if !exists {
        if super::backfilled(pg).await?
            || crate::postgres_backfill::family::active_for_table(pg, table).await?
        {
            return Err(MirrorError::MatterAuthority);
        }
        match key {
            MirrorKey::Firm(_) | MirrorKey::Project(_) => {
                super::from_source(super::tenancy::mirror(surreal, pg, &key)).await?
            }
            MirrorKey::Person(_) => {
                super::from_source(super::identity::mirror(surreal, pg, &key)).await?
            }
            _ => return Err(MirrorError::Source),
        };
    }
    Ok(())
}

async fn ensure_approval_parents(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    firm_id: Uuid,
    project_id: Option<Uuid>,
    person_id: Uuid,
) -> Result<(), MirrorError> {
    ensure_parent(surreal, pg, "firm", firm_id, MirrorKey::Firm(firm_id)).await?;
    if let Some(project_id) = project_id {
        ensure_parent(
            surreal,
            pg,
            "project",
            project_id,
            MirrorKey::Project(project_id),
        )
        .await?;
    }
    ensure_parent(
        surreal,
        pg,
        "person",
        person_id,
        MirrorKey::Person(person_id),
    )
    .await
}

pub async fn mirror(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    key: &MirrorKey,
) -> Result<Mirrored, MirrorError> {
    if !matches!(
        key,
        MirrorKey::SummarySenderApproval(_) | MirrorKey::SummarySenderApprovalEvent(_)
    ) {
        return Err(MirrorError::Source);
    }
    if !super::source_import_allowed(pg, key.table()).await? {
        return Ok(Mirrored::Unchanged);
    }
    match key {
        MirrorKey::SummarySenderApproval(id) => {
            let Some(row) = source_approval(surreal, *id).await? else {
                return Ok(Mirrored::Unchanged);
            };
            ensure_approval_parents(surreal, pg, row.firm_id, row.project_id, row.person_id)
                .await?;
            upsert_approval(pg, &row).await
        }
        MirrorKey::SummarySenderApprovalEvent(id) => {
            let Some(row) = source_event(surreal, *id).await? else {
                return Ok(Mirrored::Unchanged);
            };
            ensure_approval_parents(surreal, pg, row.firm_id, row.project_id, row.person_id)
                .await?;
            ensure_parent(
                surreal,
                pg,
                "person",
                row.actor_person_id,
                MirrorKey::Person(row.actor_person_id),
            )
            .await?;
            upsert_event(pg, &row).await
        }
        _ => Err(MirrorError::Source),
    }
}

pub async fn parity(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    if crate::postgres_backfill::family::active_for_table(pg, "summary_sender_approval").await? {
        return Ok(ParityReport { tables: vec![] });
    }
    let approvals = super::compare(
        "summary_sender_approval",
        &source_approvals(surreal).await?,
        &target_approvals(pg).await?,
    );
    let events = super::compare(
        "summary_sender_approval_event",
        &source_events(surreal).await?,
        &target_events(pg).await?,
    );
    Ok(ParityReport {
        tables: vec![approvals, events],
    })
}
pub async fn reconcile(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    if crate::postgres_backfill::family::active_for_table(pg, "summary_sender_approval").await? {
        return Ok(ParityReport { tables: vec![] });
    }
    let source = source_approvals(surreal).await?;
    let target = target_approvals(pg).await?;
    for key in divergent(&source, &target) {
        mirror(surreal, pg, &key).await?;
    }
    let source = source_events(surreal).await?;
    let target = target_events(pg).await?;
    for key in divergent(&source, &target) {
        mirror(surreal, pg, &key).await?;
    }
    parity(surreal, pg).await
}
fn divergent(
    source: &BTreeMap<MirrorKey, Columns>,
    target: &BTreeMap<MirrorKey, Columns>,
) -> Vec<MirrorKey> {
    source
        .iter()
        .filter(|(key, value)| target.get(*key) != Some(*value))
        .map(|(key, _)| key.clone())
        .chain(
            target
                .keys()
                .filter(|key| !source.contains_key(*key))
                .cloned(),
        )
        .collect()
}

#[cfg(test)]
#[path = "communications_tests.rs"]
mod tests;

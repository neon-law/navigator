//! Opt-in PostgreSQL mirror of Project Reference rows.
//!
//! A reference preserves a verified person or entity for conflict checks.
//! It does not create Project participation, grant access, or appear in a
//! matter's party list. Separate nullable endpoint columns make PostgreSQL
//! enforce the same Person/Entity boundary as the Surreal relation.

use std::collections::BTreeMap;

use chrono::{DateTime, SubsecRound, Utc};
use sqlx::Row;
use surrealdb::types::{Datetime, RecordId, SurrealValue};
use uuid::Uuid;

use super::{
    changed, identity, tenancy, MirrorError, MirrorKey, Mirrored, ParityReport, TableParity,
};
use crate::postgres::PostgresDb;
use crate::relationships::Endpoint;
use crate::surreal::{record_id, record_uuid, SurrealDb};

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReferenceImage {
    project_id: Uuid,
    party_kind: Endpoint,
    party_id: Uuid,
    reason: String,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl ReferenceImage {
    fn key(&self) -> MirrorKey {
        MirrorKey::ProjectReference {
            project_id: self.project_id,
            party_kind: self.party_kind.table().to_owned(),
            party_id: self.party_id,
            reason: self.reason.clone(),
        }
    }
}

#[derive(SurrealValue)]
struct SourceReference {
    r#in: RecordId,
    out: RecordId,
    reason: String,
    inserted_at: Datetime,
    updated_at: Datetime,
}

impl SourceReference {
    fn image(self) -> Result<ReferenceImage, MirrorError> {
        let party_kind =
            Endpoint::from_table(self.out.table.as_str()).ok_or(MirrorError::Source)?;
        Ok(ReferenceImage {
            project_id: record_uuid(&self.r#in).ok_or(MirrorError::Source)?,
            party_kind,
            party_id: record_uuid(&self.out).ok_or(MirrorError::Source)?,
            reason: self.reason,
            inserted_at: micros(self.inserted_at.into()),
            updated_at: micros(self.updated_at.into()),
        })
    }
}

fn micros(value: DateTime<Utc>) -> DateTime<Utc> {
    value.trunc_subsecs(6)
}

async fn source_rows(
    surreal: &SurrealDb,
    key: Option<&MirrorKey>,
) -> Result<Vec<ReferenceImage>, MirrorError> {
    let mut response = match key {
        Some(MirrorKey::ProjectReference {
            project_id,
            party_kind,
            party_id,
            reason,
        }) => {
            let endpoint = Endpoint::from_table(party_kind).ok_or(MirrorError::Source)?;
            surreal
                .query("SELECT in, out, reason, inserted_at, updated_at FROM project_reference WHERE in = $project AND out = $party AND reason = $reason LIMIT 1")
                .bind(("project", record_id("project", *project_id)))
                .bind(("party", record_id(endpoint.table(), *party_id)))
                .bind(("reason", reason.clone()))
                .await
        }
        Some(_) => return Err(MirrorError::Source),
        None => surreal
            .query("SELECT in, out, reason, inserted_at, updated_at FROM project_reference")
            .await,
    }
    .and_then(surrealdb::IndexedResults::check)
    .map_err(|_| MirrorError::Source)?;
    let rows: Vec<SourceReference> = response.take(0).map_err(|_| MirrorError::Source)?;
    rows.into_iter().map(SourceReference::image).collect()
}

fn upsert_sql(kind: Endpoint) -> &'static str {
    match kind {
        Endpoint::Person => "INSERT INTO project_reference (project_id, person_id, entity_id, reason, inserted_at, updated_at) VALUES ($1, $2, NULL, $3, $4, $5) ON CONFLICT (project_id, person_id, reason) WHERE person_id IS NOT NULL DO UPDATE SET inserted_at = EXCLUDED.inserted_at, updated_at = EXCLUDED.updated_at WHERE (project_reference.inserted_at, project_reference.updated_at) IS DISTINCT FROM (EXCLUDED.inserted_at, EXCLUDED.updated_at)",
        Endpoint::Entity => "INSERT INTO project_reference (project_id, person_id, entity_id, reason, inserted_at, updated_at) VALUES ($1, NULL, $2, $3, $4, $5) ON CONFLICT (project_id, entity_id, reason) WHERE entity_id IS NOT NULL DO UPDATE SET inserted_at = EXCLUDED.inserted_at, updated_at = EXCLUDED.updated_at WHERE (project_reference.inserted_at, project_reference.updated_at) IS DISTINCT FROM (EXCLUDED.inserted_at, EXCLUDED.updated_at)",
    }
}

async fn write(pg: &PostgresDb, image: &ReferenceImage) -> Result<Mirrored, MirrorError> {
    let written = sqlx::query(upsert_sql(image.party_kind))
        .bind(image.project_id)
        .bind(image.party_id)
        .bind(&image.reason)
        .bind(image.inserted_at)
        .bind(image.updated_at)
        .execute(&pg.0)
        .await?;
    Ok(changed(written.rows_affected(), Mirrored::Written))
}

/// Mirror one Project Reference, or remove its PostgreSQL row after deletion.
pub(super) async fn mirror(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    key: &MirrorKey,
) -> Result<Mirrored, MirrorError> {
    let Some(image) = source_rows(surreal, Some(key)).await?.pop() else {
        let MirrorKey::ProjectReference {
            project_id,
            party_kind,
            party_id,
            reason,
        } = key
        else {
            return Err(MirrorError::Source);
        };
        let column = match party_kind.as_str() {
            "person" => "person_id",
            "entity" => "entity_id",
            _ => return Err(MirrorError::Source),
        };
        let statement = format!(
            "DELETE FROM project_reference WHERE project_id = $1 AND {column} = $2 AND reason = $3"
        );
        let deleted = sqlx::query(sqlx::AssertSqlSafe(statement))
            .bind(*project_id)
            .bind(*party_id)
            .bind(reason)
            .execute(&pg.0)
            .await?;
        return Ok(changed(deleted.rows_affected(), Mirrored::Deleted));
    };
    let project = crate::projects::find_by_id(surreal, image.project_id)
        .await
        .map_err(|_| MirrorError::Source)?
        .ok_or(MirrorError::Source)?;
    if let Some(firm_id) = project.firm_id {
        let firm_key = MirrorKey::Firm(firm_id);
        if !super::held_by_target(pg, &firm_key).await? {
            tenancy::mirror(surreal, pg, &firm_key).await?;
        }
    }
    if let Some(jurisdiction_id) = project.jurisdiction_id {
        identity::ensure(surreal, pg, MirrorKey::Jurisdiction(jurisdiction_id)).await?;
    }
    let project_key = MirrorKey::Project(image.project_id);
    if !super::held_by_target(pg, &project_key).await? {
        tenancy::mirror(surreal, pg, &project_key).await?;
    }
    identity::ensure(
        surreal,
        pg,
        match image.party_kind {
            Endpoint::Person => MirrorKey::Person(image.party_id),
            Endpoint::Entity => MirrorKey::Entity(image.party_id),
        },
    )
    .await?;
    write(pg, &image).await
}

async fn target_rows(pg: &PostgresDb) -> Result<Vec<ReferenceImage>, MirrorError> {
    let rows = sqlx::query(
        "SELECT project_id, person_id, entity_id, reason, inserted_at, updated_at FROM project_reference",
    )
    .fetch_all(&pg.0)
    .await?;
    rows.iter()
        .map(|row| -> Result<ReferenceImage, MirrorError> {
            let person_id: Option<Uuid> = row.try_get("person_id")?;
            let entity_id: Option<Uuid> = row.try_get("entity_id")?;
            let (party_kind, party_id) = match (person_id, entity_id) {
                (Some(id), None) => (Endpoint::Person, id),
                (None, Some(id)) => (Endpoint::Entity, id),
                _ => return Err(MirrorError::Source),
            };
            Ok(ReferenceImage {
                project_id: row.try_get("project_id")?,
                party_kind,
                party_id,
                reason: row.try_get("reason")?,
                inserted_at: row.try_get("inserted_at")?,
                updated_at: row.try_get("updated_at")?,
            })
        })
        .collect()
}

/// Compare every Project Reference in both stores. Reports name keys only.
pub async fn parity(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let source: BTreeMap<_, _> = source_rows(surreal, None)
        .await?
        .into_iter()
        .map(|image| (image.key(), image))
        .collect();
    let target: BTreeMap<_, _> = target_rows(pg)
        .await?
        .into_iter()
        .map(|image| (image.key(), image))
        .collect();
    let mut divergent: Vec<_> = source
        .iter()
        .filter(|(key, image)| target.get(*key) != Some(*image))
        .map(|(key, _)| key.clone())
        .chain(
            target
                .keys()
                .filter(|key| !source.contains_key(*key))
                .cloned(),
        )
        .collect();
    divergent.sort();
    Ok(ParityReport {
        tables: vec![TableParity {
            table: "project_reference",
            source_rows: source.len(),
            target_rows: target.len(),
            divergent,
        }],
    })
}

/// Re-mirror every divergent reference, then compare again.
pub async fn reconcile(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let before = parity(surreal, pg).await?;
    for key in before.divergent() {
        if let Err(error) = mirror(surreal, pg, key).await {
            tracing::warn!(key = %key, error = %error, "PostgreSQL reconcile could not mirror the Project Reference");
        }
    }
    parity(surreal, pg).await
}

#[cfg(test)]
#[path = "project_references_tests.rs"]
mod tests;

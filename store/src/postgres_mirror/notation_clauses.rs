//! PostgreSQL mirror for lawyer-authored Notation clauses.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use sqlx::Row;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use super::tenancy::{compare, remirror, select_sql, target, upsert_sql};
use super::{changed, identity, micros, MirrorError, MirrorKey, Mirrored, ParityReport};
use crate::postgres::PostgresDb;
use crate::surreal::{record_id, record_uuid, SurrealDb};

const COLUMNS: &[&str] = &[
    "id",
    "notation_id",
    "position",
    "body_markdown",
    "authored_by_person_id",
    "inserted_at",
    "updated_at",
];
const SOURCE_FIELDS: &str =
    "id, notation_id, position, body_markdown, authored_by_person_id, inserted_at, updated_at";
static UPSERT: LazyLock<String> = LazyLock::new(|| upsert_sql("notation_clause", COLUMNS));
static SELECT: LazyLock<String> = LazyLock::new(|| select_sql("notation_clause", COLUMNS));

#[derive(SurrealValue)]
struct SourceRow {
    id: surrealdb::types::RecordId,
    notation_id: surrealdb::types::RecordId,
    position: i32,
    body_markdown: String,
    authored_by_person_id: Option<surrealdb::types::RecordId>,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Image {
    id: Uuid,
    notation_id: Uuid,
    position: i32,
    body_markdown: String,
    authored_by_person_id: Option<Uuid>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl SourceRow {
    fn image(self) -> Result<Image, MirrorError> {
        let optional_id = |id: Option<surrealdb::types::RecordId>| {
            id.map(|record| record_uuid(&record).ok_or(MirrorError::Source))
                .transpose()
        };
        Ok(Image {
            id: record_uuid(&self.id).ok_or(MirrorError::Source)?,
            notation_id: record_uuid(&self.notation_id).ok_or(MirrorError::Source)?,
            position: self.position,
            body_markdown: self.body_markdown,
            authored_by_person_id: optional_id(self.authored_by_person_id)?,
            inserted_at: pg_timestamp(self.inserted_at),
            updated_at: pg_timestamp(self.updated_at),
        })
    }
}

fn pg_timestamp(timestamp: surrealdb::types::Datetime) -> DateTime<Utc> {
    micros(timestamp.into())
}

impl Image {
    fn from_row(row: &sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            notation_id: row.try_get("notation_id")?,
            position: row.try_get("position")?,
            body_markdown: row.try_get("body_markdown")?,
            authored_by_person_id: row.try_get("authored_by_person_id")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

async fn source_row(surreal: &SurrealDb, id: Uuid) -> Result<Option<SourceRow>, MirrorError> {
    let mut result = surreal
        .query(format!("SELECT {SOURCE_FIELDS} FROM ONLY $id LIMIT 1"))
        .bind(("id", record_id(crate::notation_clauses::TABLE, id)))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?;
    result.take(0).map_err(|_| MirrorError::Source)
}

async fn rows(surreal: &SurrealDb) -> Result<Vec<SourceRow>, MirrorError> {
    let mut result = surreal
        .query(format!(
            "SELECT {SOURCE_FIELDS} FROM {}",
            crate::notation_clauses::TABLE
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?;
    result.take(0).map_err(|_| MirrorError::Source)
}

pub(super) async fn mirror(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    id: Uuid,
) -> Result<Mirrored, MirrorError> {
    let Some(row) = source_row(surreal, id).await? else {
        let result = sqlx::query("DELETE FROM notation_clause WHERE id = $1")
            .bind(id)
            .execute(&pg.0)
            .await?;
        return Ok(changed(result.rows_affected(), Mirrored::Deleted));
    };
    let image = row.image()?;
    crate::postgres_mirror::workflow::mirror(surreal, pg, &MirrorKey::Notation(image.notation_id))
        .await?;
    if let Some(person) = image.authored_by_person_id {
        identity::ensure(surreal, pg, MirrorKey::Person(person)).await?;
    }
    let written = sqlx::query(UPSERT.as_str())
        .bind(image.id)
        .bind(image.notation_id)
        .bind(image.position)
        .bind(&image.body_markdown)
        .bind(image.authored_by_person_id)
        .bind(image.inserted_at)
        .bind(image.updated_at)
        .execute(&pg.0)
        .await?;
    Ok(changed(written.rows_affected(), Mirrored::Written))
}

pub(super) async fn parity(
    surreal: &SurrealDb,
    pg: &PostgresDb,
) -> Result<ParityReport, MirrorError> {
    let mut source = BTreeMap::new();
    for row in rows(surreal).await? {
        let image = row.image()?;
        source.insert(MirrorKey::NotationClause(image.id), Some(image));
    }
    let target = target(pg, SELECT.as_str(), Image::from_row, |row| {
        MirrorKey::NotationClause(row.id)
    })
    .await?;
    Ok(ParityReport {
        tables: vec![compare("notation_clause", &source, &target)],
    })
}

pub(super) async fn reconcile(
    surreal: &SurrealDb,
    pg: &PostgresDb,
) -> Result<ParityReport, MirrorError> {
    let before = parity(surreal, pg).await?;
    remirror(surreal, pg, &before).await;
    parity(surreal, pg).await
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};

    use super::pg_timestamp;

    #[test]
    fn source_timestamp_is_projected_to_postgres_microseconds() {
        let source = DateTime::parse_from_rfc3339("2026-10-20T12:34:56.123456789Z")
            .unwrap()
            .with_timezone(&Utc);
        let projected = pg_timestamp(source.into());

        assert_eq!(projected.to_rfc3339(), "2026-10-20T12:34:56.123456+00:00");
    }
}

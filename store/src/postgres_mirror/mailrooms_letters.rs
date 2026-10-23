//! Physical Mailroom and Letter rows in the PostgreSQL mirror.
//!
//! SurrealDB remains authoritative. The same source UUID and exact source
//! fields are copied to PostgreSQL after successful source writes; address
//! and mailroom parents are mirrored before their children. Project ids on
//! Letters remain nullable and intentionally have no PostgreSQL foreign key:
//! historical rows may omit them and the source writer does not validate them.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

use super::tenancy::{compare, source as source_error, upsert_sql};
use super::{changed, MirrorError, MirrorKey, Mirrored, ParityReport};
use crate::postgres::PostgresDb;
use crate::surreal::SurrealDb;

const MAILROOM_COLUMNS: &[&str] = &["id", "name", "address_id", "inserted_at", "updated_at"];
const LETTER_COLUMNS: &[&str] = &[
    "id",
    "mailroom_id",
    "project_id",
    "direction",
    "sender",
    "recipient",
    "summary",
    "inserted_at",
    "updated_at",
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct MailroomImage {
    id: Uuid,
    name: String,
    address_id: Uuid,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<crate::mailrooms::Mailroom> for MailroomImage {
    fn from(row: crate::mailrooms::Mailroom) -> Self {
        Self {
            id: row.id,
            name: row.name,
            address_id: row.address_id,
            inserted_at: super::micros(row.inserted_at),
            updated_at: super::micros(row.updated_at),
        }
    }
}

impl MailroomImage {
    fn from_row(row: &sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            address_id: row.try_get("address_id")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }

    fn bind<'q>(
        &self,
        query: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    ) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
        query
            .bind(self.id)
            .bind(&self.name)
            .bind(self.address_id)
            .bind(self.inserted_at)
            .bind(self.updated_at)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LetterImage {
    id: Uuid,
    mailroom_id: Uuid,
    project_id: Option<Uuid>,
    direction: String,
    sender: String,
    recipient: String,
    summary: String,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<crate::letters::Letter> for LetterImage {
    fn from(row: crate::letters::Letter) -> Self {
        Self {
            id: row.id,
            mailroom_id: row.mailroom_id,
            project_id: row.project_id,
            direction: row.direction,
            sender: row.sender,
            recipient: row.recipient,
            summary: row.summary,
            inserted_at: super::micros(row.inserted_at),
            updated_at: super::micros(row.updated_at),
        }
    }
}

impl LetterImage {
    fn from_row(row: &sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            mailroom_id: row.try_get("mailroom_id")?,
            project_id: row.try_get("project_id")?,
            direction: row.try_get("direction")?,
            sender: row.try_get("sender")?,
            recipient: row.try_get("recipient")?,
            summary: row.try_get("summary")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }

    fn bind<'q>(
        &self,
        query: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    ) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
        query
            .bind(self.id)
            .bind(self.mailroom_id)
            .bind(self.project_id)
            .bind(&self.direction)
            .bind(&self.sender)
            .bind(&self.recipient)
            .bind(&self.summary)
            .bind(self.inserted_at)
            .bind(self.updated_at)
    }
}

pub(super) async fn mirror(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    key: &MirrorKey,
) -> Result<Mirrored, MirrorError> {
    match key {
        MirrorKey::Mailroom(id) => mirror_mailroom(surreal, pg, *id).await,
        MirrorKey::Letter(id) => mirror_letter(surreal, pg, *id).await,
        _ => Err(MirrorError::Source),
    }
}

async fn mirror_mailroom(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    id: Uuid,
) -> Result<Mirrored, MirrorError> {
    let Some(row) = crate::mailrooms::find_by_id(surreal, id)
        .await
        .map_err(source_error)?
    else {
        let result = sqlx::query("DELETE FROM mailroom WHERE id=$1")
            .bind(id)
            .execute(&pg.0)
            .await?;
        return Ok(changed(result.rows_affected(), Mirrored::Deleted));
    };
    let row = MailroomImage::from(row);
    super::identity_detail::mirror(surreal, pg, &MirrorKey::Address(row.address_id)).await?;
    let query = sqlx::query(sqlx::AssertSqlSafe(upsert_sql(
        "mailroom",
        MAILROOM_COLUMNS,
    )));
    let result = row.bind(query).execute(&pg.0).await?;
    Ok(changed(result.rows_affected(), Mirrored::Written))
}

async fn mirror_letter(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    id: Uuid,
) -> Result<Mirrored, MirrorError> {
    let Some(row) = crate::letters::find_by_id(surreal, id)
        .await
        .map_err(source_error)?
    else {
        let result = sqlx::query("DELETE FROM letter WHERE id=$1")
            .bind(id)
            .execute(&pg.0)
            .await?;
        return Ok(changed(result.rows_affected(), Mirrored::Deleted));
    };
    let row = LetterImage::from(row);
    mirror_mailroom(surreal, pg, row.mailroom_id).await?;
    let query = sqlx::query(sqlx::AssertSqlSafe(upsert_sql("letter", LETTER_COLUMNS)));
    let result = row.bind(query).execute(&pg.0).await?;
    Ok(changed(result.rows_affected(), Mirrored::Written))
}

pub async fn parity(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let mailrooms = crate::mailrooms::list_all(surreal)
        .await
        .map_err(source_error)?;
    let letters = crate::letters::list_all(surreal)
        .await
        .map_err(source_error)?;
    let source_mailrooms: BTreeMap<_, _> = mailrooms
        .into_iter()
        .map(|r| {
            let image = MailroomImage::from(r);
            (MirrorKey::Mailroom(image.id), Some(image))
        })
        .collect();
    let source_letters: BTreeMap<_, _> = letters
        .into_iter()
        .map(|r| {
            let image = LetterImage::from(r);
            (MirrorKey::Letter(image.id), Some(image))
        })
        .collect();
    let target_mailrooms: BTreeMap<_, _> =
        sqlx::query("SELECT id,name,address_id,inserted_at,updated_at FROM mailroom")
            .fetch_all(&pg.0)
            .await?
            .iter()
            .map(|r| {
                Ok((
                    MirrorKey::Mailroom(r.try_get("id")?),
                    MailroomImage::from_row(r)?,
                ))
            })
            .collect::<Result<_, sqlx::Error>>()?;
    let target_letters: BTreeMap<_, _> = sqlx::query("SELECT id,mailroom_id,project_id,direction,sender,recipient,summary,inserted_at,updated_at FROM letter")
        .fetch_all(&pg.0).await?.iter().map(|r| Ok((MirrorKey::Letter(r.try_get("id")?), LetterImage::from_row(r)?)))
        .collect::<Result<_, sqlx::Error>>()?;
    Ok(ParityReport {
        tables: vec![
            compare("mailroom", &source_mailrooms, &target_mailrooms),
            compare("letter", &source_letters, &target_letters),
        ],
    })
}

pub async fn reconcile(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let before = parity(surreal, pg).await?;
    let mailroom_ids: BTreeSet<_> = crate::mailrooms::list_all(surreal)
        .await
        .map_err(source_error)?
        .into_iter()
        .map(|row| row.id)
        .collect();
    let letter_ids: BTreeSet<_> = crate::letters::list_all(surreal)
        .await
        .map_err(source_error)?
        .into_iter()
        .map(|row| row.id)
        .collect();

    // Remove only rows no longer present at the source. Children go first
    // so removed Mailrooms can be deleted, and stale natural keys are freed
    // before replacement rows are upserted after an in-memory source reset.
    for key in before
        .divergent()
        .filter(|key| matches!(key, MirrorKey::Letter(id) if !letter_ids.contains(id)))
    {
        if let Err(error) = mirror(surreal, pg, key).await {
            tracing::warn!(key = %key, error = %error, "PostgreSQL reconcile could not mirror the row");
        }
    }
    for key in before
        .divergent()
        .filter(|key| matches!(key, MirrorKey::Mailroom(id) if !mailroom_ids.contains(id)))
    {
        if let Err(error) = mirror(surreal, pg, key).await {
            tracing::warn!(key = %key, error = %error, "PostgreSQL reconcile could not mirror the row");
        }
    }

    // Write parents before children once stale identifiers and unique names
    // have been released.
    for key in before
        .divergent()
        .filter(|key| matches!(key, MirrorKey::Mailroom(id) if mailroom_ids.contains(id)))
    {
        if let Err(error) = mirror(surreal, pg, key).await {
            tracing::warn!(key = %key, error = %error, "PostgreSQL reconcile could not mirror the row");
        }
    }
    for key in before
        .divergent()
        .filter(|key| matches!(key, MirrorKey::Letter(id) if letter_ids.contains(id)))
    {
        if let Err(error) = mirror(surreal, pg, key).await {
            tracing::warn!(key = %key, error = %error, "PostgreSQL reconcile could not mirror the row");
        }
    }
    parity(surreal, pg).await
}

#[cfg(test)]
#[path = "mailrooms_letters_tests.rs"]
mod tests;

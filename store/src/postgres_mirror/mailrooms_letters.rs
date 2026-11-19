//! Physical Mailroom and Letter rows in the PostgreSQL mirror.
//!
//! Explicit historical imports copy missing rows without overwriting native data.
//! Persisted Application ownership blocks Source access, even during forced replay.
//! Historical Letter Project ids remain nullable and have no foreign key.

use std::collections::BTreeMap;

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
    if !matches!(key, MirrorKey::Mailroom(_) | MirrorKey::Letter(_)) {
        return Err(MirrorError::Source);
    }
    if !super::source_import_allowed(pg, key.table()).await? {
        return Ok(Mirrored::Unchanged);
    }
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
    let Some(row) = super::from_source(crate::mailrooms::find_by_id(surreal, id))
        .await
        .map_err(source_error)?
    else {
        return Ok(Mirrored::Unchanged);
    };
    let row = MailroomImage::from(row);
    let address_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM address WHERE id=$1)")
            .bind(row.address_id)
            .fetch_one(pg.pool())
            .await?;
    if !address_exists {
        if super::backfilled(pg).await?
            || crate::postgres_backfill::family::active_for_table(pg, "address").await?
        {
            return Err(MirrorError::MatterAuthority);
        }
        super::identity_detail::mirror(surreal, pg, &MirrorKey::Address(row.address_id)).await?;
    }
    let query = sqlx::query(super::held_write(
        true,
        &upsert_sql("mailroom", MAILROOM_COLUMNS),
    ));
    let result = row.bind(query).execute(&pg.0).await?;
    Ok(changed(result.rows_affected(), Mirrored::Written))
}

async fn mirror_letter(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    id: Uuid,
) -> Result<Mirrored, MirrorError> {
    let Some(row) = super::from_source(crate::letters::find_by_id(surreal, id))
        .await
        .map_err(source_error)?
    else {
        return Ok(Mirrored::Unchanged);
    };
    let row = LetterImage::from(row);
    let mailroom_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM mailroom WHERE id=$1)")
            .bind(row.mailroom_id)
            .fetch_one(pg.pool())
            .await?;
    if !mailroom_exists {
        if super::backfilled(pg).await?
            || crate::postgres_backfill::family::active_for_table(pg, "mailroom").await?
        {
            return Err(MirrorError::MatterAuthority);
        }
        mirror_mailroom(surreal, pg, row.mailroom_id).await?;
    }
    let query = sqlx::query(super::held_write(
        true,
        &upsert_sql("letter", LETTER_COLUMNS),
    ));
    let result = row.bind(query).execute(&pg.0).await?;
    Ok(changed(result.rows_affected(), Mirrored::Written))
}

pub async fn parity(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    if crate::postgres_backfill::family::active_for_table(pg, "mailroom").await? {
        return Ok(ParityReport { tables: vec![] });
    }
    let mailrooms = super::from_source(crate::mailrooms::list_all(surreal))
        .await
        .map_err(source_error)?;
    let letters = super::from_source(crate::letters::list_all(surreal))
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
    // Parent tables appear first. Conflicts and target-only rows remain held evidence.
    for table in ["mailroom", "letter"] {
        for key in before.divergent().filter(|key| key.table() == table) {
            mirror(surreal, pg, key).await?;
        }
    }
    parity(surreal, pg).await
}

#[cfg(test)]
#[path = "mailrooms_letters_tests.rs"]
mod tests;

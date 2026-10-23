//! PostgreSQL projection of a Brand's saved provider destination.

use super::{changed, compare, micros, Columns, MirrorError, MirrorKey, Mirrored, ParityReport};
use crate::{
    postgres::PostgresDb,
    surreal::{record_uuid, SurrealDb},
};
use chrono::{DateTime, Utc};
use sqlx::{postgres::PgRow, Row};
use std::collections::BTreeMap;
use surrealdb::types::{Datetime, RecordId, RecordIdKey, SurrealValue};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SelectionImage {
    id: String,
    firm_id: uuid::Uuid,
    brand_key: String,
    provider: String,
    provider_account_id: String,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(SurrealValue)]
struct Source {
    id: RecordId,
    firm_id: RecordId,
    brand_key: String,
    provider: String,
    provider_account_id: String,
    inserted_at: Datetime,
    updated_at: Datetime,
}

impl TryFrom<Source> for SelectionImage {
    type Error = MirrorError;

    fn try_from(row: Source) -> Result<Self, Self::Error> {
        let RecordIdKey::String(id) = row.id.key else {
            return Err(MirrorError::Source);
        };
        if row.id.table.as_str() != "social_account_selection"
            || id != format!("{}:{}", row.brand_key, row.provider)
        {
            return Err(MirrorError::Source);
        }
        Ok(Self {
            id,
            firm_id: record_uuid(&row.firm_id).ok_or(MirrorError::Source)?,
            brand_key: row.brand_key,
            provider: row.provider,
            provider_account_id: row.provider_account_id,
            inserted_at: micros(row.inserted_at.into()),
            updated_at: micros(row.updated_at.into()),
        })
    }
}

fn key(image: &SelectionImage) -> MirrorKey {
    MirrorKey::SocialAccountSelection {
        brand_key: image.brand_key.clone(),
        provider: image.provider.clone(),
    }
}

const FIELDS: &str =
    "id, firm_id, brand_key, provider, provider_account_id, inserted_at, updated_at";

async fn source(surreal: &SurrealDb) -> Result<BTreeMap<MirrorKey, Columns>, MirrorError> {
    let rows: Vec<Source> = surreal
        .query(format!("SELECT {FIELDS} FROM social_account_selection"))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?
        .take(0)
        .map_err(|_| MirrorError::Source)?;
    rows.into_iter()
        .map(|row| {
            let image = SelectionImage::try_from(row)?;
            Ok((
                key(&image),
                Columns::SocialAccountSelection(Box::new(image)),
            ))
        })
        .collect()
}

async fn source_one(
    surreal: &SurrealDb,
    brand_key: &str,
    provider: &str,
) -> Result<Option<SelectionImage>, MirrorError> {
    let id = format!("{brand_key}:{provider}");
    let mut response = surreal
        .query(format!("SELECT {FIELDS} FROM ONLY $id LIMIT 1"))
        .bind(("id", RecordId::new("social_account_selection", id)))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?;
    let row: Option<Source> = response.take(0).map_err(|_| MirrorError::Source)?;
    row.map(SelectionImage::try_from).transpose()
}

fn from_row(row: &PgRow) -> Result<SelectionImage, sqlx::Error> {
    Ok(SelectionImage {
        id: row.try_get("id")?,
        firm_id: row.try_get("firm_id")?,
        brand_key: row.try_get("brand_key")?,
        provider: row.try_get("provider")?,
        provider_account_id: row.try_get("provider_account_id")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

async fn target(pg: &PostgresDb) -> Result<BTreeMap<MirrorKey, Columns>, MirrorError> {
    let rows = sqlx::query("SELECT id, firm_id, brand_key, provider, provider_account_id, inserted_at, updated_at FROM social_account_selection")
        .fetch_all(&pg.0)
        .await?;
    rows.into_iter()
        .map(|row| {
            let image = from_row(&row)?;
            Ok((
                key(&image),
                Columns::SocialAccountSelection(Box::new(image)),
            ))
        })
        .collect()
}

pub async fn mirror(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    key: &MirrorKey,
) -> Result<Mirrored, MirrorError> {
    let MirrorKey::SocialAccountSelection {
        brand_key,
        provider,
    } = key
    else {
        return Err(MirrorError::Source);
    };
    let Some(row) = source_one(surreal, brand_key, provider).await? else {
        let id = format!("{brand_key}:{provider}");
        let result = sqlx::query("DELETE FROM social_account_selection WHERE id = $1")
            .bind(id)
            .execute(&pg.0)
            .await?;
        return Ok(changed(result.rows_affected(), Mirrored::Deleted));
    };

    let brand = crate::brands::find_by_key(surreal, &row.brand_key)
        .await
        .map_err(|_| MirrorError::Source)?
        .ok_or(MirrorError::Source)?;
    if brand.firm_id != Some(row.firm_id) {
        return Err(MirrorError::Source);
    }
    super::tenancy::mirror(surreal, pg, &MirrorKey::Firm(row.firm_id)).await?;
    super::tenancy::mirror(surreal, pg, &MirrorKey::Brand(brand.id)).await?;

    let result = sqlx::query(
        "INSERT INTO social_account_selection (id, firm_id, brand_key, provider, provider_account_id, inserted_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (id) DO UPDATE SET firm_id=EXCLUDED.firm_id, brand_key=EXCLUDED.brand_key, \
         provider=EXCLUDED.provider, provider_account_id=EXCLUDED.provider_account_id, \
         inserted_at=EXCLUDED.inserted_at, updated_at=EXCLUDED.updated_at \
         WHERE (social_account_selection.firm_id, social_account_selection.brand_key, \
         social_account_selection.provider, social_account_selection.provider_account_id, \
         social_account_selection.inserted_at, social_account_selection.updated_at) IS DISTINCT FROM \
         (EXCLUDED.firm_id, EXCLUDED.brand_key, EXCLUDED.provider, EXCLUDED.provider_account_id, \
         EXCLUDED.inserted_at, EXCLUDED.updated_at)",
    )
    .bind(&row.id)
    .bind(row.firm_id)
    .bind(&row.brand_key)
    .bind(&row.provider)
    .bind(&row.provider_account_id)
    .bind(row.inserted_at)
    .bind(row.updated_at)
    .execute(&pg.0)
    .await?;
    Ok(changed(result.rows_affected(), Mirrored::Written))
}

pub async fn parity(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    Ok(ParityReport {
        tables: vec![compare(
            "social_account_selection",
            &source(surreal).await?,
            &target(pg).await?,
        )],
    })
}

pub async fn reconcile(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let source = source(surreal).await?;
    let target = target(pg).await?;
    for key in source
        .keys()
        .filter(|key| target.get(*key) != source.get(*key))
    {
        mirror(surreal, pg, key).await?;
    }
    for key in target.keys().filter(|key| !source.contains_key(*key)) {
        mirror(surreal, pg, key).await?;
    }
    parity(surreal, pg).await
}

#[cfg(test)]
#[path = "social_account_selections_tests.rs"]
mod tests;

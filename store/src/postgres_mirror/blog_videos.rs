//! PostgreSQL projection of a Brand's registered video uploads.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use sqlx::Row;
use surrealdb::types::{Datetime, RecordId, SurrealValue};
use uuid::Uuid;

use super::{changed, micros, MirrorError, MirrorKey, Mirrored, ParityReport, TableParity};
use crate::{
    postgres::PostgresDb,
    surreal::{record_id, record_uuid, SurrealDb},
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct Image {
    id: Uuid,
    brand_key: String,
    object_key: String,
    width: i64,
    height: i64,
    orientation: String,
    inserted_at: DateTime<Utc>,
}

#[derive(SurrealValue)]
struct Source {
    id: RecordId,
    brand_key: String,
    object_key: String,
    width: i64,
    height: i64,
    orientation: String,
    inserted_at: Datetime,
}

impl TryFrom<Source> for Image {
    type Error = MirrorError;

    fn try_from(row: Source) -> Result<Self, Self::Error> {
        if row.id.table.as_str() != "blog_video" || row.width <= 0 || row.height <= 0 {
            return Err(MirrorError::Source);
        }
        if !matches!(
            row.orientation.as_str(),
            "vertical" | "landscape" | "square"
        ) {
            return Err(MirrorError::Source);
        }
        Ok(Self {
            id: record_uuid(&row.id).ok_or(MirrorError::Source)?,
            brand_key: row.brand_key,
            object_key: row.object_key,
            width: row.width,
            height: row.height,
            orientation: row.orientation,
            inserted_at: micros(row.inserted_at.into()),
        })
    }
}

fn key(image: &Image) -> MirrorKey {
    MirrorKey::BlogVideo(image.id)
}

const FIELDS: &str = "id, brand_key, object_key, width, height, orientation, inserted_at";

async fn source_all(surreal: &SurrealDb) -> Result<BTreeMap<MirrorKey, Image>, MirrorError> {
    let rows: Vec<Source> = surreal
        .query(format!("SELECT {FIELDS} FROM blog_video"))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?
        .take(0)
        .map_err(|_| MirrorError::Source)?;
    rows.into_iter()
        .map(|row| {
            let image = Image::try_from(row)?;
            Ok((key(&image), image))
        })
        .collect()
}

async fn source_one(surreal: &SurrealDb, id: Uuid) -> Result<Option<Image>, MirrorError> {
    let mut response = surreal
        .query(format!("SELECT {FIELDS} FROM ONLY $id LIMIT 1"))
        .bind(("id", record_id("blog_video", id)))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?;
    let row: Option<Source> = response.take(0).map_err(|_| MirrorError::Source)?;
    row.map(Image::try_from).transpose()
}

fn from_row(row: &sqlx::postgres::PgRow) -> Result<Image, sqlx::Error> {
    Ok(Image {
        id: row.try_get("id")?,
        brand_key: row.try_get("brand_key")?,
        object_key: row.try_get("object_key")?,
        width: row.try_get("width")?,
        height: row.try_get("height")?,
        orientation: row.try_get("orientation")?,
        inserted_at: row.try_get("inserted_at")?,
    })
}

async fn target(pg: &PostgresDb) -> Result<BTreeMap<MirrorKey, Image>, MirrorError> {
    let rows = sqlx::query(
        "SELECT id, brand_key, object_key, width, height, orientation, inserted_at FROM blog_video",
    )
    .fetch_all(&pg.0)
    .await?;
    rows.into_iter()
        .map(|row| {
            let image = from_row(&row)?;
            Ok((key(&image), image))
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(Into::into)
}

async fn upsert(pg: &PostgresDb, image: &Image) -> Result<Mirrored, MirrorError> {
    let result = sqlx::query(
        "INSERT INTO blog_video (id, brand_key, object_key, width, height, orientation, inserted_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (id) DO UPDATE SET brand_key = EXCLUDED.brand_key, object_key = EXCLUDED.object_key, \
         width = EXCLUDED.width, height = EXCLUDED.height, orientation = EXCLUDED.orientation, \
         inserted_at = EXCLUDED.inserted_at \
         WHERE (blog_video.brand_key, blog_video.object_key, blog_video.width, blog_video.height, \
         blog_video.orientation, blog_video.inserted_at) IS DISTINCT FROM \
         (EXCLUDED.brand_key, EXCLUDED.object_key, EXCLUDED.width, EXCLUDED.height, \
         EXCLUDED.orientation, EXCLUDED.inserted_at)",
    )
    .bind(image.id)
    .bind(&image.brand_key)
    .bind(&image.object_key)
    .bind(image.width)
    .bind(image.height)
    .bind(&image.orientation)
    .bind(micros(image.inserted_at))
    .execute(&pg.0)
    .await?;
    Ok(changed(result.rows_affected(), Mirrored::Written))
}

async fn delete(pg: &PostgresDb, id: Uuid) -> Result<Mirrored, MirrorError> {
    let result = sqlx::query("DELETE FROM blog_video WHERE id = $1")
        .bind(id)
        .execute(&pg.0)
        .await?;
    Ok(changed(result.rows_affected(), Mirrored::Deleted))
}

pub(super) async fn mirror(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    id: Uuid,
) -> Result<Mirrored, MirrorError> {
    match source_one(surreal, id).await? {
        Some(image) => upsert(pg, &image).await,
        None => delete(pg, id).await,
    }
}

pub async fn parity(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let source = source_all(surreal).await?;
    let target = target(pg).await?;
    let mut divergent = source
        .iter()
        .filter(|(key, image)| target.get(*key) != Some(*image))
        .map(|(key, _)| key.clone())
        .chain(
            target
                .keys()
                .filter(|key| !source.contains_key(*key))
                .cloned(),
        )
        .collect::<Vec<_>>();
    divergent.sort();
    Ok(ParityReport {
        tables: vec![TableParity {
            table: "blog_video",
            source_rows: source.len(),
            target_rows: target.len(),
            divergent,
        }],
    })
}

pub async fn reconcile(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let source = source_all(surreal).await?;
    let target = target(pg).await?;
    for image in source.values() {
        upsert(pg, image).await?;
    }
    for key in target.keys().filter(|key| !source.contains_key(*key)) {
        let MirrorKey::BlogVideo(id) = key else {
            return Err(MirrorError::Source);
        };
        delete(pg, *id).await?;
    }
    parity(surreal, pg).await
}

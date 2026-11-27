//! PostgreSQL reference data for entity types and their jurisdiction links.

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::catalog;

/// One entity type.
///
/// The application-facing shape: plain Rust types, no engine handles.
/// [`EntityTypeRow`] is the seam that turns it into (and back out of)
/// what the SDK reads and writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EntityType {
    pub id: Uuid,
    /// Display name (`LLC`, `Trust`, `Human`). Unique.
    pub name: String,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Errors reading or writing an entity type.
#[derive(Debug, thiserror::Error)]
pub enum EntityTypeError {
    /// A database operation failed.
    /// The write collided with `entity_type_name` — another row already
    /// holds this name.
    #[error("that entity type name is already in use")]
    NameTaken,
    /// A write reported success but returned no row, or returned one
    /// this module could not read back — see
    /// [`EntityTypeRow::into_entity_type`].
    #[error("writing an entity type returned no usable row")]
    WriteReturnedNothing,
    /// The PostgreSQL catalog authority could not serve the call.
    #[error(transparent)]
    Postgres(#[from] catalog::CatalogError),
}

impl From<sqlx::Error> for EntityTypeError {
    fn from(error: sqlx::Error) -> Self {
        Self::Postgres(error.into())
    }
}

fn from_pg(row: &sqlx::postgres::PgRow) -> Result<EntityType, sqlx::Error> {
    use sqlx::Row;
    Ok(EntityType {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

const PG_BY_ID: &str = "SELECT id, name, inserted_at, updated_at FROM entity_type WHERE id = $1";
const PG_BY_NAME: &str =
    "SELECT id, name, inserted_at, updated_at FROM entity_type WHERE name = $1";
const PG_LIST_ASC: &str =
    "SELECT id, name, inserted_at, updated_at FROM entity_type ORDER BY name ASC";
const PG_LIST_DESC: &str =
    "SELECT id, name, inserted_at, updated_at FROM entity_type ORDER BY name DESC";
const PG_INSERT: &str =
    "INSERT INTO entity_type (id, name) VALUES ($1, $2) RETURNING id, name, inserted_at, updated_at";

/// Resolve an entity type by id — the read-back
/// `entity_commands::require_entity_type` performs before an entity
/// write stores the cross-engine reference.
///
/// # Errors
///
/// [`EntityTypeError::Db`] if the lookup fails.
pub async fn find_by_id(
    _db: &ApplicationStore,
    id: Uuid,
) -> Result<Option<EntityType>, EntityTypeError> {
    let pg = catalog::authority().await?;
    Ok(catalog::one(&pg, PG_BY_ID, id, from_pg).await?)
}

/// Resolve an entity type by its canonical `name` (`LLC`, `Human`).
/// Exact match — the names are the seed's canonical spellings.
///
/// # Errors
///
/// [`EntityTypeError::Db`] if the lookup fails.
pub async fn find_by_name(
    _db: &ApplicationStore,
    name: &str,
) -> Result<Option<EntityType>, EntityTypeError> {
    let pg = catalog::authority().await?;
    Ok(catalog::one(&pg, PG_BY_NAME, name.trim().to_string(), from_pg).await?)
}

/// List every entity type, ordered by the JSON:API `sort` spec (`(key,
/// descending)` pairs). The only advertised key is `name`; an empty
/// spec — or one that names no sortable column — falls back to
/// ascending name so the list is always stably ordered.
///
/// # Errors
///
/// [`EntityTypeError::Db`] if the lookup fails.
pub async fn list(
    _db: &ApplicationStore,
    sort: &[(String, bool)],
) -> Result<Vec<EntityType>, EntityTypeError> {
    let descending = sort
        .iter()
        .rev()
        .find_map(|(key, descending)| (key == "name").then_some(*descending))
        .unwrap_or(false);
    let pg = catalog::authority().await?;
    let sql = if descending {
        PG_LIST_DESC
    } else {
        PG_LIST_ASC
    };
    Ok(catalog::all(&pg, sql, None::<String>, from_pg).await?)
}

/// Write a new entity type row.
///
/// # Errors
///
/// [`EntityTypeError::NameTaken`] when another row already holds this
/// name, and [`EntityTypeError::Db`] for anything else.
pub async fn create(_db: &ApplicationStore, name: &str) -> Result<EntityType, EntityTypeError> {
    let id = Uuid::now_v7();
    let pg = catalog::authority().await?;
    let row = catalog::row(
        sqlx::query(PG_INSERT)
            .bind(id)
            .bind(name.trim().to_string()),
        &pg,
    )
    .await
    .map_err(catalog::CatalogError::from);
    match row {
        Ok(row) => Ok(from_pg(&row)?),
        Err(error) if error.violates("entity_type_name_key") => Err(EntityTypeError::NameTaken),
        Err(error) => Err(error.into()),
    }
}

/// Find the entity type holding `name`, creating it if absent.
/// Race-safe without a lock: a concurrent creator that wins the
/// `entity_type_name` unique index turns this call's insert into
/// [`EntityTypeError::NameTaken`], which is re-read as the winner's
/// row. The seed runs this on every boot, so idempotence is the
/// contract.
///
/// # Errors
///
/// [`EntityTypeError::Db`] if a lookup or the insert fails.
pub async fn find_or_create(
    db: &ApplicationStore,
    name: &str,
) -> Result<EntityType, EntityTypeError> {
    if let Some(existing) = find_by_name(db, name).await? {
        return Ok(existing);
    }
    match create(db, name).await {
        Ok(created) => Ok(created),
        Err(EntityTypeError::NameTaken) => find_by_name(db, name)
            .await?
            .ok_or(EntityTypeError::WriteReturnedNothing),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::{create, find_by_id, find_by_name, find_or_create, list, EntityTypeError};
    use crate::test_support::with_postgres;

    #[tokio::test]
    async fn a_created_entity_type_reads_back_by_id_and_name() {
        with_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let created = create(&db, "LLC").await.unwrap();
            assert_eq!(created.name, "LLC");

            assert_eq!(
                find_by_id(&db, created.id).await.unwrap(),
                Some(created.clone())
            );
            assert_eq!(
                find_by_name(&db, "LLC").await.unwrap(),
                Some(created.clone())
            );
            assert_eq!(find_by_name(&db, "Trust").await.unwrap(), None);
        })
        .await;
    }

    #[tokio::test]
    async fn a_duplicate_name_is_reported_as_the_name_being_taken() {
        with_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            create(&db, "LLC").await.unwrap();

            let duplicate = create(&db, "LLC").await;
            assert!(
                matches!(duplicate, Err(EntityTypeError::NameTaken)),
                "the unique `entity_type_name` index is the gate, got {duplicate:?}"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn find_or_create_is_idempotent_on_the_name() {
        with_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let first = find_or_create(&db, "Trust").await.unwrap();
            let second = find_or_create(&db, "Trust").await.unwrap();
            assert_eq!(first, second, "the second call returns the existing row");
            assert_eq!(list(&db, &[]).await.unwrap().len(), 1);
        })
        .await;
    }

    #[tokio::test]
    async fn listing_orders_by_name_and_honours_the_sort_spec() {
        with_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            for name in ["Trust", "Corporation", "LLC"] {
                create(&db, name).await.unwrap();
            }

            let names = |rows: Vec<super::EntityType>| -> Vec<String> {
                rows.into_iter().map(|t| t.name).collect()
            };

            // Empty spec → ascending name.
            assert_eq!(
                names(list(&db, &[]).await.unwrap()),
                ["Corporation", "LLC", "Trust"]
            );
            // Descending on the advertised key.
            assert_eq!(
                names(list(&db, &[("name".into(), true)]).await.unwrap()),
                ["Trust", "LLC", "Corporation"]
            );
            // A spec naming no sortable column falls back to ascending name.
            assert_eq!(
                names(list(&db, &[("bogus".into(), true)]).await.unwrap()),
                ["Corporation", "LLC", "Trust"]
            );
        })
        .await;
    }
}

//! Mail-receiving destinations, and every query against a `mailroom`
//! row.
//!
//! PostgreSQL enforces each Mailroom's unique name and Address relationship.
//! Source reads remain available for the historical import.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::addresses::{self, AddressError};
use crate::surreal::{record_id, record_uuid, SurrealDb};

/// The table these rows live in.
const TABLE: &str = "mailroom";

/// One mail-receiving destination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mailroom {
    pub id: Uuid,
    /// Unique.
    pub name: String,
    pub address_id: Uuid,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The row as the engine reads and writes it — the seam between
/// [`Mailroom`] and the SDK's own `RecordId` and `Datetime`.
#[derive(SurrealValue)]
struct MailroomRow {
    id: surrealdb::types::RecordId,
    name: String,
    address_id: surrealdb::types::RecordId,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

impl MailroomRow {
    /// `None` when a record id is not a native UUID key — a row written
    /// by something that bypassed [`crate::surreal::record_id`].
    fn into_mailroom(self) -> Option<Mailroom> {
        Some(Mailroom {
            id: record_uuid(&self.id)?,
            name: self.name,
            address_id: record_uuid(&self.address_id)?,
            inserted_at: self.inserted_at.into(),
            updated_at: self.updated_at.into(),
        })
    }
}

/// The projection every read shares, so one field list describes the row
/// and a new column cannot reach [`MailroomRow`] from only one query.
const SELECT: &str = "id, name, address_id, inserted_at, updated_at";

/// Errors reading or writing a mailroom.
#[derive(Debug, thiserror::Error)]
pub enum MailroomError {
    #[error(transparent)]
    Postgres(#[from] crate::postgres::matter::MatterAuthorityError),
    /// A database operation failed.
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    /// Reading the address a write links to failed.
    #[error(transparent)]
    Address(#[from] AddressError),
    /// The address named by a write does not exist. The engine would
    /// have accepted the dangling link; this is the check that does not.
    #[error("no address {0}")]
    NoSuchAddress(Uuid),
    /// The write collided with `mailroom_name` — another mailroom
    /// already holds this name.
    #[error("that mailroom name is already taken")]
    NameTaken,
    /// A write reported success but returned no row, or returned one
    /// this module could not read back.
    #[error("writing a mailroom returned no usable row")]
    WriteReturnedNothing,
}

/// Register a mailroom at `address_id`.
///
/// # Errors
///
/// [`MailroomError::NoSuchAddress`] when the address does not exist —
/// checked before writing —
/// [`MailroomError::NameTaken`] on a duplicate name, and
/// [`MailroomError::Db`] for anything else.
pub async fn create(
    db: &SurrealDb,
    name: &str,
    address_id: Uuid,
) -> Result<Mailroom, MailroomError> {
    let pg = crate::postgres::matter::authority_for(TABLE)
        .await?
        .ok_or(crate::postgres::matter::MatterAuthorityError::Unavailable)?;
    if addresses::find_by_id(db, address_id).await?.is_none() {
        return Err(MailroomError::NoSuchAddress(address_id));
    }
    let now = Utc::now();
    let result = crate::postgres::matter::upsert(
        &pg,
        TABLE,
        &serde_json::json!({
            "id": Uuid::now_v7(), "name": name, "address_id": address_id,
            "inserted_at": now, "updated_at": now,
        }),
    )
    .await;
    match result {
        Err(crate::postgres::matter::MatterAuthorityError::Conflict(ref index))
            if index == "mailroom_name_key" =>
        {
            Err(MailroomError::NameTaken)
        }
        value => value.map_err(MailroomError::from),
    }
}

/// Register a mailroom unless one already holds `name`.
///
/// Concurrent creators converge on the row protected by the unique name.
/// A caller that loses the race re-reads the winner's row.
///
/// `address_id` is only consulted when this call is the one that
/// creates. A caller that loses the race has already written a
/// placeholder address that nothing will point at — harmless
/// dev-portfolio data, and the alternative (wrapping both writes in one
/// transaction) buys tidiness the seed does not need.
///
/// # Errors
///
/// As [`create`], except that a lost name race resolves to the winning
/// row instead of [`MailroomError::NameTaken`].
pub async fn find_or_create(
    db: &SurrealDb,
    name: &str,
    address_id: Uuid,
) -> Result<Mailroom, MailroomError> {
    if let Some(existing) = find_by_name(db, name).await? {
        return Ok(existing);
    }
    match create(db, name, address_id).await {
        Ok(created) => Ok(created),
        Err(MailroomError::NameTaken) => find_by_name(db, name)
            .await?
            .ok_or(MailroomError::WriteReturnedNothing),
        Err(error) => Err(error),
    }
}

/// One mailroom by name. Exact match — the name is the natural key the
/// canonical seed and the letters seeder both resolve on.
///
/// # Errors
///
/// [`MailroomError::Db`] if the lookup fails.
pub async fn find_by_name(db: &SurrealDb, name: &str) -> Result<Option<Mailroom>, MailroomError> {
    if let Some(pg) = crate::postgres::matter::authority_for(TABLE).await? {
        return Ok(
            crate::postgres::matter::find(&pg, TABLE, serde_json::json!({"name": name})).await?,
        );
    }

    let mut response = db
        .query(format!(
            "SELECT {SELECT} FROM ONLY {TABLE} WHERE name = $name LIMIT 1"
        ))
        .bind(("name", name.to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let row: Option<MailroomRow> = response.take(0)?;
    Ok(row.and_then(MailroomRow::into_mailroom))
}

/// One mailroom by id.
///
/// # Errors
///
/// [`MailroomError::Db`] if the lookup fails.
pub async fn find_by_id(db: &SurrealDb, id: Uuid) -> Result<Option<Mailroom>, MailroomError> {
    if let Some(pg) = crate::postgres::matter::authority_for(TABLE).await? {
        return Ok(crate::postgres::matter::find(&pg, TABLE, serde_json::json!({"id": id})).await?);
    }

    let mut response = db
        .query(format!("SELECT {SELECT} FROM ONLY $id"))
        .bind(("id", record_id(TABLE, id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let row: Option<MailroomRow> = response.take(0)?;
    Ok(row.and_then(MailroomRow::into_mailroom))
}

/// Every mailroom, oldest first — a stable order for the lawyer listing
/// and for the inbound router, which routes through the first one.
///
/// # Errors
///
/// [`MailroomError::Db`] if the lookup fails.
pub async fn list_all(db: &SurrealDb) -> Result<Vec<Mailroom>, MailroomError> {
    if let Some(pg) = crate::postgres::matter::authority_for(TABLE).await? {
        return Ok(crate::postgres::matter::select(
            &pg,
            TABLE,
            serde_json::json!({}),
            &["inserted_at", "id"],
        )
        .await?);
    }

    let mut response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} ORDER BY inserted_at ASC, id ASC"
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<MailroomRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(MailroomRow::into_mailroom)
        .collect())
}

/// How many mailrooms exist. The production-emptiness gate asks this of
/// the engine that holds the table.
///
/// # Errors
///
/// [`MailroomError::Db`] if the count fails.
pub async fn count(db: &SurrealDb) -> Result<i64, MailroomError> {
    if let Some(pg) = crate::postgres::matter::authority_for(TABLE).await? {
        return sqlx::query_scalar("SELECT count(*) FROM mailroom")
            .fetch_one(pg.pool())
            .await
            .map_err(crate::postgres::matter::MatterAuthorityError::from)
            .map_err(MailroomError::from);
    }

    let mut response = db
        .query(format!("SELECT count() FROM {TABLE} GROUP ALL"))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let counts: Vec<CountRow> = response.take(0)?;
    Ok(counts.first().map_or(0, |c| c.count))
}

/// The one-field shape `SELECT count() ... GROUP ALL` returns.
#[derive(SurrealValue)]
struct CountRow {
    count: i64,
}

#[cfg(test)]
mod tests {
    use super::{count, create, find_by_id, find_by_name, find_or_create, list_all, MailroomError};
    use crate::addresses::{self, NewAddress};
    use crate::surreal::SurrealDb;
    use crate::test_support;
    use uuid::Uuid;

    async fn an_address(db: &SurrealDb, line1: &str) -> Uuid {
        addresses::create(
            db,
            &NewAddress {
                line1: line1.to_string(),
                city: "Reno".into(),
                region: "NV".into(),
                postal_code: "89501".into(),
                country: "US".into(),
                ..NewAddress::default()
            },
        )
        .await
        .unwrap()
        .id
    }

    #[tokio::test]
    async fn a_created_mailroom_reads_back_by_name_and_id() {
        test_support::with_required_postgres(async {
            let db = SurrealDb::uninitialized();
            let address_id = an_address(&db, "123 Main St").await;

            let created = create(&db, "HQ", address_id).await.unwrap();
            assert_eq!(created.name, "HQ");
            assert_eq!(created.address_id, address_id);

            assert_eq!(
                find_by_name(&db, "HQ").await.unwrap(),
                Some(created.clone())
            );
            assert_eq!(find_by_id(&db, created.id).await.unwrap(), Some(created));
            assert_eq!(find_by_name(&db, "Nowhere").await.unwrap(), None);
        })
        .await;
    }

    #[tokio::test]
    async fn a_mailroom_at_an_address_that_does_not_exist_is_refused() {
        test_support::with_required_postgres(async {
            let db = SurrealDb::uninitialized();
            let nowhere = Uuid::now_v7();

            let refused = create(&db, "HQ", nowhere).await;
            assert!(matches!(
                refused,
                Err(MailroomError::NoSuchAddress(id)) if id == nowhere
            ));
        })
        .await;
    }

    #[tokio::test]
    async fn the_engine_itself_would_have_accepted_the_dangling_link() {
        let db = crate::surreal::test_support::mem().await;
        // Source rows can contain dangling links, so import must handle them.
        let dangling = record_id_of_nothing();
        db.query("CREATE $id SET name = 'Ghost', address_id = $address_id")
            .bind(("id", crate::surreal::record_id("mailroom", Uuid::now_v7())))
            .bind(("address_id", dangling))
            .await
            .and_then(surrealdb::IndexedResults::check)
            .expect("the engine accepts a link to an address that was never written");

        assert!(
            crate::postgres_mirror::from_source(find_by_name(&db, "Ghost"))
                .await
                .unwrap()
                .is_some(),
            "and the row is readable, pointing at nothing"
        );
    }

    fn record_id_of_nothing() -> surrealdb::types::RecordId {
        crate::surreal::record_id("address", Uuid::now_v7())
    }

    #[tokio::test]
    async fn a_duplicate_name_is_reported_as_taken() {
        test_support::with_required_postgres(async {
            let db = SurrealDb::uninitialized();
            let first = an_address(&db, "123 Main St").await;
            let second = an_address(&db, "456 Side St").await;

            create(&db, "HQ", first).await.unwrap();
            let duplicate = create(&db, "HQ", second).await;
            assert!(
                matches!(duplicate, Err(MailroomError::NameTaken)),
                "the unique `mailroom_name` index is the gate, got {duplicate:?}"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn find_or_create_is_idempotent_on_the_name() {
        test_support::with_required_postgres(async {
            let db = SurrealDb::uninitialized();
            let first_address = an_address(&db, "123 Main St").await;
            let second_address = an_address(&db, "456 Side St").await;

            let first = find_or_create(&db, "HQ", first_address).await.unwrap();
            let second = find_or_create(&db, "HQ", second_address).await.unwrap();

            assert_eq!(first, second, "the second call returns the existing row");
            assert_eq!(
                second.address_id, first_address,
                "an existing mailroom keeps its address rather than being repointed"
            );
            assert_eq!(count(&db).await.unwrap(), 1);
        })
        .await;
    }

    #[tokio::test]
    async fn concurrent_seeders_do_not_collide_on_the_name() {
        test_support::with_required_postgres(async {
            let db = SurrealDb::uninitialized();
            // Concurrent seeders must converge on one name.
            let racers = (0..6).map(|n| {
                let db = db.clone();
                async move {
                    let address = an_address(&db, &format!("{n} Main St")).await;
                    find_or_create(&db, "HQ", address).await.unwrap().id
                }
            });
            let ids = futures::future::join_all(racers).await;
            assert_eq!(count(&db).await.unwrap(), 1, "one mailroom, not six");
            assert!(
                ids.windows(2).all(|w| w[0] == w[1]),
                "every racer resolves to the same row: {ids:?}"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn listing_is_ordered_oldest_first_so_inbound_routing_is_stable() {
        test_support::with_required_postgres(async {
            let db = SurrealDb::uninitialized();
            let a = an_address(&db, "123 Main St").await;
            let b = an_address(&db, "456 Side St").await;
            let first = create(&db, "HQ", a).await.unwrap();
            let second = create(&db, "Annex", b).await.unwrap();

            // The inbound-email router takes the first mailroom, so this
            // order is behaviour, not cosmetics.
            let ids: Vec<_> = list_all(&db)
                .await
                .unwrap()
                .into_iter()
                .map(|m| m.id)
                .collect();
            assert_eq!(ids, [first.id, second.id]);
        })
        .await;
    }

    #[tokio::test]
    async fn counting_an_empty_table_is_zero_rather_than_an_error() {
        test_support::with_required_postgres(async {
            let db = SurrealDb::uninitialized();
            assert_eq!(count(&db).await.unwrap(), 0);
            let address_id = an_address(&db, "123 Main St").await;
            create(&db, "HQ", address_id).await.unwrap();
            assert_eq!(count(&db).await.unwrap(), 1);
        })
        .await;
    }
}

//! Postal addresses, and every query against an `address` row.
//!
//! # Person and Entity references
//!
//! `person_id` and `entity_id` are nullable PostgreSQL foreign keys. An
//! address may belong to either, both, or neither: the mailroom placeholder
//! has no Person or Entity. `create` checks the Person first so a missing
//! Person produces [`AddressError::NoSuchPerson`]; PostgreSQL enforces both
//! references when it writes the row.

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use sqlx::{postgres::PgRow, AssertSqlSafe, Row};

use crate::application_store::ApplicationStore;
use crate::persons::{self, PersonError};
use crate::postgres::identity::AuthorityError;
use crate::postgres::PostgresDb;

/// The table these rows live in.
const TABLE: &str = "address";
/// One postal address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Address {
    pub id: Uuid,
    /// The person this address belongs to, if any.
    pub person_id: Option<Uuid>,
    /// The entity this address belongs to, if any.
    pub entity_id: Option<Uuid>,
    pub line1: String,
    pub line2: Option<String>,
    pub city: String,
    pub region: String,
    pub postal_code: String,
    pub country: String,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Everything [`create`] needs. A struct rather than nine positional
/// arguments, four of which are optional or empty strings.
#[derive(Debug, Clone, Default)]
pub struct NewAddress {
    pub person_id: Option<Uuid>,
    pub entity_id: Option<Uuid>,
    pub line1: String,
    pub line2: Option<String>,
    pub city: String,
    pub region: String,
    pub postal_code: String,
    pub country: String,
}

const SELECT: &str = "id, person_id, entity_id, line1, line2, city, region, postal_code, \
                      country, inserted_at, updated_at";

/// Errors reading or writing an address.
#[derive(Debug, thiserror::Error)]
pub enum AddressError {
    /// Reading the person a write links to failed.
    #[error(transparent)]
    Person(#[from] PersonError),
    /// The Person named by a write does not exist.
    #[error("no person {0}")]
    NoSuchPerson(Uuid),
    /// PostgreSQL identity authority is configured and failed.
    #[error(transparent)]
    Authority(#[from] AuthorityError),
}

fn from_pg(row: &PgRow) -> Result<Address, sqlx::Error> {
    Ok(Address {
        id: row.try_get("id")?,
        person_id: row.try_get("person_id")?,
        entity_id: row.try_get("entity_id")?,
        line1: row.try_get("line1")?,
        line2: row.try_get("line2")?,
        city: row.try_get("city")?,
        region: row.try_get("region")?,
        postal_code: row.try_get("postal_code")?,
        country: row.try_get("country")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// Addresses from PostgreSQL matching `predicate` over an optional UUID,
/// oldest first.
async fn pg_rows(
    pg: &crate::postgres::PostgresDb,
    predicate: &str,
    id: Option<Uuid>,
) -> Result<Vec<Address>, AddressError> {
    let mut query = sqlx::query(AssertSqlSafe(format!(
        "SELECT {SELECT} FROM {TABLE} WHERE {predicate} ORDER BY inserted_at, id"
    )));
    if let Some(id) = id {
        query = query.bind(id);
    }
    let rows = query.fetch_all(&pg.0).await.map_err(AuthorityError::from)?;
    Ok(rows
        .iter()
        .map(from_pg)
        .collect::<Result<_, _>>()
        .map_err(AuthorityError::from)?)
}

async fn authority() -> Result<PostgresDb, AuthorityError> {
    PostgresDb::application()
        .await
        .map_err(|error| AuthorityError::Unavailable(error.to_string()))
}

/// Write a new address under a fresh v7 UUID record id.
///
/// # Errors
///
/// [`AddressError::NoSuchPerson`] when `person_id` names no Person. The
/// check returns the domain error before PostgreSQL enforces the foreign key.
/// [`AddressError::Authority`] when the insert fails.
pub async fn create(db: &ApplicationStore, new: &NewAddress) -> Result<Address, AddressError> {
    if let Some(person_id) = new.person_id {
        if persons::find_by_id(db, person_id).await?.is_none() {
            return Err(AddressError::NoSuchPerson(person_id));
        }
    }

    let id = Uuid::now_v7();
    let pg = authority().await?;
    let row = sqlx::query(AssertSqlSafe(format!(
        "INSERT INTO {TABLE} (id, person_id, entity_id, line1, line2, city, region, \
         postal_code, country) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING {SELECT}"
    )))
    .bind(id)
    .bind(new.person_id)
    .bind(new.entity_id)
    .bind(&new.line1)
    .bind(&new.line2)
    .bind(&new.city)
    .bind(&new.region)
    .bind(&new.postal_code)
    .bind(&new.country)
    .fetch_one(&pg.0)
    .await
    .map_err(AuthorityError::from)?;
    Ok(from_pg(&row).map_err(AuthorityError::from)?)
}

/// One address by id.
///
/// # Errors
///
/// [`AddressError::Authority`] if the lookup fails.
pub async fn find_by_id(_db: &ApplicationStore, id: Uuid) -> Result<Option<Address>, AddressError> {
    let pg = authority().await?;
    Ok(Box::pin(pg_rows(&pg, "id = $1", Some(id)))
        .await?
        .into_iter()
        .next())
}

/// Every address belonging to one Entity, oldest first.
///
/// # Errors
///
/// [`AddressError::Authority`] if the lookup fails.
pub async fn for_entity(
    _db: &ApplicationStore,
    entity_id: Uuid,
) -> Result<Vec<Address>, AddressError> {
    let pg = authority().await?;
    Box::pin(pg_rows(&pg, "entity_id = $1", Some(entity_id))).await
}

/// Every address belonging to one person, oldest first — the counterpart
/// of [`for_entity`] (LAW-57).
///
/// # Errors
///
/// [`AddressError::Authority`] if the lookup fails.
pub async fn for_person(
    _db: &ApplicationStore,
    person_id: Uuid,
) -> Result<Vec<Address>, AddressError> {
    let pg = authority().await?;
    Box::pin(pg_rows(&pg, "person_id = $1", Some(person_id))).await
}

/// Find this entity's address at `line1`/`postal_code`, creating it if
/// absent — the canonical seed's idempotence contract, on the natural
/// key the seeder matches on.
///
/// There is no unique index behind this, so two concurrent seeders could
/// both miss the read and both insert. The seed is single-writer per
/// boot and the duplicate would be cosmetic rather than corrupting,
/// which is why this does not carry the re-read a unique index would
/// make possible.
///
/// # Errors
///
/// [`AddressError::Authority`] if a lookup or the insert fails.
pub async fn find_or_create_for_entity(
    db: &ApplicationStore,
    new: &NewAddress,
) -> Result<(Address, bool), AddressError> {
    if let Some(entity_id) = new.entity_id {
        let existing = for_entity(db, entity_id)
            .await?
            .into_iter()
            .find(|a| a.line1 == new.line1 && a.postal_code == new.postal_code);
        if let Some(found) = existing {
            return Ok((found, false));
        }
    }
    Ok((create(db, new).await?, true))
}

/// [`find_or_create_for_entity`]'s counterpart for a person-linked address
/// (LAW-57) — `navigator site import`'s natural key for the `address`
/// seed model.
///
/// # Errors
///
/// [`AddressError::NoSuchPerson`] when `new.person_id` names no person, and
/// [`AddressError::Authority`] if a lookup or the insert fails.
pub async fn find_or_create_for_person(
    db: &ApplicationStore,
    new: &NewAddress,
) -> Result<(Address, bool), AddressError> {
    if let Some(person_id) = new.person_id {
        let existing = for_person(db, person_id)
            .await?
            .into_iter()
            .find(|a| a.line1 == new.line1 && a.postal_code == new.postal_code);
        if let Some(found) = existing {
            return Ok((found, false));
        }
    }
    Ok((create(db, new).await?, true))
}

/// Every address, oldest first. The whole table, for listings and tests.
///
/// # Errors
///
/// [`AddressError::Authority`] if the lookup fails.
pub async fn list_all(_db: &ApplicationStore) -> Result<Vec<Address>, AddressError> {
    let pg = authority().await?;
    Box::pin(pg_rows(&pg, "true", None)).await
}

/// How many addresses exist. The production-emptiness gate asks this of
/// the engine that holds the table.
///
/// # Errors
///
/// [`AddressError::Authority`] if the count fails.
pub async fn count(_db: &ApplicationStore) -> Result<i64, AddressError> {
    let pg = authority().await?;
    Ok(
        sqlx::query_scalar(AssertSqlSafe(format!("SELECT count(*) FROM {TABLE}")))
            .fetch_one(&pg.0)
            .await
            .map_err(AuthorityError::from)?,
    )
}

#[cfg(test)]
mod tests {
    use super::{
        count, create, find_by_id, find_or_create_for_entity, find_or_create_for_person,
        for_entity, for_person, list_all, Address, AddressError, NewAddress,
    };
    use crate::persons::{self, NewPerson};
    use uuid::Uuid;

    fn at(line1: &str) -> NewAddress {
        NewAddress {
            line1: line1.to_string(),
            city: "Las Vegas".to_string(),
            region: "NV".to_string(),
            postal_code: "89101".to_string(),
            country: "US".to_string(),
            ..NewAddress::default()
        }
    }

    #[tokio::test]
    async fn a_created_address_reads_back_by_id() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let written = create(&db, &at("1 Fremont St")).await.unwrap();
            assert_eq!(written.line1, "1 Fremont St");
            assert_eq!(written.city, "Las Vegas");
            assert_eq!(find_by_id(&db, written.id).await.unwrap(), Some(written));
            assert_eq!(find_by_id(&db, Uuid::now_v7()).await.unwrap(), None);
        })
        .await;
    }

    #[tokio::test]
    async fn an_address_may_belong_to_a_person() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let person = persons::create(&db, &NewPerson::new("Libra", "libra@example.com"))
                .await
                .unwrap();
            let mut new = at("1 Fremont St");
            new.person_id = Some(person.id);

            let written = create(&db, &new).await.unwrap();
            assert_eq!(written.person_id, Some(person.id));
        })
        .await;
    }

    #[tokio::test]
    async fn an_address_for_a_person_who_does_not_exist_is_refused() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let nobody = Uuid::now_v7();
            let mut new = at("1 Fremont St");
            new.person_id = Some(nobody);

            // The accessor returns a domain error before PostgreSQL applies
            // the Person foreign key.
            let refused = create(&db, &new).await;
            assert!(matches!(
                refused,
                Err(AddressError::NoSuchPerson(id)) if id == nobody
            ));
        })
        .await;
    }

    #[tokio::test]
    async fn an_address_may_belong_to_neither_a_person_nor_an_entity() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            // The seed's mailroom placeholder is exactly this row, which is
            // why the schema carries no XOR assert.
            let written = create(&db, &at("(via mailroom: Reno)")).await.unwrap();
            assert_eq!(written.person_id, None);
            assert_eq!(written.entity_id, None);
        })
        .await;
    }

    #[tokio::test]
    async fn an_entity_id_is_stored_and_filtered_for_existing_entities() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let entity_id = crate::test_support::seed_entity(&db).await;
            let mut new = at("1 Fremont St");
            new.entity_id = Some(entity_id);
            let written = create(&db, &new).await.unwrap();

            assert_eq!(for_entity(&db, entity_id).await.unwrap(), vec![written]);
            assert!(for_entity(&db, Uuid::now_v7()).await.unwrap().is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn find_or_create_matches_the_seeders_natural_key() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let entity_id = crate::test_support::seed_entity(&db).await;
            let mut new = at("1 Fremont St");
            new.entity_id = Some(entity_id);

            let (first, created) = find_or_create_for_entity(&db, &new).await.unwrap();
            assert!(created, "the first call inserts");
            let (second, created_again) = find_or_create_for_entity(&db, &new).await.unwrap();
            assert!(!created_again, "the second call finds");
            assert_eq!(first, second);
            assert_eq!(list_all(&db).await.unwrap().len(), 1);

            // A different street at the same entity is a different address.
            let mut other = at("2 Fremont St");
            other.entity_id = Some(entity_id);
            let (_, created_other) = find_or_create_for_entity(&db, &other).await.unwrap();
            assert!(created_other);
            assert_eq!(list_all(&db).await.unwrap().len(), 2);
        })
        .await;
    }

    /// An entity keeps as many addresses as it answers mail at, and reading
    /// them back returns every one.
    ///
    /// This is a capability, not an accident: the `address.entity_id`
    /// column has no unique constraint, and `find_or_create_for_entity` is a seeder's natural key rather
    /// than a one-address-per-entity constraint.
    /// The brand seed leans on that to give one partnership a box in four
    /// states, so a change that makes an entity hold a single address breaks
    /// the firm's own postal identity. Asserted here at the store seam rather
    /// than only against seeded data, because the guarantee belongs to the
    /// table and not to one YAML file.
    #[tokio::test]
    async fn an_entity_keeps_every_address_it_answers_mail_at() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let entity_id = crate::test_support::seed_entity(&db).await;
            let in_state = |line1: &str, region: &str, zip: &str| {
                let mut new = at(line1);
                new.entity_id = Some(entity_id);
                new.region = region.to_string();
                new.postal_code = zip.to_string();
                new
            };
            let boxes = [
                in_state("5150 Mae Anne Ave Ste 405-9777", "NV", "89523"),
                in_state("1990 N California Blvd Ste 800", "CA", "94596"),
                in_state("12 E 49th St 18th Floor", "NY", "10017"),
                in_state("720 Seneca St Ste 107-715", "WA", "98101"),
            ];
            for one in &boxes {
                assert!(
                    find_or_create_for_entity(&db, one).await.unwrap().1,
                    "each jurisdiction inserts its own row"
                );
            }

            let mut held: Vec<String> = for_entity(&db, entity_id)
                .await
                .unwrap()
                .into_iter()
                .map(|a| a.region)
                .collect();
            held.sort();
            assert_eq!(
                held,
                ["CA", "NV", "NY", "WA"],
                "for_entity returns every address the entity holds, not the first"
            );

            // Re-seeding finds all four rather than duplicating any: holding
            // several addresses must not cost idempotency.
            for one in &boxes {
                assert!(!find_or_create_for_entity(&db, one).await.unwrap().1);
            }
            assert_eq!(for_entity(&db, entity_id).await.unwrap().len(), 4);

            // The natural key is the (street, ZIP) pair, so one street in two ZIPs
            // is two addresses. Matching on street alone would silently swallow
            // the second.
            let same_street_elsewhere = in_state("12 E 49th St 18th Floor", "NY", "10017-2452");
            assert!(
                find_or_create_for_entity(&db, &same_street_elsewhere)
                    .await
                    .unwrap()
                    .1
            );
            assert_eq!(for_entity(&db, entity_id).await.unwrap().len(), 5);
        })
        .await;
    }

    #[tokio::test]
    async fn counting_an_empty_table_is_zero_rather_than_an_error() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            assert_eq!(count(&db).await.unwrap(), 0);
            create(&db, &at("1 Fremont St")).await.unwrap();
            create(&db, &at("2 Fremont St")).await.unwrap();
            assert_eq!(count(&db).await.unwrap(), 2);
        })
        .await;
    }

    #[tokio::test]
    async fn listing_is_ordered_oldest_first() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let first = create(&db, &at("1 Fremont St")).await.unwrap();
            let second = create(&db, &at("2 Fremont St")).await.unwrap();
            let ids: Vec<_> = list_all(&db)
                .await
                .unwrap()
                .into_iter()
                .map(|a: Address| a.id)
                .collect();
            assert_eq!(ids, [first.id, second.id]);
        })
        .await;
    }

    /// [`for_entity`]'s counterpart for a person (LAW-57).
    #[tokio::test]
    async fn for_person_returns_every_address_that_person_holds() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let person = persons::create(&db, &NewPerson::new("Libra", "libra@example.com"))
                .await
                .unwrap();
            let mut home = at("1 Fremont St");
            home.person_id = Some(person.id);
            let written = create(&db, &home).await.unwrap();

            assert_eq!(for_person(&db, person.id).await.unwrap(), vec![written]);
            assert!(for_person(&db, Uuid::now_v7()).await.unwrap().is_empty());
        })
        .await;
    }

    /// `navigator site import`'s natural key for the `address` seed model
    /// (LAW-57): a person's (street, ZIP) pair.
    #[tokio::test]
    async fn find_or_create_for_person_matches_the_seeders_natural_key() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let person = persons::create(&db, &NewPerson::new("Libra", "libra@example.com"))
                .await
                .unwrap();
            let mut new = at("1 Fremont St");
            new.person_id = Some(person.id);

            let (first, created) = find_or_create_for_person(&db, &new).await.unwrap();
            assert!(created, "the first call inserts");
            let (second, created_again) = find_or_create_for_person(&db, &new).await.unwrap();
            assert!(!created_again, "the second call finds");
            assert_eq!(first, second);
            assert_eq!(for_person(&db, person.id).await.unwrap().len(), 1);
        })
        .await;
    }

    #[tokio::test]
    async fn find_or_create_for_person_refuses_a_person_who_does_not_exist() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let nobody = Uuid::now_v7();
            let mut new = at("1 Fremont St");
            new.person_id = Some(nobody);

            let refused = find_or_create_for_person(&db, &new).await;
            assert!(matches!(
                refused,
                Err(AddressError::NoSuchPerson(id)) if id == nobody
            ));
        })
        .await;
    }
}

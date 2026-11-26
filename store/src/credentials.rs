//! A person's licensure in a jurisdiction — the pair that makes an
//! attorney an attorney in a state.
//!
//! PostgreSQL owns credential writes. Explicit Source reads support historical
//! import. [`grant`] checks the Person and jurisdiction to return a precise
//! error before the database foreign keys enforce those references.
//! The canonical seed uses [`find_or_grant`] without overwriting a licence.

use chrono::{DateTime, Utc};
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use sqlx::{postgres::PgRow, AssertSqlSafe, Row};

use crate::jurisdictions::{self, JurisdictionError};
use crate::persons::{self, PersonError};
use crate::postgres::identity::{self as authority, AuthorityError};
use crate::surreal::{record_id, record_uuid, SurrealDb};

/// The table these rows live in.
const TABLE: &str = "credential";
/// The table `person_id` links into.
const PERSON_TABLE: &str = "person";
/// The table `jurisdiction_id` links into.
const JURISDICTION_TABLE: &str = "jurisdiction";

/// Errors reading or writing a credential.
#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    /// A database operation failed.
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    /// Reading the person a write links to failed.
    #[error(transparent)]
    Person(#[from] PersonError),
    /// Reading the jurisdiction a write links to failed.
    #[error(transparent)]
    Jurisdiction(#[from] JurisdictionError),
    /// The person named by a write does not exist.
    #[error("no person {0}")]
    NoSuchPerson(Uuid),
    /// The jurisdiction named by a write does not exist — same story as
    /// [`CredentialError::NoSuchPerson`], on the other end of the pair.
    #[error("no jurisdiction {0}")]
    NoSuchJurisdiction(Uuid),
    /// This person is already listed under this jurisdiction — the
    /// write collided with `credential_person_jurisdiction`.
    #[error("that person already holds a credential in that jurisdiction")]
    AlreadyGranted,
    /// A concurrent grant won the unique constraint but could not be read back.
    #[error("writing a credential returned no usable row")]
    WriteReturnedNothing,
    /// PostgreSQL identity authority is configured and failed.
    #[error(transparent)]
    Authority(#[from] AuthorityError),
}

fn from_pg(row: &PgRow) -> Result<Credential, sqlx::Error> {
    Ok(Credential {
        id: row.try_get("id")?,
        person_id: row.try_get("person_id")?,
        jurisdiction_id: row.try_get("jurisdiction_id")?,
        license_number: row.try_get("license_number")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn pg_error(error: sqlx::Error) -> CredentialError {
    if authority::violates(&error, "credential_person_id_jurisdiction_id_key") {
        CredentialError::AlreadyGranted
    } else {
        CredentialError::Authority(error.into())
    }
}

/// Credential writes require admitted PostgreSQL identity authority.
async fn write_authority() -> Result<crate::postgres::PostgresDb, CredentialError> {
    authority::authority().await?.ok_or_else(|| {
        AuthorityError::Unavailable("credential writes are unavailable in Source context".into())
            .into()
    })
}

/// One licensure record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credential {
    pub id: Uuid,
    pub person_id: Uuid,
    pub jurisdiction_id: Uuid,
    pub license_number: String,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The row as the engine reads and writes it. Separate from
/// [`Credential`] because the SDK owns its own `RecordId` and
/// `Datetime`, and the conversion belongs at this seam rather than in
/// every caller.
#[derive(SurrealValue)]
struct CredentialRow {
    id: surrealdb::types::RecordId,
    person_id: surrealdb::types::RecordId,
    jurisdiction_id: surrealdb::types::RecordId,
    license_number: String,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

impl CredentialRow {
    /// `None` when any record id is not a native UUID key — see
    /// [`crate::surreal`] for why the two key spellings differ.
    fn into_credential(self) -> Option<Credential> {
        Some(Credential {
            id: record_uuid(&self.id)?,
            person_id: record_uuid(&self.person_id)?,
            jurisdiction_id: record_uuid(&self.jurisdiction_id)?,
            license_number: self.license_number,
            inserted_at: self.inserted_at.into(),
            updated_at: self.updated_at.into(),
        })
    }
}

/// The projection every read shares, so one field list describes the row
/// and a new column cannot reach [`CredentialRow`] from only one query.
const SELECT: &str = "id, person_id, jurisdiction_id, license_number, inserted_at, updated_at";

/// Record that `person_id` is licensed in `jurisdiction_id`.
///
/// # Errors
///
/// [`CredentialError::NoSuchPerson`] when the person does not exist and
/// [`CredentialError::NoSuchJurisdiction`] when the jurisdiction does
/// not — both checked before the database foreign keys. [`CredentialError::AlreadyGranted`] when this person
/// is already listed under this jurisdiction, and
/// [`CredentialError::Authority`] for a database failure.
pub async fn grant(
    db: &SurrealDb,
    person_id: Uuid,
    jurisdiction_id: Uuid,
    license_number: &str,
) -> Result<Credential, CredentialError> {
    let pg = write_authority().await?;
    if persons::find_by_id(db, person_id).await?.is_none() {
        return Err(CredentialError::NoSuchPerson(person_id));
    }
    if jurisdictions::find_by_id(db, jurisdiction_id)
        .await?
        .is_none()
    {
        return Err(CredentialError::NoSuchJurisdiction(jurisdiction_id));
    }

    let id = Uuid::now_v7();
    let row = sqlx::query(AssertSqlSafe(format!(
        "INSERT INTO {TABLE} (id, person_id, jurisdiction_id, license_number) \
             VALUES ($1, $2, $3, $4) RETURNING {SELECT}"
    )))
    .bind(id)
    .bind(person_id)
    .bind(jurisdiction_id)
    .bind(license_number)
    .fetch_one(&pg.0)
    .await
    .map_err(pg_error)?;
    let credential = from_pg(&row).map_err(pg_error)?;

    Ok(credential)
}

/// The credential pairing this person with this jurisdiction, if there
/// is one. The pair is the natural key the unique index carries.
///
/// # Errors
///
/// [`CredentialError::Db`] if the lookup fails.
pub async fn find_by_person_and_jurisdiction(
    db: &SurrealDb,
    person_id: Uuid,
    jurisdiction_id: Uuid,
) -> Result<Option<Credential>, CredentialError> {
    if let Some(pg) = authority::authority().await? {
        let row = sqlx::query(AssertSqlSafe(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE person_id = $1 AND jurisdiction_id = $2"
        )))
        .bind(person_id)
        .bind(jurisdiction_id)
        .fetch_optional(&pg.0)
        .await
        .map_err(pg_error)?;
        return row.as_ref().map(from_pg).transpose().map_err(pg_error);
    }
    let mut response = db
        .query(format!(
            "SELECT {SELECT} FROM ONLY {TABLE} \
             WHERE person_id = $person_id AND jurisdiction_id = $jurisdiction_id \
             LIMIT 1"
        ))
        .bind(("person_id", record_id(PERSON_TABLE, person_id)))
        .bind((
            "jurisdiction_id",
            record_id(JURISDICTION_TABLE, jurisdiction_id),
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)?;

    let row: Option<CredentialRow> = response.take(0)?;
    Ok(row.and_then(CredentialRow::into_credential))
}

/// Grant the credential unless this person already holds one in this
/// jurisdiction. Race-safe without a lock: a concurrent grantor that
/// wins the `credential_person_jurisdiction` unique index turns this
/// call's insert into [`CredentialError::AlreadyGranted`], which is
/// re-read as the winner's row. The canonical seed runs this on every
/// boot, so idempotence is the contract.
///
/// An existing row is returned as it stands — a differing
/// `license_number` does not overwrite it. The seed is the only caller,
/// and a licence number that changed in the canonical YAML is a data
/// correction someone should make deliberately, not a silent effect of
/// the next boot.
///
/// # Errors
///
/// [`CredentialError::NoSuchPerson`] and
/// [`CredentialError::NoSuchJurisdiction`] as [`grant`] documents, and
/// [`CredentialError::Db`] if a lookup or the insert fails.
pub async fn find_or_grant(
    db: &SurrealDb,
    person_id: Uuid,
    jurisdiction_id: Uuid,
    license_number: &str,
) -> Result<Credential, CredentialError> {
    if let Some(existing) = find_by_person_and_jurisdiction(db, person_id, jurisdiction_id).await? {
        return Ok(existing);
    }
    match grant(db, person_id, jurisdiction_id, license_number).await {
        Ok(granted) => Ok(granted),
        Err(CredentialError::AlreadyGranted) => {
            find_by_person_and_jurisdiction(db, person_id, jurisdiction_id)
                .await?
                .ok_or(CredentialError::WriteReturnedNothing)
        }
        Err(error) => Err(error),
    }
}

/// Every credential held by one person, oldest first.
///
/// # Errors
///
/// [`CredentialError::Db`] if the lookup fails.
pub async fn for_person(
    db: &SurrealDb,
    person_id: Uuid,
) -> Result<Vec<Credential>, CredentialError> {
    if let Some(pg) = authority::authority().await? {
        let rows = sqlx::query(AssertSqlSafe(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE person_id = $1 ORDER BY inserted_at, id"
        )))
        .bind(person_id)
        .fetch_all(&pg.0)
        .await
        .map_err(pg_error)?;
        return rows
            .iter()
            .map(from_pg)
            .collect::<Result<_, _>>()
            .map_err(pg_error);
    }
    let mut response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE person_id = $person_id ORDER BY inserted_at ASC"
        ))
        .bind(("person_id", record_id(PERSON_TABLE, person_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;

    let rows: Vec<CredentialRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(CredentialRow::into_credential)
        .collect())
}

/// Whether this person is licensed in this jurisdiction — the question a
/// matter-open attestation asks before letting an attorney take work in
/// a state.
///
/// # Errors
///
/// [`CredentialError::Db`] if the lookup fails.
pub async fn is_licensed_in(
    db: &SurrealDb,
    person_id: Uuid,
    jurisdiction_id: Uuid,
) -> Result<bool, CredentialError> {
    if authority::authority().await?.is_some() {
        return Ok(
            find_by_person_and_jurisdiction(db, person_id, jurisdiction_id)
                .await?
                .is_some(),
        );
    }
    let mut response = db
        .query(format!(
            "SELECT VALUE count() FROM {TABLE} \
             WHERE person_id = $person_id AND jurisdiction_id = $jurisdiction_id \
             GROUP ALL"
        ))
        .bind(("person_id", record_id(PERSON_TABLE, person_id)))
        .bind((
            "jurisdiction_id",
            record_id(JURISDICTION_TABLE, jurisdiction_id),
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)?;

    let counts: Vec<i64> = response.take(0)?;
    Ok(counts.first().is_some_and(|count| *count > 0))
}

/// Remove a credential — a licence lapsed or was recorded in error.
/// Idempotent: revoking one that is not there is a no-op.
///
/// # Errors
///
/// [`CredentialError::Authority`] if the delete fails.
pub async fn revoke(_db: &SurrealDb, credential_id: Uuid) -> Result<(), CredentialError> {
    let pg = write_authority().await?;
    sqlx::query(AssertSqlSafe(format!("DELETE FROM {TABLE} WHERE id = $1")))
        .bind(credential_id)
        .execute(&pg.0)
        .await
        .map_err(pg_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        find_by_person_and_jurisdiction, find_or_grant, for_person, grant, is_licensed_in, revoke,
        CredentialError,
    };
    use crate::jurisdictions::{self, NewJurisdiction};
    use crate::persons::{self, NewPerson, Role};
    use crate::surreal::SurrealDb;
    use uuid::Uuid;

    async fn an_attorney(db: &SurrealDb, email: &str) -> Uuid {
        persons::create(db, &NewPerson::with_role("Scorpio", email, Role::Lawyer))
            .await
            .unwrap()
            .id
    }

    async fn a_state(db: &SurrealDb, name: &str, code: &str) -> Uuid {
        jurisdictions::create(db, &NewJurisdiction::new(name, code, "state"))
            .await
            .unwrap()
            .id
    }

    #[tokio::test]
    async fn source_context_refuses_credential_writes() {
        let db = SurrealDb::uninitialized();
        let id = Uuid::now_v7();
        crate::postgres_mirror::from_source(async {
            assert!(matches!(
                grant(&db, id, id, "synthetic").await,
                Err(CredentialError::Authority(_))
            ));
            assert!(matches!(
                revoke(&db, id).await,
                Err(CredentialError::Authority(_))
            ));
        })
        .await;
    }

    #[tokio::test]
    async fn a_granted_credential_reads_back_for_its_person() {
        crate::test_support::with_required_postgres(async {
            let db = SurrealDb::uninitialized();
            let person_id = an_attorney(&db, "scorpio@example.com").await;
            let nevada = a_state(&db, "Nevada", "NV").await;

            let granted = grant(&db, person_id, nevada, "NV-12345").await.unwrap();
            assert_eq!(granted.person_id, person_id);
            assert_eq!(granted.jurisdiction_id, nevada);
            assert_eq!(granted.license_number, "NV-12345");

            assert_eq!(for_person(&db, person_id).await.unwrap(), vec![granted]);
            assert!(is_licensed_in(&db, person_id, nevada).await.unwrap());
        })
        .await;
    }

    #[tokio::test]
    async fn the_natural_key_lookup_finds_only_that_pair() {
        crate::test_support::with_required_postgres(async {
            let db = SurrealDb::uninitialized();
            let person_id = an_attorney(&db, "scorpio@example.com").await;
            let nevada = a_state(&db, "Nevada", "NV").await;
            let utah = a_state(&db, "Utah", "UT").await;

            let granted = grant(&db, person_id, nevada, "NV-12345").await.unwrap();
            assert_eq!(
                find_by_person_and_jurisdiction(&db, person_id, nevada)
                    .await
                    .unwrap(),
                Some(granted)
            );
            assert_eq!(
                find_by_person_and_jurisdiction(&db, person_id, utah)
                    .await
                    .unwrap(),
                None,
                "a licence in Nevada is not a licence in Utah"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn find_or_grant_is_idempotent_on_the_person_jurisdiction_pair() {
        crate::test_support::with_required_postgres(async {
            let db = SurrealDb::uninitialized();
            let person_id = an_attorney(&db, "scorpio@example.com").await;
            let nevada = a_state(&db, "Nevada", "NV").await;

            // The canonical seed calls this on every boot.
            let first = find_or_grant(&db, person_id, nevada, "NV-12345")
                .await
                .unwrap();
            let second = find_or_grant(&db, person_id, nevada, "NV-99999")
                .await
                .unwrap();

            assert_eq!(first, second, "the second call returns the existing row");
            assert_eq!(
                second.license_number, "NV-12345",
                "an existing credential is returned as it stands, not overwritten"
            );
            assert_eq!(for_person(&db, person_id).await.unwrap().len(), 1);
        })
        .await;
    }

    #[tokio::test]
    async fn find_or_grant_still_refuses_a_dangling_person() {
        crate::test_support::with_required_postgres(async {
            let db = SurrealDb::uninitialized();
            let nobody = Uuid::now_v7();

            // Find-or-create does not weaken the read-backs: the lookup
            // misses, and the grant underneath it does the refusing.
            let refused = find_or_grant(&db, nobody, Uuid::now_v7(), "NV-1").await;
            assert!(matches!(
                refused,
                Err(CredentialError::NoSuchPerson(id)) if id == nobody
            ));
        })
        .await;
    }

    #[tokio::test]
    async fn a_credential_for_a_person_who_does_not_exist_is_refused() {
        crate::test_support::with_required_postgres(async {
            let db = SurrealDb::uninitialized();
            let nobody = Uuid::now_v7();

            // The accessor names the missing Person before the foreign key rejects it.
            let refused = grant(&db, nobody, Uuid::now_v7(), "NV-1").await;
            assert!(matches!(
                refused,
                Err(CredentialError::NoSuchPerson(id)) if id == nobody
            ));
        })
        .await;
    }

    #[tokio::test]
    async fn a_credential_for_a_jurisdiction_that_does_not_exist_is_refused() {
        crate::test_support::with_required_postgres(async {
            let db = SurrealDb::uninitialized();
            let person_id = an_attorney(&db, "scorpio@example.com").await;
            let nowhere = Uuid::now_v7();

            // The accessor names the missing jurisdiction before the foreign key rejects it.
            let refused = grant(&db, person_id, nowhere, "NV-1").await;
            assert!(matches!(
                refused,
                Err(CredentialError::NoSuchJurisdiction(id)) if id == nowhere
            ));
        })
        .await;
    }

    #[tokio::test]
    async fn a_second_credential_in_the_same_jurisdiction_is_reported_as_granted() {
        crate::test_support::with_required_postgres(async {
            let db = SurrealDb::uninitialized();
            let person_id = an_attorney(&db, "scorpio@example.com").await;
            let nevada = a_state(&db, "Nevada", "NV").await;

            grant(&db, person_id, nevada, "NV-12345").await.unwrap();
            let duplicate = grant(&db, person_id, nevada, "NV-99999").await;
            assert!(
                matches!(duplicate, Err(CredentialError::AlreadyGranted)),
                "the unique person/jurisdiction constraint is the gate, got {duplicate:?}"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn the_same_person_may_hold_several_jurisdictions() {
        crate::test_support::with_required_postgres(async {
            let db = SurrealDb::uninitialized();
            let person_id = an_attorney(&db, "scorpio@example.com").await;
            let nevada = a_state(&db, "Nevada", "NV").await;
            let california = a_state(&db, "California", "CA").await;

            grant(&db, person_id, nevada, "NV-1").await.unwrap();
            grant(&db, person_id, california, "CA-2").await.unwrap();

            assert_eq!(for_person(&db, person_id).await.unwrap().len(), 2);
            assert!(is_licensed_in(&db, person_id, nevada).await.unwrap());
            assert!(is_licensed_in(&db, person_id, california).await.unwrap());
        })
        .await;
    }

    #[tokio::test]
    async fn a_credential_is_scoped_to_its_own_person() {
        crate::test_support::with_required_postgres(async {
            let db = SurrealDb::uninitialized();
            let licensed = an_attorney(&db, "licensed@example.com").await;
            let unlicensed = an_attorney(&db, "unlicensed@example.com").await;
            let nevada = a_state(&db, "Nevada", "NV").await;

            grant(&db, licensed, nevada, "NV-1").await.unwrap();

            assert!(for_person(&db, unlicensed).await.unwrap().is_empty());
            assert!(!is_licensed_in(&db, unlicensed, nevada).await.unwrap());
        })
        .await;
    }

    #[tokio::test]
    async fn revoking_clears_the_licence_and_is_idempotent() {
        crate::test_support::with_required_postgres(async {
            let db = SurrealDb::uninitialized();
            let person_id = an_attorney(&db, "scorpio@example.com").await;
            let nevada = a_state(&db, "Nevada", "NV").await;
            let granted = grant(&db, person_id, nevada, "NV-1").await.unwrap();

            revoke(&db, granted.id).await.unwrap();
            assert!(!is_licensed_in(&db, person_id, nevada).await.unwrap());
            assert!(for_person(&db, person_id).await.unwrap().is_empty());

            // Revoking again, and revoking one that never existed, are no-ops.
            revoke(&db, granted.id).await.unwrap();
            revoke(&db, Uuid::now_v7()).await.unwrap();
        })
        .await;
    }

    #[tokio::test]
    async fn a_revoked_jurisdiction_can_be_granted_again() {
        crate::test_support::with_required_postgres(async {
            let db = SurrealDb::uninitialized();
            let person_id = an_attorney(&db, "scorpio@example.com").await;
            let nevada = a_state(&db, "Nevada", "NV").await;

            let first = grant(&db, person_id, nevada, "NV-1").await.unwrap();
            revoke(&db, first.id).await.unwrap();

            // The unique index must not keep the pair reserved after the row
            // is gone — a lapsed licence gets reinstated.
            let again = grant(&db, person_id, nevada, "NV-2").await.unwrap();
            assert_eq!(again.license_number, "NV-2");
        })
        .await;
    }
}

#[cfg(test)]
#[path = "credentials/compatibility_tests.rs"]
mod compatibility_tests;

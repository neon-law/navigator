//! The jurisdiction reference table — US states (plus DC) and foreign
//! sovereigns an Entity can be organized under — and every query that
//! reads or writes a `jurisdiction` row.
//!
//! # Where this table lives
//!
//! PostgreSQL holds it wherever it is configured: every read and write
//! here goes to PostgreSQL
//! ([`crate::postgres::catalog`]). There the unique `name` stops a racing
//! duplicate ([`JurisdictionError::NameTaken`]) and the
//! `jurisdiction_type_closed` check holds the closed type set.
//!
//! `jurisdictions` moved with its slice of #1093 (ENG-20). It is a leaf
//! lookup table — no outbound references — so the port could not cascade
//! further, and it closes the one cross-engine reference the persons
//! slice knowingly left behind: `credential.jurisdiction_id` is a real
//! `record<jurisdiction>` link now instead of a bare UUID resolved in
//! Rust.
//!
//! # Engine facts this module is shaped around
//!
//! **A unique violation carries no typed detail.** It arrives as
//! [`surrealdb::types::ErrorDetails::Internal`] with the index name in
//! the message, so the shared classifier discriminates on
//! `jurisdiction_code` — a `DEFINE INDEX` identifier this workspace
//! chose in `store/src/schema/navigator.surql`, not prose.
//!
//! **The key-value layer is optimistic, so a write can lose a race.**
//! Two writers touching one record conflict, the loser is rolled back,
//! and the engine reports a typed `TransactionConflict`; [`writing`]
//! re-runs the statement under the crate's one retry policy,
//! [`crate::surreal::retry`].
//!
//! **`code` needs no stored lowercase twin.** `person.email_lower`
//! exists because an IdP may present any casing of a mailbox; a
//! jurisdiction code is a canonical spelling from
//! `store/seeds/Jurisdiction.yaml` that every lookup matches exactly, so
//! the plain UNIQUE index is the whole constraint.
//!
//! # A link is not validated
//!
//! `entity.jurisdiction_id` is a `record<jurisdiction>` link, and the
//! engine accepts one naming a row that was never written. A caller that
//! needs the jurisdiction behind a link resolves it here, which is what
//! [`find_by_id`] exists for.

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::catalog;

/// A US state or DC — the `jurisdiction_type` IOLTA pools by, so
/// `store::iolta_accounts` names it rather than spelling the token again.
pub const JURISDICTION_TYPE_STATE: &str = "state";
/// A federal sovereign. Named beside its siblings so the closed set the
/// schema ASSERTs reads as one list in Rust too.
pub const JURISDICTION_TYPE_COUNTRY: &str = "country";
/// No single body of law: the one `Global` row a template that works
/// anywhere declares as `jurisdiction: GLOBAL`.
pub const JURISDICTION_TYPE_GLOBAL: &str = "global";

/// One jurisdiction.
///
/// The application-facing shape: plain Rust types, no engine handles.
/// [`JurisdictionRow`] is the seam that turns it into (and back out of)
/// what the SDK reads and writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Jurisdiction {
    pub id: Uuid,
    /// Display name (`Nevada`, `United States`).
    pub name: String,
    /// Short canonical code (`NV`, `CA`, `US`). Unique.
    pub code: String,
    /// `state` (US state or DC) or `country` (federal sovereign). The
    /// schema ASSERTs the closed set, so a row cannot carry anything
    /// else — the intake `country` picker filters on this field, and a
    /// typo'd value would otherwise silently vanish from it.
    pub jurisdiction_type: String,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Errors reading or writing a jurisdiction.
#[derive(Debug, thiserror::Error)]
pub enum JurisdictionError {
    /// A database operation failed.
    /// The write collided with `jurisdiction_code` — another row already
    /// holds this code.
    #[error("that jurisdiction code is already in use")]
    CodeTaken,
    /// PostgreSQL's unique `name` refused the write — another row already
    /// holds this name.
    #[error("that jurisdiction name is already in use")]
    NameTaken,
    /// A write reported success but returned no row, or returned one
    /// this module could not read back — see
    /// [`JurisdictionRow::into_jurisdiction`].
    #[error("writing a jurisdiction returned no usable row")]
    WriteReturnedNothing,
    /// The PostgreSQL catalog authority could not serve the call.
    #[error(transparent)]
    Postgres(#[from] catalog::CatalogError),
}

impl From<sqlx::Error> for JurisdictionError {
    fn from(error: sqlx::Error) -> Self {
        Self::Postgres(error.into())
    }
}

const PG_BY_ID: &str = "SELECT id, name, code, jurisdiction_type, inserted_at, updated_at \
     FROM jurisdiction WHERE id = $1";
const PG_BY_CODE: &str = "SELECT id, name, code, jurisdiction_type, inserted_at, updated_at \
     FROM jurisdiction WHERE code = $1";
const PG_BY_NAME: &str = "SELECT id, name, code, jurisdiction_type, inserted_at, updated_at \
     FROM jurisdiction WHERE name = $1";
const PG_ALL: &str = "SELECT id, name, code, jurisdiction_type, inserted_at, updated_at \
     FROM jurisdiction ORDER BY name ASC";
const PG_BY_TYPE: &str = "SELECT id, name, code, jurisdiction_type, inserted_at, updated_at \
     FROM jurisdiction WHERE jurisdiction_type = $1 ORDER BY name ASC";
const PG_INSERT: &str = "INSERT INTO jurisdiction (id, name, code, jurisdiction_type) \
     VALUES ($1, $2, $3, $4) RETURNING id, name, code, jurisdiction_type, inserted_at, updated_at";

fn from_pg(row: &sqlx::postgres::PgRow) -> Result<Jurisdiction, sqlx::Error> {
    use sqlx::Row;
    Ok(Jurisdiction {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        code: row.try_get("code")?,
        jurisdiction_type: row.try_get("jurisdiction_type")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// The fields a new jurisdiction row carries.
#[derive(Debug, Clone)]
pub struct NewJurisdiction {
    pub name: String,
    pub code: String,
    /// `state` or `country`; anything else is refused by the schema
    /// ASSERT.
    pub jurisdiction_type: String,
}

impl NewJurisdiction {
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        code: impl Into<String>,
        jurisdiction_type: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            code: code.into(),
            jurisdiction_type: jurisdiction_type.into(),
        }
    }
}

/// Resolve a jurisdiction by id.
///
/// # Errors
///
/// [`JurisdictionError::Db`] if the lookup fails.
pub async fn find_by_id(
    _db: &ApplicationStore,
    id: Uuid,
) -> Result<Option<Jurisdiction>, JurisdictionError> {
    let pg = catalog::authority().await?;
    Ok(catalog::one(&pg, PG_BY_ID, id, from_pg).await?)
}

/// Resolve a jurisdiction by its canonical `code` (`NV`, `US`). Exact
/// match — codes are canonical spellings, never user-typed.
///
/// # Errors
///
/// [`JurisdictionError::Db`] if the lookup fails.
pub async fn find_by_code(
    _db: &ApplicationStore,
    code: &str,
) -> Result<Option<Jurisdiction>, JurisdictionError> {
    let pg = catalog::authority().await?;
    Ok(catalog::one(&pg, PG_BY_CODE, code.trim().to_string(), from_pg).await?)
}

/// Resolve a jurisdiction by display name (`Nevada`). Exact match — the
/// names are the seed's canonical spellings, and the seed and importer
/// resolve rows by the name their source data carries.
///
/// # Errors
///
/// [`JurisdictionError::Db`] if the lookup fails.
pub async fn find_by_name(
    _db: &ApplicationStore,
    name: &str,
) -> Result<Option<Jurisdiction>, JurisdictionError> {
    let pg = catalog::authority().await?;
    Ok(catalog::one(&pg, PG_BY_NAME, name.trim().to_string(), from_pg).await?)
}

/// Every jurisdiction, ordered by name — what a reference picker with no
/// type filter offers.
///
/// # Errors
///
/// [`JurisdictionError::Db`] if the lookup fails.
pub async fn list_all(_db: &ApplicationStore) -> Result<Vec<Jurisdiction>, JurisdictionError> {
    let pg = catalog::authority().await?;
    Ok(catalog::all(&pg, PG_ALL, None::<String>, from_pg).await?)
}

/// Every jurisdiction of one `jurisdiction_type`, ordered by name — the
/// `country` picker (an N-400's country of birth) filters here so it
/// never offers a US state.
///
/// # Errors
///
/// [`JurisdictionError::Db`] if the lookup fails.
pub async fn list_by_type(
    _db: &ApplicationStore,
    jurisdiction_type: &str,
) -> Result<Vec<Jurisdiction>, JurisdictionError> {
    let pg = catalog::authority().await?;
    let kind = Some(jurisdiction_type.to_string());
    Ok(catalog::all(&pg, PG_BY_TYPE, kind, from_pg).await?)
}

/// Write a new jurisdiction row. The record id is minted from a fresh v7
/// `Uuid` through [`crate::surreal::record_id`], so the key stays the
/// native UUID spelling every cross-engine `jurisdiction_id` still
/// addresses.
///
/// # Errors
///
/// [`JurisdictionError::CodeTaken`] when another row already holds this
/// code, and [`JurisdictionError::Db`] for anything else — including the
/// schema ASSERT refusing a `jurisdiction_type` outside
/// `state`/`country`.
pub async fn create(
    _db: &ApplicationStore,
    input: &NewJurisdiction,
) -> Result<Jurisdiction, JurisdictionError> {
    let id = Uuid::now_v7();
    let pg = catalog::authority().await?;
    let row = catalog::row(
        sqlx::query(PG_INSERT)
            .bind(id)
            .bind(input.name.trim().to_string())
            .bind(input.code.trim().to_string())
            .bind(input.jurisdiction_type.clone()),
        &pg,
    )
    .await
    .map_err(catalog::CatalogError::from);
    match row {
        Ok(row) => Ok(from_pg(&row)?),
        Err(error) if error.violates("jurisdiction_name_key") => Err(JurisdictionError::NameTaken),
        Err(error) => Err(error.into()),
    }
}

/// Find the jurisdiction holding `input.code`, creating it if absent.
/// Race-safe without a lock: a concurrent creator that wins the
/// unique index (`code` in SurrealDB, `name` in PostgreSQL) turns this
/// call's insert into [`JurisdictionError::CodeTaken`] or
/// [`JurisdictionError::NameTaken`], which is re-read as the winner's row. The seed runs this on every boot, so idempotence is the
/// contract.
///
/// # Errors
///
/// [`JurisdictionError::Db`] if a lookup or the insert fails.
pub async fn find_or_create(
    db: &ApplicationStore,
    input: &NewJurisdiction,
) -> Result<Jurisdiction, JurisdictionError> {
    if let Some(existing) = find_by_code(db, &input.code).await? {
        return Ok(existing);
    }
    match create(db, input).await {
        Ok(created) => Ok(created),
        Err(error @ (JurisdictionError::CodeTaken | JurisdictionError::NameTaken)) => {
            find_by_code(db, &input.code).await?.ok_or(error)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {}

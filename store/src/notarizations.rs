//! `notarizations` — the notary counterpart to [`crate::signatures`], and
//! every query against the table.
//!
//! # This table lives in SurrealDB
//!
//! `notarizations` moved with wave five of #1093 (ENG-121), in the
//! satellite-ring slice.
//!
//! A Notation's document sent for remote online notarization records a row
//! here, correlated back from the provider by `(provider, provider_id)` —
//! unique, the callback's correlation key. `notary_person_id`/`asset_id`
//! name who notarized what (null until known); `notarized_at` is stamped on
//! completion.

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
pub use crate::signatures::SignatureProvider;

use crate::postgres::signatures as authority;

/// One notarization request/execution.
///
/// The application-facing shape: plain Rust types, no engine handles.
/// [`NotarizationRow`] is the seam that turns it into (and back out of)
/// what the SDK reads and writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Notarization {
    pub id: Uuid,
    pub notation_id: Uuid,
    pub notary_person_id: Option<Uuid>,
    pub asset_id: Option<Uuid>,
    /// The provider's stored string form — see [`SignatureProvider::as_str`].
    pub provider: String,
    pub provider_id: String,
    pub notarized_at: Option<String>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Errors reading or writing a notarization.
#[derive(Debug, thiserror::Error)]
pub enum NotarizationError {
    /// PostgreSQL, under the signature authority, failed.
    #[error(transparent)]
    Postgres(#[from] authority::SignatureFamilyError),
    /// A provider timestamp the SurrealDB path would store verbatim does
    /// not parse as an RFC 3339 instant, which is all the PostgreSQL
    /// `timestamptz` column accepts.
    #[error("`{0}` is not an RFC 3339 timestamp")]
    InvalidTimestamp(String),
    /// A write reported success but returned no row, or returned one this
    /// module could not read back.
    #[error("writing a notarization returned no usable row")]
    WriteReturnedNothing,
}

/// The row as PostgreSQL's `to_jsonb` serves it, under the
/// `postgres::signatures` authority. `notarized_at` comes back as a real
/// instant and is re-rendered in the RFC 3339 form the SurrealDB path
/// stores verbatim.
#[derive(serde::Deserialize)]
struct PgNotarizationRow {
    id: Uuid,
    notation_id: Uuid,
    notary_person_id: Option<Uuid>,
    asset_id: Option<Uuid>,
    provider: String,
    provider_id: String,
    notarized_at: Option<DateTime<Utc>>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<PgNotarizationRow> for Notarization {
    fn from(row: PgNotarizationRow) -> Self {
        Notarization {
            id: row.id,
            notation_id: row.notation_id,
            notary_person_id: row.notary_person_id,
            asset_id: row.asset_id,
            provider: row.provider,
            provider_id: row.provider_id,
            notarized_at: row
                .notarized_at
                .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)),
            inserted_at: row.inserted_at,
            updated_at: row.updated_at,
        }
    }
}

/// The RFC 3339 instant the `timestamptz` column accepts for a provider
/// timestamp string.
fn instant(raw: &str) -> Result<DateTime<Utc>, NotarizationError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|_| NotarizationError::InvalidTimestamp(raw.to_string()))
}

/// Record the provider's request id for a Notation's notarization.
/// Idempotent on `(provider, provider_id)`: a concurrent double-send loses
/// the `notarization_provider_request` unique index and re-reads the
/// winner's row.
///
/// # Errors
///
/// [`NotarizationError::Db`] if the write fails.
pub async fn record_request(
    db: &ApplicationStore,
    notation_id: Uuid,
    provider: SignatureProvider,
    provider_id: &str,
) -> Result<Notarization, NotarizationError> {
    let pg = authority::authority().await?;
    let row = serde_json::json!({
        "id": Uuid::now_v7(), "notation_id": notation_id,
        "provider": provider.as_str(), "provider_id": provider_id,
    });
    let query = sqlx::query(
        "INSERT INTO notarization SELECT * FROM \
         jsonb_populate_record(NULL::notarization, \
         $1 || jsonb_build_object('inserted_at', now(), 'updated_at', now())) \
         RETURNING to_jsonb(notarization) AS row",
    )
    .bind(row);
    match authority::maybe::<PgNotarizationRow>(&pg, query).await {
        Ok(Some(row)) => Ok(Notarization::from(row)),
        Ok(None) => Err(NotarizationError::WriteReturnedNothing),
        Err(error) if error.violates("notarization_provider_request") => {
            by_provider(db, provider, provider_id)
                .await?
                .filter(|request| request.notation_id == notation_id)
                .ok_or(NotarizationError::WriteReturnedNothing)
        }
        Err(error) => Err(error.into()),
    }
}

/// The notarization row for `(provider, provider_id)`, if any.
///
/// # Errors
///
/// [`NotarizationError::Db`] if the lookup fails.
pub async fn by_provider(
    _db: &ApplicationStore,
    provider: SignatureProvider,
    provider_id: &str,
) -> Result<Option<Notarization>, NotarizationError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT to_jsonb(notarization) AS row FROM notarization \
         WHERE provider = $1 AND provider_id = $2 LIMIT 1",
    )
    .bind(provider.as_str())
    .bind(provider_id);
    Ok(authority::maybe::<PgNotarizationRow>(&pg, query)
        .await?
        .map(Notarization::from))
}

/// Every notarization request on a Notation, oldest first.
///
/// # Errors
///
/// [`NotarizationError::Db`] if the lookup fails.
pub async fn for_notation(
    _db: &ApplicationStore,
    notation_id: Uuid,
) -> Result<Vec<Notarization>, NotarizationError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT to_jsonb(notarization) AS row FROM notarization \
         WHERE notation_id = $1 ORDER BY inserted_at, id",
    )
    .bind(notation_id);
    Ok(authority::rows::<PgNotarizationRow>(&pg, query)
        .await?
        .into_iter()
        .map(Notarization::from)
        .collect())
}

/// Stamp `notarized_at` when the provider reports completion. Returns
/// `false` for an unknown request.
///
/// # Errors
///
/// [`NotarizationError::Db`] if the lookup or write fails.
pub async fn stamp_notarized(
    _db: &ApplicationStore,
    provider: SignatureProvider,
    provider_id: &str,
    notarized_at: &str,
) -> Result<bool, NotarizationError> {
    let pg = authority::authority().await?;
    let notarized_at = instant(notarized_at)?;
    let query = sqlx::query(
        "UPDATE notarization SET notarized_at = $1, updated_at = now() \
         WHERE provider = $2 AND provider_id = $3",
    )
    .bind(notarized_at)
    .bind(provider.as_str())
    .bind(provider_id);
    Ok(authority::affected(&pg, query).await? > 0)
}

/// Point a notarization request at the notarized asset, once known.
///
/// # Errors
///
/// [`NotarizationError::Db`] if the write fails.
pub async fn set_asset(
    _db: &ApplicationStore,
    id: Uuid,
    asset_id: Uuid,
) -> Result<(), NotarizationError> {
    let pg = authority::authority().await?;
    sqlx::query("UPDATE notarization SET asset_id = $1, updated_at = now() WHERE id = $2")
        .bind(asset_id)
        .bind(id)
        .execute(pg.pool())
        .await
        .map_err(authority::SignatureFamilyError::from)?;
    Ok(())
}

#[cfg(test)]
mod tests {}

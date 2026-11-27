//! PostgreSQL authority for the provider-envelope family:
//! `signature`, `verification`, and `notarization`.
//!
//! Whenever `NAVIGATOR_POSTGRES_HOST` is set ([`PostgresDb::configured`]),
//! the owning modules (`signatures`, `verifications`, `notarizations`) read
//! and write PostgreSQL only, a PostgreSQL failure is returned rather than
//! falling back, and the mirror stops copying SurrealDB rows over the
//! authoritative ones. Without a PostgreSQL host the SurrealDB path is the
//! fallback. Parent rows the tables name — the Notation, the Person, the
//! Citation, and the Asset — stay on their own authorities.
//!
//! Rows come back as the JSON object `to_jsonb` makes of them, so every
//! statement selects `to_jsonb(<alias>) AS row` and the owning module's own
//! serde type decodes it.

use std::future::Future;
use std::pin::Pin;

use serde::de::DeserializeOwned;
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use crate::postgres::PostgresDb;

#[cfg(test)]
tokio::task_local! {
    static TEST_SIGNATURE_FAMILY_DB: PostgresDb;
}

/// Why PostgreSQL could not serve a provider-envelope call. Carries no row
/// values.
#[derive(Debug, thiserror::Error)]
pub enum SignatureFamilyError {
    #[error("PostgreSQL is unavailable for the signature tables")]
    Unavailable,
    #[error("PostgreSQL refused the signature operation: {0}")]
    Query(#[from] sqlx::Error),
    #[error("PostgreSQL returned an unreadable signature row")]
    Row(#[from] serde_json::Error),
}

impl SignatureFamilyError {
    /// Whether this is a unique violation of the named index.
    pub(crate) fn violates(&self, index: &str) -> bool {
        matches!(self, Self::Query(sqlx::Error::Database(database))
            if database.is_unique_violation() && database.constraint() == Some(index))
    }
}

/// A provider-envelope operation. Boxed so every owning module that awaits
/// one keeps a small future instead of embedding the PostgreSQL client's.
pub(crate) type Op<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, SignatureFamilyError>> + Send + 'a>>;

type Query<'q> = sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>;

/// The pool to serve from, or `None` when no PostgreSQL host is configured.
pub(crate) fn authority() -> Op<'static, PostgresDb> {
    Box::pin(async {
        #[cfg(test)]
        if let Ok(pg) = TEST_SIGNATURE_FAMILY_DB.try_with(Clone::clone) {
            return Ok(pg);
        }
        PostgresDb::application()
            .await
            .map_err(|_| SignatureFamilyError::Unavailable)
    })
}

fn decode<T: DeserializeOwned>(row: &sqlx::postgres::PgRow) -> Result<T, SignatureFamilyError> {
    let value: Value = row.try_get("row")?;
    Ok(serde_json::from_value(value)?)
}

/// Every row `query` selects as `row`.
pub(crate) fn rows<'a, T: DeserializeOwned + Send + 'a>(
    pg: &'a PostgresDb,
    query: Query<'a>,
) -> Op<'a, Vec<T>> {
    Box::pin(async move {
        query
            .fetch_all(pg.pool())
            .await?
            .iter()
            .map(decode)
            .collect()
    })
}

/// The row `query` selects as `row`, if any.
pub(crate) fn maybe<'a, T: DeserializeOwned + Send + 'a>(
    pg: &'a PostgresDb,
    query: Query<'a>,
) -> Op<'a, Option<T>> {
    Box::pin(async move {
        query
            .fetch_optional(pg.pool())
            .await?
            .as_ref()
            .map(decode)
            .transpose()
    })
}

/// How many rows `query` updated.
pub(crate) fn affected<'a>(pg: &'a PostgresDb, query: Query<'a>) -> Op<'a, u64> {
    Box::pin(async move { Ok(query.execute(pg.pool()).await?.rows_affected()) })
}

/// The first column of the row `query` returns, if any.
pub(crate) fn maybe_scalar<'a, T>(pg: &'a PostgresDb, query: Query<'a>) -> Op<'a, Option<T>>
where
    T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + 'a,
{
    Box::pin(async move {
        let row = query.fetch_optional(pg.pool()).await?;
        match row {
            Some(row) => Ok(Some(row.try_get(0)?)),
            None => Ok(None),
        }
    })
}

/// Run `future` with `pg` serving the provider-envelope tables.
#[cfg(test)]
pub(crate) async fn with_test_db<T>(pg: PostgresDb, future: impl Future<Output = T>) -> T {
    TEST_SIGNATURE_FAMILY_DB.scope(pg, future).await
}

/// The two columns a [`sweep_stale`] pass needs from each moved row.
#[derive(serde::Deserialize)]
struct SweptVerification {
    id: Uuid,
    revision_sha: String,
}

/// Sweep verifications of superseded revisions: every `Verified` or
/// `Rejected` axis of a citation's non-current revisions moves to `Stale`
/// (see `rules::citation::AxisStatus::goes_stale_on_edit`). Returns one
/// entry per moved axis — the axis, the verification's id, and the
/// superseded revision it was made against.
pub(crate) async fn sweep_stale(
    pg: &PostgresDb,
    citation_id: Uuid,
    current_revision_sha: &str,
) -> Result<Vec<(rules::citation::Axis, Uuid, String)>, SignatureFamilyError> {
    let stale = rules::citation::AxisStatus::Stale.as_str();
    let stale_on_edit = [
        rules::citation::AxisStatus::Verified.as_str(),
        rules::citation::AxisStatus::Rejected.as_str(),
    ];
    let mut transitions = Vec::new();
    for axis in rules::citation::Axis::ALL {
        // The field name comes from the closed `Axis` set, never from
        // caller input, so the interpolation is a vocabulary lookup.
        let field = match axis {
            rules::citation::Axis::Citation => "status_citation",
            rules::citation::Axis::Quote => "status_quote",
            rules::citation::Axis::Proposition => "status_proposition",
        };
        let query = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE verification SET {field} = '{stale}', updated_at = now() \
             WHERE citation_id = $1 AND revision_sha != $2 \
             AND {field} = ANY($3) \
             RETURNING to_jsonb(verification) AS row"
        )))
        .bind(citation_id)
        .bind(current_revision_sha)
        .bind(stale_on_edit);
        transitions.extend(
            rows::<SweptVerification>(pg, query)
                .await?
                .into_iter()
                .map(|row| (*axis, row.id, row.revision_sha)),
        );
    }
    Ok(transitions)
}

#[cfg(test)]
#[path = "signature_tests.rs"]
mod tests;

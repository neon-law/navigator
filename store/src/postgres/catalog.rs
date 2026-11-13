//! The PostgreSQL authority switch for the shared catalogs: Authorities,
//! Entity Types, Glossary Terms, Jurisdictions, Notation packages and their
//! templates, Notation rates, and Questions.
//!
//! These rows belong to no Firm or Project. `NAVIGATOR_POSTGRES_CATALOG_AUTHORITY`
//! is opt-in; once on, each owning module reads and writes PostgreSQL only,
//! a configured PostgreSQL failure is returned rather than falling back, and
//! the catalog mirror stops copying SurrealDB rows over the authoritative ones.

use std::future::Future;
use std::pin::Pin;

use sqlx::postgres::PgRow;

use crate::postgres::{PostgresDb, PostgresError};

/// When `1` or `true`, the catalog modules use PostgreSQL.
pub const AUTHORITY_ENV: &str = "NAVIGATOR_POSTGRES_CATALOG_AUTHORITY";

#[cfg(test)]
tokio::task_local! {
    static TEST_CATALOG_DB: PostgresDb;
}

/// Why the PostgreSQL catalog could not serve a call.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("NAVIGATOR_POSTGRES_CATALOG_AUTHORITY must be one of 0, 1, false, or true")]
    Configuration,
    #[error("PostgreSQL is unavailable for the catalog authority")]
    Unavailable(#[from] PostgresError),
    #[error("PostgreSQL catalog operation failed: {0}")]
    Query(#[from] sqlx::Error),
}

impl CatalogError {
    /// Whether this is a unique violation of the named constraint.
    pub(crate) fn violates(&self, constraint: &str) -> bool {
        matches!(self, Self::Query(sqlx::Error::Database(database))
            if database.is_unique_violation() && database.constraint() == Some(constraint))
    }
}

fn enabled_from(setting: Option<&str>) -> Result<bool, CatalogError> {
    match setting {
        None | Some("0" | "false") => Ok(false),
        Some("1" | "true") => Ok(true),
        Some(_) => Err(CatalogError::Configuration),
    }
}

fn setting() -> Result<Option<String>, CatalogError> {
    match std::env::var(AUTHORITY_ENV) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(CatalogError::Configuration),
    }
}

/// A boxed catalog future. Every catalog call sits inside long request
/// futures, so its PostgreSQL state lives on the heap rather than inflating
/// each caller's stack frame.
pub(crate) type Boxed<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

type Query<'q> = sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>;

/// The pool to serve the catalog from, or `None` when SurrealDB still holds it.
pub(crate) fn authority() -> Boxed<'static, Result<Option<PostgresDb>, CatalogError>> {
    Box::pin(async {
        #[cfg(test)]
        if let Ok(pg) = TEST_CATALOG_DB.try_with(Clone::clone) {
            return Ok(Some(pg));
        }
        if !enabled_from(setting()?.as_deref())? {
            return Ok(None);
        }
        Ok(Some(PostgresDb::from_env().await?))
    })
}

/// Whether the catalog mirror must leave PostgreSQL alone. An unreadable
/// setting protects PostgreSQL too, until [`authority`] fails the caller.
pub(crate) fn configured() -> bool {
    #[cfg(test)]
    if TEST_CATALOG_DB.try_with(|_| ()).is_ok() {
        return true;
    }
    setting()
        .and_then(|value| enabled_from(value.as_deref()))
        .unwrap_or(true)
}

/// A catalog row decoded from PostgreSQL.
pub(crate) type Decode<T> = fn(&PgRow) -> Result<T, sqlx::Error>;

/// The one row `query` returns.
pub(crate) fn row<'a>(
    query: Query<'a>,
    pg: &'a PostgresDb,
) -> Boxed<'a, Result<PgRow, sqlx::Error>> {
    Box::pin(query.fetch_one(pg.pool()))
}

/// The row `query` returns, if any.
pub(crate) fn maybe_row<'a>(
    query: Query<'a>,
    pg: &'a PostgresDb,
) -> Boxed<'a, Result<Option<PgRow>, sqlx::Error>> {
    Box::pin(query.fetch_optional(pg.pool()))
}

/// Run a write that returns no row.
pub(crate) fn execute<'a>(
    query: Query<'a>,
    pg: &'a PostgresDb,
) -> Boxed<'a, Result<(), sqlx::Error>> {
    Box::pin(async move { query.execute(pg.pool()).await.map(|_| ()) })
}

/// The one row `sql` selects by its single parameter `key`, if any.
pub(crate) fn one<'a, T, K>(
    pg: &'a PostgresDb,
    sql: &'static str,
    key: K,
    decode: Decode<T>,
) -> Boxed<'a, Result<Option<T>, sqlx::Error>>
where
    T: Send + 'a,
    K: for<'q> sqlx::Encode<'q, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + 'static,
{
    Box::pin(async move {
        let row = sqlx::query(sql).bind(key).fetch_optional(pg.pool()).await?;
        row.as_ref().map(decode).transpose()
    })
}

/// Every row `sql` selects by its single parameter `key`, or by none when
/// `key` is `None`.
pub(crate) fn all<'a, T, K>(
    pg: &'a PostgresDb,
    sql: &'static str,
    key: Option<K>,
    decode: Decode<T>,
) -> Boxed<'a, Result<Vec<T>, sqlx::Error>>
where
    T: Send + 'a,
    K: for<'q> sqlx::Encode<'q, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + 'static,
{
    Box::pin(async move {
        let query = sqlx::query(sql);
        let query = match key {
            Some(key) => query.bind(key),
            None => query,
        };
        query
            .fetch_all(pg.pool())
            .await?
            .iter()
            .map(decode)
            .collect()
    })
}

/// Run `future` with the catalog authority on, served by `pg`.
#[cfg(test)]
pub(crate) async fn with_test_db<T>(
    pg: PostgresDb,
    future: impl std::future::Future<Output = T>,
) -> T {
    TEST_CATALOG_DB.scope(pg, future).await
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;

//! PostgreSQL authority for what a Person or an Entity holds beside its own
//! row: Address, Credential, Email Token, Entity, Person Delegate, Person
//! External Identity, and Person Phone rows.
//!
//! These accessors share PostgreSQL identity authority and never project writes
//! into a compatibility database.

use crate::postgres::PostgresDb;

/// Why a PostgreSQL identity operation failed.
#[derive(Debug, thiserror::Error)]
pub enum AuthorityError {
    /// The authority setting is invalid or PostgreSQL is unavailable.
    #[error("PostgreSQL identity authority: {0}")]
    Unavailable(String),
    /// A PostgreSQL query failed.
    #[error("PostgreSQL identity query failed: {0}")]
    Query(#[from] sqlx::Error),
}

/// The admitted PostgreSQL pool, or `None` only inside explicit Source import reads.
///
/// # Errors
///
/// [`AuthorityError::Unavailable`] when PostgreSQL configuration or historical
/// enrollment is unavailable.
pub(crate) async fn authority() -> Result<PostgresDb, AuthorityError> {
    crate::persons::postgres_authority()
        .await
        .map_err(|error| AuthorityError::Unavailable(error.to_string()))
}

/// Whether `error` is PostgreSQL refusing a write on the named unique
/// constraint or index.
pub(crate) fn violates(error: &sqlx::Error, constraint: &str) -> bool {
    matches!(error, sqlx::Error::Database(database) if database.constraint() == Some(constraint))
}

#[cfg(test)]
#[path = "identity_tests.rs"]
mod tests;

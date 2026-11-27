//! `git_repositories` — provenance for repositories that carried
//! imported notation content, and every query against a
//! `git_repository` row.
//!
//! # This table lives in SurrealDB
//!
//! `git_repositories` moved with its slice of #1093 (ENG-20). It is a
//! leaf provenance table — nothing references it and it references
//! nothing — so the port could not cascade. Rows come from the
//! canonical seed; `navigator site seed` seeds from a local
//! directory and never writes here.
//!
//! The unique `git_repository_remote_hash` index is what enforces one row
//! per remote; a violation has no typed detail, so
//! [`crate::surreal::retry::unique_violation`] discriminates on that index
//! name.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::matter::{self, MatterAuthorityError};

/// The table these rows live in.
const TABLE: &str = "git_repository";

/// One tracked repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitRepository {
    pub id: Uuid,
    /// SHA-256 hash of `git remote get-url origin`. Unique.
    pub remote_hash: String,
    /// Last imported commit SHA.
    pub last_commit_sha: String,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Errors reading or writing a git repository row.
#[derive(Debug, thiserror::Error)]
pub enum GitRepositoryError {
    /// A database operation failed.
    /// A PostgreSQL matter-authority operation failed.
    #[error(transparent)]
    Postgres(MatterAuthorityError),
    /// The write collided with `git_repository_remote_hash` — another
    /// row already holds this remote hash.
    #[error("that repository remote is already tracked")]
    RemoteTaken,
    /// A write reported success but returned no row, or returned one
    /// this module could not read back — see
    /// [`GitRepositoryRow::into_repository`].
    #[error("writing a git repository returned no usable row")]
    WriteReturnedNothing,
}

impl From<MatterAuthorityError> for GitRepositoryError {
    fn from(error: MatterAuthorityError) -> Self {
        match error {
            MatterAuthorityError::Conflict(_) => Self::RemoteTaken,
            error => Self::Postgres(error),
        }
    }
}

/// Resolve a repository by its `remote_hash`. Exact match — the hash is
/// computed, never user-typed.
///
/// # Errors
///
/// [`GitRepositoryError::Db`] if the lookup fails.
pub async fn find_by_remote_hash(
    _db: &ApplicationStore,
    remote_hash: &str,
) -> Result<Option<GitRepository>, GitRepositoryError> {
    let pg = matter::authority().await?;
    let filter = json!({"remote_hash": remote_hash});
    Ok(matter::find(&pg, TABLE, filter).await?)
}

/// Every tracked repository, ordered by remote hash for a deterministic
/// listing.
///
/// # Errors
///
/// [`GitRepositoryError::Db`] if the lookup fails.
pub async fn list_all(_db: &ApplicationStore) -> Result<Vec<GitRepository>, GitRepositoryError> {
    let pg = matter::authority().await?;
    Ok(matter::select(&pg, TABLE, json!({}), &["remote_hash"]).await?)
}

/// Write a new repository row under a fresh v7 UUID record id.
///
/// # Errors
///
/// [`GitRepositoryError::RemoteTaken`] when another row already holds
/// this remote hash, and [`GitRepositoryError::Db`] for anything else.
pub async fn create(
    _db: &ApplicationStore,
    remote_hash: &str,
    last_commit_sha: &str,
) -> Result<GitRepository, GitRepositoryError> {
    let id = Uuid::now_v7();
    let pg = matter::authority().await?;
    let now = Utc::now();
    let row = GitRepository {
        id,
        remote_hash: remote_hash.to_owned(),
        last_commit_sha: last_commit_sha.to_owned(),
        inserted_at: now,
        updated_at: now,
    };
    Ok(matter::upsert(&pg, TABLE, &row).await?)
}

/// Find the repository holding `remote_hash`, creating it if absent.
/// Race-safe without a lock: a concurrent creator that wins the
/// `git_repository_remote_hash` unique index turns this call's insert
/// into [`GitRepositoryError::RemoteTaken`], which is re-read as the
/// winner's row. The seed runs this on every boot, so idempotence is
/// the contract.
///
/// # Errors
///
/// [`GitRepositoryError::Db`] if a lookup or the insert fails.
pub async fn find_or_create(
    db: &ApplicationStore,
    remote_hash: &str,
    last_commit_sha: &str,
) -> Result<GitRepository, GitRepositoryError> {
    if let Some(existing) = find_by_remote_hash(db, remote_hash).await? {
        return Ok(existing);
    }
    match create(db, remote_hash, last_commit_sha).await {
        Ok(created) => Ok(created),
        Err(GitRepositoryError::RemoteTaken) => find_by_remote_hash(db, remote_hash)
            .await?
            .ok_or(GitRepositoryError::WriteReturnedNothing),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {}

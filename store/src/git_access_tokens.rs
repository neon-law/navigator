//! Project-scoped Personal Access Tokens for the Git transport.
//!
//! The plaintext never reaches storage: callers provide it once, this module
//! stores only its SHA-256 hash, and a missing or expired row is unauthenticated.

use chrono::{DateTime, Utc};
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::postgres::matter;
use crate::projects;
use crate::surreal::{record_id, record_uuid, SurrealDb};
use serde_json::json;

/// Clone / fetch — read access to a repository.
pub const SCOPE_READ: &str = "read";
/// Push — write access; a strict superset of [`SCOPE_READ`].
pub const SCOPE_WRITE: &str = "write";

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GitAccessToken {
    pub id: Uuid,
    pub person_id: Uuid,
    pub project_id: Option<Uuid>,
    pub token_hash: String,
    pub scope: String,
    pub expires_at: String,
    pub inserted_at: String,
    pub updated_at: String,
}

#[derive(SurrealValue)]
struct TokenRow {
    id: surrealdb::types::RecordId,
    person_id: surrealdb::types::RecordId,
    project_id: Option<surrealdb::types::RecordId>,
    token_hash: String,
    scope: String,
    expires_at: String,
    inserted_at: String,
    updated_at: String,
}

impl TokenRow {
    fn into_token(self) -> Option<GitAccessToken> {
        Some(GitAccessToken {
            id: record_uuid(&self.id)?,
            person_id: record_uuid(&self.person_id)?,
            project_id: self.project_id.as_ref().and_then(record_uuid),
            token_hash: self.token_hash,
            scope: self.scope,
            expires_at: self.expires_at,
            inserted_at: self.inserted_at,
            updated_at: self.updated_at,
        })
    }
}

const TOKEN_SELECT: &str =
    "id, person_id, project_id, token_hash, scope, expires_at, inserted_at, updated_at";

/// Errors minting or validating a token.
#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error(transparent)]
    Postgres(#[from] matter::MatterAuthorityError),
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    #[error(transparent)]
    Project(#[from] projects::ProjectStoreError),
    #[error("minting a token returned no usable row")]
    WriteReturnedNothing,
}

/// SHA-256 hex of a token plaintext. Deterministic — `store` never generates
/// the secret itself.
#[must_use]
pub fn hash_token(plaintext: &str) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let digest = Sha256::digest(plaintext.as_bytes());
    let mut out = String::with_capacity(64);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Persist a freshly minted token. A project-scoped token read-backs its
/// project first because `record<project>` validates only record shape.
pub async fn mint(
    surreal: &SurrealDb,
    person_id: Uuid,
    project_id: Option<Uuid>,
    scope: &str,
    plaintext: &str,
    expires_at: DateTime<Utc>,
) -> Result<GitAccessToken, TokenError> {
    if let Some(project_id) = project_id {
        if projects::find_by_id(surreal, project_id).await?.is_none() {
            return Err(projects::ProjectStoreError::NoSuchProject(project_id).into());
        }
    }
    if let Some(pg) = matter::authority_for("git_access_token").await? {
        let now = Utc::now().to_rfc3339();
        let row = GitAccessToken {
            id: Uuid::now_v7(),
            person_id,
            project_id,
            token_hash: hash_token(plaintext),
            scope: scope.to_owned(),
            expires_at: expires_at.to_rfc3339(),
            inserted_at: now.clone(),
            updated_at: now,
        };
        return Ok(matter::upsert(&pg, "git_access_token", &row).await?);
    }
    let now = Utc::now().to_rfc3339();
    let id = Uuid::now_v7();
    let mut response = surreal
        .query(format!(
            "CREATE git_access_token CONTENT {{ id: $id, person_id: $person_id, project_id: $project_id, token_hash: $token_hash, scope: $scope, expires_at: $expires_at, inserted_at: $now, updated_at: $now }} RETURN {TOKEN_SELECT}"
        ))
        .bind(("id", record_id("git_access_token", id)))
        .bind(("person_id", record_id("person", person_id)))
        .bind(("project_id", project_id.map(|id| record_id("project", id))))
        .bind(("token_hash", hash_token(plaintext)))
        .bind(("scope", scope.to_string()))
        .bind(("expires_at", expires_at.to_rfc3339()))
        .bind(("now", now))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let row: Option<TokenRow> = response.take(0)?;
    crate::postgres_mirror::records::after_write(surreal, "git_access_token", id).await;
    row.and_then(TokenRow::into_token)
        .ok_or(TokenError::WriteReturnedNothing)
}

/// Resolve a presented token, treating expired or malformed expiry values as
/// absent. The natural-key index makes the lookup one row.
pub async fn validate(
    surreal: &SurrealDb,
    plaintext: &str,
    now: DateTime<Utc>,
) -> Result<Option<GitAccessToken>, TokenError> {
    if let Some(pg) = matter::authority_for("git_access_token").await? {
        let token: Option<GitAccessToken> = matter::find(
            &pg,
            "git_access_token",
            json!({"token_hash": hash_token(plaintext)}),
        )
        .await?;
        return Ok(token.filter(|token| {
            DateTime::parse_from_rfc3339(&token.expires_at).is_ok_and(|expiry| expiry > now)
        }));
    }

    let mut response = surreal
        .query(format!(
            "SELECT {TOKEN_SELECT} FROM ONLY git_access_token WHERE token_hash = $token_hash LIMIT 1"
        ))
        .bind(("token_hash", hash_token(plaintext)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let row: Option<TokenRow> = response.take(0)?;
    let token = row.and_then(TokenRow::into_token);
    Ok(token.filter(|token| {
        DateTime::parse_from_rfc3339(&token.expires_at).is_ok_and(|expiry| expiry > now)
    }))
}

/// Revoke a token by id. Deleting an absent row is intentionally a no-op.
pub async fn revoke(surreal: &SurrealDb, token_id: Uuid) -> Result<(), TokenError> {
    if let Some(pg) = matter::authority_for("git_access_token").await? {
        matter::delete(&pg, "git_access_token", json!({"id": token_id})).await?;
        return Ok(());
    }

    surreal
        .query("DELETE $id")
        .bind(("id", record_id("git_access_token", token_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    crate::postgres_mirror::records::after_write(surreal, "git_access_token", token_id).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn hash_is_stable_and_hex() {
        let hash = hash_token("hunter2");
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|character| character.is_ascii_hexdigit()));
        assert_eq!(hash, hash_token("hunter2"));
        assert_ne!(hash, hash_token("hunter3"));
    }

    #[tokio::test]
    async fn mint_then_validate_resolves_identity_and_scope() {
        let Some((options, database)) = crate::postgres_mirror::tests::fresh_schema().await else {
            return;
        };
        let pg = crate::postgres_mirror::tests::connect(&options, &database).await;
        crate::postgres::fixtures::activate_family(
            &pg,
            crate::postgres_backfill::family::Family::Satellites,
        )
        .await;
        let unavailable = crate::surreal::SurrealDb::uninitialized();
        crate::postgres::matter::with_test_db(pg.clone(), async {
            let (person, _, _) = crate::postgres::fixtures::workflow_fixture(&pg).await;
            let now = Utc::now();
            let minted = mint(
                &unavailable,
                person,
                None,
                SCOPE_WRITE,
                "the-secret",
                now + Duration::hours(1),
            )
            .await
            .unwrap();
            assert_eq!(minted.token_hash, hash_token("the-secret"));
            let resolved = validate(&unavailable, "the-secret", now)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(resolved.person_id, person);
            assert_eq!(resolved.scope, SCOPE_WRITE);
        })
        .await;
        pg.pool().close().await;
    }
}

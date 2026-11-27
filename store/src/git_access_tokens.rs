//! Project-scoped Personal Access Tokens for the Git transport.
//!
//! The plaintext never reaches storage: callers provide it once, this module
//! stores only its SHA-256 hash, and a missing or expired row is unauthenticated.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::matter;
use crate::projects;
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

/// Errors minting or validating a token.
#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error(transparent)]
    Postgres(#[from] matter::MatterAuthorityError),
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
    application_store: &ApplicationStore,
    person_id: Uuid,
    project_id: Option<Uuid>,
    scope: &str,
    plaintext: &str,
    expires_at: DateTime<Utc>,
) -> Result<GitAccessToken, TokenError> {
    if let Some(project_id) = project_id {
        if projects::find_by_id(application_store, project_id)
            .await?
            .is_none()
        {
            return Err(projects::ProjectStoreError::NoSuchProject(project_id).into());
        }
    }
    let pg = matter::authority_for("git_access_token").await?;
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
    Ok(matter::upsert(&pg, "git_access_token", &row).await?)
}

/// Resolve a presented token, treating expired or malformed expiry values as
/// absent. The natural-key index makes the lookup one row.
pub async fn validate(
    _application_store: &ApplicationStore,
    plaintext: &str,
    now: DateTime<Utc>,
) -> Result<Option<GitAccessToken>, TokenError> {
    let pg = matter::authority_for("git_access_token").await?;
    let token: Option<GitAccessToken> = matter::find(
        &pg,
        "git_access_token",
        json!({"token_hash": hash_token(plaintext)}),
    )
    .await?;
    Ok(token.filter(|token| {
        DateTime::parse_from_rfc3339(&token.expires_at).is_ok_and(|expiry| expiry > now)
    }))
}

/// Revoke a token by id. Deleting an absent row is intentionally a no-op.
pub async fn revoke(
    _application_store: &ApplicationStore,
    token_id: Uuid,
) -> Result<(), TokenError> {
    let pg = matter::authority_for("git_access_token").await?;
    matter::delete(&pg, "git_access_token", json!({"id": token_id})).await?;
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
        let Some((options, database)) = crate::postgres::fixtures::fresh_schema().await else {
            return;
        };
        let pg = crate::postgres::fixtures::connect(&options, &database).await;
        crate::postgres::fixtures::activate_family(
            &pg,
            crate::postgres::history::Family::Satellites,
        )
        .await;
        let unavailable = crate::application_store::ApplicationStore::uninitialized();
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

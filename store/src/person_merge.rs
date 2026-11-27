//! Merge two Person rows that are one human (LAW-169).
//!
//! An attorney who changes firms arrives under a new email, and a Person is
//! keyed by email, so the directory ends up with two rows for one human and
//! the Conflict-Check Graph splits their relationships across both.
//! [`merge`] folds the duplicate into the Person to keep.
//!
//! PostgreSQL People authority moves relational and tagged references, derives
//! participation from the kept Person's tier, and maintains the AGE projection
//! in one transaction. The explicit Source compatibility path remains separate.

use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::persons::{self, PersonError};

/// Why a merge was refused or failed.
#[derive(Debug, thiserror::Error)]
pub enum PersonMergeError {
    /// `keep` and `drop` name the same Person.
    #[error("keep and drop are the same Person")]
    SameRecord,
    /// One of the two Persons does not exist.
    #[error("no such Person: {0}")]
    NotFound(String),
    /// The drop Person is not a client; privileged tiers are edit-only.
    #[error(
        "only a client Person can be merged away; Owner, admin, lawyer, and clerk people are edit-only"
    )]
    Blocked,
    /// The actor may not govern the kept identity's system tier.
    #[error("your system role cannot govern this identity merge")]
    Governance,
    /// Immutable billing author or reviewer evidence retains the dropped identity.
    #[error("immutable billing history prevents merging this Person")]
    RetainedBillingHistory,
    /// Immutable Legal Plan Claim evidence retains the dropped actor identity.
    #[error("immutable Legal Plan Claim history prevents merging this Person")]
    RetainedClaimHistory,
    /// A Person read or write failed.
    #[error(transparent)]
    Person(#[from] PersonError),
    /// A database operation failed.
    /// The Project participation ledger could not be read or republished.
    #[error(transparent)]
    Project(#[from] crate::projects::ProjectStoreError),
}

/// What a merge did.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PersonMergeReport {
    pub kept_id: Uuid,
    pub dropped_id: Uuid,
    /// Every address the kept Person answers to afterwards, lowercased.
    pub emails: Vec<String>,
}

/// [`merge`], naming each Person by an address it answers to, primary or
/// alias, case-insensitively.
///
/// # Errors
///
/// See [`PersonMergeError`].
pub async fn merge_by_email(
    db: &ApplicationStore,
    keep: &str,
    dropped: &str,
) -> Result<PersonMergeReport, PersonMergeError> {
    merge_by_email_with_actor(db, keep, dropped, None).await
}

/// Merge through an actor-facing command using the existing identity-governance ladder.
///
/// # Errors
/// Refuses an actor below the kept identity; native rechecks under the Person writer locks.
pub async fn merge_by_email_as_actor(
    db: &ApplicationStore,
    keep: &str,
    dropped: &str,
    actor: persons::Role,
) -> Result<PersonMergeReport, PersonMergeError> {
    merge_by_email_with_actor(db, keep, dropped, Some(actor)).await
}

async fn merge_by_email_with_actor(
    db: &ApplicationStore,
    keep: &str,
    dropped: &str,
    actor: Option<persons::Role>,
) -> Result<PersonMergeReport, PersonMergeError> {
    let find = |email: &str| {
        let email = email.to_string();
        async move {
            persons::find_by_any_email(db, &email)
                .await?
                .ok_or(PersonMergeError::NotFound(email))
        }
    };
    let keep = find(keep).await?;
    let dropped = find(dropped).await?;
    merge_with_actor(db, keep.id, dropped.id, actor).await
}

/// Fold `drop_id` into `keep_id`: relationships and mailbox claims move to
/// the kept Person. Application merges delete the duplicate relational row
/// and converge the AGE graph in the same PostgreSQL transaction.
///
/// Only a client Person can be dropped, matching
/// [`crate::people_commands::delete_person`]. An application refusal or failure
/// rolls back the transaction, leaving both Persons and their evidence in place.
///
/// # Errors
///
/// See [`PersonMergeError`].
pub async fn merge(
    db: &ApplicationStore,
    keep_id: Uuid,
    drop_id: Uuid,
) -> Result<PersonMergeReport, PersonMergeError> {
    merge_with_actor(db, keep_id, drop_id, None).await
}

pub(crate) fn require_identity_governance(
    actor: persons::Role,
    kept: persons::Role,
) -> Result<(), PersonMergeError> {
    if !actor.is_lawyer_tier() || kept.authority_rank() > actor.authority_rank() {
        return Err(PersonMergeError::Governance);
    }
    Ok(())
}

async fn merge_with_actor(
    db: &ApplicationStore,
    keep_id: Uuid,
    drop_id: Uuid,
    actor: Option<persons::Role>,
) -> Result<PersonMergeReport, PersonMergeError> {
    if keep_id == drop_id {
        return Err(PersonMergeError::SameRecord);
    }
    let keep = persons::find_by_id(db, keep_id)
        .await?
        .ok_or_else(|| PersonMergeError::NotFound(keep_id.to_string()))?;
    let dropped = persons::find_by_id(db, drop_id)
        .await?
        .ok_or_else(|| PersonMergeError::NotFound(drop_id.to_string()))?;
    if dropped.role != persons::Role::Client {
        return Err(PersonMergeError::Blocked);
    }
    if let Some(actor) = actor {
        require_identity_governance(actor, keep.role)?;
    }
    let pg = persons::postgres_authority().await?;
    Box::pin(crate::postgres::people::merge(
        &pg, db, keep_id, drop_id, actor,
    ))
    .await?;
    let mut emails = persons::aliases(db, keep_id).await?;
    emails.push(keep.email.trim().to_lowercase());
    emails.sort();
    emails.dedup();
    Ok(PersonMergeReport {
        kept_id: keep_id,
        dropped_id: drop_id,
        emails,
    })
}

#[cfg(test)]
mod tests {}

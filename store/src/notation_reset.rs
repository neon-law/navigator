//! The daily Notation draft reset: which rows count as a draft, and the one
//! delete that removes a draft together with everything that hangs off it.
//!
//! # The draft predicate lives here, in one function
//!
//! A deployment's reset job deletes every draft once a day. "Draft" must mean
//! the same thing on every run and on every deployment, so [`is_draft`] is the
//! single definition and the job never re-derives it. A Notation is a draft
//! when all three hold:
//!
//! 1. its `state` ranks **below** `lawyer_review` on
//!    [`crate::notations`]' progress table — `BEGIN` or `intake_persisted`
//!    (an unrecognised state is **not** a draft: the table ranks it beside
//!    intake without asserting an order it cannot back, and a reset must
//!    never guess);
//! 2. it has no [`crate::notation_documents`] identity — a Word package has
//!    never been imported against it — and no rendered PDF under its
//!    `notations/<id>/` storage prefix; and
//! 3. no [`crate::signatures`] row names it.
//!
//! A [`crate::notation_drafts`] row — the stored, addressable, **unrun**
//! preview — is a draft by construction: it never has a Notation, a workflow
//! instance, or a journal, so every one of them is reset.
//!
//! # The PostgreSQL delete is a transaction and re-checks the predicate
//!
//! The PostgreSQL [`delete_draft`] re-reads the Notation and applies [`is_draft`] again
//! inside the delete, so a Notation that advanced to `lawyer_review` between
//! discovery and deletion is kept. The dependents — `answer`,
//! `notation_event`, `notation_clause`, `notation_source` — and the Notation
//! itself go in one transaction, so a crash leaves either the whole draft or
//! nothing. Deleting what is already gone is a no-op, which is what makes a
//! rerun safe.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::notations::{progress_rank, TABLE as NOTATION_TABLE};
use crate::postgres::matter;

/// Errors from the reset's reads and deletes.
#[derive(Debug, thiserror::Error)]
pub enum NotationResetError {
    #[error("PostgreSQL draft operation failed")]
    Postgres(#[from] sqlx::Error),
    #[error(transparent)]
    Authority(#[from] matter::MatterAuthorityError),
    #[error("notation: {0}")]
    Notation(#[from] crate::notations::NotationError),
}

/// Whether a Notation in `state`, with or without a document and a
/// signature, is a draft the daily reset may delete.
///
/// The one definition. `has_document` is true when a `notation_document`
/// identity or a rendered PDF exists for the Notation; `has_signature` when
/// any `signature` row names it, whatever its state.
#[must_use]
pub fn is_draft(state: &str, has_document: bool, has_signature: bool) -> bool {
    is_draft_state(state) && !has_document && !has_signature
}

/// The state half of [`is_draft`]: below `lawyer_review` on the progress
/// table, and one of the two states the table actually names there.
fn is_draft_state(state: &str) -> bool {
    let prefix = state.split_once("__").map_or(state, |(prefix, _)| prefix);
    matches!(prefix, "BEGIN" | "intake_persisted")
        && progress_rank(state) < progress_rank("lawyer_review")
}

/// What [`delete_draft`] did with one Notation id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResetOutcome {
    /// The draft and its dependents were deleted.
    Deleted,
    /// No such Notation: a rerun after a crash, or a concurrent delete.
    AlreadyGone,
    /// The Notation no longer satisfies [`is_draft`] and was left alone.
    Kept,
}

/// Every Notation id that satisfies [`is_draft`] right now. Ids only.
///
/// # Errors
///
/// [`NotationResetError`] when a read fails.
pub async fn list_drafts(db: &ApplicationStore) -> Result<Vec<Uuid>, NotationResetError> {
    let mut drafts = Vec::new();
    for notation in crate::notations::list_all(db).await? {
        if !is_draft_state(&notation.state) {
            continue;
        }
        if draft_facts(db, notation.id, &notation.state).await? {
            drafts.push(notation.id);
        }
    }
    Ok(drafts)
}

/// Every stored preview draft id, live or expired. Ids only.
///
/// # Errors
///
/// [`NotationResetError`] when the read fails.
pub async fn list_preview_drafts(_db: &ApplicationStore) -> Result<Vec<Uuid>, NotationResetError> {
    let pg = matter::authority().await?;
    Ok(
        sqlx::query_scalar("SELECT id FROM notation_draft ORDER BY id")
            .fetch_all(pg.pool())
            .await?,
    )
}

/// Apply [`is_draft`] to the live rows for one Notation.
async fn draft_facts(
    _db: &ApplicationStore,
    notation_id: Uuid,
    state: &str,
) -> Result<bool, NotationResetError> {
    let (document, signature) = {
        let pg = matter::authority_for(NOTATION_TABLE).await?;
        crate::postgres::notations::draft_facts(&pg, notation_id).await?
    };
    Ok(is_draft(state, document, signature))
}

/// Delete one draft Notation with its dependents, in one transaction, after
/// re-checking [`is_draft`] against the live rows.
///
/// # Errors
///
/// [`NotationResetError`] when a read or the delete fails. A missing
/// Notation is [`ResetOutcome::AlreadyGone`], not an error.
pub async fn delete_draft(
    _db: &ApplicationStore,
    id: Uuid,
) -> Result<ResetOutcome, NotationResetError> {
    let pg = matter::authority_for(NOTATION_TABLE).await?;
    Ok(crate::postgres::notations::delete_draft(&pg, id).await?)
}

/// Delete one stored preview draft. Deleting a missing row is a no-op.
///
/// # Errors
///
/// [`NotationResetError`] when the delete fails.
pub async fn delete_preview_draft(
    _db: &ApplicationStore,
    id: Uuid,
) -> Result<(), NotationResetError> {
    let pg = matter::authority().await?;
    matter::delete(
        &pg,
        crate::notation_drafts::TABLE,
        serde_json::json!({"id": id}),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_draft;
    use crate::notations::progress_rank;

    #[test]
    fn only_the_two_states_below_lawyer_review_are_drafts() {
        assert!(is_draft("BEGIN", false, false));
        assert!(is_draft("intake_persisted", false, false));
        assert!(is_draft("intake_persisted__client", false, false));
        for state in [
            "lawyer_review",
            "lawyer_review__pending",
            "reask",
            "client_review",
            "generate_pdf__pending",
            "sent_for_signature__pending",
            "client_signature",
            "notarization",
            "e_filing",
            "END",
        ] {
            assert!(!is_draft(state, false, false), "{state} is past draft");
        }
    }

    #[test]
    fn an_unrecognised_state_is_never_a_draft_even_though_it_ranks_beside_intake() {
        assert_eq!(
            progress_rank("some_future_step"),
            progress_rank("intake_persisted"),
            "the progress table parks an unknown prefix beside intake"
        );
        assert!(!is_draft("some_future_step", false, false));
        assert!(!is_draft("", false, false));
    }

    #[test]
    fn a_document_or_a_signature_takes_a_notation_out_of_draft() {
        assert!(!is_draft("BEGIN", true, false));
        assert!(!is_draft("BEGIN", false, true));
        assert!(!is_draft("intake_persisted", true, true));
    }

    #[test]
    fn the_predicate_agrees_with_the_progress_table() {
        for state in ["BEGIN", "intake_persisted"] {
            assert!(progress_rank(state) < progress_rank("lawyer_review"));
            assert!(is_draft(state, false, false));
        }
    }
}

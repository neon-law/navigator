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
//! # The delete is a transaction and re-checks the predicate
//!
//! [`delete_draft`] re-reads the Notation and applies [`is_draft`] again
//! inside the delete, so a Notation that advanced to `lawyer_review` between
//! discovery and deletion is kept. The dependents — `answer`,
//! `notation_event`, `notation_clause`, `notation_source` — and the Notation
//! itself go in one transaction, so a crash leaves either the whole draft or
//! nothing. Deleting what is already gone is a no-op, which is what makes a
//! rerun safe.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::notations::{progress_rank, TABLE as NOTATION_TABLE};
use crate::surreal::{record_id, record_uuid, SurrealDb};

/// Errors from the reset's reads and deletes.
#[derive(Debug, thiserror::Error)]
pub enum NotationResetError {
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
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
pub async fn list_drafts(db: &SurrealDb) -> Result<Vec<Uuid>, NotationResetError> {
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
pub async fn list_preview_drafts(db: &SurrealDb) -> Result<Vec<Uuid>, NotationResetError> {
    let mut response = db
        .query(format!(
            "SELECT VALUE id FROM {} ORDER BY id ASC",
            crate::notation_drafts::TABLE
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let ids: Vec<surrealdb::types::RecordId> = response.take(0)?;
    Ok(ids.iter().filter_map(record_uuid).collect())
}

/// Apply [`is_draft`] to the live rows for one Notation.
async fn draft_facts(
    db: &SurrealDb,
    notation_id: Uuid,
    state: &str,
) -> Result<bool, NotationResetError> {
    let notation = record_id(NOTATION_TABLE, notation_id);
    let prefix = format!("notations/{notation_id}/");
    let mut response = db
        .query(format!(
            "SELECT VALUE id FROM {} WHERE notation_id = $notation LIMIT 1; \
             SELECT VALUE id FROM asset WHERE string::starts_with(storage_key, $prefix) \
             OR (secondary_storage_key != NONE AND string::starts_with(secondary_storage_key, $prefix)) LIMIT 1; \
             SELECT VALUE id FROM {} WHERE notation_id = $notation LIMIT 1",
            crate::notation_documents::DOCUMENT_TABLE,
            crate::signatures::TABLE,
        ))
        .bind(("notation", notation))
        .bind(("prefix", prefix))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let documents: Vec<surrealdb::types::RecordId> = response.take(0)?;
    let rendered: Vec<surrealdb::types::RecordId> = response.take(1)?;
    let signatures: Vec<surrealdb::types::RecordId> = response.take(2)?;
    Ok(is_draft(
        state,
        !documents.is_empty() || !rendered.is_empty(),
        !signatures.is_empty(),
    ))
}

/// Delete one draft Notation with its dependents, in one transaction, after
/// re-checking [`is_draft`] against the live rows.
///
/// # Errors
///
/// [`NotationResetError`] when a read or the delete fails. A missing
/// Notation is [`ResetOutcome::AlreadyGone`], not an error.
pub async fn delete_draft(db: &SurrealDb, id: Uuid) -> Result<ResetOutcome, NotationResetError> {
    let Some(notation) = crate::notations::find_by_id(db, id).await? else {
        return Ok(ResetOutcome::AlreadyGone);
    };
    if !draft_facts(db, id, &notation.state).await? {
        return Ok(ResetOutcome::Kept);
    }
    db.query(
        "BEGIN TRANSACTION; \
         DELETE answer WHERE notation_id = $notation; \
         DELETE notation_event WHERE notation_id = $notation; \
         DELETE notation_clause WHERE notation_id = $notation; \
         DELETE notation_source WHERE notation_id = $notation; \
         DELETE $notation; \
         COMMIT TRANSACTION;",
    )
    .bind(("notation", record_id(NOTATION_TABLE, id)))
    .await
    .and_then(surrealdb::IndexedResults::check)?;
    // The Notation's PostgreSQL events and answers go with it.
    crate::postgres_mirror::after_write(db, crate::postgres_mirror::MirrorKey::Notation(id)).await;
    Ok(ResetOutcome::Deleted)
}

/// Delete one stored preview draft. Deleting a missing row is a no-op.
///
/// # Errors
///
/// [`NotationResetError`] when the delete fails.
pub async fn delete_preview_draft(db: &SurrealDb, id: Uuid) -> Result<(), NotationResetError> {
    db.query("DELETE $id")
        .bind(("id", record_id(crate::notation_drafts::TABLE, id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    crate::postgres_mirror::records::after_write(db, crate::notation_drafts::TABLE, id).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        delete_draft, delete_preview_draft, is_draft, list_drafts, list_preview_drafts,
        ResetOutcome,
    };
    use crate::notations::progress_rank;
    use crate::test_support::{mem_surreal, seed_notation};
    use uuid::Uuid;

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

    async fn seed_dependents(surreal: &crate::surreal::SurrealDb, notation_id: Uuid) {
        let notation = crate::notations::find_by_id(surreal, notation_id)
            .await
            .unwrap()
            .unwrap();
        let question = crate::questions::create(
            surreal,
            &crate::questions::NewQuestion::new(
                format!("reset-{}", Uuid::now_v7()),
                "Full name?",
                "string",
            ),
        )
        .await
        .unwrap();
        crate::answers::record(
            surreal,
            &crate::answers::NewAnswer::new(
                question.id,
                notation.person_id,
                crate::answers::primitive("Libra"),
            )
            .in_notation(notation_id, "intake__client"),
        )
        .await
        .unwrap();
        crate::notation_events::append_event(
            surreal,
            crate::notation_events::TransitionRecord {
                notation_id,
                acting_person_id: None,
                machine_kind: crate::notation_events::MACHINE_QUESTIONNAIRE,
                from_state: "BEGIN",
                to_state: "intake__client",
                condition: "next",
                payload_json: None,
                recorded_at: "2026-10-10T00:00:00Z",
            },
        )
        .await
        .unwrap();
    }

    async fn count(surreal: &crate::surreal::SurrealDb, table: &str, notation_id: Uuid) -> usize {
        let mut response = surreal
            .query(format!(
                "SELECT VALUE id FROM {table} WHERE notation_id = $notation"
            ))
            .bind((
                "notation",
                crate::surreal::record_id(crate::notations::TABLE, notation_id),
            ))
            .await
            .unwrap();
        let ids: Vec<surrealdb::types::RecordId> = response.take(0).unwrap();
        ids.len()
    }

    #[tokio::test]
    async fn a_begin_notation_is_deleted_with_its_answers_and_events_and_a_rerun_deletes_nothing() {
        let surreal = mem_surreal().await;
        let draft = seed_notation(&surreal).await;
        seed_dependents(&surreal, draft).await;
        assert_eq!(count(&surreal, "answer", draft).await, 1);
        assert_eq!(count(&surreal, "notation_event", draft).await, 1);

        assert_eq!(list_drafts(&surreal).await.unwrap(), vec![draft]);
        assert_eq!(
            delete_draft(&surreal, draft).await.unwrap(),
            ResetOutcome::Deleted
        );
        assert!(crate::notations::find_by_id(&surreal, draft)
            .await
            .unwrap()
            .is_none());
        assert_eq!(count(&surreal, "answer", draft).await, 0);
        assert_eq!(count(&surreal, "notation_event", draft).await, 0);

        assert!(list_drafts(&surreal).await.unwrap().is_empty());
        assert_eq!(
            delete_draft(&surreal, draft).await.unwrap(),
            ResetOutcome::AlreadyGone
        );
    }

    #[tokio::test]
    async fn a_notation_at_lawyer_review_survives_the_reset() {
        let surreal = mem_surreal().await;
        let reviewed = seed_notation(&surreal).await;
        crate::notations::update_state(&surreal, reviewed, "lawyer_review__pending")
            .await
            .unwrap();
        let draft = seed_notation(&surreal).await;

        assert_eq!(list_drafts(&surreal).await.unwrap(), vec![draft]);
        assert_eq!(
            delete_draft(&surreal, reviewed).await.unwrap(),
            ResetOutcome::Kept,
            "the delete re-checks the predicate and keeps a reviewed Notation"
        );
        assert!(crate::notations::find_by_id(&surreal, reviewed)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn a_signature_request_takes_a_begin_notation_out_of_the_reset() {
        let surreal = mem_surreal().await;
        let signing = seed_notation(&surreal).await;
        crate::signatures::record_request(
            &surreal,
            signing,
            crate::signatures::SignatureProvider::DocuSign,
            "envelope-1",
        )
        .await
        .unwrap();
        assert!(list_drafts(&surreal).await.unwrap().is_empty());
        assert_eq!(
            delete_draft(&surreal, signing).await.unwrap(),
            ResetOutcome::Kept
        );
    }

    #[tokio::test]
    async fn a_rendered_pdf_asset_takes_a_begin_notation_out_of_the_reset() {
        let surreal = mem_surreal().await;
        let rendered = seed_notation(&surreal).await;
        let notation = crate::notations::find_by_id(&surreal, rendered)
            .await
            .unwrap()
            .unwrap();
        surreal
            .query(
                "CREATE asset SET project_id = $project, storage_key = $key, \
                 content_type = 'application/pdf', byte_size = 1, sha256_hex = 'ab', \
                 filename = 'document.pdf', kind = 'unclassified', source = 'generated', \
                 visibility = 'internal'",
            )
            .bind((
                "project",
                crate::surreal::record_id(crate::projects::PROJECT_TABLE, notation.project_id),
            ))
            .bind(("key", crate::notations::document_pdf_storage_key(rendered)))
            .await
            .unwrap()
            .check()
            .unwrap();
        assert!(list_drafts(&surreal).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn every_preview_draft_is_reset_and_a_rerun_finds_none() {
        let surreal = mem_surreal().await;
        let project = crate::test_support::seed_project_surreal(&surreal, "preview").await;
        let preview = crate::notation_drafts::create(
            &surreal,
            &crate::notation_drafts::NewNotationDraft {
                project_id: project,
                slug: "engagement",
                title: "Engagement",
                source: "---\ntitle: Engagement\n---\nBody",
                notation_id: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            list_preview_drafts(&surreal).await.unwrap(),
            vec![preview.id]
        );
        delete_preview_draft(&surreal, preview.id).await.unwrap();
        assert!(list_preview_drafts(&surreal).await.unwrap().is_empty());
        delete_preview_draft(&surreal, preview.id)
            .await
            .expect("deleting a missing preview is a no-op");
    }
}

//! Review comments whose Communication spine needs the memory Source engine.
use super::tests::{database, draft};
use super::with_test_db;
use crate::document_comments::{
    self, clear_communication_links, create_with_communication, find_by_id, for_review_document,
    set_resolved, NewComment, NewLinkedComment,
};
use crate::review_documents::upsert_draft;

#[tokio::test]
async fn comments_round_trip_and_the_retention_sweep_clears_only_named_links() {
    let Some(pg) = database().await else { return };
    // Comments carrying a spine link need a real `communications::ingest`,
    // so the linked write below runs against the memory engine while its
    // satellite row lands in PostgreSQL under the review authority.
    let surreal = crate::surreal::test_support::mem().await;
    let notation_id = crate::test_support::seed_notation(&surreal).await;
    let project_id = crate::notations::find_by_id(&surreal, notation_id)
        .await
        .unwrap()
        .unwrap()
        .project_id;
    let person_id = crate::persons::find_or_create(
        &surreal,
        &crate::persons::NewPerson::new("Libra", "libra@example.com"),
    )
    .await
    .unwrap()
    .id;

    with_test_db(pg.clone(), async {
        let document_id = upsert_draft(&surreal, &draft(notation_id, "will", "<p>One</p>"))
            .await
            .unwrap();
        let anchored = |body: &'static str| NewComment {
            review_document_id: document_id,
            person_id,
            anchor_start: 3,
            anchor_end: 8,
            quoted_text: "Libra",
            body,
        };
        let first = document_comments::create(&surreal, &anchored("First"))
            .await
            .unwrap();
        let second = document_comments::create(&surreal, &anchored("Second"))
            .await
            .unwrap();

        let comment = find_by_id(&surreal, second).await.unwrap().unwrap();
        assert!(
            comment.communication_id.is_none(),
            "a plain create carries no spine link"
        );
        assert_eq!(comment.anchor_start, 3);
        assert!(!comment.resolved);

        let thread = for_review_document(&surreal, document_id).await.unwrap();
        assert_eq!(
            thread.iter().map(|c| c.id).collect::<Vec<_>>(),
            vec![first, second],
            "oldest first"
        );

        set_resolved(&surreal, first, true).await.unwrap().unwrap();
        assert!(
            find_by_id(&surreal, first).await.unwrap().unwrap().resolved,
            "the flag flips"
        );

        // The sweep clears only the links it names. Comments carrying a
        // spine link need a real `communications::ingest`, so they are
        // written through the linked path — the spine row lands on the
        // SurrealDB fallback while the satellite lands in PostgreSQL.
        let linked_comment = create_with_communication(
            &surreal,
            &NewLinkedComment {
                project_id,
                review_document_id: document_id,
                person_id,
                direction: "inbound",
                anchor_start: 0,
                anchor_end: 1,
                quoted_text: "Libra",
                body: "Linked comment",
            },
        )
        .await
        .unwrap();
        clear_communication_links(&surreal, &[linked_comment.communication_id])
            .await
            .unwrap();
        assert!(
            find_by_id(&surreal, linked_comment.comment_id)
                .await
                .unwrap()
                .unwrap()
                .communication_id
                .is_none(),
            "a named link is cleared"
        );
        assert!(
            find_by_id(&surreal, first)
                .await
                .unwrap()
                .unwrap()
                .communication_id
                .is_none(),
            "a comment with no link stays linkless"
        );
    })
    .await;
}

/// The satellite-and-spine write: the comment lands in PostgreSQL under the
/// review authority while the spine row goes through Communications' own
/// authority (the SurrealDB path here, mirroring the PG-less fallback).
#[tokio::test]
async fn create_with_communication_writes_both_rows_and_links_them() {
    let Some(pg) = database().await else { return };
    let surreal = crate::surreal::test_support::mem().await;
    let notation_id = crate::test_support::seed_notation(&surreal).await;
    let project_id = crate::notations::find_by_id(&surreal, notation_id)
        .await
        .unwrap()
        .unwrap()
        .project_id;
    let person_id = crate::persons::find_or_create(
        &surreal,
        &crate::persons::NewPerson::new("Taurus", "taurus@example.com"),
    )
    .await
    .unwrap()
    .id;

    with_test_db(pg, async {
        let document_id = upsert_draft(&surreal, &draft(notation_id, "will", "<p>One</p>"))
            .await
            .unwrap();

        let created = create_with_communication(
            &surreal,
            &NewLinkedComment {
                project_id,
                review_document_id: document_id,
                person_id,
                direction: "inbound",
                anchor_start: 3,
                anchor_end: 8,
                quoted_text: "Libra",
                body: "Should this be my full legal name?",
            },
        )
        .await
        .unwrap();

        let comment = find_by_id(&surreal, created.comment_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(comment.communication_id, Some(created.communication_id));

        let thread = crate::communications::for_project(&surreal, project_id)
            .await
            .unwrap();
        assert_eq!(thread.len(), 1);
        assert_eq!(thread[0].id, created.communication_id);
    })
    .await;
}

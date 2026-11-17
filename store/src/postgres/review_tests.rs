//! The review authority's tests, against the PostgreSQL fixture. Each test
//! skips when `NAVIGATOR_TEST_POSTGRES_PORT` is unset.
use super::*;
use crate::document_comments::{
    self, clear_communication_links, create_with_communication, find_by_id, for_review_document,
    set_resolved, NewComment, NewLinkedComment,
};
use crate::postgres_mirror::tests::{connect, fresh_schema};
use crate::review_documents::{
    by_id, create, find_by_notation_kind, for_notation, notations_with_drafts, set_status,
    upsert_draft, NewReviewDocument, STATUS_PENDING_REVIEW,
};
use crate::surreal::SurrealDb;
use uuid::Uuid;

async fn database() -> Option<PostgresDb> {
    let (options, database) = fresh_schema().await?;
    Some(connect(&options, &database).await)
}

fn draft<'a>(notation_id: Uuid, kind: &'a str, body: &'a str) -> NewReviewDocument<'a> {
    NewReviewDocument {
        notation_id,
        kind,
        title: "Last Will and Testament",
        body_html: body,
    }
}

#[tokio::test]
async fn review_document_create_and_reads_round_trip_in_postgres() {
    let Some(pg) = database().await else { return };
    let surreal = SurrealDb::uninitialized();
    let notation_id = Uuid::new_v4();

    with_test_db(pg.clone(), async {
        let id = create(&surreal, &draft(notation_id, "will", "<p>One</p>"))
            .await
            .unwrap();

        let row = by_id(&surreal, id).await.unwrap().unwrap();
        assert_eq!(row.notation_id, notation_id);
        assert_eq!(row.kind, "will");
        assert_eq!(row.status, "draft");
        assert_eq!(row.body_html, "<p>One</p>");

        assert_eq!(
            find_by_notation_kind(&surreal, notation_id, "will")
                .await
                .unwrap()
                .unwrap()
                .id,
            id
        );
        assert!(find_by_notation_kind(&surreal, notation_id, "trust")
            .await
            .unwrap()
            .is_none());

        let all = for_notation(&surreal, notation_id).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, id);
    })
    .await;
}

#[tokio::test]
async fn upsert_draft_refreshes_a_draft_and_never_a_reviewed_one() {
    let Some(pg) = database().await else { return };
    let surreal = SurrealDb::uninitialized();
    let notation_id = Uuid::new_v4();

    with_test_db(pg.clone(), async {
        let first = upsert_draft(&surreal, &draft(notation_id, "will", "<p>One</p>"))
            .await
            .unwrap();
        let second = upsert_draft(&surreal, &draft(notation_id, "will", "<p>Two</p>"))
            .await
            .unwrap();
        assert_eq!(first, second, "re-render refreshes the same row");
        let row = by_id(&surreal, first).await.unwrap().unwrap();
        assert_eq!(
            row.body_html, "<p>Two</p>",
            "a draft takes the newest render"
        );

        set_status(&surreal, first, STATUS_PENDING_REVIEW)
            .await
            .unwrap()
            .unwrap();
        let again = upsert_draft(&surreal, &draft(notation_id, "will", "<p>Three</p>"))
            .await
            .unwrap();
        assert_eq!(again, first);
        let row = by_id(&surreal, first).await.unwrap().unwrap();
        assert_eq!(
            row.body_html, "<p>Two</p>",
            "a released document is not silently overwritten"
        );
        assert_eq!(row.status, STATUS_PENDING_REVIEW);
    })
    .await;
}

#[tokio::test]
async fn the_notation_kind_slot_is_unique_in_postgres() {
    let Some(pg) = database().await else { return };
    let surreal = SurrealDb::uninitialized();
    let notation_id = Uuid::new_v4();

    with_test_db(pg.clone(), async {
        let id = upsert_draft(&surreal, &draft(notation_id, "will", "<p>One</p>"))
            .await
            .unwrap();

        // A second writer that bypasses the read-then-write upsert hits the
        // same unique slot SurrealDB's index enforces — the index the
        // upsert's conflict path races through must exist here too.
        let duplicate = sqlx::query(
            "INSERT INTO review_document (id, notation_id, kind, title, body_html, status, \
             inserted_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, now(), now())",
        )
        .bind(Uuid::now_v7())
        .bind(notation_id)
        .bind("will")
        .bind("Last Will and Testament")
        .bind("<p>Duplicate</p>")
        .bind("draft")
        .execute(pg.pool())
        .await;
        match duplicate {
            Err(sqlx::Error::Database(database)) => assert_eq!(
                database.constraint(),
                Some("review_document_notation_kind"),
                "the unique slot must carry the Surreal index's name"
            ),
            other => panic!("expected a unique violation, got {other:?}"),
        }

        // The batched lifecycle probe reports exactly the notation that
        // carries an instrument.
        let abandoned = Uuid::new_v4();
        let found = notations_with_drafts(&surreal, &[notation_id, abandoned])
            .await
            .unwrap();
        assert!(found.contains(&notation_id));
        assert!(!found.contains(&abandoned));
        assert_eq!(
            id,
            upsert_draft(&surreal, &draft(notation_id, "will", "<p>One</p>"))
                .await
                .unwrap()
        );
    })
    .await;
}

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

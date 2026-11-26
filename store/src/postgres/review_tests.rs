//! The review authority's tests, against the PostgreSQL fixture. Each test
//! fails when `NAVIGATOR_TEST_POSTGRES_PORT` is unset.
use super::*;
use crate::postgres_mirror::tests::{connect, fresh_schema};
use crate::review_documents::{
    by_id, create, find_by_notation_kind, for_notation, notations_with_drafts, set_status,
    upsert_draft, NewReviewDocument, STATUS_PENDING_REVIEW,
};
use crate::surreal::SurrealDb;
use uuid::Uuid;

pub(super) async fn database() -> Option<PostgresDb> {
    let (options, database) = fresh_schema().await?;
    Some(connect(&options, &database).await)
}

pub(super) fn draft<'a>(notation_id: Uuid, kind: &'a str, body: &'a str) -> NewReviewDocument<'a> {
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

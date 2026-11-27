//! Immutable billing evidence retains its original Person identity.
use serde_json::Value;
use store::{application_store::ApplicationStore, postgres::PostgresDb};
use uuid::Uuid;

async fn billing_rows(pg: &PostgresDb) -> Vec<Value> {
    let mut rows = Vec::new();
    // Closed canonical tables; inspect the entire retained billing/time-entry samples.
    for table in ["billing_annotation", "time_entry"] {
        rows.push(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]'::jsonb) FROM {table} t"
        ))).fetch_one(pg.pool()).await.unwrap());
    }
    rows
}

async fn refusal(reviewed: bool) {
    store::test_support::with_postgres(async {
        let db = ApplicationStore::uninitialized();
        let pg = PostgresDb::application().await.unwrap();
        let parent = super::native_project::create("synthetic-immutable-billing-merge").await;
        let scenario = super::merge_fixture::Scenario::seed(&db, &pg, &parent).await;
        // Real role transition: an original lawyer's immutable history survives a later Client tier.
        store::persons::set_role(&db, scenario.dropped, store::persons::Role::Lawyer)
            .await
            .unwrap()
            .unwrap();
        let author = if reviewed {
            scenario.kept
        } else {
            scenario.dropped
        };
        if reviewed {
            store::projects::add_participation(&db, scenario.target.id, author, "lawyer")
                .await
                .unwrap();
        }
        let annotation = store::billing_annotations::Annotation {
            id: Uuid::new_v4(),
            change_id: "k".repeat(32),
            commit_id: "a".repeat(40),
            tenths: 2,
            performed_on: chrono::Utc::now().date_naive(),
        };
        store::billing_annotations::stage(&pg, scenario.target.id, author, &annotation)
            .await
            .unwrap();
        if reviewed {
            store::billing_annotations::review(
                &pg,
                scenario.target.id,
                scenario.dropped,
                annotation.id,
                store::billing_annotations::Decision::Deny,
                "Synthetic retained lawyer review",
            )
            .await
            .unwrap();
        }
        store::persons::set_role(&db, scenario.dropped, store::persons::Role::Client)
            .await
            .unwrap()
            .unwrap();
        let actual: Value =
            sqlx::query_scalar("SELECT to_jsonb(a) FROM billing_annotation a WHERE id=$1")
                .bind(annotation.id)
                .fetch_one(pg.pool())
                .await
                .unwrap();
        assert_eq!(actual["attorney_id"], serde_json::json!(author));
        if reviewed {
            assert_eq!(actual["reviewed_by"], serde_json::json!(scenario.dropped));
            assert!(!actual["reviewed_at"].is_null());
        } else {
            assert!(actual["reviewed_by"].is_null());
            assert!(actual["reviewed_at"].is_null());
        }
        let canonical_before = super::merge_fixture::snapshot(&pg).await;
        let billing_before = billing_rows(&pg).await;
        assert!(matches!(
            store::person_merge::merge(&db, scenario.kept, scenario.dropped).await,
            Err(store::person_merge::PersonMergeError::RetainedBillingHistory)
        ));
        assert_eq!(super::merge_fixture::snapshot(&pg).await, canonical_before);
        assert_eq!(billing_rows(&pg).await, billing_before);
        assert!(store::persons::find_by_id(&db, scenario.dropped)
            .await
            .unwrap()
            .is_some());
        assert!(store::persons::find_by_id(&db, scenario.kept)
            .await
            .unwrap()
            .is_some());
    })
    .await;
}

#[tokio::test]
async fn staged_billing_author_refuses_merge_without_changing_identity_or_participation() {
    refusal(false).await;
}

#[tokio::test]
async fn retained_billing_reviewer_refuses_merge_without_rewriting_review_evidence() {
    refusal(true).await;
}

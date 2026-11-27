//! Person merge rolls back tagged links and its actual AGE projection together.
use store::application_store::ApplicationStore;
use store::postgres::PostgresDb;

#[path = "support/merge_claim_refusal.rs"]
mod merge_claim_refusal;
#[path = "support/person_merge_graph.rs"]
mod merge_fixture;
#[path = "support/merge_governance_race.rs"]
mod merge_governance_race;
#[path = "support/native_project.rs"]
mod native_project;

#[path = "support/merge_billing_refusal.rs"]
mod merge_billing_refusal;

#[tokio::test]
async fn native_merge_preserves_typed_reference_metadata_and_derives_the_kept_identity_graph() {
    store::test_support::with_postgres(async {
        let db = ApplicationStore::uninitialized();
        let pg = PostgresDb::application().await.unwrap();
        let parent = native_project::create("synthetic-native-merge-parent").await;
        let scenario = merge_fixture::Scenario::seed(&db, &pg, &parent).await;
        store::person_merge::merge(&db, scenario.kept, scenario.dropped)
            .await
            .unwrap();
        scenario.assert_merged(&db, &pg).await;
    })
    .await;
}

#[tokio::test]
async fn rejected_graph_merge_preserves_all_rows_then_retries_with_typed_links_and_handles() {
    store::test_support::with_postgres(async {
        let db = ApplicationStore::uninitialized();
        let pg = PostgresDb::application().await.unwrap();
        let parent = native_project::create("synthetic-merge-parent").await;
        let scenario = merge_fixture::Scenario::seed(&db, &pg, &parent).await;
        let before = merge_fixture::snapshot(&pg).await;
        sqlx::raw_sql(r"
            CREATE SEQUENCE public.test_person_merge_reached_age;
            ALTER FUNCTION public.graph_merge_person(uuid,uuid) RENAME TO test_original_graph_merge_person;
            CREATE FUNCTION public.graph_merge_person(kept uuid, dropped uuid) RETURNS void LANGUAGE plpgsql AS $$
            BEGIN
                PERFORM public.test_original_graph_merge_person(kept,dropped);
                PERFORM nextval('public.test_person_merge_reached_age');
                RAISE EXCEPTION 'synthetic rejection after actual Person graph merge';
            END;
            $$;
        ").execute(pg.pool()).await.unwrap();
        let failed = store::person_merge::merge(&db, scenario.kept, scenario.dropped).await;
        assert!(matches!(failed, Err(store::person_merge::PersonMergeError::Person(
            store::persons::PersonError::Postgres(_)))));
        // nextval is not rolled back: only the real projector returning can fire this witness.
        let reached: (i64, bool) = sqlx::query_as(
            "SELECT last_value,is_called FROM public.test_person_merge_reached_age",
        ).fetch_one(pg.pool()).await.unwrap();
        assert_eq!(reached, (1, true), "failure did not reach the after-real-AGE injection");
        assert_eq!(merge_fixture::snapshot(&pg).await, before);
        sqlx::raw_sql("DROP FUNCTION public.graph_merge_person(uuid,uuid); ALTER FUNCTION public.test_original_graph_merge_person(uuid,uuid) RENAME TO graph_merge_person; DROP SEQUENCE public.test_person_merge_reached_age;")
            .execute(pg.pool()).await.unwrap();
        store::person_merge::merge(&db, scenario.kept, scenario.dropped).await.unwrap();
        scenario.assert_merged(&db, &pg).await;
    }).await;
}

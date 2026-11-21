//! Person merge rolls back tagged links and its actual AGE projection together.
use store::postgres::PostgresDb;
use store::surreal::SurrealDb;

#[path = "support/person_merge_graph.rs"]
mod merge_fixture;
#[path = "support/native_project.rs"]
mod native_project;

#[tokio::test]
async fn native_merge_preserves_typed_reference_metadata_and_derives_the_kept_identity_graph() {
    store::test_support::with_postgres(async {
        let db = SurrealDb::uninitialized();
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
        let db = SurrealDb::uninitialized();
        let pg = PostgresDb::application().await.unwrap();
        let parent = native_project::create("synthetic-merge-parent").await;
        let scenario = merge_fixture::Scenario::seed(&db, &pg, &parent).await;
        let before = merge_fixture::snapshot(&pg).await;
        sqlx::raw_sql(r"
            ALTER FUNCTION public.graph_merge_person(uuid,uuid) RENAME TO test_original_graph_merge_person;
            CREATE FUNCTION public.graph_merge_person(kept uuid, dropped uuid) RETURNS void LANGUAGE plpgsql AS $$
            BEGIN
                PERFORM public.test_original_graph_merge_person(kept,dropped);
                RAISE EXCEPTION 'synthetic rejection after actual Person graph merge';
            END;
            $$;
        ").execute(pg.pool()).await.unwrap();
        let failed = store::person_merge::merge(&db, scenario.kept, scenario.dropped).await;
        assert!(matches!(failed, Err(store::person_merge::PersonMergeError::Person(
            store::persons::PersonError::Postgres(_)))));
        assert_eq!(merge_fixture::snapshot(&pg).await, before);
        sqlx::raw_sql("DROP FUNCTION public.graph_merge_person(uuid,uuid); ALTER FUNCTION public.test_original_graph_merge_person(uuid,uuid) RENAME TO graph_merge_person;")
            .execute(pg.pool()).await.unwrap();
        store::person_merge::merge(&db, scenario.kept, scenario.dropped).await.unwrap();
        scenario.assert_merged(&db, &pg).await;
    }).await;
}

#[tokio::test]
async fn colliding_typed_holdings_refuse_the_merge_without_discarding_either_line() {
    store::test_support::with_postgres(async {
        let db = SurrealDb::uninitialized();
        let pg = PostgresDb::application().await.unwrap();
        let parent = native_project::create("synthetic-merge-collision-parent").await;
        let scenario = merge_fixture::Scenario::seed(&db, &pg, &parent).await;
        let share_class: String = sqlx::query_scalar("SELECT share_class FROM cap_table_holding WHERE project_id=$1 AND holder_id->>'tb'='person' AND holder_id->'id'->>'Uuid'=$2")
            .bind(scenario.target.id).bind(scenario.dropped.to_string()).fetch_one(pg.pool()).await.unwrap();
        store::cap_table_holdings::insert(&db, scenario.target.id, &store::cap_table_holdings::CapTableHoldingFields {
            holder: store::project_references::Party::person(scenario.kept), share_class,
            shares: 11, vesting_note: Some("Synthetic distinct vested line".into()),
        }).await.unwrap();
        let before = merge_fixture::snapshot(&pg).await;
        assert!(matches!(store::person_merge::merge(&db, scenario.kept, scenario.dropped).await,
            Err(store::person_merge::PersonMergeError::Person(store::persons::PersonError::Postgres(_)))));
        assert_eq!(merge_fixture::snapshot(&pg).await, before);
    }).await;
}

#[tokio::test]
async fn concurrent_relationship_writers_share_one_vertex_for_each_typed_endpoint() {
    store::test_support::with_postgres(async {
        let db = SurrealDb::uninitialized();
        let pg = PostgresDb::application().await.unwrap();
        let parent = native_project::create("synthetic-concurrent-graph-parent").await;
        let person = store::persons::create(
            &db,
            &store::persons::NewPerson::new(
                "Synthetic concurrent graph person",
                format!("racer-{}@example.test", uuid::Uuid::new_v4()),
            ),
        )
        .await
        .unwrap();
        let (person_id, entity_id) = (person.id, parent.entity_id);
        let mut writers = tokio::task::JoinSet::new();
        for _ in 0..6 {
            let owned = pg.clone();
            writers.spawn(async move {
                PostgresDb::with_application_db(owned, async {
                    store::relationships::record(
                        &SurrealDb::uninitialized(),
                        &store::relationships::NewRelationship {
                            from: store::relationships::Endpoint::Person,
                            from_id: person_id,
                            to: store::relationships::Endpoint::Entity,
                            to_id: entity_id,
                            kind: store::relationships::KIND_ADVERSE_TO.into(),
                            confidence_pct: 100,
                            source_kind: store::relationships::SOURCE_MANUAL.into(),
                            source_id: None,
                            detail: Some("Synthetic concurrent canonical write".into()),
                        },
                    )
                    .await
                    .unwrap();
                })
                .await
                .unwrap();
            });
        }
        while let Some(result) = writers.join_next().await {
            result.unwrap();
        }
        for (label, id) in [("Person", person_id), ("Entity", entity_id)] {
            let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT count(*) FROM navigator_graph.\"{label}\" WHERE properties::text::jsonb->>'id'=$1"
            )))
            .bind(id.to_string())
            .fetch_one(pg.pool())
            .await
            .unwrap();
            assert_eq!(count, 1);
        }
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM relationship")
            .fetch_one(pg.pool())
            .await
            .unwrap();
        let edges: i64 =
            sqlx::query_scalar("SELECT count(*) FROM navigator_graph.\"RELATIONSHIP\"")
                .fetch_one(pg.pool())
                .await
                .unwrap();
        assert_eq!((rows, edges), (6, 6));
    })
    .await;
}

#[tokio::test]
async fn identical_relationship_properties_preserve_both_typed_uuid_endpoints() {
    store::test_support::with_postgres(async {
        let db = SurrealDb::uninitialized();
        let pg = PostgresDb::application().await.unwrap();
        let parent = native_project::create("synthetic-typed-edge-collision").await;
        let mut people = Vec::new();
        for name in ["Synthetic collision first", "Synthetic collision second"] {
            people.push(
                store::persons::create(
                    &db,
                    &store::persons::NewPerson::new(
                        name,
                        format!("typed-{}@example.test", uuid::Uuid::new_v4()),
                    ),
                )
                .await
                .unwrap()
                .id,
            );
        }
        merge_fixture::typed_endpoint_collision(&db, &pg, parent.entity_id, people[0], people[1])
            .await;
    })
    .await;
}

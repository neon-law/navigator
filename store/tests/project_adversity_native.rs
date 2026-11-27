//! An adversity command commits its audit trail and conflict edge together.
use store::application_store::ApplicationStore;
use store::postgres::PostgresDb;
use store::project_references::{
    self, AdversityError, AdversitySubject, Party, RecordAdversityCommand,
};
use uuid::Uuid;

#[path = "support/native_project.rs"]
mod native_project;

async fn counts(pg: &PostgresDb, project: Uuid) -> (i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM relationship_log WHERE project_id=$1 AND action='adversity_recorded'), (SELECT count(*) FROM relationship r JOIN relationship_log l ON r.source_id=to_jsonb(l.id::text) WHERE l.project_id=$1 AND l.action='adversity_recorded')")
        .bind(project).fetch_one(pg.pool()).await.unwrap()
}

async fn graph_rows(pg: &PostgresDb, first: Uuid, second: Uuid) -> Vec<String> {
    sqlx::query_scalar("SELECT t.v::text FROM graph_conflict_edges($1) AS t(v) ORDER BY t.v::text")
        .bind(serde_json::json!([first.to_string(), second.to_string()]).to_string())
        .fetch_all(pg.pool())
        .await
        .unwrap()
}

async fn fail_after_loading_edge(pg: &PostgresDb) {
    sqlx::raw_sql(
        r"
        CREATE SEQUENCE public.test_adversity_reached_age;
        ALTER FUNCTION graph_load_edges(text, text) RENAME TO test_original_graph_load_edges;
        CREATE FUNCTION graph_load_edges(kind text, rows text)
        RETURNS SETOF ag_catalog.agtype LANGUAGE plpgsql AS $$
        BEGIN
            RETURN QUERY SELECT * FROM test_original_graph_load_edges(kind, rows);
            PERFORM nextval('public.test_adversity_reached_age');
            RAISE EXCEPTION 'synthetic failure after actual AGE edge insertion';
        END
        $$;
    ",
    )
    .execute(pg.pool())
    .await
    .unwrap();
}

#[tokio::test]
async fn age_failure_rolls_back_the_whole_adversity_command_without_touching_another_project() {
    store::test_support::with_postgres(async {
        let db = ApplicationStore::uninitialized();
        let pg = PostgresDb::application().await.unwrap();
        let target = native_project::create("synthetic-adversity-target").await;
        let other = native_project::create("synthetic-adversity-other").await;
        let actor = store::persons::create(&db, &store::persons::NewPerson::new(
            "Synthetic adversity actor", format!("actor-{}@example.test", Uuid::new_v4()),
        )).await.unwrap();
        let command = RecordAdversityCommand {
            project_id: target.id, party: Party::entity(other.entity_id),
            subject: AdversitySubject::Entity, actor_person_id: Some(actor.id),
        };
        assert!(matches!(project_references::record_adversity(&db, &command).await,
            Err(AdversityError::PartyNotReferenced)));
        assert_eq!(counts(&pg, target.id).await, (0, 0));
        for (project, party) in [(target.id, other.entity_id), (other.id, target.entity_id)] {
            project_references::reference(&db, project, Party::entity(party),
                project_references::REASON_COUNTERPARTY_ENTITY).await.unwrap();
        }
        let previous = project_references::record_adversity(&db, &RecordAdversityCommand {
            project_id: other.id, party: Party::entity(target.entity_id), ..command.clone()
        }).await.unwrap();
        assert_eq!(counts(&pg, other.id).await, (1, 1));
        let before_graph = graph_rows(&pg, target.entity_id, other.entity_id).await;
        assert!(!before_graph.is_empty());
        fail_after_loading_edge(&pg).await;
        let rejected = project_references::record_adversity(&db, &command).await;
        assert!(matches!(rejected, Err(AdversityError::Relationship(
            store::relationships::RelationshipError::Graph(store::postgres_graph::GraphError::Store(_))))));
        // Sequence advancement survives rollback and proves the real writer returned.
        let reached: (i64, bool) = sqlx::query_as(
            "SELECT last_value,is_called FROM public.test_adversity_reached_age",
        ).fetch_one(pg.pool()).await.unwrap();
        assert_eq!(reached, (1, true), "failure did not reach the after-real-AGE injection");
        assert_eq!(counts(&pg, target.id).await, (0, 0), "failed command left an audit entry");
        assert_eq!(counts(&pg, other.id).await, (1, 1));
        assert_eq!(graph_rows(&pg, target.entity_id, other.entity_id).await, before_graph);
        sqlx::raw_sql("DROP FUNCTION graph_load_edges(text,text); ALTER FUNCTION test_original_graph_load_edges(text,text) RENAME TO graph_load_edges; DROP SEQUENCE public.test_adversity_reached_age;")
            .execute(pg.pool()).await.unwrap();
        let accepted = project_references::record_adversity(&db, &command).await.unwrap();
        assert_eq!(counts(&pg, target.id).await, (1, 1));
        assert_eq!(counts(&pg, other.id).await, (1, 1));
        assert_eq!(accepted.from_id, target.entity_id);
        assert_eq!(accepted.to_id, other.entity_id);
        assert_eq!(accepted.kind, store::relationships::KIND_ADVERSE_TO);
        assert_eq!(accepted.confidence_pct, 100);
        assert_eq!(accepted.source_kind, store::relationships::SOURCE_MANUAL);
        let log = store::relationship_logs::for_project(&db, target.id).await.unwrap()
            .into_iter().find(|log| log.action == project_references::ADVERSITY_RECORDED_ACTION).unwrap();
        assert_eq!(accepted.source_id, Some(log.id));
        assert_eq!(log.actor_person_id, Some(actor.id));
        assert_eq!(log.subject_id, target.id);
        assert_eq!(log.subject_type, "project");
        assert_eq!(log.detail, format!("entity:{} adverse_to entity:{}", target.entity_id, other.entity_id));
        let after_graph = graph_rows(&pg, target.entity_id, other.entity_id).await;
        assert_eq!(after_graph.len(), before_graph.len() + 1);
        let properties: Vec<serde_json::Value> = sqlx::query_scalar(
            "SELECT properties::text::jsonb FROM navigator_graph.\"RELATIONSHIP\" ORDER BY id",
        ).fetch_all(pg.pool()).await.unwrap();
        for source in [log.id, previous.source_id.unwrap()] {
            assert!(properties.iter().any(|edge| edge["source_id"] == source.to_string()));
        }
    }).await;
}

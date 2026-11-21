//! Canonical initializer acceptance: fresh writes, bounded projection, no AGE DML.
use super::{Matter, PostgresDb, SurrealDb};
use sqlx::AssertSqlSafe;
use uuid::Uuid;

#[path = "../../../../store/tests/support/person_merge_graph.rs"]
mod merge_fixture;

async fn rejected(app: &PostgresDb, sql: &str, code: &str) {
    let error = sqlx::query(AssertSqlSafe(sql))
        .execute(app.pool())
        .await
        .unwrap_err();
    assert_eq!(
        error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some(code)
    );
}

async fn check_function_owner(app: &PostgresDb) {
    let owners: Vec<(String, String, bool, Vec<String>)> = sqlx::query_as(
        "SELECT p.proname::text,pg_get_userbyid(p.proowner)::text,p.prosecdef,p.proconfig FROM pg_proc p WHERE p.pronamespace='public'::regnamespace AND p.proname IN ('graph_ensure_vertices','graph_load_edges','graph_merge_person') ORDER BY p.proname")
        .fetch_all(app.pool()).await.unwrap();
    assert_eq!(owners.len(), 3);
    for (_, owner, definer, config) in owners {
        assert_eq!(owner, "navigator_owner");
        assert!(definer);
        let path = config
            .iter()
            .find_map(|line| line.strip_prefix("search_path="))
            .unwrap();
        assert_eq!(
            path.split(',').map(str::trim).collect::<Vec<_>>(),
            ["pg_catalog", "public", "ag_catalog", "pg_temp"]
        );
    }
}

async fn fresh_vertices_and_edge(app: &PostgresDb, db: &SurrealDb) -> (Uuid, Uuid) {
    let first = store::persons::create(
        db,
        &store::persons::NewPerson::new(
            "Synthetic new app graph first",
            format!("graph-{}@example.test", Uuid::new_v4()),
        ),
    )
    .await
    .unwrap();
    let second = store::persons::create(
        db,
        &store::persons::NewPerson::new(
            "Synthetic new app graph second",
            format!("graph-{}@example.test", Uuid::new_v4()),
        ),
    )
    .await
    .unwrap();
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM navigator_graph.\"Person\" WHERE properties::text::jsonb->>'id'=ANY($1::text[])")
        .bind(vec![first.id.to_string(), second.id.to_string()]).fetch_one(app.pool()).await.unwrap();
    assert_eq!(
        before, 0,
        "new relational people must not be preseeded AGE vertices"
    );
    store::relationships::record(
        db,
        &store::relationships::NewRelationship {
            from: store::relationships::Endpoint::Person,
            from_id: first.id,
            to: store::relationships::Endpoint::Person,
            to_id: second.id,
            kind: store::relationships::KIND_ADVERSE_TO.into(),
            confidence_pct: 100,
            source_kind: store::relationships::SOURCE_MANUAL.into(),
            source_id: None,
            detail: Some("Synthetic current app role effect".into()),
        },
    )
    .await
    .unwrap();
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM navigator_graph.\"Person\" WHERE properties::text::jsonb->>'id'=ANY($1::text[])")
        .bind(vec![first.id.to_string(), second.id.to_string()]).fetch_one(app.pool()).await.unwrap();
    assert_eq!(
        after, 2,
        "the application writer must mint both actual new AGE vertices"
    );
    let edges: Vec<String> =
        sqlx::query_scalar("SELECT t.v::text FROM graph_conflict_edges($1) AS t(v)")
            .bind(serde_json::json!([first.id.to_string()]).to_string())
            .fetch_all(app.pool())
            .await
            .unwrap();
    assert!(edges
        .iter()
        .any(|edge| edge.contains(&first.id.to_string()) && edge.contains(&second.id.to_string())));
    (first.id, second.id)
}

async fn typed_uuid_collision_keeps_endpoint_labels(
    app: &PostgresDb,
    db: &SurrealDb,
    first: Uuid,
    second: Uuid,
    model_entity: Uuid,
) {
    merge_fixture::typed_endpoint_collision(db, app, model_entity, first, second).await;
}

async fn mutation_boundaries(app: &PostgresDb, first: Uuid, second: Uuid) {
    let before = merge_fixture::snapshot(app).await;
    for table in [
        "Person",
        "Entity",
        "Project",
        "Firm",
        "RELATIONSHIP",
        "HAS_ROLE",
        "WORKS_ON",
        "MEMBER_OF",
        "HANDLES",
    ] {
        for statement in [format!("INSERT INTO navigator_graph.\"{table}\" SELECT * FROM navigator_graph.\"{table}\" WHERE false"),
            format!("UPDATE navigator_graph.\"{table}\" SET properties=properties WHERE false"),
            format!("DELETE FROM navigator_graph.\"{table}\" WHERE false"),
            format!("TRUNCATE TABLE navigator_graph.\"{table}\"")] {
            rejected(app, &statement, "42501").await;
        }
    }
    for statement in [
        "SELECT * FROM graph_load_vertices('person','{\"rows\":[] }')",
        "SELECT * FROM graph_clear_navigator()",
        "SELECT * FROM graph_load_edges_raw('relationship','{\"rows\":[] }')",
        "SELECT graph_ensure_vertices_raw('person','{\"rows\":[] }')",
        "SELECT graph_insert_canonical_edge('relationship','{}'::jsonb)",
        "SELECT graph_require_unfiltered_sources()",
        "SELECT graph_canonical_vertex('person','{}'::jsonb)",
        "SELECT graph_canonical_edge('relationship','{}'::jsonb)",
    ] {
        rejected(app, statement, "42501").await;
    }
    let forged = serde_json::json!({"rows":[{"from_id":first,"to_id":second,"from_kind":"person","to_kind":"person","kind":"adverse_to",
        "confidence_pct":17,"source_kind":"manual","source_id":null,"detail":"forged, absent from the authoritative row"}]}).to_string();
    let error = sqlx::query("SELECT * FROM graph_load_edges('relationship',$1)")
        .bind(forged)
        .execute(app.pool())
        .await
        .unwrap_err();
    assert_eq!(
        error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("23514")
    );
    let invented =
        serde_json::json!({"rows":[{"id":Uuid::new_v4(),"name":"forged absent vertex"}]})
            .to_string();
    let error = sqlx::query("SELECT graph_ensure_vertices('person',$1)")
        .bind(invented)
        .execute(app.pool())
        .await
        .unwrap_err();
    assert_eq!(
        error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("23503")
    );
    let error = sqlx::query("SELECT graph_merge_person($1,$2)")
        .bind(first)
        .bind(second)
        .execute(app.pool())
        .await
        .unwrap_err();
    assert_eq!(
        error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("23514")
    );
    assert_eq!(merge_fixture::snapshot(app).await, before);
}

async fn rls_does_not_become_an_owner_bypass(
    app: &PostgresDb,
    owner: &PostgresDb,
    first: Uuid,
    second: Uuid,
) {
    let enabled: bool = sqlx::query_scalar("SELECT relrowsecurity OR relforcerowsecurity FROM pg_class WHERE oid='public.relationship'::regclass")
        .fetch_one(owner.pool()).await.unwrap();
    assert!(
        !enabled,
        "the private tier begins without a relationship row policy"
    );
    let before = merge_fixture::snapshot(app).await;
    sqlx::query("ALTER TABLE public.relationship ENABLE ROW LEVEL SECURITY")
        .execute(owner.pool())
        .await
        .unwrap();
    let payload = serde_json::json!({"rows":[{"from_id":first,"to_id":second,"from_kind":"person","to_kind":"person","kind":"adverse_to",
        "confidence_pct":100,"source_kind":"manual","source_id":null,"detail":"Synthetic current app role effect"}]}).to_string();
    let attempted = sqlx::query("SELECT * FROM graph_load_edges('relationship',$1)")
        .bind(payload)
        .execute(app.pool())
        .await;
    let restored = sqlx::query("ALTER TABLE public.relationship DISABLE ROW LEVEL SECURITY")
        .execute(owner.pool())
        .await;
    restored.unwrap();
    let error = attempted.unwrap_err();
    assert_eq!(
        error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("42501")
    );
    assert_eq!(merge_fixture::snapshot(app).await, before);
}

pub(super) async fn runtime_graph_and_merge(app: &PostgresDb, owner: &PostgresDb, first: &Matter) {
    check_function_owner(app).await;
    PostgresDb::with_test_db(app.clone(), async {
        let db = SurrealDb::uninitialized();
        let (one, two) = fresh_vertices_and_edge(app, &db).await;
        typed_uuid_collision_keeps_endpoint_labels(app, &db, one, two, first.entity).await;
        mutation_boundaries(app, one, two).await;
        rls_does_not_become_an_owner_bypass(app, owner, one, two).await;
        let parent = store::projects::find_by_id(&db, first.project)
            .await
            .unwrap()
            .unwrap();
        let scenario = merge_fixture::Scenario::seed(&db, app, &parent).await;
        store::person_merge::merge(&db, scenario.kept, scenario.dropped)
            .await
            .unwrap();
        scenario.assert_merged(&db, app).await;
    })
    .await;
}

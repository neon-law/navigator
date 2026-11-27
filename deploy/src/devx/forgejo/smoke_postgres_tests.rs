use store::postgres::PostgresDb;

#[tokio::test]
async fn native_forgejo_fixture_derives_real_owned_repository_and_routes_spawned_reads() {
    store::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let source = store::application_store::ApplicationStore::uninitialized();
        let seeded = super::seed_firm_project(&source).await.unwrap();
        assert_eq!(seeded.path.to_string(), "example-law-pc/sample-litigation");
        let linked: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM project p JOIN brand b ON b.brand_key=p.brand AND b.firm_id=p.firm_id JOIN firm f ON f.id=p.firm_id JOIN person_firm_role m ON m.firm_id=f.id AND m.membership='admin' AND m.is_dri JOIN person a ON a.id=m.person_id JOIN entity e ON e.id=p.entity_id JOIN entity_type t ON t.id=e.entity_type_id JOIN jurisdiction j ON j.id=e.jurisdiction_id WHERE p.id=$1 AND a.role='admin')")
            .bind(seeded.project_id).fetch_one(pg.pool()).await.unwrap();
        assert!(linked);
        let native = super::receiver_journal().await.unwrap();
        // A task does not inherit a task-local fixture; the serving host gives
        // each real spawned reader the same admitted native pool explicitly.
        let found = tokio::spawn(async move {
            store::postgres::projects::find_by_code_on(&native, "sample-litigation").await.unwrap().unwrap()
        }).await.unwrap();
        assert_eq!(found.id, seeded.project_id);
    }).await;
}

#[tokio::test]
async fn receiver_refuses_an_unprepared_database_without_installing_schema_or_receipts() {
    store::test_support::with_postgres(async {
        let admin = PostgresDb::application().await.unwrap();
        let database = format!("unprepared_smoke_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
            .execute(admin.pool())
            .await
            .unwrap();
        let options = (*admin.pool().connect_options())
            .clone()
            .database(&database);
        let target = PostgresDb::connect(options).await.unwrap();
        let before: i64 =
            sqlx::query_scalar("SELECT count(*) FROM pg_tables WHERE schemaname='public'")
                .fetch_one(target.pool())
                .await
                .unwrap();
        let refused = PostgresDb::with_test_db(target.clone(), super::receiver_journal())
            .await
            .is_err();
        let tables: i64 =
            sqlx::query_scalar("SELECT count(*) FROM pg_tables WHERE schemaname='public'")
                .fetch_one(target.pool())
                .await
                .unwrap();
        target.pool().close().await;
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP DATABASE {database}")))
            .execute(admin.pool())
            .await
            .unwrap();
        assert!(refused);
        assert_eq!(tables, before);
    })
    .await;
}

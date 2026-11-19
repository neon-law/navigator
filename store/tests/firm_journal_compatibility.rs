use store::firm_journal::*;
use store::postgres::PostgresDb;
use uuid::Uuid;
#[path = "support/firm_journal.rs"]
mod journal_fixture;
use journal_fixture::entry;

async fn postgres() -> Option<PostgresDb> {
    let port = if let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") {
        port.parse::<u16>().unwrap()
    } else {
        assert!(
            std::env::var("NAV_REQUIRE_POSTGRES").as_deref() != Ok("1"),
            "PostgreSQL integration lane requires NAVIGATOR_TEST_POSTGRES_PORT"
        );
        return None;
    };
    let options = sqlx::postgres::PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port)
        .username("postgres")
        .password("synthetic")
        .database(
            &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").unwrap_or_else(|_| "smoke".into()),
        )
        .ssl_mode(sqlx::postgres::PgSslMode::Disable);
    let admin = sqlx::PgPool::connect_with(options.clone()).await.unwrap();
    let database = format!("journal_test_{}", Uuid::now_v7().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&admin)
        .await
        .unwrap();
    let pg = PostgresDb::connect(options.database(&database))
        .await
        .unwrap();
    pg.migrate_embedded(None).await.unwrap();
    Some(pg)
}

// Historical Firm rows deliberately remain distinct from the target's parents.
async fn source_firm(db: &store::surreal::SurrealDb) -> (Uuid, String) {
    let id = Uuid::now_v7();
    let slug = format!("firm-{}", id.simple());
    db.query("CREATE $id SET name='Synthetic accounting firm', slug=$slug, status='active', inserted_at=$at, updated_at=$at")
        .bind(("id", store::surreal::record_id("firm", id)))
        .bind(("slug", slug.clone()))
        .bind(("at", "2026-10-11T00:00:00Z"))
        .await.unwrap().check().unwrap();
    (id, slug)
}

#[tokio::test]
async fn journal_firm_parent_rejects_another_identity_with_the_same_slug() {
    store::test_support::with_source_import(async {
        let Some(pg) = postgres().await else {
            return;
        };
        let db = store::test_support::mem_surreal().await;
        let (firm_id, source_slug) = source_firm(&db).await;
        let other_id = Uuid::new_v4();
        sqlx::query(
        "INSERT INTO firm (id,name,slug,status) VALUES ($1,'Synthetic existing Firm',$2,'active')",
    )
    .bind(other_id)
    .bind(&source_slug)
    .execute(pg.pool())
    .await
    .unwrap();
        assert!(matches!(
            post(&db, &pg, &entry(firm_id, "synthetic:slug-conflict")).await,
            Err(JournalError::Invalid(_))
        ));
        let held: Uuid = sqlx::query_scalar("SELECT id FROM firm WHERE slug=$1")
            .bind(&source_slug)
            .fetch_one(pg.pool())
            .await
            .unwrap();
        assert_eq!(held, other_id);
        assert!(entries(&pg, firm_id).await.unwrap().is_empty());
    })
    .await;
}

#[tokio::test]
async fn journal_firm_parent_rejects_an_immutable_slug_mismatch() {
    store::test_support::with_source_import(async {
    let Some(pg) = postgres().await else {
        return;
    };
    let db = store::test_support::mem_surreal().await;
    let (firm_id, _) = source_firm(&db).await;
    sqlx::query("INSERT INTO firm (id,name,slug,status) VALUES ($1,'Synthetic existing Firm','synthetic-other-slug','active')")
        .bind(firm_id).execute(pg.pool()).await.unwrap();
    assert!(matches!(
        post(&db, &pg, &entry(firm_id, "synthetic:identity-conflict")).await,
        Err(JournalError::Invalid(_))
    ));
    let held: String = sqlx::query_scalar("SELECT slug FROM firm WHERE id=$1")
        .bind(firm_id)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(held, "synthetic-other-slug");
    assert!(entries(&pg, firm_id).await.unwrap().is_empty());

    }).await;
}

#[tokio::test]
async fn journal_firm_parent_preserves_existing_nonidentity_fields() {
    store::test_support::with_source_import(async {
    let Some(pg) = postgres().await else {
        return;
    };
    let db = store::test_support::mem_surreal().await;
    let (firm_id, source_slug) = source_firm(&db).await;
    let retained_at: chrono::DateTime<chrono::Utc> = "2025-01-02T03:04:05Z".parse().unwrap();
    sqlx::query("INSERT INTO firm (id,name,slug,status,inserted_at,updated_at) VALUES ($1,'Synthetic retained Firm',$2,'inactive',$3,$3)")
        .bind(firm_id).bind(&source_slug).bind(retained_at).execute(pg.pool()).await.unwrap();
    post(&db, &pg, &entry(firm_id, "synthetic:retained-parent"))
        .await
        .unwrap();
    let held: (
        String,
        String,
        chrono::DateTime<chrono::Utc>,
        chrono::DateTime<chrono::Utc>,
    ) = sqlx::query_as("SELECT name,status,inserted_at,updated_at FROM firm WHERE id=$1")
        .bind(firm_id)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(
        held,
        (
            "Synthetic retained Firm".into(),
            "inactive".into(),
            retained_at,
            retained_at
        )
    );

    }).await;
}

#[tokio::test]
async fn surreal_journal_store_posts_reads_and_feeds_the_postgres_import() {
    store::test_support::with_source_import(async {
        let db = store::test_support::mem_surreal().await;
        let (firm, _) = source_firm(&db).await;
        let journal = JournalStore::Surreal(db.clone());
        assert_eq!(journal.backend(), "surrealdb");
        let e = entry(firm, "manual:surreal");
        assert_eq!(journal.preview(&db, &e).await.unwrap(), e.id);
        assert!(journal.entries(firm).await.unwrap().is_empty());
        let (a, b) = tokio::join!(journal.post(&db, &e), journal.post(&db, &e));
        assert_eq!(a.unwrap(), e.id);
        assert_eq!(b.unwrap(), e.id);
        assert_eq!(journal.entries(firm).await.unwrap(), vec![e.clone()]);
        let mut changed = e.clone();
        changed.memo = "Different economic event".into();
        assert!(matches!(
            journal.post(&db, &changed).await,
            Err(JournalError::Conflict)
        ));
        let reversal = e
            .reversal(
                "manual:surreal:reversal",
                e.actor_id,
                "2026-10-12T00:00:00Z",
            )
            .unwrap();
        journal.post(&db, &reversal).await.unwrap();
        let again = e
            .reversal("manual:surreal:again", e.actor_id, "2026-10-12T00:00:00Z")
            .unwrap();
        assert!(journal.post(&db, &again).await.is_err());
        assert_eq!(journal.entries(firm).await.unwrap(), vec![e, reversal]);
        // Rows carry an insertion timestamp, so the PostgreSQL startup import
        // takes them without triage.
        assert!(store::journal_startup_migration::triage(&db, firm)
            .await
            .unwrap()
            .is_empty());
        assert!(matches!(
            journal.postgres(),
            Err(JournalError::RequiresPostgres)
        ));
    })
    .await;
}

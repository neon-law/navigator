use surrealdb::types::SurrealValue;
#[derive(SurrealValue)]
struct Count {
    total: i64,
}
use store::firm_journal::*;
use store::postgres::PostgresDb;
use uuid::Uuid;
async fn postgres() -> Option<(PostgresDb, sqlx::PgPool)> {
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
    let setup = sqlx::PgPool::connect_with(options.clone().database(&database))
        .await
        .unwrap();
    let pg = PostgresDb::connect(options.database(&database))
        .await
        .unwrap();
    pg.migrate_embedded(None).await.unwrap();
    Some((pg, setup))
}
async fn firm(db: &store::surreal::SurrealDb) -> Uuid {
    let admin = store::persons::create(
        db,
        &store::persons::NewPerson::with_role(
            "Synthetic administrator",
            format!("admin-{}@example.com", Uuid::now_v7()),
            store::persons::Role::Admin,
        ),
    )
    .await
    .unwrap();
    store::firms::create(
        db,
        &store::firms::NewFirm {
            name: "Synthetic accounting firm".into(),
            status: "active".into(),
            entity_id: store::test_support::seed_entity(db).await,
            admin_dri_person_id: admin.id,
        },
    )
    .await
    .unwrap()
    .id
}

fn entry(firm: Uuid, key: &str) -> JournalEntry {
    JournalEntry {
        id: Uuid::now_v7(),
        firm_id: firm,
        key: key.into(),
        actor_id: Uuid::now_v7(),
        occurred_at: "2026-10-11T00:00:00Z".into(),
        memo: "Synthetic receipt".into(),
        postings: vec![
            Posting {
                account: "bank".into(),
                ownership: store::firm_journal::BeneficialOwnership::Firm,
                restrictions: vec![],
                unit: Unit::fiat("USD", 2),
                quantity: "10000".into(),
                project_id: None,
                category: Category::Cash,
            },
            Posting {
                account: "client-liability".into(),
                ownership: store::firm_journal::BeneficialOwnership::Client,
                restrictions: vec![],
                unit: Unit::fiat("USD", 2),
                quantity: "-10000".into(),
                project_id: None,
                category: Category::Trust,
            },
        ],
        sources: vec![],
        receipts: vec![],
        links: vec![],
        reversal_of: None,
    }
}

async fn legacy(db: &store::surreal::SurrealDb, entry: &JournalEntry, timestamp: Option<&str>) {
    // Historical definitions varied; the importer must tolerate absent metadata.
    db.query("DEFINE FIELD OVERWRITE inserted_at ON journal_entry TYPE option<string | datetime>")
        .await
        .unwrap()
        .check()
        .unwrap();
    db.query("CREATE journal_entry CONTENT {firm_id:$firm, command_key:$key, payload:$payload, inserted_at:$timestamp}")
        .bind(("firm", entry.firm_id.to_string())).bind(("key", entry.key.clone()))
        .bind(("payload", serde_json::to_string(entry).unwrap()))
        .bind(("timestamp", timestamp.map(str::to_owned)))
        .await.unwrap().check().unwrap();
}

#[tokio::test]
async fn startup_import_preserves_identity_timestamps_and_reversal_order_then_retries() {
    let Some((pg, pool)) = postgres().await else {
        return;
    };
    let db = store::test_support::mem_surreal().await;
    let firm = firm(&db).await;
    let mut original = entry(firm, "historical");
    original.sources.push(SourceReference {
        provider: "synthetic-bank".into(),
        scope: "synthetic-firm".into(),
        account: "operating".into(),
        object_type: "transaction".into(),
        external_id: "synthetic-payment".into(),
        locator: None,
    });
    let reversal = original
        .reversal(
            "historical-reversal",
            Uuid::now_v7(),
            "2026-10-11T01:00:00Z",
        )
        .unwrap();
    // Insert reversal first to prove the source's arbitrary order is irrelevant.
    legacy(&db, &reversal, Some("2026-10-11T01:05:00Z")).await;
    legacy(&db, &original, Some("2026-10-10T01:02:03.123456789Z")).await;
    let (left, right) = tokio::join!(
        store::journal_startup_migration::import(&db, &pg),
        store::journal_startup_migration::import(&db, &pg),
    );
    assert_eq!(left.unwrap().imported + right.unwrap().imported, 2);
    assert_eq!(
        store::journal_startup_migration::import(&db, &pg)
            .await
            .unwrap()
            .imported,
        0
    );
    let imported = entries(&pg, firm).await.unwrap();
    assert_eq!(imported.len(), 2);
    assert!(imported
        .iter()
        .any(|e| serde_json::to_value(e).unwrap() == serde_json::to_value(&original).unwrap()));
    assert!(imported
        .iter()
        .any(|e| serde_json::to_value(e).unwrap() == serde_json::to_value(&reversal).unwrap()));
    let (inserted_at, known, imported, source): (chrono::DateTime<chrono::Utc>, bool, bool, String) = sqlx::query_as(
        "SELECT inserted_at,legacy_inserted_at_known,legacy_imported_at IS NOT NULL,legacy_inserted_at_source FROM journal_entry WHERE id=$1")
        .bind(original.id).fetch_one(&pool).await.unwrap();
    assert_eq!(inserted_at.to_rfc3339(), "2026-10-10T01:02:03.123457+00:00");
    assert_eq!(source, "2026-10-10T01:02:03.123456789Z");
    assert!(known && imported);
    let known: bool =
        sqlx::query_scalar("SELECT legacy_inserted_at_known FROM journal_entry WHERE id=$1")
            .bind(reversal.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(known);
    let sources: i64 =
        sqlx::query_scalar("SELECT count(*) FROM journal_entry_source WHERE entry_id=$1")
            .bind(original.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(sources, 1);
    let postings: i64 =
        sqlx::query_scalar("SELECT count(*) FROM journal_posting WHERE entry_id=$1")
            .bind(original.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(postings, 2);
    let mut source = db
        .query("SELECT count() AS total FROM journal_entry GROUP ALL")
        .await
        .unwrap();
    let count: Vec<Count> = source.take(0).unwrap();
    assert_eq!(count[0].total, 2);
}

#[tokio::test]
async fn startup_import_refuses_changed_postgres_record_and_rolls_back_entire_batch() {
    let Some((pg, _pool)) = postgres().await else {
        return;
    };
    let db = store::test_support::mem_surreal().await;
    let firm = firm(&db).await;
    let original = entry(firm, "collision");
    post(&db, &pg, &original).await.unwrap();
    let mut altered = original.clone();
    altered.memo = "Altered historical source".into();
    legacy(&db, &altered, Some("2026-10-10T00:00:00Z")).await;
    legacy(
        &db,
        &entry(firm, "must-roll-back"),
        Some("2026-10-10T00:00:00Z"),
    )
    .await;
    assert!(matches!(
        store::journal_startup_migration::import(&db, &pg).await,
        Err(JournalError::Conflict)
    ));
    assert_eq!(entries(&pg, firm).await.unwrap().len(), 1);
}

#[tokio::test]
async fn absent_insertion_timestamp_and_dependent_reversal_are_triaged_without_import() {
    use store::journal_startup_migration::{import, triage, TriageReason};
    let Some((pg, pool)) = postgres().await else {
        return;
    };
    let db = store::test_support::mem_surreal().await;
    let firm = firm(&db).await;
    let unknown = entry(firm, "no-insertion-timestamp");
    let dependent = unknown
        .reversal("dependent", Uuid::now_v7(), "2026-10-11T01:00:00Z")
        .unwrap();
    let valid = entry(firm, "known-insertion-timestamp");
    let invalid = entry(firm, "invalid-insertion-timestamp");
    legacy(&db, &invalid, Some("not-a-date")).await;
    legacy(&db, &unknown, None).await;
    legacy(&db, &dependent, Some("2026-10-11T01:05:00Z")).await;
    legacy(&db, &valid, Some("2026-10-10T01:00:00Z")).await;
    let read_only = triage(&db, firm).await.unwrap();
    assert_eq!(read_only.len(), 3);
    assert!(read_only
        .iter()
        .any(|row| row.entry_id == invalid.id
            && row.reason == TriageReason::InvalidInsertionTimestamp));
    let issue = read_only
        .iter()
        .find(|row| row.entry_id == unknown.id)
        .unwrap();
    assert_eq!(issue.firm_id, firm);
    assert_eq!(issue.reason, TriageReason::MissingInsertionTimestamp);
    assert_eq!(issue.occurred_at, unknown.occurred_at);
    assert_eq!(issue.inserted_at, None);
    let safe_json = serde_json::to_string(&read_only).unwrap();
    assert!(!safe_json.contains("Synthetic receipt"));
    assert!(!safe_json.contains("client-liability"));
    assert!(!safe_json.contains("not-a-date"));
    let report = import(&db, &pg).await.unwrap();
    assert_eq!(report.imported, 1);
    assert_eq!(report.triage, read_only);
    assert_eq!(entries(&pg, firm).await.unwrap()[0].id, valid.id);
    let unknown_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM journal_entry WHERE id=$1 OR id=$2")
            .bind(unknown.id)
            .bind(dependent.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(unknown_count, 0);
    assert_eq!(import(&db, &pg).await.unwrap().imported, 0);
}

#[tokio::test]
async fn missing_reversal_parent_is_triaged_while_independent_entry_migrates() {
    use store::journal_startup_migration::{import, TriageReason};
    let Some((pg, _)) = postgres().await else {
        return;
    };
    let db = store::test_support::mem_surreal().await;
    let firm = firm(&db).await;
    let missing_parent = entry(firm, "not-in-source");
    let reversal = missing_parent
        .reversal("orphan", Uuid::now_v7(), "2026-10-11T01:00:00Z")
        .unwrap();
    let valid = entry(firm, "independent");
    legacy(&db, &reversal, Some("2026-10-11T01:05:00Z")).await;
    legacy(&db, &valid, Some("2026-10-10T01:00:00Z")).await;
    let read_only = store::journal_startup_migration::triage(&db, firm)
        .await
        .unwrap();
    let report = import(&db, &pg).await.unwrap();
    assert_eq!(report.imported, 1);
    assert_eq!(report.triage.len(), 1);
    assert_eq!(report.triage, read_only);
    assert_eq!(report.triage[0].entry_id, reversal.id);
    assert_eq!(
        report.triage[0].reason,
        TriageReason::UnresolvedReversalTarget
    );
    assert_eq!(entries(&pg, firm).await.unwrap().len(), 1);
}

#[tokio::test]
async fn source_overflow_fails_visibly_without_importing_a_partial_subset() {
    use surrealdb::types::SurrealValue;
    #[derive(SurrealValue)]
    struct Row {
        firm_id: String,
        command_key: String,
        payload: String,
        inserted_at: String,
    }
    let Some((pg, _)) = postgres().await else {
        return;
    };
    let db = store::test_support::mem_surreal().await;
    let firm = firm(&db).await;
    let rows: Vec<Row> = (0..1001)
        .map(|index| {
            let entry = entry(firm, &format!("bounded-source-{index}"));
            Row {
                firm_id: firm.to_string(),
                command_key: entry.key.clone(),
                payload: serde_json::to_string(&entry).unwrap(),
                inserted_at: "2026-10-10T00:00:00Z".into(),
            }
        })
        .collect();
    db.query("FOR $row IN $rows { CREATE journal_entry CONTENT $row; }")
        .bind(("rows", rows))
        .await
        .unwrap()
        .check()
        .unwrap();
    assert!(
        matches!(store::journal_startup_migration::triage(&db, firm).await,
        Err(JournalError::Invalid(message)) if message.contains("1000"))
    );
    assert!(
        matches!(store::journal_startup_migration::import(&db, &pg).await,
        Err(JournalError::Invalid(message)) if message.contains("1000"))
    );
    assert!(entries(&pg, firm).await.unwrap().is_empty());
}

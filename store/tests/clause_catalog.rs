//! The shared Clause library's PostgreSQL read model and its idempotent seed.
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use sqlx::PgPool;
use store::clauses;
use store::postgres::PostgresDb;
use uuid::Uuid;

async fn database() -> Option<(PostgresDb, PgPool)> {
    let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
        assert_ne!(
            std::env::var("NAV_REQUIRE_POSTGRES").as_deref(),
            Ok("1"),
            "PostgreSQL integration lane requires NAVIGATOR_TEST_POSTGRES_PORT"
        );
        return None;
    };
    let options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port.parse().unwrap())
        .username("postgres")
        .password("synthetic")
        .database(
            &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").unwrap_or_else(|_| "smoke".into()),
        )
        .ssl_mode(PgSslMode::Disable);
    let admin = PgPool::connect_with(options.clone()).await.unwrap();
    let name = format!("clause_test_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    let pg = PostgresDb::connect(options.clone().database(&name))
        .await
        .unwrap();
    pg.migrate_embedded(None).await.unwrap();
    let pool = PgPool::connect_with(options.database(&name)).await.unwrap();
    Some((pg, pool))
}

fn sha256(body: &str) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    Sha256::digest(body.as_bytes())
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

#[tokio::test]
async fn a_seeded_library_has_one_row_per_version_and_reseeding_writes_nothing() {
    let Some((pg, pool)) = database().await else {
        return;
    };
    let library = rules::clauses::library_clauses().unwrap();
    let first = clauses::seed(&pg, &library).await.unwrap();
    assert_eq!(first.inserted, library.len());

    let rows = clauses::list(&pg).await.unwrap();
    assert_eq!(rows.len(), library.len(), "one row per library entry");
    for (row, source) in rows.iter().zip(&library) {
        assert_eq!((&row.code, row.version), (&source.code, source.version));
        assert_eq!(row.kind, source.kind);
        assert_eq!(row.body, source.body);
        assert_eq!(row.body_sha256, sha256(&source.body));
        assert_eq!(row.inputs, serde_json::to_value(&source.inputs).unwrap());
        let newest = library
            .iter()
            .filter(|other| other.code == row.code)
            .map(|other| other.version)
            .max()
            .unwrap();
        assert_eq!(row.is_current, row.version == newest, "{}", row.code);
        assert!(row.retired_at.is_none());
    }

    let before: Vec<(String, i32, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT xmin::text, version, inserted_at FROM clause ORDER BY code, version",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let again = clauses::seed(&pg, &library).await.unwrap();
    assert!(again.is_unchanged(), "{again:?}");
    let after: Vec<(String, i32, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT xmin::text, version, inserted_at FROM clause ORDER BY code, version",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(before, after, "an unchanged library writes nothing");

    let governing = clauses::get(&pg, "governing_law", 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(governing.kind, "agreement");
    assert!(clauses::get(&pg, "governing_law", 99)
        .await
        .unwrap()
        .is_none());
    let current = clauses::current(&pg, "nevada_engagement_arbitration")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.version, 2);
    assert!(clauses::current(&pg, "absent").await.unwrap().is_none());
}

#[tokio::test]
async fn appending_a_version_moves_current_and_a_changed_pin_is_refused() {
    let Some((pg, _pool)) = database().await else {
        return;
    };
    let mut library = rules::clauses::library_clauses().unwrap();
    clauses::seed(&pg, &library).await.unwrap();

    let mut next = library
        .iter()
        .find(|clause| clause.code == "governing_law")
        .unwrap()
        .clone();
    next.version = 2;
    next.body = "Governing law: {{jurisdiction}}. Exclusive courts: {{forum}}.".into();
    library.push(next);
    let appended = clauses::seed(&pg, &library).await.unwrap();
    assert_eq!(appended.inserted, 1);
    assert_eq!(appended.current_changed, 1);
    assert_eq!(
        clauses::current(&pg, "governing_law")
            .await
            .unwrap()
            .unwrap()
            .version,
        2
    );
    assert!(
        !clauses::get(&pg, "governing_law", 1)
            .await
            .unwrap()
            .unwrap()
            .is_current
    );

    let mut edited = library.clone();
    edited
        .iter_mut()
        .find(|clause| clause.code == "governing_law" && clause.version == 1)
        .unwrap()
        .body = "Rewritten in place.".into();
    assert!(
        matches!(
            clauses::seed(&pg, &edited).await,
            Err(clauses::ClauseError::ChangedVersion { .. })
        ),
        "a pinned version is never rewritten"
    );
    assert_eq!(
        clauses::get(&pg, "governing_law", 1)
            .await
            .unwrap()
            .unwrap()
            .body,
        library[library
            .iter()
            .position(|clause| clause.code == "governing_law" && clause.version == 1)
            .unwrap()]
        .body
    );
}

#[tokio::test]
async fn the_database_refuses_a_second_current_version_and_a_false_digest() {
    let Some((pg, pool)) = database().await else {
        return;
    };
    clauses::seed(&pg, &rules::clauses::library_clauses().unwrap())
        .await
        .unwrap();
    let insert = "INSERT INTO clause (code, version, kind, body, body_sha256, is_current) VALUES ($1, $2, 'agreement', $3, $4, $5)";
    let body = "Synthetic clause body.";
    let second_current = sqlx::query(insert)
        .bind("governing_law")
        .bind(7)
        .bind(body)
        .bind(sha256(body))
        .bind(true)
        .execute(&pool)
        .await;
    assert!(second_current.is_err(), "one current version per code");
    let false_digest = sqlx::query(insert)
        .bind("synthetic_clause")
        .bind(1)
        .bind(body)
        .bind(sha256("another body"))
        .bind(false)
        .execute(&pool)
        .await;
    assert!(false_digest.is_err(), "body_sha256 must match the body");
    let duplicate_version = sqlx::query(insert)
        .bind("governing_law")
        .bind(1)
        .bind(body)
        .bind(sha256(body))
        .bind(false)
        .execute(&pool)
        .await;
    assert!(duplicate_version.is_err(), "UNIQUE (code, version)");
}

#[tokio::test]
async fn the_seed_skips_without_a_configured_postgres_host() {
    if std::env::var_os("NAVIGATOR_POSTGRES_HOST").is_some() {
        return;
    }
    assert!(clauses::seed_library_from_env().await.unwrap().is_none());
}

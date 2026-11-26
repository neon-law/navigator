//! The signature cutover retains imported rows while enforcing new parent links.

use sqlx::Connection;
use uuid::Uuid;

#[tokio::test]
async fn signature_parent_constraint_retains_history_and_rejects_new_orphans() {
    let (options, database) = crate::postgres_mirror::tests::test_postgres();
    let mut connection = sqlx::PgConnection::connect_with(&options.database(&database))
        .await
        .unwrap();
    // Temporary tables shadow this connection's canonical names, so the real
    // migration runs without changing another test's graph or migration ledger.
    sqlx::raw_sql(
        "CREATE TEMP TABLE notation(id UUID PRIMARY KEY);
         CREATE TEMP TABLE signature(id UUID PRIMARY KEY, notation_id UUID NOT NULL,
             provider TEXT NOT NULL, provider_id TEXT NOT NULL);",
    )
    .execute(&mut connection)
    .await
    .unwrap();
    let orphan = Uuid::new_v4();
    sqlx::query("INSERT INTO signature VALUES ($1,$2,'docusign','historical')")
        .bind(Uuid::new_v4())
        .bind(orphan)
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!(
        "../../postgres/migrations/20261117160000_signature_notation_reference.sql"
    ))
    .execute(&mut connection)
    .await
    .unwrap();
    let retained: i64 = sqlx::query_scalar("SELECT count(*) FROM signature WHERE notation_id=$1")
        .bind(orphan)
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(retained, 1);

    let rejected = sqlx::query("INSERT INTO signature VALUES ($1,$2,'docusign','new-orphan')")
        .bind(Uuid::new_v4())
        .bind(orphan)
        .execute(&mut connection)
        .await
        .unwrap_err();
    assert_eq!(
        rejected
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("23503")
    );
    let parent = Uuid::new_v4();
    sqlx::query("INSERT INTO notation VALUES ($1)")
        .bind(parent)
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("INSERT INTO signature VALUES ($1,$2,'docusign','new-valid')")
        .bind(Uuid::new_v4())
        .bind(parent)
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
}

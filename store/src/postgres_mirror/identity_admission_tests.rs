//! Persisted identity ownership must not depend on an available import source.
use super::*;
use crate::postgres_mirror::tests::{connect, fresh_schema};

#[tokio::test]
async fn active_identity_global_parity_and_replays_require_no_source() {
    let (options, database) = fresh_schema().await.expect("required PostgreSQL fixture");
    let pg = connect(&options, &database).await;
    let person = Uuid::new_v4();
    sqlx::query("INSERT INTO person(id,name,email) VALUES($1,'Native Person','native-identity@example.com')")
        .bind(person).execute(pg.pool()).await.unwrap();
    for mailbox in ["native-identity@example.com", "native-alias@example.com"] {
        sqlx::query("INSERT INTO person_mailbox(id,person_id) VALUES($1,$2)")
            .bind(mailbox)
            .bind(person)
            .execute(pg.pool())
            .await
            .unwrap();
    }
    crate::postgres::fixtures::activate_application(&pg).await;
    let source = crate::surreal::SurrealDb::uninitialized();
    assert!(super::super::from_source(parity(&source, &pg))
        .await
        .unwrap()
        .tables
        .is_empty());
    assert!(
        super::super::from_source(super::super::identity_detail::parity(&source, &pg))
            .await
            .unwrap()
            .tables
            .is_empty()
    );
    sqlx::query("DELETE FROM person_mailbox WHERE id='native-alias@example.com'")
        .execute(pg.pool())
        .await
        .unwrap();
    for key in [MirrorKey::Person(person), MirrorKey::PersonMailbox(person)] {
        assert_eq!(
            super::super::mirror(&source, &pg, &key).await.unwrap(),
            Mirrored::Unchanged
        );
    }
    let claims: i64 = sqlx::query_scalar("SELECT count(*) FROM person_mailbox WHERE person_id=$1")
        .bind(person)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(claims, 1);
    sqlx::query("DROP TABLE mirror_backfill_checkpoint")
        .execute(pg.pool())
        .await
        .unwrap();
    assert!(matches!(
        parity(&source, &pg).await,
        Err(MirrorError::Family(_))
    ));
    assert!(matches!(
        super::super::identity_detail::parity(&source, &pg).await,
        Err(MirrorError::Family(_))
    ));
}

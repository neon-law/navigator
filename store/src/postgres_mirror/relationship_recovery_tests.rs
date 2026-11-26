use super::*;
use crate::postgres_mirror::tests::{connect, fresh_schema};
use serde_json::{json, Value};

fn captured_relationship(id: &str, kind: &str) -> Value {
    json!({
        "id": id,
        "in": {"tb": "person", "id": {"Uuid": Uuid::now_v7().to_string()}},
        "out": {"tb": "entity", "id": {"Uuid": Uuid::now_v7().to_string()}},
        "kind": kind,
        "confidence_pct": 91,
        "source_kind": "manual",
        "source_id": null,
        "detail": "synthetic captured relationship",
        "inserted_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z"
    })
}

#[tokio::test]
async fn first_global_relationship_copy_preserves_existing_target_and_replays() {
    let (options, database) = fresh_schema().await.unwrap();
    let pg = connect(&options, &database).await;
    let captured = captured_relationship("synthetic-relationship", "adverse_to");
    assert_eq!(
        copy_record(&pg, "relationship", captured.clone())
            .await
            .unwrap(),
        Mirrored::Written
    );
    assert_eq!(
        super::super::during_initial_copy(copy_record(&pg, "relationship", captured.clone()))
            .await
            .unwrap(),
        Mirrored::Unchanged
    );
    sqlx::query("UPDATE relationship SET kind = 'related_party' WHERE id = $1")
        .bind("synthetic-relationship")
        .execute(pg.pool())
        .await
        .unwrap();
    assert_eq!(
        super::super::during_initial_copy(copy_record(&pg, "relationship", captured))
            .await
            .unwrap(),
        Mirrored::Unchanged
    );
    let kind: String = sqlx::query_scalar("SELECT kind FROM relationship WHERE id = $1")
        .bind("synthetic-relationship")
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(kind, "related_party");
    pg.pool().close().await;
}

#[tokio::test]
async fn native_origin_refuses_source_relationship_before_source_read() {
    crate::test_support::with_postgres(async {
        let pg = crate::postgres::PostgresDb::configured()
            .await
            .unwrap()
            .unwrap();
        let db = crate::surreal::SurrealDb::uninitialized();
        let first = crate::persons::create(
            &db,
            &crate::persons::NewPerson::new(
                "Synthetic First",
                format!("{}@example.test", Uuid::now_v7()),
            ),
        )
        .await
        .unwrap();
        let second = crate::persons::create(
            &db,
            &crate::persons::NewPerson::new(
                "Synthetic Second",
                format!("{}@example.test", Uuid::now_v7()),
            ),
        )
        .await
        .unwrap();
        crate::relationships::record(
            &db,
            &crate::relationships::NewRelationship {
                from: crate::relationships::Endpoint::Person,
                from_id: first.id,
                to: crate::relationships::Endpoint::Person,
                to_id: second.id,
                kind: "related_party".into(),
                confidence_pct: 100,
                source_kind: "manual".into(),
                source_id: None,
                detail: None,
            },
        )
        .await
        .unwrap();
        let id: String = sqlx::query_scalar("SELECT id FROM relationship")
            .fetch_one(pg.pool())
            .await
            .unwrap();
        let captured = captured_relationship(&id, "adverse_to");
        let source = crate::surreal::SurrealDb::uninitialized();
        assert!(matches!(
            super::super::during_initial_copy(mirror_string(&source, &pg, "relationship", &id))
                .await,
            Err(MirrorError::MatterAuthority)
        ));
        assert!(matches!(
            super::super::during_initial_copy(copy_record(&pg, "relationship", captured)).await,
            Err(MirrorError::MatterAuthority)
        ));
        let kind: String = sqlx::query_scalar("SELECT kind FROM relationship WHERE id = $1")
            .bind(&id)
            .fetch_one(pg.pool())
            .await
            .unwrap();
        assert_eq!(kind, "related_party");
    })
    .await;
}

use super::*;
use crate::postgres_mirror::tests::{connect, fresh_schema, synthetic_entity};
use crate::test_support::mem_surreal;

async fn captured_person(source: &SurrealDb) -> Uuid {
    let person = super::super::from_source(crate::persons::create(
        source,
        &crate::persons::NewPerson::with_role(
            "Synthetic Person",
            format!("identity-{}@example.com", Uuid::new_v4()),
            crate::persons::Role::Admin,
        ),
    ))
    .await
    .unwrap()
    .id;
    super::super::from_source(crate::persons::set_admitted(source, person, false))
        .await
        .unwrap();
    person
}

async fn import(source: &SurrealDb, pg: &PostgresDb, key: &MirrorKey) -> Mirrored {
    super::super::during_initial_copy(super::super::mirror(source, pg, key))
        .await
        .unwrap()
}

#[tokio::test]
async fn scoped_identity_import_retains_native_drift_and_deletion() {
    let (options, database) = fresh_schema().await.expect("required PostgreSQL fixture");
    let pg = connect(&options, &database).await;
    let source = mem_surreal().await;
    let entity = synthetic_entity(&source).await;
    let person = captured_person(&source).await;
    let before = scoped_parity(&source, &pg).await.unwrap();
    assert_eq!(
        before
            .tables
            .iter()
            .map(|table| (table.table, table.source_rows, table.target_rows))
            .collect::<Vec<_>>(),
        [("entity", 1, 0), ("person", 1, 0)]
    );
    for key in [MirrorKey::Entity(entity), MirrorKey::Person(person)] {
        assert_eq!(import(&source, &pg, &key).await, Mirrored::Written);
        assert_eq!(import(&source, &pg, &key).await, Mirrored::Unchanged);
    }
    assert!(scoped_parity(&source, &pg).await.unwrap().is_clean());
    sqlx::query(
        "UPDATE person SET name='Native changed person',role='client',is_admitted=true WHERE id=$1",
    )
    .bind(person)
    .execute(pg.pool())
    .await
    .unwrap();
    assert_eq!(
        import(&source, &pg, &MirrorKey::Person(person)).await,
        Mirrored::Unchanged
    );
    let state: (String, String, bool) =
        sqlx::query_as("SELECT name,role,is_admitted FROM person WHERE id=$1")
            .bind(person)
            .fetch_one(pg.pool())
            .await
            .unwrap();
    assert_eq!(
        state,
        ("Native changed person".into(), "client".into(), true)
    );
    assert_eq!(
        scoped_parity(&source, &pg).await.unwrap().tables[1].divergent,
        [MirrorKey::Person(person)]
    );
    sqlx::query("DELETE FROM entity WHERE id=$1")
        .bind(entity)
        .execute(pg.pool())
        .await
        .unwrap();
    assert_eq!(
        super::super::mirror(&source, &pg, &MirrorKey::Entity(entity))
            .await
            .unwrap(),
        Mirrored::Unchanged
    );
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM entity WHERE id=$1)")
        .bind(entity)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert!(!exists);
}

#[tokio::test]
async fn scoped_identity_refuses_bad_source_keys_and_never_removes_target_only_rows() {
    let (options, database) = fresh_schema().await.expect("required PostgreSQL fixture");
    let pg = connect(&options, &database).await;
    let source = mem_surreal().await;
    let native = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO person(id,name,email) VALUES($1,'Native Person','native-only@example.com')",
    )
    .bind(native)
    .execute(pg.pool())
    .await
    .unwrap();
    let report = scoped_parity(&source, &pg).await.unwrap();
    assert_eq!(
        (report.tables[1].source_rows, report.tables[1].target_rows),
        (0, 1)
    );
    assert_eq!(report.tables[1].divergent, [MirrorKey::Person(native)]);
    assert_eq!(
        import(&source, &pg, &MirrorKey::Person(native)).await,
        Mirrored::Unchanged
    );
    source.query("CREATE person:invalid_uuid SET name='Malformed source key', email='invalid-key@example.com'").await.unwrap().check().unwrap();
    assert!(matches!(
        scoped_parity(&source, &pg).await,
        Err(MirrorError::Source)
    ));
    source
        .query("DELETE person:invalid_uuid")
        .await
        .unwrap()
        .check()
        .unwrap();
    let entity = synthetic_entity(&source).await;
    let row = super::super::from_source(crate::entities::find_by_id(&source, entity))
        .await
        .unwrap()
        .unwrap();
    source
        .query("CREATE entity:invalid_uuid SET name='Malformed entity',entity_type_id=$kind,jurisdiction_id=$jurisdiction")
        .bind(("kind",record_id("entity_type",row.entity_type_id)))
        .bind(("jurisdiction",record_id("jurisdiction",row.jurisdiction_id)))
        .await
        .unwrap()
        .check()
        .unwrap();
    assert!(matches!(
        scoped_parity(&source, &pg).await,
        Err(MirrorError::Source)
    ));
    let present: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM person WHERE id=$1)")
        .bind(native)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert!(present);
}

#[tokio::test]
async fn catalog_parents_copy_only_before_global_history_completes() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = super::super::from_source(mem_surreal()).await;
    let entity = synthetic_entity(&surreal).await;
    let key = MirrorKey::Entity(entity);
    let outcome = super::super::from_source(super::super::during_initial_copy(mirror(
        &surreal, &pg, &key,
    )))
    .await
    .unwrap();
    assert!(matches!(outcome, Mirrored::Written));
    let jurisdiction: Uuid = sqlx::query_scalar("SELECT jurisdiction_id FROM entity WHERE id=$1")
        .bind(entity)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM entity WHERE id=$1")
        .bind(entity)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM jurisdiction WHERE id=$1")
        .bind(jurisdiction)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO mirror_backfill_run(id,batch_size,status,scope,completed_at) VALUES($1,1,'completed','global',now())")
        .bind(Uuid::new_v4()).execute(pg.pool()).await.unwrap();
    assert!(
        super::super::from_source(crate::jurisdictions::find_by_id(&surreal, jurisdiction))
            .await
            .unwrap()
            .is_some()
    );
    let result = super::super::from_source(super::super::during_initial_copy(mirror(
        &surreal, &pg, &key,
    )))
    .await;
    assert!(matches!(result, Err(MirrorError::MatterAuthority)));
    let restored: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM jurisdiction WHERE id=$1)")
            .bind(jurisdiction)
            .fetch_one(pg.pool())
            .await
            .unwrap();
    assert!(!restored);
    let result =
        super::super::from_source(ensure(&surreal, &pg, MirrorKey::Jurisdiction(jurisdiction)))
            .await;
    assert!(result.is_err());
}

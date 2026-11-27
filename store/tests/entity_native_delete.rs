//! Entity deletion reads native references without consulting a Source engine.
use store::application_store::ApplicationStore;
use store::entities::{self, Entity, EntityError, NewEntity};
use store::entity_commands::{self, EntityCommandError};
use store::postgres::{identity::AuthorityError, PostgresDb};
use store::relationships::{self, Endpoint, NewRelationship};
use uuid::Uuid;

async fn entity(db: &ApplicationStore, id: Uuid) -> Entity {
    let jurisdiction = store::jurisdictions::create(
        db,
        &store::jurisdictions::NewJurisdiction::new(
            format!("Synthetic jurisdiction {id}"),
            id.to_string(),
            "state",
        ),
    )
    .await
    .unwrap();
    let kind = store::entity_types::create(db, &format!("Synthetic kind {id}"))
        .await
        .unwrap();
    entities::upsert_with_id(
        db,
        id,
        &NewEntity {
            name: format!("Synthetic organization {id}"),
            entity_type_id: kind.id,
            jurisdiction_id: jurisdiction.id,
            phone: None,
            url: None,
            xero_id: None,
            firm_anchor_key: None,
        },
    )
    .await
    .unwrap()
}

async fn person(db: &ApplicationStore) -> Uuid {
    store::persons::create(
        db,
        &store::persons::NewPerson::with_role(
            "Synthetic participant",
            format!("synthetic-{}@example.com", Uuid::new_v4()),
            store::persons::Role::Client,
        ),
    )
    .await
    .unwrap()
    .id
}

async fn relationship(
    db: &ApplicationStore,
    from: Endpoint,
    from_id: Uuid,
    to: Endpoint,
    to_id: Uuid,
) {
    relationships::record(
        db,
        &NewRelationship {
            from,
            from_id,
            to,
            to_id,
            kind: relationships::KIND_RELATED_PARTY.into(),
            confidence_pct: 100,
            source_kind: relationships::SOURCE_MANUAL.into(),
            source_id: None,
            detail: None,
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn unreferenced_native_entity_deletes_and_absent_entity_is_not_found() {
    store::test_support::with_postgres(async {
        let db = ApplicationStore::uninitialized();
        let target = entity(&db, Uuid::new_v4()).await;
        assert!(entities::dependents(&db, target.id)
            .await
            .unwrap()
            .is_empty());
        let removed = entity_commands::delete_entity(&db, target.id, "Synthetic firm")
            .await
            .unwrap();
        assert_eq!(removed.id, target.id);
        assert!(entities::find_by_id(&db, target.id)
            .await
            .unwrap()
            .is_none());
        assert!(matches!(
            entity_commands::delete_entity(&db, target.id, "Synthetic firm").await,
            Err(EntityCommandError::NotFound)
        ));
    })
    .await;
}

#[tokio::test]
async fn native_dependent_counts_preserve_all_categories_order_and_endpoint_kinds() {
    store::test_support::with_postgres(async {
        let db = ApplicationStore::uninitialized();
        // The same UUID exists in two legitimate tables. Only typed Entity
        // graph endpoints may prevent this Entity from being removed.
        let same_id_person = person(&db).await;
        let target = entity(&db, same_id_person).await;
        let other = person(&db).await;
        relationship(&db, Endpoint::Person, same_id_person, Endpoint::Person, other).await;
        assert!(entities::dependents(&db, target.id).await.unwrap().is_empty());
        let pg = PostgresDb::application().await.unwrap();
        let project = Uuid::new_v4();
        sqlx::query("INSERT INTO project(id,code,name,status,entity_id,brand) VALUES($1,$2,'Synthetic matter','open',$3,'synthetic')")
            .bind(project).bind(format!("synthetic-{}", project.simple())).bind(target.id)
            .execute(pg.pool()).await.unwrap();
        for suffix in ["one", "two"] {
            store::addresses::create(&db, &store::addresses::NewAddress {
                person_id: None, entity_id: Some(target.id), line1: format!("Synthetic {suffix}"),
                line2: None, city: "Synthetic".into(), region: "NV".into(),
                postal_code: "00000".into(), country: "US".into(),
            }).await.unwrap();
        }
        store::entity_roles::grant(&db, other, target.id, "agent").await.unwrap();
        store::project_references::reference(&db, project,
            store::project_references::Party::entity(target.id),
            store::project_references::REASON_SIGNATORY).await.unwrap();
        relationship(&db, Endpoint::Entity, target.id, Endpoint::Person, other).await;
        relationship(&db, Endpoint::Person, other, Endpoint::Entity, target.id).await;
        relationship(&db, Endpoint::Entity, target.id, Endpoint::Entity, target.id).await;
        let found = entities::dependents(&db, target.id).await.unwrap();
        assert_eq!(found.iter().map(|row|(row.singular,row.plural,row.count)).collect::<Vec<_>>(), [
            ("project","projects",1), ("address","addresses",2), ("entity role","entity roles",1),
            ("relationship","relationships",3), ("project reference","project references",1),
        ]);
        assert!(matches!(entity_commands::delete_entity(&db, target.id, "Synthetic firm").await,
            Err(EntityCommandError::Referenced(reason)) if reason ==
                "1 project, 2 addresses, 1 entity role, 3 relationships and 1 project reference still reference it"));
        assert!(entities::find_by_id(&db, target.id).await.unwrap().is_some());
        assert!(store::projects::find_by_id(&db, project).await.unwrap().is_some());
    }).await;
}

#[tokio::test]
async fn native_firm_anchor_remains_protected_by_command_and_delete_statement() {
    store::test_support::with_postgres(async {
        let db = ApplicationStore::uninitialized();
        let catalog = entity(&db, Uuid::new_v4()).await;
        let anchor = entity_commands::create_entity(
            &db,
            "Synthetic firm",
            &entity_commands::CreateEntityCommand {
                name: "Synthetic firm".into(),
                entity_type_id: catalog.entity_type_id,
                jurisdiction_id: catalog.jurisdiction_id,
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            entity_commands::delete_entity(&db, anchor.id, "Synthetic firm").await,
            Err(EntityCommandError::FirmAnchorProtected)
        ));
        assert!(entities::delete_unless_firm_anchor(&db, anchor.id)
            .await
            .unwrap()
            .is_none());
        assert!(entities::find_by_id(&db, anchor.id)
            .await
            .unwrap()
            .is_some());
    })
    .await;
}

#[tokio::test]
async fn native_dependent_count_failure_never_reads_source_or_deletes_entity() {
    store::test_support::with_postgres(async {
        let db = ApplicationStore::uninitialized();
        let target = entity(&db, Uuid::new_v4()).await;
        let pg = PostgresDb::application().await.unwrap();
        let observer = sqlx::PgPool::connect_with(pg.pool().connect_options().as_ref().clone())
            .await
            .unwrap();
        pg.pool().close().await;
        assert!(matches!(
            entities::dependents(&db, target.id).await,
            Err(EntityError::Authority(AuthorityError::Unavailable(_)))
        ));
        let remains: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM entity WHERE id=$1)")
            .bind(target.id)
            .fetch_one(&observer)
            .await
            .unwrap();
        assert!(remains);
        observer.close().await;
    })
    .await;
}

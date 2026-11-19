//! Native Project deletion must finish on its admitted PostgreSQL pool.
use store::persons::Role;
use store::postgres::PostgresDb;
use store::projects::{self, ProjectCommandError};
use store::surreal::SurrealDb;
use uuid::Uuid;

async fn entity(db: &SurrealDb) -> Uuid {
    let id = Uuid::new_v4();
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
    store::entities::create(
        db,
        &store::entities::NewEntity {
            name: format!("Synthetic entity {id}"),
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
    .id
}

async fn fixture(db: &SurrealDb) -> (Uuid, Uuid) {
    let admin = store::persons::create(
        db,
        &store::persons::NewPerson::with_role(
            "Synthetic administrator",
            format!("synthetic-{}@example.com", Uuid::new_v4()),
            Role::Admin,
        ),
    )
    .await
    .unwrap();
    let firm = store::firms::create(
        db,
        &store::firms::NewFirm {
            name: "Synthetic firm".to_owned(),
            status: "active".to_owned(),
            entity_id: entity(db).await,
            admin_dri_person_id: admin.id,
        },
    )
    .await
    .unwrap();
    let key = format!("synthetic-{}", Uuid::new_v4().simple());
    store::brands::create(
        db,
        Role::Admin,
        Some(admin.id),
        &store::brands::NewBrand {
            name: "Synthetic brand".to_owned(),
            key: key.clone(),
            firm_id: Some(firm.id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    store::firms::attach_brand(db, Role::Owner, None, firm.id, &key)
        .await
        .unwrap();
    let project = projects::create(
        db,
        &projects::NewProject {
            code: format!("synthetic-{}", Uuid::new_v4().simple()),
            name: "Synthetic matter".to_owned(),
            status: "open".to_owned(),
            entity_id: entity(db).await,
            firm_id: Some(firm.id),
            brand: key,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    (project.id, admin.id)
}

#[tokio::test]
async fn deleting_an_empty_project_finishes_without_a_source_connection() {
    store::test_support::with_postgres(async {
        let db = SurrealDb::uninitialized();
        let (id, _) = fixture(&db).await;
        let deleted = projects::delete_project_with_surreal(&db, id)
            .await
            .unwrap();
        assert_eq!(deleted.id, id);
        assert!(projects::find_by_id(&db, id).await.unwrap().is_none());
    })
    .await;
}

#[tokio::test]
async fn a_participating_person_still_protects_the_project_from_deletion() {
    store::test_support::with_postgres(async {
        let db = SurrealDb::uninitialized();
        let (id, admin) = fixture(&db).await;
        projects::add_participation(&db, id, admin, "lawyer")
            .await
            .unwrap();
        assert!(matches!(
            projects::delete_project_with_surreal(&db, id).await,
            Err(ProjectCommandError::Referenced(table)) if table == "person_project_roles"
        ));
        assert!(projects::find_by_id(&db, id).await.unwrap().is_some());
        assert_eq!(
            projects::participations_for_project(&db, id)
                .await
                .unwrap()
                .len(),
            1
        );
    })
    .await;
}

#[tokio::test]
async fn an_unavailable_configured_pool_does_not_fall_back_to_source() {
    store::test_support::with_postgres(async {
        let db = SurrealDb::uninitialized();
        let (id, _) = fixture(&db).await;
        let pg = PostgresDb::application().await.unwrap();
        let observer = sqlx::PgPool::connect_with(pg.pool().connect_options().as_ref().clone())
            .await
            .unwrap();
        pg.pool().close().await;
        assert!(matches!(
            projects::delete_project_with_surreal(&db, id).await,
            Err(ProjectCommandError::Db(_))
        ));
        let remains: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM project WHERE id=$1)")
            .bind(id)
            .fetch_one(&observer)
            .await
            .unwrap();
        assert!(remains);
        observer.close().await;
    })
    .await;
}

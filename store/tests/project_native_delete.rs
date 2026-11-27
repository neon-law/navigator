//! Native Project deletion must finish on its admitted PostgreSQL pool.
use store::application_store::ApplicationStore;
use store::persons::Role;
use store::postgres::PostgresDb;
use store::projects::{self, ProjectCommandError};
use uuid::Uuid;

async fn entity(db: &ApplicationStore) -> Uuid {
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

async fn fixture(db: &ApplicationStore) -> (Uuid, Uuid) {
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
            name: format!("Firm {}", Uuid::new_v4().simple()),
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
            name: format!("Brand {}", Uuid::new_v4().simple()),
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
        let db = ApplicationStore::uninitialized();
        let (id, _) = fixture(&db).await;
        let deleted = projects::delete_project(&db, id).await.unwrap();
        assert_eq!(deleted.id, id);
        assert!(projects::find_by_id(&db, id).await.unwrap().is_none());
    })
    .await;
}

#[tokio::test]
async fn a_participating_person_still_protects_the_project_from_deletion() {
    store::test_support::with_postgres(async {
        let db = ApplicationStore::uninitialized();
        let (id, admin) = fixture(&db).await;
        projects::add_participation(&db, id, admin, "lawyer")
            .await
            .unwrap();
        assert!(matches!(
            projects::delete_project(&db, id).await,
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
        let db = ApplicationStore::uninitialized();
        let (id, _) = fixture(&db).await;
        let pg = PostgresDb::application().await.unwrap();
        let observer = sqlx::PgPool::connect_with(pg.pool().connect_options().as_ref().clone())
            .await
            .unwrap();
        pg.pool().close().await;
        assert!(matches!(
            projects::delete_project(&db, id).await,
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

#[tokio::test]
async fn removing_participation_finishes_on_postgres_and_preserves_other_projects() {
    store::test_support::with_postgres(async {
        let db = ApplicationStore::uninitialized();
        let (project, person) = fixture(&db).await;
        let (other_project, other_person) = fixture(&db).await;
        let role = projects::add_participation(&db, project, person, "lawyer")
            .await
            .unwrap();
        let other = projects::add_participation(&db, other_project, other_person, "lawyer")
            .await
            .unwrap();
        projects::remove_participation(&db, role.id).await.unwrap();
        assert!(projects::participation_by_id(&db, role.id)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            projects::participation_by_id(&db, other.id).await.unwrap(),
            Some(other)
        );
        projects::remove_participation(&db, role.id).await.unwrap();
        projects::remove_participation(&db, Uuid::new_v4())
            .await
            .unwrap();
        assert!(projects::participations_for_project(&db, project)
            .await
            .unwrap()
            .is_empty());
    })
    .await;
}

#[tokio::test]
async fn native_participation_removal_keeps_the_last_lawyer_guard_and_absent_error() {
    use store::participation::{self, DriActor, RemoveParticipantError};
    store::test_support::with_postgres(async {
        let db = ApplicationStore::uninitialized();
        let (project, first) = fixture(&db).await;
        projects::designate_dri(&db, project, first, projects::DriSide::Lawyer)
            .await
            .unwrap();
        let first_row = projects::participation_for_person(&db, first, project)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            participation::remove_participant(&db, project, first_row.id, DriActor::Person(first))
                .await,
            Err(RemoveParticipantError::DriLockout)
        ));
        assert_eq!(
            projects::participation_by_id(&db, first_row.id)
                .await
                .unwrap(),
            Some(first_row.clone())
        );
        let second = store::persons::create(
            &db,
            &store::persons::NewPerson::with_role(
                "Synthetic lawyer",
                format!("synthetic-{}@example.com", Uuid::new_v4()),
                Role::Lawyer,
            ),
        )
        .await
        .unwrap();
        projects::designate_dri(&db, project, second.id, projects::DriSide::Lawyer)
            .await
            .unwrap();
        let second_row = projects::participation_for_person(&db, second.id, project)
            .await
            .unwrap()
            .unwrap();
        participation::remove_participant(&db, project, first_row.id, DriActor::Person(second.id))
            .await
            .unwrap();
        assert!(projects::participation_by_id(&db, first_row.id)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            projects::participation_by_id(&db, second_row.id)
                .await
                .unwrap(),
            Some(second_row)
        );
        assert!(matches!(
            participation::remove_participant(
                &db,
                project,
                first_row.id,
                DriActor::Person(second.id)
            )
            .await,
            Err(RemoveParticipantError::NotFound)
        ));
    })
    .await;
}

#[tokio::test]
async fn participation_removal_refuses_failed_postgres_admission_without_source_fallback() {
    store::test_support::with_postgres(async {
        let db = ApplicationStore::uninitialized();
        let (project, person) = fixture(&db).await;
        let role = projects::add_participation(&db, project, person, "lawyer")
            .await
            .unwrap();
        let pg = PostgresDb::application().await.unwrap();
        let observer = sqlx::PgPool::connect_with(pg.pool().connect_options().as_ref().clone())
            .await
            .unwrap();
        pg.pool().close().await;
        assert!(matches!(
            projects::remove_participation(&db, role.id).await,
            Err(projects::ProjectStoreError::Tenancy(
                store::postgres::tenancy::TenancyError::Postgres(
                    store::postgres::PostgresError::History
                )
            ))
        ));
        let remains: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM person_project_role WHERE id=$1)")
                .bind(role.id)
                .fetch_one(&observer)
                .await
                .unwrap();
        assert!(remains);
        observer.close().await;
    })
    .await;
}

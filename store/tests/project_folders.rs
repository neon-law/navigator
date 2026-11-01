//! Project Folder bindings against an isolated PostgreSQL database.
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use store::document_pointers::MatterFolder;
use store::postgres::PostgresDb;
use store::project_folders::{self, ProjectFolderError, ProjectFolderSlot as Slot};
use uuid::Uuid;

async fn database() -> Option<PostgresDb> {
    let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
        assert_ne!(std::env::var("NAV_REQUIRE_POSTGRES").as_deref(), Ok("1"));
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
    let admin = sqlx::PgPool::connect_with(options.clone()).await.unwrap();
    let name = format!("project_folder_{}", Uuid::now_v7().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    let pg = PostgresDb::connect(options.database(&name)).await.unwrap();
    pg.migrate_embedded(None).await.unwrap();
    pg.migrate_embedded(None).await.unwrap();
    Some(pg)
}

async fn project(pg: &PostgresDb) -> Uuid {
    let id = Uuid::now_v7();
    let entity = Uuid::now_v7();
    let kind = Uuid::now_v7();
    let jurisdiction = Uuid::now_v7();
    sqlx::query("INSERT INTO jurisdiction(id,name) VALUES ($1,$2)")
        .bind(jurisdiction)
        .bind(format!("Synthetic {id}"))
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO entity_type(id,name) VALUES ($1,$2)")
        .bind(kind)
        .bind(format!("Synthetic {id}"))
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO entity(id,name,entity_type_id,jurisdiction_id) VALUES ($1,'Synthetic Entity',$2,$3)")
        .bind(entity).bind(kind).bind(jurisdiction).execute(pg.pool()).await.unwrap();
    sqlx::query("INSERT INTO project(id,code,name,status,entity_id) VALUES ($1,$2,'Synthetic Project','open',$3)")
        .bind(id).bind(format!("synthetic-{id}")).bind(entity).execute(pg.pool()).await.unwrap();
    id
}

#[test]
fn slots_reuse_the_current_matter_tree_and_refuse_unknown_names() {
    for folder in MatterFolder::TREE {
        let slot = Slot::Matter(folder);
        assert_eq!(Slot::parse(slot.key()), Some(slot));
    }
    assert_eq!(Slot::parse("root"), Some(Slot::Root));
    assert_eq!(
        Slot::parse("client_submissions"),
        Some(Slot::ClientSubmissions)
    );
    assert_eq!(Slot::parse("Discovery"), None);
    assert_eq!(Slot::parse("unknown"), None);
}

#[tokio::test]
async fn concurrent_slot_retries_converge_and_refuse_cross_project_or_changed_bindings() {
    let Some(pg) = database().await else { return };
    let a = project(&pg).await;
    let b = project(&pg).await;
    let (first, retry) = tokio::join!(
        project_folders::bind(
            &pg,
            a,
            Slot::Root,
            "drive-root-a",
            None,
            Some("shared-parent")
        ),
        project_folders::bind(
            &pg,
            a,
            Slot::Root,
            "drive-root-a",
            None,
            Some("shared-parent")
        ),
    );
    let first = first.unwrap();
    assert!(project_folders::recorded_elsewhere(&pg, b, "drive-root-a")
        .await
        .unwrap());
    assert!(!project_folders::recorded_elsewhere(&pg, a, "drive-root-a")
        .await
        .unwrap());
    assert!(
        !project_folders::recorded_elsewhere(&pg, b, "unbound-folder")
            .await
            .unwrap()
    );
    assert_eq!(first, retry.unwrap());
    assert_eq!(project_folders::list(&pg, a).await.unwrap().len(), 1);
    assert!(project_folders::list(&pg, b).await.unwrap().is_empty());
    assert!(matches!(
        project_folders::bind(&pg, b, Slot::Root, "drive-root-a", None, None).await,
        Err(ProjectFolderError::BindingConflict)
    ));
    assert!(matches!(
        project_folders::bind(&pg, a, Slot::Root, "replacement", None, None).await,
        Err(ProjectFolderError::BindingConflict)
    ));
    assert!(matches!(
        project_folders::bind(&pg, a, Slot::ClientSubmissions, "drive-root-a", None, None).await,
        Err(ProjectFolderError::BindingConflict)
    ));
    assert_eq!(
        project_folders::get(&pg, a, Slot::Root).await.unwrap(),
        Some(first)
    );
}

#[tokio::test]
async fn interrupted_tree_provisioning_resumes_with_same_project_typed_parents() {
    let Some(pg) = database().await else { return };
    let a = project(&pg).await;
    let b = project(&pg).await;
    let root = project_folders::bind(&pg, a, Slot::Root, "root", None, None)
        .await
        .unwrap();
    let work = project_folders::bind(
        &pg,
        a,
        Slot::Matter(MatterFolder::WorkProduct),
        "work",
        Some(root.id),
        Some("root"),
    )
    .await
    .unwrap();
    // A later pass resumes from the durable parent rather than creating it again.
    let recovered = project_folders::get(&pg, a, Slot::Matter(MatterFolder::WorkProduct))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered, work);
    // The relational constraint also refuses cross-Project parent links
    // from a writer that bypasses the typed store API.
    assert!(sqlx::query(
        "INSERT INTO project_folder (id,project_id,slot,drive_folder_id,parent_id,parent_drive_folder_id,inserted_at) \
         VALUES ($1,$2,'Work Product','cross-project', $3,'root',now())",
    )
    .bind(Uuid::now_v7()).bind(b).bind(root.id).execute(pg.pool()).await.is_err());
    let research = Slot::Matter(MatterFolder::ResearchWorkProduct);
    assert!(matches!(
        project_folders::bind(&pg, a, research, "research", Some(root.id), Some("root")).await,
        Err(ProjectFolderError::InvalidParent)
    ));
    assert!(matches!(
        project_folders::bind(&pg, b, research, "research", Some(work.id), Some("work")).await,
        Err(ProjectFolderError::InvalidParent)
    ));
    assert!(matches!(
        project_folders::bind(
            &pg,
            a,
            research,
            "research",
            Some(work.id),
            Some("wrong-parent")
        )
        .await,
        Err(ProjectFolderError::InvalidParent)
    ));
    let saved = project_folders::bind(&pg, a, research, "research", Some(work.id), Some("work"))
        .await
        .unwrap();
    assert_eq!(
        project_folders::bind(&pg, a, research, "research", Some(work.id), Some("work"))
            .await
            .unwrap(),
        saved
    );
    let intake = project_folders::bind(
        &pg,
        a,
        Slot::ClientSubmissions,
        "submissions",
        None,
        Some("shared-parent"),
    )
    .await
    .unwrap();
    assert_eq!(intake.parent_id, None);
    assert_eq!(project_folders::list(&pg, a).await.unwrap().len(), 4);
    assert!(matches!(
        project_folders::bind(
            &pg,
            a,
            Slot::Matter(MatterFolder::Discovery),
            "retired",
            Some(root.id),
            Some("root")
        )
        .await,
        Err(ProjectFolderError::InvalidSlot)
    ));
    assert!(matches!(
        project_folders::bind(&pg, b, Slot::Root, "bad identity", None, None).await,
        Err(ProjectFolderError::InvalidIdentity)
    ));
}

#[tokio::test]
async fn reservations_are_hidden_until_verified_and_retries_preserve_the_same_id() {
    let Some(pg) = database().await else { return };
    let a = project(&pg).await;
    let slot = Slot::Root;
    let reserved = project_folders::bind(&pg, a, slot, "reserved-root", None, None)
        .await
        .unwrap();
    assert!(reserved.verified_at.is_none());
    assert!(project_folders::get_verified(&pg, a, slot)
        .await
        .unwrap()
        .is_none());
    assert!(project_folders::list_verified(&pg, a)
        .await
        .unwrap()
        .is_empty());
    // Simulate a remote timeout: the next pass must reuse the reserved ID.
    assert_eq!(
        project_folders::bind(&pg, a, slot, "reserved-root", None, None)
            .await
            .unwrap(),
        reserved
    );
    let verified = project_folders::mark_verified(&pg, a, slot, "reserved-root")
        .await
        .unwrap();
    assert!(verified.verified_at.is_some());
    assert_eq!(
        project_folders::mark_verified(&pg, a, slot, "reserved-root")
            .await
            .unwrap(),
        verified
    );
    assert_eq!(
        project_folders::list_verified(&pg, a).await.unwrap(),
        vec![verified]
    );
    assert!(matches!(
        project_folders::mark_verified(&pg, a, slot, "wrong-id").await,
        Err(ProjectFolderError::BindingConflict)
    ));
    project_folders::clear_verification(&pg, a, slot, "reserved-root")
        .await
        .unwrap();
    assert!(project_folders::get_verified(&pg, a, slot)
        .await
        .unwrap()
        .is_none());
    assert!(project_folders::list_verified(&pg, a)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        project_folders::get(&pg, a, slot)
            .await
            .unwrap()
            .unwrap()
            .id,
        reserved.id
    );
}

#[tokio::test]
async fn starting_reconciliation_hides_all_old_successes_on_only_that_project() {
    let Some(pg) = database().await else { return };
    let a = project(&pg).await;
    let b = project(&pg).await;
    for (project_id, slot, id) in [
        (a, Slot::Root, "a-root"),
        (a, Slot::ClientSubmissions, "a-submissions"),
        (b, Slot::Root, "b-root"),
    ] {
        project_folders::bind(&pg, project_id, slot, id, None, None)
            .await
            .unwrap();
        project_folders::mark_verified(&pg, project_id, slot, id)
            .await
            .unwrap();
    }
    project_folders::clear_project_verification(&pg, a)
        .await
        .unwrap();
    assert!(project_folders::list_verified(&pg, a)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(project_folders::list(&pg, a).await.unwrap().len(), 2);
    assert_eq!(
        project_folders::list_verified(&pg, b).await.unwrap().len(),
        1
    );
    project_folders::clear_project_verification(&pg, a)
        .await
        .unwrap();
    assert_eq!(project_folders::list(&pg, a).await.unwrap().len(), 2);
}

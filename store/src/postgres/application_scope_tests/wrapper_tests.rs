//! Public wrapper and shared-provider-tail ownership boundaries.
use super::super::PostgresDb;
use cloud::DriveService as _;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uuid::Uuid;

async fn project(pg: &PostgresDb) -> (Uuid, Uuid, Uuid) {
    let (actor, notation, _) = super::super::fixtures::workflow_fixture(pg).await;
    let (firm, project) = super::super::fixtures::firm_for_notation(pg, notation).await;
    let brand = format!("synthetic-{}", Uuid::new_v4().simple());
    sqlx::query("INSERT INTO brand(name,brand_key,firm_id) VALUES($1,$1,$2)")
        .bind(&brand)
        .bind(firm)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO firm_brand(firm_id,brand_key) VALUES($1,$2)")
        .bind(firm)
        .bind(&brand)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE project SET brand=$1 WHERE id=$2")
        .bind(brand)
        .bind(project)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE person SET role='lawyer',email='scope-lawyer@example.test' WHERE id=$1")
        .bind(actor)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO person_firm_role(person_id,firm_id,membership) VALUES($1,$2,'lawyer')",
    )
    .bind(actor)
    .bind(firm)
    .execute(pg.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO person_project_role(person_id,project_id,participation,is_lawyer_dri) VALUES($1,$2,'lawyer',true)")
        .bind(actor).bind(project).execute(pg.pool()).await.unwrap();
    (actor, firm, project)
}

#[derive(Default)]
struct Clean(AtomicUsize);
#[async_trait::async_trait]
impl crate::drive_import::IngestScreen for Clean {
    async fn is_clean(&self, _: &[u8]) -> Result<bool, ()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(true)
    }
}

async fn folders(
    db: &crate::surreal::SurrealDb,
    pg: &PostgresDb,
    id: Uuid,
) -> (
    cloud::drive::FakeDrive,
    crate::project_folder_reconcile::FolderProvisioningPolicy,
) {
    let drive = cloud::drive::FakeDrive::default();
    let parent = drive.create_folder("synthetic-projects").await.unwrap();
    let policy = crate::project_folder_reconcile::FolderProvisioningPolicy {
        projects_parent_id: parent.id,
        trusted_organizers: std::collections::BTreeSet::default(),
    };
    let report = crate::project_folder_reconcile::reconcile(db, pg, &drive, id, &policy)
        .await
        .unwrap();
    assert_eq!(
        report.outcome,
        crate::project_folder_reconcile::FolderOutcome::Complete
    );
    (drive, policy)
}

#[tokio::test]
async fn drive_public_import_files_only_into_the_owning_pool() {
    crate::test_support::with_postgres(async {
        let owned = PostgresDb::application().await.unwrap();
        let (actor, _, id) = project(&owned).await;
        crate::test_support::with_postgres(async {
            let ambient = PostgresDb::application().await.unwrap();
            PostgresDb::with_application_db(owned.clone(), async {
                let db = crate::surreal::SurrealDb::uninitialized();
                let (drive, _) = folders(&db, &owned, id).await;
                let slot = crate::project_folders::ProjectFolderSlot::Matter(
                    crate::document_pointers::MatterFolder::WorkProduct,
                );
                let folder = crate::project_folders::get_verified(&owned, id, slot)
                    .await
                    .unwrap()
                    .unwrap();
                let file = drive
                    .add_file(
                        &folder.drive_folder_id,
                        "synthetic.txt",
                        "text/plain",
                        b"synthetic body".to_vec(),
                        None,
                    )
                    .unwrap();
                let directory = tempfile::tempdir().unwrap();
                let storage: Arc<dyn cloud::StorageService> =
                    Arc::new(cloud::FsStorage::new(directory.path()).await.unwrap());
                let screen = Clean::default();
                let args = crate::drive_import::DriveImportArgs {
                    kind: "unclassified",
                    visibility: crate::documents::visibility::CLIENT,
                    description: None,
                    dry_run: false,
                };
                let first = crate::drive_import::import_project_files(
                    &db, &storage, &drive, &screen, id, actor, &args,
                )
                .await
                .unwrap();
                assert_eq!(first.imported.len(), 1);
                assert_eq!(first.imported[0].drive_file_id, file.id);
                assert!(first.imported[0].changed);
                let head = crate::assets::find_by_id(&db, first.imported[0].asset_id)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(head.project_id, Some(id));
                assert_eq!(
                    head.sha256_hex,
                    crate::assets::sha256_hex(b"synthetic body")
                );
                assert_eq!(
                    storage.get(&head.storage_key).await.unwrap().bytes,
                    b"synthetic body"
                );
                assert_eq!(head.metadata.as_ref().unwrap()["drive_file_id"], file.id);
                let replay = crate::drive_import::import_project_files(
                    &db, &storage, &drive, &screen, id, actor, &args,
                )
                .await
                .unwrap();
                assert_eq!(replay.imported[0].asset_id, head.id);
                assert!(!replay.imported[0].changed);
                assert!(screen.0.load(Ordering::SeqCst) > 0);
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM asset")
                        .fetch_one(ambient.pool())
                        .await
                        .unwrap(),
                    0
                );
                let calls = drive.list_files_calls();
                assert!(matches!(
                    crate::drive_import::import_project_files(
                        &db,
                        &storage,
                        &drive,
                        &screen,
                        id,
                        Uuid::new_v4(),
                        &args
                    )
                    .await,
                    Err(crate::drive_import::DriveImportError::Unauthorized)
                ));
                assert_eq!(drive.list_files_calls(), calls);
            })
            .await
            .unwrap();
        })
        .await;
    })
    .await;
}

#[tokio::test]
async fn configured_folder_tail_uses_owned_bindings_and_real_acl_reconciliation() {
    crate::test_support::with_postgres(async {
        let owned = PostgresDb::application().await.unwrap();
        let (_, _, id) = project(&owned).await;
        crate::test_support::with_postgres(async {
            let ambient = PostgresDb::application().await.unwrap();
            PostgresDb::with_application_db(owned.clone(), async {
                let db = crate::surreal::SurrealDb::uninitialized();
                let drive = cloud::drive::FakeDrive::default();
                let parent = drive.create_folder("synthetic-projects").await.unwrap();
                let policy = crate::project_folder_reconcile::FolderProvisioningPolicy {
                    projects_parent_id: parent.id,
                    trusted_organizers: std::collections::BTreeSet::default(),
                };
                let report =
                    crate::project_folder_reconcile::reconcile_configured(&db, &drive, id, &policy)
                        .await
                        .unwrap()
                        .unwrap();
                assert_eq!(
                    report.outcome,
                    crate::project_folder_reconcile::FolderOutcome::Complete
                );
                let bindings = crate::project_folders::list_verified(&owned, id)
                    .await
                    .unwrap();
                assert!(!bindings.is_empty());
                for binding in &bindings {
                    assert!(drive
                        .folder_access(&binding.drive_folder_id)
                        .await
                        .unwrap()
                        .permissions
                        .iter()
                        .any(|grant| grant.email.as_deref() == Some("scope-lawyer@example.test")));
                }
                assert!(crate::project_folders::list_verified(&ambient, id)
                    .await
                    .unwrap()
                    .is_empty());
                let again =
                    crate::project_folder_reconcile::reconcile_configured(&db, &drive, id, &policy)
                        .await
                        .unwrap()
                        .unwrap();
                assert_eq!(
                    again.outcome,
                    crate::project_folder_reconcile::FolderOutcome::Complete
                );
                assert_eq!(
                    crate::project_folders::list_verified(&owned, id)
                        .await
                        .unwrap()
                        .len(),
                    bindings.len()
                );
            })
            .await
            .unwrap();
        })
        .await;
    })
    .await;
}

async fn subprocess(test: &str, env: &[(&str, String)]) {
    let output = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .envs(env.iter().cloned())
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "isolated wrapper test failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
}

#[tokio::test]
async fn plan_seed_public_wrapper_keeps_owned_anchor_and_user_choices_on_replay() {
    const CHILD: &str = "NAVIGATOR_PLAN_SCOPE_PROBE";
    if std::env::var_os(CHILD).is_none() {
        let name = format!(
            "{}::plan_seed_public_wrapper_keeps_owned_anchor_and_user_choices_on_replay",
            module_path!().split_once("::").unwrap().1
        );
        subprocess(
            &name,
            &[
                (CHILD, "1".into()),
                ("NAVIGATOR_POSTGRES_HOST", "127.0.0.1".into()),
                (
                    crate::seed::BOOTSTRAP_COMPANY_ENV,
                    "Synthetic Plan Anchor".into(),
                ),
            ],
        )
        .await;
        return;
    }
    crate::test_support::with_postgres(async {
        let owned=PostgresDb::application().await.unwrap();
        let (_,firm,id)=project(&owned).await;
        let key=crate::entity_commands::firm_anchor_key("Synthetic Plan Anchor","Synthetic Plan Anchor").unwrap();
        sqlx::query("UPDATE entity SET name='Synthetic Plan Anchor',firm_anchor_key=$1 WHERE id=(SELECT entity_id FROM project WHERE id=$2)")
            .bind(key).bind(id).execute(owned.pool()).await.unwrap();
        sqlx::query("UPDATE firm SET entity_id=(SELECT entity_id FROM project WHERE id=$2) WHERE id=$1")
            .bind(firm).bind(id).execute(owned.pool()).await.unwrap();
        crate::test_support::with_postgres(async {
            let ambient=PostgresDb::application().await.unwrap();
            PostgresDb::with_application_db(owned.clone(),async {
                let db=crate::surreal::SurrealDb::uninitialized();
                assert_eq!(crate::firms::anchor_firm(&db).await.unwrap().unwrap().id,firm);
                crate::plan_seeds::seed_quickstart(&db).await.unwrap();
                let before:Vec<(Uuid,i32)>=sqlx::query_as("SELECT id,plan_fee_cents FROM plan_fee_schedule WHERE firm_id=$1 ORDER BY id")
                    .bind(firm).fetch_all(owned.pool()).await.unwrap();
                assert!(!before.is_empty());
                sqlx::query("UPDATE stage_definition SET label='Firm chosen label' WHERE firm_id=$1 AND position=(SELECT min(position) FROM stage_definition WHERE firm_id=$1)")
                    .bind(firm).execute(owned.pool()).await.unwrap();
                crate::plan_seeds::seed_quickstart(&db).await.unwrap();
                let after:Vec<(Uuid,i32)>=sqlx::query_as("SELECT id,plan_fee_cents FROM plan_fee_schedule WHERE firm_id=$1 ORDER BY id")
                    .bind(firm).fetch_all(owned.pool()).await.unwrap();
                assert_eq!(after,before);
                assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM stage_definition WHERE firm_id=$1").bind(firm).fetch_one(owned.pool()).await.unwrap(),6);
                assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM stage_definition WHERE firm_id=$1 AND label='Firm chosen label'").bind(firm).fetch_one(owned.pool()).await.unwrap(),1);
                assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM legal_plan").fetch_one(ambient.pool()).await.unwrap(),0);
                assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM plan_fee_schedule").fetch_one(ambient.pool()).await.unwrap(),0);
            }).await.unwrap();
        }).await;
    }).await;
}

const KEY:&str="ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIO+kywI2x0e1JcY7eJ50pKflZgKO0UKaS+cV8ssit45e synthetic@example.test";

#[tokio::test]
async fn membership_removal_reconciles_keys_and_stamps_only_the_owning_pool() {
    const CHILD: &str = "NAVIGATOR_SSH_SCOPE_PROBE";
    if std::env::var_os(CHILD).is_none() {
        let server = forgejo_server().await;
        let name = format!(
            "{}::membership_removal_reconciles_keys_and_stamps_only_the_owning_pool",
            module_path!().split_once("::").unwrap().1
        );
        subprocess(
            &name,
            &[
                (CHILD, "1".into()),
                ("NAVIGATOR_FORGEJO_ROOT_URL", server.uri()),
                (
                    "NAVIGATOR_FORGEJO_KEY_SYNC_LOGIN",
                    "navigator-key-sync".into(),
                ),
                ("NAVIGATOR_FORGEJO_KEY_SYNC_TOKEN", "synthetic-token".into()),
            ],
        )
        .await;
        server.verify().await;
        return;
    }
    crate::test_support::with_postgres(async {
        let owned = PostgresDb::application().await.unwrap();
        let (actor, firm, _) = project(&owned).await;
        let key = crate::ssh_public_keys::add(&owned, actor, KEY, Some("fixture"))
            .await
            .unwrap();
        assert!(key.last_synced_at.is_none());
        crate::test_support::with_postgres(async {
            let ambient = PostgresDb::application().await.unwrap();
            PostgresDb::with_application_db(owned.clone(), async {
                let db = crate::surreal::SurrealDb::uninitialized();
                crate::firms::remove_membership(
                    &db,
                    crate::persons::Role::Owner,
                    None,
                    actor,
                    firm,
                )
                .await
                .unwrap();
                assert!(crate::firms::memberships_for_person(&db, actor)
                    .await
                    .unwrap()
                    .is_empty());
                let keys = crate::ssh_public_keys::list_for_person(&owned, actor)
                    .await
                    .unwrap();
                assert_eq!(keys.len(), 1);
                assert_eq!(keys[0].id, key.id);
                assert!(keys[0].last_synced_at.is_some());
                assert!(crate::ssh_public_keys::list_for_person(&ambient, actor)
                    .await
                    .unwrap()
                    .is_empty());
            })
            .await
            .unwrap();
        })
        .await;
    })
    .await;
}

async fn forgejo_server() -> wiremock::MockServer {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .and(header("authorization", "token synthetic-token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"login":"navigator-key-sync"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/admin/users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!([{"login":"synthetic-lawyer","email":"scope-lawyer@example.test"}]),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/users/synthetic-lawyer/keys"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id":7,"key":KEY,"title":"navigator:fixture"},
            {"id":8,"key":KEY,"title":"personal"}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/admin/users/synthetic-lawyer/keys/7"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/admin/users/synthetic-lawyer/keys/8"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    server
}

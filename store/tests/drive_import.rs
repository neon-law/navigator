//! Drive ingest into Navigator storage: receipts, versions, retries, screening,
//! and folder scoping, all against a synthetic Drive and object store.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use cloud::{DriveService, FakeDrive, FsStorage, StorageService};
use serde_json::json;
use store::document_pointers::MatterFolder;
use store::documents::{source, visibility};
use store::drive_import::{
    import_project_files, import_project_files_with_folders, DriveImportArgs, DriveImportError,
    DriveImportReport, IngestScreen,
};
use store::persons::{self, NewPerson, Role};
use store::projects::{create, set_drive_folder_id, NewProject, Project};
use store::surreal::SurrealDb;
use store::test_support::{mem_surreal, seed_entity};

/// A screen that flags bytes containing a marker and can be made unavailable.
#[derive(Default)]
struct Screen {
    unavailable: bool,
    calls: AtomicUsize,
}

#[async_trait]
impl IngestScreen for Screen {
    async fn is_clean(&self, bytes: &[u8]) -> Result<bool, ()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.unavailable {
            return Err(());
        }
        Ok(!bytes.windows(6).any(|window| window == b"EICAR!"))
    }
}

struct World {
    surreal: SurrealDb,
    pg: store::postgres::PostgresDb,
    storage: Arc<dyn StorageService>,
    drive: FakeDrive,
    project: Project,
    folder_id: String,
    actor: Uuid,
    _dir: tempfile::TempDir,
}

use uuid::Uuid;

const ARGS: DriveImportArgs<'static> = DriveImportArgs {
    kind: "unclassified",
    visibility: visibility::CLIENT,
    description: Some("Dropped in Workspace"),
    dry_run: false,
};

/// The same pass, planned and never written.
const DRY_RUN: DriveImportArgs<'static> = DriveImportArgs {
    kind: "unclassified",
    visibility: visibility::CLIENT,
    description: Some("Dropped in Workspace"),
    dry_run: true,
};

async fn project(surreal: &SurrealDb, code: &str) -> Project {
    create(
        surreal,
        &NewProject {
            code: code.to_string(),
            name: format!("Synthetic {code}"),
            status: "open".to_string(),
            entity_id: seed_entity(surreal).await,
            ..Default::default()
        },
    )
    .await
    .expect("create matter")
}

/// A Project whose recorded folder carries a display name unrelated to its
/// code: the import is scoped by the folder link, never by name.
async fn setup(code: &str) -> Option<World> {
    let pg = database().await?;
    let surreal = mem_surreal().await;
    let project = project(&surreal, code).await;
    let drive = FakeDrive::default();
    let parent = drive
        .create_folder("Synthetic shared parent")
        .await
        .expect("shared parent");
    let folder = drive
        .ensure_child_folder(&parent.id, "Legacy display name")
        .await
        .expect("folder creation");
    set_drive_folder_id(&surreal, project.id, Some(&folder.id))
        .await
        .expect("set Drive folder")
        .expect("matter exists");
    let actor = persons::create(
        &surreal,
        &NewPerson::with_role("Drive Operator", "drive-operator@example.com", Role::Owner),
    )
    .await
    .expect("create operator")
    .id;
    store::projects::add_participation(&surreal, project.id, actor, "owner")
        .await
        .expect("firm participation");
    seed_pg_project(&pg, project.id).await;
    let submissions = drive
        .ensure_child_folder(&parent.id, "Synthetic submissions")
        .await
        .expect("submissions");
    for (slot, id) in [
        (
            store::project_folders::ProjectFolderSlot::Root,
            folder.id.as_str(),
        ),
        (
            store::project_folders::ProjectFolderSlot::ClientSubmissions,
            submissions.id.as_str(),
        ),
    ] {
        store::project_folders::bind(&pg, project.id, slot, id, None, Some(&parent.id))
            .await
            .expect("bind folder");
        drive
            .restrict_folder(id)
            .await
            .expect("isolated synthetic folder");
        drive
            .ensure_direct_member_permission(
                id,
                &cloud::drive::DriveMember {
                    kind: cloud::drive::DriveMemberKind::User,
                    email: "drive-operator@example.com".into(),
                },
                cloud::drive::DriveRole::Writer,
            )
            .await
            .expect("root grant");
        store::project_folders::mark_verified(&pg, project.id, slot, id)
            .await
            .expect("verify folder");
    }
    let dir = tempfile::tempdir().expect("storage directory");
    let storage: Arc<dyn StorageService> =
        Arc::new(FsStorage::new(dir.path()).await.expect("storage"));
    Some(World {
        surreal,
        pg,
        storage,
        drive,
        project,
        folder_id: folder.id,
        actor,
        _dir: dir,
    })
}

impl World {
    // Mirrors the importer's own signature.
    #[allow(clippy::result_large_err)]
    async fn import(&self, screen: &Screen) -> Result<DriveImportReport, DriveImportError> {
        import_project_files_with_folders(
            &self.surreal,
            &self.pg,
            &self.storage,
            &self.drive,
            screen,
            self.project.id,
            self.actor,
            &ARGS,
        )
        .await
    }

    async fn revisions(&self, slug: &str) -> Vec<store::assets::Asset> {
        let slug = store::assets::for_project(&self.surreal, self.project.id)
            .await
            .expect("assets")
            .into_iter()
            .find(|asset| {
                asset.slug.as_deref() == Some(slug)
                    || asset.filename.as_deref() == Some(slug)
                    || asset.documents_path() == slug
            })
            .and_then(|asset| asset.slug)
            .unwrap_or_else(|| slug.to_owned());
        store::assets::revisions(&self.surreal, self.project.id, &slug)
            .await
            .expect("revision chain")
    }

    /// The dry-run pass: everything [`Self::import`] does, planned and never
    /// written.
    #[allow(clippy::result_large_err)]
    async fn plan(&self, screen: &Screen) -> Result<DriveImportReport, DriveImportError> {
        import_project_files_with_folders(
            &self.surreal,
            &self.pg,
            &self.storage,
            &self.drive,
            screen,
            self.project.id,
            self.actor,
            &DRY_RUN,
        )
        .await
    }
}

#[tokio::test]
async fn drive_files_become_project_assets_and_a_rerun_writes_nothing() {
    let Some(world) = setup("drive-import").await else {
        return;
    };
    let screen = Screen::default();
    let file = world
        .drive
        .add_file(
            &world.folder_id,
            "notice.pdf",
            "application/pdf",
            b"synthetic drive bytes".to_vec(),
            Some("2026-09-21T12:00:00Z".to_string()),
        )
        .expect("file creation");

    let report = world.import(&screen).await.expect("Drive import");
    assert_eq!(report.imported.len(), 1);
    assert!(report.imported[0].changed);
    assert!(report.refused.is_empty());
    let assets = store::assets::for_project(&world.surreal, world.project.id)
        .await
        .expect("asset listing");
    assert_eq!(assets.len(), 1);
    let asset = &assets[0];
    assert_eq!(asset.source.as_deref(), Some(source::DRIVE));
    assert_eq!(asset.visibility, visibility::CLIENT);
    assert_eq!(
        asset.sha256_hex,
        store::assets::sha256_hex(b"synthetic drive bytes")
    );
    assert_eq!(
        asset.metadata,
        Some(json!({
            "drive_file_id": file.id,
            "drive_relative_path": "notice.pdf",
            "drive_source_root_id": world.folder_id,
            "drive_checkout_path": "Work Product/notice.pdf",
            "drive_matter_folder": "Work Product",
            "drive_modified_time": "2026-09-21T12:00:00Z",
            "drive_version": "1",
            "drive_md5_checksum": cloud::md5_hex(b"synthetic drive bytes"),
        }))
    );
    assert!(asset
        .storage_key
        .starts_with("projects/drive-import/documents/"));

    // An authorized read comes from Navigator storage, not Drive.
    let downloads = world.drive.download_calls();
    assert_eq!(
        store::assets::fetch(&world.surreal, &world.storage, asset.id)
            .await
            .expect("stored bytes"),
        b"synthetic drive bytes"
    );
    assert_eq!(world.drive.download_calls(), downloads);

    let repeated = world
        .import(&screen)
        .await
        .expect("idempotent Drive import");
    assert_eq!(repeated.imported.len(), 1);
    assert!(!repeated.imported[0].changed);
    assert_eq!(world.revisions("notice.pdf").await.len(), 1);
    // The document identity is the lawyer's own filename (LAW-216), and the
    // report names the matter folder a checkout downloads it into.
    assert_eq!(repeated.imported[0].filename, "notice.pdf");
    assert_eq!(
        repeated.imported[0].matter_folder,
        MatterFolder::WorkProduct
    );
}

#[tokio::test]
async fn a_new_drive_version_is_recorded_as_a_new_revision() {
    let Some(world) = setup("drive-versions").await else {
        return;
    };
    let screen = Screen::default();
    let file = world
        .drive
        .add_file(
            &world.folder_id,
            "plan.pdf",
            "application/pdf",
            b"first".to_vec(),
            None,
        )
        .expect("file creation");
    world.import(&screen).await.expect("first import");
    world.drive.replace_file(
        &file.id,
        b"second".to_vec(),
        Some("2026-09-22T09:00:00Z".into()),
    );
    let report = world.import(&screen).await.expect("second import");
    assert!(report.imported[0].changed);
    let chain = world.revisions("plan.pdf").await;
    assert_eq!(chain.len(), 2);
    let versions: Vec<_> = chain
        .iter()
        .map(|asset| asset.metadata.as_ref().unwrap()["drive_version"].clone())
        .collect();
    assert!(versions.contains(&json!("1")) && versions.contains(&json!("2")));
}

#[tokio::test]
async fn an_interrupted_download_converges_without_duplicates() {
    let Some(world) = setup("drive-interrupted").await else {
        return;
    };
    let screen = Screen::default();
    world
        .drive
        .add_file(
            &world.folder_id,
            "a.pdf",
            "application/pdf",
            b"alpha".to_vec(),
            None,
        )
        .expect("file a");
    world
        .drive
        .add_file(
            &world.folder_id,
            "b.pdf",
            "application/pdf",
            b"bravo".to_vec(),
            None,
        )
        .expect("file b");
    // The first file is filed, then the transfer of the second fails.
    world.import(&screen).await.expect("seed one file");
    world.drive.fail_downloads(1);
    assert!(matches!(
        world.import(&screen).await,
        Err(DriveImportError::Drive(_))
    ));
    let report = world.import(&screen).await.expect("rerun converges");
    assert_eq!(report.imported.len(), 2);
    assert_eq!(world.revisions("a.pdf").await.len(), 1);
    assert_eq!(world.revisions("b.pdf").await.len(), 1);
}

#[tokio::test]
async fn a_malformed_receipt_or_malware_files_nothing_and_an_unavailable_screen_stops() {
    let Some(world) = setup("drive-receipts").await else {
        return;
    };
    let file = world
        .drive
        .add_file(
            &world.folder_id,
            "deed.pdf",
            "application/pdf",
            b"deed".to_vec(),
            None,
        )
        .expect("file creation");
    world.drive.set_listed_checksum(&file.id, &"0".repeat(32));
    let report = world.import(&Screen::default()).await.expect("import runs");
    assert!(report.imported.is_empty());
    assert_eq!(report.refused[0].reason, "receipt_mismatch");
    assert!(world.revisions("deed.pdf").await.is_empty());

    let Some(malware) = setup("drive-malware").await else {
        return;
    };
    malware
        .drive
        .add_file(
            &malware.folder_id,
            "x.pdf",
            "application/pdf",
            b"..EICAR!..".to_vec(),
            None,
        )
        .expect("file creation");
    let report = malware
        .import(&Screen::default())
        .await
        .expect("import runs");
    assert_eq!(report.refused[0].reason, "malware_found");
    assert!(malware.revisions("x.pdf").await.is_empty());

    let unavailable = Screen {
        unavailable: true,
        ..Screen::default()
    };
    assert!(matches!(
        malware.import(&unavailable).await,
        Err(DriveImportError::ScreenUnavailable)
    ));
}

#[tokio::test]
async fn unauthorized_drive_import_stops_before_listing_or_persisting() {
    let Some(world) = setup("drive-unauthorized").await else {
        return;
    };
    let client = persons::create(
        &world.surreal,
        &NewPerson::new("Drive Client", "drive-client@example.com"),
    )
    .await
    .expect("create client");
    let error = import_project_files(
        &world.surreal,
        &world.storage,
        &world.drive,
        &Screen::default(),
        world.project.id,
        client.id,
        &ARGS,
    )
    .await
    .unwrap_err();
    assert!(matches!(error, DriveImportError::Unauthorized));
    assert_eq!(world.drive.list_files_calls(), 0);
    assert_eq!(world.drive.download_calls(), 0);
}

#[tokio::test]
async fn another_projects_folder_or_a_folder_outside_the_drive_is_refused() {
    let Some(world) = setup("drive-target").await else {
        return;
    };
    let other = project(&world.surreal, "drive-other").await;
    let other_folder = world
        .drive
        .create_folder(&other.code)
        .await
        .expect("other folder creation");
    world
        .drive
        .add_file(
            &other_folder.id,
            "other.pdf",
            "application/pdf",
            b"other".to_vec(),
            None,
        )
        .expect("other file creation");
    set_drive_folder_id(&world.surreal, other.id, Some(&other_folder.id))
        .await
        .expect("record the other matter's folder")
        .expect("matter exists");
    set_drive_folder_id(&world.surreal, world.project.id, Some(&other_folder.id))
        .await
        .expect("point at the other matter's folder")
        .expect("matter exists");
    assert!(matches!(
        world.import(&Screen::default()).await,
        Err(DriveImportError::FolderMismatch)
    ));

    set_drive_folder_id(
        &world.surreal,
        world.project.id,
        Some("folder-not-in-drive"),
    )
    .await
    .expect("point outside the drive")
    .expect("matter exists");
    assert!(matches!(
        world.import(&Screen::default()).await,
        Err(DriveImportError::FolderMismatch)
    ));
    assert_eq!(world.drive.list_files_calls(), 0);
    assert_eq!(world.drive.download_calls(), 0);
}

#[tokio::test]
async fn a_dry_run_predicts_a_real_pass_and_writes_nothing() {
    let Some(world) = setup("drive-dry-run").await else {
        return;
    };
    let screen = Screen::default();
    world
        .drive
        .add_file(
            &world.folder_id,
            "notice.pdf",
            "application/pdf",
            b"synthetic drive bytes".to_vec(),
            None,
        )
        .expect("file creation");

    let plan = world.plan(&screen).await.expect("dry run");
    assert_eq!(plan.imported.len(), 1);
    assert!(plan.imported[0].changed, "a fresh file would be filed");
    assert_eq!(plan.imported[0].filename, "notice.pdf");
    assert_eq!(plan.imported[0].matter_folder, MatterFolder::WorkProduct);
    assert_eq!(world.drive.download_calls(), 1, "the plan downloads");
    let assets = store::assets::for_project(&world.surreal, world.project.id)
        .await
        .expect("asset listing");
    assert!(assets.is_empty(), "a dry run files nothing");

    world.import(&screen).await.expect("real import");
    let rerun_plan = world.plan(&screen).await.expect("dry rerun");
    assert!(
        !rerun_plan.imported[0].changed,
        "the plan predicts a rerun writes nothing"
    );
    let assets = store::assets::for_project(&world.surreal, world.project.id)
        .await
        .expect("asset listing");
    assert_eq!(assets.len(), 1, "the second dry run still files nothing");
}

#[tokio::test]
async fn a_dry_run_refuses_exactly_what_a_real_pass_would() {
    let Some(world) = setup("drive-dry-refusals").await else {
        return;
    };
    world
        .drive
        .add_file(
            &world.folder_id,
            "statement.pdf",
            "application/pdf",
            b"Statement".to_vec(),
            None,
        )
        .expect("file creation");
    world
        .drive
        .add_file(
            &world.folder_id,
            "Statement.PDF",
            "application/pdf",
            b"Different bytes".to_vec(),
            None,
        )
        .expect("colliding file creation");
    world
        .drive
        .add_file(
            &world.folder_id,
            "invoice.pdf",
            "application/pdf",
            b"..EICAR!..".to_vec(),
            None,
        )
        .expect("malware creation");
    world
        .drive
        .add_file(
            &world.folder_id,
            "notice.pdf",
            "application/pdf",
            b"clean".to_vec(),
            None,
        )
        .expect("clean file creation");

    let plan = world.plan(&Screen::default()).await.expect("dry run");
    assert_eq!(plan.imported.len(), 1, "only the clean file is planned");
    assert_eq!(plan.imported[0].filename, "notice.pdf");
    let mut collided = plan
        .refused
        .iter()
        .filter(|refused| refused.reason == "name_collision")
        .map(|refused| refused.drive_file_id.as_str())
        .collect::<Vec<_>>();
    collided.sort_unstable();
    assert_eq!(collided.len(), 2, "every colliding member is flagged");
    assert!(
        plan.refused
            .iter()
            .any(|refused| refused.reason == "malware_found"),
        "the plan screens too"
    );
    let assets = store::assets::for_project(&world.surreal, world.project.id)
        .await
        .expect("asset listing");
    assert!(assets.is_empty());
}

#[tokio::test]
async fn same_name_files_are_refused_together_case_insensitively() {
    let Some(world) = setup("drive-collisions").await else {
        return;
    };
    let screen = Screen::default();
    world
        .drive
        .add_file(
            &world.folder_id,
            "report.pdf",
            "application/pdf",
            b"one".to_vec(),
            None,
        )
        .expect("file creation");
    world
        .drive
        .add_file(
            &world.folder_id,
            "Report.PDF",
            "application/pdf",
            b"two".to_vec(),
            None,
        )
        .expect("colliding file creation");

    let report = world.import(&screen).await.expect("import runs");
    assert!(report.imported.is_empty());
    assert_eq!(report.refused.len(), 2);
    assert!(
        report
            .refused
            .iter()
            .all(|refused| refused.reason == "name_collision"),
        "both colliding members are refused"
    );
    let assets = store::assets::for_project(&world.surreal, world.project.id)
        .await
        .expect("asset listing");
    assert!(assets.is_empty(), "nothing silently merged");
}

#[tokio::test]
async fn a_name_that_cannot_be_a_checkout_document_is_refused_before_download() {
    let Some(world) = setup("drive-unusable-names").await else {
        return;
    };
    world
        .drive
        .add_file(
            &world.folder_id,
            "../deed.pdf",
            "application/pdf",
            b"escaped".to_vec(),
            None,
        )
        .expect("file creation");

    assert!(
        world.import(&Screen::default()).await.is_err(),
        "unsafe provider tree is refused"
    );
    assert_eq!(
        world.drive.download_calls(),
        0,
        "an unusable name is refused before its bytes move"
    );
}

async fn database() -> Option<store::postgres::PostgresDb> {
    let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
        assert_ne!(std::env::var("NAV_REQUIRE_POSTGRES").as_deref(), Ok("1"));
        return None;
    };
    let options = sqlx::postgres::PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port.parse().unwrap())
        .username("postgres")
        .password("synthetic")
        .database(
            &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").unwrap_or_else(|_| "smoke".into()),
        )
        .ssl_mode(sqlx::postgres::PgSslMode::Disable);
    let admin = sqlx::PgPool::connect_with(options.clone()).await.unwrap();
    let name = format!("project_folder_{}", Uuid::now_v7().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    let pg = store::postgres::PostgresDb::connect(options.database(&name))
        .await
        .unwrap();
    pg.migrate_embedded(None).await.unwrap();
    pg.migrate_embedded(None).await.unwrap();
    Some(pg)
}

async fn seed_pg_project(pg: &store::postgres::PostgresDb, id: Uuid) {
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
}

#[tokio::test]
async fn nested_same_basename_and_submissions_preserve_paths_and_retry_without_duplicates() {
    let Some(world) = setup("drive-tree-paths").await else {
        return;
    };
    let one = world
        .drive
        .ensure_child_folder(&world.folder_id, "one")
        .await
        .unwrap();
    let two = world
        .drive
        .ensure_child_folder(&world.folder_id, "two")
        .await
        .unwrap();
    for folder in [&one, &two] {
        world.drive.inherit_folder(&folder.id).await.unwrap();
    }
    let a = world
        .drive
        .add_file(
            &one.id,
            "notice.pdf",
            "application/pdf",
            b"one".to_vec(),
            None,
        )
        .unwrap();
    world
        .drive
        .add_file(
            &two.id,
            "notice.pdf",
            "application/pdf",
            b"two".to_vec(),
            None,
        )
        .unwrap();
    let intake = store::project_folders::get_verified(
        &world.pg,
        world.project.id,
        store::project_folders::ProjectFolderSlot::ClientSubmissions,
    )
    .await
    .unwrap()
    .unwrap();
    world
        .drive
        .add_file(
            &intake.drive_folder_id,
            "notice.pdf",
            "application/pdf",
            b"intake".to_vec(),
            None,
        )
        .unwrap();
    let first = world.import(&Screen::default()).await.unwrap();
    assert_eq!(first.imported.len(), 3);
    let mut paths = store::assets::for_project(&world.surreal, world.project.id)
        .await
        .unwrap()
        .into_iter()
        .map(|asset| asset.documents_path())
        .collect::<Vec<_>>();
    paths.sort();
    assert_eq!(
        paths,
        [
            "Client Production/notice.pdf",
            "Work Product/one/notice.pdf",
            "Work Product/two/notice.pdf"
        ]
    );
    let retry = world.import(&Screen::default()).await.unwrap();
    assert!(retry.imported.iter().all(|row| !row.changed));
    assert_eq!(
        store::assets::for_project(&world.surreal, world.project.id)
            .await
            .unwrap()
            .len(),
        3
    );
    world.drive.replace_file(&a.id, b"updated".to_vec(), None);
    world.import(&Screen::default()).await.unwrap();
    assert_eq!(
        world.revisions("Work Product/one/notice.pdf").await.len(),
        2
    );
}

#[tokio::test]
async fn legacy_flat_source_identity_keeps_its_slug_and_checkout_placement() {
    let Some(world) = setup("drive-legacy-flat").await else {
        return;
    };
    let file = world
        .drive
        .add_file(
            &world.folder_id,
            "legacy.pdf",
            "application/pdf",
            b"legacy".to_vec(),
            None,
        )
        .unwrap();
    let identity = store::documents::DocumentIdentity {
        slug: Some("legacy.pdf"),
        metadata: Some(json!({"drive_file_id":file.id})),
        ..Default::default()
    };
    let ingest = store::documents::IngestArgs {
        project_id: world.project.id,
        source: source::DRIVE,
        filename: "legacy.pdf",
        kind: "unclassified",
        content_type: "application/pdf",
        description: None,
        secondary_storage_key: None,
        visibility: visibility::CLIENT,
    };
    store::assets::file_revision(
        &world.surreal,
        &world.storage,
        &ingest,
        &identity,
        b"legacy",
    )
    .await
    .unwrap();
    let retry = world.import(&Screen::default()).await.unwrap();
    assert_eq!(retry.imported.len(), 1);
    assert!(!retry.imported[0].changed);
    let assets = store::assets::for_project(&world.surreal, world.project.id)
        .await
        .unwrap();
    assert_eq!(assets.len(), 1);
    assert_eq!(assets[0].slug.as_deref(), Some("legacy.pdf"));
    assert_eq!(assets[0].documents_path(), "Work Product/legacy.pdf");
}

#[tokio::test]
async fn source_moves_renames_and_lawyer_reslug_preserve_one_identity() {
    let Some(world) = setup("drive-stable-identity").await else {
        return;
    };
    let file = world
        .drive
        .add_file(
            &world.folder_id,
            "original.pdf",
            "application/pdf",
            b"original".to_vec(),
            None,
        )
        .unwrap();
    world.import(&Screen::default()).await.unwrap();
    let initial = store::assets::for_project(&world.surreal, world.project.id)
        .await
        .unwrap()
        .remove(0);
    let reserved = initial.slug.clone().unwrap();
    assert!(reserved.starts_with("drive/"));
    assert_eq!(
        std::path::Path::new(&reserved)
            .extension()
            .and_then(|ext| ext.to_str()),
        Some("pdf")
    );
    assert!(!reserved.contains("original"));
    let nested = world
        .drive
        .ensure_child_folder(&world.folder_id, "nested")
        .await
        .unwrap();
    world.drive.inherit_folder(&nested.id).await.unwrap();
    assert!(world.drive.move_file(&file.id, &nested.id, "renamed.pdf"));
    let moved = world.import(&Screen::default()).await.unwrap();
    assert!(!moved.imported[0].changed);
    let assets = store::assets::for_project(&world.surreal, world.project.id)
        .await
        .unwrap();
    assert_eq!(assets.len(), 1);
    assert_eq!(assets[0].id, initial.id);
    assert_eq!(assets[0].slug.as_deref(), Some(reserved.as_str()));
    assert_eq!(
        assets[0].documents_path(),
        "Work Product/nested/renamed.pdf"
    );
    store::assets::reslug(
        &world.surreal,
        world.project.id,
        initial.id,
        "lawyer-chosen.pdf",
        false,
    )
    .await
    .unwrap();
    world
        .drive
        .replace_file(&file.id, b"new bytes".to_vec(), None);
    world.import(&Screen::default()).await.unwrap();
    let chain = store::assets::revisions(&world.surreal, world.project.id, "lawyer-chosen.pdf")
        .await
        .unwrap();
    assert_eq!(chain.len(), 2);
    assert!(chain
        .iter()
        .all(|asset| asset.slug.as_deref() == Some("lawyer-chosen.pdf")));
    assert!(world.drive.move_file(&file.id, &nested.id, "renamed.docx"));
    let refused = world.import(&Screen::default()).await.unwrap();
    assert!(refused.imported.is_empty());
    assert_eq!(refused.refused[0].reason, "source_format_changed");
    assert_eq!(
        store::assets::revisions(&world.surreal, world.project.id, "lawyer-chosen.pdf")
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn client_intake_is_production_even_when_kind_is_letter() {
    let Some(world) = setup("drive-intake-folder").await else {
        return;
    };
    let intake = store::project_folders::get_verified(
        &world.pg,
        world.project.id,
        store::project_folders::ProjectFolderSlot::ClientSubmissions,
    )
    .await
    .unwrap()
    .unwrap();
    let nested = world
        .drive
        .ensure_child_folder(&intake.drive_folder_id, "client-notes")
        .await
        .unwrap();
    world.drive.inherit_folder(&nested.id).await.unwrap();
    world
        .drive
        .add_file(
            &nested.id,
            "letter.pdf",
            "application/pdf",
            b"intake letter".to_vec(),
            None,
        )
        .unwrap();
    let args = DriveImportArgs {
        kind: "letter",
        ..ARGS
    };
    let report = import_project_files_with_folders(
        &world.surreal,
        &world.pg,
        &world.storage,
        &world.drive,
        &Screen::default(),
        world.project.id,
        world.actor,
        &args,
    )
    .await
    .unwrap();
    assert_eq!(
        report.imported[0].matter_folder,
        MatterFolder::ClientProduction
    );
    let assets = store::assets::for_project(&world.surreal, world.project.id)
        .await
        .unwrap();
    assert_eq!(assets[0].kind.as_deref(), Some("letter"));
    assert_eq!(
        assets[0].documents_path(),
        "Client Production/client-notes/letter.pdf"
    );
    assert_eq!(MatterFolder::TREE.len(), 14);
    assert!(!assets[0].documents_path().contains("Client Submissions"));
}

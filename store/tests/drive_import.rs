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
    import_project_files, DriveImportArgs, DriveImportError, DriveImportReport, IngestScreen,
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
async fn setup(code: &str) -> World {
    let surreal = mem_surreal().await;
    let project = project(&surreal, code).await;
    let drive = FakeDrive::default();
    let folder = drive
        .create_folder("Legacy display name")
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
    let dir = tempfile::tempdir().expect("storage directory");
    let storage: Arc<dyn StorageService> =
        Arc::new(FsStorage::new(dir.path()).await.expect("storage"));
    World {
        surreal,
        storage,
        drive,
        project,
        folder_id: folder.id,
        actor,
        _dir: dir,
    }
}

impl World {
    // Mirrors the importer's own signature.
    #[allow(clippy::result_large_err)]
    async fn import(&self, screen: &Screen) -> Result<DriveImportReport, DriveImportError> {
        import_project_files(
            &self.surreal,
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
        store::assets::revisions(&self.surreal, self.project.id, slug)
            .await
            .expect("revision chain")
    }

    /// The dry-run pass: everything [`Self::import`] does, planned and never
    /// written.
    #[allow(clippy::result_large_err)]
    async fn plan(&self, screen: &Screen) -> Result<DriveImportReport, DriveImportError> {
        import_project_files(
            &self.surreal,
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
    let world = setup("drive-import").await;
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
    let world = setup("drive-versions").await;
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
    let world = setup("drive-interrupted").await;
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
    let world = setup("drive-receipts").await;
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

    let malware = setup("drive-malware").await;
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
    let world = setup("drive-unauthorized").await;
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
    let world = setup("drive-target").await;
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
    let world = setup("drive-dry-run").await;
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
    let world = setup("drive-dry-refusals").await;
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
    collided.sort();
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
    let world = setup("drive-collisions").await;
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
        "both members are refused: {:?}",
        report.refused
    );
    let assets = store::assets::for_project(&world.surreal, world.project.id)
        .await
        .expect("asset listing");
    assert!(assets.is_empty(), "nothing silently merged");
}

#[tokio::test]
async fn a_name_that_cannot_be_a_checkout_document_is_refused_before_download() {
    let world = setup("drive-unusable-names").await;
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

    let report = world.import(&Screen::default()).await.expect("import runs");
    assert!(report.imported.is_empty());
    assert_eq!(report.refused[0].reason, "unusable_name");
    assert_eq!(
        world.drive.download_calls(),
        0,
        "an unusable name is refused before its bytes move"
    );
}

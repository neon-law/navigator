//! Drive ingest into Navigator storage: receipts, versions, retries, screening,
//! and folder scoping, all against a synthetic Drive and object store.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use cloud::{DriveService, FakeDrive, FsStorage, StorageService};
use serde_json::json;
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

    async fn revisions(&self, file_id: &str) -> Vec<store::assets::Asset> {
        store::assets::revisions(&self.surreal, self.project.id, &format!("drive-{file_id}"))
            .await
            .expect("revision chain")
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
    assert_eq!(world.revisions(&file.id).await.len(), 1);
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
    let chain = world.revisions(&file.id).await;
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
    let first = world
        .drive
        .add_file(
            &world.folder_id,
            "a.pdf",
            "application/pdf",
            b"alpha".to_vec(),
            None,
        )
        .expect("file a");
    let second = world
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
    assert_eq!(world.revisions(&first.id).await.len(), 1);
    assert_eq!(world.revisions(&second.id).await.len(), 1);
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
    assert!(world.revisions(&file.id).await.is_empty());

    let malware = setup("drive-malware").await;
    let infected = malware
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
    assert!(malware.revisions(&infected.id).await.is_empty());

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

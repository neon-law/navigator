//! The persisted document `folder` (LAW-241).
//!
//! A pointer's `folder:` key is the lawyer's stated intent, and the live
//! matter only honors it when the asset row carries the same value. What
//! these tests pin: a folder named at ingest persists and reads back, an
//! unnamed one stays `None`, an unknown value is refused before any byte is
//! stored, and `store::assets::set_folder` sets, clears, reports no-change,
//! and refuses a document that is not on the given matter.

use store::assets::{find_by_id, for_project, set_folder};
use store::document_pointers::MatterFolder;
use store::documents::{ingest_bytes_as, visibility, DocumentIdentity, IngestArgs};
use store::surreal::test_support::mem;
use store::test_support::seed_project_surreal;
use uuid::Uuid;

/// An engine, a filesystem storage, and one matter to file against.
async fn fixtures() -> (
    store::surreal::SurrealDb,
    std::sync::Arc<dyn cloud::StorageService>,
    tempfile::TempDir,
    Uuid,
) {
    let db = mem().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let storage: std::sync::Arc<dyn cloud::StorageService> = std::sync::Arc::new(
        cloud::FsStorage::new(tmp.path().to_path_buf())
            .await
            .expect("fs storage"),
    );
    let project_id = seed_project_surreal(&db, "document-folder").await;
    (db, storage, tmp, project_id)
}

async fn file_document(
    db: &store::surreal::SurrealDb,
    storage: &std::sync::Arc<dyn cloud::StorageService>,
    project_id: Uuid,
    folder: Option<&str>,
) -> Uuid {
    let args = IngestArgs {
        project_id,
        source: store::documents::source::UPLOAD,
        filename: "agreement.pdf",
        kind: "agreement",
        content_type: "application/pdf",
        description: None,
        secondary_storage_key: None,
        visibility: visibility::INTERNAL,
    };
    let identity = DocumentIdentity {
        slug: Some("agreement"),
        folder,
        ..Default::default()
    };
    ingest_bytes_as(db, storage, &args, &identity, b"bytes")
        .await
        .expect("file the document")
        .asset_id
}

/// The folder an upload names persists on the asset row and reads back.
#[tokio::test]
async fn a_named_folder_persists_and_reads_back() {
    let (db, storage, _tmp, project_id) = fixtures().await;
    let asset_id = file_document(&db, &storage, project_id, Some("Discovery")).await;
    let asset = find_by_id(&db, asset_id)
        .await
        .expect("read the asset back")
        .expect("the asset exists");
    assert_eq!(asset.folder.as_deref(), Some("Discovery"));
}

/// A folder-less upload keeps the kind's default placement: the row carries
/// no folder at all, not an empty string.
#[tokio::test]
async fn an_unnamed_folder_stays_none() {
    let (db, storage, _tmp, project_id) = fixtures().await;
    let asset_id = file_document(&db, &storage, project_id, None).await;
    let asset = find_by_id(&db, asset_id)
        .await
        .expect("read the asset back")
        .expect("the asset exists");
    assert_eq!(asset.folder, None);
}

/// A folder that is not one of the Firm's matter folders is refused before
/// anything is stored — the fails-closed rule, never a silent default.
#[tokio::test]
async fn an_unknown_folder_is_refused_and_writes_nothing() {
    let (db, storage, _tmp, project_id) = fixtures().await;
    let result = ingest_bytes_as(
        &db,
        &storage,
        &IngestArgs {
            project_id,
            source: store::documents::source::UPLOAD,
            filename: "agreement.pdf",
            kind: "agreement",
            content_type: "application/pdf",
            description: None,
            secondary_storage_key: None,
            visibility: visibility::INTERNAL,
        },
        &DocumentIdentity {
            slug: Some("agreement"),
            folder: Some("Left Drawer"),
            ..Default::default()
        },
        b"bytes",
    )
    .await;
    assert!(
        matches!(
            &result,
            Err(store::documents::IngestError::UnknownFolder(_))
        ),
        "an unknown folder must be refused: {result:?}"
    );
    assert!(
        for_project(&db, project_id)
            .await
            .expect("list the matter's documents")
            .is_empty(),
        "a refused folder writes no asset row"
    );
}

/// `set_folder` moves a row into a folder, then an explicit clear returns it
/// to the kind's default placement; the read-back follows each write.
#[tokio::test]
async fn set_then_clear_moves_the_row() {
    let (db, storage, _tmp, project_id) = fixtures().await;
    let asset_id = file_document(&db, &storage, project_id, None).await;

    let set = set_folder(&db, project_id, asset_id, Some("Discovery"))
        .await
        .expect("set the folder");
    assert_eq!(set, Some(true), "a set from no folder is a change");
    let asset = find_by_id(&db, asset_id)
        .await
        .expect("read the asset back")
        .expect("the asset exists");
    assert_eq!(asset.folder.as_deref(), Some("Discovery"));

    let cleared = set_folder(&db, project_id, asset_id, None)
        .await
        .expect("clear the folder");
    assert_eq!(cleared, Some(true), "a clear from a folder is a change");
    let asset = find_by_id(&db, asset_id)
        .await
        .expect("read the asset back")
        .expect("the asset exists");
    assert_eq!(asset.folder, None);
}

/// Setting the folder the row already sits in changes nothing — the
/// idempotence `project sync` relies on to skip finished work.
#[tokio::test]
async fn setting_the_same_folder_changes_nothing() {
    let (db, storage, _tmp, project_id) = fixtures().await;
    let asset_id = file_document(&db, &storage, project_id, Some("Discovery")).await;
    let unchanged = set_folder(&db, project_id, asset_id, Some("Discovery"))
        .await
        .expect("set the same folder");
    assert_eq!(unchanged, Some(false), "the row already sits in Discovery");
}

/// A folder write is scoped to the matter: an asset that is not on the
/// given Project is not found, and an unknown folder name is refused with
/// the accepted values spelled out.
#[tokio::test]
async fn a_folder_write_is_scoped_and_fails_closed() {
    let (db, storage, _tmp, project_id) = fixtures().await;
    let asset_id = file_document(&db, &storage, project_id, None).await;

    let absent = set_folder(&db, Uuid::now_v7(), asset_id, Some("Discovery"))
        .await
        .expect("set the folder on another matter");
    assert_eq!(absent, None, "the asset is not on that matter");

    let refused = set_folder(&db, project_id, asset_id, Some("Left Drawer")).await;
    let error = refused.expect_err("an unknown folder is refused");
    assert!(
        error.to_string().contains("Left Drawer"),
        "the refusal names the value: {error}"
    );
    assert!(
        error.to_string().contains(MatterFolder::Discovery.as_str()),
        "the refusal names the accepted values: {error}"
    );
}

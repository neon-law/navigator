//! Import receipt and identity boundaries independent of provider enumeration.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct Clean(AtomicUsize);
#[async_trait::async_trait]
impl IngestScreen for Clean {
    async fn is_clean(&self, _bytes: &[u8]) -> Result<bool, ()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(true)
    }
}
fn file() -> cloud::DriveFile {
    cloud::DriveFile {
        id: "source-id".into(),
        name: "notice.pdf".into(),
        mime_type: "application/pdf".into(),
        size_bytes: Some(4),
        modified_time: None,
        md5_checksum: None,
        version: Some("v1".into()),
    }
}
#[test]
fn native_file_receipts_without_checksums_still_obey_size_and_batch_limits() {
    let mut receipt = file();
    assert!(receipt_matches(&receipt, b"same"));
    receipt.size_bytes = Some(5);
    assert!(!receipt_matches(&receipt, b"same"));
    receipt.size_bytes = None;
    assert!(receipt_matches(&receipt, b"same"));
    receipt.md5_checksum = Some(cloud::md5_hex(b"same").to_ascii_uppercase());
    assert!(receipt_matches(&receipt, b"same"));
    receipt.md5_checksum = Some(cloud::md5_hex(b"other"));
    assert!(!receipt_matches(&receipt, b"same"));
    assert!(validate_declared_size("native", None, MAX_TOTAL_BYTES).is_ok());
    assert!(validate_declared_size(
        "boundary",
        Some(MAX_FILE_BYTES as u64),
        MAX_TOTAL_BYTES - MAX_FILE_BYTES
    )
    .is_ok());
    assert!(
        matches!(validate_declared_size("oversized",Some(MAX_FILE_BYTES as u64+1),0),Err(DriveImportError::FileTooLarge{actual,..}) if actual==MAX_FILE_BYTES as u64+1)
    );
    assert!(matches!(
        validate_declared_size("batch", Some(1), MAX_TOTAL_BYTES),
        Err(DriveImportError::BatchTooLarge)
    ));
    assert!(matches!(
        validate_declared_size("overflow", Some(1), usize::MAX),
        Err(DriveImportError::BatchTooLarge)
    ));
}
#[test]
fn opaque_provider_id_and_checkout_extension_bounds_never_fabricate_an_identity() {
    let mut item = file();
    item.id = String::new();
    assert!(reserved_drive_slug(&item).is_none());
    item.id = "x".repeat(121);
    assert!(reserved_drive_slug(&item).is_none());
    item.id = "x".repeat(120);
    assert!(reserved_drive_slug(&item).is_some());
    item.name = "README".into();
    assert!(reserved_drive_slug(&item).is_none());
    item.name = format!("notice.{}", "x".repeat(20));
    assert!(reserved_drive_slug(&item).is_none());
}
#[tokio::test]
async fn octet_stream_download_uses_listed_mime_and_dry_kind_changes_do_not_rewrite_the_head() {
    let db = crate::test_support::mem_surreal().await;
    let project = crate::test_support::seed_project(&db, "Synthetic Import Receipt").await;
    let directory = tempfile::tempdir().unwrap();
    let storage: Arc<dyn StorageService> =
        Arc::new(cloud::FsStorage::new(directory.path()).await.unwrap());
    let screen = Clean::default();
    let args = DriveImportArgs {
        kind: "unclassified",
        visibility: documents::visibility::CLIENT,
        description: None,
        dry_run: false,
    };
    let downloaded = cloud::DriveFileDownload {
        bytes: b"same".to_vec(),
        content_type: "application/octet-stream".into(),
    };
    let outcome = ingest_one(
        &db,
        &storage,
        &screen,
        project.id,
        &args,
        file(),
        &downloaded,
        "stable.pdf",
        "Work Product/notice.pdf",
        "notice.pdf",
        "synthetic-root",
    )
    .await
    .unwrap();
    let Outcome::Imported(imported) = outcome else {
        panic!("valid receipt must file")
    };
    let head = assets::find_by_id(&db, imported.asset_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(head.content_type, "application/pdf");
    assert_eq!(screen.0.load(Ordering::SeqCst), 1);
    let changed = DriveImportArgs {
        kind: "letter",
        dry_run: true,
        ..args
    };
    assert!(matches!(
        plan_one(
            &db,
            project.id,
            &changed,
            file(),
            b"same",
            MatterFolder::WorkProduct,
            "stable.pdf",
            "notice.pdf"
        )
        .await,
        Err(DriveImportError::Asset(RevisionError::KindChanged { .. }))
    ));
    let same = plan_one(
        &db,
        project.id,
        &args,
        file(),
        b"same",
        MatterFolder::WorkProduct,
        "stable.pdf",
        "notice.pdf",
    )
    .await
    .unwrap();
    assert!(!same.changed);
    assert_eq!(same.asset_id, head.id);
    let new = plan_one(
        &db,
        project.id,
        &args,
        file(),
        b"new",
        MatterFolder::WorkProduct,
        "stable.pdf",
        "notice.pdf",
    )
    .await
    .unwrap();
    assert!(new.changed);
    assert!(new.asset_id.is_nil());
    assert_eq!(
        assets::revisions(&db, project.id, "stable.pdf")
            .await
            .unwrap()
            .len(),
        1
    );
}

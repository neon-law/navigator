//! Import receipt and identity boundaries independent of provider enumeration.
use super::*;

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

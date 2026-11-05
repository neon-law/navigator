//! Full `StorageService` contract exercised through the trait object on
//! the filesystem backend — the backend every dev, test rig, and small
//! deployment actually runs. The conformance suite in
//! `storage_conformance.rs` runs its full contract only against live S3
//! (env-gated); this suite pins the same black-box behaviors locally,
//! including the error and unsupported-operation edges.

use std::sync::Arc;

use cloud::{FsStorage, StorageError, StorageService};

#[tokio::test]
async fn filesystem_backend_honors_the_storage_contract_through_the_trait() {
    let root = tempfile::TempDir::new().unwrap();
    let storage: Arc<dyn StorageService> =
        Arc::new(FsStorage::new(root.path().to_str().unwrap()).await.unwrap());

    // Missing-key edges answer, they do not panic: get is NotFound,
    // head and exists report absence.
    let missing = "contract/absent";
    assert!(matches!(
        storage.get(missing).await,
        Err(StorageError::NotFound(key)) if key == missing
    ));
    assert!(!storage.exists(missing).await.unwrap());
    assert!(storage.head(missing).await.unwrap().is_none());

    // put_cached stores text with its content type; put stores binary
    // bytes intact.
    let text_key = "contract/a.txt";
    let bin_key = "contract/b.bin";
    storage
        .put_cached(text_key, b"fs-contract", "text/plain", "public, max-age=60")
        .await
        .expect("put cached object");
    storage
        .put(bin_key, b"\0\x01\x02", "application/octet-stream")
        .await
        .expect("put binary object");

    assert!(storage.exists(text_key).await.unwrap());
    let text = storage.get(text_key).await.unwrap();
    assert_eq!(text.bytes, b"fs-contract");
    assert_eq!(text.content_type, "text/plain");
    let binary = storage.get(bin_key).await.unwrap();
    assert_eq!(binary.bytes, b"\0\x01\x02");

    // head reports the size without a backend notion of metadata — the
    // default reads through get.
    let head = storage.head(text_key).await.unwrap().expect("head");
    assert_eq!(
        head.size_bytes,
        u64::try_from(b"fs-contract".len()).unwrap()
    );

    // Overwrite semantics: a second put replaces the object completely.
    storage
        .put(text_key, b"replaced", "text/plain")
        .await
        .expect("overwrite");
    assert_eq!(storage.get(text_key).await.unwrap().bytes, b"replaced");

    // list returns keys and byte sizes under a prefix, without order
    // guarantees.
    let listed = storage.list("contract/").await.expect("list prefix");
    let mut entries: Vec<(String, u64)> = listed
        .into_iter()
        .map(|entry| (entry.key, entry.size_bytes))
        .collect();
    entries.sort();
    assert_eq!(
        entries,
        vec![
            (text_key.to_string(), b"replaced".len() as u64),
            (bin_key.to_string(), 3),
        ]
    );
    assert!(storage
        .list("contract/nothing-here/")
        .await
        .unwrap()
        .is_empty());

    // Operations the filesystem backend cannot answer fail explicitly
    // with Unsupported, so callers fall back (e.g. to streaming bytes).
    assert!(matches!(
        storage
            .signed_url(text_key, std::time::Duration::from_secs(60))
            .await,
        Err(StorageError::Unsupported(_))
    ));
    assert!(matches!(
        storage.put_immutable(text_key, b"x", "text/plain").await,
        Err(StorageError::Unsupported(_))
    ));
    assert!(matches!(
        storage.get_generation(text_key, 1).await,
        Err(StorageError::Unsupported(_))
    ));
    // No bucket policy surface → "not locked", not an error.
    assert!(!storage.retention_locked().await.unwrap());

    // delete is idempotent, and deletion empties the object out of every
    // read path.
    storage.delete(text_key).await.expect("delete");
    storage
        .delete(text_key)
        .await
        .expect("delete is idempotent");
    assert!(storage.head(text_key).await.unwrap().is_none());
    assert!(!storage.exists(text_key).await.unwrap());
}

use super::*;
use std::{collections::BTreeMap, sync::Mutex};

use cloud::{ImmutableObject, ObjectListing, StorageError, StoredObject};
use futures::TryStreamExt as _;

#[derive(Default)]
pub(crate) struct CreateOnly {
    pub(crate) objects: Mutex<BTreeMap<String, Vec<u8>>>,
    // Fail after committing this index, simulating a server success/lost ACK.
    pub(crate) lost_ack: Option<usize>,
}

#[async_trait::async_trait]
impl StorageService for CreateOnly {
    async fn put(&self, _: &str, _: &[u8], _: &str) -> Result<(), StorageError> {
        panic!("writer attempted overwrite")
    }
    async fn get(&self, _: &str) -> Result<StoredObject, StorageError> {
        panic!("writer attempted read")
    }
    async fn head(&self, _: &str) -> Result<Option<cloud::ObjectHead>, StorageError> {
        panic!("writer attempted head")
    }
    async fn exists(&self, _: &str) -> Result<bool, StorageError> {
        panic!("writer attempted existence read")
    }
    async fn get_generation(
        &self,
        _: &str,
        _: u64,
    ) -> Result<cloud::VersionedObject, StorageError> {
        panic!("writer attempted generation read")
    }
    async fn list(&self, _: &str) -> Result<Vec<ObjectListing>, StorageError> {
        panic!("writer attempted list")
    }
    async fn delete(&self, _: &str) -> Result<(), StorageError> {
        panic!("writer attempted delete")
    }
    async fn signed_url(&self, _: &str, _: std::time::Duration) -> Result<String, StorageError> {
        panic!("writer attempted signed read")
    }
    async fn retention_locked(&self) -> Result<bool, StorageError> {
        panic!("writer attempted bucket-policy read")
    }
    async fn bucket_project_number(&self) -> Result<i64, StorageError> {
        panic!("writer attempted project read")
    }
    async fn put_immutable(
        &self,
        key: &str,
        bytes: &[u8],
        _: &str,
    ) -> Result<ImmutableObject, StorageError> {
        let mut objects = self.objects.lock().unwrap();
        if objects.contains_key(key) {
            return Err(StorageError::Unsupported(
                "synthetic create precondition failed",
            ));
        }
        objects.insert(key.to_owned(), bytes.to_vec());
        if self.lost_ack == Some(objects.len() - 1) {
            return Err(StorageError::Unsupported("synthetic lost acknowledgement"));
        }
        Ok(ImmutableObject {
            generation: objects.len() as u64,
            size_bytes: bytes.len() as u64,
        })
    }
}

fn sample() -> Capture {
    let rows = [serde_json::json!({"id":"synthetic", "name":"frozen"})];
    let batch = crate::batch_from_rows(&rows).unwrap().unwrap();
    Capture(
        crate::postgres::Extracted {
            cutoff_at: chrono::Utc::now(),
            snapshot: "synthetic-unit-snapshot".to_owned(),
            server_version: "synthetic-unit-version".to_owned(),
            tables: vec![(
                "probe".to_owned(),
                1,
                crate::encode_parquet(&batch).unwrap(),
            )],
        },
        None,
    )
}

#[tokio::test]
async fn destinations_receive_independent_complete_iceberg_closures_without_any_reads() {
    let capture = sample();
    for root in [
        "gs://example-independent-project-one",
        "gs://example-independent-project-two",
    ] {
        let bundle = capture.bundle(root).await.unwrap();
        let writer = CreateOnly::default();
        let ack = bundle.upload(&writer).await.unwrap();
        assert_eq!(ack.inventory_key, bundle.descriptor);
        assert_eq!(ack.inventory_uri, format!("{root}/{}", bundle.descriptor));
        let objects = writer.objects.lock().unwrap().clone();
        let inventory: Inventory = serde_json::from_slice(&objects[&ack.inventory_key]).unwrap();
        assert_eq!(objects.len(), inventory.objects.len() + 1);
        assert!(objects
            .keys()
            .all(|key| !key.contains("version-hint") && !key.contains("latest")));
        let io = iceberg::io::FileIO::new_with_memory();
        for entry in &inventory.objects {
            assert_eq!(
                entry.sha256,
                crate::postgres::sha256_hex(&objects[&entry.key])
            );
            assert_eq!(entry.bytes, objects[&entry.key].len());
            io.new_output(format!("{root}/{}", entry.key))
                .unwrap()
                .write(objects[&entry.key].clone().into())
                .await
                .unwrap();
        }
        drop(objects);
        assert_eq!(read_rows(io, &inventory.metadata[0]).await, 1);
    }
}

#[tokio::test]
async fn partial_failure_and_lost_inventory_ack_never_become_a_success_or_remote_read() {
    let capture = sample();
    let successful = capture
        .bundle("gs://example-successful-project")
        .await
        .unwrap();
    successful.upload(&CreateOnly::default()).await.unwrap();
    let failed = capture
        .bundle("gs://example-unconfirmed-project")
        .await
        .unwrap();
    for index in [1, failed.files.len()] {
        let writer = CreateOnly {
            lost_ack: Some(index),
            ..CreateOnly::default()
        };
        let error = failed.upload(&writer).await.unwrap_err();
        assert_eq!(error.acknowledged, index);
        let before = writer.objects.lock().unwrap().clone();
        assert_eq!(
            before.contains_key(&failed.descriptor),
            index == failed.files.len()
        );
        // Reusing the same attempt refuses; it cannot replace objects or infer success.
        assert_eq!(failed.upload(&writer).await.unwrap_err().acknowledged, 0);
        assert_eq!(*writer.objects.lock().unwrap(), before);
    }
}

async fn read_batches(io: iceberg::io::FileIO, metadata: &str) -> Vec<arrow_array58::RecordBatch> {
    let table = iceberg::table::StaticTable::from_metadata_file(
        metadata,
        iceberg::TableIdent::from_strs(["write_only", "probe"]).unwrap(),
        io,
    )
    .await
    .unwrap();
    let tasks = table.scan().build().unwrap().plan_files().await.unwrap();
    let batches: Vec<_> = table
        .reader_builder()
        .build()
        .read(tasks)
        .unwrap()
        .stream()
        .try_collect()
        .await
        .unwrap();
    batches
}

async fn read_rows(io: iceberg::io::FileIO, metadata: &str) -> usize {
    read_batches(io, metadata)
        .await
        .iter()
        .map(arrow_array58::RecordBatch::num_rows)
        .sum()
}

#[tokio::test]
async fn real_pg_snapshot_is_readable_as_raw_local_iceberg_and_hashes_detect_corruption() {
    store::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO person(id,name,email) VALUES($1,'Frozen archive person',$2)")
            .bind(id)
            .bind(format!("{id}@example.test"))
            .execute(pg.pool())
            .await
            .unwrap();
        let capture = Capture::navigator(&pg).await.unwrap();
        sqlx::query("DELETE FROM person WHERE id=$1")
            .bind(id)
            .execute(pg.pool())
            .await
            .unwrap();
        let local = tempfile::tempdir().unwrap();
        let canonical = local.path().canonicalize().unwrap();
        let bundle = capture
            .bundle(&format!("file://{}", canonical.display()))
            .await
            .unwrap();
        bundle.write_local(&canonical).await.unwrap();
        bundle.verify_local(&canonical).await.unwrap();
        let inventory: Inventory = serde_json::from_slice(
            &tokio::fs::read(canonical.join(&bundle.descriptor))
                .await
                .unwrap(),
        )
        .unwrap();
        let person = capture
            .0
            .tables
            .iter()
            .find(|(table, _, _)| table == "person")
            .unwrap();
        assert!(person.1 > 0);
        let metadata = inventory
            .metadata
            .iter()
            .find(|uri| uri.contains("/person/"))
            .unwrap();
        assert_eq!(
            read_rows(iceberg::io::FileIO::new_with_fs(), metadata).await,
            person.1
        );
        let batches = read_batches(iceberg::io::FileIO::new_with_fs(), metadata).await;
        assert!(batches.iter().any(|batch| {
            let names = batch
                .column_by_name("name")
                .unwrap()
                .as_any()
                .downcast_ref::<arrow_array58::StringArray>()
                .unwrap();
            names
                .iter()
                .flatten()
                .any(|name| name == "Frozen archive person")
        }));
        let live: i64 = sqlx::query_scalar("SELECT count(*) FROM person WHERE id=$1")
            .bind(id)
            .fetch_one(pg.pool())
            .await
            .unwrap();
        assert_eq!(
            live, 0,
            "local Iceberg preserves the frozen row after live deletion"
        );
        for entry in &inventory.objects {
            let bytes = tokio::fs::read(canonical.join(&entry.key)).await.unwrap();
            assert_eq!(entry.bytes, bytes.len());
            assert_eq!(entry.sha256, crate::postgres::sha256_hex(&bytes));
        }
        let entry = inventory
            .objects
            .iter()
            .find(|entry| entry.key.ends_with(".parquet"))
            .unwrap();
        tokio::fs::write(canonical.join(&entry.key), b"synthetic corruption")
            .await
            .unwrap();
        assert!(bundle.verify_local(&canonical).await.is_err());
        assert_ne!(
            entry.sha256,
            crate::postgres::sha256_hex(
                &tokio::fs::read(canonical.join(&entry.key)).await.unwrap()
            )
        );
        assert!(
            bundle.write_local(&canonical).await.is_err(),
            "cannot overwrite local attempt"
        );
    })
    .await;
}

#[tokio::test]
async fn closure_rejects_foreign_metadata_before_any_reader_or_network_exists() {
    let mut bundle = sample()
        .bundle("gs://example-bound-project-bucket")
        .await
        .unwrap();
    super::closure::validate(
        &bundle.inventory,
        &bundle.root,
        &bundle.descriptor,
        &bundle.files,
    )
    .unwrap();
    let (key, bytes, _) = bundle
        .files
        .iter_mut()
        .find(|(key, _, _)| key.ends_with("v1.metadata.json"))
        .unwrap();
    let mut metadata: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    metadata["snapshots"][0]["manifest-list"] =
        serde_json::json!("file:///private/synthetic-unowned.avro");
    *bytes = serde_json::to_vec(&metadata).unwrap();
    let declared = bundle
        .inventory
        .objects
        .iter_mut()
        .find(|entry| entry.key == *key)
        .unwrap();
    declared.sha256 = crate::postgres::sha256_hex(bytes);
    declared.bytes = bytes.len();
    assert!(super::closure::validate(
        &bundle.inventory,
        &bundle.root,
        &bundle.descriptor,
        &bundle.files
    )
    .is_err());
}

#[tokio::test]
async fn closure_rejects_foreign_avro_data_uri_and_false_row_count_before_scan() {
    for (path, count) in [
        ("gs://example-foreign-project/attempts/unowned.parquet", 1),
        ("bound", 2),
    ] {
        let mut bundle = sample()
            .bundle("gs://example-bound-project-bucket")
            .await
            .unwrap();
        let (data_key, bytes, _) = bundle
            .files
            .iter()
            .find(|(key, _, _)| key.ends_with(".parquet"))
            .unwrap();
        let location = format!(
            "{}/{}",
            bundle.root,
            data_key.strip_suffix("/data/snapshot.parquet").unwrap()
        );
        let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(
            bytes::Bytes::copy_from_slice(bytes),
        )
        .unwrap();
        let files = [super::DataFileSpec {
            path: if path == "bound" {
                format!("{}/{data_key}", bundle.root)
            } else {
                path.to_owned()
            },
            record_count: count,
            file_size_in_bytes: bytes.len() as u64,
        }];
        let authored = super::author_snapshot(&super::SnapshotInput {
            table: "probe",
            arrow_schema: reader.schema(),
            table_location: &location,
            data_files: &files,
            snapshot_id: 1,
            sequence_number: 1,
            timestamp_ms: bundle.inventory.cutoff_at.timestamp_millis(),
            version: 1,
            prior: None,
            expire_before_ms: None,
        })
        .await
        .unwrap();
        bundle.files.retain(|(key, _, _)| key.ends_with(".parquet"));
        bundle
            .inventory
            .objects
            .retain(|object| object.key.ends_with(".parquet"));
        let key = location
            .strip_prefix(&format!("{}/", bundle.root))
            .unwrap()
            .to_owned();
        bundle.inventory.metadata = vec![authored.metadata_location];
        for (relative, bytes) in authored.files {
            if relative != "metadata/version-hint.text" {
                bundle.add(format!("{key}/{relative}"), bytes, "application/avro");
            }
        }
        let error = super::closure::validate(
            &bundle.inventory,
            &bundle.root,
            &bundle.descriptor,
            &bundle.files,
        )
        .unwrap_err();
        assert!(error.to_string().contains(if path == "bound" {
            "row count"
        } else {
            "foreign"
        }));
    }
}

#[tokio::test]
async fn structured_copy_records_real_asset_reference_without_claiming_missing_blob_recovery() {
    store::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let blobs = tempfile::tempdir().unwrap();
        let storage: std::sync::Arc<dyn cloud::StorageService> =
            std::sync::Arc::new(cloud::FsStorage::new(blobs.path()).await.unwrap());
        let bytes = b"synthetic external document payload";
        let sha = crate::postgres::sha256_hex(bytes);
        store::assets::ingest_content(
            &store::application_store::ApplicationStore::uninitialized(),
            &storage,
            bytes,
            "text/plain",
        )
        .await
        .unwrap();
        let capture = Capture::navigator(&pg).await.unwrap();
        let local = tempfile::tempdir().unwrap();
        let root = local.path().canonicalize().unwrap();
        let bundle = capture
            .bundle(&format!("file://{}", root.display()))
            .await
            .unwrap();
        bundle.write_local(&root).await.unwrap();
        let reference = bundle
            .inventory
            .dependencies
            .referenced
            .iter()
            .find(|reference| {
                reference.column == "storage_key"
                    && reference.expected_sha256.as_deref() == Some(sha.as_str())
            })
            .expect("the actual frozen Asset reference must be inventoried");
        assert_eq!(reference.expected_size, Some(bytes.len() as u64));
        assert_eq!(
            storage
                .get(&reference.object_or_repository)
                .await
                .unwrap()
                .bytes,
            bytes.to_vec()
        );
        assert!(bundle
            .inventory
            .dependencies
            .copied_external_payloads
            .is_empty());
        assert!(!bundle.inventory.dependencies.references_exhaustive);
        drop(storage);
        blobs.close().unwrap();
        bundle.verify_local(&root).await.unwrap();
        let metadata = bundle
            .inventory
            .metadata
            .iter()
            .find(|uri| uri.contains("/asset/"))
            .unwrap();
        assert!(read_rows(iceberg::io::FileIO::new_with_fs(), metadata).await > 0);
        assert!(bundle
            .inventory
            .dependencies
            .missing
            .iter()
            .any(|gap| gap.contains("referenced, not copied")));
    })
    .await;
}

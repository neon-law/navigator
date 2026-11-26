#![cfg(test)]
//! Real PostgreSQL extraction and publication across two synthetic owned databases.
//! This is not a claim about a deployed Forgejo schema or physical recovery.
use super::*;
use crate::coverage::{export_pair, inventory, CoverageManifest, DatabaseSource, Rule};
use crate::runner::snapshot_selected;
use crate::test_support::CollisionStorage;
use cloud::{ImmutableObject, ObjectListing, StorageError, StorageService, StoredObject};
use futures::{FutureExt as _, TryStreamExt as _};
use sqlx::PgPool;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

async fn with_pair<F, Fut>(test: F)
where
    F: FnOnce(PgPool, PgPool) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    store::test_support::with_postgres(async {
        let navigator = store::postgres::PostgresDb::application().await.unwrap();
        let companion = store::test_support::PostgresFixture::create_unactivated(
            navigator.pool().connect_options().as_ref().clone(),
        ).await.unwrap();
        let result = std::panic::AssertUnwindSafe(async {
            sqlx::raw_sql("CREATE SCHEMA forge; CREATE TABLE forge.repository(id bigint PRIMARY KEY, name text); INSERT INTO forge.repository VALUES(1,'Synthetic repository'); CREATE TABLE forge.empty(id bigint)")
                .execute(companion.postgres().pool()).await.unwrap();
            test(navigator.pool().clone(), companion.postgres().pool().clone()).await;
        }).catch_unwind().await;
        companion.close().await.unwrap();
        if let Err(panic) = result { std::panic::resume_unwind(panic); }
    }).await;
}

async fn reviewed(pool: &PgPool) -> CoverageManifest {
    let mut tx = pool.begin().await.unwrap();
    let relations = inventory(&mut tx)
        .await
        .unwrap()
        .into_iter()
        .map(
            |relation| match (relation.schema.as_str(), relation.name.as_str()) {
                ("public", "person_directory" | "firm_visible_person" | "project_list") => {
                    Rule::Derived {
                        relation,
                        reason: "recreated by the canonical Navigator schema".into(),
                    }
                }
                ("public", "geography_columns" | "geometry_columns") => Rule::Derived {
                    relation,
                    reason: "recreated by PostGIS".into(),
                },
                _ => Rule::Export(relation),
            },
        )
        .collect();
    tx.rollback().await.unwrap();
    CoverageManifest { relations }
}

async fn paired(
    nav: &PgPool,
    companion: &PgPool,
    storage: &dyn StorageService,
) -> anyhow::Result<crate::coverage::PairedManifest> {
    let navigator = reviewed(nav).await;
    let forgejo = reviewed(companion).await;
    export_pair(
        DatabaseSource {
            name: "navigator",
            pool: nav,
            coverage: &navigator,
        },
        DatabaseSource {
            name: "forgejo",
            pool: companion,
            coverage: &forgejo,
        },
        storage,
        "gs://synthetic-archives",
    )
    .await
}

struct FaultStorage {
    inner: Arc<CollisionStorage>,
    deny_policy: bool,
    fail_data_once: AtomicBool,
    data_writes: AtomicUsize,
}
impl FaultStorage {
    fn new(deny_policy: bool, fail_data_once: bool) -> Self {
        Self {
            inner: CollisionStorage::locked(),
            deny_policy,
            fail_data_once: AtomicBool::new(fail_data_once),
            data_writes: AtomicUsize::new(0),
        }
    }
}
#[async_trait::async_trait]
impl StorageService for FaultStorage {
    async fn put(&self, key: &str, bytes: &[u8], content_type: &str) -> Result<(), StorageError> {
        self.inner.put(key, bytes, content_type).await
    }
    async fn put_immutable(
        &self,
        key: &str,
        bytes: &[u8],
        content_type: &str,
    ) -> Result<ImmutableObject, StorageError> {
        if key.contains("/data/")
            && self.data_writes.fetch_add(1, Ordering::SeqCst) == 1
            && self.fail_data_once.swap(false, Ordering::SeqCst)
        {
            return Err(StorageError::Unsupported(
                "synthetic interrupted Parquet upload",
            ));
        }
        self.inner.put_immutable(key, bytes, content_type).await
    }
    async fn get(&self, key: &str) -> Result<StoredObject, StorageError> {
        self.inner.get(key).await
    }
    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        self.inner.delete(key).await
    }
    async fn signed_url(
        &self,
        key: &str,
        ttl: std::time::Duration,
    ) -> Result<String, StorageError> {
        self.inner.signed_url(key, ttl).await
    }
    async fn list(&self, prefix: &str) -> Result<Vec<ObjectListing>, StorageError> {
        self.inner.list(prefix).await
    }
    async fn retention_locked(&self) -> Result<bool, StorageError> {
        if self.deny_policy {
            Err(StorageError::Gcs {
                key: "<bucket-policy>".into(),
                message: "storage.buckets.get denied".into(),
            })
        } else {
            Ok(true)
        }
    }
}

async fn summary(
    storage: &dyn StorageService,
    outcome: anyhow::Result<crate::coverage::PairedManifest>,
) -> SnapshotSummary {
    let mut summary = snapshot_selected(None, storage, None).await;
    summary.postgres = Some(match outcome {
        Ok(value) => PostgresExport::Paired(value),
        Err(error) => PostgresExport::Failed {
            error: format!("{error:#}"),
        },
    });
    summary
}

#[tokio::test]
async fn denied_retention_preflight_exports_no_bytes_and_reports_failure() {
    with_pair(|nav, companion| async move {
        let storage = FaultStorage::new(true, false);
        let outcome = paired(&nav, &companion, &storage).await;
        assert!(outcome.is_err());
        assert!(storage.inner.keys().is_empty());
        let summary = summary(&storage, outcome).await;
        assert!(matches!(
            run_report(&summary, "synthetic"),
            Err(RunFailure::Postgres)
        ));
        let digest = digest_message(&summary, |key| {
            (key == "NAVIGATOR_ARCHIVES_BUCKET").then(|| "synthetic-archives".into())
        })
        .unwrap();
        assert!(
            digest.starts_with("⚠️ *Iceberg data export FAILED"),
            "{digest}"
        );
        assert!(digest.contains("storage.buckets.get denied"), "{digest}");
    })
    .await;
}

#[tokio::test]
async fn locked_paired_exports_preserve_previous_parquet_and_iceberg_generations() {
    with_pair(|nav, companion| async move {
        let storage = CollisionStorage::locked();
        let first = paired(&nav, &companion, storage.as_ref()).await.unwrap();
        let mut first_objects = Vec::new();
        for key in storage.keys() {
            let object = storage.get(&key).await.unwrap();
            first_objects.push((object.key, object.bytes));
        }
        sqlx::query("INSERT INTO forge.repository VALUES(2,'Second synthetic repository')")
            .execute(&companion)
            .await
            .unwrap();
        let second = paired(&nav, &companion, storage.as_ref()).await.unwrap();
        assert_ne!(first.generation, second.generation);
        for (key, bytes) in first_objects {
            assert_eq!(storage.get(&key).await.unwrap().bytes, bytes);
        }
        for (generation, expected) in [(&first, 1), (&second, 2)] {
            let database = generation
                .databases
                .iter()
                .find(|db| db.database == "forgejo")
                .unwrap();
            let entry = database
                .tables
                .iter()
                .find(|entry| entry.table == "forge.repository")
                .unwrap();
            assert_eq!(entry.rows, expected);
            assert_eq!(
                database
                    .tables
                    .iter()
                    .find(|entry| entry.table == "forge.empty")
                    .unwrap()
                    .rows,
                0
            );
            let table_prefix = entry.key.split_once("/data/").unwrap().0;
            let metadata_key = database
                .iceberg_metadata_keys
                .iter()
                .find(|key| key.starts_with(&format!("{table_prefix}/metadata/")))
                .unwrap();
            let metadata: iceberg::spec::TableMetadata =
                serde_json::from_slice(&storage.get(metadata_key).await.unwrap().bytes).unwrap();
            assert_eq!(metadata.snapshots().count(), 1);
            let snapshot = metadata.current_snapshot().unwrap();
            let list_key = snapshot
                .manifest_list()
                .strip_prefix("gs://synthetic-archives/")
                .unwrap();
            let bytes = storage.get(list_key).await.unwrap().bytes;
            let list = iceberg::spec::ManifestList::parse_with_version(
                &bytes,
                iceberg::spec::FormatVersion::V2,
            )
            .unwrap();
            assert_eq!(list.entries().len(), 1);
            assert_eq!(list.entries()[0].added_rows_count, Some(expected as u64));
        }
        let summary = summary(storage.as_ref(), Ok(second)).await;
        assert!(run_report(&summary, "synthetic").is_ok());
        let digest = digest_message(&summary, |key| {
            (key == "NAVIGATOR_ARCHIVES_BUCKET").then(|| "synthetic-archives".into())
        })
        .unwrap();
        assert!(digest.contains("PostgreSQL navigator:"));
        assert!(digest.contains("PostgreSQL forgejo:"));
        assert!(digest.contains("atomic recovery is unverified"));
        assert!(!storage
            .keys()
            .iter()
            .any(|key| key.ends_with("version-hint.text")));
    })
    .await;
}

#[tokio::test]
async fn interrupted_native_upload_has_no_commit_marker_and_retry_recovers() {
    with_pair(|nav, companion| async move {
        let storage = FaultStorage::new(false, true);
        let failed = paired(&nav, &companion, &storage).await;
        assert!(failed.is_err());
        assert!(
            !storage.inner.keys().is_empty(),
            "exercise a genuinely interrupted partial upload"
        );
        assert!(storage
            .inner
            .keys()
            .iter()
            .all(|key| !key.starts_with("postgres/_paired/")));
        let committed = paired(&nav, &companion, &storage).await.unwrap();
        let key = format!("postgres/_paired/{}.json", committed.generation);
        let read: crate::coverage::PairedManifest =
            serde_json::from_slice(&storage.get(&key).await.unwrap().bytes).unwrap();
        assert_eq!(read, committed);
        assert!(!read.cross_database_atomic);
    })
    .await;
}

#[tokio::test]
async fn catalog_drift_refuses_before_writes_then_reviewed_quoted_relation_exports_safely() {
    with_pair(|nav, companion| async move {
        let navigator = reviewed(&nav).await;
        let forgejo = reviewed(&companion).await;
        sqlx::raw_sql("CREATE TABLE forge.\"../archive-runtime-probe\"(id bigint PRIMARY KEY,payload text); INSERT INTO forge.\"../archive-runtime-probe\" VALUES(1,'Synthetic archive row')")
            .execute(&companion).await.unwrap();
        let storage = CollisionStorage::locked();
        let refused = export_pair(
            DatabaseSource { name:"navigator", pool:&nav, coverage:&navigator },
            DatabaseSource { name:"forgejo", pool:&companion, coverage:&forgejo },
            storage.as_ref(), "gs://synthetic-archives",
        ).await;
        assert!(refused.is_err(), "a new stored relation needs reviewed coverage");
        assert!(storage.keys().is_empty(), "refuse inventory drift before uploading client rows");
        let current = paired(&nav,&companion,storage.as_ref()).await.unwrap();
        let companion = current.databases.iter().find(|db| db.database=="forgejo").unwrap();
        let entry = companion.tables.iter().find(|entry|entry.table=="forge.../archive-runtime-probe").unwrap();
        assert_eq!(entry.rows,1);
        assert!(!entry.key.contains("../"));
        assert!(!entry.key.contains("archive-runtime-probe"), "relation names are encoded, not object path fragments");
        let object = storage.get(&entry.key).await.unwrap();
        let mut reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(object.bytes)).unwrap().build().unwrap();
        let batch = reader.next().unwrap().unwrap();
        assert_eq!(batch.num_rows(),1);
        let payload = batch.column(batch.schema().index_of("payload").unwrap()).as_any().downcast_ref::<arrow::array::StringArray>().unwrap();
        assert_eq!(payload.value(0),"Synthetic archive row");
    }).await;
}

#[tokio::test]
async fn write_only_pair_keeps_reviewed_coverage_real_sdk_rows_and_raw_local_iceberg() {
    with_pair(|navigator, forgejo| async move {
        record_real_sdk_checkpoint(&navigator).await;
        let navigator_coverage = reviewed(&navigator).await;
        let forgejo_coverage = reviewed(&forgejo).await;
        let capture = crate::write_only::Capture::paired(
            DatabaseSource {
                name: "navigator",
                pool: &navigator,
                coverage: &navigator_coverage,
            },
            DatabaseSource {
                name: "forgejo",
                pool: &forgejo,
                coverage: &forgejo_coverage,
            },
        )
        .await
        .unwrap();
        let local = tempfile::tempdir().unwrap();
        let root = local.path().canonicalize().unwrap();
        let bundle = capture
            .bundle(&format!("file://{}", root.display()))
            .await
            .unwrap();
        bundle.write_local(&root).await.unwrap();
        bundle.verify_local(&root).await.unwrap();
        let paired = bundle.inventory().paired.as_ref().unwrap();
        assert!(!paired.cross_database_atomic);
        assert_eq!(paired.databases.len(), 2);
        assert!(paired.databases[0]
            .tables
            .iter()
            .any(|table| table.table.starts_with("dbos_write_only.") && table.rows > 0));
        for source in &paired.databases {
            assert_eq!(
                source
                    .coverage
                    .relations
                    .iter()
                    .filter(|rule| matches!(rule, Rule::Export(_)))
                    .count(),
                source.tables.len()
            );
            for (entry, key) in source.tables.iter().zip(&source.iceberg_metadata_keys) {
                let table = iceberg::table::StaticTable::from_metadata_file(
                    &format!("file://{}/{key}", root.display()),
                    iceberg::TableIdent::from_strs(["write_only", "paired"]).unwrap(),
                    iceberg::io::FileIO::new_with_fs(),
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
                assert_eq!(
                    entry.rows,
                    batches
                        .iter()
                        .map(arrow_array58::RecordBatch::num_rows)
                        .sum::<usize>()
                );
            }
        }
        assert!(bundle
            .inventory()
            .dependencies
            .copied_external_payloads
            .is_empty());
        assert!(!bundle.inventory().dependencies.references_exhaustive);
        assert!(!bundle.inventory().dependencies.missing.is_empty());
    })
    .await;
}

struct OwnedWriteOnlySource {
    pg: store::postgres::PostgresDb,
    reads: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ArchivesDbosPhases for OwnedWriteOnlySource {
    async fn write_only_postgres(
        &self,
    ) -> Result<store::postgres::PostgresDb, ArchivesDbosPhaseError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        store::postgres::PostgresDb::with_application_db(self.pg.clone(), async { self.pg.clone() })
            .await
            .map_err(|_| ArchivesDbosPhaseError)
    }
    async fn snapshot(
        &self,
        _: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<SnapshotSummary, ArchivesDbosPhaseError> {
        panic!("explicit write-only request must not open the legacy storage/source phase")
    }
    async fn cost(&self) -> Result<(), ArchivesDbosPhaseError> {
        panic!("not the write-only flow")
    }
    async fn promote(&self, _: &SnapshotSummary) -> Vec<String> {
        panic!("writer cannot run legacy promotion")
    }
    fn render_digest(&self, _: &SnapshotSummary) -> Result<String, ArchivesDbosPhaseError> {
        panic!("not the write-only digest")
    }
}

fn write_only_runtime(url: &str) -> dbos::DBOS {
    let mut config = dbos::Config::new("write-only-replay", url);
    config.schema = "dbos_write_only_replay".into();
    config.app_version = Some("synthetic-write-only-replay-v1".into());
    dbos::DBOS::new(config)
}

#[tokio::test]
async fn real_write_only_sdk_restarts_join_same_receipt_and_refuse_missing_local_without_source() {
    store::test_support::with_postgres(async {
        let pg = store::postgres::PostgresDb::application().await.unwrap();
        let database: String = sqlx::query_scalar("SELECT current_database()::text")
            .fetch_one(pg.pool())
            .await
            .unwrap();
        let port = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT").unwrap();
        let url = format!("postgresql://postgres:synthetic@127.0.0.1:{port}/{database}");
        let storage = tempfile::tempdir().unwrap();
        let root = storage.path().canonicalize().unwrap();
        let config = crate::write_only::operations::Config {
            format_version: 1,
            object_store: None,
            local_root: root.clone(),
            source_project_number: None,
            source_identity_sha256: crate::write_only::operations::source_identity(pg.pool())
                .await
                .unwrap(),
            capture: crate::write_only::operations::CaptureScope::RegisteredNavigator,
            destinations: Vec::new(),
        };
        let request = crate::write_only::operations::Request {
            capture_id: None,
            delivery_attempt: 0,
            config_sha256: config.digest().unwrap(),
            config,
            reuse_local: None,
        };
        let reads = Arc::new(AtomicUsize::new(0));
        let phases = Arc::new(OwnedWriteOnlySource {
            pg: pg.clone(),
            reads: Arc::clone(&reads),
        });
        let slack = Arc::new(workflows::CapturingSlackBot::new());
        let first = write_only_runtime(&url);
        let workflow = register_dbos_with(
            &first,
            slack.clone(),
            "synthetic-archives".into(),
            true,
            phases.clone(),
        )
        .unwrap();
        let input = RunRequest {
            scheduled_for: None,
            write_only: Some(request.clone()),
        };
        let saved = drain_sdk(&first, async {
            first.launch().await.unwrap();
            let result = workflow
                .run_with(
                    input.clone(),
                    dbos::RunOptions {
                        workflow_id: Some("write-only-stable-id"),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            let saved = result.write_only.unwrap();
            assert!(saved.all_destinations_acknowledged);
            assert!(!saved.catalog_promoted);
            assert_eq!(result.tables, crate::postgres::POSTGRES_TABLES.len());
            saved
        })
        .await;
        pg.pool().close().await;
        let second = write_only_runtime(&url);
        let workflow = register_dbos_with(
            &second,
            slack.clone(),
            "synthetic-archives".into(),
            true,
            phases,
        )
        .unwrap();
        drain_sdk(&second, async {
            second.launch().await.unwrap();
            assert_closed_source_replay(&workflow, input, saved, &reads, &slack, &root).await;
        })
        .await;
    })
    .await;
}

async fn drain_sdk<T>(runtime: &dbos::DBOS, body: impl std::future::Future<Output = T>) -> T {
    let result = std::panic::AssertUnwindSafe(body).catch_unwind().await;
    runtime.shutdown().await;
    match result {
        Ok(value) => value,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

async fn assert_closed_source_replay(
    workflow: &ArchivesDbosWorkflow,
    input: RunRequest,
    saved: crate::write_only::operations::Report,
    reads: &AtomicUsize,
    slack: &workflows::CapturingSlackBot,
    root: &std::path::Path,
) {
    let request = input.write_only.as_ref().unwrap().clone();
    let replay = workflow
        .run_with(
            input,
            dbos::RunOptions {
                workflow_id: Some("write-only-stable-id"),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        replay.write_only.unwrap().local.inventory_sha256,
        saved.local.inventory_sha256
    );
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!(slack.posted_messages().len(), 1);
    let count = std::fs::read_dir(root.join("attempts")).unwrap().count();
    tokio::fs::remove_file(root.join(&saved.local.inventory_key))
        .await
        .unwrap();
    let retry = RunRequest {
        scheduled_for: None,
        write_only: Some(crate::write_only::operations::Request {
            capture_id: None,
            delivery_attempt: 0,
            reuse_local: Some(saved.local),
            ..request
        }),
    };
    let refused = workflow
        .run_with(
            retry,
            dbos::RunOptions {
                workflow_id: Some("write-only-missing-local"),
                ..Default::default()
            },
        )
        .await;
    assert!(matches!(
        refused,
        Err(dbos::Error::Application(ArchivesDbosError::Report(_)))
    ));
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!(
        std::fs::read_dir(root.join("attempts")).unwrap().count(),
        count
    );
}

async fn record_real_sdk_checkpoint(navigator: &PgPool) {
    let database: String = sqlx::query_scalar("SELECT current_database()::text")
        .fetch_one(navigator)
        .await
        .unwrap();
    let port = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT").unwrap();
    let mut config = dbos::Config::new(
        "write-only-coverage",
        format!("postgresql://postgres:synthetic@127.0.0.1:{port}/{database}"),
    );
    config.schema = "dbos_write_only".into();
    config.app_version = Some("synthetic-write-only-v1".into());
    let runtime = dbos::DBOS::new(config);
    let workflow = runtime
        .register_workflow("write-only-probe", |()| async {
            dbos::step("real-checkpoint", || async { Ok::<_, dbos::Error>(17_u32) }).await
        })
        .unwrap();
    drain_sdk(&runtime, async {
        runtime.launch().await.unwrap();
        assert_eq!(
            workflow
                .run_with(
                    (),
                    dbos::RunOptions {
                        workflow_id: Some("write-only-real-sdk-fixture"),
                        ..Default::default()
                    }
                )
                .await
                .unwrap(),
            17
        );
    })
    .await;
}

struct ObjectBackedSource {
    pg: store::postgres::PostgresDb,
    reads: AtomicUsize,
    source: Arc<crate::write_only::operations::object_store::tests::Memory>,
    target: Arc<crate::write_only::tests::CreateOnly>,
    first: AtomicBool,
    prepared: tokio::sync::Notify,
}
#[async_trait::async_trait]
impl ArchivesDbosPhases for ObjectBackedSource {
    async fn write_only_postgres(
        &self,
    ) -> Result<store::postgres::PostgresDb, ArchivesDbosPhaseError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(self.pg.clone())
    }
    async fn write_only_store(
        &self,
        _: &crate::write_only::operations::object_store::Config,
    ) -> Result<Arc<dyn crate::write_only::operations::object_store::Store>, ArchivesDbosPhaseError>
    {
        Ok(self.source.clone())
    }
    async fn write_only_destination(
        &self,
        _: String,
    ) -> Result<Arc<dyn cloud::StorageService>, ArchivesDbosPhaseError> {
        if self.first.swap(false, Ordering::SeqCst) {
            self.prepared.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(self.target.clone())
    }
    async fn snapshot(
        &self,
        _: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<SnapshotSummary, ArchivesDbosPhaseError> {
        panic!("object capture must not enter analytical storage")
    }
    async fn cost(&self) -> Result<(), ArchivesDbosPhaseError> {
        panic!("not the object-backed path")
    }
    async fn promote(&self, _: &SnapshotSummary) -> Vec<String> {
        panic!("create-only writer cannot promote")
    }
    fn render_digest(&self, _: &SnapshotSummary) -> Result<String, ArchivesDbosPhaseError> {
        panic!("not the object-backed digest")
    }
}
#[tokio::test]
async fn write_only_object_store_sdk_replacement_resumes_sealed_bytes_after_source_closes() {
    object_store_sdk_restart(false).await;
}
#[tokio::test]
async fn write_only_object_store_lost_capture_checkpoint_reuses_committed_seal() {
    object_store_sdk_restart(true).await;
}
async fn object_store_sdk_restart(capture_checkpoint_loss: bool) {
    store::test_support::with_postgres(async {
        let pg = store::postgres::PostgresDb::application().await.unwrap();
        let database: String = sqlx::query_scalar("SELECT current_database()::text")
            .fetch_one(pg.pool())
            .await
            .unwrap();
        let port = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT").unwrap();
        let url = format!("postgresql://postgres:synthetic@127.0.0.1:{port}/{database}");
        let mut config = crate::write_only::operations::object_store::tests::configured();
        config.source_identity_sha256 = crate::write_only::operations::source_identity(pg.pool())
            .await
            .unwrap();
        let phases = Arc::new(ObjectBackedSource {
            pg: pg.clone(),
            reads: AtomicUsize::new(0),
            source: Arc::new(crate::write_only::operations::object_store::tests::Memory::default()),
            target: Arc::new(crate::write_only::tests::CreateOnly::default()),
            first: AtomicBool::new(!capture_checkpoint_loss),
            prepared: tokio::sync::Notify::new(),
        });
        phases
            .source
            .pause_after_seal
            .store(capture_checkpoint_loss, Ordering::SeqCst);
        let input = RunRequest {
            scheduled_for: None,
            write_only: Some(crate::write_only::operations::Request {
                capture_id: None,
                delivery_attempt: 0,
                config_sha256: config.digest().unwrap(),
                config,
                reuse_local: None,
            }),
        };
        let slack = Arc::new(workflows::CapturingSlackBot::new());
        let first = write_only_runtime(&url);
        let workflow = register_dbos_with(
            &first,
            slack.clone(),
            "synthetic-archive".into(),
            true,
            phases.clone(),
        )
        .unwrap();
        first.launch().await.unwrap();
        let pending = workflow
            .start_with(
                input.clone(),
                dbos::StartOptions {
                    workflow_id: Some("object-backed-stable-request"),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            if capture_checkpoint_loss {
                phases.source.sealed.notified().await;
            } else {
                phases.prepared.notified().await;
            }
        })
        .await
        .unwrap();
        first.shutdown().await;
        drop(pending);
        drop(workflow);
        assert_eq!(phases.reads.load(Ordering::SeqCst), 1);
        pg.pool().close().await;
        complete_object_restart(&url, phases, slack, input).await;
    })
    .await;
}

async fn complete_object_restart(
    url: &str,
    phases: Arc<ObjectBackedSource>,
    slack: Arc<workflows::CapturingSlackBot>,
    input: RunRequest,
) {
    let second = write_only_runtime(url);
    let workflow = register_dbos_with(
        &second,
        slack.clone(),
        "synthetic-archive".into(),
        true,
        phases.clone(),
    )
    .unwrap();
    drain_sdk(&second, async {
        second.launch().await.unwrap();
        let report = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            workflow.run_with(
                input,
                dbos::RunOptions {
                    workflow_id: Some("object-backed-stable-request"),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let saved = report.write_only.unwrap();
        assert!(saved.all_destinations_acknowledged && saved.local.object_store.is_some());
        assert!(saved.local.root.as_os_str().is_empty());
        assert_eq!(
            phases.reads.load(Ordering::SeqCst),
            1,
            "replacement must never reopen PostgreSQL"
        );
        assert!(!phases.target.objects.lock().unwrap().is_empty());
        assert_eq!(
            slack.posted_messages().len(),
            1,
            "recovery emits one correlated digest"
        );
    })
    .await;
}

#[tokio::test]
async fn the_published_iceberg_snapshot_restores_the_event_before_calendar_retirement() {
    use cloud::calendar::{
        CalendarEventBackup, CalendarService, FakeCalendar, FAKE_CALENDAR_OWNER,
    };
    use store::project_calendar::{backup_events, ensure};
    with_pair(|nav, companion| async move {
        let db = store::surreal::SurrealDb::uninitialized();
        let (project, firm) = store::test_support::seed_calendar_project().await;
        store::firms::set_calendar_owner(&db, store::persons::Role::Owner, None, firm, FAKE_CALENDAR_OWNER).await.unwrap();
        let calendar = FakeCalendar::default();
        ensure(&db, project, &calendar).await.unwrap();
        let id = calendar.calendars().pop().unwrap().id;
        let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:synthetic\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        calendar.set_event_backups(&id, vec![CalendarEventBackup { href: "/sample.ics".into(), etag: "one".into(), ics: ics.into() }]);
        sqlx::query("UPDATE project SET status='closed' WHERE id=$1").bind(project).execute(&nav).await.unwrap();
        backup_events(&db, project, &calendar).await.unwrap();

        let storage = CollisionStorage::locked();
        let generation = paired(&nav, &companion, storage.as_ref()).await.unwrap();
        let source = generation.databases.iter().find(|db| db.database == "navigator").unwrap();
        let entry = source.tables.iter().find(|table| table.table == "public.project_calendar_event_backup").unwrap();
        let prefix = entry.key.split_once("/data/").unwrap().0;
        let metadata = source.iceberg_metadata_keys.iter().find(|key| key.starts_with(prefix)).unwrap();
        let io = iceberg::io::FileIO::new_with_memory();
        for key in storage.keys().into_iter().filter(|key| key.starts_with(prefix)) {
            io.new_output(format!("gs://synthetic-archives/{key}")).unwrap()
                .write(storage.get(&key).await.unwrap().bytes.into()).await.unwrap();
        }
        let table = iceberg::table::StaticTable::from_metadata_file(
            &format!("gs://synthetic-archives/{metadata}"),
            iceberg::TableIdent::from_strs(["calendar", "backup"]).unwrap(), io
        ).await.unwrap();
        let batches: Vec<_> = table.reader_builder().build().read(table.scan().build().unwrap().plan_files().await.unwrap())
            .unwrap().stream().try_collect().await.unwrap();
        assert_eq!(batches.iter().map(arrow_array58::RecordBatch::num_rows).sum::<usize>(), 1);
        let column = batches[0].column_by_name("ics").unwrap();
        assert_eq!(column.as_any().downcast_ref::<arrow_array58::StringArray>().unwrap().value(0), ics);
        assert!(calendar.calendar_exists(&id).await.unwrap());
        let pg = store::postgres::PostgresDb::application().await.unwrap();
        crate::postgres::confirm_calendar_backups(&pg, &generation).await.unwrap();
        backup_events(&db, project, &calendar).await.unwrap();
        assert!(!calendar.calendar_exists(&id).await.unwrap());
    }).await;
}

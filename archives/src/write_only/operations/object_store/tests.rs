mod restore;
use super::*;

#[test]
fn object_store_does_not_alias_an_offsite_bucket_or_accept_v1_config() {
    let local: crate::write_only::operations::Config = serde_json::from_value(serde_json::json!({
        "format_version": 1, "local_root": "/synthetic-private-archive",
        "source_project_number": null, "source_identity_sha256": "ab".repeat(32),
        "capture": {"scope":"registered_navigator"}, "destinations": [],
    }))
    .unwrap();
    local.validate().unwrap();
    let mut configured = local.clone();
    configured.object_store = Some(Config {
        bucket: "example-source-sealed".into(),
        prefix: "sealed".into(),
        authentication: Authentication::CredentialsFile,
        credentials_file: "/synthetic-private/source-credential.json".into(),
        max_generation_bytes: 64 * 1024 * 1024,
        max_object_bytes: 16 * 1024 * 1024,
        max_objects: 4096,
        max_rows_per_table: 100_000,
    });
    assert!(
        configured.validate().is_err(),
        "v1 local configuration must not be reinterpreted"
    );
    configured.format_version = 2;
    configured.local_root = std::path::PathBuf::new();
    configured.validate().unwrap();
    configured
        .destinations
        .push(crate::write_only::operations::Destination {
            bucket: "example-source-sealed".into(),
            project_number: 11,
            location: "us-west4".into(),
            minimum_retention_seconds: cloud::archives_project::OFFSITE_ARCHIVES_RETENTION_SECONDS,
        });
    assert!(
        configured.validate().is_err(),
        "source read privileges must not reach a destination"
    );
}

#[derive(Default)]
pub(crate) struct Memory {
    objects: std::sync::Mutex<std::collections::BTreeMap<String, (u64, Vec<u8>)>>,
    lost_ack: std::sync::Mutex<Option<String>>,
    pub(crate) pause_after_seal: std::sync::atomic::AtomicBool,
    pub(crate) sealed: tokio::sync::Notify,
}
#[async_trait::async_trait]
impl Store for Memory {
    async fn read(
        &self,
        key: &str,
        generation: Option<u64>,
        limit: usize,
    ) -> Result<Option<VersionedObject>> {
        let objects = self.objects.lock().unwrap();
        let Some((version, bytes)) = objects.get(key) else {
            return Ok(None);
        };
        ensure!(
            bytes.len() <= limit && generation.is_none_or(|wanted| wanted == *version),
            "source bound/version mismatch"
        );
        Ok(Some(VersionedObject {
            object: cloud::StoredObject {
                key: key.to_owned(),
                bytes: bytes.clone(),
                content_type: "application/octet-stream".into(),
            },
            generation: *version,
        }))
    }
    async fn create(&self, key: &str, bytes: &[u8], _: &str) -> Result<ImmutableObject> {
        let generation = {
            let mut objects = self.objects.lock().unwrap();
            ensure!(
                !objects.contains_key(key),
                "source create precondition failed"
            );
            let generation = objects.len() as u64 + 1;
            objects.insert(key.to_owned(), (generation, bytes.to_vec()));
            generation
        };
        if key.ends_with("inventory.json")
            && self
                .pause_after_seal
                .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            self.sealed.notify_one();
            std::future::pending::<()>().await;
        }
        let lost = self
            .lost_ack
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|part| key.contains(part));
        ensure!(!lost, "synthetic source acknowledgement lost");
        Ok(ImmutableObject {
            generation,
            size_bytes: bytes.len() as u64,
        })
    }
}
pub(crate) fn configured() -> crate::write_only::operations::Config {
    crate::write_only::operations::Config {
        format_version: 2,
        local_root: PathBuf::new(),
        object_store: Some(Config {
            bucket: "example-sealed-source".into(),
            prefix: "sealed".into(),
            authentication: Authentication::CredentialsFile,
            credentials_file: "/private/synthetic-source-credentials.json".into(),
            max_generation_bytes: 64 * 1024 * 1024,
            max_object_bytes: 16 * 1024 * 1024,
            max_objects: 4096,
            max_rows_per_table: 100_000,
        }),
        source_project_number: None,
        source_identity_sha256: "ab".repeat(32),
        capture: crate::write_only::operations::CaptureScope::RegisteredNavigator,
        destinations: vec![crate::write_only::operations::Destination {
            project_number: 11,
            bucket: "example-independent-one".into(),
            location: "us-west4".into(),
            minimum_retention_seconds: cloud::archives_project::OFFSITE_ARCHIVES_RETENTION_SECONDS,
        }],
    }
}
#[tokio::test]
async fn interrupted_capture_fence_refuses_new_rows_and_old_filesystem_receipts() {
    let config = configured();
    let source = Memory::default();
    let calls = std::sync::atomic::AtomicUsize::new(0);
    for _ in 0..2 {
        assert!(prepare(&source, &config, "same-request", None, || async {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            anyhow::bail!("synthetic extraction interruption")
        })
        .await
        .is_err());
    }
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let local:LocalReceipt=serde_json::from_value(serde_json::json!({"root":"/synthetic/local","root_uri":"file:///synthetic/local","inventory_key":"attempts/example/inventory.json","inventory_sha256":"ab".repeat(32),"cutoff_at":"2026-11-01T00:00:00Z","tables":1,"rows":0})).unwrap();
    assert!(local.object_store.is_none());
    assert!(open(&source, &config, &local).await.is_err());
}
#[tokio::test]
async fn sealed_objects_replay_without_files_or_source_and_destinations_never_read() {
    store::test_support::with_postgres(async {
        let pg = store::postgres::PostgresDb::application().await.unwrap();
        let person=uuid::Uuid::new_v4();
        sqlx::query("INSERT INTO person(id,name,email) VALUES($1,'Synthetic archived person','archive-person@example.invalid')").bind(person).execute(pg.pool()).await.unwrap();
        let restore_options=pg.pool().connect_options().as_ref().clone();
        let mut config = configured();
        config.source_identity_sha256 = super::super::source_identity(pg.pool()).await.unwrap();
        let source = Memory::default();
        *source.lost_ack.lock().unwrap() = Some("inventory.json".into());
        let saved = prepare(&source, &config, "frozen-request", None, || {
            capture_native(&pg, &config)
        })
        .await
        .unwrap();
        assert!(saved.root.as_os_str().is_empty());
        assert_eq!(saved.object_store.as_ref().unwrap().format_version, 2);
        let count = source.objects.lock().unwrap().len();
        pg.pool().close().await;
        let replay = prepare(&source, &config, "frozen-request", None, || async {
            panic!("checkpoint loss must not reextract")
        })
        .await
        .unwrap();
        assert_eq!(saved.inventory_sha256, replay.inventory_sha256);
        assert_eq!(source.objects.lock().unwrap().len(), count);
        let parquet_count = source
            .objects
            .lock()
            .unwrap()
            .keys()
            .filter(|key| key.ends_with("snapshot.parquet"))
            .count();
        let prepared = prepare_destination(&source, &config, &saved, 0, 0)
            .await
            .unwrap();
        assert_eq!(
            source
                .objects
                .lock()
                .unwrap()
                .keys()
                .filter(|key| key.ends_with("snapshot.parquet"))
                .count(),
            parquet_count,
            "preparing a destination must not copy source Parquet again"
        );
        let target = crate::write_only::tests::CreateOnly::default();
        let acknowledged = upload(
            &source,
            &config,
            &prepared,
            0,
            &saved.inventory_sha256,
            &target,
        )
        .await
        .unwrap();
        assert!(matches!(
            acknowledged,
            DestinationOutcome::Acknowledged { .. }
        ));
        let count = target.objects.lock().unwrap().len();
        assert!(matches!(
            upload(
                &source,
                &config,
                &prepared,
                0,
                &saved.inventory_sha256,
                &target
            )
            .await
            .unwrap(),
            DestinationOutcome::Acknowledged { .. }
        ));
        assert_eq!(
            target.objects.lock().unwrap().len(),
            count,
            "completed receipt must prevent repeat creates"
        );
        restore::person_from_destination(&target,&prepared,restore_options,person).await;
        let bundle = open(&source, &config, &saved).await.unwrap();
        let verified = super::super::verify::from_bundle(bundle, &saved)
            .await
            .unwrap();
        assert_eq!(verified.query_counts.len(), saved.tables);
        assert!(!verified.bucket_policy_checked);
        // A changed source object is refused before a target can be reached.
        let key = source
            .objects
            .lock()
            .unwrap()
            .keys()
            .find(|key| key.ends_with("snapshot.parquet"))
            .unwrap()
            .clone();
        source.objects.lock().unwrap().get_mut(&key).unwrap().1 = b"synthetic corruption".to_vec();
        assert!(open(&source, &config, &saved).await.is_err());
        assert_eq!(target.objects.lock().unwrap().len(), count);
    })
    .await;
}
#[tokio::test]
async fn destination_lost_ack_and_started_without_outcome_never_retry_target_calls() {
    store::test_support::with_postgres(async {
        let pg = store::postgres::PostgresDb::application().await.unwrap();
        let mut config = configured();
        config.source_identity_sha256 = super::super::source_identity(pg.pool()).await.unwrap();
        let source = Memory::default();
        let saved = prepare(&source, &config, "lost-target-ack", None, || {
            capture_native(&pg, &config)
        })
        .await
        .unwrap();
        let prepared = prepare_destination(&source, &config, &saved, 0, 0)
            .await
            .unwrap();
        let target = crate::write_only::tests::CreateOnly {
            lost_ack: Some(0),
            ..Default::default()
        };
        assert!(matches!(
            upload(
                &source,
                &config,
                &prepared,
                0,
                &saved.inventory_sha256,
                &target
            )
            .await
            .unwrap(),
            DestinationOutcome::Unconfirmed { .. }
        ));
        let count = target.objects.lock().unwrap().len();
        assert!(matches!(
            upload(
                &source,
                &config,
                &prepared,
                0,
                &saved.inventory_sha256,
                &target
            )
            .await
            .unwrap(),
            DestinationOutcome::Unconfirmed { .. }
        ));
        assert_eq!(target.objects.lock().unwrap().len(), count);
        // Lose the SDK checkpoint and the source outcome after a begun attempt.
        source
            .objects
            .lock()
            .unwrap()
            .retain(|key, _| !key.contains("/outcomes/"));
        assert!(matches!(
            upload(
                &source,
                &config,
                &prepared,
                0,
                &saved.inventory_sha256,
                &target
            )
            .await
            .unwrap(),
            DestinationOutcome::Unconfirmed {
                acknowledged: 0,
                ..
            }
        ));
        assert_eq!(target.objects.lock().unwrap().len(), count);
    })
    .await;
}

#[tokio::test]
async fn raw_and_encoded_bounds_refuse_before_a_generation_can_be_published() {
    store::test_support::with_postgres(async {
        let pg=store::postgres::PostgresDb::application().await.unwrap();
        sqlx::raw_sql("CREATE TABLE synthetic_archive_limit(payload text); INSERT INTO synthetic_archive_limit VALUES('one'),('two')").execute(pg.pool()).await.unwrap();
        for limits in [crate::postgres::ExportBounds {generation_bytes:1024,object_bytes:1024,rows_per_table:1},crate::postgres::ExportBounds {generation_bytes:32,object_bytes:32,rows_per_table:2}] {
            let mut tx=pg.pool().begin().await.unwrap();
            assert!(crate::postgres::export_table_bounded(&mut tx,"public","synthetic_archive_limit",None,Some(&limits)).await.is_err());
            tx.rollback().await.unwrap();
        }
    }).await;
}

#[tokio::test]
async fn duplicate_source_claims_enter_extraction_once() {
    let config = configured();
    let source = Memory::default();
    let entered = tokio::sync::Notify::new();
    let release = tokio::sync::Notify::new();
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let first = prepare(&source, &config, "concurrent-request", None, || async {
        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        entered.notify_one();
        release.notified().await;
        anyhow::bail!("synthetic interruption after ownership")
    });
    let duplicate = async {
        entered.notified().await;
        let result = prepare(&source, &config, "concurrent-request", None, || async {
            panic!("duplicate claim must not extract")
        })
        .await;
        release.notify_one();
        result
    };
    let (first, duplicate) = tokio::join!(first, duplicate);
    assert!(first.is_err() && duplicate.is_err());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn v1_request_roundtrip_preserves_its_config_digest_and_local_backing() {
    let old_config = format!(
        r#"{{"format_version":1,"local_root":"/synthetic/private","source_project_number":null,"source_identity_sha256":"{}","capture":{{"scope":"registered_navigator"}},"destinations":[]}}"#,
        "ab".repeat(32)
    );
    let old_digest = crate::postgres::sha256_hex(old_config.as_bytes());
    let old_request =
        format!(r#"{{"config":{old_config},"config_sha256":"{old_digest}","reuse_local":null}}"#);
    let request: crate::write_only::operations::Request =
        serde_json::from_str(&old_request).unwrap();
    request.validate().unwrap();
    assert_eq!(request.config.digest().unwrap(), old_digest);
    assert_eq!(
        serde_json::to_vec(&request.config).unwrap(),
        old_config.as_bytes()
    );
    let serialized = serde_json::to_value(&request).unwrap();
    assert!(serialized.get("capture_id").is_none() && serialized.get("delivery_attempt").is_none());
    assert!(serialized["config"].get("object_store").is_none());
    let restored: crate::write_only::operations::Request =
        serde_json::from_value(serialized).unwrap();
    restored.validate().unwrap();
    assert_eq!(
        serde_json::to_vec(&restored.config).unwrap(),
        old_config.as_bytes()
    );
    assert_eq!(restored.config_sha256, old_digest);
    assert!(restored.config.object_store.is_none());
}

#[test]
fn explicit_file_v2_digest_survives_metadata_mode_and_ambiguous_modes_refuse() {
    let source = r#"{"bucket":"example-sealed-source","prefix":"sealed","credentials_file":"/private/synthetic-source-credentials.json","max_generation_bytes":67108864,"max_object_bytes":16777216,"max_objects":4096,"max_rows_per_table":100000}"#;
    let original = format!(
        r#"{{"format_version":2,"object_store":{source},"local_root":"","source_project_number":null,"source_identity_sha256":"{}","capture":{{"scope":"registered_navigator"}},"destinations":[]}}"#,
        "ab".repeat(32)
    );
    let mut config: crate::write_only::operations::Config =
        serde_json::from_str(&original).unwrap();
    config.validate().unwrap();
    assert_eq!(serde_json::to_vec(&config).unwrap(), original.as_bytes());
    assert_eq!(
        config.digest().unwrap(),
        crate::postgres::sha256_hex(original.as_bytes())
    );
    let before = config.digest().unwrap();
    config.object_store.as_mut().unwrap().authentication = Authentication::GkeMetadata;
    assert!(
        config.validate().is_err(),
        "metadata mode must reject an ambiguous file credential"
    );
    config
        .object_store
        .as_mut()
        .unwrap()
        .credentials_file
        .clear();
    config.validate().unwrap();
    assert_ne!(
        config.digest().unwrap(),
        before,
        "source identity mode is bound by the request digest"
    );
    let serialized = serde_json::to_value(&config).unwrap();
    assert_eq!(serialized["object_store"]["authentication"], "gke_metadata");
    assert!(serialized["object_store"].get("credentials_file").is_none());
}

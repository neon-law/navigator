use super::*;
use crate::persons::{NewPerson, Role};
use crate::postgres_graph;
use crate::postgres_mirror::tests::{connect, fresh_schema};
use crate::test_support::{
    mem_surreal, seed_entity, SEED_ENTITY_JURISDICTION_ID, SEED_ENTITY_TYPE_ID,
};

/// Rows SurrealDB holds that PostgreSQL never received: three People and an
/// Entity. Both stores hold the synthetic Entity's reference rows.
async fn history() -> Option<(SurrealDb, PostgresDb, Vec<Uuid>, Uuid)> {
    let (options, database) = fresh_schema().await?;
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    // The schema's seeded Brands name Firms this synthetic source lacks.
    surreal
        .query("DELETE brand")
        .await
        .unwrap()
        .check()
        .unwrap();
    references(&surreal, &pg).await;
    let entity = seed_entity(&surreal).await;
    let mut people = Vec::new();
    for index in 0..3 {
        people.push(
            crate::persons::create(
                &surreal,
                &NewPerson::with_role(
                    "Synthetic Person",
                    format!("backfill-{index}-{}@example.com", Uuid::now_v7()),
                    Role::Client,
                ),
            )
            .await
            .unwrap()
            .id,
        );
    }
    Some((surreal, pg, people, entity))
}

async fn references(surreal: &SurrealDb, pg: &PostgresDb) {
    let inserted_at = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let updated_at = inserted_at;
    surreal
        .query(
            "CREATE $id SET name = 'Synthetic Entity Type', inserted_at = $inserted_at, \
             updated_at = $updated_at",
        )
        .bind((
            "id",
            crate::surreal::record_id("entity_type", SEED_ENTITY_TYPE_ID),
        ))
        .bind(("inserted_at", surrealdb::types::Datetime::from(inserted_at)))
        .bind(("updated_at", surrealdb::types::Datetime::from(updated_at)))
        .await
        .unwrap()
        .check()
        .unwrap();
    surreal
        .query(
            "CREATE $id SET name = 'Synthetic Jurisdiction', code = 'ZZ', \
             jurisdiction_type = 'state', inserted_at = $inserted_at, updated_at = $updated_at",
        )
        .bind((
            "id",
            crate::surreal::record_id("jurisdiction", SEED_ENTITY_JURISDICTION_ID),
        ))
        .bind(("inserted_at", surrealdb::types::Datetime::from(inserted_at)))
        .bind(("updated_at", surrealdb::types::Datetime::from(updated_at)))
        .await
        .unwrap()
        .check()
        .unwrap();
    sqlx::query(
        "INSERT INTO entity_type (id, name, inserted_at, updated_at) \
         VALUES ($1, 'Synthetic Entity Type', $2, $3)",
    )
    .bind(SEED_ENTITY_TYPE_ID)
    .bind(inserted_at)
    .bind(updated_at)
    .execute(&pg.0)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO jurisdiction \
         (id, name, code, jurisdiction_type, inserted_at, updated_at) \
         VALUES ($1, 'Synthetic Jurisdiction', 'ZZ', 'state', $2, $3)",
    )
    .bind(SEED_ENTITY_JURISDICTION_ID)
    .bind(inserted_at)
    .bind(updated_at)
    .execute(&pg.0)
    .await
    .unwrap();
}

async fn rows(pg: &PostgresDb, table: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
        .fetch_one(&pg.0)
        .await
        .unwrap()
}

fn table<'a>(plan: &'a Plan, name: &str) -> &'a TablePlan {
    plan.tables
        .iter()
        .find(|table| table.table == name)
        .unwrap()
}

#[tokio::test]
async fn asset_object_verification_counts_bytes_without_disclosing_keys() {
    use super::{check_asset_objects, expected_object, AssetObjectVerification};
    use cloud::{FsStorage, StorageService};
    use std::collections::BTreeMap;

    let dir = tempfile::tempdir().unwrap();
    let storage = FsStorage::new(dir.path()).await.unwrap();
    let bytes = b"synthetic document bytes";
    let digest = crate::assets::sha256_hex(bytes);
    storage
        .put(
            "projects/synthetic/documents/one",
            bytes,
            "application/octet-stream",
        )
        .await
        .unwrap();

    let mut report = AssetObjectVerification::default();
    let mut expected = BTreeMap::new();
    expected_object(
        &mut expected,
        &mut report,
        "projects/synthetic/documents/one".into(),
        u64::try_from(bytes.len()).unwrap(),
        digest,
    );
    expected_object(
        &mut expected,
        &mut report,
        "projects/synthetic/documents/missing".into(),
        9,
        "f".repeat(64),
    );
    report.unique_objects = expected.len();
    check_asset_objects(&storage, expected, &mut report).await;

    assert_eq!(report.unique_objects, 2);
    assert_eq!(report.verified_bytes, u64::try_from(bytes.len()).unwrap());
    assert_eq!(report.missing_objects, 1);
    assert!(!report.is_clean());
    assert!(!format!("{report:?}").contains("projects/"));

    let public_dir = tempfile::tempdir().unwrap();
    let public_storage = FsStorage::new(public_dir.path()).await.unwrap();
    public_storage
        .put(
            "brand-synthetic/logo.png",
            b"synthetic public logo",
            "image/png",
        )
        .await
        .unwrap();
    let public_keys = std::collections::BTreeSet::from([
        "brand-synthetic/logo.png".to_string(),
        "brand-synthetic/missing.woff2".to_string(),
    ]);
    let mut public_bytes = 0;
    let mut public_missing = 0;
    let mut public_unreadable = 0;
    super::check_referenced_objects(
        &public_storage,
        public_keys,
        &mut public_bytes,
        &mut public_missing,
        &mut public_unreadable,
    )
    .await;
    assert_eq!(public_bytes, b"synthetic public logo".len() as u64);
    assert_eq!(public_missing, 1);
    assert_eq!(public_unreadable, 0);
    assert!(
        !format!("{public_bytes} {public_missing} {public_unreadable}").contains("brand-synthetic")
    );
}

#[tokio::test]
async fn complete_asset_verification_reconciles_both_metadata_sources_and_retention() {
    use cloud::{FsStorage, StorageService};
    let Some((surreal, pg, _, _)) = history().await else {
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    let storage = FsStorage::new(directory.path()).await.unwrap();
    let bytes = b"synthetic bytes";
    let digest = crate::assets::sha256_hex(bytes);
    for key in ["synthetic/shared", "synthetic/copy"] {
        storage
            .put(key, bytes, "application/octet-stream")
            .await
            .unwrap();
    }
    storage
        .put("synthetic/corrupt", b"bad", "application/octet-stream")
        .await
        .unwrap();
    surreal.query("CREATE asset SET storage_key = 'synthetic/shared', secondary_storage_key = 'synthetic/copy', content_type = 'application/octet-stream', byte_size = $size, sha256_hex = $digest")
        .bind(("size", i64::try_from(bytes.len()).unwrap()))
        .bind(("digest", digest.clone()))
        .await.unwrap().check().unwrap();
    surreal.query("CREATE asset SET storage_key = $key, content_type = 'application/octet-stream', byte_size = 1, sha256_hex = $digest, purged_at = time::now()")
        .bind(("key", "purged"))
        .bind(("digest", digest.clone()))
        .await.unwrap().check().unwrap();
    for key in ["synthetic/shared", "synthetic/corrupt"] {
        sqlx::query("INSERT INTO asset (storage_key, content_type, byte_size, sha256_hex) VALUES ($1, 'application/octet-stream', $2, $3)")
            .bind(key).bind(i64::try_from(bytes.len()).unwrap()).bind(&digest)
            .execute(&pg.0).await.unwrap();
    }
    let report = verify_asset_objects(&surreal, &pg, &storage, &storage)
        .await
        .unwrap();
    assert_eq!(report.source_assets, 2);
    assert_eq!(report.target_assets, 2);
    assert_eq!(report.unique_objects, 3);
    assert_eq!(report.purged_assets, 1);
    assert_eq!(
        report.verified_bytes,
        u64::try_from(bytes.len() * 2).unwrap()
    );
    assert_eq!(report.size_mismatches, 1);
    assert_eq!(report.digest_mismatches, 1);
    assert_eq!(report.missing_objects, 0);
    assert!(!report.is_clean());
    assert!(!format!("{report:?}").contains("synthetic/"));

    sqlx::query("UPDATE asset SET sha256_hex = 'conflicting-metadata' WHERE storage_key = 'synthetic/shared'")
        .execute(&pg.0).await.unwrap();
    let report = verify_asset_objects(&surreal, &pg, &storage, &storage)
        .await
        .unwrap();
    assert_eq!(report.conflicting_expectations, 1);
    assert_eq!(report.unique_objects, 3);
    pg.0.close().await;
}

#[tokio::test]
async fn object_reference_inventory_accounts_for_finance_run_input_and_report_keys() {
    use cloud::{FsStorage, StorageService};

    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let project_id = Uuid::now_v7();
    let entity_id = Uuid::now_v7();
    surreal
        .query(
            "CREATE $project_id SET code = 'synthetic-finance', name = 'Synthetic Finance', \
             status = 'open', brand = 'Synthetic', entity_id = $entity_id, \
             inserted_at = '2026-10-22T00:00:00Z', \
             updated_at = '2026-10-22T00:00:00Z'",
        )
        .bind((
            "project_id",
            crate::surreal::record_id("project", project_id),
        ))
        .bind(("entity_id", crate::surreal::record_id("entity", entity_id)))
        .await
        .unwrap()
        .check()
        .unwrap();
    surreal
        .query(
            "CREATE $run_id SET project_id = $project_id, run_id = 'run-1', \
             computed_at = time::now(), parameters = { overhead_pct: 28, \
             attorney_pct: 70, lead_pct: 80 }, report_json = '{}', \
             inserted_at = time::now(), updated_at = time::now()",
        )
        .bind((
            "run_id",
            surrealdb::types::RecordId::new("finance_aggregate", "run-1"),
        ))
        .bind((
            "project_id",
            crate::surreal::record_id("project", project_id),
        ))
        .await
        .unwrap()
        .check()
        .unwrap();

    let (_, _, finance_keys, invalid) = non_asset_object_references(&surreal, &pg).await.unwrap();
    assert_eq!(invalid, 0);
    assert_eq!(
        finance_keys,
        std::collections::BTreeSet::from([
            crate::finance_aggregate::input_key("synthetic-finance"),
            crate::finance_aggregate::report_key("synthetic-finance", "run-1"),
        ])
    );
    let storage_dir = tempfile::tempdir().unwrap();
    let storage = FsStorage::new(storage_dir.path()).await.unwrap();
    for key in &finance_keys {
        storage
            .put(key, b"synthetic finance blob", "application/json")
            .await
            .unwrap();
    }
    let mut finance_bytes = 0;
    let mut missing = 0;
    let mut unreadable = 0;
    check_referenced_objects(
        &storage,
        finance_keys,
        &mut finance_bytes,
        &mut missing,
        &mut unreadable,
    )
    .await;
    assert_eq!(finance_bytes, 2 * b"synthetic finance blob".len() as u64);
    assert_eq!((missing, unreadable), (0, 0));
    pg.0.close().await;
}

#[test]
fn negative_source_byte_size_is_a_fail_closed_metadata_error() {
    let mut report = super::AssetObjectVerification::default();
    let size = super::verified_byte_size(-1, &mut report);
    assert_eq!(size, u64::MAX);
    assert_eq!(report.invalid_byte_sizes, 1);
    assert!(!report.is_clean());
}

#[test]
fn parity_key_samples_are_bounded_and_withhold_non_uuid_keys() {
    let mut keys: Vec<MirrorKey> = (0..52).map(|_| MirrorKey::Person(Uuid::new_v4())).collect();
    keys.push(MirrorKey::FirmBrand {
        brand_key: "private-client-key@example.test".into(),
    });
    let (sample, omitted, withheld) = safe_key_sample(&keys);
    assert_eq!(sample.len(), 50);
    assert_eq!(omitted, 2);
    assert_eq!(withheld, 1);
    assert!(sample.iter().all(|id| Uuid::parse_str(id).is_ok()));
    assert!(!format!("{sample:?}").contains("private-client-key"));
}

#[tokio::test]
async fn a_dry_run_plans_deterministically_and_writes_nothing() {
    let Some((surreal, pg, people, entity)) = history().await else {
        return;
    };
    let before = (rows(&pg, "person").await, rows(&pg, "entity").await);
    let plan = super::plan(&surreal, &pg, 2).await.unwrap();
    assert!(!plan.is_clean());
    assert_eq!(plan, super::plan(&surreal, &pg, 2).await.unwrap());
    assert_eq!(
        (rows(&pg, "person").await, rows(&pg, "entity").await),
        before
    );
    assert_eq!(rows(&pg, "mirror_backfill_run").await, 0);

    let person = table(&plan, "person");
    assert_eq!((person.missing, person.batches), (3, 2));
    let mut expected: Vec<String> = people.iter().map(Uuid::to_string).collect();
    expected.sort();
    assert_eq!(person.missing_ids, expected);
    assert_eq!(table(&plan, "entity").missing_ids, [entity.to_string()]);
    // A plan names rows by UUID only.
    assert!(!format!("{plan:?}").contains("Synthetic"));
    assert!(matches!(
        super::plan(&surreal, &pg, 0).await,
        Err(BackfillError::BatchSize)
    ));
}

#[tokio::test]
async fn an_interrupted_run_resumes_without_duplicates_or_gaps() {
    let Some((surreal, pg, people, _)) = history().await else {
        return;
    };
    let planned = super::plan(&surreal, &pg, 1).await.unwrap().missing();
    assert!(planned >= 4, "{planned}");

    let paused = start(&surreal, &pg, 1, Some(2)).await.unwrap();
    assert_eq!(paused.status, RunStatus::Paused);
    assert_eq!(paused.batches_this_invocation, 2);
    assert_eq!(
        paused
            .groups
            .iter()
            .map(|group| group.applied)
            .sum::<usize>(),
        2
    );

    // A crash after the second batch's row committed but before its
    // checkpoint: the checkpoint is one batch behind the rows.
    sqlx::query(
        "UPDATE mirror_backfill_checkpoint SET batches = batches - 1, applied = applied - 1 \
         WHERE run_id = $1 AND batches > 0 AND completed_at IS NULL",
    )
    .bind(paused.run_id)
    .execute(&pg.0)
    .await
    .unwrap();

    let resumed = resume(&surreal, &pg, paused.run_id, None).await.unwrap();
    assert!(resumed.is_clean(), "{resumed:?}");
    // The rewound row was already written, so it was not written again.
    // The graph arm's projection counts vertices and edges, not mirror
    // rows, so its applied count stays out of the relational arithmetic.
    assert_eq!(
        resumed
            .groups
            .iter()
            .filter(|group| group.mirror != postgres_graph::GRAPH_MIRROR)
            .map(|group| group.applied)
            .sum::<usize>()
            + 1,
        planned
    );
    assert!(resumed.groups.iter().all(|group| group.completed));
    let clean_plan = super::plan(&surreal, &pg, 1).await.unwrap();
    assert_eq!(clean_plan.missing(), 0);
    assert!(clean_plan.is_clean());
    let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM person ORDER BY id")
        .fetch_all(&pg.0)
        .await
        .unwrap();
    let mut expected = people.clone();
    expected.sort();
    assert_eq!(ids, expected);
}

#[tokio::test]
async fn a_rerun_is_idempotent() {
    let Some((surreal, pg, _, _)) = history().await else {
        return;
    };
    let first = start(&surreal, &pg, 500, None).await.unwrap();
    assert!(first.is_clean(), "{first:?}");
    let replay = resume(&surreal, &pg, first.run_id, None).await.unwrap();
    assert_eq!(replay.batches_this_invocation, 0);
    assert_eq!(replay.groups, first.groups);

    let second = start(&surreal, &pg, 500, None).await.unwrap();
    assert!(second.is_clean(), "{second:?}");
    assert_eq!(second.batches_this_invocation, 0);
    assert!(second.groups.iter().all(|group| group.applied == 0));
}

#[tokio::test]
async fn a_completed_run_reports_field_drift_and_is_not_clean() {
    let Some((surreal, pg, people, _)) = history().await else {
        return;
    };
    let first = start(&surreal, &pg, 500, None).await.unwrap();
    assert!(first.is_clean(), "{first:?}");

    sqlx::query("UPDATE person SET name = 'Synthetic changed target' WHERE id = $1")
        .bind(people[0])
        .execute(&pg.0)
        .await
        .unwrap();
    let report = resume(&surreal, &pg, first.run_id, None).await.unwrap();
    assert_eq!(report.status, RunStatus::Completed);
    assert!(!report.is_clean(), "field drift must fail the parity gate");
    let person = report
        .parity
        .iter()
        .find(|table| table.table == "person")
        .expect("person parity is included");
    assert_eq!((person.missing, person.differing, person.extra), (0, 1, 0));
    assert_eq!(person.divergent_ids, [people[0].to_string()]);
    assert!(!format!("{report:?}").contains("Synthetic changed target"));
}

#[tokio::test]
async fn a_row_that_cannot_land_is_reported_by_table_and_id() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    // The Entity names reference rows neither store can supply.
    let entity = seed_entity(&surreal).await;
    let run = start(&surreal, &pg, 10, None).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed);
    assert!(!run.is_clean());
    let failure = run
        .failures
        .iter()
        .find(|failure| failure.table == "entity")
        .expect("the Entity is reported");
    assert_eq!(failure.id.as_deref(), Some(entity.to_string().as_str()));
    assert!(failure.reason.contains("SQLSTATE"), "{}", failure.reason);
    assert!(!format!("{run:?}").contains("Synthetic"));
}

#[tokio::test]
async fn one_backfill_advances_at_a_time() {
    let Some((surreal, pg, _, _)) = history().await else {
        return;
    };
    let mut other = pg.0.acquire().await.unwrap();
    let held: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(LOCK)
        .fetch_one(&mut *other)
        .await
        .unwrap();
    assert!(held);
    assert!(matches!(
        start(&surreal, &pg, 10, None).await,
        Err(BackfillError::Busy)
    ));
    // A refused start leaves no run behind.
    assert_eq!(rows(&pg, "mirror_backfill_run").await, 0);
    assert!(matches!(
        resume(&surreal, &pg, Uuid::new_v4(), None).await,
        Err(BackfillError::Busy)
    ));
    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(LOCK)
        .execute(&mut *other)
        .await
        .unwrap();
    drop(other);
    assert!(matches!(
        resume(&surreal, &pg, Uuid::new_v4(), None).await,
        Err(BackfillError::UnknownRun(_))
    ));
}

#[tokio::test]
async fn populated_source_tables_without_mirror_coverage_fail_closed() {
    let Some((surreal, pg, _, _)) = history().await else {
        return;
    };
    surreal
        .query(
            "DEFINE TABLE unregistered_backfill_fixture SCHEMAFULL; \
             DEFINE FIELD payload ON unregistered_backfill_fixture TYPE string; \
             CREATE unregistered_backfill_fixture:fixture SET payload = 'Synthetic marker';",
        )
        .await
        .unwrap()
        .check()
        .unwrap();

    let plan_error = super::plan(&surreal, &pg, 10)
        .await
        .expect_err("a populated table outside the mirror registry must block planning");
    assert!(matches!(
        &plan_error,
        BackfillError::UncoveredTables(tables)
            if tables == &[UncoveredTable {
                table: "unregistered_backfill_fixture".to_string(),
                rows: 1,
            }]
    ));
    assert!(!plan_error.to_string().contains("Synthetic marker"));

    let run_error = start(&surreal, &pg, 10, None)
        .await
        .expect_err("a populated unsupported table must block the write run");
    assert!(matches!(run_error, BackfillError::UncoveredTables(_)));
    assert_eq!(rows(&pg, "mirror_backfill_run").await, 0);
}

#[tokio::test]
async fn unmapped_populated_source_fields_block_plan_and_start_before_writes() {
    let Some((surreal, pg, _, entity)) = history().await else {
        return;
    };
    surreal
        .query("DEFINE FIELD unmapped_fixture ON firm_anchor TYPE option<string>; CREATE firm_anchor:⟨synthetic-field-coverage⟩ SET entity_id = $entity, unmapped_fixture = 'Synthetic private payload';")
        .bind(("entity", crate::surreal::record_id("entity", entity)))
        .await
        .unwrap()
        .check()
        .unwrap();
    for error in [
        plan(&surreal, &pg, 2)
            .await
            .expect_err("dry-run must not declare incomplete source fields preserved"),
        start(&surreal, &pg, 2, None)
            .await
            .expect_err("write-run must refuse incomplete source field coverage"),
    ] {
        let text = error.to_string();
        assert!(
            text.contains("firm_anchor"),
            "the affected table is reported"
        );
        assert!(!text.contains("unmapped_fixture"));
        assert!(!text.contains("Synthetic private payload"));
    }
    assert_eq!(rows(&pg, "mirror_backfill_run").await, 0);
    assert_eq!(rows(&pg, "mirror_backfill_checkpoint").await, 0);
    assert_eq!(rows(&pg, "person").await, 0);
    assert_eq!(rows(&pg, "entity").await, 0);
    assert_eq!(rows(&pg, "firm_anchor").await, 0);
    // Omitted fields carry no source value; the configured mapping can be planned again.
    surreal
        .query("UPDATE firm_anchor:⟨synthetic-field-coverage⟩ UNSET unmapped_fixture;")
        .await
        .unwrap()
        .check()
        .unwrap();
    plan(&surreal, &pg, 2)
        .await
        .expect("removing the unmodeled field unblocks the plan");
    pg.0.close().await;
}

#[tokio::test]
async fn source_inventory_counts_uncovered_tables_without_reading_rows() {
    let surreal = mem_surreal().await;
    surreal
        .query(
            "DELETE brand; \
             DEFINE TABLE unregistered_backfill_fixture SCHEMAFULL; \
             DEFINE FIELD payload ON unregistered_backfill_fixture TYPE string; \
             CREATE unregistered_backfill_fixture:fixture SET payload = 'Synthetic marker';",
        )
        .await
        .unwrap()
        .check()
        .unwrap();

    let error = ensure_source_coverage(&surreal, &BTreeSet::new())
        .await
        .expect_err("a populated non-mirrored table blocks migration");
    assert!(
        matches!(
            error,
            BackfillError::UncoveredTables(ref tables)
                if tables == &[UncoveredTable {
                    table: "unregistered_backfill_fixture".to_string(),
                    rows: 1,
                }]
        ),
        "{error:?}"
    );
    assert!(!error.to_string().contains("Synthetic marker"));
}

#[tokio::test]
async fn source_inventory_excludes_schema_version_metadata() {
    let surreal = mem_surreal().await;
    surreal
        .query("DELETE brand")
        .await
        .unwrap()
        .check()
        .unwrap();

    ensure_source_coverage(&surreal, &BTreeSet::new())
        .await
        .expect("schema version is reconstructed by target schema migrations");
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One fixture spans both stores and both storage lanes.
async fn asset_object_verification_reads_both_stores_and_reports_counts_only() {
    use cloud::{FsStorage, StorageService};

    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    surreal
        .query("DELETE brand")
        .await
        .unwrap()
        .check()
        .unwrap();
    let document_dir = tempfile::tempdir().unwrap();
    let documents = FsStorage::new(document_dir.path()).await.unwrap();
    let public_dir = tempfile::tempdir().unwrap();
    let public = FsStorage::new(public_dir.path()).await.unwrap();

    let bytes = b"synthetic verified document";
    let digest = crate::assets::sha256_hex(bytes);
    let size = i64::try_from(bytes.len()).unwrap();
    for key in [
        "projects/synthetic/documents/present",
        "projects/synthetic/documents/present-copy",
    ] {
        documents
            .put(key, bytes, "application/octet-stream")
            .await
            .unwrap();
    }
    documents
        .put(
            "projects/synthetic/documents/wrong-size",
            b"short",
            "application/octet-stream",
        )
        .await
        .unwrap();
    public
        .put("brand-synthetic/logo.png", b"synthetic logo", "image/png")
        .await
        .unwrap();

    surreal
        .query(
            "CREATE asset SET storage_key = 'projects/synthetic/documents/present', \
             secondary_storage_key = 'projects/synthetic/documents/present-copy', \
             content_type = 'application/octet-stream', byte_size = $size, sha256_hex = $digest; \
             CREATE asset SET storage_key = 'projects/synthetic/documents/purged', \
             content_type = 'application/octet-stream', byte_size = 1, sha256_hex = $digest, \
             purged_at = time::now(); \
             CREATE asset SET storage_key = 'projects/synthetic/documents/negative', \
             content_type = 'application/octet-stream', byte_size = -1, sha256_hex = $digest;",
        )
        .bind(("size", size))
        .bind(("digest", digest.clone()))
        .await
        .unwrap()
        .check()
        .unwrap();

    for (key, secondary, byte_size, purged) in [
        ("projects/synthetic/documents/present", None, size, false),
        (
            "projects/synthetic/documents/wrong-size",
            Some("projects/synthetic/documents/present"),
            size,
            false,
        ),
        ("projects/synthetic/documents/gone", None, 3, true),
    ] {
        sqlx::query(
            "INSERT INTO asset (id, storage_key, secondary_storage_key, content_type, byte_size, \
             sha256_hex, purged_at) VALUES ($1, $2, $3, 'application/octet-stream', $4, $5, \
             CASE WHEN $6 THEN now() ELSE NULL END)",
        )
        .bind(Uuid::now_v7())
        .bind(key)
        .bind(secondary)
        .bind(byte_size)
        .bind(&digest)
        .bind(purged)
        .execute(&pg.0)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO brand (name, brand_key, logo_object_key, font_object_key) \
         VALUES ('Synthetic Brand', 'synthetic-assets', 'brand-synthetic/logo.png', '')",
    )
    .execute(&pg.0)
    .await
    .unwrap();

    let report = verify_asset_objects(&surreal, &pg, &documents, &public)
        .await
        .unwrap();
    assert_eq!((report.source_assets, report.target_assets), (3, 3));
    assert_eq!(report.purged_assets, 1);
    assert_eq!(report.invalid_byte_sizes, 1);
    // present, present-copy, negative, wrong-size.
    assert_eq!(report.unique_objects, 4);
    assert_eq!(report.verified_bytes, 2 * bytes.len() as u64);
    assert_eq!(report.missing_objects, 1);
    assert_eq!((report.size_mismatches, report.digest_mismatches), (1, 1));
    assert_eq!(report.public_reference_objects, 1);
    assert_eq!(
        report.public_reference_bytes,
        b"synthetic logo".len() as u64
    );
    assert_eq!(report.invalid_object_keys, 1);
    assert_eq!(report.private_reference_objects, 0);
    assert!(!report.is_clean());
    assert!(!format!("{report:?}").contains("projects/"));
    pg.0.close().await;
}

#[tokio::test]
async fn zero_batch_runs_are_refused_before_either_store_is_read() {
    let surreal = mem_surreal().await;
    // A lazy pool never dials; the refusal must come first.
    let pg = PostgresDb(
        sqlx::postgres::PgPoolOptions::new().connect_lazy_with(
            sqlx::postgres::PgConnectOptions::new()
                .host("127.0.0.1")
                .port(9),
        ),
    );
    assert!(matches!(
        start(&surreal, &pg, 0, None).await,
        Err(BackfillError::BatchSize)
    ));
    assert!(matches!(
        start(&surreal, &pg, usize::MAX, None).await,
        Err(BackfillError::BatchSize)
    ));
    assert!(matches!(
        super::plan(&surreal, &pg, 0).await,
        Err(BackfillError::BatchSize)
    ));
    assert_eq!(RunStatus::Paused.label(), "paused");
    assert_eq!(
        BackfillError::SourceTableCount("synthetic".into()).to_string(),
        "could not decode SurrealDB source table synthetic count"
    );
}

#[tokio::test]
async fn object_verification_times_out_and_drops_a_hung_backend_request() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    struct RequestGuard(Arc<AtomicBool>);
    impl Drop for RequestGuard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    let dropped = Arc::new(AtomicBool::new(false));
    let guard = RequestGuard(Arc::clone(&dropped));
    let hung = async move {
        let _guard = guard;
        std::future::pending::<Result<(), cloud::StorageError>>().await
    };
    let error = object_check_with_timeout(hung, Duration::from_millis(1))
        .await
        .unwrap_err();
    assert!(matches!(error, ObjectCheckError::TimedOut));
    assert!(
        dropped.load(Ordering::SeqCst),
        "timed-out reads must be canceled"
    );
}

#[tokio::test]
async fn object_verification_preserves_success_and_missing_object_classification() {
    let success = object_check_with_timeout(
        async { Ok::<_, cloud::StorageError>(7_u64) },
        Duration::from_secs(1),
    )
    .await
    .unwrap();
    assert_eq!(success, 7);
    let missing = object_check_with_timeout(
        async { Err::<(), _>(cloud::StorageError::NotFound("synthetic-object".into())) },
        Duration::from_secs(1),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        missing,
        ObjectCheckError::Backend(cloud::StorageError::NotFound(_))
    ));
}

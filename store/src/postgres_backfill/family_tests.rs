//! PostgreSQL-only enrollment receipts, retries, and authority replay fences.

use super::{Family, FamilyError, SourceReceipt};

pub(super) fn receipt(family: Family, export: &[u8]) -> SourceReceipt {
    use sha2::Digest;
    SourceReceipt {
        format_version: 1,
        consistency: "operator_quiesced".into(),
        export_bytes: u64::try_from(export.len()).unwrap(),
        export_sha256: hex::encode(sha2::Sha256::digest(export)),
        schema_sha256: "ab".repeat(32),
        table_counts: family
            .tables()
            .iter()
            .map(|table| ((*table).into(), 0))
            .collect(),
    }
}

#[test]
fn capture_evidence_requires_exact_bytes_strict_capture_and_complete_census() {
    let export = b"synthetic export";
    let proof = receipt(Family::Satellites, export);
    proof.validate(Family::Satellites).unwrap();
    proof.clone().verify_export(export).unwrap();
    assert!(matches!(
        proof.clone().verify_export(b"changed bytes"),
        Err(FamilyError::Receipt)
    ));
    let mut best_effort = proof.clone();
    best_effort.consistency = "best_effort".into();
    // Recovery still accepts the existing receipt format; authority requires strict capture.
    best_effort.clone().verify_export(export).unwrap();
    assert!(matches!(
        best_effort.validate(Family::Satellites),
        Err(FamilyError::Receipt)
    ));
    let mut incomplete = proof.clone();
    incomplete.table_counts.remove("testimonial");
    assert!(matches!(
        incomplete.validate(Family::Satellites),
        Err(FamilyError::Receipt)
    ));
    let mut malformed = proof;
    malformed.schema_sha256 = "unusable".into();
    assert!(matches!(
        malformed.validate(Family::Satellites),
        Err(FamilyError::Receipt)
    ));
}

#[test]
fn an_existing_source_orphan_cannot_mask_a_different_missing_postgres_parent() {
    let mut reference = crate::postgres_mirror::records::UnresolvedReference {
        table: "citation",
        field: "authority_use_id",
        source_orphans: 0,
        postgres_missing: 0,
        newly_target_missing: 0,
    };
    reference.observe(false, true);
    reference.observe(true, false);
    assert_eq!(
        (reference.source_orphans, reference.postgres_missing),
        (1, 1)
    );
    assert_eq!(reference.newly_target_missing, 1);
    let snapshot = super::Snapshot {
        schema_sha256: "ab".repeat(32),
        export_sha256: String::new(),
        export_bytes: 0,
        tables: vec![],
        raw_counts: std::collections::BTreeMap::default(),
        references: vec![reference],
        reset_fact_differences: vec![],
        foreign_keys_hold: true,
        asset_union: None,
        reset_union: None,
        template_pins: None,
        project_lifecycle: None,
        sent_email_union: None,
        journal_handoff: None,
    };
    assert!(!snapshot.clean());
}

use crate::postgres::PostgresDb;
use crate::postgres_mirror::{records, MirrorKey, TableParity};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};
use uuid::Uuid;

struct FrozenRows {
    rows: Mutex<Vec<Value>>,
    fail_next: AtomicBool,
    unavailable: AtomicBool,
}

impl FrozenRows {
    fn capture(&self) -> (SourceReceipt, Vec<u8>) {
        let rows = self.rows.lock().unwrap();
        let bytes = serde_json::to_vec(&*rows).unwrap();
        let mut capture = receipt(Family::Satellites, &bytes);
        capture
            .table_counts
            .insert("playbook".into(), i64::try_from(rows.len()).unwrap());
        (capture, bytes)
    }
}

impl super::Source for FrozenRows {
    fn snapshot<'a>(
        &'a self,
        pg: &'a PostgresDb,
        family: Family,
    ) -> super::SourceOp<'a, super::Snapshot> {
        Box::pin(async move {
            assert!(
                !self.unavailable.load(Ordering::SeqCst),
                "completed replay must never touch Source"
            );
            let (capture, _) = self.capture();
            let rows = self.rows.lock().unwrap().clone();
            let source = rows
                .into_iter()
                .map(|row| records::normalized_record("playbook", row))
                .collect::<Result<_, _>>()?;
            let target = records::target_record_rows(pg, "playbook").await?;
            let comparison = records::compare_record_rows("playbook", source, target);
            let mut tables = Vec::new();
            for table in family.tables() {
                if *table == "playbook" {
                    tables.push(comparison.clone());
                } else {
                    let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                        "SELECT count(*) FROM \"{table}\""
                    )))
                    .fetch_one(pg.pool())
                    .await?;
                    tables.push(TableParity {
                        table,
                        source_rows: 0,
                        target_rows: usize::try_from(count).unwrap(),
                        divergent: vec![],
                    });
                }
            }
            Ok(super::Snapshot {
                tables,
                raw_counts: capture
                    .table_counts
                    .iter()
                    .map(|(name, count)| (name.clone(), usize::try_from(*count).unwrap()))
                    .collect(),
                schema_sha256: capture.schema_sha256,
                export_sha256: capture.export_sha256,
                export_bytes: capture.export_bytes,
                references: vec![],
                reset_fact_differences: vec![],
                foreign_keys_hold: super::foreign_keys_hold(pg, family).await?,
                asset_union: None,
                reset_union: None,
                template_pins: None,
                project_lifecycle: None,
                sent_email_union: None,
                journal_handoff: None,
            })
        })
    }

    fn write<'a>(&'a self, pg: &'a PostgresDb, key: &'a MirrorKey) -> super::SourceOp<'a, ()> {
        Box::pin(async move {
            if self.fail_next.swap(false, Ordering::SeqCst) {
                return Err(crate::postgres_mirror::MirrorError::Source.into());
            }
            let rows = self.rows.lock().unwrap().clone();
            let row = rows
                .into_iter()
                .find_map(|row| {
                    let (found, normalized) = records::normalized_record("playbook", row).ok()?;
                    (found == *key).then_some(normalized)
                })
                .ok_or(crate::postgres_mirror::MirrorError::Source)?;
            records::copy_record(pg, "playbook", row).await?;
            Ok(())
        })
    }
}

async fn fixture() -> PostgresDb {
    let (options, database) = crate::postgres_mirror::tests::fresh_schema()
        .await
        .expect("required PostgreSQL fixture");
    crate::postgres_mirror::tests::connect(&options, &database).await
}

async fn source(pg: &PostgresDb) -> FrozenRows {
    let (_, notation, _) = crate::postgres::fixtures::workflow_fixture(pg).await;
    let entity: Uuid = sqlx::query_scalar(
        "SELECT entity_id FROM project WHERE id = (SELECT project_id FROM notation WHERE id = $1)",
    )
    .bind(notation)
    .fetch_one(pg.pool())
    .await
    .unwrap();
    FrozenRows {
        rows: Mutex::new(vec![
            json!({"id":Uuid::new_v4(),"entity_id":entity,"name":"Synthetic import","positions":null,"active":true,"inserted_at":"2026-11-17T00:00:00Z","updated_at":"2026-11-17T00:00:00Z"}),
        ]),
        fail_next: AtomicBool::new(false),
        unavailable: AtomicBool::new(false),
    }
}

async fn enroll(pg: &PostgresDb, source: &FrozenRows) -> Uuid {
    let (capture, bytes) = source.capture();
    let run = Uuid::new_v4();
    super::enroll(
        pg,
        run,
        Family::Satellites,
        1,
        capture.verify_export(&bytes).unwrap(),
    )
    .await
    .unwrap();
    run
}

async fn advance(
    pg: &PostgresDb,
    source: &FrozenRows,
    run: Uuid,
    batches: Option<usize>,
    activate: bool,
) -> Result<super::Advancement, crate::postgres_backfill::BackfillError> {
    Box::pin(crate::postgres_backfill::with_import_lock(
        pg,
        super::during_import(
            run,
            Family::Satellites,
            super::advance_with(source, pg, run, batches, activate),
        ),
    ))
    .await
}

#[tokio::test]
async fn scoped_import_repairs_sql_null_then_retries_failure_without_replaying_active_authority() {
    let pg = fixture().await;
    for family in Family::all()
        .iter()
        .filter(|family| **family != Family::Satellites)
    {
        super::activate_fixture(&pg, *family).await;
    }
    let source = source(&pg).await;
    let row = source.rows.lock().unwrap()[0].clone();
    let id = row["id"].as_str().unwrap();
    sqlx::query("INSERT INTO playbook(id,entity_id,name,positions,active,inserted_at,updated_at) VALUES ($1,$2,'Synthetic import',NULL,true,'2026-11-17','2026-11-17')")
        .bind(Uuid::parse_str(id).unwrap()).bind(Uuid::parse_str(row["entity_id"].as_str().unwrap()).unwrap()).execute(pg.pool()).await.unwrap();
    assert!(matches!(
        super::require_scope(&pg, Family::Satellites).await,
        Err(FamilyError::NotActive)
    ));
    let run = enroll(&pg, &source).await;
    source.fail_next.store(true, Ordering::SeqCst);
    let failed = advance(&pg, &source, run, None, true).await.unwrap();
    assert!(!failed.snapshot.unwrap().clean());
    let failures: i64 =
        sqlx::query_scalar("SELECT count(*) FROM mirror_backfill_failure WHERE run_id = $1")
            .bind(run)
            .fetch_one(pg.pool())
            .await
            .unwrap();
    assert_eq!(failures, 1);
    assert!(super::require_scope(&pg, Family::Satellites).await.is_err());
    advance(&pg, &source, run, None, true).await.unwrap();
    super::require_scope(&pg, Family::Satellites).await.unwrap();
    let is_json_null: bool = sqlx::query_scalar(
        "SELECT positions = 'null'::jsonb AND positions IS NOT NULL FROM playbook WHERE id = $1",
    )
    .bind(Uuid::parse_str(id).unwrap())
    .fetch_one(pg.pool())
    .await
    .unwrap();
    assert!(
        is_json_null,
        "SQL NULL and explicit JSON null must not compare equal"
    );
    sqlx::query("UPDATE mirror_backfill_checkpoint SET verification = jsonb_set(verification, '{references}', $2) WHERE run_id = $1 AND mirror = 'table:playbook'")
        .bind(run).bind(json!([{"field":"entity_id","source_orphans":1,"postgres_missing":1,"newly_target_missing":0}])).execute(pg.pool()).await.unwrap();
    let completed = crate::postgres_backfill::completed_family_report(&pg, Family::Satellites, run)
        .await
        .unwrap()
        .unwrap();
    assert!(completed.is_clean());
    assert_eq!(completed.unresolved_references.len(), 1);
    assert_eq!(completed.unresolved_references[0].source_orphans, 1);
    super::require_runtime_scopes(&pg).await.unwrap();
    assert!(
        !crate::postgres_mirror::backfilled(&pg).await.unwrap(),
        "scoped completion cannot replace global completion"
    );
    sqlx::query("DELETE FROM playbook WHERE id = $1")
        .bind(Uuid::parse_str(id).unwrap())
        .execute(pg.pool())
        .await
        .unwrap();
    source.unavailable.store(true, Ordering::SeqCst);
    assert!(advance(&pg, &source, run, None, true)
        .await
        .unwrap()
        .snapshot
        .is_none());
    let mode = crate::postgres_mirror::during_initial_copy(super::import_mode(&pg, "playbook"))
        .await
        .unwrap();
    assert_eq!(mode, super::ImportMode::Refuse);
    let blocked =
        crate::postgres_mirror::during_initial_copy(records::copy_record(&pg, "playbook", row))
            .await;
    assert!(blocked.is_err());
    assert!(matches!(
        super::enroll(
            &pg,
            Uuid::new_v4(),
            Family::Satellites,
            1,
            source
                .capture()
                .0
                .verify_export(&source.capture().1)
                .unwrap()
        )
        .await,
        Err(FamilyError::Overlap)
    ));
    pg.pool().close().await;
}

#[tokio::test]
async fn pending_capture_refresh_preserves_progress_and_active_capture_is_immutable() {
    let pg = fixture().await;
    let source = source(&pg).await;
    let run = enroll(&pg, &source).await;
    advance(&pg, &source, run, None, false).await.unwrap();
    source.rows.lock().unwrap()[0]["name"] = json!("Synthetic changed field");
    assert!(
        advance(&pg, &source, run, None, true).await.is_err(),
        "same identities/counts with changed bytes are stale"
    );
    let (capture, bytes) = source.capture();
    crate::postgres_backfill::with_import_lock(
        &pg,
        super::refresh_capture_with(
            &source,
            &pg,
            run,
            capture.clone().verify_export(&bytes).unwrap(),
        ),
    )
    .await
    .unwrap();
    let history: Value =
        sqlx::query_scalar("SELECT source_receipt_history FROM mirror_backfill_run WHERE id = $1")
            .bind(run)
            .fetch_one(pg.pool())
            .await
            .unwrap();
    assert_eq!(history.as_array().unwrap().len(), 1);
    let applied: i32 = sqlx::query_scalar("SELECT applied FROM mirror_backfill_checkpoint WHERE run_id = $1 AND mirror = 'table:playbook'").bind(run).fetch_one(pg.pool()).await.unwrap();
    assert_eq!(applied, 1);
    crate::postgres_backfill::with_import_lock(
        &pg,
        super::refresh_capture_with(
            &source,
            &pg,
            run,
            capture.clone().verify_export(&bytes).unwrap(),
        ),
    )
    .await
    .unwrap();
    let replay_history: Value =
        sqlx::query_scalar("SELECT source_receipt_history FROM mirror_backfill_run WHERE id = $1")
            .bind(run)
            .fetch_one(pg.pool())
            .await
            .unwrap();
    assert_eq!(replay_history, history);
    advance(&pg, &source, run, None, true).await.unwrap();
    source.unavailable.store(true, Ordering::SeqCst);
    assert!(
        super::refresh_capture_with(&source, &pg, run, capture.verify_export(&bytes).unwrap())
            .await
            .is_err()
    );
    pg.pool().close().await;
}

#[tokio::test]
async fn absent_optional_json_survives_import_and_remains_readable() {
    let pg = fixture().await;
    let source = source(&pg).await;
    let id = {
        let mut rows = source.rows.lock().unwrap();
        rows[0].as_object_mut().unwrap().remove("positions");
        Uuid::parse_str(rows[0]["id"].as_str().unwrap()).unwrap()
    };
    let run = enroll(&pg, &source).await;
    advance(&pg, &source, run, None, true).await.unwrap();
    super::require_scope(&pg, Family::Satellites).await.unwrap();
    let unavailable = crate::surreal::SurrealDb::uninitialized();
    let row = crate::postgres::matter::with_test_db(
        pg.clone(),
        crate::playbooks::by_id(&unavailable, id),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(row.positions, Value::Null);
    let absent: bool = sqlx::query_scalar("SELECT positions IS NULL FROM playbook WHERE id = $1")
        .bind(id)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert!(
        absent,
        "reading the public Json view must not turn SQL absence into JSON null"
    );
    pg.pool().close().await;
}

#[tokio::test]
async fn a_refreshed_empty_capture_does_not_delete_target_only_history() {
    let pg = fixture().await;
    let source = source(&pg).await;
    let run = enroll(&pg, &source).await;
    advance(&pg, &source, run, None, false).await.unwrap();
    source.rows.lock().unwrap().clear();
    let (capture, bytes) = source.capture();
    crate::postgres_backfill::with_import_lock(
        &pg,
        super::refresh_capture_with(&source, &pg, run, capture.verify_export(&bytes).unwrap()),
    )
    .await
    .unwrap();
    let blocked = advance(&pg, &source, run, None, true).await.unwrap();
    assert!(!blocked.snapshot.unwrap().clean());
    let retained: String = sqlx::query_scalar("SELECT name FROM playbook")
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(retained, "Synthetic import");
    assert!(matches!(
        super::require_runtime_scopes(&pg).await,
        Err(FamilyError::NotActive)
    ));
    let failures: i64 =
        sqlx::query_scalar("SELECT count(*) FROM mirror_backfill_failure WHERE run_id = $1")
            .bind(run)
            .fetch_one(pg.pool())
            .await
            .unwrap();
    assert_eq!(
        failures, 1,
        "source absence is reviewed drift, never permission to delete"
    );
    pg.pool().close().await;
}

#[tokio::test]
async fn a_completed_global_run_does_not_skip_new_family_history_or_its_batch_limit() {
    let pg = fixture().await;
    sqlx::query("INSERT INTO mirror_backfill_run(id,batch_size,status,completed_at) VALUES ($1,1,'completed',now())")
        .bind(Uuid::new_v4()).execute(pg.pool()).await.unwrap();
    assert!(crate::postgres_mirror::backfilled(&pg).await.unwrap());
    let source = source(&pg).await;
    let mut second = source.rows.lock().unwrap()[0].clone();
    second["id"] = json!(Uuid::new_v4());
    second["name"] = json!("Synthetic second import");
    source.rows.lock().unwrap().push(second);
    let run = enroll(&pg, &source).await;
    let partial = advance(&pg, &source, run, Some(1), true).await.unwrap();
    assert_eq!(partial.batches, 1);
    assert!(!partial.snapshot.unwrap().clean());
    assert!(super::require_scope(&pg, Family::Satellites).await.is_err());
    let remaining = advance(&pg, &source, run, None, true).await.unwrap();
    assert_eq!(
        remaining.batches, 1,
        "resume copies only the remaining identity"
    );
    super::require_scope(&pg, Family::Satellites).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM playbook")
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(count, 2);
    pg.pool().close().await;
}

#[tokio::test]
async fn malformed_extra_and_ambiguous_completion_proofs_fail_closed() {
    let pg = fixture().await;
    assert!(matches!(
        super::require_runtime_scopes(&pg).await,
        Err(FamilyError::NotActive)
    ));
    for family in Family::all() {
        super::activate_fixture(&pg, *family).await;
    }
    super::require_runtime_scopes(&pg).await.unwrap();
    let run = super::validated_scope(&pg, Family::Satellites)
        .await
        .unwrap();
    let proof: Value = sqlx::query_scalar("SELECT verification FROM mirror_backfill_checkpoint WHERE run_id = $1 AND mirror = 'table:playbook'")
        .bind(run).fetch_one(pg.pool()).await.unwrap();
    for replacement in [
        json!({"differences":0}),
        {
            let mut changed = proof.clone();
            changed["source_rows"] = json!(-1);
            changed
        },
        {
            let mut changed = proof.clone();
            changed["source_rows"] = json!(1);
            changed["target_rows"] = json!(1);
            changed
        },
    ] {
        sqlx::query("UPDATE mirror_backfill_checkpoint SET verification = $2 WHERE run_id = $1 AND mirror = 'table:playbook'")
            .bind(run).bind(replacement).execute(pg.pool()).await.unwrap();
        assert!(matches!(
            super::require_scope(&pg, Family::Satellites).await,
            Err(FamilyError::Receipt)
        ));
        assert!(matches!(
            super::require_supported_scopes(&pg).await,
            Err(FamilyError::Receipt)
        ));
    }
    sqlx::query("UPDATE mirror_backfill_checkpoint SET verification = $2 WHERE run_id = $1 AND mirror = 'table:playbook'")
        .bind(run).bind(proof).execute(pg.pool()).await.unwrap();
    sqlx::query("INSERT INTO mirror_backfill_checkpoint(run_id,mirror) VALUES ($1,'table:unexpected_pending')")
        .bind(run).execute(pg.pool()).await.unwrap();
    assert!(matches!(
        super::require_scope(&pg, Family::Satellites).await,
        Err(FamilyError::Receipt)
    ));
    sqlx::query("DELETE FROM mirror_backfill_checkpoint WHERE run_id = $1 AND mirror = 'table:unexpected_pending'")
        .bind(run).execute(pg.pool()).await.unwrap();
    super::require_scope(&pg, Family::Satellites).await.unwrap();
    let other = Uuid::new_v4();
    sqlx::query("INSERT INTO mirror_backfill_run(id,batch_size,status,scope,source_receipt,completed_at) SELECT $2,batch_size,status,scope,source_receipt,completed_at FROM mirror_backfill_run WHERE id = $1")
        .bind(run).bind(other).execute(pg.pool()).await.unwrap();
    assert!(matches!(
        super::require_scope(&pg, Family::Satellites).await,
        Err(FamilyError::Receipt)
    ));
    assert!(super::require_runtime_scopes(&pg).await.is_err());
    assert!(
        crate::postgres_backfill::completed_family_report(&pg, Family::Satellites, other)
            .await
            .is_err()
    );
    assert!(
        crate::postgres_backfill::completed_family_report(&pg, Family::Satellites, run)
            .await
            .is_err()
    );
    sqlx::query("UPDATE mirror_backfill_run SET scope = 'satellites:2' WHERE id = $1")
        .bind(other)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO mirror_backfill_checkpoint(run_id,mirror,completed_at,verification) SELECT $2,mirror,completed_at,verification FROM mirror_backfill_checkpoint WHERE run_id = $1 AND mirror = 'table:playbook'").bind(run).bind(other).execute(pg.pool()).await.unwrap();
    assert!(
        matches!(
            super::require_runtime_scopes(&pg).await,
            Err(FamilyError::Receipt)
        ),
        "another completed version cannot own the same table"
    );
    pg.pool().close().await;
}

#[tokio::test]
async fn cancelling_an_import_lease_does_not_return_a_locked_session_to_the_pool() {
    let pg = fixture().await;
    let held_pg = pg.clone();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        crate::postgres_backfill::with_import_lock(&held_pg, async move {
            entered.send(()).unwrap();
            std::future::pending::<Result<(), crate::postgres_backfill::BackfillError>>().await
        })
        .await
    });
    ready.await.unwrap();
    assert!(matches!(
        crate::postgres_backfill::with_import_lock(&pg, async { Ok(()) }).await,
        Err(crate::postgres_backfill::BackfillError::Busy)
    ));
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match crate::postgres_backfill::with_import_lock(&pg, async { Ok(()) }).await {
                Ok(()) => break,
                Err(crate::postgres_backfill::BackfillError::Busy) => {
                    tokio::task::yield_now().await;
                }
                Err(error) => panic!("unexpected importer lease failure: {error}"),
            }
        }
    })
    .await
    .expect("cancelled lease releases the PostgreSQL session lock");
    pg.pool().close().await;
}

#[tokio::test]
async fn pending_import_refuses_partial_person_merge_drift_but_preserves_source_only_orphans() {
    let pg = fixture().await;
    let id = Uuid::new_v4();
    let kept_person = Uuid::new_v4();
    let source_person = Uuid::new_v4();
    let project = Uuid::new_v4();
    sqlx::query("INSERT INTO expunge_record(id,project_id,path,category,authorized_by_person_id,inserted_at,updated_at) VALUES ($1,$2,'synthetic','privilege',$3,'2026-11-17','2026-11-17')")
        .bind(id).bind(project).bind(kept_person).execute(pg.pool()).await.unwrap();
    let conflicting = json!({"id":id,"project_id":project,"path":"synthetic","category":"privilege","authorized_by_person_id":source_person,"inserted_at":"2026-11-17T00:00:00Z","updated_at":"2026-11-17T00:00:00Z"});
    let mut historical_orphan = conflicting.clone();
    let orphan_id = Uuid::new_v4();
    historical_orphan["id"] = json!(orphan_id);
    let bytes = serde_json::to_vec(&[conflicting.clone(), historical_orphan.clone()]).unwrap();
    let mut capture = receipt(Family::Satellites, &bytes);
    capture.table_counts.insert("expunge_record".into(), 2);
    let run = Uuid::new_v4();
    super::enroll(
        &pg,
        run,
        Family::Satellites,
        1,
        capture.verify_export(&bytes).unwrap(),
    )
    .await
    .unwrap();
    super::during_import(run, Family::Satellites, async {
        let (_, row) = records::normalized_record("expunge_record", conflicting).unwrap();
        assert!(matches!(
            records::copy_record(&pg, "expunge_record", row).await,
            Err(crate::postgres_mirror::MirrorError::MatterAuthority)
        ));
        let (_, row) = records::normalized_record("expunge_record", historical_orphan).unwrap();
        records::copy_record(&pg, "expunge_record", row.clone())
            .await
            .unwrap();
        records::copy_record(&pg, "expunge_record", row)
            .await
            .unwrap();
    })
    .await;
    let retained: Uuid =
        sqlx::query_scalar("SELECT authorized_by_person_id FROM expunge_record WHERE id = $1")
            .bind(id)
            .fetch_one(pg.pool())
            .await
            .unwrap();
    assert_eq!(retained, kept_person);
    let orphan: Uuid =
        sqlx::query_scalar("SELECT authorized_by_person_id FROM expunge_record WHERE id = $1")
            .bind(orphan_id)
            .fetch_one(pg.pool())
            .await
            .unwrap();
    assert_eq!(orphan, source_person);
    assert!(super::require_runtime_scopes(&pg).await.is_err());
    pg.pool().close().await;
}

#[tokio::test]
async fn an_older_runtime_refuses_disjoint_future_authority_but_reports_remain_available() {
    let pg = fixture().await;
    super::require_supported_scopes(&pg).await.unwrap();
    assert!(matches!(
        super::require_runtime_scopes(&pg).await,
        Err(FamilyError::NotActive)
    ));
    for family in Family::all() {
        super::activate_fixture(&pg, *family).await;
    }
    let known = super::validated_scope(&pg, Family::Satellites)
        .await
        .unwrap();
    let future = Uuid::new_v4();
    sqlx::query("INSERT INTO mirror_backfill_run(id,batch_size,status,scope,source_receipt) VALUES ($1,1,'running','future-documents:1','{}')")
        .bind(future).execute(pg.pool()).await.unwrap();
    super::require_runtime_scopes(&pg).await.unwrap();
    sqlx::query("INSERT INTO mirror_backfill_checkpoint(run_id,mirror,completed_at,verification) VALUES ($1,'table:future_projection',now(),'{}')")
        .bind(future).execute(pg.pool()).await.unwrap();
    sqlx::query(
        "UPDATE mirror_backfill_run SET status = 'completed', completed_at = now() WHERE id = $1",
    )
    .bind(future)
    .execute(pg.pool())
    .await
    .unwrap();
    super::require_scope(&pg, Family::Satellites).await.unwrap();
    assert!(matches!(
        super::require_runtime_scopes(&pg).await,
        Err(FamilyError::Receipt)
    ));
    assert!(
        crate::postgres_backfill::completed_family_report(&pg, Family::Satellites, known)
            .await
            .unwrap()
            .unwrap()
            .is_clean()
    );
    assert!(matches!(
        super::require_supported_scopes(&pg).await,
        Err(FamilyError::Receipt)
    ));
    let unavailable = crate::surreal::SurrealDb::uninitialized();
    assert!(matches!(
        crate::postgres_backfill::start(&unavailable, &pg, 1, None).await,
        Err(crate::postgres_backfill::BackfillError::Family(
            FamilyError::Receipt
        ))
    ));
    assert!(matches!(
        crate::postgres_mirror::reconcile_all(&unavailable, &pg).await,
        Err(crate::postgres_mirror::MirrorError::Family(
            FamilyError::Receipt
        ))
    ));
    let operator = PostgresDb::connect(pg.pool().connect_options().as_ref().clone())
        .await
        .unwrap();
    operator.pool().close().await;
    pg.pool().close().await;
}

#[test]
fn native_asset_extras_cannot_cancel_unknown_rows_or_overlapping_drift() {
    let mut proof = super::AssetUnionProof {
        compared_overlaps: 2,
        resolved_native_overlaps: 1,
        unresolved_overlaps: 1,
        proven_native_target_only: 3,
        unknown_target_only: 1,
        full_metadata_compared: true,
        ..Default::default()
    };
    assert!(proof.valid(2, 6, 2));
    assert!(!proof.valid(2, 6, 0));
    proof.unresolved_overlaps = 0;
    proof.unknown_target_only = 0;
    assert!(proof.valid(2, 5, 0));
    assert!(!proof.valid(2, 6, 0));
    proof.proven_native_target_only = usize::MAX;
    assert!(!proof.valid(2, 0, 0));
}

#[test]
fn lifecycle_accounting_cannot_overflow_or_mask_unresolved_overlap() {
    let proof = super::ProjectLifecycleProof {
        compared_overlaps: 1,
        retained_native_projections: 1,
        full_metadata_compared: true,
        ledger_projections_checked: true,
        ..Default::default()
    };
    assert!(proof.valid(1, 1, 0));
    let unresolved = super::ProjectLifecycleProof {
        unresolved_overlaps: 1,
        ..proof.clone()
    };
    assert!(!unresolved.valid(1, 1, 1));
    let overflow = super::ProjectLifecycleProof {
        source_only_remaining: 1,
        compared_overlaps: usize::MAX,
        full_metadata_compared: true,
        ledger_projections_checked: true,
        ..Default::default()
    };
    assert!(!overflow.valid(0, usize::MAX, 1));
    let unchecked = super::ProjectLifecycleProof {
        ledger_projections_checked: false,
        ..proof
    };
    assert!(!unchecked.valid(1, 1, 0));
}

#[tokio::test]
async fn sealed_scopes_reject_lifecycle_extension_without_changing_their_receipts() {
    for family in [Family::Satellites, Family::Notation] {
        let (options, database) = crate::postgres_mirror::tests::fresh_schema()
            .await
            .expect("required PostgreSQL");
        let pg = crate::postgres_mirror::tests::connect(&options, &database).await;
        super::activate_fixture(&pg, family).await;
        let (run, table, original): (uuid::Uuid, String, serde_json::Value) = sqlx::query_as(
            "SELECT r.id,c.mirror,c.verification FROM mirror_backfill_run r JOIN mirror_backfill_checkpoint c ON c.run_id=r.id WHERE r.scope=$1 ORDER BY c.mirror LIMIT 1")
            .bind(family.scope()).fetch_one(pg.pool()).await.unwrap();
        assert!(original.get("project_lifecycle").is_none());
        let mut corrupted = original.clone();
        corrupted["project_lifecycle"] = serde_json::to_value(super::ProjectLifecycleProof {
            full_metadata_compared: true,
            ledger_projections_checked: true,
            ..Default::default()
        })
        .unwrap();
        sqlx::query(
            "UPDATE mirror_backfill_checkpoint SET verification=$3 WHERE run_id=$1 AND mirror=$2",
        )
        .bind(run)
        .bind(&table)
        .bind(corrupted)
        .execute(pg.pool())
        .await
        .unwrap();
        assert!(matches!(
            super::require_scope(&pg, family).await,
            Err(super::FamilyError::Receipt)
        ));
        sqlx::query(
            "UPDATE mirror_backfill_checkpoint SET verification=$3 WHERE run_id=$1 AND mirror=$2",
        )
        .bind(run)
        .bind(table)
        .bind(original)
        .execute(pg.pool())
        .await
        .unwrap();
        super::require_scope(&pg, family).await.unwrap();
    }
}

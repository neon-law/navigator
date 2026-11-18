//! The real scoped ledger seals the typed eight-table adapter proof and never replays it.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

// Domain evidence is checked by the real-PG Asset classifier tests. This adapter
// double isolates how the real ledger validates, seals, and replays that typed result.
struct CapturedNotation {
    receipt: SourceReceipt,
    asset: Uuid,
    known_native: bool,
    unavailable: AtomicBool,
}

impl Source for CapturedNotation {
    fn snapshot<'a>(&'a self, pg: &'a PostgresDb, family: Family) -> SourceOp<'a, Snapshot> {
        Box::pin(async move {
            assert!(!self.unavailable.load(Ordering::SeqCst));
            let mut tables = Vec::new();
            for table in family.tables() {
                let target: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                    "SELECT count(*) FROM {table}"
                )))
                .fetch_one(pg.pool())
                .await?;
                tables.push(crate::postgres_mirror::TableParity {
                    table,
                    source_rows: usize::try_from(self.receipt.table_counts[*table]).unwrap(),
                    target_rows: usize::try_from(target).unwrap(),
                    divergent: if *table == "asset" && !self.known_native {
                        vec![crate::postgres_mirror::MirrorKey::Asset(self.asset)]
                    } else {
                        Vec::new()
                    },
                });
            }
            Ok(Snapshot {
                schema_sha256: self.receipt.schema_sha256.clone(),
                export_sha256: self.receipt.export_sha256.clone(),
                export_bytes: self.receipt.export_bytes,
                tables,
                raw_counts: self
                    .receipt
                    .table_counts
                    .iter()
                    .map(|(table, count)| (table.clone(), usize::try_from(*count).unwrap()))
                    .collect(),
                references: vec![],
                reset_fact_differences: vec![],
                foreign_keys_hold: foreign_keys_hold(pg, family).await?,
                asset_union: Some(AssetUnionProof {
                    proven_native_target_only: usize::from(self.known_native),
                    unknown_target_only: usize::from(!self.known_native),
                    full_metadata_compared: true,
                    ..Default::default()
                }),
                reset_union: Some(ResetUnionProof {
                    compared: 1,
                    retained_native_protections: 0,
                }),
                template_pins: Some(TemplatePinProof {
                    compared: 1,
                    differences: 0,
                }),
            })
        })
    }

    fn write<'a>(
        &'a self,
        _pg: &'a PostgresDb,
        _key: &'a crate::postgres_mirror::MirrorKey,
    ) -> SourceOp<'a, ()> {
        // A target-only row is never deleted or guessed into Source by the importer.
        Box::pin(async { Err(crate::postgres_mirror::MirrorError::Source.into()) })
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one real enrollment and completed-replay protocol journey"
)]
async fn handoff_case(known_native: bool, old_global_complete: bool) {
    use sha2::Digest;
    let (options, database) = crate::postgres_mirror::tests::fresh_schema()
        .await
        .expect("required PostgreSQL");
    let pg = crate::postgres_mirror::tests::connect(&options, &database).await;
    let (_, notation, _) = crate::postgres::fixtures::workflow_fixture(&pg).await;
    activate_fixture(&pg, Family::Satellites).await;
    let satellite: serde_json::Value = sqlx::query_scalar(
        "SELECT verification FROM mirror_backfill_checkpoint WHERE mirror='table:playbook'",
    )
    .fetch_one(pg.pool())
    .await
    .unwrap();
    assert!(satellite.get("asset_union").is_none());
    assert!(satellite.get("template_pins").is_none());
    if old_global_complete {
        sqlx::query("INSERT INTO mirror_backfill_run(id,batch_size,status,completed_at) VALUES($1,1,'completed',now())")
            .bind(Uuid::new_v4()).execute(pg.pool()).await.unwrap();
    }
    let asset = Uuid::new_v4();
    sqlx::query("INSERT INTO asset(id,storage_key,content_type,byte_size,sha256_hex) VALUES($1,'blobs/native','text/plain',1,'synthetic')")
        .bind(asset).execute(pg.pool()).await.unwrap();
    let export = b"synthetic frozen Notation source adapter";
    let receipt = SourceReceipt {
        format_version: 1,
        consistency: "operator_quiesced".into(),
        export_bytes: u64::try_from(export.len()).unwrap(),
        export_sha256: hex::encode(sha2::Sha256::digest(export)),
        schema_sha256: "ab".repeat(32),
        table_counts: Family::Notation
            .tables()
            .iter()
            .map(|table| ((*table).into(), i64::from(*table == "notation")))
            .collect(),
    };
    let source = CapturedNotation {
        receipt: receipt.clone(),
        asset,
        known_native,
        unavailable: AtomicBool::new(false),
    };
    let run = Uuid::new_v4();
    enroll(
        &pg,
        run,
        Family::Notation,
        1,
        receipt.verify_export(export).unwrap(),
    )
    .await
    .unwrap();
    let advance = during_import(
        run,
        Family::Notation,
        advance_with(&source, &pg, run, None, true),
    )
    .await
    .unwrap();
    let snapshot = advance.snapshot.unwrap();
    assert_eq!(snapshot.clean(), known_native);
    if known_native {
        require_scope(&pg, Family::Notation).await.unwrap();
        let (tables, _) = completed_proof(&pg, Family::Notation, run).await.unwrap();
        let proof = tables.iter().find(|table| table.table == "asset").unwrap();
        assert_eq!((proof.surreal, proof.postgres), (0, 1));
        assert_eq!(
            proof
                .asset_union
                .as_ref()
                .unwrap()
                .proven_native_target_only,
            1
        );
        assert_eq!(
            crate::postgres::notations::delete_draft(&pg, notation)
                .await
                .unwrap(),
            crate::notation_reset::ResetOutcome::Deleted
        );
        source.unavailable.store(true, Ordering::SeqCst);
        assert!(during_import(
            run,
            Family::Notation,
            advance_with(&source, &pg, run, None, true)
        )
        .await
        .unwrap()
        .snapshot
        .is_none());
        let absent = crate::surreal::SurrealDb::uninitialized();
        for key in [
            crate::postgres_mirror::MirrorKey::Notation(notation),
            crate::postgres_mirror::MirrorKey::Asset(asset),
        ] {
            assert!(matches!(
                crate::postgres_mirror::during_initial_copy(crate::postgres_mirror::mirror(
                    &absent, &pg, &key
                ))
                .await,
                Err(crate::postgres_mirror::MirrorError::MatterAuthority)
            ));
        }
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM notation WHERE id=$1")
            .bind(notation)
            .fetch_one(pg.pool())
            .await
            .unwrap();
        assert_eq!(rows, 0);
    } else {
        assert!(matches!(
            require_scope(&pg, Family::Notation).await,
            Err(FamilyError::NotActive)
        ));
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM asset WHERE id=$1")
            .bind(asset)
            .fetch_one(pg.pool())
            .await
            .unwrap();
        assert_eq!(rows, 1);
    }
    let after: serde_json::Value = sqlx::query_scalar(
        "SELECT verification FROM mirror_backfill_checkpoint WHERE mirror='table:playbook'",
    )
    .fetch_one(pg.pool())
    .await
    .unwrap();
    assert_eq!(after, satellite);
    pg.pool().close().await;
}

#[tokio::test]
async fn unknown_assets_block_and_native_union_seals_after_fresh_or_completed_global_import() {
    handoff_case(false, false).await;
    handoff_case(true, false).await;
    handoff_case(true, true).await;
}

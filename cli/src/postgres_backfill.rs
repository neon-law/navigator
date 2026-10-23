//! `navigator ops postgres backfill` — copy the `SurrealDB` rows the
//! PostgreSQL mirror never saw, in bounded batches a later invocation can
//! resume.
//!
//! `--dry-run` reads both stores and prints the plan: per table, the rows a
//! run would write and how many batches they need, plus the differing and
//! PostgreSQL-only rows a run leaves to `ops postgres mirror reconcile`. It
//! writes nothing, not even a run record. Without it, a run is recorded in
//! `mirror_backfill_run` and checkpointed per mirror group; `--max-batches`
//! stops after that many batches, and `--resume <run-id>` continues it.
//!
//! Rows are named by UUID only and failures by table, UUID, and a sanitized
//! reason; no column value leaves either store. It reads the same
//! `NAVIGATOR_SURREAL_*` and `NAVIGATOR_POSTGRES_*` variables `web` reads.
//!
//! Exit status: 0 when nothing is left to write, 1 when a plan has rows to
//! write or a completed run recorded failures, 2 when a store is
//! unavailable or the run could not advance, 3 when a run paused at its
//! batch limit.

use std::process::ExitCode;

use clap::Args;
use serde_json::json;
use store::postgres::PostgresDb;
use store::postgres_backfill::{BackfillError, Plan, RunReport, RunStatus, DEFAULT_BATCH_SIZE};
use uuid::Uuid;

/// `navigator ops postgres backfill`.
#[derive(Debug, Args)]
pub struct BackfillArgs {
    /// Print what a run would write; write nothing.
    #[arg(long, conflicts_with_all = ["resume", "max_batches"])]
    dry_run: bool,
    /// Verify document asset bytes/digests plus public and private blob-key references (requires --dry-run).
    #[arg(long, requires = "dry_run")]
    verify_assets: bool,
    /// Rows written per checkpointed batch, for a new run.
    #[arg(long, default_value_t = DEFAULT_BATCH_SIZE, conflicts_with = "resume")]
    batch_size: usize,
    /// Stop after this many batches; the run stays resumable.
    #[arg(long)]
    max_batches: Option<usize>,
    /// Continue the run with this id from its checkpoints.
    #[arg(long, value_name = "RUN_ID")]
    resume: Option<Uuid>,
    /// Print one JSON object instead of text.
    #[arg(long)]
    json: bool,
}

const UNAVAILABLE: u8 = 2;
const PAUSED: u8 = 3;

enum Outcome {
    Plan {
        plan: Plan,
        assets: Option<store::postgres_backfill::AssetObjectVerification>,
    },
    Run(RunReport),
}

/// Run one `ops postgres backfill` command.
pub fn run(args: &BackfillArgs) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("navigator: could not start a runtime: {error}");
            return ExitCode::from(UNAVAILABLE);
        }
    };
    match runtime.block_on(execute(args)) {
        Ok(Outcome::Plan { plan, assets }) => {
            println!(
                "{}",
                if args.json {
                    plan_json(&plan, assets.as_ref()).to_string()
                } else {
                    plan_text(&plan, assets.as_ref())
                }
            );
            ExitCode::from(u8::from(
                !plan.is_clean() || assets.as_ref().is_some_and(|report| !report.is_clean()),
            ))
        }
        Ok(Outcome::Run(report)) => {
            println!(
                "{}",
                if args.json {
                    run_json(&report).to_string()
                } else {
                    run_text(&report)
                }
            );
            match report.status {
                RunStatus::Paused => ExitCode::from(PAUSED),
                RunStatus::Completed => ExitCode::from(u8::from(!report.is_clean())),
            }
        }
        Err(error) => {
            if args.json {
                println!("{}", error_json(&error));
            } else {
                eprintln!("navigator: backfill: {error}");
            }
            ExitCode::from(UNAVAILABLE)
        }
    }
}

async fn execute(args: &BackfillArgs) -> anyhow::Result<Outcome> {
    let surreal = store::surreal::connect_from_env()
        .await
        .map_err(|_| anyhow::anyhow!("SurrealDB is unavailable"))?;
    let pg = PostgresDb::from_env()
        .await
        .map_err(|_| anyhow::anyhow!("PostgreSQL is unavailable"))?;
    Ok(if args.dry_run {
        let plan = store::postgres_backfill::plan(&surreal, &pg, args.batch_size).await?;
        let assets = if args.verify_assets {
            let document_storage = cloud::from_env()
                .await
                .map_err(|_| anyhow::anyhow!("document storage is unavailable"))?;
            let public_assets_storage = cloud::assets_from_env()
                .await
                .map_err(|_| anyhow::anyhow!("public assets storage is unavailable"))?;
            Some(
                store::postgres_backfill::verify_asset_objects(
                    &surreal,
                    &pg,
                    document_storage.as_ref(),
                    public_assets_storage.as_ref(),
                )
                .await?,
            )
        } else {
            None
        };
        Outcome::Plan { plan, assets }
    } else if let Some(run_id) = args.resume {
        Outcome::Run(
            store::postgres_backfill::resume(&surreal, &pg, run_id, args.max_batches).await?,
        )
    } else {
        Outcome::Run(
            store::postgres_backfill::start(&surreal, &pg, args.batch_size, args.max_batches)
                .await?,
        )
    })
}

fn error_json(error: &anyhow::Error) -> serde_json::Value {
    if let Some(BackfillError::UncoveredTables(tables)) = error.downcast_ref::<BackfillError>() {
        json!({
            "mode": "backfill",
            "error": "populated source tables lack PostgreSQL mirror coverage",
            "canonical_import_coverage": false,
            "archived_raw_data_is_import_coverage": false,
            "uncovered_tables": tables.iter().map(|table| json!({
                "table": table.table,
                "rows": table.rows,
            })).collect::<Vec<_>>(),
        })
    } else {
        json!({"mode": "backfill", "error": error.to_string()})
    }
}

fn plan_json(
    plan: &Plan,
    assets: Option<&store::postgres_backfill::AssetObjectVerification>,
) -> serde_json::Value {
    let mut result = json!({
        "mode": "backfill-dry-run",
        "batch_size": plan.batch_size,
        "missing": plan.missing(),
        "tables": plan.tables.iter().map(|table| json!({
            "mirror": table.mirror,
            "table": table.table,
            "surreal": table.surreal,
            "postgres": table.postgres,
            "missing": table.missing,
            "differing": table.differing,
            "extra": table.extra,
            "batches": table.batches,
            "missing_ids": table.missing_ids,
            "unnamed": table.unnamed,
        })).collect::<Vec<_>>(),
        "unresolved_references": plan.unresolved_references.iter().map(|reference| json!({
            "table": reference.table,
            "field": reference.field,
            "source_orphans": reference.source_orphans,
            "postgres_missing": reference.postgres_missing,
        })).collect::<Vec<_>>(),
    });
    if let Some(assets) = assets {
        result["asset_objects"] = json!({
            "clean": assets.is_clean(),
            "source_assets": assets.source_assets,
            "target_assets": assets.target_assets,
            "unique_objects": assets.unique_objects,
            "verified_bytes": assets.verified_bytes,
            "missing_objects": assets.missing_objects,
            "unreadable_objects": assets.unreadable_objects,
            "size_mismatches": assets.size_mismatches,
            "digest_mismatches": assets.digest_mismatches,
            "conflicting_expectations": assets.conflicting_expectations,
            "invalid_byte_sizes": assets.invalid_byte_sizes,
            "purged_assets": assets.purged_assets,
            "public_reference_objects": assets.public_reference_objects,
            "public_reference_bytes": assets.public_reference_bytes,
            "public_missing_objects": assets.public_missing_objects,
            "public_unreadable_objects": assets.public_unreadable_objects,
            "private_reference_objects": assets.private_reference_objects,
            "private_reference_bytes": assets.private_reference_bytes,
            "private_missing_objects": assets.private_missing_objects,
            "private_unreadable_objects": assets.private_unreadable_objects,
            "invalid_object_keys": assets.invalid_object_keys,
        });
    }
    result
}

fn plan_text(
    plan: &Plan,
    assets: Option<&store::postgres_backfill::AssetObjectVerification>,
) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:<28} {:<28} {:>8} {:>8} {:>8} {:>9} {:>6} {:>8}",
        "mirror", "table", "surreal", "postgres", "missing", "differing", "extra", "batches"
    );
    for table in &plan.tables {
        let _ = writeln!(
            out,
            "{:<28} {:<28} {:>8} {:>8} {:>8} {:>9} {:>6} {:>8}",
            table.mirror,
            table.table,
            table.surreal,
            table.postgres,
            table.missing,
            table.differing,
            table.extra,
            table.batches
        );
        for id in &table.missing_ids {
            let _ = writeln!(out, "    {id}");
        }
        if table.unnamed > 0 {
            let _ = writeln!(out, "    {} row(s) keyed by Brand key", table.unnamed);
        }
    }
    if !plan.unresolved_references.is_empty() {
        let _ = writeln!(out, "unresolved references:");
        for reference in &plan.unresolved_references {
            let _ = writeln!(
                out,
                "  {}.{}: {} source orphan(s), {} missing PostgreSQL target(s)",
                reference.table,
                reference.field,
                reference.source_orphans,
                reference.postgres_missing
            );
        }
    }
    let _ = write!(
        out,
        "dry run: {} row(s) to write in batches of {}; differing and PostgreSQL-only rows are left to reconcile",
        plan.missing(),
        plan.batch_size
    );
    if let Some(assets) = assets {
        let _ = write!(
            out,
            "\ndocument asset objects: {} digest-verified byte(s), {} unique object(s), {} missing, {} unreadable, {} size mismatch(es), {} digest mismatch(es), {} conflicting expectation(s), {} invalid byte size(s), {} purged asset(s); public references: {} objects / {} byte(s), {} missing, {} unreadable; private references: {} objects / {} byte(s), {} missing, {} unreadable; {} invalid empty key(s)",
            assets.verified_bytes,
            assets.unique_objects,
            assets.missing_objects,
            assets.unreadable_objects,
            assets.size_mismatches,
            assets.digest_mismatches,
            assets.conflicting_expectations,
            assets.invalid_byte_sizes,
            assets.purged_assets,
            assets.public_reference_objects,
            assets.public_reference_bytes,
            assets.public_missing_objects,
            assets.public_unreadable_objects,
            assets.private_reference_objects,
            assets.private_reference_bytes,
            assets.private_missing_objects,
            assets.private_unreadable_objects,
            assets.invalid_object_keys
        );
    }
    out
}

fn run_json(report: &RunReport) -> serde_json::Value {
    json!({
        "mode": "backfill",
        "run_id": report.run_id,
        "status": report.status.label(),
        "clean": report.is_clean(),
        "batch_size": report.batch_size,
        "batches": report.batches_this_invocation,
        "groups": report.groups.iter().map(|group| json!({
            "mirror": group.mirror,
            "batches": group.batches,
            "applied": group.applied,
            "failed": group.failed,
            "completed": group.completed,
        })).collect::<Vec<_>>(),
        "failures": report.failures.iter().map(|failure| json!({
            "mirror": failure.mirror,
            "table": failure.table,
            "id": failure.id,
            "reason": failure.reason,
        })).collect::<Vec<_>>(),
        "unresolved_references": report.unresolved_references.iter().map(|reference| json!({
            "table": reference.table,
            "field": reference.field,
            "source_orphans": reference.source_orphans,
            "postgres_missing": reference.postgres_missing,
        })).collect::<Vec<_>>(),
        "parity": report.parity.iter().map(|table| json!({
            "mirror": table.mirror,
            "table": table.table,
            "surreal": table.surreal,
            "postgres": table.postgres,
            "missing": table.missing,
            "differing": table.differing,
            "extra": table.extra,
            "divergent_ids": table.divergent_ids,
            "omitted_divergent_ids": table.omitted_divergent_ids,
            "withheld_non_uuid_keys": table.withheld_non_uuid_keys,
        })).collect::<Vec<_>>(),
    })
}

fn run_text(report: &RunReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "run {} {} ({} batch(es) this invocation, batch size {})",
        report.run_id,
        report.status.label(),
        report.batches_this_invocation,
        report.batch_size
    );
    for group in &report.groups {
        let _ = writeln!(
            out,
            "{:<28} batches {:>6} applied {:>8} failed {:>6}{}",
            group.mirror,
            group.batches,
            group.applied,
            group.failed,
            if group.completed {
                ""
            } else {
                " (in progress)"
            }
        );
    }
    for failure in &report.failures {
        let _ = writeln!(
            out,
            "    {} {} {}: {}",
            failure.mirror,
            failure.table,
            failure.id.as_deref().unwrap_or("(Brand-keyed row)"),
            failure.reason
        );
    }
    for reference in &report.unresolved_references {
        let _ = writeln!(
            out,
            "unresolved reference {}.{}: {} source orphan(s), {} missing PostgreSQL target(s)",
            reference.table, reference.field, reference.source_orphans, reference.postgres_missing
        );
    }
    for table in &report.parity {
        if table.missing > 0 || table.differing > 0 || table.extra > 0 {
            let _ = writeln!(
                out,
                "parity {} {}: missing {} differing {} extra {}",
                table.mirror, table.table, table.missing, table.differing, table.extra
            );
            for id in &table.divergent_ids {
                let _ = writeln!(out, "    {id}");
            }
            if table.omitted_divergent_ids > 0 {
                let _ = writeln!(
                    out,
                    "    {} additional UUID key(s) omitted",
                    table.omitted_divergent_ids
                );
            }
            if table.withheld_non_uuid_keys > 0 {
                let _ = writeln!(
                    out,
                    "    {} non-UUID key(s) withheld",
                    table.withheld_non_uuid_keys
                );
            }
        }
    }
    let _ = match (report.status, report.failures.len()) {
        (RunStatus::Paused, _) => write!(out, "paused: resume with --resume {}", report.run_id),
        (RunStatus::Completed, 0) if report.is_clean() => {
            write!(out, "completed: all mirrored tables are in parity")
        }
        (RunStatus::Completed, 0) => write!(out, "completed with parity mismatches"),
        (RunStatus::Completed, failed) => {
            write!(out, "completed with {failed} row(s) still missing")
        }
    };
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use store::postgres_backfill::{Failure, GroupProgress, TablePlan};

    #[derive(Debug, Parser)]
    struct Cli {
        #[command(flatten)]
        args: BackfillArgs,
    }

    #[test]
    fn flags_parse_and_conflicting_modes_are_refused() {
        let fresh = Cli::try_parse_from(["backfill"]).unwrap().args;
        assert_eq!(fresh.batch_size, DEFAULT_BATCH_SIZE);
        assert!(!fresh.dry_run && fresh.resume.is_none());
        let run_id = Uuid::new_v4();
        let resumed = Cli::try_parse_from([
            "backfill",
            "--resume",
            &run_id.to_string(),
            "--max-batches",
            "4",
        ])
        .unwrap()
        .args;
        assert_eq!(
            (resumed.resume, resumed.max_batches),
            (Some(run_id), Some(4))
        );
        assert!(
            Cli::try_parse_from(["backfill", "--dry-run", "--resume", &run_id.to_string()])
                .is_err()
        );
        assert!(Cli::try_parse_from(["backfill", "--dry-run", "--max-batches", "1"]).is_err());
        assert!(Cli::try_parse_from(["backfill", "--verify-assets"]).is_err());
        assert!(
            Cli::try_parse_from(["backfill", "--dry-run", "--verify-assets"])
                .unwrap()
                .args
                .verify_assets
        );
        assert!(Cli::try_parse_from(["backfill", "--resume", "not-a-uuid"]).is_err());
    }

    #[test]
    fn reports_uncovered_source_tables_as_table_and_count_only() {
        let error = anyhow::Error::new(BackfillError::UncoveredTables(vec![
            store::postgres_backfill::UncoveredTable {
                table: "unregistered_table".into(),
                rows: 3,
            },
        ]));
        let rendered = error_json(&error);
        assert_eq!(rendered["canonical_import_coverage"], false);
        assert_eq!(rendered["archived_raw_data_is_import_coverage"], false);
        assert_eq!(
            rendered["uncovered_tables"][0]["table"],
            "unregistered_table"
        );
        assert_eq!(rendered["uncovered_tables"][0]["rows"], 3);
        assert_eq!(
            rendered["uncovered_tables"][0].as_object().unwrap().len(),
            2
        );
    }

    #[test]
    fn object_verification_reports_each_storage_lane_without_exposing_keys() {
        use store::postgres_backfill::AssetObjectVerification;
        let plan = Plan {
            batch_size: 10,
            tables: vec![],
            unresolved_references: vec![],
        };
        let clean = AssetObjectVerification {
            source_assets: 2,
            target_assets: 2,
            unique_objects: 2,
            verified_bytes: 37,
            purged_assets: 1,
            public_reference_objects: 3,
            public_reference_bytes: 41,
            private_reference_objects: 4,
            private_reference_bytes: 43,
            ..AssetObjectVerification::default()
        };
        let report = plan_json(&plan, Some(&clean));
        assert_eq!(report["asset_objects"]["clean"], true);
        assert_eq!(report["asset_objects"]["verified_bytes"], 37);
        assert_eq!(report["asset_objects"]["public_reference_bytes"], 41);
        assert_eq!(report["asset_objects"]["private_reference_bytes"], 43);
        assert_eq!(report["asset_objects"]["purged_assets"], 1);
        let text = plan_text(&plan, Some(&clean));
        assert!(text.contains("37 digest-verified byte(s), 2 unique object(s)"));
        assert!(text.contains("public references: 3 objects / 41 byte(s)"));
        assert!(text.contains("private references: 4 objects / 43 byte(s)"));
        assert!(plan_json(&plan, None).get("asset_objects").is_none());

        let broken = AssetObjectVerification {
            missing_objects: 1,
            unreadable_objects: 2,
            size_mismatches: 3,
            digest_mismatches: 4,
            conflicting_expectations: 5,
            invalid_byte_sizes: 6,
            public_missing_objects: 7,
            public_unreadable_objects: 8,
            private_missing_objects: 9,
            private_unreadable_objects: 10,
            invalid_object_keys: 11,
            ..clean
        };
        let report = plan_json(&plan, Some(&broken));
        assert_eq!(report["asset_objects"]["clean"], false);
        for (field, expected) in [
            ("missing_objects", 1),
            ("unreadable_objects", 2),
            ("size_mismatches", 3),
            ("digest_mismatches", 4),
            ("conflicting_expectations", 5),
            ("invalid_byte_sizes", 6),
            ("public_missing_objects", 7),
            ("public_unreadable_objects", 8),
            ("private_missing_objects", 9),
            ("private_unreadable_objects", 10),
            ("invalid_object_keys", 11),
        ] {
            assert_eq!(report["asset_objects"][field], expected);
        }
        assert!(plan_text(&plan, Some(&broken)).contains("11 invalid empty key(s)"));
        assert_eq!(report["asset_objects"].as_object().unwrap().len(), 21);
    }

    #[test]
    fn reports_name_rows_by_uuid_and_reason_only() {
        let id = Uuid::new_v4().to_string();
        let plan = Plan {
            batch_size: 2,
            unresolved_references: vec![store::postgres_mirror::records::UnresolvedReference {
                table: "relationship",
                field: "out",
                source_orphans: 1,
                postgres_missing: 1,
            }],
            tables: vec![TablePlan {
                mirror: "identity",
                table: "person",
                surreal: 3,
                postgres: 0,
                missing: 3,
                differing: 0,
                extra: 0,
                batches: 2,
                missing_ids: vec![id.clone()],
                unnamed: 2,
            }],
        };
        let rendered = plan_json(&plan, None);
        assert_eq!(rendered["missing"], 3);
        assert_eq!(rendered["tables"][0]["missing_ids"][0], id.as_str());
        assert_eq!(rendered["unresolved_references"][0]["source_orphans"], 1);
        assert_eq!(rendered["unresolved_references"][0]["postgres_missing"], 1);
        assert!(plan_text(&plan, None).contains("2 row(s) keyed by Brand key"));
        assert!(plan_text(&plan, None).contains("relationship.out: 1 source orphan(s)"));

        let report = RunReport {
            run_id: Uuid::nil(),
            status: RunStatus::Completed,
            batch_size: 2,
            batches_this_invocation: 2,
            groups: vec![GroupProgress {
                mirror: "identity".into(),
                batches: 2,
                applied: 2,
                failed: 1,
                completed: true,
            }],
            failures: vec![Failure {
                mirror: "identity".into(),
                table: "entity".into(),
                id: Some(id.clone()),
                reason: "PostgreSQL refused the row (SQLSTATE 23503)".into(),
            }],
            parity: vec![store::postgres_backfill::ParityTableReport {
                mirror: "identity".into(),
                table: "entity".into(),
                surreal: 1,
                postgres: 0,
                missing: 1,
                differing: 0,
                extra: 0,
                divergent_ids: vec![id.clone()],
                omitted_divergent_ids: 0,
                withheld_non_uuid_keys: 0,
            }],
            unresolved_references: vec![],
        };
        let rendered = run_json(&report);
        assert_eq!(rendered["status"], "completed");
        assert_eq!(rendered["clean"], false);
        assert_eq!(rendered["failures"][0]["id"], id.as_str());
        assert_eq!(rendered["parity"][0]["missing"], 1);
        assert_eq!(rendered["parity"][0]["divergent_ids"][0], id.as_str());
        assert!(run_text(&report).ends_with("completed with 1 row(s) still missing"));
        let paused = RunReport {
            status: RunStatus::Paused,
            failures: vec![],
            ..report
        };
        assert!(run_text(&paused).contains("resume with --resume"));
    }

    #[test]
    fn parity_mismatches_make_a_completed_report_unclean() {
        let id = Uuid::new_v4().to_string();
        let report = RunReport {
            run_id: Uuid::nil(),
            status: RunStatus::Completed,
            batch_size: 10,
            batches_this_invocation: 0,
            groups: vec![],
            failures: vec![],
            parity: vec![store::postgres_backfill::ParityTableReport {
                mirror: "identity".into(),
                table: "person".into(),
                surreal: 1,
                postgres: 1,
                missing: 0,
                differing: 1,
                extra: 0,
                divergent_ids: vec![id.clone()],
                omitted_divergent_ids: 0,
                withheld_non_uuid_keys: 0,
            }],
            unresolved_references: vec![],
        };
        assert!(!report.is_clean());
        let rendered = run_json(&report);
        assert_eq!(rendered["clean"], false);
        assert_eq!(rendered["parity"][0]["differing"], 1);
        assert!(run_text(&report).contains("parity identity person: missing 0 differing 1 extra 0"));
        assert!(run_text(&report).contains(&id));
    }

    fn asset_verification() -> store::postgres_backfill::AssetObjectVerification {
        store::postgres_backfill::AssetObjectVerification {
            source_assets: 4,
            target_assets: 3,
            unique_objects: 3,
            verified_bytes: 2048,
            missing_objects: 1,
            unreadable_objects: 0,
            size_mismatches: 0,
            digest_mismatches: 0,
            conflicting_expectations: 0,
            invalid_byte_sizes: 0,
            purged_assets: 1,
            public_reference_objects: 2,
            public_reference_bytes: 512,
            public_missing_objects: 0,
            public_unreadable_objects: 0,
            private_reference_objects: 1,
            private_reference_bytes: 64,
            private_missing_objects: 0,
            private_unreadable_objects: 0,
            invalid_object_keys: 2,
        }
    }

    #[test]
    fn asset_verification_is_reported_by_count_only() {
        let plan = Plan {
            batch_size: 50,
            unresolved_references: vec![],
            tables: vec![],
        };
        let assets = asset_verification();
        let rendered = plan_json(&plan, Some(&assets));
        let objects = &rendered["asset_objects"];
        assert_eq!(objects["clean"], false);
        assert_eq!(objects["verified_bytes"], 2048);
        assert_eq!(objects["missing_objects"], 1);
        assert_eq!(objects["purged_assets"], 1);
        assert_eq!(objects["public_reference_bytes"], 512);
        assert_eq!(objects["private_reference_objects"], 1);
        assert_eq!(objects["invalid_object_keys"], 2);
        assert!(objects
            .as_object()
            .unwrap()
            .values()
            .all(|value| value.is_number() || value.is_boolean()));
        assert!(plan_json(&plan, None).get("asset_objects").is_none());

        let text = plan_text(&plan, Some(&assets));
        assert!(text.contains("dry run: 0 row(s) to write in batches of 50"));
        assert!(text.contains("document asset objects: 2048 digest-verified byte(s)"));
        assert!(text.contains("1 missing"));
        assert!(text.contains("public references: 2 objects / 512 byte(s)"));
        assert!(text.contains("private references: 1 objects / 64 byte(s)"));
        assert!(text.ends_with("2 invalid empty key(s)"));
        assert!(!text.contains("unresolved references"));
        assert!(!plan_text(&plan, None).contains("document asset objects"));
    }

    #[test]
    fn other_errors_render_only_their_message() {
        let rendered = error_json(&anyhow::anyhow!("PostgreSQL is unavailable"));
        assert_eq!(
            rendered,
            json!({"mode": "backfill", "error": "PostgreSQL is unavailable"})
        );
    }

    #[test]
    fn a_clean_run_reports_parity_and_omitted_keys_are_counted() {
        let clean = RunReport {
            run_id: Uuid::nil(),
            status: RunStatus::Completed,
            batch_size: 10,
            batches_this_invocation: 1,
            groups: vec![GroupProgress {
                mirror: "identity".into(),
                batches: 1,
                applied: 1,
                failed: 0,
                completed: false,
            }],
            failures: vec![],
            parity: vec![store::postgres_backfill::ParityTableReport {
                mirror: "identity".into(),
                table: "person".into(),
                surreal: 1,
                postgres: 1,
                missing: 0,
                differing: 0,
                extra: 0,
                divergent_ids: vec![],
                omitted_divergent_ids: 0,
                withheld_non_uuid_keys: 0,
            }],
            unresolved_references: vec![],
        };
        assert!(clean.is_clean());
        let text = run_text(&clean);
        assert!(text.contains("(in progress)"));
        assert!(!text.contains("parity identity person"));
        assert!(text.ends_with("completed: all mirrored tables are in parity"));
        assert_eq!(run_json(&clean)["clean"], true);

        let divergent = RunReport {
            parity: vec![store::postgres_backfill::ParityTableReport {
                mirror: "identity".into(),
                table: "brand".into(),
                surreal: 5,
                postgres: 9,
                missing: 0,
                differing: 0,
                extra: 4,
                divergent_ids: vec![],
                omitted_divergent_ids: 3,
                withheld_non_uuid_keys: 1,
            }],
            unresolved_references: vec![store::postgres_mirror::records::UnresolvedReference {
                table: "relationship",
                field: "in",
                source_orphans: 2,
                postgres_missing: 0,
            }],
            ..clean
        };
        let text = run_text(&divergent);
        assert!(text.contains("parity identity brand: missing 0 differing 0 extra 4"));
        assert!(text.contains("3 additional UUID key(s) omitted"));
        assert!(text.contains("1 non-UUID key(s) withheld"));
        assert!(text.contains("unresolved reference relationship.in: 2 source orphan(s)"));
        assert!(text.ends_with("completed with parity mismatches"));
        let rendered = run_json(&divergent);
        assert_eq!(rendered["parity"][0]["omitted_divergent_ids"], 3);
        assert_eq!(rendered["parity"][0]["withheld_non_uuid_keys"], 1);
        assert_eq!(rendered["unresolved_references"][0]["field"], "in");
    }
}

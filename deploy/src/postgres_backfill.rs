//! `hudson ops postgres backfill` — copy the `SurrealDB` rows the
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

/// `hudson ops postgres backfill`.
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
    #[command(flatten)]
    enrollment: FamilyArgs,
    /// Print one JSON object instead of text.
    #[arg(long)]
    json: bool,
}

/// Explicit capture and activation options belonging to one immutable family.
#[derive(Debug, Args)]
struct FamilyArgs {
    /// Enroll the exact versioned family, such as satellites:1 (never a backend toggle).
    #[arg(long, conflicts_with = "dry_run")]
    family: Option<String>,
    /// The canonical strict export captured while all compatibility writers remain stopped.
    #[arg(long, requires_all = ["family", "capture_receipt"])]
    capture: Option<std::path::PathBuf>,
    /// The existing archive receipt paired with --capture.
    #[arg(long, requires_all = ["family", "capture"])]
    capture_receipt: Option<std::path::PathBuf>,
    /// Replace a rejected pending capture after a new verified frozen export.
    #[arg(long, requires_all = ["resume", "capture"])]
    refresh_capture: bool,
    /// Activate this family only after complete final parity and reference verification.
    #[arg(long, requires = "family")]
    activate: bool,
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
    let pg = PostgresDb::from_env()
        .await
        .map_err(|_| anyhow::anyhow!("PostgreSQL is unavailable"))?;
    store::postgres_backfill::family::require_capture_target(&pg).await?;
    if let Some(scope) = &args.enrollment.family {
        return execute_family(args, &pg, scope).await.map(Outcome::Run);
    }
    if let Some(run) = args.resume {
        anyhow::ensure!(
            store::postgres_backfill::report(&pg, run).await?.scope == "global",
            "scoped run requires its explicit --family"
        );
    }
    if !args.dry_run {
        store::postgres_backfill::family::require_supported_scopes(&pg).await?;
    }
    let surreal = store::surreal::connect_from_env()
        .await
        .map_err(|_| anyhow::anyhow!("SurrealDB is unavailable"))?;
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

async fn execute_family(
    args: &BackfillArgs,
    pg: &PostgresDb,
    scope: &str,
) -> anyhow::Result<RunReport> {
    use store::postgres_backfill::{self, family::Family};
    postgres_backfill::family::require_capture_target(pg).await?;
    let family = Family::from_scope(scope)
        .ok_or_else(|| anyhow::anyhow!("unknown immutable family scope"))?;
    if let Some(run) = args.resume {
        if let Some(report) = postgres_backfill::completed_family_report(pg, family, run).await? {
            anyhow::ensure!(
                !args.enrollment.refresh_capture && args.enrollment.capture.is_none(),
                "activated captures are immutable"
            );
            return Ok(report);
        }
    }
    let receipt = match (&args.enrollment.capture, &args.enrollment.capture_receipt) {
        (Some(export), Some(receipt)) => {
            let receipt: store::postgres_backfill::family::SourceReceipt =
                serde_json::from_reader(std::fs::File::open(receipt)?)?;
            Some(receipt.verify_export_reader(std::fs::File::open(export)?)?)
        }
        (None, None) => None,
        _ => anyhow::bail!("capture export and receipt must be supplied together"),
    };
    anyhow::ensure!(
        args.resume.is_some() || receipt.is_some(),
        "new family enrollment requires a verified capture"
    );
    anyhow::ensure!(
        args.resume.is_none() || args.enrollment.refresh_capture == receipt.is_some(),
        "pending capture replacement requires --refresh-capture"
    );
    // Family proofs fingerprint export bytes through the same backup surface as strict captures.
    let config = store::surreal::SurrealConfig::from_env()
        .map_err(|_| anyhow::anyhow!("SurrealDB is unavailable"))?;
    let surreal = crate::surreal_archive::connect_http(&config)
        .await
        .map_err(|_| anyhow::anyhow!("SurrealDB is unavailable"))?;
    match (args.resume, receipt) {
        (Some(run), Some(receipt)) => Ok(postgres_backfill::refresh_family(
            &surreal,
            pg,
            family,
            run,
            receipt,
            args.max_batches,
            args.enrollment.activate,
        )
        .await?),
        (Some(run), None) => Ok(postgres_backfill::resume_family(
            &surreal,
            pg,
            family,
            run,
            args.max_batches,
            args.enrollment.activate,
        )
        .await?),
        (None, Some(receipt)) => Ok(postgres_backfill::start_family(
            &surreal,
            pg,
            family,
            receipt,
            args.batch_size,
            args.max_batches,
            args.enrollment.activate,
        )
        .await?),
        (None, None) => anyhow::bail!("new family enrollment requires a verified capture"),
    }
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
        "source_tables": plan.source_tables.iter().map(|table| json!({
            "table": table.table,
            "rows": table.rows,
            "disposition": table.disposition.label(),
        })).collect::<Vec<_>>(),
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
    let _ = writeln!(out, "source inventory:");
    for table in &plan.source_tables {
        let _ = writeln!(
            out,
            "  {}: {} row(s), {}",
            table.table,
            table.rows,
            table.disposition.label()
        );
    }
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
        "scope": report.scope,
        "parity_basis": if report.scope == "global" { "live_source" } else { "verified_capture" },
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
            "newly_target_missing": reference.newly_target_missing,
        })).collect::<Vec<_>>(),
        "parity": report.parity.iter().map(|table| { let mut value = json!({
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
        });
            if let Some(proof) = &table.asset_union { value["asset_union"] = json!(proof); }
            if let Some(proof) = &table.reset_union { value["reset_union"] = json!(proof); }
            if let Some(proof) = &table.template_pins { value["template_pins"] = json!(proof); }
            if let Some(proof) = &table.project_lifecycle { value["project_lifecycle"] = json!(proof); }
            if let Some(proof) = &table.sent_email_union { value["sent_email_union"] = json!(proof); }
            if let Some(proof) = &table.journal_handoff { value["journal_handoff"] = json!(proof); }
            value
        }).collect::<Vec<_>>(),
    })
}

fn write_application_proof(out: &mut String, table: &store::postgres_backfill::ParityTableReport) {
    use std::fmt::Write as _;
    if let Some(proof) = &table.sent_email_union {
        let _ = writeln!(out, "Sent Email handoff: captured {} overlaps {} purged {} native-only {} missing {} differing {}",
            table.surreal, proof.compared_overlaps, proof.excluded_purged, proof.native_target_only,
            proof.source_only_remaining, proof.differing_overlaps);
    }
    if let Some(proof) = &table.journal_handoff {
        let _ = writeln!(out, "Journal handoff: captured {} represented {} PostgreSQL {} native-only {} unresolved {} integrity failures {}",
            proof.source_rows, proof.represented_source_rows, proof.target_rows,
            proof.native_target_only, proof.unresolved_source_rows, proof.target_integrity_failures);
    }
}

fn write_family_proof(out: &mut String, table: &store::postgres_backfill::ParityTableReport) {
    use std::fmt::Write as _;
    write_application_proof(out, table);
    if let Some(proof) = &table.template_pins {
        let _ = writeln!(
            out,
            "Template pins: compared {} differences {}",
            proof.compared, proof.differences
        );
    }
    if let Some(proof) = &table.project_lifecycle {
        let _ = writeln!(
            out,
            "Project lifecycle: overlaps {} retained native {} unresolved {} target-only {}",
            proof.compared_overlaps,
            proof.retained_native_projections,
            proof.unresolved_overlaps,
            proof.target_only
        );
    }
    if let Some(proof) = &table.asset_union {
        let _ = writeln!(out,"Asset union: source {} PostgreSQL {} resolved native overlaps {} proven native target-only {} unknown target-only {}",table.surreal,table.postgres,proof.resolved_native_overlaps,proof.proven_native_target_only,proof.unknown_target_only);
    }
    if let Some(proof) = &table.reset_union {
        let _ = writeln!(
            out,
            "reset protection: compared {} retained native {}",
            proof.compared, proof.retained_native_protections
        );
    }
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
        write_family_proof(&mut out, table);
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
        (RunStatus::Paused, _) if report.scope != "global" => write!(
            out,
            "paused: resume with --family {} --resume {}",
            report.scope, report.run_id
        ),
        (RunStatus::Paused, _) => write!(out, "paused: resume with --resume {}", report.run_id),
        (RunStatus::Completed, 0) if report.is_clean() && report.scope != "global" => {
            write!(
                out,
                "completed: verified capture parity for scope {}",
                report.scope
            )
        }
        (RunStatus::Completed, 0) if report.is_clean() => {
            write!(out, "completed: all mirrored tables are in parity")
        }
        (RunStatus::Completed, 0) => write!(out, "completed with parity mismatches"),
        (RunStatus::Completed, failed) if report.scope != "global" => {
            write!(
                out,
                "completed with {failed} recorded failure(s) for capture scope {}",
                report.scope
            )
        }
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

    #[tokio::test]
    async fn native_target_refuses_dry_run_and_family_before_source_configuration_or_capture_files()
    {
        use sqlx::postgres::{PgConnectOptions, PgSslMode};
        use store::postgres::native_origin::{self, TierBinding};
        let port = store::test_support::postgres_test_port();
        let options = PgConnectOptions::new()
            .host("127.0.0.1")
            .port(port)
            .username("postgres")
            .password("synthetic")
            .ssl_mode(PgSslMode::Disable)
            .database(
                &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE")
                    .unwrap_or_else(|_| "smoke".into()),
            )
            .options([("session_preload_libraries", "age")]);
        let operation = Uuid::new_v4();
        let native = native_origin::create(
            options.clone(),
            operation,
            TierBinding {
                tier_id: Uuid::new_v4(),
                namespace_uid: Uuid::new_v4(),
                cluster_name: "synthetic-cli-origin".into(),
            },
        )
        .await
        .unwrap()
        .initialize()
        .await
        .unwrap();
        for flags in [
            vec!["backfill"],
            vec!["backfill", "--dry-run"],
            vec![
                "backfill",
                "--family",
                "application:1",
                "--capture",
                "/missing/private-capture",
                "--capture-receipt",
                "/missing/private-receipt",
            ],
        ] {
            let args = Cli::try_parse_from(flags).unwrap().args;
            let result = PostgresDb::with_test_db(native.postgres().clone(), execute(&args)).await;
            let Err(error) = result else {
                panic!("native target must refuse capture");
            };
            assert!(matches!(
                error.downcast_ref::<store::postgres_backfill::family::FamilyError>(),
                Some(store::postgres_backfill::family::FamilyError::NativeOwnership)
            ));
        }
        let family_args = Cli::try_parse_from([
            "backfill",
            "--family",
            "application:1",
            "--resume",
            &Uuid::new_v4().to_string(),
        ])
        .unwrap()
        .args;
        let error = execute_family(&family_args, native.postgres(), "application:1")
            .await
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<store::postgres_backfill::family::FamilyError>(),
            Some(store::postgres_backfill::family::FamilyError::NativeOwnership)
        ));
        let histories: i64 = sqlx::query_scalar("SELECT count(*) FROM mirror_backfill_run")
            .fetch_one(native.postgres().pool())
            .await
            .unwrap();
        assert_eq!(histories, 0);
        let name = native.database();
        native.postgres().pool().close().await;
        let admin = PostgresDb::connect(options).await.unwrap();
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP DATABASE \"{name}\"")))
            .execute(admin.pool())
            .await
            .unwrap();
        admin.pool().close().await;
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
    fn family_flags_require_explicit_scope_and_paired_capture() {
        assert!(Cli::try_parse_from(["backfill", "--activate"]).is_err());
        assert!(Cli::try_parse_from([
            "backfill",
            "--family",
            "satellites:1",
            "--capture",
            "synthetic.surql"
        ])
        .is_err());
        assert!(
            Cli::try_parse_from(["backfill", "--family", "satellites:1", "--refresh-capture"])
                .is_err()
        );
        let run = Uuid::new_v4().to_string();
        let args = Cli::try_parse_from([
            "backfill",
            "--family",
            "satellites:1",
            "--resume",
            &run,
            "--activate",
        ])
        .unwrap()
        .args;
        assert!(args.enrollment.activate);
        assert!(
            args.enrollment.capture.is_none(),
            "completed replay needs no capture file"
        );
        let refresh = Cli::try_parse_from([
            "backfill",
            "--family",
            "satellites:1",
            "--resume",
            &run,
            "--refresh-capture",
            "--capture",
            "synthetic.surql",
            "--capture-receipt",
            "synthetic.receipt.json",
        ])
        .unwrap()
        .args;
        assert!(refresh.enrollment.refresh_capture);
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
    fn dry_run_reports_full_source_coverage_without_row_values() {
        use store::postgres_backfill::{SourceDisposition, SourceTableCoverage};
        let plan = Plan {
            batch_size: 10,
            tables: vec![],
            unresolved_references: vec![],
            source_tables: vec![
                SourceTableCoverage {
                    table: "person".into(),
                    rows: 3,
                    disposition: SourceDisposition::Mirrored,
                },
                SourceTableCoverage {
                    table: "schema_version".into(),
                    rows: 1,
                    disposition: SourceDisposition::RecreatedMetadata,
                },
                SourceTableCoverage {
                    table: "empty_fixture".into(),
                    rows: 0,
                    disposition: SourceDisposition::Unsupported,
                },
                SourceTableCoverage {
                    table: "derived_fixture".into(),
                    rows: 2,
                    disposition: SourceDisposition::Derived,
                },
            ],
        };
        let rendered = plan_json(&plan, None);
        assert_eq!(
            rendered["source_tables"],
            json!([
                {"table":"person", "rows":3, "disposition":"mirrored"},
                {"table":"schema_version", "rows":1, "disposition":"recreated_metadata"},
                {"table":"empty_fixture", "rows":0, "disposition":"unsupported"},
                {"table":"derived_fixture", "rows":2, "disposition":"derived"},
            ])
        );
        let text = plan_text(&plan, None);
        assert!(text.contains("schema_version: 1 row(s), recreated_metadata"));
        assert!(text.contains("empty_fixture: 0 row(s), unsupported"));
    }

    #[test]
    fn object_verification_reports_each_storage_lane_without_exposing_keys() {
        use store::postgres_backfill::AssetObjectVerification;
        let plan = Plan {
            source_tables: vec![],
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
            source_tables: vec![],
            batch_size: 2,
            unresolved_references: vec![store::postgres_mirror::records::UnresolvedReference {
                table: "relationship",
                field: "out",
                source_orphans: 1,
                postgres_missing: 1,
                newly_target_missing: 0,
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
            scope: "global".into(),
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
                asset_union: None,
                reset_union: None,
                template_pins: None,
                project_lifecycle: None,
                sent_email_union: None,
                journal_handoff: None,
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
            scope: "global".into(),
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
                asset_union: None,
                reset_union: None,
                template_pins: None,
                project_lifecycle: None,
                sent_email_union: None,
                journal_handoff: None,
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
            source_tables: vec![],
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
            scope: "global".into(),
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
                asset_union: None,
                reset_union: None,
                template_pins: None,
                project_lifecycle: None,
                sent_email_union: None,
                journal_handoff: None,
            }],
            unresolved_references: vec![],
        };
        assert!(clean.is_clean());
        let text = run_text(&clean);
        assert!(text.contains("(in progress)"));
        assert!(!text.contains("parity identity person"));
        assert!(text.ends_with("completed: all mirrored tables are in parity"));
        assert_eq!(run_json(&clean)["clean"], true);

        let mut scoped = clean.clone();
        scoped.scope = "satellites:1".into();
        assert_eq!(run_json(&scoped)["scope"], "satellites:1");
        assert_eq!(run_json(&scoped)["parity_basis"], "verified_capture");
        assert!(run_text(&scoped)
            .ends_with("completed: verified capture parity for scope satellites:1"));
        scoped.status = RunStatus::Paused;
        assert!(run_text(&scoped).contains("--family satellites:1 --resume"));

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
                asset_union: None,
                reset_union: None,
                template_pins: None,
                project_lifecycle: None,
                sent_email_union: None,
                journal_handoff: None,
            }],
            unresolved_references: vec![store::postgres_mirror::records::UnresolvedReference {
                table: "relationship",
                field: "in",
                source_orphans: 2,
                postgres_missing: 0,
                newly_target_missing: 0,
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

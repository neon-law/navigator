//! `navigator ops postgres mirror` — measure and repair drift between
//! `SurrealDB` and its PostgreSQL mirrors.
//!
//! Every mirror in [`store::postgres_mirror::MIRRORS`] is covered in registry
//! dependency order. The registry owns the table groups and their ordering.
//! `parity` and `reconcile --dry-run` only read. `reconcile`
//! re-mirrors each divergent row from `SurrealDB`, parents first, and exits
//! non-zero when drift remains. A clean result applies only to measured tables;
//! compiled declaration coverage also lists unmapped and unmeasured source tables.
//! Reconcile JSON retains before counts and available after counts. A partial
//! failure or unavailable after-state requires a successful retry. Neither the comparison nor repair is a cross-store snapshot.
//!
//! The report names rows by UUID only: an Entity Role by its Person and
//! Entity, never its role, and a mailbox claim by its Person, never the
//! mailbox. A row keyed by a Brand key (a Social Handle or a `firm_brand`
//! row) is counted but never named, and no column
//! value leaves either store. It reads the same `NAVIGATOR_SURREAL_*` and
//! `NAVIGATOR_POSTGRES_*` variables `web` reads.

use std::collections::BTreeSet;
use std::process::ExitCode;

use clap::Subcommand;
use serde_json::json;
use store::postgres::PostgresDb;
use store::postgres_mirror::{Drift, MirrorKey, MirrorParity};

/// `navigator ops postgres`.
#[derive(Debug, Subcommand)]
pub enum PostgresCmd {
    /// Compare or reconcile the `SurrealDB` mirrors in PostgreSQL.
    #[command(subcommand)]
    Mirror(MirrorCmd),
    /// Copy the `SurrealDB` rows the mirror never saw, in resumable batches.
    Backfill(crate::postgres_backfill::BackfillArgs),
}

/// `navigator ops postgres mirror`.
#[derive(Debug, Subcommand)]
pub enum MirrorCmd {
    /// Report drift per mirrored table; writes nothing.
    Parity {
        /// Print one JSON object instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Re-mirror every divergent row from `SurrealDB`.
    Reconcile {
        /// Report what would be reconciled; write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Print one JSON object instead of a table.
        #[arg(long)]
        json: bool,
    },
}

/// Exit status when drift remains.
const DRIFT: u8 = 1;
/// Exit status when comparison is unavailable or a repair pass failed.
const UNAVAILABLE: u8 = 2;

/// What the run did, for the report header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Parity,
    DryRun,
    Reconcile,
}

impl Mode {
    const fn label(self) -> &'static str {
        match self {
            Self::Parity => "parity",
            Self::DryRun => "dry-run",
            Self::Reconcile => "reconcile",
        }
    }
}

/// One table's line in the report.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TableRow {
    mirror: &'static str,
    table: &'static str,
    surreal: usize,
    postgres: usize,
    drift: Drift,
    /// Divergent rows named by UUID.
    keys: Vec<String>,
    /// Divergent rows with no UUID key, counted only.
    unnamed: usize,
}

impl TableRow {
    fn is_clean(&self) -> bool {
        self.keys.is_empty() && self.unnamed == 0
    }
}

/// The whole run's report.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Report {
    mode: Mode,
    /// Divergent rows found before a reconcile wrote anything.
    reconciled: Option<usize>,
    tables: Vec<TableRow>,
    /// A write pass may partially commit before a failure; retry from source.
    reconciliation_failed: bool,
    comparison_available: bool,
    before: Option<Vec<TableRow>>,
}

impl Report {
    fn is_clean(&self) -> bool {
        self.comparison_available
            && !self.reconciliation_failed
            && self.tables.iter().all(TableRow::is_clean)
    }

    fn exit_code(&self) -> ExitCode {
        if self.reconciliation_failed || !self.comparison_available {
            ExitCode::from(UNAVAILABLE)
        } else if self.is_clean() {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(DRIFT)
        }
    }
}

/// Run one `ops postgres` command.
pub fn run(command: PostgresCmd) -> ExitCode {
    let command = match command {
        PostgresCmd::Mirror(command) => command,
        PostgresCmd::Backfill(args) => return crate::postgres_backfill::run(&args),
    };
    let (mode, json) = match command {
        MirrorCmd::Parity { json } => (Mode::Parity, json),
        MirrorCmd::Reconcile {
            dry_run: true,
            json,
        } => (Mode::DryRun, json),
        MirrorCmd::Reconcile {
            dry_run: false,
            json,
        } => (Mode::Reconcile, json),
    };
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
    if let Ok(report) = runtime.block_on(measure(mode)) {
        println!(
            "{}",
            if json {
                render_json(&report)
            } else {
                render_text(&report)
            }
        );
        report.exit_code()
    } else {
        if json {
            println!(
                "{}",
                json!({"mode": mode.label(), "clean": false, "error": "store unavailable; parity could not be measured"})
            );
        } else {
            eprintln!(
                "navigator: mirror {}: store unavailable; parity could not be measured",
                mode.label()
            );
        }
        ExitCode::from(UNAVAILABLE)
    }
}

async fn measure(mode: Mode) -> anyhow::Result<Report> {
    let surreal = store::surreal::connect_from_env()
        .await
        .map_err(|_| anyhow::anyhow!("SurrealDB is unavailable"))?;
    let pg = PostgresDb::from_env().await?;
    let before = store::postgres_mirror::parity_all(&surreal, &pg).await?;
    let before_rows = table_rows(&pg, &before).await?;
    let reconciled = (mode == Mode::Reconcile).then(|| divergent_rows(&before));
    let (mirrors, reconciliation_failed) = match mode {
        Mode::Parity | Mode::DryRun => (Some(before), false),
        Mode::Reconcile => match store::postgres_mirror::reconcile_all(&surreal, &pg).await {
            Ok(after) => (Some(after), false),
            Err(_) => (
                store::postgres_mirror::parity_all(&surreal, &pg).await.ok(),
                true,
            ),
        },
    };
    let measured = if mode == Mode::Reconcile {
        match mirrors {
            Some(mirrors) => table_rows(&pg, &mirrors).await.ok(),
            None => None,
        }
    } else {
        Some(before_rows.clone())
    };
    let comparison_available = measured.is_some();
    let tables = measured.unwrap_or_default();
    Ok(Report {
        mode,
        reconciled,
        tables,
        reconciliation_failed,
        comparison_available,
        before: (mode == Mode::Reconcile).then_some(before_rows),
    })
}

async fn table_rows(pg: &PostgresDb, mirrors: &[MirrorParity]) -> anyhow::Result<Vec<TableRow>> {
    let mut tables = Vec::new();
    for mirror in mirrors {
        for table in &mirror.report.tables {
            let drift = store::postgres_mirror::drift(pg, table).await?;
            tables.push(row(mirror.mirror, table, drift));
        }
    }
    Ok(tables)
}

fn divergent_rows(mirrors: &[MirrorParity]) -> usize {
    mirrors
        .iter()
        .map(|mirror| mirror.report.divergent().count())
        .sum()
}

fn row(
    mirror: &'static str,
    table: &store::postgres_mirror::TableParity,
    drift: Drift,
) -> TableRow {
    let keys: Vec<String> = table
        .divergent
        .iter()
        .filter_map(MirrorKey::report_id)
        .collect();
    TableRow {
        mirror,
        table: table.table,
        surreal: table.source_rows,
        postgres: table.target_rows,
        drift,
        unnamed: table.divergent.len() - keys.len(),
        keys,
    }
}

fn render_text(report: &Report) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:<15} {:<24} {:>8} {:>8} {:>8} {:>6} {:>9}",
        "mirror", "table", "surreal", "postgres", "missing", "extra", "differing"
    );
    let scope = coverage(report);
    let _ = writeln!(
        out,
        "coverage: {} source tables; {} SQL targets; {} unmapped; {} measured",
        scope["source_tables"],
        scope["sql_target_tables"].as_array().map_or(0, Vec::len),
        scope["unmapped_tables"].as_array().map_or(0, Vec::len),
        scope["measured_tables"]
    );
    if let Some(unmapped) = scope["unmapped_tables"].as_array() {
        let names: Vec<_> = unmapped
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect();
        let _ = writeln!(out, "unmapped: {}", names.join(", "));
    }
    for row in &report.tables {
        let _ = writeln!(
            out,
            "{:<15} {:<24} {:>8} {:>8} {:>8} {:>6} {:>9}",
            row.mirror,
            row.table,
            row.surreal,
            row.postgres,
            row.drift.missing,
            row.drift.extra,
            row.drift.differing
        );
        for key in &row.keys {
            let _ = writeln!(out, "    {key}");
        }
        if row.unnamed > 0 {
            let _ = writeln!(out, "    {} row(s) keyed by Brand key", row.unnamed);
        }
    }
    if let Some(reconciled) = report.reconciled {
        let _ = writeln!(
            out,
            "attempted reconciliation of {reconciled} divergent row(s)"
        );
    }
    if report.reconciliation_failed {
        let _ = writeln!(
            out,
            "reconciliation failed after possible partial writes; inspect parity and retry"
        );
    }
    let remaining: usize = report
        .tables
        .iter()
        .map(|row| row.keys.len() + row.unnamed)
        .sum();
    if !report.comparison_available {
        let _ = write!(out, "after comparison unavailable; retry required");
        return out;
    }
    let _ = match (report.mode, remaining, report.reconciliation_failed) {
        (_, _, true) => write!(
            out,
            "parity measured after failed reconciliation; retry required"
        ),
        (_, 0, false) => write!(out, "clean: measured tables match"),
        (Mode::Reconcile, rows, false) => write!(out, "drift remains: {rows} row(s)"),
        (Mode::Parity | Mode::DryRun, rows, false) => write!(out, "drift: {rows} row(s)"),
    };
    out
}

/// Declaration coverage is separate from the rows actually compared in this run.
fn coverage(report: &Report) -> serde_json::Value {
    let source: BTreeSet<String> = include_str!("../../store/src/schema/navigator.surql")
        .lines()
        .filter_map(|line| line.strip_prefix("DEFINE TABLE "))
        .map(|line| {
            line.strip_prefix("IF NOT EXISTS ")
                .or_else(|| line.strip_prefix("OVERWRITE "))
                .unwrap_or(line)
        })
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_owned)
        .collect();
    let mut excluded = BTreeSet::from(["schema_version".to_owned()]);
    excluded.extend(
        store::schema::derived_tables()
            .into_iter()
            .map(|table| table.name),
    );
    let eligible: BTreeSet<_> = source.difference(&excluded).cloned().collect();
    let target: BTreeSet<String> = include_str!("../../store/postgres/schema.sql")
        .lines()
        .filter_map(|line| line.strip_prefix("CREATE TABLE "))
        .map(|line| line.strip_prefix("IF NOT EXISTS ").unwrap_or(line))
        .filter_map(|line| line.split_whitespace().next())
        .map(|name| name.trim_matches('"').to_owned())
        .collect();
    let measured: BTreeSet<String> = report
        .tables
        .iter()
        .map(|row| row.table.to_owned())
        .collect();
    let mapped: Vec<_> = eligible.intersection(&target).cloned().collect();
    let sql_targets_unmeasured: Vec<_> = mapped
        .iter()
        .filter(|&table| !measured.contains(table.as_str()))
        .cloned()
        .collect();
    let unmapped: Vec<_> = eligible.difference(&target).cloned().collect();
    let unmeasured: Vec<_> = eligible.difference(&measured).cloned().collect();
    json!({
        "source_tables": source.len(),
        "eligible_source_tables": eligible.len(),
        "excluded_source_tables": excluded.intersection(&source).cloned().collect::<Vec<_>>(),
        "sql_target_tables": mapped,
        "sql_targets_unmeasured": sql_targets_unmeasured,
        "unmapped_tables": unmapped,
        "unmeasured_tables": unmeasured,
        "measured_tables": measured.len(),
        "complete": eligible.is_subset(&measured),
        "basis": "compiled schema declarations; runtime parity covers only reported tables",
    })
}

fn json_rows(rows: &[TableRow]) -> Vec<serde_json::Value> {
    rows.iter()
        .map(|row| {
            json!({
                "mirror": row.mirror,
                "table": row.table,
                "surreal": row.surreal,
                "postgres": row.postgres,
                "missing": row.drift.missing,
                "extra": row.drift.extra,
                "differing": row.drift.differing,
                "keys": row.keys,
                "unnamed": row.unnamed,
            })
        })
        .collect()
}

fn render_json(report: &Report) -> String {
    json!({
        "scope": "measured_tables_only",
        "coverage": coverage(report),
        "reconciliation_failed": report.reconciliation_failed,
        "comparison_available": report.comparison_available,
        "before": report.before.as_deref().map(json_rows),
        "mode": report.mode.label(),
        "clean": report.is_clean(),
        "reconciled": report.reconciled,
        "tables": json_rows(&report.tables),
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use store::postgres_mirror::{ParityReport, TableParity};
    use uuid::Uuid;

    use super::*;

    const CLIENT_BRAND: &str = "synthetic-client-brand";

    /// A comparison with one row of every key shape, as `parity_all` would
    /// return it for a store whose rows carry client-identifying text.
    fn drifted() -> (Vec<MirrorParity>, Uuid, Uuid, Uuid) {
        let (firm, person, post) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mirrors = vec![
            MirrorParity {
                mirror: "tenancy",
                report: ParityReport {
                    tables: vec![
                        TableParity {
                            table: "firm",
                            source_rows: 2,
                            target_rows: 1,
                            divergent: vec![MirrorKey::Firm(firm)],
                        },
                        TableParity {
                            table: "firm_brand",
                            source_rows: 1,
                            target_rows: 0,
                            divergent: vec![MirrorKey::FirmBrand {
                                brand_key: CLIENT_BRAND.into(),
                            }],
                        },
                        TableParity {
                            table: "person_firm_role",
                            source_rows: 1,
                            target_rows: 1,
                            divergent: vec![MirrorKey::FirmMembership {
                                person_id: person,
                                firm_id: firm,
                            }],
                        },
                    ],
                },
            },
            MirrorParity {
                mirror: "post",
                report: ParityReport {
                    tables: vec![
                        TableParity {
                            table: "post",
                            source_rows: 0,
                            target_rows: 0,
                            divergent: vec![],
                        },
                        TableParity {
                            table: "social_handle",
                            source_rows: 0,
                            target_rows: 1,
                            divergent: vec![MirrorKey::SocialHandle {
                                brand_key: CLIENT_BRAND.into(),
                                provider: "x".into(),
                            }],
                        },
                        TableParity {
                            table: "social_publication",
                            source_rows: 1,
                            target_rows: 0,
                            divergent: vec![MirrorKey::SocialPublication {
                                post_id: post,
                                provider: "x".into(),
                            }],
                        },
                    ],
                },
            },
        ];
        (mirrors, firm, person, post)
    }

    fn report(mode: Mode, mirrors: &[MirrorParity]) -> Report {
        Report {
            mode,
            reconciliation_failed: false,
            comparison_available: true,
            before: None,
            reconciled: (mode == Mode::Reconcile).then(|| divergent_rows(mirrors)),
            tables: mirrors
                .iter()
                .flat_map(|mirror| {
                    mirror
                        .report
                        .tables
                        .iter()
                        .map(|table| row(mirror.mirror, table, Drift::default()))
                })
                .collect(),
        }
    }

    #[test]
    fn unavailable_after_state_preserves_before_without_claiming_parity() {
        let (mirrors, ..) = drifted();
        let mut report = report(Mode::Reconcile, &mirrors);
        report.before = Some(report.tables.clone());
        report.tables.clear();
        report.comparison_available = false;
        let json: serde_json::Value = serde_json::from_str(&render_json(&report)).unwrap();
        assert_eq!(json["clean"], false);
        assert_eq!(json["comparison_available"], false);
        assert_eq!(json["before"][0]["surreal"], 2);
        assert_eq!(json["tables"], json!([]));
        assert_eq!(report.exit_code(), ExitCode::from(UNAVAILABLE));
        assert!(render_text(&report).ends_with("after comparison unavailable; retry required"));
    }

    #[test]
    fn partial_reconciliation_requires_retry_even_if_after_state_is_clean() {
        let (mirrors, ..) = drifted();
        let mut report = report(Mode::Reconcile, &mirrors);
        report.before = Some(report.tables.clone());
        report.reconciliation_failed = true;
        assert_eq!(report.exit_code(), ExitCode::from(UNAVAILABLE));
        let parsed: serde_json::Value = serde_json::from_str(&render_json(&report)).unwrap();
        assert_eq!(parsed["reconciliation_failed"], true);
        assert_eq!(parsed["before"][0]["surreal"], 2);
        for table in &mut report.tables {
            table.keys.clear();
            table.unnamed = 0;
        }
        assert!(!report.is_clean());
        assert!(!render_text(&report).contains("clean: measured"));
        // A successful second pass can confirm convergence independently.
        report.reconciliation_failed = false;
        assert!(report.is_clean());
        assert_eq!(report.exit_code(), ExitCode::SUCCESS);
    }

    #[test]
    fn clean_report_only_claims_measured_scope() {
        let report = report(Mode::Parity, &[]);
        let json: serde_json::Value = serde_json::from_str(&render_json(&report)).unwrap();
        assert_eq!(json["coverage"]["source_tables"], 106);
        assert_eq!(json["coverage"]["eligible_source_tables"], 104);
        assert_eq!(
            json["coverage"]["excluded_source_tables"],
            json!(["draft", "schema_version"])
        );
        assert_eq!(
            json["coverage"]["sql_target_tables"]
                .as_array()
                .unwrap()
                .len(),
            104
        );
        assert_eq!(
            json["coverage"]["unmapped_tables"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        assert!(!json["coverage"]["unmapped_tables"]
            .as_array()
            .unwrap()
            .iter()
            .any(|table| table == "social_account_selection"));
        assert!(!json["coverage"]["unmapped_tables"]
            .as_array()
            .unwrap()
            .iter()
            .any(|table| table == "blog_video"));
        assert_eq!(json["coverage"]["measured_tables"], 0);
        assert_eq!(json["coverage"]["complete"], false);
        assert!(render_text(&report).contains("measured tables match"));
        assert!(!render_text(&report).contains("both stores match"));
    }

    #[test]
    fn rows_are_named_by_uuid_and_brand_keys_are_only_counted() {
        let (mirrors, firm, person, post) = drifted();
        let initial_report = report(Mode::Parity, &mirrors);
        for rendered in [render_text(&initial_report), render_json(&initial_report)] {
            assert!(!rendered.contains(CLIENT_BRAND), "{rendered}");
            assert!(!rendered.contains("synthetic"), "{rendered}");
            assert!(rendered.contains(&firm.to_string()));
            assert!(rendered.contains(&format!("{firm}/{person}")));
            assert!(rendered.contains(&post.to_string()));
        }
        let firm_brand = &initial_report.tables[1];
        assert_eq!((firm_brand.keys.len(), firm_brand.unnamed), (0, 1));
        for key in [
            MirrorKey::Person(person),
            MirrorKey::Entity(person),
            MirrorKey::EntityType(person),
            MirrorKey::Jurisdiction(person),
            MirrorKey::Authority(person),
            MirrorKey::GlossaryTerm(person),
            MirrorKey::Question(person),
            MirrorKey::NotationPackage(person),
            MirrorKey::NotationPackageTemplate(person),
            MirrorKey::NotationRate(person),
            MirrorKey::Address(person),
            MirrorKey::Credential(person),
            MirrorKey::EmailToken(person),
            MirrorKey::PersonDelegate(person),
            MirrorKey::ExternalIdentity(person),
            MirrorKey::PersonMailbox(person),
            MirrorKey::Template(person),
        ] {
            assert_eq!(key.report_id(), Some(person.to_string()));
        }
        let tie = MirrorKey::EntityRole {
            person_id: person,
            entity_id: firm,
            role: "synthetic-role".into(),
        };
        assert_eq!(tie.report_id(), Some(format!("{person}/{firm}")));

        let selection = MirrorKey::SocialAccountSelection {
            brand_key: CLIENT_BRAND.into(),
            provider: "facebook".into(),
        };
        assert_eq!(selection.report_id(), None);
        let report = report(
            Mode::Parity,
            &[MirrorParity {
                mirror: "social_account_selections",
                report: ParityReport {
                    tables: vec![TableParity {
                        table: "social_account_selection",
                        source_rows: 1,
                        target_rows: 0,
                        divergent: vec![selection],
                    }],
                },
            }],
        );
        for rendered in [render_text(&report), render_json(&report)] {
            assert!(!rendered.contains(CLIENT_BRAND));
            assert!(
                rendered.contains("\"unnamed\":1")
                    || rendered.contains("row(s) keyed by Brand key")
            );
        }
    }

    #[test]
    fn orphan_attestation_drift_is_named_by_uuid() {
        let id = Uuid::new_v4();
        let mirrors = vec![MirrorParity {
            mirror: "attestation",
            report: ParityReport {
                tables: vec![TableParity {
                    table: "attestation",
                    source_rows: 1,
                    target_rows: 0,
                    divergent: vec![MirrorKey::Attestation(id)],
                }],
            },
        }];
        let report = report(Mode::Parity, &mirrors);

        assert_eq!(MirrorKey::Attestation(id).report_id(), Some(id.to_string()));
        assert!(render_text(&report).contains(&id.to_string()));
        assert!(render_json(&report).contains(&id.to_string()));
    }

    #[test]
    fn json_carries_every_count_per_table() {
        let (mirrors, ..) = drifted();
        let mut report = report(Mode::Reconcile, &mirrors);
        report.tables[0].drift = Drift {
            missing: 1,
            extra: 0,
            differing: 0,
        };
        let parsed: serde_json::Value = serde_json::from_str(&render_json(&report)).unwrap();
        assert_eq!(parsed["mode"], "reconcile");
        assert_eq!(parsed["clean"], false);
        assert_eq!(parsed["reconciled"], 5);
        let firm = &parsed["tables"][0];
        assert_eq!(firm["mirror"], "tenancy");
        assert_eq!(firm["table"], "firm");
        for (field, value) in [
            ("surreal", 2),
            ("postgres", 1),
            ("missing", 1),
            ("extra", 0),
            ("differing", 0),
        ] {
            assert_eq!(firm[field], value, "{field}");
        }
        assert_eq!(parsed["tables"].as_array().unwrap().len(), 6);
    }

    #[test]
    fn drift_exits_non_zero_and_clean_stores_exit_zero() {
        let (mirrors, ..) = drifted();
        let drifted = report(Mode::Reconcile, &mirrors);
        assert!(!drifted.is_clean());
        assert_eq!(drifted.exit_code(), ExitCode::from(DRIFT));
        assert!(render_text(&drifted).ends_with("drift remains: 5 row(s)"));

        let clean: Vec<MirrorParity> = mirrors
            .into_iter()
            .map(|mut mirror| {
                for table in &mut mirror.report.tables {
                    table.divergent.clear();
                }
                mirror
            })
            .collect();
        let clean = report(Mode::Parity, &clean);
        assert_eq!(clean.exit_code(), ExitCode::SUCCESS);
        assert!(render_text(&clean).ends_with("clean: measured tables match"));
        assert!(render_json(&clean).contains("\"reconciled\":null"));
    }

    #[test]
    fn read_only_modes_report_drift_without_claiming_a_repair() {
        let (mirrors, ..) = drifted();
        for mode in [Mode::Parity, Mode::DryRun] {
            let report = report(mode, &mirrors);
            assert_eq!(report.reconciled, None);
            assert_eq!(report.exit_code(), ExitCode::from(DRIFT));
            let text = render_text(&report);
            assert!(text.ends_with("drift: 5 row(s)"), "{text}");
            assert!(!text.contains("attempted reconciliation"));
            let parsed: serde_json::Value = serde_json::from_str(&render_json(&report)).unwrap();
            assert_eq!(parsed["mode"], mode.label());
            assert_eq!(parsed["before"], serde_json::Value::Null);
        }
        assert_eq!(Mode::DryRun.label(), "dry-run");
        let reconciled = report(Mode::Reconcile, &mirrors);
        assert!(render_text(&reconciled).contains("attempted reconciliation of 5 divergent row(s)"));
    }

    #[test]
    fn dry_run_and_parity_are_read_only_modes() {
        use clap::Parser;

        #[derive(Parser)]
        struct Probe {
            #[command(subcommand)]
            command: PostgresCmd,
        }
        let parsed =
            Probe::try_parse_from(["probe", "mirror", "reconcile", "--dry-run", "--json"]).unwrap();
        assert!(matches!(
            parsed.command,
            PostgresCmd::Mirror(MirrorCmd::Reconcile {
                dry_run: true,
                json: true
            })
        ));
        assert!(Probe::try_parse_from(["probe", "mirror", "parity", "--dry-run"]).is_err());
    }
}

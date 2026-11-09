//! Slack digest for the scheduled `archives` workflow.
//!
//! Builds the one Slack message a completed data export posts to the
//! firm-ops channel: an iceberg glyph and the run date, then one line per
//! snapshotted table — table name, row count, and a link that opens the
//! table's Iceberg files in the Google Cloud Storage browser. A table that
//! failed to write is the headline, not a footnote: the message opens with
//! the failure count and names each failing table with its error before any
//! table that landed, so a run that wrote nothing never reads as a normal
//! digest. That run also ends as a failure in the workflow engine
//! ([`crate::workflow`]); the digest is where a lawyer-ops reader sees why.
//!
//! Slack **mrkdwn**, not a fenced code block: the per-table links must render
//! as clickable `<url|label>`, which a code fence would print as literal text.
//! That is why `Archives` posts this message straight to the [`Notifier`] like
//! `Heartbeat` does, rather than routing a plain-text body through the
//! `SlackOpsDelivery` code-fence adapter the other ops digests use.
//!
//! [`Notifier`]: workflows::Notifier

use std::fmt::Write as _;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::drift::DriftDecision;
use crate::runner::TableFailure;
use crate::snapshot::APPLICATION_LANE;

/// One table's snapshot outcome. Serializable because it is part of the
/// journaled snapshot-phase output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotEntry {
    pub table: String,
    pub rows: usize,
    pub bytes: usize,
    pub key: String,
    pub drift: DriftDecision,
}

/// The data a completed workflow run hands to [`render_archives_slack`]:
/// the run date, the export bucket the per-table links point into, and the
/// snapshot outcomes (successes and failures). `archive_project` carries
/// the archive lane's GCP project (`NAVIGATOR_ARCHIVES_GCP_PROJECT_ENV`) —
/// set when the bucket lives offsite, so the console links open in the
/// archive project's context and the digest names where the data went.
#[derive(Debug, Clone)]
pub struct DiagnosticReport {
    pub run_date: NaiveDate,
    pub bucket: String,
    pub archive_project: Option<String>,
    pub snapshots: Vec<SnapshotEntry>,
    pub failures: Vec<TableFailure>,
}

/// The Google Cloud Storage browser. Every snapshotted table here is a
/// `SurrealDB` entity table, so its Iceberg files live under the
/// `application/<table>/` prefix of the export bucket (see
/// [`crate::snapshot`]) — linking to that prefix opens the table's `data/`
/// and `metadata/` objects in the console.
const GCS_BROWSER_BASE: &str = "https://console.cloud.google.com/storage/browser";

/// Render the data export as a single Slack **mrkdwn** message: an iceberg
/// glyph and the run date, then one line per snapshotted table — name, row
/// count, and a link into the Google Cloud Storage browser for that table's
/// Iceberg files. When any table failed to write, the header says `FAILED`
/// with the failure count and the failing tables come first, each with the
/// first line of its error. Pure and exposed so the formatting is unit-tested.
#[must_use]
pub fn render_archives_slack(report: &DiagnosticReport) -> String {
    let mut out = if report.failures.is_empty() {
        format!(
            "🧊 *Iceberg data export — {}*  ({} tables)\n",
            report.run_date,
            report.snapshots.len()
        )
    } else {
        format!(
            "⚠️ *Iceberg data export FAILED — {}*  ({} of {} tables failed to write)\n",
            report.run_date,
            report.failures.len(),
            report.failures.len() + report.snapshots.len()
        )
    };
    for failure in &report.failures {
        let _ = writeln!(
            out,
            "• ⚠️ *{}* — snapshot failed: {}",
            failure.table,
            error_summary(&failure.error)
        );
    }
    if let Some(project) = &report.archive_project {
        let _ = writeln!(out, "• *archive project* — {project} (offsite bucket)");
    }
    for entry in &report.snapshots {
        // The `project` query parameter pins the console to the archive
        // project: without it the browser opens the bucket in whichever
        // project the viewer last visited, which for an offsite bucket is
        // not the deployment's.
        let project_query = report
            .archive_project
            .as_deref()
            .map(|project| format!("?project={project}"))
            .unwrap_or_default();
        let url = format!(
            "{GCS_BROWSER_BASE}/{}/{APPLICATION_LANE}/{}{project_query}",
            report.bucket, entry.table
        );
        let _ = writeln!(
            out,
            "• *{}* — {} rows · <{url}|view in GCP>",
            entry.table, entry.rows
        );
    }
    out.trim_end().to_string()
}

/// The error's first line, bounded so a long GCS message cannot swallow the
/// digest. The full error stays in the journaled summary and the logs.
fn error_summary(error: &str) -> String {
    const LIMIT: usize = 160;
    let line = error.lines().next().unwrap_or_default().trim();
    match line.char_indices().nth(LIMIT) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{render_archives_slack, DiagnosticReport, SnapshotEntry};
    use crate::drift::DriftDecision;
    use crate::runner::TableFailure;
    use chrono::NaiveDate;

    fn sample(snapshots: Vec<SnapshotEntry>, failures: Vec<TableFailure>) -> DiagnosticReport {
        DiagnosticReport {
            run_date: NaiveDate::from_ymd_opt(2026, 7, 10).unwrap(),
            bucket: "navigator-exports".into(),
            archive_project: None,
            snapshots,
            failures,
        }
    }

    fn snap(table: &str, rows: usize) -> SnapshotEntry {
        SnapshotEntry {
            table: table.into(),
            rows,
            bytes: rows * 32,
            key: format!("application/{table}/data/dt=2026-07-10/part-abc.parquet"),
            drift: DriftDecision::Unchanged,
        }
    }

    #[test]
    fn message_opens_with_iceberg_glyph_and_run_date() {
        let msg = render_archives_slack(&sample(vec![snap("persons", 10)], vec![]));
        assert!(
            msg.starts_with("🧊"),
            "message must lead with the iceberg glyph: {msg}"
        );
        assert!(
            msg.contains("2026-07-10"),
            "message must carry the run date"
        );
    }

    #[test]
    fn header_counts_the_snapshotted_tables() {
        let msg = render_archives_slack(&sample(
            vec![snap("persons", 10), snap("documents", 20)],
            vec![],
        ));
        assert!(
            msg.contains("(2 tables)"),
            "header should count the tables: {msg}"
        );
    }

    #[test]
    fn each_table_line_carries_name_rows_and_a_gcs_browser_link() {
        let msg = render_archives_slack(&sample(
            vec![snap("persons", 312), snap("documents", 1_204)],
            vec![],
        ));
        // Table name in bold, the row count, and a mrkdwn link — one line each.
        assert!(msg.contains("• *persons* — 312 rows · "));
        assert!(msg.contains("• *documents* — 1204 rows · "));
        // The link points into the GCS browser at this table's application/ prefix.
        assert!(msg.contains(
            "<https://console.cloud.google.com/storage/browser/navigator-exports/application/persons|view in GCP>"
        ));
        assert!(msg.contains(
            "<https://console.cloud.google.com/storage/browser/navigator-exports/application/documents|view in GCP>"
        ));
    }

    #[test]
    fn message_is_mrkdwn_not_a_code_block() {
        // A fenced block would render the <url|label> links as literal text, so
        // the digest must never wrap itself in ``` — that is the whole reason it
        // bypasses the SlackOpsDelivery code-fence adapter.
        let msg = render_archives_slack(&sample(vec![snap("persons", 10)], vec![]));
        assert!(
            !msg.contains("```"),
            "digest must be mrkdwn, not a code block: {msg}"
        );
    }

    #[test]
    fn a_failed_table_leads_the_digest_with_its_error_and_no_link() {
        let msg = render_archives_slack(&sample(
            vec![snap("persons", 10)],
            vec![TableFailure {
                table: "documents".into(),
                error: "connection reset\nsecond line stays in the journal".into(),
            }],
        ));
        // A run with a failure is never rendered as a normal digest: the
        // header says so, with the count, before any table that landed.
        assert!(
            msg.starts_with(
                "⚠️ *Iceberg data export FAILED — 2026-07-10*  (1 of 2 tables failed to write)"
            ),
            "header must lead with the failure: {msg}"
        );
        let failure_at = msg.find("• ⚠️ *documents* — snapshot failed: connection reset");
        let success_at = msg.find("• *persons* — 10 rows");
        assert!(
            failure_at.is_some() && failure_at < success_at,
            "failing tables come first, each with its error: {msg}"
        );
        assert!(!msg.contains("second line"));
        // The failed table has no GCS link (it never landed in the bucket).
        assert!(!msg.contains("application/documents"));
    }

    #[test]
    fn a_run_that_wrote_nothing_but_failed_is_not_a_normal_digest() {
        let msg = render_archives_slack(&sample(
            vec![],
            vec![
                TableFailure {
                    table: "persons".into(),
                    error: "x".repeat(400),
                },
                TableFailure {
                    table: "documents".into(),
                    error: "denied".into(),
                },
            ],
        ));
        assert!(msg.contains("(2 of 2 tables failed to write)"), "{msg}");
        assert!(!msg.contains("(0 tables)"), "{msg}");
        assert!(msg.contains("*persons*") && msg.contains("*documents*"));
        // A long error is cut so the digest stays readable.
        assert!(msg.contains(&format!("{}…", "x".repeat(160))), "{msg}");
    }

    #[test]
    fn only_the_requested_fields_appear() {
        // The digest is deliberately just the table list — no bytes, cost,
        // BigQuery query, drift, telemetry, or Restate footer leaks in.
        let msg = render_archives_slack(&sample(vec![snap("persons", 10)], vec![]));
        for banned in [
            "Bytes",
            "COST",
            "SELECT",
            "DRIFT",
            "invocation",
            "TELEMETRY",
        ] {
            assert!(
                !msg.contains(banned),
                "digest should omit {banned:?}: {msg}"
            );
        }
    }

    #[test]
    fn empty_export_renders_header_only_without_panicking() {
        let msg = render_archives_slack(&sample(vec![], vec![]));
        assert!(msg.contains("(0 tables)"));
        assert!(!msg.contains('•'));
    }

    #[test]
    fn an_archive_project_is_annotated_and_pinned_in_the_console_links() {
        // The offsite lane names its GCP project: the digest carries the
        // annotation line, and every per-table link opens the console in
        // the archive project's context.
        let mut report = sample(vec![snap("persons", 10)], vec![]);
        report.archive_project = Some("neon-law-archive".into());
        let msg = render_archives_slack(&report);
        assert!(
            msg.contains("• *archive project* — neon-law-archive (offsite bucket)"),
            "annotation line: {msg}"
        );
        assert!(
            msg.contains(
                "<https://console.cloud.google.com/storage/browser/navigator-exports/application/persons?project=neon-law-archive|view in GCP>"
            ),
            "console link pinned to the archive project: {msg}"
        );
    }
}

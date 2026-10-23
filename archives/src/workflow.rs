//! The `Archives` Restate workflow.
//!
//! Hosted by the `workflows-service` worker (which binds it alongside
//! the `Notation` virtual object — all workflows live on that one
//! endpoint, so there is no separate always-on archives pod).
//!
//! One invocation == one scheduled export. Four durable steps:
//!
//! 1. `ctx.run("snapshot", …)` — open the database + object storage,
//!    snapshot every registered table to Parquet on GCS, return a
//!    journaled [`SnapshotSummary`]. A transient failure (database or
//!    GCS unreachable) replays just this step.
//! 2. `ctx.run("cost", …)` — when `BILLING_EXPORT_TABLE` is set, query
//!    every listed GCP billing export (one table per billing account)
//!    for trailing-window spend, merge the accounts by service, and
//!    snapshot the result to the export lake as `gcp_cost`. A clean
//!    no-op when the env var is unset (KIND / dev / OSS forks).
//! 3. `ctx.run("iceberg_telemetry", …)` — promote the day's telemetry
//!    Parquet (`telemetry/otel_*/data/dt=<date>/`) AND every store
//!    table's scheduled snapshot Parquet, written in phase 1
//!    (`application/<table>/data/dt=<date>/`), to Iceberg tables via
//!    the entity-table writer ([`crate::author_iceberg_for_prefix`]).
//!    Infallible per table: a lake-metadata hiccup never fails the
//!    export of the underlying Parquet, which already landed. A
//!    telemetry table is a no-op until the collector's OTLP→Parquet
//!    shim writes those files.
//! 4. `ctx.run("email", …)` — render the Slack digest (one line per
//!    snapshotted table, each linking to its Iceberg files in the Google
//!    Cloud console) and post it to firm ops through the worker's
//!    Slack bot. Each step is journaled, so a retry re-uses the cached
//!    prior results rather than re-snapshotting. The step name stays `email`
//!    (its journal key on replay) even though it now posts Slack, not email.
//!
//! The `CronJob` `archives-trigger` POSTs to the Restate ingress to start
//! one invocation at 05:55 and one at 17:55 UTC each day; Restate owns the retry schedule and the
//! invocation history a failed run is diagnosed from.

use std::fmt::Write as _;
use std::sync::Arc;

use chrono::{DateTime, NaiveDate, Utc};
use dbos::{CancellationToken, StepOptions, WorkflowRef, DBOS};
use restate_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use tracing::Instrument;
use workflows::SlackBot;

use billing::gcp_cost::CostReport;

use crate::digest::{render_archives_slack, DiagnosticReport};
use crate::postgres::{PostgresExport, POSTGRES_LANE};
use crate::runner::{cost_phase, open_resources, snapshot_selected, SnapshotSummary};
use crate::snapshot::{APPLICATION_LANE, TELEMETRY_LANE};
use crate::tables::ALL_TABLES;
use cloud::{StorageError, StorageService};

/// Request body for `Archives::run`. The UTC scheduled time isolates the
/// two daily snapshots. Empty requests remain valid for manual invocations
/// and already-journaled runs.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct RunRequest {
    #[serde(default)]
    pub scheduled_for: Option<DateTime<Utc>>,
}

/// Summary returned to the caller (and visible in Restate Cloud as
/// the invocation's output).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RunReport {
    pub run_date: NaiveDate,
    pub invocation_id: String,
    pub tables: usize,
    pub rows: usize,
    pub failures: usize,
}

/// Independent opt-in for the DBOS Archives schedule. Restate remains the
/// default scheduler until the deployment deliberately switches this flag.
pub const DBOS_ENABLED_ENV: &str = "NAVIGATOR_DBOS_ARCHIVES_ENABLED";
const DBOS_WORKFLOW: &str = "Archives";
const DBOS_NOTIFY_ATTEMPTS: u32 = 3;
const DBOS_LATE_START: chrono::Duration = chrono::Duration::minutes(15);
pub type ArchivesDbosWorkflow = WorkflowRef<RunRequest, RunReport, ArchivesDbosError>;

#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
pub enum ArchivesDbosError {
    #[error("archive snapshot failed")]
    Snapshot,
    #[error("archive cost phase failed")]
    Cost,
    #[error("archive notification failed")]
    Notify,
    #[error("archive result could not be reported")]
    Report,
}

#[derive(Debug, thiserror::Error)]
#[error("archive phase failed")]
pub struct ArchivesDbosPhaseError;

/// Phase boundary used by the registered DBOS workflow. Production uses the
/// archive/Postgres/object-storage implementations below; the boundary also
/// lets the real DBOS/Postgres recovery suite exercise this adapter without
/// touching external services.
#[async_trait::async_trait]
pub trait ArchivesDbosPhases: Send + Sync + 'static {
    async fn snapshot(
        &self,
        scheduled_for: Option<DateTime<Utc>>,
    ) -> Result<SnapshotSummary, ArchivesDbosPhaseError>;
    async fn cost(&self) -> Result<(), ArchivesDbosPhaseError>;
    async fn promote(&self, summary: &SnapshotSummary) -> Vec<String>;
    fn render_digest(&self, summary: &SnapshotSummary) -> Result<String, ArchivesDbosPhaseError>;
}

struct ProductionDbosPhases;

#[async_trait::async_trait]
impl ArchivesDbosPhases for ProductionDbosPhases {
    async fn snapshot(
        &self,
        scheduled_for: Option<DateTime<Utc>>,
    ) -> Result<SnapshotSummary, ArchivesDbosPhaseError> {
        let (db, storage) = open_resources().await.map_err(|_| ArchivesDbosPhaseError)?;
        let mut summary = snapshot_selected(db.as_ref(), storage.as_ref(), scheduled_for).await;
        summary.postgres = crate::postgres::export_configured(
            |key| std::env::var(key).ok(),
            storage.as_ref(),
            scheduled_for,
        )
        .await;
        Ok(summary)
    }

    async fn cost(&self) -> Result<(), ArchivesDbosPhaseError> {
        cost_phase(|key| std::env::var(key).ok())
            .await
            .map(|_| ())
            .map_err(|_| ArchivesDbosPhaseError)
    }

    async fn promote(&self, summary: &SnapshotSummary) -> Vec<String> {
        promote_iceberg_metadata(summary, |key| std::env::var(key).ok()).await
    }

    fn render_digest(&self, summary: &SnapshotSummary) -> Result<String, ArchivesDbosPhaseError> {
        digest_message(summary, |key| std::env::var(key).ok()).map_err(|_| ArchivesDbosPhaseError)
    }
}

/// Resolve the Archives schedule's explicit opt-in. Unknown values fail
/// closed so a misspelled cutover flag cannot silently change scheduling.
pub fn dbos_enabled(lookup: impl Fn(&str) -> Option<String>) -> Result<bool, &'static str> {
    match lookup(DBOS_ENABLED_ENV).as_deref().map(str::trim) {
        None | Some("" | "false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err("NAVIGATOR_DBOS_ARCHIVES_ENABLED must be true or false"),
    }
}

#[must_use]
pub fn dbos_workflow_id(slot: DateTime<Utc>) -> String {
    // Restate uses only the invocation key within a named service; DBOS IDs
    // share one global namespace. Preserve the slot in the suffix while
    // keeping a workflow discriminator to prevent cross-job collisions.
    format!("Archives:{}", slot.format("%Y-%m-%dT%H:%M:%SZ"))
}

#[must_use]
pub fn next_dbos_slot(now: DateTime<Utc>) -> DateTime<Utc> {
    [
        now.date_naive().and_hms_opt(5, 55, 0).unwrap().and_utc(),
        now.date_naive().and_hms_opt(17, 55, 0).unwrap().and_utc(),
    ]
    .into_iter()
    .find(|slot| *slot >= now - DBOS_LATE_START)
    .unwrap_or_else(|| {
        (now.date_naive() + chrono::Days::new(1))
            .and_hms_opt(5, 55, 0)
            .unwrap()
            .and_utc()
    })
}

/// Register Archives in the shared DBOS registry. Every side-effecting phase
/// has its own checkpoint; the workflow ID is the UTC 05:55/17:55 slot, so
/// restarts and replicas join the same execution.
pub fn register_dbos(
    dbos: &DBOS,
    slack: Arc<dyn SlackBot>,
    channel_id: String,
    slack_enabled: bool,
) -> dbos::Result<ArchivesDbosWorkflow> {
    register_dbos_with(
        dbos,
        slack,
        channel_id,
        slack_enabled,
        Arc::new(ProductionDbosPhases),
    )
}

/// Register Archives using an explicit phase provider. The deployed adapter
/// calls [`register_dbos`] and therefore always uses production resources.
pub fn register_dbos_with(
    dbos: &DBOS,
    slack: Arc<dyn SlackBot>,
    channel_id: String,
    slack_enabled: bool,
    phases: Arc<dyn ArchivesDbosPhases>,
) -> dbos::Result<ArchivesDbosWorkflow> {
    dbos.register_workflow(DBOS_WORKFLOW, move |request: RunRequest| {
        let slack = Arc::clone(&slack);
        let channel_id = channel_id.clone();
        let phases = Arc::clone(&phases);
        async move {
            let scheduled_for = request.scheduled_for;
            let phases_for_snapshot = Arc::clone(&phases);
            let summary = dbos::step("snapshot", move || {
                let phases = Arc::clone(&phases_for_snapshot);
                async move {
                    phases
                        .snapshot(scheduled_for)
                        .await
                        .map_err(|_| dbos::Error::Application(ArchivesDbosError::Snapshot))
                }
            })
            .await?;

            let phases_for_cost = Arc::clone(&phases);
            dbos::step("cost", move || {
                let phases = Arc::clone(&phases_for_cost);
                async move {
                    phases
                        .cost()
                        .await
                        .map_err(|_| dbos::Error::Application(ArchivesDbosError::Cost))
                }
            })
            .await?;

            let summary_for_iceberg = summary.clone();
            let phases_for_promotion = Arc::clone(&phases);
            let promotion = dbos::step("iceberg_telemetry", move || {
                let summary = summary_for_iceberg.clone();
                let phases = Arc::clone(&phases_for_promotion);
                async move { Ok(phases.promote(&summary).await) }
            })
            .await?;

            let summary_for_digest = summary.clone();
            let phases_for_digest = Arc::clone(&phases);
            let slack_for_post = Arc::clone(&slack);
            let channel_for_post = channel_id.clone();
            let correlation_id = format!(
                "archives-{}-digest",
                dbos::workflow_id().unwrap_or_default()
            );
            dbos::step_with(
                "email",
                StepOptions {
                    max_attempts: DBOS_NOTIFY_ATTEMPTS,
                    ..StepOptions::default()
                },
                move || {
                    let summary = summary_for_digest.clone();
                    let phases = Arc::clone(&phases_for_digest);
                    let promotion = promotion.clone();
                    let slack = Arc::clone(&slack_for_post);
                    let channel = channel_for_post.clone();
                    let correlation = correlation_id.clone();
                    async move {
                        let mut message = phases
                            .render_digest(&summary)
                            .map_err(|_| dbos::Error::Application(ArchivesDbosError::Notify))?;
                        for line in promotion
                            .iter()
                            .filter(|line| line.contains("FAILED") || line.contains("skipped"))
                        {
                            let _ = write!(message, "\n⚠️ {line}");
                        }
                        post_digest_idempotent(
                            slack.as_ref(),
                            &channel,
                            slack_enabled,
                            &message,
                            &correlation,
                        )
                        .await
                        .map_err(|_| dbos::Error::Application(ArchivesDbosError::Notify))
                    }
                },
            )
            .await?;

            let invocation_id = dbos::workflow_id().unwrap_or_default();
            run_report(&summary, &invocation_id)
                .map_err(|_| dbos::Error::Application(ArchivesDbosError::Report))
        }
    })
}

/// Start the next eligible daily slot. Starting after the 15-minute handoff
/// window skips today's slot; it never races a Restate run during cutover.
pub async fn schedule_dbos(workflow: ArchivesDbosWorkflow, stop: CancellationToken) {
    let mut last_started = None;
    loop {
        let now = Utc::now();
        let today = [
            now.date_naive().and_hms_opt(5, 55, 0).unwrap().and_utc(),
            now.date_naive().and_hms_opt(17, 55, 0).unwrap().and_utc(),
        ];
        let target = next_dbos_slot(now);
        let target = if Some(target) == last_started {
            today
                .into_iter()
                .find(|slot| *slot > target)
                .unwrap_or_else(|| {
                    (now.date_naive() + chrono::Days::new(1))
                        .and_hms_opt(5, 55, 0)
                        .unwrap()
                        .and_utc()
                })
        } else {
            target
        };
        if target <= now {
            let id = dbos_workflow_id(target);
            match workflow
                .start_with(
                    RunRequest {
                        scheduled_for: Some(target),
                    },
                    dbos::StartOptions {
                        workflow_id: Some(&id),
                        ..Default::default()
                    },
                )
                .await
            {
                Ok(_handle) => {
                    last_started = Some(target);
                }
                Err(error) => {
                    tracing::error!(workflow_id = %id, %error, "Archives DBOS start failed");
                    tokio::select! {
                        () = stop.cancelled() => return,
                        () = tokio::time::sleep(std::time::Duration::from_secs(60)) => continue,
                    }
                }
            }
        }
        let next = if last_started == Some(target) {
            today
                .into_iter()
                .find(|slot| *slot > target)
                .unwrap_or_else(|| {
                    (now.date_naive() + chrono::Days::new(1))
                        .and_hms_opt(5, 55, 0)
                        .unwrap()
                        .and_utc()
                })
        } else {
            target
        };
        let wait = (next - Utc::now())
            .to_std()
            .unwrap_or(std::time::Duration::ZERO);
        tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep(wait) => {}
        }
    }
}

/// Service struct registered with the Restate endpoint. Holds only the
/// worker-side Slack bot and messages channel (for the digest); the database and
/// object-storage handles are opened inside the snapshot step so no
/// connection is held idle between scheduled runs. Posts to the Slack bot
/// directly rather than through the `SlackOpsDelivery` code-fence adapter,
/// because the digest is mrkdwn with clickable links a code fence would
/// break — the same reason `Heartbeat` talks to the notifier directly.
#[derive(Clone)]
pub struct ArchivesService {
    slack: Arc<dyn SlackBot>,
    channel_id: String,
    slack_enabled: bool,
}

impl ArchivesService {
    #[must_use]
    pub fn new(slack: Arc<dyn SlackBot>, channel_id: String, slack_enabled: bool) -> Self {
        Self {
            slack,
            channel_id,
            slack_enabled,
        }
    }
}

#[restate_sdk::workflow(name = "Archives")]
impl ArchivesService {
    #[restate_sdk::handler]
    async fn run(
        &self,
        ctx: WorkflowContext<'_>,
        req: Json<RunRequest>,
    ) -> Result<Json<RunReport>, HandlerError> {
        let scheduled_for = req.into_inner().scheduled_for;
        let invocation_id = ctx.invocation_id().to_string();

        // Join the caller's trace: extract the W3C `traceparent` the trigger
        // injected (telemetry) from the invocation headers and parent this
        // run's span on it, so a `web`-initiated "run nightly export now" and
        // its durable steps appear as one trace. A no-op when none is present.
        let span = tracing::info_span!("archives.run", invocation_id = %invocation_id);
        {
            let headers = ctx.headers();
            telemetry::set_span_parent(
                &span,
                headers.get("traceparent").map(String::as_str),
                headers.get("tracestate").map(String::as_str),
            );
        }

        // `.instrument(span)` rather than `span.enter()`: the worker runs on a
        // multi-thread runtime, where a guard held across `.await` is the
        // documented footgun (it can leak to another task). Instrumenting the
        // future keeps the span attached only while this future is polled.
        async {
            // Phase 1 — snapshot. The whole loop is one journaled step:
            // open the handles fresh, snapshot every table, return the
            // serializable summary. `?` on `open_resources` yields a
            // retryable HandlerError so a database/GCS outage replays the
            // step.
            let summary: SnapshotSummary = ctx
                .run(|| async {
                    let (db, storage) = open_resources().await?;
                    let mut summary =
                        snapshot_selected(db.as_ref(), storage.as_ref(), scheduled_for).await;
                    // Retain completed source outcomes for the digest.
                    // PostgreSQL failures terminate the run after notification.
                    summary.postgres = crate::postgres::export_configured(
                        |k| std::env::var(k).ok(),
                        storage.as_ref(),
                        scheduled_for,
                    )
                    .await;
                    Ok(Json(summary))
                })
                .name("snapshot")
                .await?
                .into_inner();

            // Phase 2: GCP cost summary (no-op unless BILLING_EXPORT_TABLE
            // is set). Journaled like the snapshot. It runs for its side
            // effect, the trailing-window spend snapshotted to the export lake
            // as `gcp_cost`; the returned report does not feed the digest,
            // which is just the table list.
            let _cost: Option<CostReport> = ctx
                .run(|| async { Ok(Json(cost_phase(|k| std::env::var(k).ok()).await?)) })
                .name("cost")
                .await?
                .into_inner();

            // Phase 3: promote the day's telemetry Parquet (otel_*) AND
            // every store table's scheduled snapshot Parquet (phase 1) to
            // Iceberg tables, reusing the entity-table writer. Journaled and
            // infallible per table: Parquet remains available after a metadata
            // failure, which the digest reports as a warning.
            //
            // The step is named `iceberg_telemetry` although it covers the
            // application lane too: the name is the journal key Restate
            // matches on replay, and renaming it raises JOURNAL_MISMATCH for
            // any invocation replayed across a deploy.
            let iceberg_promotion: Vec<String> = ctx
                .run(|| async {
                    Ok::<_, HandlerError>(Json(
                        promote_iceberg_metadata(&summary, |k| std::env::var(k).ok()).await,
                    ))
                })
                .name("iceberg_telemetry")
                .await?
                .into_inner();

            // Phase 4 — Slack digest, rendered from the journaled snapshot so a
            // retry re-posts without re-running the prior steps. Posts straight
            // to the Slack bot (mrkdwn with links a code fence would break); a
            // Slack failure surfaces as a `HandlerError` so Restate retries the
            // step rather than dropping the only copy of the signal.
            //
            // The durable step keeps its original name `email`: the name is the
            // journal key Restate matches on replay (`RunCommand` header
            // equality covers it), so renaming it would raise JOURNAL_MISMATCH
            // for any invocation replayed across the deploy that flipped this
            // step from email to Slack. The step now posts Slack, not email.
            let summary_for_digest = summary.clone();
            let slack = Arc::clone(&self.slack);
            let channel_id = self.channel_id.clone();
            let slack_enabled = self.slack_enabled;
            ctx.run(move || {
                let summary = summary_for_digest.clone();
                let slack = Arc::clone(&slack);
                let channel_id = channel_id.clone();
                let iceberg_promotion = iceberg_promotion.clone();
                async move {
                    // Composed inside the step, not above it: an unresolvable
                    // bucket is then a step failure Restate retries, rather
                    // than an error escaping the handler before the journal
                    // records anything.
                    let mut message = digest_message(&summary, |k| std::env::var(k).ok())
                        .map_err(|error| HandlerError::from(anyhow::Error::from(error)))?;
                    for line in iceberg_promotion
                        .iter()
                        .filter(|line| line.contains("FAILED") || line.contains("skipped"))
                    {
                        let _ = write!(message, "\n⚠️ {line}");
                    }
                    post_digest(slack.as_ref(), &channel_id, slack_enabled, &message).await
                }
            })
            .name("email")
            .await?;

            Ok(Json(run_report(&summary, &invocation_id)?))
        }
        .instrument(span)
        .await
    }
}

/// Validate the journaled source outcomes after delivering their digest.
/// A failed coverage attempt is terminal: replaying its recorded snapshot
/// cannot re-extract data, so recovery requires a new invocation.
fn run_report(summary: &SnapshotSummary, invocation_id: &str) -> Result<RunReport, TerminalError> {
    if matches!(summary.postgres, Some(PostgresExport::Failed { .. })) {
        return Err(TerminalError::new(
            "PostgreSQL coverage failed; no paired generation was published",
        ));
    }
    let postgres_tables: Vec<_> = match &summary.postgres {
        Some(PostgresExport::Published(generation)) => generation.tables.iter().collect(),
        Some(PostgresExport::Paired(generation)) => generation
            .databases
            .iter()
            .flat_map(|database| &database.tables)
            .collect(),
        _ => Vec::new(),
    };
    Ok(RunReport {
        run_date: summary.run_date,
        invocation_id: invocation_id.into(),
        tables: summary.entries.len() + postgres_tables.len(),
        rows: summary
            .entries
            .iter()
            .map(|entry| entry.rows)
            .sum::<usize>()
            + postgres_tables
                .iter()
                .map(|table| table.rows)
                .sum::<usize>(),
        failures: summary.failures.len(),
    })
}

async fn post_digest(
    slack: &dyn SlackBot,
    channel_id: &str,
    enabled: bool,
    message: &str,
) -> Result<(), HandlerError> {
    if !enabled || channel_id.trim().is_empty() {
        return Err(HandlerError::from(anyhow::anyhow!(
            "Archives requires SLACK_BOT_TOKEN and its messages channel"
        )));
    }
    slack
        .post_message(channel_id, message)
        .await
        .map_err(HandlerError::from)
}

/// DBOS can repeat a step after Slack accepts its request but before the step
/// checkpoint commits. Reuse one Slack `client_msg_id` per slot so recovery
/// joins the same external effect.
async fn post_digest_idempotent(
    slack: &dyn SlackBot,
    channel_id: &str,
    enabled: bool,
    message: &str,
    correlation_id: &str,
) -> Result<(), HandlerError> {
    if !enabled || channel_id.trim().is_empty() {
        return Err(HandlerError::from(anyhow::anyhow!(
            "Archives requires SLACK_BOT_TOKEN and its messages channel"
        )));
    }
    if slack
        .message_receipt_by_correlation(channel_id, correlation_id)
        .await?
        .is_some()
    {
        return Ok(());
    }
    slack
        .post_message_with_receipt(channel_id, message, Some(correlation_id))
        .await
        .map(|_| ())
        .map_err(HandlerError::from)
}

/// Compose the Slack digest message a completed run posts: assemble the
/// [`DiagnosticReport`] from the journaled summary and the archive bucket,
/// then render it to mrkdwn. Split out of the handler — where it sits inside
/// a Restate `ctx.run` step — so the whole summary → message pipeline is
/// unit-tested without a workflow context. Shares `build_report`'s `get` seam,
/// so the bucket in each per-table link is exercised without touching env.
fn digest_message<F: Fn(&str) -> Option<String>>(
    summary: &SnapshotSummary,
    get: F,
) -> Result<String, StorageError> {
    let mut message = render_archives_slack(&build_report(summary, get)?);
    match &summary.postgres {
        Some(PostgresExport::Published(generation)) => {
            let _ = write!(
                message,
                "\nPostgreSQL generation published: {} tables, {} rows, cutoff {} UTC",
                generation.tables.len(),
                generation.tables.iter().map(|t| t.rows).sum::<usize>(),
                generation.cutoff_at.format("%Y-%m-%d %H:%M:%S")
            );
        }
        Some(PostgresExport::Paired(generation)) => {
            for database in &generation.databases {
                let _ = write!(
                    message,
                    "\nPostgreSQL {}: {} tables, cutoff {} UTC, snapshot {}",
                    database.database,
                    database.tables.len(),
                    database.cutoff_at.format("%Y-%m-%d %H:%M:%S"),
                    database.snapshot
                );
            }
            message.push_str("\nPaired coverage complete; cross-database atomic recovery is unverified. Git/LFS/document bytes require separate backups.");
        }
        Some(PostgresExport::Failed { error }) => {
            let _ = write!(message, "\n⚠️ PostgreSQL generation not published: {error}");
        }
        None => {}
    }
    if let Some(scheduled) = summary.scheduled_for {
        let _ = write!(
            message,
            "\nScheduled for {} UTC",
            scheduled.format("%Y-%m-%d %H:%M")
        );
    }
    Ok(message)
}

/// Assemble the [`DiagnosticReport`] from the journaled summary and the
/// archive bucket. Takes a `key -> value` lookup seam so the mapping is
/// unit-testable without mutating process env. The bucket is the only env
/// the digest needs — it is the root of every per-table GCS console link.
///
/// Fails rather than rendering a placeholder. The snapshot phase cannot have
/// produced a summary without resolving this same bucket, so by the time the
/// digest runs the value exists; an absent one is a bug, and a link built
/// from a sentinel is a broken link posted to a channel rather than an error
/// anyone can act on.
fn build_report<F: Fn(&str) -> Option<String>>(
    summary: &SnapshotSummary,
    get: F,
) -> Result<DiagnosticReport, StorageError> {
    let bucket = get("NAVIGATOR_ARCHIVES_BUCKET")
        .filter(|s| !s.is_empty())
        .ok_or(StorageError::MissingEnv("NAVIGATOR_ARCHIVES_BUCKET"))?;
    Ok(DiagnosticReport {
        run_date: summary.run_date,
        bucket,
        snapshots: summary.entries.clone(),
        failures: summary.failures.clone(),
    })
}

/// The `otel_*` tables promoted from the telemetry lake's daily Parquet.
const TELEMETRY_TABLES: &[&str] = &["otel_logs", "otel_traces", "otel_metrics"];

/// Promote the day's telemetry Parquet (`telemetry/otel_*/data/dt=<date>/`)
/// AND every store table's scheduled snapshot Parquet
/// (`application/<table>/data/dt=<date>/run=<HHMM>/`, written by phase 1) to Iceberg
/// tables, reusing the entity-table writer
/// ([`crate::author_iceberg_for_prefix`]).
///
/// **Infallible per table by design** — a lake-metadata hiccup must never
/// fail the nightly export, whose Parquet already landed in phase 1 — so it
/// returns one human-readable line per table for the diagnostic email rather
/// than a `Result`. A clean no-op ("no data") until a data file exists under
/// that table's prefix for the run date.
async fn promote_iceberg_metadata<F: Fn(&str) -> Option<String>>(
    summary: &SnapshotSummary,
    get: F,
) -> Vec<String> {
    let storage = match cloud::archives_from_env().await {
        Ok(s) => s,
        Err(e) => {
            return vec![format!(
                "(iceberg metadata promotion skipped — storage unavailable: {e})"
            )]
        }
    };
    let Some(bucket) = get("NAVIGATOR_ARCHIVES_BUCKET").filter(|s| !s.is_empty()) else {
        return vec![
            "(iceberg metadata promotion skipped — NAVIGATOR_ARCHIVES_BUCKET is required)"
                .to_string(),
        ];
    };
    // Derive the snapshot identity from the journaled slot, not wall-clock
    // time: DBOS may replay this step after a crash before its checkpoint.
    let now_ms = summary.scheduled_for.map_or_else(
        || {
            summary
                .run_date
                .and_hms_opt(0, 0, 0)
                .expect("midnight is valid")
                .and_utc()
                .timestamp_millis()
        },
        |slot| slot.timestamp_millis(),
    );
    promote_iceberg_metadata_over(storage.as_ref(), &bucket, summary, now_ms).await
}

/// One promotion: lane, table, snapshot-expiry cutoff, and data selection.
/// The selection is `Some(None)` for a whole telemetry day, `Some(Some(key))`
/// for one run file, and `None` when the run produced no file to promote.
type PlannedTable<'a> = (&'a str, &'a str, Option<i64>, Option<Option<&'a str>>);

/// The testable core of [`promote_iceberg_metadata`]: everything after the
/// storage handle is open and the clock has been read, so a test can inject
/// an `FsStorage` temp dir and a pinned `now_ms` without touching process env
/// or the clock.
async fn promote_iceberg_metadata_over(
    storage: &dyn StorageService,
    bucket: &str,
    summary: &SnapshotSummary,
    now_ms: i64,
) -> Vec<String> {
    let location_base = format!("gs://{bucket}");
    let run_date = summary.run_date;
    // 30-day cutoff for the short-retention telemetry tables (traces/metrics);
    // their GCS lifecycle deletes data at 30d, so the snapshot log is pruned
    // to match. Every store table keeps its FULL snapshot log below — these
    // are binding records, never pruned to match a telemetry lifecycle rule.
    let cutoff_30d_ms = now_ms - 30 * 24 * 60 * 60 * 1000;

    // (lane, table, expiry, data key). Telemetry promotes the whole day; an
    // application table selects its journaled run file, or is reported as
    // absent; a PostgreSQL table is promoted only from a published generation.
    let mut plan: Vec<PlannedTable<'_>> = TELEMETRY_TABLES
        .iter()
        .map(|&table| {
            // otel_logs keeps its full snapshot log (10-year, content-free);
            // otel_traces / otel_metrics prune to the 30-day lifecycle window.
            let expire_before_ms = (table != "otel_logs").then_some(cutoff_30d_ms);
            (TELEMETRY_LANE, table, expire_before_ms, Some(None))
        })
        .collect();
    plan.extend(ALL_TABLES.iter().map(|table| {
        let data_key = summary
            .entries
            .iter()
            .find(|entry| &entry.table == table)
            .map(|entry| Some(entry.key.as_str()));
        (APPLICATION_LANE, table.as_str(), None, data_key)
    }));
    if let Some(PostgresExport::Published(generation)) = &summary.postgres {
        plan.extend(generation.tables.iter().map(|entry| {
            (
                POSTGRES_LANE,
                entry.table.as_str(),
                None,
                Some(Some(entry.key.as_str())),
            )
        }));
    }

    let mut lines = Vec::with_capacity(plan.len());
    for (i, (lane, table, expire_before_ms, data_key)) in plan.into_iter().enumerate() {
        let snapshot_id = now_ms.saturating_add(i64::try_from(i).unwrap_or(0));
        let Some(data_key) = data_key else {
            lines.push(format!("{lane}/{table} (no snapshot data)"));
            continue;
        };
        match crate::author_iceberg_for_prefix(
            storage,
            crate::PromotionRequest {
                lane,
                table,
                location_base: &location_base,
                run_date,
                data_key,
                snapshot_id,
                timestamp_ms: now_ms,
                expire_before_ms,
            },
        )
        .await
        {
            Ok(Some(authored)) => lines.push(format!("{lane}/{table} v{}", authored.version)),
            Ok(None) => lines.push(format!("{lane}/{table} (no data)")),
            Err(e) => lines.push(format!("{lane}/{table} FAILED: {e}")),
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::{
        build_report, dbos_enabled, dbos_workflow_id, digest_message, next_dbos_slot, post_digest,
        post_digest_idempotent, promote_iceberg_metadata_over, RunRequest, SnapshotSummary,
        StorageError, DBOS_ENABLED_ENV,
    };
    use crate::digest::SnapshotEntry;
    use crate::drift::DriftDecision;
    use crate::runner::TableFailure;
    use chrono::NaiveDate;
    use std::collections::HashMap;

    #[test]
    fn dbos_archives_slots_keep_the_restate_identity_and_late_start_fence() {
        let morning: chrono::DateTime<chrono::Utc> = "2026-10-11T05:55:00Z".parse().unwrap();
        let evening: chrono::DateTime<chrono::Utc> = "2026-10-11T17:55:00Z".parse().unwrap();
        assert_eq!(dbos_workflow_id(morning), "Archives:2026-10-11T05:55:00Z");
        assert_eq!(dbos_workflow_id(evening), "Archives:2026-10-11T17:55:00Z");
        assert_ne!(dbos_workflow_id(morning), dbos_workflow_id(evening));
        assert_eq!(dbos_workflow_id(morning), dbos_workflow_id(morning));
        assert_ne!(
            dbos_workflow_id(morning),
            format!("BillingDigest:{}", morning.format("%Y-%m-%dT%H:%M:%SZ")),
            "DBOS workflow IDs share a global namespace across job types"
        );
        assert_eq!(
            next_dbos_slot("2026-10-11T06:09:59Z".parse().unwrap()),
            morning
        );
        assert_eq!(
            next_dbos_slot("2026-10-11T06:10:01Z".parse().unwrap()),
            evening,
            "after the handoff window, wait for the next scheduled slot"
        );
    }

    #[test]
    fn archives_dbos_schedule_is_explicitly_opted_in() {
        assert!(!dbos_enabled(|_| None).unwrap());
        assert!(!dbos_enabled(|_| Some("false".into())).unwrap());
        assert!(dbos_enabled(|_| Some("true".into())).unwrap());
        assert_eq!(
            dbos_enabled(|_| Some("yes".into())).unwrap_err(),
            "NAVIGATOR_DBOS_ARCHIVES_ENABLED must be true or false"
        );
        assert_eq!(DBOS_ENABLED_ENV, "NAVIGATOR_DBOS_ARCHIVES_ENABLED");
    }

    #[test]
    fn existing_requests_and_journaled_summaries_still_decode() {
        let request: RunRequest = serde_json::from_str("{}").unwrap();
        assert!(request.scheduled_for.is_none());
        let summary: SnapshotSummary =
            serde_json::from_str(r#"{"run_date":"2026-10-11","entries":[],"failures":[]}"#)
                .unwrap();
        assert!(summary.scheduled_for.is_none());
    }

    #[tokio::test]
    async fn both_daily_digests_post_to_the_requested_channel() {
        let slack = workflows::CapturingSlackBot::new();
        for time in ["05:55", "17:55"] {
            let mut summary = empty_summary();
            summary.scheduled_for = Some(format!("2026-10-11T{time}:00Z").parse().unwrap());
            let message = digest_message(
                &summary,
                lookup(&[("NAVIGATOR_ARCHIVES_BUCKET", "sample-archives")]),
            )
            .unwrap();
            post_digest(&slack, "sample-messages", true, &message)
                .await
                .unwrap();
        }
        let posted = slack.posted_messages();
        assert_eq!(posted.len(), 2);
        assert!(posted
            .iter()
            .all(|(channel, _)| channel == "sample-messages"));
        assert!(posted[0].1.contains("2026-10-11 05:55 UTC"));
        assert!(posted[1].1.contains("2026-10-11 17:55 UTC"));
    }

    #[tokio::test]
    async fn dbos_digest_recovery_reuses_the_slot_correlated_slack_effect() {
        let slack = workflows::CapturingSlackBot::new();
        for _attempt in 0..2 {
            post_digest_idempotent(
                &slack,
                "synthetic-channel",
                true,
                "synthetic archive digest",
                "archives-2026-10-11T05:55:00Z-digest",
            )
            .await
            .unwrap();
        }
        assert_eq!(
            slack.posted_messages().len(),
            1,
            "a checkpoint replay must not duplicate a correlated notice"
        );
    }

    #[tokio::test]
    async fn a_missing_bot_or_channel_cannot_silently_capture_the_digest() {
        let slack = workflows::CapturingSlackBot::new();
        assert!(post_digest(&slack, "sample-messages", false, "digest")
            .await
            .is_err());
        assert!(post_digest(&slack, " ", true, "digest").await.is_err());
        assert!(slack.posted_messages().is_empty());
    }

    #[tokio::test]
    async fn slack_transport_failure_is_retryable_and_recovery_delivers_the_digest() {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut bodies = Vec::new();
            for status in [500, 200] {
                let (stream, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                assert!(line.starts_with("POST /chat.postMessage "), "{line}");
                let mut length = 0;
                loop {
                    line.clear();
                    reader.read_line(&mut line).await.unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).await.unwrap();
                bodies.push(serde_json::from_slice::<serde_json::Value>(&body).unwrap());
                let response = r#"{"ok":true,"channel":"sample-messages","ts":"1.0"}"#;
                let wire = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                    response.len()
                );
                reader.get_mut().write_all(wire.as_bytes()).await.unwrap();
            }
            bodies
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let slack =
                workflows::SlackBotClient::with_base_url("test-token", format!("http://{address}"));
            assert!(
                post_digest(&slack, "sample-messages", true, "scheduled digest")
                    .await
                    .is_err()
            );
            post_digest(&slack, "sample-messages", true, "scheduled digest")
                .await
                .unwrap();
            let bodies = server.await.unwrap();
            assert_eq!(bodies.len(), 2);
            for body in bodies {
                assert_eq!(body["channel"], "sample-messages");
                assert!(body["text"].as_str().unwrap().contains("scheduled digest"));
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn two_daily_snapshots_and_a_retry_do_not_mix_full_table_generations() {
        use cloud::{FsStorage, StorageService};
        use iceberg::spec::{FormatVersion, ManifestList, TableMetadata};

        let db = store::surreal::test_support::mem().await;
        db.query("CREATE person:sample SET name = 'Sample', email = 'sample@example.com'")
            .await
            .unwrap()
            .check()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let storage = FsStorage::new(dir.path()).await.unwrap();
        let morning: chrono::DateTime<chrono::Utc> = "2026-10-11T05:55:00Z".parse().unwrap();
        let evening: chrono::DateTime<chrono::Utc> = "2026-10-11T17:55:00Z".parse().unwrap();
        for (scheduled, expected_rows, version) in [(morning, 1, 1), (evening, 2, 2)] {
            if scheduled == evening {
                db.query("CREATE person:second SET name = 'Second', email = 'second@example.com'")
                    .await
                    .unwrap()
                    .check()
                    .unwrap();
            }
            let summary = crate::snapshot_all(&db, &storage, Some(scheduled)).await;
            assert!(summary.failures.is_empty(), "{:?}", summary.failures);
            // A crash after an upload re-extracts into the same generation key.
            let retry = crate::snapshot_all(&db, &storage, Some(scheduled)).await;
            let person = summary
                .entries
                .iter()
                .find(|entry| entry.table == "person")
                .unwrap();
            let retried = retry
                .entries
                .iter()
                .find(|entry| entry.table == "person")
                .unwrap();
            assert_eq!(person.key, retried.key);
            assert_eq!(person.rows, expected_rows);
            let generation = person.key.strip_suffix("snapshot.parquet").unwrap();
            assert_eq!(storage.list(generation).await.unwrap().len(), 1);
            let lines = promote_iceberg_metadata_over(
                &storage,
                "sample-archives",
                &summary,
                scheduled.timestamp_millis(),
            )
            .await;
            assert!(
                lines.iter().all(|line| !line.contains("FAILED")),
                "{lines:?}"
            );
            let metadata = storage
                .get(&format!(
                    "application/person/metadata/v{version}.metadata.json"
                ))
                .await
                .unwrap();
            let table: TableMetadata = serde_json::from_slice(&metadata.bytes).unwrap();
            assert_eq!(table.snapshots().count(), version);
            let current = table.current_snapshot().unwrap();
            let list_key = current
                .manifest_list()
                .strip_prefix("gs://sample-archives/")
                .unwrap();
            let list_bytes = storage.get(list_key).await.unwrap();
            let list =
                ManifestList::parse_with_version(&list_bytes.bytes, FormatVersion::V2).unwrap();
            // Read the persisted Avro independently of the authoring path.
            assert_eq!(list.entries().len(), 1);
            assert_eq!(
                list.entries()[0].added_rows_count,
                Some(u64::try_from(expected_rows).unwrap())
            );
        }
    }

    fn generation(key: &str) -> crate::postgres::PostgresGeneration {
        crate::postgres::PostgresGeneration {
            source: crate::postgres::POSTGRES_LANE.into(),
            cutoff_at: "2026-10-12T05:55:01Z".parse().unwrap(),
            snapshot: "100:100:".into(),
            server_version: "16".into(),
            manifest_key: "postgres/navigator/_generations/dt=2026-10-12/run=0555/generation.json"
                .into(),
            tables: vec![crate::postgres::PostgresTableEntry {
                table: "post".into(),
                rows: 1,
                bytes: 0,
                key: key.into(),
                sha256: String::new(),
            }],
        }
    }

    #[tokio::test]
    async fn only_a_published_postgres_generation_is_promoted_and_reported() {
        use crate::postgres::PostgresExport;
        use cloud::{FsStorage, StorageService};

        let dir = tempfile::tempdir().unwrap();
        let storage = FsStorage::new(dir.path()).await.unwrap();
        let key = "postgres/navigator/post/data/dt=2026-05-29/run=0555/snapshot.parquet";
        let batch = crate::batch_from_rows(&[serde_json::json!({"slug": "sample"})])
            .unwrap()
            .unwrap();
        storage
            .put(
                key,
                &crate::encode_parquet(&batch).unwrap(),
                "application/vnd.apache.parquet",
            )
            .await
            .unwrap();
        let bucket = lookup(&[("NAVIGATOR_ARCHIVES_BUCKET", "sample-archives")]);

        let mut failed = empty_summary();
        failed.postgres = Some(PostgresExport::Failed {
            error: "synthetic outage".into(),
        });
        let lines = promote_iceberg_metadata_over(&storage, "sample-archives", &failed, 1).await;
        assert!(
            lines.iter().all(|line| !line.starts_with("postgres/")),
            "{lines:?}"
        );
        assert!(storage
            .list("postgres/navigator/post/metadata/")
            .await
            .unwrap()
            .is_empty());
        assert!(digest_message(&failed, &bucket)
            .unwrap()
            .contains("⚠️ PostgreSQL generation not published: synthetic outage"));

        let mut published = empty_summary();
        published.postgres = Some(PostgresExport::Published(generation(key)));
        assert!(super::run_report(&published, "synthetic").is_ok());
        let lines = promote_iceberg_metadata_over(&storage, "sample-archives", &published, 2).await;
        assert!(
            lines.contains(&"postgres/navigator/post v1".to_string()),
            "{lines:?}"
        );
        assert!(digest_message(&published, &bucket).unwrap().contains(
            "PostgreSQL generation published: 1 tables, 1 rows, cutoff 2026-10-12 05:55:01 UTC"
        ));
        assert!(!digest_message(&empty_summary(), &bucket)
            .unwrap()
            .contains("PostgreSQL"));
    }

    #[tokio::test]
    async fn paired_report_preserves_both_watermarks_without_claiming_atomicity() {
        let database = |name: &str, snapshot: &str| crate::coverage::DatabaseManifest {
            database: name.into(),
            database_name: name.into(),
            cutoff_at: "2026-10-12T05:55:01Z".parse().unwrap(),
            snapshot: snapshot.into(),
            server_version: "16".into(),
            extensions: Vec::new(),
            column_schema_sha256: String::new(),
            coverage: crate::coverage::CoverageManifest {
                relations: Vec::new(),
            },
            tables: Vec::new(),
            non_mvcc_relations: Vec::new(),
            iceberg_metadata_keys: Vec::new(),
            schemas: Vec::new(),
        };
        let mut summary = empty_summary();
        summary.postgres = Some(crate::postgres::PostgresExport::Paired(
            crate::coverage::PairedManifest {
                generation: uuid::Uuid::new_v4(),
                databases: vec![
                    database("navigator", "100:100:"),
                    database("forgejo", "101:101:"),
                ],
                cross_database_atomic: false,
            },
        ));
        let decoded: SnapshotSummary =
            serde_json::from_slice(&serde_json::to_vec(&summary).unwrap()).unwrap();
        assert_eq!(decoded.postgres, summary.postgres);
        assert!(super::run_report(&summary, "synthetic").is_ok());
        let message = digest_message(
            &summary,
            lookup(&[("NAVIGATOR_ARCHIVES_BUCKET", "sample-archives")]),
        )
        .unwrap();
        assert!(message.contains("PostgreSQL navigator:"));
        assert!(message.contains("PostgreSQL forgejo:"));
        assert!(message.contains("100:100:"));
        assert!(message.contains("101:101:"));
        assert!(message.contains("atomic recovery is unverified"));
        assert!(message.contains("bytes require separate backups"));
    }

    #[test]
    fn postgres_only_report_counts_exported_rows() {
        let mut summary = empty_summary();
        summary.postgres = Some(crate::postgres::PostgresExport::Published(generation(
            "synthetic",
        )));
        let report = super::run_report(&summary, "synthetic").unwrap();
        assert_eq!((report.tables, report.rows), (1, 1));
    }

    #[test]
    fn failed_postgres_coverage_cannot_return_a_successful_run_report() {
        let mut summary = empty_summary();
        assert!(super::run_report(&summary, "synthetic").is_ok());
        summary.postgres = Some(crate::postgres::PostgresExport::Failed {
            error: "synthetic coverage failure".into(),
        });
        assert!(super::run_report(&summary, "synthetic").is_err());
    }

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |k: &str| map.get(k).cloned()
    }

    fn empty_summary() -> SnapshotSummary {
        summary_with(Vec::new(), Vec::new())
    }

    fn summary_with(entries: Vec<SnapshotEntry>, failures: Vec<TableFailure>) -> SnapshotSummary {
        SnapshotSummary {
            run_date: NaiveDate::from_ymd_opt(2026, 5, 29).unwrap(),
            scheduled_for: None,
            entries,
            failures,
            postgres: None,
        }
    }

    fn entry(table: &str, rows: usize) -> SnapshotEntry {
        SnapshotEntry {
            table: table.into(),
            rows,
            bytes: rows * 16,
            key: format!("application/{table}/data/dt=2026-05-29/part-0.parquet"),
            drift: DriftDecision::Unchanged,
        }
    }

    #[test]
    fn build_report_threads_bucket_through() {
        let report = build_report(
            &empty_summary(),
            lookup(&[("NAVIGATOR_ARCHIVES_BUCKET", "proj-archives")]),
        )
        .expect("a named bucket resolves");
        assert_eq!(report.bucket, "proj-archives");
        assert!(report.snapshots.is_empty());
        assert!(report.failures.is_empty());
    }

    /// The digest fails rather than naming a bucket it could not resolve.
    /// An empty value is the same misconfiguration as an absent one — a
    /// deployment that sets the key to nothing has not named a bucket.
    #[test]
    fn build_report_fails_closed_when_archives_bucket_is_absent() {
        for get in [
            &(|_: &str| None) as &dyn Fn(&str) -> Option<String>,
            &|key: &str| (key == "NAVIGATOR_ARCHIVES_BUCKET").then(String::new),
        ] {
            let error = build_report(&empty_summary(), get)
                .expect_err("an unresolved bucket must fail the digest");
            assert!(
                matches!(error, StorageError::MissingEnv("NAVIGATOR_ARCHIVES_BUCKET")),
                "the failure must name the key it needs; got {error:?}"
            );
        }
    }

    #[test]
    fn build_report_carries_snapshots_and_failures_through() {
        // The empty-summary tests above prove how the bucket resolves; this
        // proves the report actually threads the journaled snapshot data —
        // the lines the digest renders from — rather than silently dropping it.
        let report = build_report(
            &summary_with(
                vec![entry("persons", 312), entry("documents", 1_204)],
                vec![TableFailure {
                    table: "notations".into(),
                    error: "connection reset".into(),
                }],
            ),
            lookup(&[("NAVIGATOR_ARCHIVES_BUCKET", "proj-archives")]),
        )
        .expect("a named bucket resolves");
        assert_eq!(
            report.run_date,
            NaiveDate::from_ymd_opt(2026, 5, 29).unwrap()
        );
        let tables: Vec<&str> = report.snapshots.iter().map(|e| e.table.as_str()).collect();
        assert_eq!(tables, ["persons", "documents"]);
        assert_eq!(report.snapshots[0].rows, 312);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].table, "notations");
    }

    #[test]
    fn digest_message_renders_tables_links_and_failures_from_a_summary() {
        // End-to-end for the seam the handler calls: a journaled summary plus
        // the archive bucket becomes the exact Slack digest, per-table GCS links
        // pointed at the threaded bucket, with any failure surfaced as a ⚠️ line.
        let msg = digest_message(
            &summary_with(
                vec![entry("persons", 312)],
                vec![TableFailure {
                    table: "documents".into(),
                    error: "timeout".into(),
                }],
            ),
            lookup(&[("NAVIGATOR_ARCHIVES_BUCKET", "proj-archives")]),
        )
        .expect("a named bucket resolves");
        assert!(msg.starts_with("🧊"), "leads with the iceberg glyph: {msg}");
        assert!(msg.contains("(1 tables)"), "header counts snapshots: {msg}");
        assert!(
            msg.contains("• *persons* — 312 rows · "),
            "per-table line: {msg}"
        );
        assert!(
            msg.contains(
                "<https://console.cloud.google.com/storage/browser/proj-archives/application/persons|view in GCP>"
            ),
            "link points into the threaded bucket: {msg}"
        );
        assert!(
            msg.contains("• ⚠️ *documents* — snapshot failed"),
            "failed table surfaces as a warning: {msg}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn promotion_authors_iceberg_metadata_for_a_store_table_and_chains_a_second_run() {
        use crate::runner::snapshot_all;
        use cloud::{FsStorage, StorageService};
        use iceberg::spec::TableMetadata;

        let db = store::surreal::test_support::mem().await;
        db.query(
            "CREATE person:analyst SET \
             name = 'Analyst', \
             email = 'analyst@example.com', \
             inserted_at = d'2026-08-09T00:00:00Z', \
             updated_at = d'2026-08-09T00:00:00Z'",
        )
        .await
        .unwrap()
        .check()
        .unwrap();

        let dir = tempfile::TempDir::new().unwrap();
        let storage = FsStorage::new(dir.path().to_path_buf()).await.unwrap();
        // Phase 1: writes `application/person/data/dt=<today>/*.parquet`.
        let summary = snapshot_all(&db, &storage, None).await;
        assert!(summary.failures.is_empty(), "{:#?}", summary.failures);

        // Phase 3, first run: author Iceberg metadata over that data file.
        let now_ms = chrono::Utc::now().timestamp_millis();
        promote_iceberg_metadata_over(&storage, "exports", &summary, now_ms).await;

        let meta = storage
            .get("application/person/metadata/v1.metadata.json")
            .await
            .unwrap();
        let table_metadata: TableMetadata = serde_json::from_slice(&meta.bytes).unwrap();
        assert_eq!(
            table_metadata.snapshots().count(),
            1,
            "first run authors exactly one snapshot"
        );
        let hint = storage
            .get("application/person/metadata/version-hint.text")
            .await
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&hint.bytes).trim(), "1");

        // Crash window: the Iceberg metadata has persisted but the DBOS step
        // checkpoint has not. Replaying the same deterministic slot must
        // recognize its snapshot id instead of appending a duplicate.
        promote_iceberg_metadata_over(&storage, "exports", &summary, now_ms).await;
        let hint = storage
            .get("application/person/metadata/version-hint.text")
            .await
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&hint.bytes).trim(), "1");
        let replayed: TableMetadata = serde_json::from_slice(
            &storage
                .get("application/person/metadata/v1.metadata.json")
                .await
                .unwrap()
                .bytes,
        )
        .unwrap();
        assert_eq!(replayed.snapshots().count(), 1);

        // Second run over the same storage and day: chains onto the prior
        // snapshot log rather than starting a fresh table (store tables never
        // prune — expire_before_ms is always None for the application lane).
        promote_iceberg_metadata_over(&storage, "exports", &summary, now_ms + 1000).await;

        let meta = storage
            .get("application/person/metadata/v2.metadata.json")
            .await
            .unwrap();
        let table_metadata: TableMetadata = serde_json::from_slice(&meta.bytes).unwrap();
        assert_eq!(
            table_metadata.snapshots().count(),
            2,
            "second run chains onto the prior log — a two-entry snapshot log"
        );
        let hint = storage
            .get("application/person/metadata/version-hint.text")
            .await
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&hint.bytes).trim(), "2");
    }
}

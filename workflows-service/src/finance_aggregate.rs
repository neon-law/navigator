//! The `FinanceAggregate` Restate workflow: ingest a Project's matter rows from
//! object storage, run the fee waterfall, and keep the result in `SurrealDB` and
//! object storage.
//!
//! Three journaled steps, so a retry of one never repeats another:
//!
//! 1. `ctx.run("ingest")`: read `finance/<project_code>/input.json` through the
//!    worker's [`StorageService`] and parse it. The parsed input is journaled,
//!    so a replay aggregates exactly the rows the first attempt read even if
//!    the object is replaced meanwhile. A missing or malformed input is a
//!    terminal error (retrying cannot fix it); a storage outage is retried.
//! 2. `ctx.run("aggregate")`: [`store::finance_aggregate::aggregate`] over the
//!    journaled input. The clock read lives inside the step, so a replay reuses
//!    the recorded `computed_at`.
//! 3. `ctx.run("store")`: write the report JSON to
//!    `finance/<project_code>/<run_id>.json` and upsert the `finance_aggregate`
//!    row keyed on the run id. Both writes are idempotent, so a retry
//!    rewrites rather than duplicates.
//!
//! The workflow key is the run id. A manual run uses `manual-<uuid>`, so an
//! operator's run is never deduplicated against an earlier one.
//!
//! **Bucket.** The worker's `cloud::from_env()` handle: the private documents
//! bucket (`NAVIGATOR_DOCUMENTS_BUCKET`, else `NAVIGATOR_STORAGE_BUCKET`)
//! that `web` also reads, so no new bucket or credential is needed. The
//! `finance/` prefix keeps it clear of document keys.
//!
//! **Telemetry.** A `finance_aggregate.run` span parented on the Restate
//! `traceparent` header, with one child span per step. Fields are the run id,
//! Project id and code, and row counts. Amounts, attorney names and matter ids
//! never enter a span (see the observability skill). Dash0 receives the spans
//! through the deployment's collector; there is no Dash0 code here.

use std::sync::Arc;

use chrono::Utc;
use cloud::{StorageError, StorageService};
use restate_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use store::finance_aggregate::{self, FinanceInput, Parameters};
use store::surreal::SurrealDb;
use tracing::Instrument;
use uuid::Uuid;

/// Request body for `FinanceAggregate::run`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RunRequest {
    pub project_id: Uuid,
    /// The Project's immutable code; names the object-storage prefix.
    pub project_code: String,
    /// Percentages for this run. Omitted means the documented 28 / 70 / 80.
    #[serde(default)]
    pub parameters: Option<Parameters>,
}

/// Invocation output: identifiers and counts only.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct FinanceAggregateReport {
    pub run_id: String,
    pub matters: i64,
    pub attorneys: usize,
    pub report_key: String,
}

/// Service registered with the Restate endpoint: the worker's `SurrealDB`
/// handle and object storage, both opened once at boot.
#[derive(Clone)]
pub struct FinanceAggregateService {
    surreal: SurrealDb,
    storage: Arc<dyn StorageService>,
}

impl FinanceAggregateService {
    #[must_use]
    pub fn new(surreal: SurrealDb, storage: Arc<dyn StorageService>) -> Self {
        Self { surreal, storage }
    }
}

fn terminal(message: impl Into<String>) -> HandlerError {
    HandlerError::from(TerminalError::new(message))
}

/// Read and parse the input document for `project_code`.
///
/// # Errors
///
/// A terminal error when the object is absent or is not a valid input; a
/// retryable error for any other storage failure.
pub async fn read_input(
    storage: &dyn StorageService,
    project_code: &str,
) -> Result<FinanceInput, HandlerError> {
    let key = finance_aggregate::input_key(project_code);
    match storage.get(&key).await {
        Ok(object) => serde_json::from_slice(&object.bytes)
            .map_err(|error| terminal(format!("finance input {key} is not valid: {error}"))),
        Err(StorageError::NotFound(_)) => Err(terminal(format!(
            "finance input {key} has not been uploaded"
        ))),
        Err(error) => Err(HandlerError::from(error)),
    }
}

/// The `aggregate` step's work: the waterfall over the journaled input, with
/// the clock read inside the step so a replay reuses the recorded instant.
///
/// # Errors
///
/// A terminal error when the input fails validation; retrying cannot fix it.
pub fn compute(
    input: &FinanceInput,
    parameters: Parameters,
    run_id: &str,
) -> Result<finance_aggregate::FinanceReport, HandlerError> {
    finance_aggregate::aggregate(input, parameters, run_id, Utc::now())
        .map_err(|error| terminal(format!("finance input rejected: {error}")))
}

/// The `store` step's work: the report JSON to object storage, then the row.
/// Both writes are idempotent (fixed key, row keyed on the run id), so a retry
/// of the step rewrites rather than duplicates.
///
/// # Errors
///
/// A retryable error when either write fails.
pub async fn persist(
    storage: &dyn StorageService,
    surreal: &SurrealDb,
    project_id: Uuid,
    key: &str,
    report: &finance_aggregate::FinanceReport,
) -> Result<(), HandlerError> {
    let bytes = serde_json::to_vec(report)
        .map_err(|error| terminal(format!("report could not be encoded: {error}")))?;
    storage
        .put(key, &bytes, "application/json")
        .await
        .map_err(HandlerError::from)?;
    finance_aggregate::upsert(surreal, project_id, report)
        .await
        .map_err(HandlerError::from)?;
    Ok(())
}

/// The root span of a `FinanceAggregate::run` invocation. Identifiers only.
fn run_span(
    run_id: &str,
    req: &RunRequest,
    invocation_id: impl std::fmt::Display,
) -> tracing::Span {
    tracing::info_span!(
        "finance_aggregate.run",
        run_id = %run_id,
        project_id = %req.project_id,
        project_code = %req.project_code,
        invocation_id = %invocation_id,
        navigator.keep = true,
        matters = tracing::field::Empty,
    )
}

#[restate_sdk::workflow(name = "FinanceAggregate")]
impl FinanceAggregateService {
    #[restate_sdk::handler]
    async fn run(
        &self,
        ctx: WorkflowContext<'_>,
        req: Json<RunRequest>,
    ) -> Result<Json<FinanceAggregateReport>, HandlerError> {
        let req = req.into_inner();
        let run_id = ctx.key().to_string();
        let span = run_span(&run_id, &req, ctx.invocation_id());
        {
            let headers = ctx.headers();
            telemetry::set_span_parent(
                &span,
                headers.get("traceparent").map(String::as_str),
                headers.get("tracestate").map(String::as_str),
            );
        }
        let parameters = req.parameters.unwrap_or_default();

        async {
            let storage = Arc::clone(&self.storage);
            let code = req.project_code.clone();
            let input: FinanceInput = ctx
                .run(move || {
                    let span = tracing::info_span!(
                        "finance_aggregate.ingest",
                        matters = tracing::field::Empty
                    );
                    let recorded = span.clone();
                    async move {
                        let input = read_input(storage.as_ref(), &code).await?;
                        recorded.record("matters", input.matters.len());
                        Ok(Json(input))
                    }
                    .instrument(span)
                })
                .name("ingest")
                .await?
                .into_inner();
            tracing::Span::current().record("matters", input.matters.len());

            let aggregate_run_id = run_id.clone();
            let report = ctx
                .run(move || {
                    let span = tracing::info_span!(
                        "finance_aggregate.aggregate",
                        groups = input.attorneys.len()
                            + input.states.len()
                            + input.verticals.len()
                            + input.months.len()
                    );
                    async move { compute(&input, parameters, &aggregate_run_id).map(Json) }
                        .instrument(span)
                })
                .name("aggregate")
                .await?
                .into_inner();

            let surreal = self.surreal.clone();
            let storage = Arc::clone(&self.storage);
            let project_id = req.project_id;
            let key = finance_aggregate::report_key(&req.project_code, &run_id);
            let stored_report = report.clone();
            let stored_key = key.clone();
            ctx.run(move || {
                let span = tracing::info_span!("finance_aggregate.store", report_key = %stored_key);
                async move {
                    persist(
                        storage.as_ref(),
                        &surreal,
                        project_id,
                        &stored_key,
                        &stored_report,
                    )
                    .await
                }
                .instrument(span)
            })
            .name("store")
            .await?;

            Ok(Json(FinanceAggregateReport {
                run_id,
                matters: report.totals.matters,
                attorneys: report.by_attorney.len(),
                report_key: key,
            }))
        }
        .instrument(span)
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cloud::FsStorage;

    const INPUT: &str = include_str!("../../store/tests/data/finance/sample-finance-input.json");

    #[test]
    fn the_run_root_span_is_kept_by_the_collector() {
        let req = RunRequest {
            project_id: Uuid::now_v7(),
            project_code: "sample".to_owned(),
            parameters: None,
        };
        let fields = crate::span_capture::fields_of(|| run_span("run-1", &req, "inv_1"));
        assert_eq!(
            fields.get("navigator.keep").map(String::as_str),
            Some("true")
        );
    }

    async fn storage() -> (tempfile::TempDir, FsStorage) {
        let dir = tempfile::tempdir().unwrap();
        let fs = FsStorage::new(dir.path()).await.unwrap();
        (dir, fs)
    }

    #[tokio::test]
    async fn read_input_parses_the_uploaded_document() {
        let (_dir, fs) = storage().await;
        fs.put(
            &finance_aggregate::input_key("sample-finance"),
            INPUT.as_bytes(),
            "application/json",
        )
        .await
        .unwrap();
        let input = read_input(&fs, "sample-finance").await.unwrap();
        assert_eq!(input.matters.len(), 633);
    }

    #[tokio::test]
    async fn a_missing_input_is_a_terminal_error() {
        let (_dir, fs) = storage().await;
        let error = read_input(&fs, "sample-finance").await.unwrap_err();
        let shown = format!("{error:?}");
        assert!(shown.contains("Terminal"), "{shown}");
        assert!(shown.contains("has not been uploaded"), "{shown}");
    }

    #[tokio::test]
    async fn a_malformed_input_is_a_terminal_error() {
        let (_dir, fs) = storage().await;
        fs.put(
            &finance_aggregate::input_key("sample-finance"),
            b"{not json",
            "application/json",
        )
        .await
        .unwrap();
        let error = read_input(&fs, "sample-finance").await.unwrap_err();
        assert!(format!("{error:?}").contains("Terminal"));
    }

    #[test]
    fn compute_runs_the_waterfall_and_rejects_bad_input() {
        let mut input: FinanceInput = serde_json::from_str(INPUT).unwrap();
        let report = compute(&input, Parameters::default(), "run-1").unwrap();
        assert_eq!(report.run_id, "run-1");
        assert_eq!(report.totals.matters, 633);
        input.matters[0].state = "ZZ".into();
        let error = compute(&input, Parameters::default(), "run-1").unwrap_err();
        let shown = format!("{error:?}");
        assert!(
            shown.contains("Terminal") && shown.contains("rejected"),
            "{shown}"
        );
    }

    #[tokio::test]
    async fn persist_writes_the_object_and_the_row_and_is_replayable() {
        let (_dir, fs) = storage().await;
        let surreal = store::test_support::mem_surreal().await;
        let project = store::test_support::seed_project_surreal(&surreal, "Finance Sample").await;
        let input: FinanceInput = serde_json::from_str(INPUT).unwrap();
        let report = compute(&input, Parameters::default(), "run-1").unwrap();
        let key = finance_aggregate::report_key("sample-finance", "run-1");

        for _ in 0..2 {
            persist(&fs, &surreal, project, &key, &report)
                .await
                .unwrap();
        }

        let object = fs.get(&key).await.unwrap();
        let stored: finance_aggregate::FinanceReport =
            serde_json::from_slice(&object.bytes).unwrap();
        assert_eq!(stored, report);
        let row = finance_aggregate::latest_for_project(&surreal, project)
            .await
            .unwrap();
        assert_eq!(row, Some(report));
    }

    #[test]
    fn request_defaults_the_parameters() {
        let id = Uuid::nil();
        let request: RunRequest =
            serde_json::from_value(serde_json::json!({"project_id": id, "project_code": "c"}))
                .unwrap();
        assert!(request.parameters.is_none());
        assert_eq!(request.parameters.unwrap_or_default().lead_pct, 80);
    }
}

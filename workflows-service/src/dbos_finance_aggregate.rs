//! DBOS adapter for manually requested `FinanceAggregate` runs.
//!
//! The workflow id is supplied by the caller and remains the immutable run id
//! used by the report object and Surreal record. Repeated caller retries join
//! the same run; neither output is duplicated.

use std::sync::Arc;

use dbos::{WorkflowRef, DBOS};
use serde::{Deserialize, Serialize};
use store::finance_aggregate::{self, FinanceInput, Parameters};
use store::surreal::SurrealDb;

use cloud::StorageService;

pub const WORKFLOW: &str = "FinanceAggregate";
pub const ENABLED_ENV: &str = "NAVIGATOR_DBOS_FINANCE_AGGREGATE_ENABLED";

pub type FinanceAggregateWorkflow =
    WorkflowRef<RunRequest, FinanceAggregateReport, FinanceAggregateError>;

#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
pub enum FinanceAggregateError {
    #[error("finance input could not be read")]
    Input,
    #[error("finance input could not be aggregated")]
    Aggregate,
    #[error("finance report could not be persisted")]
    Persist,
}

#[derive(Clone)]
pub struct Context {
    surreal: SurrealDb,
    storage: Arc<dyn StorageService>,
}

impl Context {
    #[must_use]
    pub fn new(surreal: SurrealDb, storage: Arc<dyn StorageService>) -> Self {
        Self { surreal, storage }
    }
}

pub fn enabled(get: impl Fn(&str) -> Option<String>) -> Result<bool, &'static str> {
    match get(ENABLED_ENV).as_deref().map(str::trim) {
        None | Some("" | "false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err("NAVIGATOR_DBOS_FINANCE_AGGREGATE_ENABLED must be true or false"),
    }
}

/// DBOS ids are global across workflow names, unlike Restate's service/key
/// pair. Preserve the Restate run key as the suffix of the namespaced id.
#[must_use]
pub fn workflow_id(run_id: &str) -> String {
    format!("{WORKFLOW}:{run_id}")
}

pub fn register(dbos: &DBOS, context: Context) -> dbos::Result<FinanceAggregateWorkflow> {
    dbos.register_workflow(WORKFLOW, move |request: RunRequest| {
        let context = context.clone();
        async move {
            let id = dbos::workflow_id().unwrap_or_default();
            let Some(run_id) = id.strip_prefix("FinanceAggregate:").map(str::to_owned) else {
                return Err(dbos::Error::Application(FinanceAggregateError::Aggregate));
            };
            let code = request.project_code.clone();
            let storage = Arc::clone(&context.storage);
            let input: FinanceInput = dbos::step("ingest", move || {
                let storage = Arc::clone(&storage);
                let code = code.clone();
                async move {
                    let key = finance_aggregate::input_key(&code);
                    let object = storage
                        .get(&key)
                        .await
                        .map_err(|_| dbos::Error::Application(FinanceAggregateError::Input))?;
                    serde_json::from_slice(&object.bytes)
                        .map_err(|_| dbos::Error::Application(FinanceAggregateError::Input))
                }
            })
            .await?;

            let aggregate_input = input.clone();
            let aggregate_id = run_id.clone();
            let parameters = request.parameters.unwrap_or_default();
            let report = dbos::step("aggregate", move || {
                let input = aggregate_input.clone();
                let run_id = aggregate_id.clone();
                async move {
                    finance_aggregate::aggregate(&input, parameters, &run_id, chrono::Utc::now())
                        .map_err(|_| dbos::Error::Application(FinanceAggregateError::Aggregate))
                }
            })
            .await?;

            let storage = Arc::clone(&context.storage);
            let surreal = context.surreal.clone();
            let project_id = request.project_id;
            let key = finance_aggregate::report_key(&request.project_code, &run_id);
            let stored_key = key.clone();
            let stored_report = report.clone();
            dbos::step("persist", move || {
                let storage = Arc::clone(&storage);
                let key = stored_key.clone();
                let report = stored_report.clone();
                let surreal = surreal.clone();
                async move {
                    let bytes = serde_json::to_vec(&report)
                        .map_err(|_| dbos::Error::Application(FinanceAggregateError::Persist))?;
                    storage
                        .put(&key, &bytes, "application/json")
                        .await
                        .map_err(|_| dbos::Error::Application(FinanceAggregateError::Persist))?;
                    finance_aggregate::upsert(&surreal, project_id, &report)
                        .await
                        .map_err(|_| dbos::Error::Application(FinanceAggregateError::Persist))
                }
            })
            .await?;

            Ok(FinanceAggregateReport {
                run_id,
                matters: report.totals.matters,
                attorneys: report.by_attorney.len(),
                report_key: key,
            })
        }
    })
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RunRequest {
    pub project_id: uuid::Uuid,
    pub project_code: String,
    #[serde(default)]
    pub parameters: Option<Parameters>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct FinanceAggregateReport {
    pub run_id: String,
    pub matters: i64,
    pub attorneys: usize,
    pub report_key: String,
}

#[cfg(test)]
mod tests {
    use super::{enabled, workflow_id, ENABLED_ENV};

    #[test]
    fn finance_run_ids_are_namespaced_and_keep_the_restate_key() {
        let id = workflow_id("manual-123");
        assert_eq!(id, "FinanceAggregate:manual-123");
        assert_eq!(id.strip_prefix("FinanceAggregate:"), Some("manual-123"));
        assert_ne!(id, "Archives:manual-123");
    }

    #[test]
    fn registration_is_opt_in_and_rejects_invalid_values() {
        assert!(!enabled(|_| None).unwrap());
        assert!(!enabled(|key| (key == ENABLED_ENV).then(|| "false".to_owned())).unwrap());
        assert!(enabled(|key| (key == ENABLED_ENV).then(|| "true".to_owned())).unwrap());
        assert!(enabled(|key| (key == ENABLED_ENV).then(|| "yes".to_owned())).is_err());
    }
}

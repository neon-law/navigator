//! Restate adapter for the shared `BillingDigest` core.

use restate_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use workflows::EmailService;

pub use workflows::billing_digest::{
    build_digest_email, AccountCost, BillingDigestReport, DigestOutcome,
};
use workflows::billing_digest::{query_report, DigestConfig};

/// Request body for `BillingDigest::run`; the trigger supplies no fields.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct RunRequest {}

#[derive(Clone)]
pub struct BillingDigestService {
    email: Arc<dyn EmailService>,
}

impl BillingDigestService {
    #[must_use]
    pub fn new(email: Arc<dyn EmailService>) -> Self {
        Self { email }
    }
}

#[restate_sdk::workflow(name = "BillingDigest")]
impl BillingDigestService {
    #[restate_sdk::handler]
    async fn run(
        &self,
        ctx: WorkflowContext<'_>,
        _req: Json<RunRequest>,
    ) -> Result<Json<DigestOutcome>, HandlerError> {
        let cfg = DigestConfig::from_env(|key| std::env::var(key).ok());
        let Some(query) = cfg.query.clone() else {
            tracing::info!(
                "BILLING_EXPORT_TABLE / BIGQUERY_PROJECT unset; skipping billing digest (no send)"
            );
            return Ok(Json(DigestOutcome {
                sent: false,
                cost_total: 0.0,
                services: 0,
                projects: 0,
            }));
        };
        let window = cfg.window_days;
        let report_json = ctx
            .run(move || async move {
                query_report(&query, window)
                    .await
                    .and_then(|report| {
                        serde_json::to_string(&report).map_err(|error| error.to_string())
                    })
                    .map_err(|error| HandlerError::from(anyhow::anyhow!(error)))
            })
            .name("query")
            .await?;
        let report: BillingDigestReport = serde_json::from_str(&report_json)
            .map_err(|error| HandlerError::from(anyhow::anyhow!(error)))?;
        let outcome = DigestOutcome {
            sent: true,
            cost_total: report.cost_total(),
            services: report.service_count(),
            projects: report.project_count(),
        };
        let email = build_digest_email(&report, &cfg.recipient);
        let service = Arc::clone(&self.email);
        ctx.run(move || async move {
            service
                .send(email)
                .await
                .map(|_| ())
                .map_err(HandlerError::from)
        })
        .name("email")
        .await?;
        Ok(Json(outcome))
    }
}

//! Daily DBOS port of the GCP BillingDigest.
//!
//! The stable UTC-date workflow id joins executor replicas and restart recovery.
//! Slack webhooks have no delivery idempotency key, so the email step is at least once.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use dbos::{CancellationToken, WorkflowHandle, WorkflowRef, DBOS};
use serde::{Deserialize, Serialize};

use super::SettingsError;
use crate::billing_digest::{BillingDigestReport, DigestConfig, DigestOutcome, QueryConfig};
use crate::{EmailService, SlackOpsDelivery};

pub const WORKFLOW: &str = "BillingDigest";
pub const ENABLED_ENV: &str = "NAVIGATOR_DBOS_BILLING_DIGEST_ENABLED";
const RETRY_START: Duration = Duration::from_mins(1);
const SLOT_HOUR_UTC: u32 = 13;
const SLOT_MINUTE_UTC: u32 = 15;

pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;
pub type BillingDigestWorkflow = WorkflowRef<(), DigestOutcome, BillingDigestError>;

#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
pub enum BillingDigestError {
    #[error("billing export query failed")]
    Query,
    #[error("operations notice delivery failed")]
    Notify,
}

#[async_trait::async_trait]
pub trait BillingDigestSource: Send + Sync {
    async fn query(
        &self,
        query: &QueryConfig,
        window_days: u32,
    ) -> Result<BillingDigestReport, BillingDigestError>;
}

struct BigQuerySource;

#[async_trait::async_trait]
impl BillingDigestSource for BigQuerySource {
    async fn query(
        &self,
        query: &QueryConfig,
        window_days: u32,
    ) -> Result<BillingDigestReport, BillingDigestError> {
        crate::billing_digest::query_report(query, window_days)
            .await
            .map_err(|_| {
                tracing::error!("BillingDigest BigQuery query failed");
                BillingDigestError::Query
            })
    }
}

#[derive(Clone)]
pub struct Context {
    source: Arc<dyn BillingDigestSource>,
    email: Arc<dyn EmailService>,
    config: DigestConfig,
}

impl Context {
    /// Resolve the production adapters only when this independent schedule is enabled.
    pub fn from_lookup<F: Fn(&str) -> Option<String>>(lookup: F) -> Result<Self, SettingsError> {
        let config = DigestConfig::from_env(&lookup);
        let webhook = if config.query.is_some() {
            Some(
                lookup("SLACK_WEBHOOK_URL")
                    .map(|value| value.trim().to_owned())
                    .filter(|value| !value.is_empty())
                    .ok_or(SettingsError::Missing("SLACK_WEBHOOK_URL"))?,
            )
        } else {
            None
        };
        let notifier = crate::notify::notifier_from_lookup(|key| {
            if key == "SLACK_WEBHOOK_URL" {
                webhook.clone()
            } else {
                lookup(key)
            }
        });
        Ok(Self {
            source: Arc::new(BigQuerySource),
            email: Arc::new(SlackOpsDelivery::new(notifier)),
            config,
        })
    }

    /// Construct a deterministic context for PostgreSQL-backed workflow tests.
    pub fn with_adapters(
        source: Arc<dyn BillingDigestSource>,
        email: Arc<dyn EmailService>,
        config: DigestConfig,
    ) -> Self {
        Self {
            source,
            email,
            config,
        }
    }
}

pub fn enabled(lookup: impl Fn(&str) -> Option<String>) -> Result<bool, SettingsError> {
    match lookup(ENABLED_ENV).as_deref().map(str::trim) {
        None | Some("" | "false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err(SettingsError::Invalid(ENABLED_ENV)),
    }
}

pub fn system_clock() -> Clock {
    Arc::new(Utc::now)
}

/// Today's 13:00 UTC slot, identified only by the UTC date.
#[must_use]
pub fn slot(now: DateTime<Utc>) -> DateTime<Utc> {
    now.date_naive()
        .and_hms_opt(SLOT_HOUR_UTC, 0, 0)
        .and_then(|time| time.and_local_timezone(Utc).single())
        .unwrap_or(now)
}

#[must_use]
pub fn workflow_id(slot: DateTime<Utc>) -> String {
    format!("{WORKFLOW}-{}", slot.format("%Y-%m-%d"))
}

/// The next eligible 13:00 UTC slot. The current day's slot expires at 13:15.
#[must_use]
pub fn next_slot(now: DateTime<Utc>) -> DateTime<Utc> {
    let today = slot(now);
    if now < today || now < today + ChronoDuration::minutes(i64::from(SLOT_MINUTE_UTC)) {
        today
    } else {
        today + ChronoDuration::days(1)
    }
}

pub fn register(dbos: &DBOS, context: Context) -> dbos::Result<BillingDigestWorkflow> {
    dbos.register_workflow(WORKFLOW, move |()| {
        let context = context.clone();
        async move {
            let Some(query) = context.config.query.clone() else {
                tracing::info!(
                    "billing export is not configured; skipping BillingDigest (no send)"
                );
                return dbos::Result::<_, BillingDigestError>::Ok(DigestOutcome {
                    sent: false,
                    cost_total: 0.0,
                    services: 0,
                    projects: 0,
                });
            };
            let source = Arc::clone(&context.source);
            let window = context.config.window_days;
            let report = dbos::step("query", move || {
                let source = Arc::clone(&source);
                let query = query.clone();
                async move {
                    source
                        .query(&query, window)
                        .await
                        .map_err(dbos::Error::Application)
                }
            })
            .await?;
            let outcome = DigestOutcome {
                sent: true,
                cost_total: report.cost_total(),
                services: report.service_count(),
                projects: report.project_count(),
            };
            let email =
                crate::billing_digest::build_digest_email(&report, &context.config.recipient);
            let service = Arc::clone(&context.email);
            dbos::step("email", move || {
                let (service, email) = (Arc::clone(&service), email.clone());
                async move {
                    service
                        .send(email)
                        .await
                        .map(|_| ())
                        .map_err(|_| dbos::Error::Application(BillingDigestError::Notify))
                }
            })
            .await?;
            dbos::Result::<_, BillingDigestError>::Ok(outcome)
        }
    })
}

pub async fn fire(
    workflow: &BillingDigestWorkflow,
    slot: DateTime<Utc>,
) -> dbos::Result<WorkflowHandle<DigestOutcome, BillingDigestError>, BillingDigestError> {
    let id = workflow_id(slot);
    workflow
        .start_with(
            (),
            dbos::StartOptions {
                workflow_id: Some(&id),
                ..Default::default()
            },
        )
        .await
}

/// Schedule today's run only before 13:15 UTC. A later engine switch waits for tomorrow.
pub async fn schedule(workflow: BillingDigestWorkflow, clock: Clock, stop: CancellationToken) {
    loop {
        let now = clock();
        let today = slot(now);
        let target = next_slot(now);
        let wait = if target == today && now >= today {
            match fire(&workflow, today).await {
                Ok(_) => (today + ChronoDuration::days(1) - now)
                    .to_std()
                    .unwrap_or(Duration::ZERO),
                Err(error) => {
                    tracing::error!(workflow_id = workflow_id(today), %error, "billing digest start failed");
                    RETRY_START
                }
            }
        } else {
            (target - now).to_std().unwrap_or(Duration::ZERO)
        };
        tokio::select! { () = stop.cancelled() => break, () = tokio::time::sleep(wait) => {} }
    }
}

#[cfg(test)]
mod tests {
    use super::{enabled, slot, workflow_id, Context, SettingsError, ENABLED_ENV};
    use chrono::{TimeZone, Utc};

    #[test]
    fn daily_slot_uses_utc_date_and_defaults_off() {
        let now = Utc.with_ymd_and_hms(2026, 10, 21, 13, 5, 0).unwrap();
        let expected = Utc.with_ymd_and_hms(2026, 10, 21, 13, 0, 0).unwrap();
        assert_eq!(slot(now), expected);
        assert_eq!(workflow_id(expected), "BillingDigest-2026-10-21");
        let before = Utc.with_ymd_and_hms(2026, 10, 21, 12, 59, 59).unwrap();
        let in_slot = Utc.with_ymd_and_hms(2026, 10, 21, 13, 14, 59).unwrap();
        let expired = Utc.with_ymd_and_hms(2026, 10, 21, 13, 15, 0).unwrap();
        assert_eq!(super::next_slot(before), expected);
        assert_eq!(super::next_slot(in_slot), expected);
        assert_eq!(
            super::next_slot(expired),
            expected + chrono::Duration::days(1)
        );
        assert!(!enabled(|_| None).unwrap());
        assert!(enabled(|_| Some("true".into())).unwrap());
        assert_eq!(
            enabled(|_| Some("yes".into())).unwrap_err(),
            SettingsError::Invalid(ENABLED_ENV)
        );
        assert!(Context::from_lookup(|_| None).is_ok());
        let configured = |key: &str| match key {
            "BILLING_EXPORT_TABLE" => Some("project.dataset.export".to_owned()),
            "BIGQUERY_PROJECT" => Some("synthetic-project".to_owned()),
            _ => None,
        };
        assert!(matches!(
            Context::from_lookup(configured),
            Err(SettingsError::Missing("SLACK_WEBHOOK_URL"))
        ));
    }
}

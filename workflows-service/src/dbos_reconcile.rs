//! DBOS adapter for the nightly Xero invoice reconciliation.
//!
//! The existing provider-agnostic reconciliation remains the single source
//! for the business operation. DBOS journals the reconciliation result and
//! delivers the resulting finance notice with a slot-correlated Slack effect.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use dbos::{CancellationToken, StepOptions, WorkflowRef, DBOS};
use serde::{Deserialize, Serialize};
use store::surreal::SurrealDb;
use tokio::time::{sleep, Duration};
use workflows::SlackBot;

use billing::XeroBillingProvider;
use billing_workflows::{reconcile_once, ReconcileReport};

pub const ENABLED_ENV: &str = "NAVIGATOR_DBOS_RECONCILE_INVOICES_ENABLED";
const WORKFLOW: &str = "ReconcileInvoices";
const NOTIFY_ATTEMPTS: u32 = 3;
const SLOT_HOUR_UTC: u32 = 11;
pub type ReconcileWorkflow = WorkflowRef<(), ReconcileReport, ReconcileDbosError>;

#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
pub enum ReconcileDbosError {
    #[error("Xero invoice reconciliation failed")]
    Reconcile,
    #[error("finance notice delivery failed")]
    Notify,
}

#[async_trait::async_trait]
pub trait ReconcileRunner: Send + Sync + 'static {
    async fn run(&self) -> Result<ReconcileReport, ReconcileDbosError>;
}

struct LiveReconcileRunner(SurrealDb);

#[async_trait::async_trait]
impl ReconcileRunner for LiveReconcileRunner {
    async fn run(&self) -> Result<ReconcileReport, ReconcileDbosError> {
        let provider = XeroBillingProvider::from_env().ok_or(ReconcileDbosError::Reconcile)?;
        reconcile_once(&provider, &self.0)
            .await
            .map_err(|_| ReconcileDbosError::Reconcile)
    }
}

#[derive(Clone)]
pub struct Context {
    runner: Arc<dyn ReconcileRunner>,
    slack: Arc<dyn SlackBot>,
    finance_channel: String,
    simulated: bool,
}

impl Context {
    pub fn from_env(
        surreal: SurrealDb,
        slack: Arc<dyn SlackBot>,
        simulated: bool,
        get: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, &'static str> {
        if XeroBillingProvider::from_env().is_none() {
            return Err("DBOS ReconcileInvoices requires XERO_* configuration");
        }
        if !crate::notify_config::bot_enabled(&get) {
            return Err("DBOS ReconcileInvoices requires SLACK_BOT_TOKEN");
        }
        let finance_channel = get("SLACK_FINANCE_CHANNEL_ID").unwrap_or_default();
        if finance_channel.trim().is_empty() {
            return Err("DBOS ReconcileInvoices requires SLACK_FINANCE_CHANNEL_ID");
        }
        Ok(Self {
            runner: Arc::new(LiveReconcileRunner(surreal)),
            slack,
            finance_channel,
            simulated,
        })
    }

    pub fn with_runner(
        runner: Arc<dyn ReconcileRunner>,
        slack: Arc<dyn SlackBot>,
        finance_channel: String,
        simulated: bool,
    ) -> Self {
        Self {
            runner,
            slack,
            finance_channel,
            simulated,
        }
    }
}

pub fn enabled(get: impl Fn(&str) -> Option<String>) -> Result<bool, &'static str> {
    match get(ENABLED_ENV).as_deref().map(str::trim) {
        None | Some("" | "false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err("NAVIGATOR_DBOS_RECONCILE_INVOICES_ENABLED must be true or false"),
    }
}

#[must_use]
pub fn workflow_id(slot: DateTime<Utc>) -> String {
    format!("{WORKFLOW}:{}", slot.format("%Y-%m-%d"))
}

#[must_use]
pub fn daily_slot(now: DateTime<Utc>) -> DateTime<Utc> {
    now.date_naive()
        .and_hms_opt(SLOT_HOUR_UTC, 0, 0)
        .expect("valid UTC hour")
        .and_utc()
}

pub fn register(dbos: &DBOS, context: Context) -> dbos::Result<ReconcileWorkflow> {
    dbos.register_workflow(WORKFLOW, move |()| {
        let context = context.clone();
        async move {
            let runner = Arc::clone(&context.runner);
            let report = dbos::step("reconcile", move || {
                let runner = Arc::clone(&runner);
                async move { runner.run().await.map_err(dbos::Error::Application) }
            })
            .await?;

            let correlation = format!(
                "reconcile-invoices-{}-finance",
                dbos::workflow_id().unwrap_or_default()
            );
            let report_for_notify = report.clone();
            let slack = Arc::clone(&context.slack);
            let channel = context.finance_channel.clone();
            let simulated = context.simulated;
            dbos::step_with(
                "notify_finance",
                StepOptions {
                    max_attempts: NOTIFY_ATTEMPTS,
                    ..StepOptions::default()
                },
                move || {
                    let report = report_for_notify.clone();
                    let slack = Arc::clone(&slack);
                    let channel = channel.clone();
                    let correlation = correlation.clone();
                    async move {
                        if slack
                            .message_receipt_by_correlation(&channel, &correlation)
                            .await
                            .map_err(|_| dbos::Error::Application(ReconcileDbosError::Notify))?
                            .is_none()
                        {
                            let message =
                                billing_workflows::reconcile::finance_message(&report, simulated);
                            slack
                                .post_message_with_receipt(&channel, &message, Some(&correlation))
                                .await
                                .map_err(|_| {
                                    dbos::Error::Application(ReconcileDbosError::Notify)
                                })?;
                        }
                        Ok(())
                    }
                },
            )
            .await?;
            dbos::Result::<_, ReconcileDbosError>::Ok(report)
        }
    })
}

pub async fn fire(
    workflow: &ReconcileWorkflow,
    slot: DateTime<Utc>,
) -> dbos::Result<dbos::WorkflowHandle<ReconcileReport, ReconcileDbosError>, ReconcileDbosError> {
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

pub async fn schedule(workflow: ReconcileWorkflow, stop: CancellationToken) {
    loop {
        let now = Utc::now();
        let slot = daily_slot(now);
        let wait = if now >= slot {
            let id = workflow_id(slot);
            if let Err(error) = fire(&workflow, slot).await {
                tracing::error!(workflow_id = %id, %error, "DBOS ReconcileInvoices schedule start failed");
                Duration::from_secs(60)
            } else {
                (slot + chrono::Duration::days(1) - Utc::now())
                    .to_std()
                    .unwrap_or(Duration::from_secs(1))
            }
        } else {
            (slot - now).to_std().unwrap_or(Duration::from_secs(1))
        };
        tokio::select! {
            () = sleep(wait) => {},
            () = stop.cancelled() => return,
        }
    }
}

/// A registered workflow on a DBOS instance that never launches, so a start
/// fails before reaching PostgreSQL. For route and schedule tests only.
#[cfg(test)]
pub(crate) fn unlaunched_workflow() -> ReconcileWorkflow {
    struct Unused;

    #[async_trait::async_trait]
    impl ReconcileRunner for Unused {
        async fn run(&self) -> Result<ReconcileReport, ReconcileDbosError> {
            Err(ReconcileDbosError::Reconcile)
        }
    }

    let dbos = DBOS::new(workflows::dbos_engine::config(
        "postgres://localhost/navigator?sslmode=disable",
        "test",
    ));
    register(
        &dbos,
        Context::with_runner(
            Arc::new(Unused),
            Arc::new(workflows::CapturingSlackBot::new()),
            "C_FINANCE".to_owned(),
            true,
        ),
    )
    .expect("register on an unlaunched executor")
}

#[cfg(test)]
mod tests {
    use super::{
        daily_slot, enabled, fire, schedule, unlaunched_workflow, workflow_id, Context,
        ReconcileDbosError, ENABLED_ENV,
    };
    use chrono::{TimeZone, Utc};
    use dbos::CancellationToken;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn a_misspelled_switch_is_refused_rather_than_left_off() {
        for value in ["TRUE", "yes", "1"] {
            assert_eq!(
                enabled(|_| Some(value.into())),
                Err("NAVIGATOR_DBOS_RECONCILE_INVOICES_ENABLED must be true or false")
            );
        }
        assert!(!enabled(|_| Some("  ".into())).unwrap());
    }

    #[test]
    fn every_instant_of_a_utc_day_shares_one_slot() {
        let expected = Utc.with_ymd_and_hms(2026, 10, 22, 11, 0, 0).unwrap();
        for hour in [0, 10, 11, 23] {
            let now = Utc.with_ymd_and_hms(2026, 10, 22, hour, 59, 59).unwrap();
            assert_eq!(daily_slot(now), expected);
            assert_eq!(workflow_id(daily_slot(now)), "ReconcileInvoices:2026-10-22");
        }
    }

    #[test]
    fn errors_name_the_failed_phase_only() {
        assert_eq!(
            ReconcileDbosError::Reconcile.to_string(),
            "Xero invoice reconciliation failed"
        );
        assert_eq!(
            ReconcileDbosError::Notify.to_string(),
            "finance notice delivery failed"
        );
    }

    #[tokio::test]
    async fn a_live_context_refuses_to_build_without_xero_configuration() {
        // The suite never configures XERO_*, so the first refusal is the
        // provider's, before any Slack setting is consulted.
        if billing::XeroBillingProvider::from_env().is_some() {
            return;
        }
        let surreal = store::test_support::mem_surreal().await;
        let refused = Context::from_env(
            surreal,
            Arc::new(workflows::CapturingSlackBot::new()),
            true,
            |_| Some("configured".into()),
        );
        assert_eq!(
            refused.err(),
            Some("DBOS ReconcileInvoices requires XERO_* configuration")
        );
    }

    #[tokio::test]
    async fn an_unlaunched_executor_refuses_to_fire_the_daily_slot() {
        let workflow = unlaunched_workflow();
        let slot = Utc.with_ymd_and_hms(2026, 10, 22, 11, 0, 0).unwrap();
        assert!(fire(&workflow, slot).await.is_err());
    }

    #[tokio::test]
    async fn the_schedule_returns_when_the_host_stops_it() {
        let stop = CancellationToken::new();
        stop.cancel();
        tokio::time::timeout(
            Duration::from_secs(10),
            schedule(unlaunched_workflow(), stop),
        )
        .await
        .expect("a cancelled schedule returns instead of sleeping to the next slot");
    }

    #[test]
    fn reconcile_uses_explicit_opt_in_and_namespaced_daily_ids() {
        assert!(!enabled(|_| None).unwrap());
        assert!(!enabled(|_| Some("false".into())).unwrap());
        assert!(enabled(|_| Some("true".into())).unwrap());
        assert_eq!(ENABLED_ENV, "NAVIGATOR_DBOS_RECONCILE_INVOICES_ENABLED");
        let morning = Utc.with_ymd_and_hms(2026, 10, 22, 11, 0, 0).unwrap();
        assert_eq!(workflow_id(morning), "ReconcileInvoices:2026-10-22");
        assert_eq!(
            daily_slot(Utc.with_ymd_and_hms(2026, 10, 22, 18, 0, 0).unwrap()),
            morning
        );
    }
}

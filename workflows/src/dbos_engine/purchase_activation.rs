//! DBOS schedule that turns paid Purchases on.
//!
//! Every five minutes this schedule refreshes pending payments and Stripe
//! renewals, least recently checked first. Each provider read and resulting
//! store write runs inside the existing durable step. A one-time Purchase
//! activates once its Stripe invoice is paid in full; a Stripe subscription
//! also requires a verified paid-through boundary. The queue survives pod loss. Journals carry Purchase identifiers
//! and reasons, never client content.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, DurationRound, TimeDelta, Utc};
use dbos::{CancellationToken, WorkflowHandle, WorkflowRef, DBOS};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const WORKFLOW: &str = "PurchaseActivation";
/// Independent opt-in: hosting DBOS alone never calls Stripe.
pub const ENABLED_ENV: &str = "NAVIGATOR_DBOS_PURCHASE_ACTIVATION_ENABLED";
/// How many invoiced Purchases one slot checks.
pub const BATCH: i64 = 25;
const SLOT_MINUTES: i64 = 5;
const RETRY_START: Duration = Duration::from_mins(1);

pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;
pub type PurchaseActivationWorkflow = WorkflowRef<(), ActivationReport, ActivationError>;

/// What one slot did. Identifiers and reasons only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivationReport {
    pub activated: Vec<Uuid>,
    pub unpaid: Vec<Uuid>,
    pub stopped: Vec<(Uuid, String)>,
}

#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
pub enum ActivationError {
    #[error("invoiced purchases could not be read")]
    Queue,
}

/// The work a slot performs, behind a trait so tests can drive it.
#[async_trait::async_trait]
pub trait ActivationSource: Send + Sync {
    async fn invoiced(&self) -> Result<Vec<Uuid>, ActivationError>;
    /// Whether the Purchase is active after this check.
    async fn activate(&self, purchase_id: Uuid) -> Result<bool, String>;
}

#[derive(Clone)]
pub struct Context {
    source: Arc<dyn ActivationSource>,
}

impl Context {
    #[must_use]
    pub fn new(source: Arc<dyn ActivationSource>) -> Self {
        Self { source }
    }
}

/// Whether this schedule is enabled. Only literal `true` opts in.
pub fn enabled(lookup: impl Fn(&str) -> Option<String>) -> Result<bool, super::SettingsError> {
    match lookup(ENABLED_ENV).as_deref().map(str::trim) {
        None | Some("" | "false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err(super::SettingsError::Invalid(ENABLED_ENV)),
    }
}

#[must_use]
pub fn system_clock() -> Clock {
    Arc::new(Utc::now)
}

/// The five-minute slot `now` falls in.
#[must_use]
pub fn slot(now: DateTime<Utc>) -> DateTime<Utc> {
    now.duration_trunc(TimeDelta::minutes(SLOT_MINUTES))
        .unwrap_or(now)
}

#[must_use]
pub fn workflow_id(slot: DateTime<Utc>) -> String {
    format!("{WORKFLOW}-{}", slot.format("%Y-%m-%dT%H:%M"))
}

impl ActivationReport {
    /// File one Purchase's outcome.
    pub fn record(&mut self, purchase_id: Uuid, outcome: Result<bool, String>) {
        match outcome {
            Ok(true) => self.activated.push(purchase_id),
            Ok(false) => self.unpaid.push(purchase_id),
            Err(reason) => {
                tracing::warn!(%purchase_id, %reason, "purchase activation stopped");
                self.stopped.push((purchase_id, reason));
            }
        }
    }
}

pub fn register(dbos: &DBOS, context: Context) -> dbos::Result<PurchaseActivationWorkflow> {
    dbos.register_workflow(WORKFLOW, move |()| {
        let context = context.clone();
        async move {
            let source = Arc::clone(&context.source);
            let queue = dbos::step("invoiced", move || {
                let source = Arc::clone(&source);
                async move { source.invoiced().await.map_err(dbos::Error::Application) }
            })
            .await?;
            let mut report = ActivationReport::default();
            for purchase_id in queue {
                let source = Arc::clone(&context.source);
                let outcome: Result<bool, String> =
                    dbos::step(&format!("purchase-{purchase_id}"), move || {
                        let source = Arc::clone(&source);
                        async move {
                            dbos::Result::<_, ActivationError>::Ok(
                                source.activate(purchase_id).await,
                            )
                        }
                    })
                    .await?;
                report.record(purchase_id, outcome);
            }
            dbos::Result::<_, ActivationError>::Ok(report)
        }
    })
}

pub async fn fire(
    workflow: &PurchaseActivationWorkflow,
    slot: DateTime<Utc>,
) -> dbos::Result<WorkflowHandle<ActivationReport, ActivationError>, ActivationError> {
    let id = workflow_id(slot);
    workflow
        .start_with(
            (),
            dbos::StartOptions {
                workflow_id: Some(&id),
                ..dbos::StartOptions::default()
            },
        )
        .await
}

pub async fn schedule(workflow: PurchaseActivationWorkflow, clock: Clock, stop: CancellationToken) {
    loop {
        let now = clock();
        let current = slot(now);
        let wait = match fire(&workflow, current).await {
            Ok(_) => (current + TimeDelta::minutes(SLOT_MINUTES) - now)
                .to_std()
                .unwrap_or(Duration::ZERO),
            Err(error) => {
                tracing::error!(workflow_id = workflow_id(current), %error, "purchase activation start failed");
                RETRY_START
            }
        };
        tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep(wait) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn a_slot_files_each_purchase_by_what_stripe_reported() {
        let (paid, unpaid, broken) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        let mut report = ActivationReport::default();
        report.record(paid, Ok(true));
        report.record(unpaid, Ok(false));
        report.record(broken, Err("stripe responded 503".to_string()));
        assert_eq!(report.activated, vec![paid]);
        assert_eq!(report.unpaid, vec![unpaid]);
        assert_eq!(
            report.stopped,
            vec![(broken, "stripe responded 503".to_string())]
        );
    }

    #[test]
    fn every_instant_in_five_minutes_shares_one_slot() {
        let start = Utc.with_ymd_and_hms(2026, 10, 20, 9, 35, 0).unwrap();
        let late = Utc.with_ymd_and_hms(2026, 10, 20, 9, 39, 59).unwrap();
        assert_eq!(slot(late), start);
        assert_eq!(workflow_id(start), "PurchaseActivation-2026-10-20T09:35");
    }

    #[test]
    fn schedule_requires_a_literal_true_flag() {
        for off in ["", "false", " false "] {
            assert!(!enabled(|_| Some(off.into())).unwrap(), "{off:?}");
        }
        assert!(!enabled(|_| None).unwrap());
        assert!(enabled(|_| Some(" true ".into())).unwrap());
        assert_eq!(
            enabled(|_| Some("yes".into())).unwrap_err(),
            super::super::SettingsError::Invalid(ENABLED_ENV)
        );
    }
}

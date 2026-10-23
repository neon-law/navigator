//! Weekly DBOS port of the contact-only BillingCanary.

use std::sync::Arc;
use std::time::Duration;

use billing::XeroBillingProvider;
use chrono::{DateTime, Datelike, Days, Utc};
use dbos::{CancellationToken, WorkflowHandle, WorkflowRef, DBOS};
use serde::{Deserialize, Serialize};

use super::SettingsError;
use crate::{EmailService, SlackOpsDelivery};

pub const WORKFLOW: &str = "BillingCanary";
pub const ENABLED_ENV: &str = "NAVIGATOR_DBOS_BILLING_CANARY_ENABLED";
const RETRY_START: Duration = Duration::from_mins(1);
const WEEK: Duration = Duration::from_hours(168);

pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;
pub use crate::billing_canary::CanaryReport;
pub type BillingCanaryWorkflow = WorkflowRef<(), CanaryReport, BillingCanaryError>;

#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
pub enum BillingCanaryError {
    #[error("billing provider contact lookup failed")]
    Provider,
    #[error("operations notification failed")]
    Notify,
}

#[derive(Clone)]
pub struct Context {
    pub provider: Arc<dyn crate::billing_canary::BillingCanaryProvider>,
    pub email: Arc<dyn EmailService>,
    pub recipient: String,
}

impl Context {
    /// Build the live Xero and Slack-backed adapters only when this workflow is enabled.
    pub fn from_lookup<F: Fn(&str) -> Option<String>>(lookup: F) -> Result<Self, SettingsError> {
        use crate::billing_canary::{notify_recipient, BillingProviderAdapter};
        use billing::BillingProvider;

        let provider = XeroBillingProvider::from_env().ok_or(SettingsError::Missing("XERO_*"))?;
        let provider: Arc<dyn BillingProvider> = Arc::new(provider);
        let provider: Arc<dyn crate::billing_canary::BillingCanaryProvider> =
            Arc::new(BillingProviderAdapter(provider));
        let webhook = lookup("SLACK_WEBHOOK_URL")
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .ok_or(SettingsError::Missing("SLACK_WEBHOOK_URL"))?;
        let notifier = crate::notify::notifier_from_lookup(|key| {
            if key == "SLACK_WEBHOOK_URL" {
                Some(webhook.clone())
            } else {
                lookup(key)
            }
        });
        Ok(Self {
            provider,
            email: Arc::new(SlackOpsDelivery::new(notifier)),
            recipient: notify_recipient(&lookup),
        })
    }

    pub fn new(
        provider: Arc<dyn crate::billing_canary::BillingCanaryProvider>,
        email: Arc<dyn EmailService>,
    ) -> Self {
        Self {
            provider,
            email,
            recipient: "nick@neonlaw.com".to_owned(),
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

/// This week's Sunday at 14:00 UTC, whether the slot has passed or is upcoming.
#[must_use]
pub fn slot(now: DateTime<Utc>) -> DateTime<Utc> {
    let date = now.date_naive();
    let days_since_sunday = date.weekday().num_days_from_sunday();
    let sunday = date
        .checked_sub_days(Days::new(u64::from(days_since_sunday)))
        .unwrap_or(date);
    sunday
        .and_hms_opt(14, 0, 0)
        .and_then(|time| time.and_local_timezone(Utc).single())
        .unwrap_or(now)
}

#[must_use]
pub fn workflow_id(slot: DateTime<Utc>) -> String {
    format!("{WORKFLOW}-{}", slot.format("%Y-%m-%dT%H-%MZ"))
}

pub fn register(dbos: &DBOS, context: Context) -> dbos::Result<BillingCanaryWorkflow> {
    dbos.register_workflow(WORKFLOW, move |()| {
        let context = context.clone();
        async move {
            let provider = Arc::clone(&context.provider);
            let report = dbos::step("xero", move || {
                let provider = Arc::clone(&provider);
                async move {
                    crate::billing_canary::run_canary(provider.as_ref())
                        .await
                        .map_err(|_| dbos::Error::Application(BillingCanaryError::Provider))
                }
            })
            .await?;
            let email = crate::billing_canary::build_confirmation(
                &crate::billing_canary::CanaryReport {
                    contact_id: report.contact_id.clone(),
                    idempotent: report.idempotent,
                },
                &context.recipient,
            );
            let service = Arc::clone(&context.email);
            dbos::step("email", move || {
                let (service, email) = (Arc::clone(&service), email.clone());
                async move {
                    service
                        .send(email)
                        .await
                        .map(|_| ())
                        .map_err(|_| dbos::Error::Application(BillingCanaryError::Notify))
                }
            })
            .await?;
            dbos::Result::<_, BillingCanaryError>::Ok(report)
        }
    })
}

pub async fn fire(
    workflow: &BillingCanaryWorkflow,
    slot: DateTime<Utc>,
) -> dbos::Result<WorkflowHandle<CanaryReport, BillingCanaryError>, BillingCanaryError> {
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

pub async fn schedule(workflow: BillingCanaryWorkflow, clock: Clock, stop: CancellationToken) {
    loop {
        let now = clock();
        let current = slot(now);
        let wait = if now >= current {
            match fire(&workflow, current).await {
                Ok(_) => (current + WEEK - now).to_std().unwrap_or(Duration::ZERO),
                Err(error) => {
                    tracing::error!(workflow_id = workflow_id(current), %error, "billing canary start failed");
                    RETRY_START
                }
            }
        } else {
            (current - now).to_std().unwrap_or(Duration::ZERO)
        };
        tokio::select! { () = stop.cancelled() => break, () = tokio::time::sleep(wait) => {} }
    }
}

#[cfg(test)]
mod tests {
    use super::{enabled, slot, workflow_id, ENABLED_ENV};
    use chrono::{TimeZone, Utc};
    #[test]
    fn weekly_slot_is_sunday_at_fourteen_utc_and_stable() {
        let now = Utc.with_ymd_and_hms(2026, 10, 21, 10, 0, 0).unwrap();
        let expected = Utc.with_ymd_and_hms(2026, 10, 18, 14, 0, 0).unwrap();
        assert_eq!(slot(now), expected);
        assert_eq!(workflow_id(expected), "BillingCanary-2026-10-18T14-00Z");
        let before = Utc.with_ymd_and_hms(2026, 10, 18, 13, 59, 0).unwrap();
        assert_eq!(slot(before), expected);
    }
    #[test]
    fn switch_defaults_off_and_rejects_invalid_values() {
        assert!(!enabled(|_| None).unwrap());
        assert!(!enabled(|_| Some("false".into())).unwrap());
        assert!(enabled(|_| Some("true".into())).unwrap());
        assert_eq!(
            enabled(|_| Some("yes".into())).unwrap_err(),
            super::SettingsError::Invalid(ENABLED_ENV)
        );
    }
}

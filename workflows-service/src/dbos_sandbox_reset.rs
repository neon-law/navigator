//! Monthly Sandbox credit and invoicing through DBOS.

use std::sync::Arc;
use std::time::Duration;

use billing_workflows::sandbox_reset::{self, OverageOutcome, SandboxResetReport};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dbos::{StepOptions, WorkflowRef, DBOS};
use serde::{Deserialize, Serialize};
use store::{postgres::PostgresDb, surreal::SurrealDb};

pub type SandboxResetWorkflow =
    WorkflowRef<SandboxResetStart, SandboxResetReport, SandboxResetError>;

// Xero accepts the same idempotency key for six minutes. Keep a margin for
// the bounded provider call and fail closed if a restart outlives the key.
const REPLAY_WINDOW: chrono::Duration = chrono::Duration::minutes(5);
const INVOICE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SandboxResetStart {
    pub period: String,
}

#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
pub enum SandboxResetError {
    #[error("invalid SandboxCreditReset billing period")]
    InvalidPeriod,
    #[error("the Sandbox credit grant or overage roster failed")]
    Store,
    #[error("the Sandbox overage invoice failed")]
    Invoice,
    #[error("the Sandbox overage invoice needs provider reconciliation before retry")]
    ReconciliationRequired,
}

fn period_start(period: &str) -> Option<NaiveDate> {
    let date = NaiveDate::parse_from_str(&format!("{period}-01"), "%Y-%m-%d").ok()?;
    (date.format("%Y-%m").to_string() == period).then_some(date)
}

fn within_replay_window(anchor: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    let elapsed = now.signed_duration_since(anchor);
    elapsed >= chrono::Duration::zero() && elapsed < REPLAY_WINDOW
}

pub fn register(
    dbos: &DBOS,
    postgres: PostgresDb,
    surreal: SurrealDb,
) -> dbos::Result<SandboxResetWorkflow> {
    dbos.register_workflow("SandboxCreditReset", move |request: SandboxResetStart| {
        let (postgres, surreal) = (postgres.clone(), surreal.clone());
        async move { run(postgres, surreal, request).await }
    })
}

async fn run(
    postgres: PostgresDb,
    surreal: SurrealDb,
    request: SandboxResetStart,
) -> dbos::Result<SandboxResetReport, SandboxResetError> {
    let month = period_start(&request.period)
        .and_then(|date| date.checked_sub_months(chrono::Months::new(1)))
        .ok_or(dbos::Error::Application(SandboxResetError::InvalidPeriod))?;
    let closed_period = store::sandbox_usage::billing_period(month.year(), month.month());
    let granted = dbos::step("grant", || async {
        sandbox_reset::grant_active_subscribers(&postgres, &surreal, &request.period)
            .await
            .map_err(|_| dbos::Error::Application(SandboxResetError::Store))
    })
    .await?;
    let overages = dbos::step("closed-overage-roster", || async {
        sandbox_reset::closed_overage_roster(&surreal, &closed_period)
            .await
            .map_err(|_| dbos::Error::Application(SandboxResetError::Store))
    })
    .await?;
    let mut report = SandboxResetReport {
        new_period: request.period,
        closed_period: closed_period.clone(),
        granted,
        ..SandboxResetReport::default()
    };
    for overage in overages {
        let anchor_step = format!("invoice-attempt-anchor-{}", overage.person_id);
        let anchor = dbos::step(&anchor_step, || async {
            Ok::<_, dbos::Error<SandboxResetError>>(Utc::now())
        })
        .await?;
        let invoice_step = format!("invoice-overage-{}", overage.person_id);
        let outcome = dbos::step_with(
            &invoice_step,
            StepOptions {
                max_attempts: 3,
                should_retry: Some(Arc::new(|error| {
                    matches!(error, dbos::Error::Application(SandboxResetError::Invoice))
                })),
                ..StepOptions::default()
            },
            || {
                let (postgres, surreal, period, overage) = (
                    postgres.clone(),
                    surreal.clone(),
                    closed_period.clone(),
                    overage.clone(),
                );
                async move {
                    if !within_replay_window(anchor, Utc::now()) {
                        return Err(dbos::Error::Application(
                            SandboxResetError::ReconciliationRequired,
                        ));
                    }
                    tokio::time::timeout(
                        INVOICE_TIMEOUT,
                        sandbox_reset::invoice_one_from_env(&postgres, &surreal, &period, overage),
                    )
                    .await
                    .map_err(|_| dbos::Error::Application(SandboxResetError::Invoice))?
                    .map_err(|_| dbos::Error::Application(SandboxResetError::Invoice))
                }
            },
        )
        .await?;
        match outcome {
            OverageOutcome::Invoiced(_) => report.overage_invoiced += 1,
            OverageOutcome::Skipped => report.overage_skipped += 1,
            OverageOutcome::AlreadyBilled => report.overage_already_billed += 1,
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::{period_start, within_replay_window};
    use chrono::{Duration, Utc};

    #[test]
    fn invoice_replay_requires_a_live_provider_key_window() {
        let anchor = Utc::now();
        assert!(within_replay_window(
            anchor,
            anchor + Duration::seconds(299)
        ));
        assert!(!within_replay_window(anchor, anchor + Duration::minutes(5)));
        assert!(!within_replay_window(anchor, anchor - Duration::seconds(1)));
    }

    #[test]
    fn only_canonical_months_are_accepted() {
        assert!(period_start("2026-11").is_some());
        assert!(period_start("2026-1").is_none());
        assert!(period_start("2026-13").is_none());
    }
}

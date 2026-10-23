//! The `BillingCanary` Restate workflow — a live, multi-step health check
//! for the billing integration.
//!
//! Two durable steps, each journaled independently (which is exactly why
//! this is a Restate workflow and not a one-shot batch — a retry of the
//! Xero step must not re-send the confirmation email, and vice versa):
//!
//! 1. `ctx.run("xero")` — find-or-create a single **stable** canary
//!    contact in the connected Xero org and confirm the resolve is
//!    idempotent. Because [`billing`]'s find-or-create keys on email, a
//!    fixed canary email means exactly one canary contact ever exists
//!    (created on the first run, found on every run after) — a heartbeat,
//!    not nightly junk. This proves token minting, tenant routing, the
//!    contact lookup `where` predicate, and the create payload all still
//!    agree with Xero's API.
//! 2. `ctx.run("email")` — send a confirmation email with the result.
//!
//! Boundary (per the legal-council review): the canary touches **Contacts
//! only** — never an invoice, payment, or trust transaction — so it never
//! writes a financial record into the firm's books of account.
//!
//! The Xero step's core ([`run_canary`]) and the email body
//! ([`build_confirmation`]) are pure/provider-agnostic so they unit-test
//! against the [`billing::StubBillingProvider`] without a worker.

use std::sync::Arc;

use billing::{BillingProvider, XeroBillingProvider};
use restate_sdk::prelude::*;
use serde::{Deserialize, Serialize};
pub use workflows::billing_canary::CanaryReport;
use workflows::billing_canary::{BillingProviderAdapter, CanaryProviderError};
use workflows::{EmailService, OutboundEmail};

/// Request body for `BillingCanary::run`. Empty — the trigger only starts
/// the workflow — but kept as a struct so fields can be threaded later
/// without changing the handler signature.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct RunRequest {}

/// Service registered with the Restate endpoint. Holds the worker-side
/// [`EmailService`] (for the confirmation step); the Xero provider is
/// built from env inside the Xero step so no token or HTTP client is held
/// idle between runs. Same shape as `archives`'s `ArchivesService`.
#[derive(Clone)]
pub struct BillingCanaryService {
    email: Arc<dyn EmailService>,
}

impl BillingCanaryService {
    #[must_use]
    pub fn new(email: Arc<dyn EmailService>) -> Self {
        Self { email }
    }
}

#[restate_sdk::workflow(name = "BillingCanary")]
impl BillingCanaryService {
    #[restate_sdk::handler]
    async fn run(
        &self,
        ctx: WorkflowContext<'_>,
        _req: Json<RunRequest>,
    ) -> Result<Json<CanaryReport>, HandlerError> {
        // Step 1 — Xero: build the real provider and run the canary. A
        // missing Xero config is terminal (no point retrying); a
        // provider/API error is retryable, so Restate replays just this
        // step without re-sending the email below.
        let report: CanaryReport = ctx
            .run(|| async {
                let provider = XeroBillingProvider::from_env().ok_or_else(|| {
                    TerminalError::new("Xero is not configured (XERO_* env unset)")
                })?;
                let adapter = BillingProviderAdapter(Arc::new(provider));
                let report = workflows::billing_canary::run_canary(&adapter)
                    .await
                    .map_err(|_| {
                        HandlerError::from(billing::BillingError::Provider(
                            "canary contact resolve failed".into(),
                        ))
                    })?;
                Ok(Json(report))
            })
            .name("xero")
            .await?
            .into_inner();

        // Step 2 — email confirmation, journaled separately so a Xero-step
        // retry never re-sends and an email-send retry never re-resolves.
        let email = build_confirmation(&report, &notify_recipient(|k| std::env::var(k).ok()));
        let svc = Arc::clone(&self.email);
        ctx.run(move || async move {
            svc.send(email)
                .await
                .map(|_| ())
                .map_err(HandlerError::from)
        })
        .name("email")
        .await?;

        Ok(Json(report))
    }
}

/// Resolve the stable canary contact twice and confirm the same id comes
/// back. Provider-agnostic so it unit-tests against the stub.
pub async fn run_canary(
    provider: &dyn BillingProvider,
) -> Result<CanaryReport, billing::BillingError> {
    struct Borrowed<'a>(&'a dyn BillingProvider);
    #[async_trait::async_trait]
    impl workflows::billing_canary::BillingCanaryProvider for Borrowed<'_> {
        async fn ensure_contact(
            &self,
            request: &billing::ContactRequest,
        ) -> Result<billing::ContactId, CanaryProviderError> {
            self.0
                .ensure_contact(request)
                .await
                .map_err(|_| CanaryProviderError)
        }
    }
    workflows::billing_canary::run_canary(&Borrowed(provider))
        .await
        .map_err(|_| billing::BillingError::Provider("canary contact resolve failed".into()))
}

/// The confirmation-email recipient: `BILLING_CANARY_NOTIFY_EMAIL`, else
/// the default. Takes a `key -> value` lookup so it is unit-testable
/// without mutating process env.
fn notify_recipient<F: Fn(&str) -> Option<String>>(get: F) -> String {
    workflows::billing_canary::notify_recipient(get)
}

/// Build the plain-text confirmation email for a completed canary run.
/// Pure — exposed for unit-testing the rendered subject/body.
#[must_use]
pub fn build_confirmation(report: &CanaryReport, recipient: &str) -> OutboundEmail {
    workflows::billing_canary::build_confirmation(report, recipient)
}

#[cfg(test)]
mod tests {
    use super::{build_confirmation, notify_recipient, run_canary, CanaryReport};
    use billing::StubBillingProvider;

    #[tokio::test]
    async fn canary_resolves_idempotently_against_the_stub() {
        let stub = StubBillingProvider::new();
        let report = run_canary(&stub).await.expect("stub never errors");
        assert!(report.idempotent, "two resolves return the same id");
        assert!(
            report.contact_id.starts_with("stub-contact-"),
            "real provider id shape: {}",
            report.contact_id
        );
        // The canary resolved the contact exactly twice.
        assert_eq!(stub.contact_calls().len(), 2);
    }

    #[test]
    fn confirmation_email_carries_status_and_contact_id() {
        let report = CanaryReport {
            contact_id: "c-123".into(),
            idempotent: true,
        };
        let email = build_confirmation(&report, "ops@example.com");
        assert_eq!(email.to, "ops@example.com");
        assert!(email.subject.contains("OK"), "subject: {}", email.subject);
        assert!(email.subject.contains("c-123"));
        assert!(email.body.contains("Idempotent"));
    }

    #[test]
    fn notify_recipient_defaults_then_honors_env() {
        assert_eq!(notify_recipient(|_| None), "nick@neonlaw.com");
        assert_eq!(
            notify_recipient(|_| Some(String::new())),
            "nick@neonlaw.com"
        );
        assert_eq!(
            notify_recipient(
                |k| (k == "BILLING_CANARY_NOTIFY_EMAIL").then(|| "ops@example.com".to_string())
            ),
            "ops@example.com"
        );
    }
}

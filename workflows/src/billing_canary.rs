//! Shared provider-independent behavior for the internal billing canary.

use std::sync::Arc;

use billing::{BillingProvider, ContactId, ContactRequest};
use serde::{Deserialize, Serialize};

const CANARY_NAME: &str = "Neon Law Navigator Billing Canary [system health check]";
const CANARY_EMAIL: &str = "billing-canary@neonlaw.com";
const DEFAULT_NOTIFY_EMAIL: &str = "nick@neonlaw.com";

/// The provider did not resolve the stable system contact.
#[derive(Debug, thiserror::Error)]
#[error("billing contact resolution failed")]
pub struct CanaryProviderError;

/// The narrow provider behavior this workflow needs.
#[async_trait::async_trait]
pub trait BillingCanaryProvider: Send + Sync {
    async fn ensure_contact(
        &self,
        request: &ContactRequest,
    ) -> Result<ContactId, CanaryProviderError>;
}

/// Adapt the shared billing provider to the canary's contact-only contract.
pub struct BillingProviderAdapter(pub Arc<dyn BillingProvider>);

#[async_trait::async_trait]
impl BillingCanaryProvider for BillingProviderAdapter {
    async fn ensure_contact(
        &self,
        request: &ContactRequest,
    ) -> Result<ContactId, CanaryProviderError> {
        self.0
            .ensure_contact(request)
            .await
            .map_err(|_| CanaryProviderError)
    }
}

/// Result of the Xero canary step.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct CanaryReport {
    /// The Xero contact id resolved by the canary.
    pub contact_id: String,
    /// Whether repeated stable-email resolves returned the same id.
    pub idempotent: bool,
}

/// Resolve the stable system contact twice and confirm the provider returns
/// the same contact id. The stable email is the provider-side duplicate guard.
pub async fn run_canary(
    provider: &dyn BillingCanaryProvider,
) -> Result<CanaryReport, CanaryProviderError> {
    let request = ContactRequest {
        name: CANARY_NAME.to_owned(),
        email: CANARY_EMAIL.to_owned(),
    };
    let first = provider.ensure_contact(&request).await?;
    let second = provider.ensure_contact(&request).await?;
    Ok(CanaryReport {
        idempotent: first == second,
        contact_id: first.0,
    })
}

/// Resolve the confirmation recipient from deployment configuration.
#[must_use]
pub fn notify_recipient<F: Fn(&str) -> Option<String>>(get: F) -> String {
    get("BILLING_CANARY_NOTIFY_EMAIL")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_NOTIFY_EMAIL.to_owned())
}

/// Build the operations confirmation. SlackOpsDelivery treats this as a
/// single internal notice; the provider has no delivery idempotency key.
#[must_use]
pub fn build_confirmation(report: &CanaryReport, recipient: &str) -> crate::OutboundEmail {
    let status = if report.idempotent { "OK" } else { "FAILED" };
    let subject = format!(
        "Billing canary {status} — Xero contact {}",
        report.contact_id
    );
    let body = format!(
        "The weekly billing canary ran against Xero.\n\n\
         Resolved contact: {}\n\n\
         Idempotent (same id on re-resolve): {}\n\n\
         This is an automated health check — it find-or-creates one stable \
         contact and never touches invoices or payments.\n",
        report.contact_id, report.idempotent,
    );
    let html = crate::email::render_email_html(&body, &crate::email::base_url_from_env());
    crate::OutboundEmail::new(recipient.to_owned(), subject, body).with_html(html)
}

#[cfg(test)]
mod tests {
    use super::{build_confirmation, notify_recipient, BillingCanaryProvider, CanaryProviderError};
    use billing::{ContactId, ContactRequest};
    use std::sync::Mutex;

    #[derive(Default)]
    struct StableProvider(Mutex<Option<ContactId>>);

    #[async_trait::async_trait]
    impl BillingCanaryProvider for StableProvider {
        async fn ensure_contact(
            &self,
            _request: &ContactRequest,
        ) -> Result<ContactId, CanaryProviderError> {
            let mut contact = self.0.lock().expect("contact lock");
            Ok(contact
                .get_or_insert_with(|| ContactId("synthetic-contact".to_owned()))
                .clone())
        }
    }

    #[tokio::test]
    async fn shared_canary_core_resolves_one_stable_contact() {
        let report = super::run_canary(&StableProvider::default())
            .await
            .expect("synthetic provider succeeds");
        assert!(report.idempotent);
        assert_eq!(report.contact_id, "synthetic-contact");
    }

    #[test]
    fn confirmation_retains_status_contact_id_and_recipient() {
        let report = super::CanaryReport {
            contact_id: "synthetic-contact".into(),
            idempotent: true,
        };
        let email = build_confirmation(&report, "ops@example.com");
        assert_eq!(email.to, "ops@example.com");
        assert!(email.subject.contains("Billing canary OK"));
        assert!(email.body.contains("synthetic-contact"));
        assert!(email
            .body
            .contains("Idempotent (same id on re-resolve): true"));
    }

    #[test]
    fn recipient_is_environment_driven_with_a_default() {
        assert_eq!(notify_recipient(|_| None), "nick@neonlaw.com");
        assert_eq!(
            notify_recipient(
                |key| (key == "BILLING_CANARY_NOTIFY_EMAIL").then(|| "ops@example.com".into())
            ),
            "ops@example.com"
        );
    }
}

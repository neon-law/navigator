//! [`LoggingEmail`]: the decorator that journals every outbound message to
//! the `sent_email` audit trail (`store::sent_emails`).
//!
//! It lives here, beside [`EmailService`], so both senders wrap their backend
//! in the same decorator: the `web` process (`portal::email::from_env`) and
//! the `workflows-service` worker, whose workflow-driven template sends wrote
//! no audit row before. PostgreSQL holds the trail whenever it is configured.

use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use super::{EmailError, EmailService, OutboundEmail, SendReceipt};

/// Decorator that journals every outbound message to the
/// `sent_emails` audit table after the inner backend resolves.
/// `outcome = "sent"` on success, `outcome = "failed:<reason>"` on
/// error. The DB insert is best-effort — if it fails the send result
/// is still returned and the audit miss is logged via `tracing::warn`.
///
/// One row per attempt: when the inner backend is `RetryingEmail`,
/// only the final outcome is journaled (the retry decorator absorbs
/// transient failures internally). When the inner backend is `Send`
/// directly, every attempt produces a row.
#[derive(Clone)]
pub struct LoggingEmail {
    inner: Arc<dyn EmailService>,
    db: store::surreal::SurrealDb,
    /// Default `From:` address recorded on each audit row when the
    /// caller didn't supply `OutboundEmail.from`.
    sender: String,
}

impl LoggingEmail {
    #[must_use]
    pub fn new(
        inner: Arc<dyn EmailService>,
        db: store::surreal::SurrealDb,
        sender: impl Into<String>,
    ) -> Self {
        Self {
            inner,
            db,
            sender: sender.into(),
        }
    }

    async fn log(&self, email: &OutboundEmail, outcome: &str, sg_message_id: Option<String>) {
        let from = email.from.clone().unwrap_or_else(|| self.sender.clone());
        let row = store::sent_emails::NewSentEmail {
            recipient: email.to.clone(),
            subject: email.subject.clone(),
            sender: from,
            // `OutboundEmail` carries no matter context today (ENG-310):
            // every send that reaches this decorator is either firm-wide
            // (a welcome email, a heartbeat notice) or, for a workflow-driven
            // template send, keyed to a person who may participate in more
            // than one matter — an ambiguity a project id must not paper
            // over. Threading a real matter through is future work for the
            // one call site that would actually know it.
            project_id: None,
            person_id: person_uuid(email.person_id.as_deref()),
            template_slug: email.template_slug.clone(),
            body: email.body.clone(),
            outcome: outcome.to_string(),
            // SendGrid's `X-Message-Id`, the join key to the
            // delivery-side Event Webhook stream. `None` until the
            // message clears SendGrid (failed sends, and the
            // capturing backend, never get one).
            sg_message_id,
            sent_at: chrono::Utc::now(),
        };
        if let Err(e) = store::sent_emails::record(&self.db, &row).await {
            // The recipient address stays out of the log field — it is
            // client-identifying and telemetry leaves the firm's trust
            // boundary (`telemetry/src/lib.rs`). `person_id` is carried on the
            // message when the caller set it; the row that failed to insert
            // holds the address itself, and `template_slug` says which kind of
            // message it was.
            tracing::warn!(
                error = %e,
                person_id = email.person_id.as_deref().unwrap_or("<unset>"),
                template_slug = email.template_slug.as_deref().unwrap_or("<none>"),
                "sent_emails audit insert failed",
            );
        }
    }
}

/// `OutboundEmail::person_id` is a string so SendGrid can echo it back in
/// `custom_args`; the audit row keeps it only when it names a Person UUID,
/// bare or as a `person:<uuid>` record key.
fn person_uuid(person_id: Option<&str>) -> Option<Uuid> {
    let raw = person_id?.trim();
    Uuid::parse_str(raw.strip_prefix("person:").unwrap_or(raw)).ok()
}

#[async_trait]
impl EmailService for LoggingEmail {
    async fn send(&self, email: OutboundEmail) -> Result<SendReceipt, EmailError> {
        let result = self.inner.send(email.clone()).await;
        let (outcome, message_id) = match &result {
            Ok(receipt) => ("sent".to_string(), receipt.message_id.clone()),
            Err(e) => (format!("failed:{e}"), None),
        };
        self.log(&email, &outcome, message_id).await;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::{person_uuid, LoggingEmail};
    use crate::email::{CapturingEmail, EmailError, EmailService, OutboundEmail};
    use std::sync::Arc;

    async fn logging() -> (LoggingEmail, store::surreal::SurrealDb) {
        let db = store::surreal::test_support::mem().await;
        let inner: Arc<dyn EmailService> = Arc::new(CapturingEmail::new());
        (
            LoggingEmail::new(inner, db.clone(), "support@example.com"),
            db,
        )
    }

    #[tokio::test]
    async fn logging_email_writes_audit_row_on_success() {
        let (svc, db) = logging().await;
        let person = uuid::Uuid::now_v7();
        svc.send(
            OutboundEmail::new("aries@example.com", "Welcome to the firm", "Body")
                .with_template("welcome")
                .with_person(person.to_string()),
        )
        .await
        .expect("send succeeds");
        let rows = store::sent_emails::all(&db).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].recipient, "aries@example.com");
        assert_eq!(rows[0].subject, "Welcome to the firm");
        assert_eq!(rows[0].sender, "support@example.com");
        assert_eq!(rows[0].template_slug.as_deref(), Some("welcome"));
        assert_eq!(rows[0].outcome, "sent");
        assert!(rows[0].body.contains("Body"));
    }

    #[tokio::test]
    async fn logging_email_writes_failure_row_when_inner_rejects_recipient() {
        let (svc, db) = logging().await;
        let err = svc
            .send(OutboundEmail::new("not-an-email", "x", "x"))
            .await
            .unwrap_err();
        assert!(matches!(err, EmailError::InvalidRecipient(_)));
        let rows = store::sent_emails::all(&db).await.unwrap();
        assert_eq!(rows.len(), 1, "failure path must still audit");
        assert!(
            rows[0].outcome.starts_with("failed:"),
            "outcome should mark failure, got {}",
            rows[0].outcome
        );
    }

    #[tokio::test]
    async fn logging_email_uses_per_message_from_when_provided() {
        let (svc, db) = logging().await;
        let mut msg = OutboundEmail::new("aries@example.com", "s", "b");
        msg.from = Some("ops@example.com".into());
        svc.send(msg).await.unwrap();
        let rows = store::sent_emails::all(&db).await.unwrap();
        assert_eq!(rows[0].sender, "ops@example.com");
    }

    #[test]
    fn a_person_id_is_kept_only_when_it_names_a_uuid() {
        let id = uuid::Uuid::now_v7();
        assert_eq!(person_uuid(Some(&id.to_string())), Some(id));
        assert_eq!(person_uuid(Some(&format!("person:{id}"))), Some(id));
        assert_eq!(person_uuid(Some("not-a-person")), None);
        assert_eq!(person_uuid(None), None);
    }
}

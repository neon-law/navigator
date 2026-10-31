//! Save public support intake before sending one branded acknowledgment.
//! The shared Firm inbox owns storage; the email service owns transport.

use crate::inbound_email::InboundEmail;
use crate::inbox::{extract_addr, ingest_support_email};

const MAILBOXES: [&str; 2] = ["support@neonlaw.com", "staging@neonlaw.com"];

const ACKNOWLEDGMENT: &str = "We received your message and will respond soon.";

/// Prefer the actual receiving mailbox over display headers. Forwarded parsed
/// mail can retain its public recipient only in the original To header.
pub(crate) fn mailbox(email: &InboundEmail) -> Option<&'static str> {
    email
        .envelope
        .as_ref()
        .and_then(|envelope| {
            MAILBOXES.into_iter().find(|mailbox| {
                envelope
                    .to
                    .iter()
                    .any(|to| to.trim().eq_ignore_ascii_case(mailbox))
            })
        })
        .or_else(|| {
            MAILBOXES
                .into_iter()
                .find(|mailbox| extract_addr(&email.to).eq_ignore_ascii_case(mailbox))
        })
}

fn receipt_allowed(email: &InboundEmail) -> bool {
    let from = extract_addr(&email.from).to_lowercase();
    if !from.contains('@')
        || from.contains(['\r', '\n'])
        || MAILBOXES.contains(&from.as_str())
        || email.envelope.as_ref().is_some_and(|envelope| {
            envelope
                .from
                .as_ref()
                .is_some_and(|from| from.trim().is_empty() || from.trim() == "<>")
        })
    {
        return false;
    }
    let headers = if email.raw.is_empty() {
        email.headers.as_bytes()
    } else {
        &email.raw
    };
    if let Some(message) = mail_parser::MessageParser::default().parse(headers) {
        let headers = message.headers();
        let header = |name| headers.value(name).and_then(|value| value.as_text());
        if header("Auto-Submitted").is_some_and(|value| !value.trim().eq_ignore_ascii_case("no"))
            || header("Precedence").is_some_and(|value| {
                ["bulk", "list", "junk"].contains(&value.trim().to_lowercase().as_str())
            })
            || headers.contains("List-Id")
            || headers.value("Return-Path").is_some_and(|value| {
                matches!(value, mail_parser::HeaderValue::Empty)
                    || value.as_text().is_some_and(|value| value.trim() == "<>")
            })
        {
            return false;
        }
    }
    true
}

/// Save support intake before acknowledging it, including the summary lane.
/// Reservation is at most once: uncertain sends remain visible in the inbox.
pub(crate) async fn receive(
    db: &store::surreal::SurrealDb,
    storage: &std::sync::Arc<dyn cloud::StorageService>,
    service: &dyn crate::email::EmailService,
    email: &InboundEmail,
) -> Result<(), String> {
    let Some(inbound) = ingest_support_email(db, storage, email).await? else {
        return Ok(());
    };
    crate::inbox::start_inbound_message_alert(inbound.message_id).await?;
    let Some(mailbox) = mailbox(email).filter(|_| receipt_allowed(email)) else {
        return Ok(());
    };
    acknowledge(db, service, email, mailbox, &inbound).await
}

async fn acknowledge(
    db: &store::surreal::SurrealDb,
    service: &dyn crate::email::EmailService,
    email: &InboundEmail,
    mailbox: &str,
    inbound: &store::inbox::IngestedInbound,
) -> Result<(), String> {
    let Some(id) = store::inbox::reserve_acknowledgment(db, inbound, ACKNOWLEDGMENT)
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(());
    };
    let html = views::brand::scope(&views::brand::DEFAULT_BRANDING, async {
        workflows::email::render_email_html(ACKNOWLEDGMENT, &workflows::email::base_url_from_env())
    })
    .await;
    let mut outbound = crate::email::OutboundEmail::new(
        extract_addr(&email.from),
        "We received your message",
        ACKNOWLEDGMENT,
    )
    .with_from(mailbox)
    .with_reply_to(mailbox)
    .with_template("message-received")
    .with_html(html);
    outbound.auto_submitted = true;
    if let Some(message_id) = email.message_id.as_deref() {
        outbound = outbound.with_thread_refs(&[message_id.to_string()]);
    }
    if let Ok(receipt) = service.send(outbound).await {
        store::inbox::acknowledgment_submitted(db, id, receipt.message_id)
            .await
            .map_err(|error| error.to_string())
    } else {
        // The provider may have accepted a timed-out request. Preserve the
        // reservation and let intake succeed without another external send.
        tracing::warn!(message_id = %id, "support acknowledgment outcome unknown");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_email() -> InboundEmail {
        InboundEmail {
            from: "Sender <sender@example.com>".into(),
            to: "support@example.com".into(),
            subject: "Question".into(),
            text: "Hello".into(),
            ..InboundEmail::default()
        }
    }

    #[test]
    fn receipt_targets_only_the_two_public_mailboxes() {
        let mut email = sample_email();
        assert!(mailbox(&email).is_none());
        email.to = "Neon Law <STAGING@neonlaw.com>".into();
        assert_eq!(mailbox(&email), Some("staging@neonlaw.com"));
        email.to = "other@example.com".into();
        email.envelope = Some(crate::inbound_email::SmtpEnvelope {
            to: vec!["support@neonlaw.com".into()],
            from: None,
        });
        assert_eq!(mailbox(&email), Some("support@neonlaw.com"));
    }

    #[test]
    fn smtp_mailbox_takes_precedence_over_the_display_recipient() {
        let mut email = sample_email();
        email.to = "support@neonlaw.com".into();
        email.envelope = Some(crate::inbound_email::SmtpEnvelope {
            to: vec!["staging@neonlaw.com".into()],
            from: None,
        });
        assert_eq!(mailbox(&email), Some("staging@neonlaw.com"));
    }

    #[test]
    fn automatic_and_bulk_mail_never_receive_a_receipt() {
        let mut email = sample_email();
        assert!(receipt_allowed(&email));
        for header in [
            "Auto-Submitted: auto-replied",
            "Auto-Submitted: auto-generated",
            "Precedence: bulk",
            "List-Id: <list.example.com>",
            "Return-Path: <>",
        ] {
            email.raw = format!("From: sender@example.com\r\n{header}\r\n\r\nHello").into_bytes();
            assert!(!receipt_allowed(&email), "{header}");
        }
        email.raw =
            b"Auto-Submitted: no\r\nReturn-Path: <sender@example.com>\r\n\r\nHello".to_vec();
        assert!(receipt_allowed(&email));
        email.raw.clear();
        email.headers = "Auto-Submitted: auto-replied\r\n\r\n".into();
        assert!(!receipt_allowed(&email));
        email.headers.clear();
        email.from = "support@neonlaw.com".into();
        assert!(!receipt_allowed(&email));
        email.from = "sender@example.com".into();
        email.envelope = Some(crate::inbound_email::SmtpEnvelope {
            to: vec![],
            from: Some(String::new()),
        });
        assert!(!receipt_allowed(&email));
    }

    #[tokio::test]
    async fn support_email_is_saved_before_one_branded_receipt_across_retries() {
        let db = store::test_support::mem_surreal().await;
        store::test_support::seed_firm_with_brand(&db, "neon").await;
        let service = crate::email::CapturingEmail::new();
        let storage = crate::test_support::fs_storage().await;
        let mut email = sample_email();
        email.to = "staging@neonlaw.com".into();
        email.message_id = Some("fixture-message@example.com".into());
        receive(&db, &storage, &service, &email).await.unwrap();
        receive(&db, &storage, &service, &email).await.unwrap();
        let inbound = ingest_support_email(&db, &storage, &email)
            .await
            .unwrap()
            .unwrap();
        let saved = store::inbox::message_by_id(&db, inbound.message_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.body, "Hello");
        assert!(inbound.deduped);
        let sent = service.captured();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].to, "sender@example.com");
        assert_eq!(sent[0].from.as_deref(), Some("staging@neonlaw.com"));
        assert_eq!(sent[0].body, ACKNOWLEDGMENT);
        assert!(sent[0].auto_submitted);
        assert!(sent[0].html_body.as_ref().unwrap().contains("<img"));
        assert_eq!(
            sent[0].in_reply_to.as_deref(),
            Some("<fixture-message@example.com>")
        );
        let receipt = store::inbox::acknowledgment_for(&db, inbound.message_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(receipt.delivery_state, store::inbox::delivery::SUBMITTED);
    }

    #[tokio::test]
    async fn suppressed_automatic_mail_is_still_saved() {
        let db = store::test_support::mem_surreal().await;
        store::test_support::seed_firm_with_brand(&db, "neon").await;
        let service = crate::email::CapturingEmail::new();
        let storage = crate::test_support::fs_storage().await;
        let mut email = sample_email();
        email.to = "support@neonlaw.com".into();
        email.raw = b"Auto-Submitted: auto-replied\r\n\r\nHello".to_vec();
        receive(&db, &storage, &service, &email).await.unwrap();
        assert!(service.captured().is_empty());
        assert!(
            ingest_support_email(&db, &storage, &email)
                .await
                .unwrap()
                .unwrap()
                .deduped
        );
    }

    #[tokio::test]
    async fn public_support_mail_from_a_lawyer_is_saved_and_acknowledged() {
        let db = store::test_support::mem_surreal().await;
        store::test_support::seed_firm_with_brand(&db, "neon").await;
        store::persons::create(
            &db,
            &store::persons::NewPerson::with_role(
                "Fixture Lawyer",
                "sender@example.com",
                store::persons::Role::Lawyer,
            ),
        )
        .await
        .unwrap();
        let service = crate::email::CapturingEmail::new();
        let storage = crate::test_support::fs_storage().await;
        let mut email = sample_email();
        email.to = "support@neonlaw.com".into();
        receive(&db, &storage, &service, &email).await.unwrap();
        assert_eq!(service.captured().len(), 1);
        assert!(
            ingest_support_email(&db, &storage, &email)
                .await
                .unwrap()
                .unwrap()
                .deduped
        );
    }

    #[tokio::test]
    async fn concurrent_receipts_reserve_one_send() {
        let db = store::test_support::mem_surreal().await;
        store::test_support::seed_firm_with_brand(&db, "neon").await;
        let service = crate::email::CapturingEmail::new();
        let storage = crate::test_support::fs_storage().await;
        let mut email = sample_email();
        email.to = "support@neonlaw.com".into();
        let inbound = ingest_support_email(&db, &storage, &email)
            .await
            .unwrap()
            .unwrap();
        let (first, second) = tokio::join!(
            acknowledge(&db, &service, &email, "support@neonlaw.com", &inbound),
            acknowledge(&db, &service, &email, "support@neonlaw.com", &inbound),
        );
        first.unwrap();
        second.unwrap();
        assert_eq!(service.captured().len(), 1);
    }

    #[tokio::test]
    async fn messages_without_ids_dedupe_by_content_instead_of_subject() {
        let db = store::test_support::mem_surreal().await;
        store::test_support::seed_firm_with_brand(&db, "neon").await;
        let service = crate::email::CapturingEmail::new();
        let storage = crate::test_support::fs_storage().await;
        let mut email = sample_email();
        email.to = "support@neonlaw.com".into();
        receive(&db, &storage, &service, &email).await.unwrap();
        receive(&db, &storage, &service, &email).await.unwrap();
        email.text = "A different message".into();
        receive(&db, &storage, &service, &email).await.unwrap();
        assert_eq!(service.captured().len(), 2);
    }

    #[tokio::test]
    async fn messages_without_ids_keep_distinct_attachments() {
        let db = store::test_support::mem_surreal().await;
        store::test_support::seed_firm_with_brand(&db, "neon").await;
        let service = crate::email::CapturingEmail::new();
        let storage = crate::test_support::fs_storage().await;
        let mut email = sample_email();
        email.to = "support@neonlaw.com".into();
        email
            .attachments
            .push(crate::inbound_email::InboundAttachment {
                filename: "fixture.txt".into(),
                content_type: "text/plain".into(),
                bytes: b"First".to_vec(),
            });
        receive(&db, &storage, &service, &email).await.unwrap();
        email.attachments[0].bytes = b"Second".to_vec();
        receive(&db, &storage, &service, &email).await.unwrap();
        assert_eq!(service.captured().len(), 2);
    }

    struct UncertainEmail(std::sync::atomic::AtomicUsize);

    #[async_trait::async_trait]
    impl crate::email::EmailService for UncertainEmail {
        async fn send(
            &self,
            _: crate::email::OutboundEmail,
        ) -> Result<crate::email::SendReceipt, crate::email::EmailError> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(crate::email::EmailError::Transport("timeout".into()))
        }
    }

    #[tokio::test]
    async fn uncertain_send_is_saved_and_never_resent() {
        let db = store::test_support::mem_surreal().await;
        store::test_support::seed_firm_with_brand(&db, "neon").await;
        let service = UncertainEmail(std::sync::atomic::AtomicUsize::new(0));
        let storage = crate::test_support::fs_storage().await;
        let mut email = sample_email();
        email.to = "support@neonlaw.com".into();
        receive(&db, &storage, &service, &email).await.unwrap();
        receive(&db, &storage, &service, &email).await.unwrap();
        assert_eq!(service.0.load(std::sync::atomic::Ordering::SeqCst), 1);
        let inbound = ingest_support_email(&db, &storage, &email)
            .await
            .unwrap()
            .unwrap();
        let receipt = store::inbox::acknowledgment_for(&db, inbound.message_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(receipt.delivery_state, store::inbox::delivery::UNKNOWN);
    }

    #[tokio::test]
    async fn missing_firm_never_sends_a_receipt() {
        let db = store::test_support::mem_surreal().await;
        let service = crate::email::CapturingEmail::new();
        let storage = crate::test_support::fs_storage().await;
        let mut email = sample_email();
        email.to = "support@neonlaw.com".into();
        assert!(receive(&db, &storage, &service, &email).await.is_err());
        assert!(service.captured().is_empty());
    }
}

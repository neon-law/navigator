use std::sync::Arc;

use uuid::Uuid;

use super::*;
use crate::communications::{
    self, channel, direction, CallDetails, CommunicationError, EmailDetails, IngestArgs, NewCall,
};
use crate::email_conversations::{self, EmailConversationError, NewConversation, NewMessage};
use crate::email_deliveries;
use crate::email_receipts::{self, NewEmailReceipt};
use crate::postgres_mirror::tests::{connect, fresh_schema};
use crate::postgres_mirror::{MirrorKey, Mirrored};
use crate::surreal::SurrealDb;
use crate::test_support::mem_surreal;

/// A Project only PostgreSQL holds: the communications tables reference it
/// by foreign key, and nothing here reads SurrealDB's.
async fn project(pg: &PostgresDb, code: &str) -> Uuid {
    let (id, entity_type, jurisdiction, entity) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );
    sqlx::query(
        "WITH t AS (INSERT INTO entity_type (id, name) VALUES ($1, $5) \
         RETURNING id), j AS (INSERT INTO jurisdiction (id, name) \
         VALUES ($2, $5) RETURNING id), \
         e AS (INSERT INTO entity (id, name, entity_type_id, jurisdiction_id) \
         SELECT $3, 'Synthetic Entity', t.id, j.id FROM t, j RETURNING id) \
         INSERT INTO project (id, code, name, status, entity_id) \
         SELECT $4, $5, $5, 'open', e.id FROM e",
    )
    .bind(entity_type)
    .bind(jurisdiction)
    .bind(entity)
    .bind(id)
    .bind(code)
    .execute(pg.pool())
    .await
    .unwrap();
    id
}

async fn surreal_count(surreal: &SurrealDb, table: &str) -> usize {
    let rows: Vec<serde_json::Value> = surreal
        .query(format!("SELECT VALUE id FROM {table}"))
        .await
        .unwrap()
        .take(0)
        .unwrap();
    rows.len()
}

fn receipt(digest: &str) -> NewEmailReceipt<'_> {
    NewEmailReceipt {
        receiving_mailbox: "support@example.com",
        deployment: "staging",
        firm_id: None,
        provider_account: None,
        sender_authenticated: false,
        raw_digest: digest,
        source_message_id: Some("synthetic@example.com"),
        archive_key: "inbound/synthetic.eml",
        letter_id: Uuid::now_v7(),
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one receipt walks every delivery state"
)]
async fn email_receipts_and_deliveries_live_in_postgres() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    with_test_db(
        pg.clone(),
        Box::pin(async {
            let new = receipt("digest-a");
            let (first, second) = tokio::join!(
                email_receipts::ensure(&surreal, &new),
                email_receipts::ensure(&surreal, &new)
            );
            let (first, second) = (first.unwrap(), second.unwrap());
            assert_eq!(first.receipt.id, second.receipt.id);
            assert_ne!(first.deduped, second.deduped);
            assert_eq!(email_receipts::count(&surreal).await.unwrap(), 1);
            let id = first.receipt.id;

            let later = chrono::Utc::now() + chrono::Duration::minutes(1);
            assert_eq!(
                email_receipts::list_overdue(&surreal, later)
                    .await
                    .unwrap()
                    .len(),
                1
            );
            let delivery = email_deliveries::ensure(&surreal, id, "C123")
                .await
                .unwrap();
            assert_eq!(
                email_deliveries::ensure(&surreal, id, "C999")
                    .await
                    .unwrap(),
                delivery
            );

            let (a, b) = tokio::join!(
                email_deliveries::admit_attempt(&surreal, id, "corr-a"),
                email_deliveries::admit_attempt(&surreal, id, "corr-b")
            );
            let admitted = [a.unwrap(), b.unwrap()];
            assert_eq!(
                admitted.iter().filter(|a| a.attempt_id.is_some()).count(),
                1
            );
            email_deliveries::mark_unknown(&surreal, id, "transport")
                .await
                .unwrap();
            email_deliveries::authorize_resend(&surreal, id, "synthetic-operator")
                .await
                .unwrap();
            let resent = email_deliveries::admit_attempt(&surreal, id, "corr-c")
                .await
                .unwrap();
            assert!(resent.attempt_id.is_some());
            let confirmed =
                email_deliveries::mark_confirmed(&surreal, id, "C123", "1700000000.000001")
                    .await
                    .unwrap();
            assert_eq!(confirmed.reconciliation.as_deref(), None);
            let attempts = email_deliveries::attempts(&surreal, id).await.unwrap();
            assert_eq!(
                attempts
                    .iter()
                    .map(|a| a.state.as_str())
                    .collect::<Vec<_>>(),
                ["unknown", "confirmed"]
            );
            assert_eq!(
                email_deliveries::list_by_state(&surreal, email_deliveries::CONFIRMED)
                    .await
                    .unwrap(),
                vec![confirmed]
            );
            let reopened =
                email_deliveries::reopen_after_failed_result(&surreal, id, "synthetic-operator")
                    .await
                    .unwrap();
            assert_eq!(
                (reopened.state.as_str(), reopened.current_correlation_id),
                (email_deliveries::NOT_ATTEMPTED, None)
            );
            assert!(reopened.reconciled_at.is_some());

            // The receipt reads its delivery's state.
            let found = email_receipts::find_by_id(&surreal, id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(found.delivery_state, email_deliveries::NOT_ATTEMPTED);
            email_receipts::mark_archived(&surreal, id).await.unwrap();
            let found =
                email_receipts::find(&surreal, "support@example.com", "staging", "digest-a")
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(found.processing_state, email_receipts::PROCESSING_ARCHIVED);
            assert!(matches!(
                email_deliveries::mark_failed(&surreal, Uuid::now_v7(), "rejected").await,
                Err(email_deliveries::EmailDeliveryError::NotFound)
            ));

            // Nothing reached SurrealDB, and the mirror leaves PostgreSQL alone.
            for table in ["email_receipt", "email_delivery", "email_delivery_attempt"] {
                assert_eq!(surreal_count(&surreal, table).await, 0, "{table}");
            }
            assert_eq!(
                crate::postgres_mirror::mirror(&surreal, &pg, &MirrorKey::EmailReceipt(id))
                    .await
                    .unwrap(),
                Mirrored::Unchanged
            );
            assert!(
                crate::postgres_mirror::email_deliveries::reconcile(&surreal, &pg)
                    .await
                    .unwrap()
                    .tables
                    .is_empty()
            );
            assert_eq!(email_receipts::count(&surreal).await.unwrap(), 1);
        }),
    )
    .await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one thread exercises every conversation entry point"
)]
async fn email_conversations_live_in_postgres() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let matter = project(&pg, "conversation-authority").await;
    with_test_db(
        pg.clone(),
        Box::pin(async {
            let new = NewConversation {
                token: "synthetic-token",
                external_email: "client@example.com",
                external_name: Some("Synthetic Client"),
                subject: "Question",
                person_id: None,
                notation_id: None,
            };
            let id = email_conversations::open(&surreal, &new).await.unwrap();
            assert!(matches!(
                email_conversations::open(&surreal, &new).await,
                Err(EmailConversationError::TokenTaken)
            ));
            let opened = email_conversations::by_token(&surreal, "synthetic-token")
                .await
                .unwrap()
                .unwrap();
            assert_eq!((opened.id, opened.status.as_str()), (id, "open"));
            let newer = email_conversations::open(
                &surreal,
                &NewConversation {
                    token: "synthetic-token-2",
                    ..new.clone()
                },
            )
            .await
            .unwrap();
            assert_eq!(
                email_conversations::by_external_email(&surreal, "client@example.com")
                    .await
                    .unwrap()
                    .map(|c| c.id),
                Some(newer)
            );

            let first = email_conversations::append(
                &surreal,
                &NewMessage {
                    conversation_id: id,
                    direction: email_conversations::DIRECTION_FROM_EXTERNAL,
                    from_addr: "client@example.com",
                    to_addr: "support@example.com",
                    subject: "Question",
                    body_text: "Hello",
                    ..NewMessage::default()
                },
            )
            .await
            .unwrap();
            let second = email_conversations::append(
                &surreal,
                &NewMessage {
                    conversation_id: id,
                    direction: email_conversations::DIRECTION_TO_LAWYER,
                    from_addr: "support@example.com",
                    to_addr: "lawyer@example.com",
                    subject: "Question",
                    body_text: "Hello",
                    provider_message_id: Some("provider-1"),
                    ..NewMessage::default()
                },
            )
            .await
            .unwrap();
            let messages = email_conversations::messages(&surreal, id).await.unwrap();
            assert_eq!(
                messages.iter().map(|m| m.id).collect::<Vec<_>>(),
                [first, second]
            );
            assert_eq!(
                email_conversations::message_by_id(&surreal, second)
                    .await
                    .unwrap()
                    .and_then(|m| m.provider_message_id),
                Some("provider-1".to_owned())
            );

            let awaiting = email_conversations::set_status(
                &surreal,
                id,
                email_conversations::STATUS_AWAITING_LAWYER,
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(awaiting.status, "awaiting_lawyer");
            let scoped = email_conversations::set_scope(&surreal, id, None, Some(matter))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(scoped.project_id, Some(matter));
            assert_eq!(
                email_conversations::by_id(&surreal, id).await.unwrap(),
                Some(scoped)
            );
            assert!(
                email_conversations::set_status(&surreal, Uuid::now_v7(), "closed")
                    .await
                    .unwrap()
                    .is_none()
            );

            for table in ["email_conversation", "email_conversation_message"] {
                assert_eq!(surreal_count(&surreal, table).await, 0, "{table}");
            }
            assert_eq!(
                crate::postgres_mirror::mirror(&surreal, &pg, &MirrorKey::EmailConversation(id))
                    .await
                    .unwrap(),
                Mirrored::Unchanged
            );
            assert!(email_conversations::by_id(&surreal, id)
                .await
                .unwrap()
                .is_some());
        }),
    )
    .await;
}

fn args<'a>(
    project_id: Uuid,
    channel: &'a str,
    direction: &'a str,
    body: &'a str,
) -> IngestArgs<'a> {
    IngestArgs {
        project_id,
        channel,
        direction,
        author_person_id: None,
        counterparty: Some("client@example.com"),
        subject: None,
        body,
        source_ref: None,
        asset_id: None,
        occurred_at: "2026-06-08T10:00:00Z",
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one synthetic matter exercises every Communication entry point"
)]
async fn communications_live_in_postgres() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let matter = project(&pg, "communication-authority").await;
    let other = project(&pg, "communication-authority-other").await;
    let tmp = tempfile::tempdir().unwrap();
    let storage: Arc<dyn cloud::StorageService> = Arc::new(
        cloud::FsStorage::new(tmp.path().to_path_buf())
            .await
            .unwrap(),
    );
    with_test_db(
        pg.clone(),
        Box::pin(async {
            let sent = communications::ingest_email(
                &surreal,
                &IngestArgs {
                    source_ref: Some("<sent@example.com>"),
                    ..args(
                        matter,
                        channel::EMAIL_OUTBOUND,
                        direction::OUTBOUND,
                        "Engagement",
                    )
                },
                EmailDetails::default(),
            )
            .await
            .unwrap();
            assert!(!sent.deduped);
            let reply = IngestArgs {
                source_ref: Some("<reply@example.com>"),
                occurred_at: "2026-06-08T12:00:00+03:00",
                ..args(
                    matter,
                    channel::EMAIL_INBOUND,
                    direction::INBOUND,
                    "Received",
                )
            };
            let details = EmailDetails {
                counterparty_role: Some("client"),
                acknowledges: Some(sent.communication_id),
            };
            let (a, b) = tokio::join!(
                communications::ingest_email(&surreal, &reply, details),
                communications::ingest_email(&surreal, &reply, details)
            );
            let (a, b) = (a.unwrap(), b.unwrap());
            assert_eq!(a.communication_id, b.communication_id);
            assert_ne!(a.deduped, b.deduped);
            assert!(matches!(
                communications::ingest_email(
                    &surreal,
                    &reply,
                    EmailDetails {
                        counterparty_role: Some("court"),
                        ..details
                    }
                )
                .await,
                Err(CommunicationError::Invalid(_))
            ));
            assert!(matches!(
                communications::ingest(
                    &surreal,
                    &IngestArgs {
                        project_id: other,
                        ..reply.clone()
                    }
                )
                .await,
                Err(CommunicationError::Invalid(_))
            ));

            let call = communications::record_call(
                &surreal,
                &NewCall {
                    project_id: matter,
                    counterparty: "Opposing Counsel".into(),
                    body: "Meet and confer".into(),
                    at: "2026-06-09T09:00:00Z".into(),
                    call: CallDetails {
                        direction: "outbound".into(),
                        counterparty_role: "opposing_counsel".into(),
                        duration_minutes: Some(15),
                        recorded: "none".into(),
                        meet_and_confer: Some("discovery".into()),
                    },
                    agreements: vec!["Extend the deadline".into()],
                    ..NewCall::default()
                },
            )
            .await
            .unwrap();
            assert_eq!(call.direction, direction::INTERNAL);
            assert!(call.has_unconfirmed_agreements());
            let confirmed = communications::confirm_agreements(
                &surreal,
                matter,
                call.id,
                sent.communication_id,
            )
            .await
            .unwrap();
            assert!(!confirmed.has_unconfirmed_agreements());
            assert!(matches!(
                communications::confirm_agreements(&surreal, other, call.id, sent.communication_id)
                    .await,
                Err(CommunicationError::NotFound)
            ));

            let thread = communications::for_project(&surreal, matter).await.unwrap();
            assert_eq!(
                thread
                    .iter()
                    .map(|row| row.body.as_str())
                    .collect::<Vec<_>>(),
                ["Received", "Engagement", "Meet and confer"]
            );
            let outbound = thread
                .iter()
                .find(|row| row.id == sent.communication_id)
                .unwrap();
            assert_eq!(
                outbound.receipt_acknowledged_at.as_deref(),
                Some("2026-06-08T09:00:00.000000Z")
            );
            let client = communications::for_project_client_visible(&surreal, matter)
                .await
                .unwrap();
            assert_eq!(client.len(), 2);

            communications::move_to_project(&surreal, a.communication_id, other)
                .await
                .unwrap();
            assert!(communications::exists_for_project(&surreal, other)
                .await
                .unwrap());
            assert_eq!(
                communications::find(&surreal, other, a.communication_id)
                    .await
                    .unwrap()
                    .map(|row| row.acknowledges),
                Some(Some(sent.communication_id))
            );

            assert_eq!(surreal_count(&surreal, "communication").await, 0);
            assert_eq!(
                crate::postgres_mirror::conversations::reconcile(&surreal, &pg)
                    .await
                    .unwrap()
                    .tables
                    .len(),
                0
            );

            let report = communications::purge_for_project(&surreal, &storage, matter)
                .await
                .unwrap();
            assert_eq!(report.communications_deleted, 2);
            assert!(!communications::exists_for_project(&surreal, matter)
                .await
                .unwrap());
            // The receipt moved to the other Project keeps its row; the link
            // to the purged email clears.
            assert_eq!(
                communications::find(&surreal, other, a.communication_id)
                    .await
                    .unwrap()
                    .map(|row| row.acknowledges),
                Some(None)
            );
        }),
    )
    .await;
}

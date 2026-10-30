//! PostgreSQL Inbox slice tests (LAW-238 D0), in the established
//! `store/src/postgres/*_tests.rs` pattern: a fresh schema per test from the
//! compiled migrations, skipped silently when no local PostgreSQL is running.
//!
//! The mem-Surreal store remains the authorization and Communication-spine
//! side of every test — that is the slice's contract. Postgres-authoritative
//! ingest is exercised through the same entry points the dispatcher uses, so
//! replay semantics (unique `inbox_ingress` key) and threading
//! (`find_open_thread`'s partition key) are proven against the real SQL.

use std::sync::Arc;

use super::inbox as pg_inbox;
use super::PostgresDb;
use crate::firms::{self, NewFirm};
use crate::inbox::{channel, delivery, direction, purpose, InboxError, IngestInbound};
use crate::persons::{self, NewPerson, Role};
use crate::postgres_mirror::tests::{connect, fresh_schema};
use crate::projects::{self, NewProject};
use crate::surreal::SurrealDb;
use crate::test_support::{mem_surreal, seed_entity};
use uuid::Uuid;

/// The `OutboundTransport` that fails the test if anything is dispatched.
struct NoTransport;

#[async_trait::async_trait]
impl crate::inbox::OutboundTransport for NoTransport {
    async fn deliver(
        &self,
        _envelope: &crate::inbox::OutboundEnvelope,
    ) -> Result<crate::inbox::TransportOutcome, String> {
        panic!("historical imports must never dispatch");
    }
}

struct Fixture {
    pg: PostgresDb,
    db: SurrealDb,
    firm: Uuid,
    owner: Uuid,
    lawyer: Uuid,
    project: Uuid,
    /// The synthetic Entity behind the seeded Firm — reference rows for
    /// additional Projects a test creates.
    entity: Uuid,
}

// Builds the full cross-store world: PostgreSQL schema plus reference rows
// and the mem-Surreal firm/project/participation graph.
#[allow(clippy::too_many_lines)]
async fn fixture() -> Option<Fixture> {
    let (options, database) = fresh_schema().await?;
    let pg = connect(&options, &database).await;
    let db = mem_surreal().await;
    let owner = persons::create(
        &db,
        &NewPerson::with_role("Owner", "owner@example.com", Role::Owner),
    )
    .await
    .unwrap()
    .id;
    let lawyer = persons::create(
        &db,
        &NewPerson::with_role("Lawyer", "lawyer@example.com", Role::Lawyer),
    )
    .await
    .unwrap()
    .id;
    let firm = firms::create(
        &db,
        &NewFirm {
            name: "Practice".into(),
            status: "active".into(),
            entity_id: seed_entity(&db).await,
            admin_dri_person_id: owner,
        },
    )
    .await
    .unwrap()
    .id;
    let project = projects::create(
        &db,
        &NewProject {
            code: "alpha-matter".into(),
            name: "Alpha Matter".into(),
            status: "open".into(),
            entity_id: seed_entity(&db).await,
            firm_id: Some(firm),
            ..NewProject::default()
        },
    )
    .await
    .unwrap()
    .id;

    // Reference rows in PostgreSQL under the same ids so the Inbox foreign
    // keys resolve (the projects_tests.rs pattern).
    let jurisdiction = Uuid::new_v4();
    let entity_type = Uuid::new_v4();
    let entity = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO jurisdiction (id,name,code) VALUES ($1,'Synthetic Jurisdiction','TST')",
    )
    .bind(jurisdiction)
    .execute(&pg.0)
    .await
    .unwrap();
    sqlx::query("INSERT INTO entity_type (id,name) VALUES ($1,'Synthetic Entity')")
        .bind(entity_type)
        .execute(&pg.0)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO entity (id,name,entity_type_id,jurisdiction_id) VALUES ($1,'Synthetic Entity',$2,$3)",
    )
    .bind(entity)
    .bind(entity_type)
    .bind(jurisdiction)
    .execute(&pg.0)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO firm (id,name,slug,status,entity_id) VALUES ($1,'Practice','practice-firm','active',$2)",
    )
    .bind(firm)
    .bind(entity)
    .execute(&pg.0)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO person (id,name,email,role) VALUES ($1,'Owner','owner@example.com','owner')",
    )
    .bind(owner)
    .execute(&pg.0)
    .await
    .unwrap();
    sqlx::query("INSERT INTO person (id,name,email,role) VALUES ($1,'Lawyer','lawyer@example.com','lawyer')")
        .bind(lawyer)
        .execute(&pg.0)
        .await
        .unwrap();
    // The proven reply-authority combination from the Surreal fixture: a
    // lawyer DRI on the matter — the designation itself is the lawyer's
    // firm-side participation row.
    projects::designate_dri_in_surreal(&db, project, lawyer, projects::DriSide::Lawyer)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO project (id,code,name,status,entity_id,firm_id) VALUES ($1,'alpha-matter','Alpha Matter','open',$2,$3)",
    )
    .bind(project)
    .bind(entity)
    .bind(firm)
    .execute(&pg.0)
    .await
    .unwrap();

    Some(Fixture {
        pg,
        db,
        firm,
        owner,
        lawyer,
        project,
        entity,
    })
}

/// A second Project mirrored into PostgreSQL under the fixture's Entity —
/// re-attribution targets need a PG row or the conversation FK refuses.
async fn seed_project(
    pg: &PostgresDb,
    db: &SurrealDb,
    entity: Uuid,
    firm: Uuid,
    code: &str,
    name: &str,
) -> Uuid {
    let project = projects::create(
        db,
        &NewProject {
            code: code.into(),
            name: name.into(),
            status: "open".into(),
            entity_id: entity,
            firm_id: Some(firm),
            ..NewProject::default()
        },
    )
    .await
    .unwrap()
    .id;
    sqlx::query(
        "INSERT INTO project (id,code,name,status,entity_id,firm_id) VALUES ($1,$2,$3,'open',$4,$5)",
    )
    .bind(project)
    .bind(code)
    .bind(name)
    .bind(entity)
    .bind(firm)
    .execute(&pg.0)
    .await
    .unwrap();
    project
}
fn inbound<'a>(
    firm: Uuid,
    channel_name: &'a str,
    account: &'a str,
    provider_message_id: &'a str,
    address: &'a str,
) -> IngestInbound<'a> {
    IngestInbound {
        firm_id: firm,
        brand_key: None,
        interaction_type: None,
        provider_post_id: None,
        parent_provider_message_id: None,
        visibility: None,
        channel: channel_name,
        provider_account: account,
        provider_message_id,
        provider_thread_ref: None,
        external_address: address,
        external_name: Some("Sender"),
        subject: Some("Question"),
        body: "Hello from outside.",
        source_page: Some("mailbox"),
        source_channel: Some(channel_name),
        matter_consent: true,
        promotional_opt_in: false,
        email_conversation_id: None,
        attachment_asset_id: None,
        occurred_at: "2026-09-27T00:00:00Z",
        raw_storage_key: None,
    }
}

fn inbound_with<'a>(
    base: &IngestInbound<'a>,
    thread_ref: Option<&'a str>,
    address: &'a str,
    provider_message_id: &'a str,
) -> IngestInbound<'a> {
    IngestInbound {
        provider_thread_ref: thread_ref,
        external_address: address,
        provider_message_id,
        ..base.clone()
    }
}

async fn message_count(pg: &PostgresDb) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM message")
        .fetch_one(&pg.0)
        .await
        .unwrap()
}

async fn conversation_count(pg: &PostgresDb) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM inbox_conversation")
        .fetch_one(&pg.0)
        .await
        .unwrap()
}

async fn ingress_count(pg: &PostgresDb) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM inbox_ingress")
        .fetch_one(&pg.0)
        .await
        .unwrap()
}

/// AC2: concurrent deliveries of the same provider id converge on one
/// message; the losers report the winner's ids as deduped, and no orphan
/// conversation or second ingress row survives.
#[tokio::test]
async fn concurrent_replays_dedup_race_safely() {
    let Some(fixture) = fixture().await else {
        return;
    };
    let pg = Arc::new(fixture.pg);
    let inbound = inbound(
        fixture.firm,
        channel::EMAIL,
        "nick@neonlaw.com",
        "msg-1",
        "sender@example.com",
    );

    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let pg = pg.clone();
        let inbound = inbound.clone();
        tasks.spawn(async move {
            pg_inbox::ingest_inbound_on(&pg, &inbound)
                .await
                .expect("ingest")
        });
    }
    let mut results = Vec::new();
    while let Some(result) = tasks.join_next().await {
        results.push(result.unwrap());
    }

    let deduped = results.iter().filter(|ingested| ingested.deduped).count();
    assert_eq!(deduped, 7, "exactly one delivery wins; others dedup");
    let first = &results[0];
    assert!(results
        .iter()
        .all(|ingested| ingested.message_id == first.message_id));
    assert!(results
        .iter()
        .all(|ingested| ingested.conversation_id == first.conversation_id));
    assert_eq!(message_count(&pg).await, 1);
    assert_eq!(conversation_count(&pg).await, 1);
    assert_eq!(ingress_count(&pg).await, 1);
}

/// AC2: threading follows the partition key exactly — the counterparty
/// address and provider thread reference split conversations; nothing else
/// does.
#[tokio::test]
async fn threading_partitions_by_the_surreal_key() {
    let Some(fixture) = fixture().await else {
        return;
    };
    let base = inbound(
        fixture.firm,
        channel::EMAIL,
        "nick@neonlaw.com",
        "msg-1",
        "sender@example.com",
    );

    let first = pg_inbox::ingest_inbound_on(&fixture.pg, &base)
        .await
        .unwrap();
    let replay = pg_inbox::ingest_inbound_on(&fixture.pg, &base)
        .await
        .unwrap();
    assert!(replay.deduped);
    assert_eq!(replay.conversation_id, first.conversation_id);

    // Same account and thread, different counterparty: a new conversation.
    let other_party = inbound_with(&base, None, "someone-else@example.com", "msg-2");
    let second = pg_inbox::ingest_inbound_on(&fixture.pg, &other_party)
        .await
        .unwrap();
    assert!(!second.deduped);
    assert_ne!(second.conversation_id, first.conversation_id);

    // Same counterparty, different thread reference: a new conversation.
    let other_thread = inbound_with(&base, Some("thread-9"), "sender@example.com", "msg-3");
    let third = pg_inbox::ingest_inbound_on(&fixture.pg, &other_thread)
        .await
        .unwrap();
    assert!(!third.deduped);
    assert_ne!(third.conversation_id, first.conversation_id);

    // Same counterparty and thread reference: threaded together.
    let same_thread = inbound_with(&base, Some("thread-9"), "sender@example.com", "msg-4");
    let fourth = pg_inbox::ingest_inbound_on(&fixture.pg, &same_thread)
        .await
        .unwrap();
    assert_eq!(fourth.conversation_id, third.conversation_id);
    assert_eq!(conversation_count(&fixture.pg).await, 3);
    assert_eq!(message_count(&fixture.pg).await, 4);
}

/// Sequential replay of an imported payload is a no-op that reports the
/// existing ids — LAW-238's idempotent import contract.
#[tokio::test]
async fn replay_reports_existing_ids_and_exits_clean() {
    let Some(fixture) = fixture().await else {
        return;
    };
    let inbound = inbound(
        fixture.firm,
        channel::EMAIL,
        "nick@neonlaw.com",
        "msg-1",
        "sender@example.com",
    );
    let first = pg_inbox::ingest_inbound_on(&fixture.pg, &inbound)
        .await
        .unwrap();
    let second = pg_inbox::ingest_inbound_on(&fixture.pg, &inbound)
        .await
        .unwrap();
    assert!(second.deduped);
    assert_eq!(second.message_id, first.message_id);
    assert_eq!(second.conversation_id, first.conversation_id);
    assert_eq!(message_count(&fixture.pg).await, 1);
}

/// AC4 seed: a lawyer-sent import ingests as `outbound` with the author set,
/// and historical outbound mail is never dispatched.
#[tokio::test]
async fn lawyer_sent_imports_ingest_outbound_with_author() {
    let Some(fixture) = fixture().await else {
        return;
    };
    let ingested = pg_inbox::ingest_message_on(
        &fixture.pg,
        direction::OUTBOUND,
        Some(fixture.owner),
        &inbound(
            fixture.firm,
            channel::EMAIL,
            "nick@neonlaw.com",
            "sent-1",
            "client@example.com",
        ),
    )
    .await
    .unwrap();
    let message = crate::postgres::inbox::message_by_id_on(&fixture.pg, ingested.message_id)
        .await
        .unwrap()
        .expect("message row");
    assert_eq!(message.direction, direction::OUTBOUND);
    assert_eq!(message.author_person_id, Some(fixture.owner));
    assert_eq!(message.delivery_state, delivery::RECEIVED);

    // Dispatching the queue does not resend historical imports.
    let report = crate::postgres::inbox::dispatch_queued_on(&fixture.pg, &fixture.db, &NoTransport)
        .await
        .unwrap();
    assert_eq!(report.submitted, 0);
    assert_eq!(report.failed, 0);
    assert_eq!(report.suppressed, 0);
}

/// AC7 seed: attribution projects each message as an email Communication with
/// `source_ref = inbox:{message-id}`, re-attribution moves the rows, and a
/// second projection is a dedup no-op.
#[tokio::test]
async fn attribution_projects_moves_and_dedups() {
    let Some(fixture) = fixture().await else {
        return;
    };
    let ingested = pg_inbox::ingest_inbound_on(
        &fixture.pg,
        &inbound(
            fixture.firm,
            channel::EMAIL,
            "nick@neonlaw.com",
            "msg-1",
            "sender@example.com",
        ),
    )
    .await
    .unwrap();

    let assignment = pg_inbox::assign_on(
        &fixture.pg,
        &fixture.db,
        fixture.owner,
        ingested.conversation_id,
        fixture.project,
    )
    .await
    .unwrap();
    assert_eq!(assignment.to_project_id, fixture.project);

    let communications = crate::communications::for_project(&fixture.db, fixture.project)
        .await
        .unwrap();
    assert_eq!(communications.len(), 1, "one projected Communication");
    assert_eq!(
        communications[0].channel,
        crate::communications::channel::EMAIL_INBOUND
    );
    assert_eq!(
        communications[0].source_ref.as_deref(),
        Some(format!("inbox:{}", ingested.message_id).as_str())
    );

    // Re-attribution moves the row; a repeated projection does not duplicate.
    let other_project = seed_project(
        &fixture.pg,
        &fixture.db,
        fixture.entity,
        fixture.firm,
        "beta-matter",
        "Beta Matter",
    )
    .await;
    pg_inbox::assign_on(
        &fixture.pg,
        &fixture.db,
        fixture.owner,
        ingested.conversation_id,
        other_project,
    )
    .await
    .unwrap();
    let moved = crate::communications::for_project(&fixture.db, other_project)
        .await
        .unwrap();
    assert_eq!(moved.len(), 1);
    assert!(
        crate::communications::for_project(&fixture.db, fixture.project)
            .await
            .unwrap()
            .is_empty()
    );
}

/// AC3 + AC5 seed: a token-less .eml walks the real adapter → ingest seam,
/// replays deduped via the scoped digest, and leaves one row. The digest
/// identity — not a native Message-ID — is what carries idempotency here.
#[tokio::test]
async fn email_import_survives_replay_without_a_message_id() {
    use crate::inbox_import::{EmailAdapter, ImportSource as _};

    let Some(fixture) = fixture().await else {
        return;
    };
    // No Message-ID header: the digest fallback is the only identity.
    let raw = b"From: Clerk <clerk@court.example>\r\n\
                To: nick@neonlaw.com\r\n\
                Subject: Re: discovery schedule\r\n\
                Date: Tue, 28 Oct 2026 14:03:00 +0000\r\n\
                \r\n\
                The schedule is entered.";

    let mut first = None::<crate::inbox::IngestedInbound>;
    for _ in 0..2 {
        let parsed = EmailAdapter
            .parse("nick@neonlaw.com", raw)
            .expect("token-less email parses");
        assert_eq!(parsed.len(), 1);
        let ingest = crate::inbox_import::import_message_to_ingest(
            &parsed[0],
            fixture.firm,
            "nick@neonlaw.com",
        );
        let ingested = pg_inbox::ingest_message_on(&fixture.pg, direction::INBOUND, None, &ingest)
            .await
            .unwrap();
        if let Some(first) = first {
            assert!(ingested.deduped, "the replay dedups");
            assert_eq!(ingested.message_id, first.message_id);
            assert_eq!(ingested.conversation_id, first.conversation_id);
        }
        first = Some(ingested);
    }
    assert_eq!(message_count(&fixture.pg).await, 1);
    assert_eq!(conversation_count(&fixture.pg).await, 1);
}

/// AC3 (CSV) + AC9 store half: a LinkedIn export threads into one
/// Conversation per CONVERSATION ID, replaying the same export dedups every
/// row, and the member's own row lands `outbound` with the author set.
#[tokio::test]
async fn linkedin_import_threads_one_conversation_per_export_thread() {
    use crate::inbox_import::{ImportSource as _, LinkedInAdapter};

    let Some(fixture) = fixture().await else {
        return;
    };
    let csv = "CONVERSATION ID,FROM,SENDER PROFILE URL,TO,DATE,SUBJECT,CONTENT\r\n\
               conv-1,Jordan Cliente,https://linkedin.com/in/jordan,Me,2026-10-29 10:00:00 UTC,,Can we talk about the retainer?\r\n\
               conv-1,Me,https://linkedin.com/in/me,Jordan Cliente,2026-10-29 11:00:00 UTC,,Of course - Tuesday works.\r\n\
               conv-2,Payee Recruiter,https://linkedin.com/in/recruiter,Me,2026-10-28 09:30:00 UTC,,Role opening\r\n";
    let mailbox = "https://linkedin.com/in/me";

    let mut thread_conversation_id = None;
    let mut replay_ids = None::<Vec<crate::inbox::IngestedInbound>>;
    for _ in 0..2 {
        let parsed = LinkedInAdapter
            .parse(mailbox, csv.as_bytes())
            .expect("export parses");
        assert_eq!(parsed.len(), 3);
        let mut pass = Vec::new();
        for message in &parsed {
            // The member's own rows are lawyer-sent; the door resolves the
            // mailbox owner's Person for them.
            let (direction_value, author) =
                if message.direction == crate::inbox_import::Direction::Outbound {
                    (direction::OUTBOUND, Some(fixture.owner))
                } else {
                    (direction::INBOUND, None)
                };
            let ingest =
                crate::inbox_import::import_message_to_ingest(message, fixture.firm, mailbox);
            let ingested =
                pg_inbox::ingest_message_on(&fixture.pg, direction_value, author, &ingest)
                    .await
                    .unwrap();
            pass.push(ingested);
            // The conv-1 thread holds the pair this test projects later; the
            // inbound member of the pair is the first row of the thread.
            if message.provider_thread_ref.as_deref() == Some("conv-1")
                && message.direction == crate::inbox_import::Direction::Inbound
            {
                thread_conversation_id = Some(ingested.conversation_id);
            }
        }
        if let Some(first) = &replay_ids {
            for (first, replayed) in first.iter().zip(&pass) {
                assert!(replayed.deduped, "replayed export dedups");
                assert_eq!(replayed.message_id, first.message_id);
            }
        }
        replay_ids = Some(pass);
    }
    // Two CONVERSATION IDs → exactly two Conversations, three Messages.
    assert_eq!(conversation_count(&fixture.pg).await, 2);
    assert_eq!(message_count(&fixture.pg).await, 3);

    // AC7 (linkedin): attributing the conv-1 thread projects a
    // linkedin_inbound/linkedin_outbound Communication pair.
    let thread = thread_conversation_id.expect("conv-1 thread ingested");
    pg_inbox::assign_on(
        &fixture.pg,
        &fixture.db,
        fixture.owner,
        thread,
        fixture.project,
    )
    .await
    .unwrap();
    let communications = crate::communications::for_project(&fixture.db, fixture.project)
        .await
        .unwrap();
    assert_eq!(communications.len(), 2, "one Communication per message");
    let mut channels = communications
        .iter()
        .map(|communication| communication.channel.as_str())
        .collect::<Vec<_>>();
    channels.sort_unstable();
    assert_eq!(
        channels,
        vec![
            crate::communications::channel::LINKEDIN_INBOUND,
            crate::communications::channel::LINKEDIN_OUTBOUND,
        ]
    );
}

/// AC6 seed: the verbatim payload's object-storage key recorded on ingest
/// survives the read path — bytes never live in a row, only the key.
#[tokio::test]
async fn raw_storage_key_persists_on_imported_messages() {
    let Some(fixture) = fixture().await else {
        return;
    };
    let mut ingest = inbound(
        fixture.firm,
        channel::EMAIL,
        "nick@neonlaw.com",
        "msg-1",
        "sender@example.com",
    );
    ingest.raw_storage_key = Some("mail/1761800000-notice.eml");
    let ingested = pg_inbox::ingest_inbound_on(&fixture.pg, &ingest)
        .await
        .unwrap();
    let message = pg_inbox::message_by_id_on(&fixture.pg, ingested.message_id)
        .await
        .unwrap()
        .expect("message row");
    assert_eq!(
        message.raw_storage_key.as_deref(),
        Some("mail/1761800000-notice.eml")
    );
}

/// AC8 store half: the attachment slot is first-wins — re-running the import
/// never re-points the Message, and the remaining attachments of the same
/// message resolve through MailProvenance, not this slot.
#[tokio::test]
async fn attachment_slot_is_first_wins() {
    let Some(fixture) = fixture().await else {
        return;
    };
    let ingested = pg_inbox::ingest_inbound_on(
        &fixture.pg,
        &inbound(
            fixture.firm,
            channel::EMAIL,
            "nick@neonlaw.com",
            "msg-1",
            "sender@example.com",
        ),
    )
    .await
    .unwrap();
    let seed_asset = |id: Uuid, sha: &str| {
        sqlx::query(
            "INSERT INTO asset (id, storage_key, content_type, byte_size, sha256_hex) \
             VALUES ($1, 'projects/x/documents/' || $1::text, 'application/pdf', 3, $2)",
        )
        .bind(id)
        .bind(sha)
    };
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    seed_asset(first, "aa")
        .execute(fixture.pg.pool())
        .await
        .unwrap();
    seed_asset(second, "bb")
        .execute(fixture.pg.pool())
        .await
        .unwrap();
    pg_inbox::set_attachment_asset_on(&fixture.pg, ingested.message_id, first)
        .await
        .unwrap();
    pg_inbox::set_attachment_asset_on(&fixture.pg, ingested.message_id, second)
        .await
        .unwrap();
    let message = pg_inbox::message_by_id_on(&fixture.pg, ingested.message_id)
        .await
        .unwrap()
        .expect("message row");
    assert_eq!(message.attachment_asset_id, Some(first));
}

/// Promotional opt-out suppresses the queued promotional send for that
/// address.
#[tokio::test]
async fn promotional_opt_out_suppresses_queued_sends() {
    let Some(fixture) = fixture().await else {
        return;
    };
    let ingested = pg_inbox::ingest_inbound_on(
        &fixture.pg,
        &inbound(
            fixture.firm,
            channel::EMAIL,
            "nick@neonlaw.com",
            "msg-1",
            "sender@example.com",
        ),
    )
    .await
    .unwrap();

    // Replies require the conversation to sit on a Project the actor
    // participates in — Owners may attribute, a participating Lawyer replies.
    pg_inbox::assign_on(
        &fixture.pg,
        &fixture.db,
        fixture.owner,
        ingested.conversation_id,
        fixture.project,
    )
    .await
    .unwrap();
    let message_id = pg_inbox::queue_reply_on(
        &fixture.pg,
        &fixture.db,
        fixture.lawyer,
        ingested.conversation_id,
        "Join our webinar",
        purpose::PROMOTIONAL,
    )
    .await
    .unwrap();

    let suppressed = pg_inbox::record_promotional_opt_out_on(
        &fixture.pg,
        fixture.firm,
        channel::EMAIL,
        "sender@example.com",
    )
    .await
    .unwrap();
    assert_eq!(suppressed, 1);
    let message = crate::postgres::inbox::message_by_id_on(&fixture.pg, message_id)
        .await
        .unwrap()
        .expect("message row");
    assert_eq!(message.delivery_state, delivery::SUPPRESSED);
}

/// The Slack notice lifecycle: one reserve wins, the second loses, a recorded
/// result never regresses, and reconciliation only surfaces unresolved rows.
#[tokio::test]
async fn alert_lifecycle_reserves_once_and_never_regresses() {
    let Some(fixture) = fixture().await else {
        return;
    };
    let ingested = pg_inbox::ingest_inbound_on(
        &fixture.pg,
        &inbound(
            fixture.firm,
            channel::EMAIL,
            "nick@neonlaw.com",
            "msg-1",
            "sender@example.com",
        ),
    )
    .await
    .unwrap();

    assert!(
        pg_inbox::reserve_alert_on(&fixture.pg, ingested.message_id, "slack-channel")
            .await
            .unwrap()
    );
    assert!(
        !pg_inbox::reserve_alert_on(&fixture.pg, ingested.message_id, "slack-channel")
            .await
            .unwrap()
    );

    pg_inbox::record_alert_on(
        &fixture.pg,
        ingested.message_id,
        "notified",
        Some("2026-10-30T00:00:00Z"),
    )
    .await
    .unwrap();
    let receipt = pg_inbox::alert_receipt_on(&fixture.pg, ingested.message_id)
        .await
        .unwrap();
    assert_eq!(receipt.state.as_deref(), Some("notified"));
    assert_eq!(receipt.channel_id.as_deref(), Some("slack-channel"));

    // Notified rows are never re-reconciled; a fresh ingest re-enters the queue.
    assert!(pg_inbox::awaiting_alert_reconciliation_on(&fixture.pg, 10)
        .await
        .unwrap()
        .is_empty());
}

/// An unsupported channel is refused before any row is written.
#[tokio::test]
async fn unsupported_channels_are_refused() {
    let Some(fixture) = fixture().await else {
        return;
    };
    let error = pg_inbox::ingest_inbound_on(
        &fixture.pg,
        &inbound(
            fixture.firm,
            "carrier_pigeon",
            "nick@neonlaw.com",
            "msg-1",
            "sender@example.com",
        ),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, InboxError::Denied));
    assert_eq!(message_count(&fixture.pg).await, 0);
    assert_eq!(conversation_count(&fixture.pg).await, 0);
}

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

#[tokio::test]
async fn native_authority_routes_the_public_inbox_lifecycle_to_postgres() {
    const CHILD: &str = "NAVIGATOR_TEST_PUBLIC_INBOX_AUTHORITY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        if crate::postgres_mirror::tests::test_postgres().is_none() {
            return;
        }
        let name = format!(
            "{}::native_authority_routes_the_public_inbox_lifecycle_to_postgres",
            module_path!().split_once("::").unwrap().1
        );
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &name, "--nocapture"])
            .env(CHILD, "1")
            .env(pg_inbox::AUTHORITY_ENV, "0")
            .env_remove("NAVIGATOR_POSTGRES_MIRROR")
            .output()
            .unwrap();
        assert!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "isolated native lifecycle: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let Some(f) = fixture().await else { return };
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(f.pg.pool())
        .await
        .unwrap();
    for (name, value) in [
        ("NAVIGATOR_POSTGRES_HOST", "127.0.0.1".to_string()),
        (
            "NAVIGATOR_POSTGRES_PORT",
            std::env::var("NAVIGATOR_TEST_POSTGRES_PORT").unwrap(),
        ),
        ("NAVIGATOR_POSTGRES_USER", "postgres".to_string()),
        ("NAVIGATOR_POSTGRES_PASSWORD", "synthetic".to_string()),
        ("NAVIGATOR_POSTGRES_SSLMODE", "disable".to_string()),
        ("NAVIGATOR_POSTGRES_DATABASE", database),
        (pg_inbox::AUTHORITY_ENV, "1".to_string()),
    ] {
        std::env::set_var(name, value);
    }
    assert_public_native_lifecycle(&f).await;
    assert_public_native_private_triage(&f).await;
    assert_public_native_call_and_fi_triage_coordination(&f).await;
}

async fn assert_public_native_private_triage(f: &Fixture) {
    use crate::inbox_triage::{self, NewInboxTriage, PrivilegeLabel, TriageDisposition};
    let mut input = inbound(
        f.firm,
        channel::X,
        "synthetic-x-account",
        "native-private-x",
        "synthetic-x-sender",
    );
    input.brand_key = Some("neon");
    input.interaction_type = Some("direct_message");
    input.visibility = Some("private");
    let saved = crate::inbox::ingest_inbound(&f.db, &input).await.unwrap();
    let review = inbox_triage::record(
        &f.db,
        &NewInboxTriage {
            message_id: saved.message_id,
            firm_id: f.firm,
            suggested_project_id: Some(f.project),
            project_confidence: Some(1.0),
            privilege: PrivilegeLabel::NotPrivileged,
            privilege_confidence: Some(1.0),
            rationale: Some("synthetic automation".into()),
            model_id: Some("synthetic-model".into()),
            prompt_version: "synthetic-prompt".into(),
            raw_output: Some("synthetic output".into()),
            disposition: TriageDisposition::Suggested,
            labeled_example: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(review.privilege, PrivilegeLabel::Uncertain);
    assert_eq!(review.disposition, TriageDisposition::NeedsHuman);
    assert!(review.raw_output.is_none() && review.model_id.is_none());
    assert!(inbox_triage::list_for_review(&f.db, f.owner)
        .await
        .unwrap()
        .iter()
        .any(|row| row.triage.id == review.id));
    projects::add_participation(
        &f.db,
        f.project,
        f.owner,
        projects::participation_for_role(Role::Owner),
    )
    .await
    .unwrap();
    let assigned = inbox_triage::confirm_project(&f.db, f.owner, review.id, f.project)
        .await
        .unwrap();
    assert_eq!(assigned.privilege, PrivilegeLabel::Uncertain);
    assert_eq!(assigned.disposition, TriageDisposition::NeedsHuman);
    let resolved = inbox_triage::resolve_privilege(
        &f.db,
        f.owner,
        review.id,
        PrivilegeLabel::Privileged,
        "synthetic human review",
    )
    .await
    .unwrap();
    assert_eq!(resolved.privilege, PrivilegeLabel::Privileged);
    assert_eq!(resolved.disposition, TriageDisposition::Confirmed);
    assert!(!inbox_triage::list_for_review(&f.db, f.owner)
        .await
        .unwrap()
        .iter()
        .any(|row| row.triage.id == review.id));
}

async fn assert_public_native_lifecycle(f: &Fixture) {
    use crate::inbox;
    let account = format!("+{}", 15_550_100_000_u64);
    let sender = format!("+{}", 15_555_550_127_u64);
    let mut input = inbound(
        f.firm,
        channel::SMS,
        &account,
        "public-native-flow",
        &sender,
    );
    input.brand_key = Some("neon");
    let saved = inbox::ingest_inbound(&f.db, &input).await.unwrap();
    assert_eq!(message_count(&f.pg).await, 1);
    assert_eq!(
        inbox::message_by_id(&f.db, saved.message_id)
            .await
            .unwrap()
            .unwrap()
            .body,
        "Hello from outside."
    );
    assert_eq!(
        inbox::conversation_by_id(&f.db, saved.conversation_id)
            .await
            .unwrap()
            .unwrap()
            .channel,
        channel::SMS
    );
    let mut source =
        f.db.query("SELECT VALUE id FROM inbox_conversation")
            .await
            .unwrap()
            .check()
            .unwrap();
    let source_ids: Vec<surrealdb::types::RecordId> = source.take(0).unwrap();
    assert!(
        source_ids.is_empty(),
        "native reads must not depend on a Source conversation copy"
    );
    let intake = inbox::list_intake(&f.db, f.owner).await.unwrap();
    assert_eq!(intake.len(), 1);
    assert_eq!(
        inbox::intake_brands(&f.db, f.owner).await.unwrap(),
        ["neon"]
    );
    assert_eq!(
        inbox::list_intake_in_brand(&f.db, f.owner, "neon")
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(inbox::list_intake_in_brand(&f.db, f.owner, "another-brand")
        .await
        .unwrap()
        .is_empty());
    let detail = inbox::intake_detail(&f.db, f.owner, saved.conversation_id)
        .await
        .unwrap();
    assert!(detail.can_sms_reply && detail.bodies_visible);
    inbox::assign(&f.db, f.owner, saved.conversation_id, f.project)
        .await
        .unwrap();
    assert_eq!(
        inbox::assignments(&f.db, saved.conversation_id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_public_native_project_actions(f, &saved).await;
}

async fn assert_public_native_project_actions(f: &Fixture, saved: &crate::inbox::IngestedInbound) {
    use crate::inbox;
    let note = inbox::post_note(
        &f.db,
        f.lawyer,
        saved.conversation_id,
        "synthetic internal note",
    )
    .await
    .unwrap();
    assert_eq!(
        inbox::message_by_id(&f.db, note)
            .await
            .unwrap()
            .unwrap()
            .direction,
        direction::INTERNAL
    );
    let reply = inbox::queue_sms_reply(&f.db, f.lawyer, saved.conversation_id, "synthetic reply")
        .await
        .unwrap();
    let thread = inbox::project_thread(&f.db, f.lawyer, f.project, saved.conversation_id)
        .await
        .unwrap();
    assert_eq!(thread.messages.len(), 3);
    assert_eq!(
        inbox::list_project(&f.db, f.lawyer, f.project)
            .await
            .unwrap()
            .len(),
        1
    );
    let report = inbox::dispatch_queued(&f.db, &CapturingTransport::default())
        .await
        .unwrap();
    assert_eq!(report.submitted, 1);
    assert_eq!(
        inbox::message_by_id(&f.db, reply)
            .await
            .unwrap()
            .unwrap()
            .delivery_state,
        delivery::SUBMITTED
    );
}

#[tokio::test]
async fn native_triage_assignment_respects_source_and_target_scope() {
    let Some(f) = fixture().await else { return };
    let saved = pg_inbox::ingest_inbound_on(
        &f.pg,
        &inbound(
            f.firm,
            channel::EMAIL,
            "scope@example.com",
            "native-review-scope",
            "sender@example.com",
        ),
    )
    .await
    .unwrap();
    pg_inbox::assign_on(&f.pg, &f.db, f.owner, saved.conversation_id, f.project)
        .await
        .unwrap();
    let target = seed_project(
        &f.pg,
        &f.db,
        f.entity,
        f.firm,
        "review-target",
        "Synthetic Review Project",
    )
    .await;
    assert!(matches!(
        pg_inbox::assign_for_triage_review_on(
            &f.pg,
            &f.db,
            f.lawyer,
            saved.conversation_id,
            target
        )
        .await,
        Err(InboxError::Denied)
    ));
    projects::add_participation(
        &f.db,
        target,
        f.lawyer,
        projects::participation_for_role(Role::Lawyer),
    )
    .await
    .unwrap();
    projects::designate_dri_in_surreal(&f.db, f.project, f.owner, projects::DriSide::Lawyer)
        .await
        .unwrap();
    let source = projects::participation_for_person(&f.db, f.lawyer, f.project)
        .await
        .unwrap()
        .unwrap();
    projects::remove_participation(&f.db, source.id)
        .await
        .unwrap();
    assert!(matches!(
        pg_inbox::assign_for_triage_review_on(
            &f.pg,
            &f.db,
            f.lawyer,
            saved.conversation_id,
            target
        )
        .await,
        Err(InboxError::Denied)
    ));
    assert_eq!(
        pg_inbox::conversation_by_id_on(&f.pg, saved.conversation_id)
            .await
            .unwrap()
            .unwrap()
            .project_id,
        Some(f.project)
    );
    let admin = native_review_admin(&f).await;
    let assigned =
        pg_inbox::assign_for_triage_review_on(&f.pg, &f.db, admin, saved.conversation_id, target)
            .await
            .unwrap();
    assert_eq!(assigned.to_project_id, target);
    assert!(matches!(
        pg_inbox::assign_on(&f.pg, &f.db, admin, saved.conversation_id, target).await,
        Err(InboxError::Denied)
    ));
    let detail = pg_inbox::intake_detail_on(&f.pg, &f.db, admin, saved.conversation_id)
        .await
        .unwrap();
    assert!(!detail.bodies_visible && detail.messages.is_empty());
}

async fn native_review_admin(f: &Fixture) -> Uuid {
    let admin = persons::create(
        &f.db,
        &NewPerson::with_role("synthetic reviewer", "reviewer@example.com", Role::Admin),
    )
    .await
    .unwrap();
    firms::add_membership(
        &f.db,
        &firms::NewPersonFirmRole {
            person_id: admin.id,
            firm_id: f.firm,
            membership: firms::FirmMembership::Admin,
            is_dri: false,
        },
    )
    .await
    .unwrap();
    sqlx::query("INSERT INTO person (id,name,email,role) VALUES ($1,'synthetic reviewer','reviewer@example.com','admin')")
        .bind(admin.id).execute(f.pg.pool()).await.unwrap();
    admin.id
}

#[tokio::test]
async fn native_contact_link_preserves_a_persons_existing_number() {
    let Some(f) = fixture().await else { return };
    let original = format!("+{}", 15_555_550_125_u64);
    let incoming = format!("+{}", 15_555_550_126_u64);
    let account = format!("+{}", 15_550_100_000_u64);
    let contact = persons::create(
        &f.db,
        &NewPerson::with_role(
            "synthetic contact",
            "existing.contact@example.com",
            Role::Client,
        ),
    )
    .await
    .unwrap();
    persons::set_phone(&f.db, contact.id, Some(&original))
        .await
        .unwrap();
    let sms = pg_inbox::ingest_inbound_on(
        &f.pg,
        &inbound(
            f.firm,
            channel::SMS,
            &account,
            "existing-contact-number",
            &incoming,
        ),
    )
    .await
    .unwrap();
    assert!(matches!(
        pg_inbox::link_contact_phone_on(&f.pg, &f.db, f.owner, sms.conversation_id, &contact.email)
            .await,
        Err(InboxError::Contact(_))
    ));
    let saved = persons::find_by_id(&f.db, contact.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.phone.as_deref(), Some(original.as_str()));
    assert!(persons::find_by_phone(&f.db, &incoming)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn native_voice_ingest_preserves_transcripts_and_provider_replay() {
    let Some(f) = fixture().await else { return };
    let provider_id = format!("synthetic-call-{}", Uuid::now_v7());
    let mut input = inbound(
        f.firm,
        channel::VOICE,
        "synthetic-receiving-account",
        &provider_id,
        "synthetic-caller",
    );
    input.visibility = Some("private");
    input.body = "Synthetic voice transcript.";
    let first = pg_inbox::ingest_inbound_on(&f.pg, &input).await.unwrap();
    let replay = pg_inbox::ingest_inbound_on(&f.pg, &input).await.unwrap();
    assert!(!first.deduped);
    assert!(replay.deduped);
    assert_eq!(first.conversation_id, replay.conversation_id);
    assert_eq!(first.message_id, replay.message_id);
    let saved_channel: String =
        sqlx::query_scalar("SELECT channel FROM inbox_conversation WHERE id=$1")
            .bind(first.conversation_id)
            .fetch_one(f.pg.pool())
            .await
            .unwrap();
    assert_eq!(saved_channel, channel::VOICE);
    let body: String = sqlx::query_scalar("SELECT body FROM message WHERE id=$1")
        .bind(first.message_id)
        .fetch_one(f.pg.pool())
        .await
        .unwrap();
    assert_eq!(body, input.body);
    assert_eq!(message_count(&f.pg).await, 1);
    assert_eq!(ingress_count(&f.pg).await, 1);
}

#[tokio::test]
async fn native_sms_requires_consent_and_reuses_the_private_thread() {
    let Some(f) = fixture().await else { return };
    let account = format!("+{}", 15_550_100_000_u64);
    let recipient = format!("+{}", 15_555_550_123_u64);
    let open = |actor, consent, body: &'static str| {
        let account = account.clone();
        let recipient = recipient.clone();
        let f = &f;
        async move {
            pg_inbox::open_sms_conversation_on(
                &f.pg, &f.db, actor, f.firm, None, &account, &recipient, body, consent,
            )
            .await
        }
    };
    assert!(matches!(
        open(f.owner, false, "hello").await,
        Err(InboxError::Denied)
    ));
    assert!(matches!(
        open(f.lawyer, true, "hello").await,
        Err(InboxError::Denied)
    ));
    assert!(matches!(
        open(f.owner, true, "  ").await,
        Err(InboxError::Empty)
    ));
    assert_eq!(conversation_count(&f.pg).await, 0);
    let (conversation, first) = open(f.owner, true, " first text ").await.unwrap();
    let (again, second) = open(f.owner, true, "second text").await.unwrap();
    assert_eq!(again, conversation);
    assert_ne!(first, second);
    let row = pg_inbox::conversation_by_id_on(&f.pg, conversation)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.channel, channel::SMS);
    assert_eq!(row.external_address, recipient);
    assert!(row.matter_consent);
    let reply = pg_inbox::queue_sms_reply_on(&f.pg, &f.db, f.owner, conversation, " reply ")
        .await
        .unwrap();
    let message = pg_inbox::message_by_id_on(&f.pg, reply)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(message.body, "reply");
    assert_eq!(message.delivery_state, delivery::QUEUED);
    assert_eq!(message.direction, direction::OUTBOUND);
    assert!(matches!(
        pg_inbox::queue_sms_reply_on(&f.pg, &f.db, f.lawyer, conversation, "denied").await,
        Err(InboxError::Denied)
    ));
    for body in [String::new(), "x".repeat(crate::inbox::SMS_BODY_LIMIT + 1)] {
        assert!(matches!(
            pg_inbox::queue_sms_reply_on(&f.pg, &f.db, f.owner, conversation, &body).await,
            Err(InboxError::Empty)
        ));
    }
    let detail = pg_inbox::intake_detail_on(&f.pg, &f.db, f.owner, conversation)
        .await
        .unwrap();
    assert!(detail.can_sms_reply && detail.bodies_visible);
    assert_eq!(detail.messages.len(), 3);
    let listed = pg_inbox::list_intake_on(&f.pg, &f.db, f.owner)
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, conversation);
}

#[tokio::test]
async fn native_contact_link_is_idempotent_and_refuses_number_takeover() {
    let Some(f) = fixture().await else { return };
    let account = format!("+{}", 15_550_100_000_u64);
    let number = format!("+{}", 15_555_550_124_u64);
    let sms = pg_inbox::ingest_inbound_on(
        &f.pg,
        &inbound(f.firm, channel::SMS, &account, "contact-link", &number),
    )
    .await
    .unwrap();
    let email = "synthetic.contact@example.com";
    let contact = persons::create(
        &f.db,
        &NewPerson::with_role("synthetic contact", email, Role::Client),
    )
    .await
    .unwrap();
    assert!(matches!(
        pg_inbox::link_contact_phone_on(&f.pg, &f.db, f.lawyer, sms.conversation_id, email).await,
        Err(InboxError::Denied)
    ));
    assert!(matches!(
        pg_inbox::link_contact_phone_on(
            &f.pg,
            &f.db,
            f.owner,
            sms.conversation_id,
            "missing@example.com"
        )
        .await,
        Err(InboxError::Contact(_))
    ));
    let linked = pg_inbox::link_contact_phone_on(
        &f.pg,
        &f.db,
        f.owner,
        sms.conversation_id,
        " SYNTHETIC.CONTACT@example.com ",
    )
    .await
    .unwrap();
    assert_eq!(linked.person_id, contact.id);
    let again = pg_inbox::link_contact_phone_on(&f.pg, &f.db, f.owner, sms.conversation_id, email)
        .await
        .unwrap();
    assert_eq!(again.person_id, contact.id);
    let rival = persons::create(
        &f.db,
        &NewPerson::with_role("synthetic rival", "rival@example.com", Role::Client),
    )
    .await
    .unwrap();
    assert!(matches!(
        pg_inbox::link_contact_phone_on(&f.pg, &f.db, f.owner, sms.conversation_id, &rival.email)
            .await,
        Err(InboxError::Contact(_))
    ));
    assert_eq!(
        persons::find_by_id(&f.db, contact.id)
            .await
            .unwrap()
            .unwrap()
            .phone
            .as_deref(),
        Some(number.as_str())
    );
    let detail = pg_inbox::intake_detail_on(&f.pg, &f.db, f.owner, sms.conversation_id)
        .await
        .unwrap();
    assert_eq!(detail.contact.unwrap().person_id, contact.id);
    let email_thread = pg_inbox::ingest_inbound_on(
        &f.pg,
        &inbound(f.firm, channel::EMAIL, "mailbox", "email-not-sms", email),
    )
    .await
    .unwrap();
    assert!(matches!(
        pg_inbox::link_contact_phone_on(&f.pg, &f.db, f.owner, email_thread.conversation_id, email)
            .await,
        Err(InboxError::Denied)
    ));
    assert!(matches!(
        pg_inbox::queue_sms_reply_on(&f.pg, &f.db, f.owner, email_thread.conversation_id, "text")
            .await,
        Err(InboxError::Denied)
    ));
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
        "INSERT INTO firm (id,name,slug,status,entity_id) VALUES ($1,'Practice','practice','active',$2)",
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
        project_id: None,
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
        attachments: Vec::new(),
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
            "INSERT INTO asset (id, storage_key, content_type, byte_size, sha256_hex,project_id) \
             VALUES ($1, 'projects/x/documents/' || $1::text, 'application/pdf', 3, $2,$3)",
        )
        .bind(id)
        .bind(sha)
        .bind(fixture.project)
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

#[tokio::test]
async fn targeted_ingest_preserves_project_scope_and_all_attachments() {
    let Some(fixture) = fixture().await else {
        return;
    };
    let mut input = inbound(
        fixture.firm,
        channel::SCRAPER,
        "court.example.test",
        "scraper-seed",
        "clerk@example.test",
    );
    input.project_id = Some(fixture.project);
    for index in 0..2 {
        let id = Uuid::now_v7();
        let sha = format!("{index:064x}");
        sqlx::query("INSERT INTO asset (id, storage_key, content_type, byte_size, sha256_hex,project_id) VALUES ($1, $2, 'application/pdf', 3, $3,$4)")
            .bind(id).bind(format!("synthetic/{id}")).bind(&sha).bind(fixture.project).execute(fixture.pg.pool()).await.unwrap();
        input.attachments.push(crate::inbox::IngestAttachment {
            asset_id: id,
            filename: format!("notice-{index}.pdf"),
            sha256: sha,
        });
    }
    let first = pg_inbox::ingest_inbound_on(&fixture.pg, &input)
        .await
        .unwrap();
    let conversation = pg_inbox::conversation_by_id_on(&fixture.pg, first.conversation_id)
        .await
        .unwrap()
        .unwrap();
    let links: i64 =
        sqlx::query_scalar("SELECT count(*) FROM inbox_message_asset WHERE message_id = $1")
            .bind(first.message_id)
            .fetch_one(fixture.pg.pool())
            .await
            .unwrap();
    assert_eq!(
        (conversation.project_id, links),
        (Some(fixture.project), 2),
        "targeted ingest retains its Project and every provider attachment"
    );
    let replay = pg_inbox::ingest_inbound_on(&fixture.pg, &input)
        .await
        .unwrap();
    assert_eq!(replay.message_id, first.message_id);
    let links: i64 =
        sqlx::query_scalar("SELECT count(*) FROM inbox_message_asset WHERE message_id = $1")
            .bind(first.message_id)
            .fetch_one(fixture.pg.pool())
            .await
            .unwrap();
    assert_eq!(links, 2, "replay must not duplicate attachment links");
    input.project_id = None;
    input.provider_message_id = "unassigned-seed";
    let unassigned = pg_inbox::ingest_inbound_on(&fixture.pg, &input)
        .await
        .unwrap();
    assert_ne!(
        unassigned.conversation_id, first.conversation_id,
        "unassigned intake cannot join a matter-scoped thread"
    );
}

async fn assigned_email(fixture: &Fixture, provider_id: &str) -> crate::inbox::IngestedInbound {
    let input = inbound(
        fixture.firm,
        channel::EMAIL,
        "inbox@example.com",
        provider_id,
        "sender@example.com",
    );
    let ingested = pg_inbox::ingest_inbound_on(&fixture.pg, &input)
        .await
        .unwrap();
    pg_inbox::assign_on(
        &fixture.pg,
        &fixture.db,
        fixture.owner,
        ingested.conversation_id,
        fixture.project,
    )
    .await
    .unwrap();
    ingested
}

#[tokio::test]
async fn native_email_relay_loses_authority_when_participation_is_removed() {
    let Some(f) = fixture().await else { return };
    let ingested = assigned_email(&f, "relay-authority").await;
    let email_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO email_conversation (id,token,external_email,subject,status,firm_id,project_id,inserted_at,updated_at) \
         VALUES ($1,$2,'sender@example.com','Synthetic thread','open',$3,$4,now(),now())",
    ).bind(email_id).bind(email_id.to_string()).bind(f.firm).bind(f.project)
        .execute(f.pg.pool()).await.unwrap();
    sqlx::query("UPDATE inbox_conversation SET email_conversation_id=$2 WHERE id=$1")
        .bind(ingested.conversation_id)
        .bind(email_id)
        .execute(f.pg.pool())
        .await
        .unwrap();
    let clerk = persons::create(
        &f.db,
        &NewPerson::with_role("synthetic clerk", "clerk@example.com", Role::Clerk),
    )
    .await
    .unwrap();
    assert!(
        !pg_inbox::clerk_may_relay_email_thread_on(&f.pg, &f.db, clerk.id, email_id)
            .await
            .unwrap()
    );
    let participation = projects::add_participation(
        &f.db,
        f.project,
        clerk.id,
        projects::participation_for_role(Role::Clerk),
    )
    .await
    .unwrap();
    assert!(
        pg_inbox::clerk_may_relay_email_thread_on(&f.pg, &f.db, clerk.id, email_id)
            .await
            .unwrap()
    );
    assert!(
        pg_inbox::clerk_may_relay_email_thread_on(&f.pg, &f.db, f.lawyer, email_id)
            .await
            .unwrap()
    );
    assert!(
        !pg_inbox::clerk_may_relay_email_thread_on(&f.pg, &f.db, f.owner, email_id)
            .await
            .unwrap()
    );
    assert!(
        !pg_inbox::clerk_may_relay_email_thread_on(&f.pg, &f.db, Uuid::now_v7(), email_id)
            .await
            .unwrap()
    );
    assert!(
        !pg_inbox::clerk_may_relay_email_thread_on(&f.pg, &f.db, f.lawyer, Uuid::now_v7())
            .await
            .unwrap()
    );
    projects::remove_participation(&f.db, participation.id)
        .await
        .unwrap();
    assert!(
        !pg_inbox::clerk_may_relay_email_thread_on(&f.pg, &f.db, clerk.id, email_id)
            .await
            .unwrap()
    );
}

#[derive(Default)]
struct CapturingTransport(std::sync::Mutex<Vec<crate::inbox::OutboundEnvelope>>);

#[async_trait::async_trait]
impl crate::inbox::OutboundTransport for CapturingTransport {
    async fn deliver(
        &self,
        envelope: &crate::inbox::OutboundEnvelope,
    ) -> Result<crate::inbox::TransportOutcome, String> {
        self.0.lock().unwrap().push(envelope.clone());
        match envelope.body.as_str() {
            "unknown" => Ok(crate::inbox::TransportOutcome::Unknown),
            "rejected" => Ok(crate::inbox::TransportOutcome::Rejected {
                detail: "synthetic rejection".into(),
            }),
            "transport-error" => Err("synthetic transport failure".into()),
            _ => Ok(crate::inbox::TransportOutcome::Submitted {
                provider_ref: "synthetic-receipt".into(),
            }),
        }
    }
}

async fn assert_native_receipts(fixture: &Fixture, message_id: Uuid) {
    for (firm, channel_name, account) in [
        (Uuid::now_v7(), channel::EMAIL, "inbox@example.com"),
        (fixture.firm, channel::FACEBOOK, "inbox@example.com"),
        (fixture.firm, channel::EMAIL, "other@example.com"),
    ] {
        assert_eq!(
            pg_inbox::apply_delivery_on(
                &fixture.pg,
                &fixture.db,
                firm,
                channel_name,
                account,
                "synthetic-receipt",
                delivery::DELIVERED
            )
            .await
            .unwrap(),
            None
        );
    }
    assert_eq!(
        pg_inbox::apply_delivery_on(
            &fixture.pg,
            &fixture.db,
            fixture.firm,
            channel::EMAIL,
            "inbox@example.com",
            "synthetic-receipt",
            delivery::DELIVERED
        )
        .await
        .unwrap(),
        Some(message_id)
    );
    pg_inbox::apply_delivery_on(
        &fixture.pg,
        &fixture.db,
        fixture.firm,
        channel::EMAIL,
        "inbox@example.com",
        "synthetic-receipt",
        delivery::SUBMITTED,
    )
    .await
    .unwrap();
    assert_eq!(
        pg_inbox::message_by_id_on(&fixture.pg, message_id)
            .await
            .unwrap()
            .unwrap()
            .delivery_state,
        delivery::DELIVERED
    );
    assert_eq!(
        pg_inbox::apply_delivery_on(
            &fixture.pg,
            &fixture.db,
            fixture.firm,
            channel::EMAIL,
            "inbox@example.com",
            "missing-receipt",
            delivery::DELIVERED
        )
        .await
        .unwrap(),
        None
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM inbox_delivery_attempt")
        .fetch_one(fixture.pg.pool())
        .await
        .unwrap();
    assert_eq!(count, 5, "four provider outcomes and one advancing receipt");
}

#[tokio::test]
async fn native_dispatch_retains_each_outcome_and_never_resends_uncertain_messages() {
    let Some(fixture) = fixture().await else {
        return;
    };
    let ingested = assigned_email(&fixture, "native-dispatch").await;
    let note = pg_inbox::post_note_on(
        &fixture.pg,
        &fixture.db,
        fixture.lawyer,
        ingested.conversation_id,
        " internal note ",
    )
    .await
    .unwrap();
    assert_eq!(
        pg_inbox::message_by_id_on(&fixture.pg, note)
            .await
            .unwrap()
            .unwrap()
            .direction,
        direction::INTERNAL
    );
    let mut ids = Vec::new();
    for body in ["submitted", "unknown", "rejected", "transport-error"] {
        ids.push(
            pg_inbox::queue_reply_on(
                &fixture.pg,
                &fixture.db,
                fixture.lawyer,
                ingested.conversation_id,
                body,
                purpose::MATTER,
            )
            .await
            .unwrap(),
        );
    }
    let transport = CapturingTransport::default();
    let report = pg_inbox::dispatch_queued_on(&fixture.pg, &fixture.db, &transport)
        .await
        .unwrap();
    assert_eq!(
        (
            report.submitted,
            report.unknown,
            report.failed,
            report.suppressed
        ),
        (1, 1, 2, 0)
    );
    assert_eq!(transport.0.lock().unwrap().len(), 4);
    for (id, expected) in ids.iter().zip([
        delivery::SUBMITTED,
        delivery::UNKNOWN,
        delivery::FAILED,
        delivery::FAILED,
    ]) {
        assert_eq!(
            pg_inbox::message_by_id_on(&fixture.pg, *id)
                .await
                .unwrap()
                .unwrap()
                .delivery_state,
            expected
        );
    }
    let again = pg_inbox::dispatch_queued_on(&fixture.pg, &fixture.db, &transport)
        .await
        .unwrap();
    assert_eq!(again, crate::inbox::DispatchReport::default());
    assert_eq!(transport.0.lock().unwrap().len(), 4);
    assert_native_receipts(&fixture, ids[0]).await;
}

#[tokio::test]
async fn native_project_queries_preserve_scope_and_read_markers() {
    let Some(fixture) = fixture().await else {
        return;
    };
    let ingested = assigned_email(&fixture, "native-thread").await;
    let listed = pg_inbox::list_intake_on(&fixture.pg, &fixture.db, fixture.owner)
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].unread);
    let project =
        pg_inbox::list_project_on(&fixture.pg, &fixture.db, fixture.lawyer, fixture.project)
            .await
            .unwrap();
    assert_eq!(project[0].direction, direction::INBOUND);
    let thread = pg_inbox::project_thread_on(
        &fixture.pg,
        &fixture.db,
        fixture.lawyer,
        fixture.project,
        ingested.conversation_id,
    )
    .await
    .unwrap();
    assert!(thread.can_note && thread.can_send);
    assert_eq!(thread.messages.len(), 1);
    assert!(
        !pg_inbox::conversation_by_id_on(&fixture.pg, ingested.conversation_id)
            .await
            .unwrap()
            .unwrap()
            .unread
    );
    let assignments = pg_inbox::assignments_on(&fixture.pg, ingested.conversation_id)
        .await
        .unwrap();
    assert_eq!(assignments.len(), 1);
    assert_eq!(assignments[0].to_project_id, fixture.project);
    let outsider = persons::create(
        &fixture.db,
        &NewPerson::with_role("Outside Lawyer", "outside@example.com", Role::Lawyer),
    )
    .await
    .unwrap();
    assert!(matches!(
        pg_inbox::list_project_on(&fixture.pg, &fixture.db, outsider.id, fixture.project).await,
        Err(InboxError::Denied)
    ));
    assert!(matches!(
        pg_inbox::project_thread_on(
            &fixture.pg,
            &fixture.db,
            outsider.id,
            fixture.project,
            ingested.conversation_id
        )
        .await,
        Err(InboxError::Denied)
    ));
    assert!(matches!(
        pg_inbox::readable_attachment_on(
            &fixture.pg,
            &fixture.db,
            outsider.id,
            ingested.message_id
        )
        .await,
        Err(InboxError::Denied)
    ));
    assert!(matches!(
        pg_inbox::post_note_on(
            &fixture.pg,
            &fixture.db,
            outsider.id,
            ingested.conversation_id,
            "private note"
        )
        .await,
        Err(InboxError::Denied)
    ));
    let detail = pg_inbox::intake_detail_on(
        &fixture.pg,
        &fixture.db,
        fixture.owner,
        ingested.conversation_id,
    )
    .await
    .unwrap();
    assert!(
        !detail.bodies_visible,
        "global intake admission does not grant matter participation"
    );
    assert!(detail.messages.is_empty());
    assert!(!pg_inbox::clerk_may_relay_email_thread_on(
        &fixture.pg,
        &fixture.db,
        outsider.id,
        Uuid::now_v7()
    )
    .await
    .unwrap());
}

#[tokio::test]
async fn native_processing_and_acknowledgment_lifecycles_are_replay_safe() {
    let Some(fixture) = fixture().await else {
        return;
    };
    let ingested = assigned_email(&fixture, "native-processing").await;
    assert!(pg_inbox::process_state_on(&fixture.pg, Uuid::now_v7())
        .await
        .unwrap()
        .is_none());
    let initial = pg_inbox::process_state_on(&fixture.pg, ingested.message_id)
        .await
        .unwrap()
        .unwrap();
    assert!(initial.route.is_none() && initial.processed_at.is_none());
    assert_eq!(
        pg_inbox::unprocessed_inbox_on(&fixture.pg, Some(&[fixture.firm]))
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        pg_inbox::unprocessed_inbox_on(&fixture.pg, Some(&[Uuid::now_v7()]))
            .await
            .unwrap()
            .is_empty()
    );
    let now = chrono::Utc::now();
    pg_inbox::mark_processed_on(&fixture.pg, ingested.message_id, "other", now)
        .await
        .unwrap();
    let processed = pg_inbox::process_state_on(&fixture.pg, ingested.message_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(processed.route.as_deref(), Some("other"));
    assert_eq!(processed.processed_at, Some(now));
    assert!(pg_inbox::unprocessed_inbox_on(&fixture.pg, None)
        .await
        .unwrap()
        .is_empty());
    let acknowledgment =
        pg_inbox::reserve_acknowledgment_on(&fixture.pg, &ingested, "Synthetic acknowledgment")
            .await
            .unwrap()
            .unwrap();
    assert!(pg_inbox::reserve_acknowledgment_on(
        &fixture.pg,
        &ingested,
        "Synthetic acknowledgment"
    )
    .await
    .unwrap()
    .is_none());
    pg_inbox::acknowledgment_submitted_on(
        &fixture.pg,
        acknowledgment,
        Some("synthetic-ack".into()),
    )
    .await
    .unwrap();
    let message = pg_inbox::message_by_id_on(&fixture.pg, acknowledgment)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(message.delivery_state, delivery::SUBMITTED);
    assert_eq!(message.provider_ref.as_deref(), Some("synthetic-ack"));
}

#[tokio::test]
async fn native_social_replies_keep_private_threads_private_through_dispatch() {
    let Some(fixture) = fixture().await else {
        return;
    };
    let mut input = inbound(
        fixture.firm,
        channel::FACEBOOK,
        "synthetic-page",
        "private-comment",
        "synthetic-sender",
    );
    input.interaction_type = Some("comment");
    input.provider_post_id = Some("synthetic-post");
    input.visibility = Some("private");
    let ingested = pg_inbox::ingest_inbound_on(&fixture.pg, &input)
        .await
        .unwrap();
    assert!(matches!(
        pg_inbox::queue_social_reply_on(
            &fixture.pg,
            &fixture.db,
            fixture.owner,
            ingested.conversation_id,
            "public reply",
            "public"
        )
        .await,
        Err(InboxError::Denied)
    ));
    assert!(matches!(
        pg_inbox::queue_social_reply_on(
            &fixture.pg,
            &fixture.db,
            fixture.owner,
            ingested.conversation_id,
            " ",
            "private"
        )
        .await,
        Err(InboxError::Empty)
    ));
    let reply = pg_inbox::queue_social_reply_on(
        &fixture.pg,
        &fixture.db,
        fixture.owner,
        ingested.conversation_id,
        "private reply",
        "private",
    )
    .await
    .unwrap();
    let transport = CapturingTransport::default();
    let report = pg_inbox::dispatch_queued_on(&fixture.pg, &fixture.db, &transport)
        .await
        .unwrap();
    assert_eq!(report.submitted, 1);
    let sent = transport.0.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].visibility.as_deref(), Some("private"));
    assert_eq!(
        sent[0].reply_to_provider_message_id.as_deref(),
        Some("private-comment")
    );
    drop(sent);
    assert_eq!(
        pg_inbox::message_by_id_on(&fixture.pg, reply)
            .await
            .unwrap()
            .unwrap()
            .delivery_state,
        delivery::SUBMITTED
    );
    // Even a corrupted queued public reply is refused by the final send check.
    let unsafe_reply = pg_inbox::queue_social_reply_on(
        &fixture.pg,
        &fixture.db,
        fixture.owner,
        ingested.conversation_id,
        "unsafe reply",
        "private",
    )
    .await
    .unwrap();
    sqlx::query("UPDATE message SET visibility = 'public' WHERE id = $1")
        .bind(unsafe_reply)
        .execute(fixture.pg.pool())
        .await
        .unwrap();
    let refused = pg_inbox::dispatch_queued_on(&fixture.pg, &fixture.db, &transport)
        .await
        .unwrap();
    assert_eq!(refused.suppressed, 1);
    assert_eq!(transport.0.lock().unwrap().len(), 1);
}

async fn seed_native_attachment(
    fixture: &Fixture,
    message_id: Uuid,
) -> crate::inbox::IngestAttachment {
    let asset = Uuid::now_v7();
    sqlx::query("INSERT INTO asset (id, storage_key, content_type, byte_size, sha256_hex,project_id) VALUES ($1, 'synthetic/notice.pdf', 'application/pdf', 3, 'synthetic-digest',$2)")
        .bind(asset).bind(fixture.project).execute(fixture.pg.pool()).await.unwrap();
    pg_inbox::set_attachment_asset_on(&fixture.pg, message_id, asset)
        .await
        .unwrap();
    let attachment = crate::inbox::IngestAttachment {
        asset_id: asset,
        filename: "notice.pdf".into(),
        sha256: "synthetic-digest".into(),
    };
    for _ in 0..2 {
        pg_inbox::link_message_assets_on(
            &fixture.pg,
            fixture.firm,
            message_id,
            std::slice::from_ref(&attachment),
        )
        .await
        .unwrap();
    }
    attachment
}

#[tokio::test]
async fn native_attachments_and_intake_details_follow_firm_and_project_scope() {
    let Some(fixture) = fixture().await else {
        return;
    };
    let ingested = assigned_email(&fixture, "native-attachment-scope").await;
    firms::add_membership(
        &fixture.db,
        &firms::NewPersonFirmRole {
            person_id: fixture.lawyer,
            firm_id: fixture.firm,
            membership: firms::FirmMembership::Lawyer,
            is_dri: false,
        },
    )
    .await
    .unwrap();
    let attachment = seed_native_attachment(&fixture, ingested.message_id).await;
    let asset = attachment.asset_id;
    let attached = pg_inbox::message_assets_on(&fixture.pg, fixture.firm, ingested.message_id)
        .await
        .unwrap();
    assert_eq!(attached.len(), 1);
    assert_eq!(
        (
            attached[0].asset_id,
            attached[0].filename.as_str(),
            attached[0].position
        ),
        (asset, "notice.pdf", 0)
    );
    assert!(
        pg_inbox::message_assets_on(&fixture.pg, Uuid::now_v7(), ingested.message_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        pg_inbox::link_message_assets_on(
            &fixture.pg,
            Uuid::now_v7(),
            ingested.message_id,
            &[attachment]
        )
        .await,
        Err(InboxError::NotFound)
    ));
    assert_eq!(
        pg_inbox::readable_attachment_on(
            &fixture.pg,
            &fixture.db,
            fixture.lawyer,
            ingested.message_id
        )
        .await
        .unwrap(),
        Some(asset)
    );
    let detail = pg_inbox::intake_detail_on(
        &fixture.pg,
        &fixture.db,
        fixture.lawyer,
        ingested.conversation_id,
    )
    .await
    .unwrap();
    assert!(detail.bodies_visible);
    assert_eq!(detail.messages.len(), 1);
    let listed = pg_inbox::list_intake_on(&fixture.pg, &fixture.db, fixture.lawyer)
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].has_attachments);
    assert!(!listed[0].unread);
    let client = persons::create(
        &fixture.db,
        &NewPerson::new("Synthetic Client", "client@example.com"),
    )
    .await
    .unwrap();
    assert!(matches!(
        pg_inbox::list_intake_on(&fixture.pg, &fixture.db, client.id).await,
        Err(InboxError::Denied)
    ));
    assert!(matches!(
        pg_inbox::intake_detail_on(
            &fixture.pg,
            &fixture.db,
            client.id,
            ingested.conversation_id
        )
        .await,
        Err(InboxError::Denied)
    ));
    assert!(matches!(
        pg_inbox::readable_attachment_on(&fixture.pg, &fixture.db, client.id, ingested.message_id)
            .await,
        Err(InboxError::Denied)
    ));
}

#[tokio::test]
async fn legacy_inbox_repair_cannot_replace_native_authority_or_guess_on_invalid_config() {
    const CHILD: &str = "NAVIGATOR_TEST_INBOX_REPAIR_AUTHORITY_CHILD";
    let Some(case) = std::env::var(CHILD).ok() else {
        if crate::postgres_mirror::tests::test_postgres().is_none() {
            return;
        }
        for case in ["1", "not-a-valid-authority"] {
            let output=std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact",format!("{}::legacy_inbox_repair_cannot_replace_native_authority_or_guess_on_invalid_config",module_path!().split_once("::").unwrap().1).as_str(),"--nocapture"])
                .env(CHILD,case).env(pg_inbox::AUTHORITY_ENV,"0").env_remove("NAVIGATOR_POSTGRES_MIRROR")
                .output().unwrap();
            assert!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "authority case {case}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return;
    };
    let Some(f) = fixture().await else {
        return;
    };
    ensure_receipt_firm_entity(&f).await;
    let source = crate::inbox::ingest_inbound(
        &f.db,
        &inbound(
            f.firm,
            channel::EMAIL,
            "authority@example.com",
            "authority-proof",
            "sender@example.com",
        ),
    )
    .await
    .unwrap();
    // Use explicit legal values for nullable checked text fields. Surreal's
    // option-string NONE projection otherwise reaches PostgreSQL's enum CHECK
    // before this authority regression can exercise the intended boundary.
    f.db.query("UPDATE $id SET visibility = 'private', route = 'other'")
        .bind((
            "id",
            crate::surreal::record_id("message", source.message_id),
        ))
        .await
        .unwrap()
        .check()
        .unwrap();
    crate::postgres_mirror::mirror(
        &f.db,
        &f.pg,
        &crate::postgres_mirror::MirrorKey::InboxMessage(source.message_id),
    )
    .await
    .unwrap();
    sqlx::query(
        "UPDATE message SET body='native authoritative version',updated_at=now() WHERE id=$1",
    )
    .bind(source.message_id)
    .execute(f.pg.pool())
    .await
    .unwrap();
    std::env::set_var(pg_inbox::AUTHORITY_ENV, &case);
    let repair = crate::postgres_mirror::mirror(
        &f.db,
        &f.pg,
        &crate::postgres_mirror::MirrorKey::InboxMessage(source.message_id),
    )
    .await;
    assert!(repair.is_err(),"a stale Source row cannot overwrite native Inbox or select authority on malformed configuration");
    assert!(crate::postgres_mirror::inbox::reconcile(&f.db, &f.pg)
        .await
        .is_err());
    let body: String = sqlx::query_scalar("SELECT body FROM message WHERE id=$1")
        .bind(source.message_id)
        .fetch_one(f.pg.pool())
        .await
        .unwrap();
    assert_eq!(body, "native authoritative version");
}

#[tokio::test]
async fn bare_attachment_ids_and_metadata_are_not_firm_receipt_evidence() {
    let Some(f) = fixture().await else {
        return;
    };
    let asset = Uuid::now_v7();
    let sha = crate::assets::sha256_hex(b"synthetic bytes received elsewhere");
    sqlx::query("INSERT INTO asset(id,storage_key,content_type,byte_size,sha256_hex,metadata) VALUES ($1,'synthetic/unclaimed','text/plain',1,$2,$3)")
        .bind(asset).bind(&sha).bind(serde_json::json!({"firm_id":f.firm.to_string()})).execute(f.pg.pool()).await.unwrap();
    let attachment = crate::inbox::IngestAttachment {
        asset_id: asset,
        filename: "supplied.txt".into(),
        sha256: sha,
    };
    let mut input = inbound(
        f.firm,
        channel::EMAIL,
        "receipt@example.com",
        "unclaimed-proof",
        "sender@example.com",
    );
    input.attachments = vec![attachment.clone()];
    assert!(
        matches!(
            pg_inbox::ingest_inbound_on(&f.pg, &input).await,
            Err(InboxError::Denied)
        ),
        "an arbitrary bare Asset ID and caller metadata must not grant attachment access"
    );
    assert_eq!(
        message_count(&f.pg).await,
        0,
        "refusal rolls back Message, Conversation and links"
    );
    input.attachments.clear();
    input.provider_message_id = "empty-message";
    let saved = pg_inbox::ingest_inbound_on(&f.pg, &input).await.unwrap();
    assert!(matches!(
        pg_inbox::link_message_assets_on(
            &f.pg,
            f.firm,
            saved.message_id,
            std::slice::from_ref(&attachment)
        )
        .await,
        Err(InboxError::Denied)
    ));
    assert!(matches!(
        pg_inbox::set_attachment_asset_on(&f.pg, saved.message_id, asset).await,
        Err(InboxError::Denied)
    ));
    assert!(pg_inbox::message_assets_on(&f.pg, f.firm, saved.message_id)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn trusted_clean_bytes_claims_are_firm_scoped_and_do_not_bypass_exact_project() {
    let Some(f) = fixture().await else {
        return;
    };
    ensure_receipt_firm_entity(&f).await;
    let directory = tempfile::tempdir().unwrap();
    let storage: Arc<dyn cloud::StorageService> =
        Arc::new(cloud::FsStorage::new(directory.path()).await.unwrap());
    let first = crate::inbox::ingest_attachment_on(
        Some(&f.pg),
        &f.db,
        &storage,
        f.firm,
        None,
        "first.txt",
        "text/plain",
        b"shared synthetic bytes",
    )
    .await
    .unwrap();
    let repeat = crate::inbox::ingest_attachment_on(
        Some(&f.pg),
        &f.db,
        &storage,
        f.firm,
        None,
        "repeat.txt",
        "text/plain",
        b"shared synthetic bytes",
    )
    .await
    .unwrap();
    assert_eq!(first.asset_id, repeat.asset_id);
    let other = crate::firms::create(
        &f.db,
        &NewFirm {
            name: "Second synthetic Firm".into(),
            status: "active".into(),
            entity_id: seed_entity(&f.db).await,
            admin_dri_person_id: f.owner,
        },
    )
    .await
    .unwrap();
    let entity = other.entity_id.unwrap();
    sqlx::query("INSERT INTO entity(id,name,entity_type_id,jurisdiction_id) SELECT $1,'Second synthetic Source Firm',entity_type_id,jurisdiction_id FROM entity WHERE id=$2 ON CONFLICT(id) DO NOTHING").bind(entity).bind(f.entity).execute(f.pg.pool()).await.unwrap();
    let independent = crate::inbox::ingest_attachment_on(
        Some(&f.pg),
        &f.db,
        &storage,
        other.id,
        None,
        "independent.txt",
        "text/plain",
        b"shared synthetic bytes",
    )
    .await
    .unwrap();
    assert_eq!(
        independent.asset_id, first.asset_id,
        "global bytes dedup remains legitimate across Firms"
    );
    assert_independent_firm_receipts(&f, first.asset_id).await;
    let mut input = inbound(
        f.firm,
        channel::EMAIL,
        "receipt@example.com",
        "received-proof",
        "sender@example.com",
    );
    input.attachments = vec![first.clone()];
    let saved = pg_inbox::ingest_inbound_on(&f.pg, &input).await.unwrap();
    assert_eq!(
        pg_inbox::message_assets_on(&f.pg, f.firm, saved.message_id)
            .await
            .unwrap()
            .len(),
        1
    );
    input.provider_message_id = "wrong-project-proof";
    input.project_id = Some(f.project);
    assert!(
        matches!(
            pg_inbox::ingest_inbound_on(&f.pg, &input).await,
            Err(InboxError::Denied)
        ),
        "a staging receipt cannot authorize a bare Asset in an attributed Project"
    );
    assert_eq!(message_count(&f.pg).await, 1);
    let wrong_hash = crate::inbox::IngestAttachment {
        sha256: crate::assets::sha256_hex(b"other bytes"),
        ..first
    };
    assert!(matches!(
        pg_inbox::link_message_assets_on(&f.pg, f.firm, saved.message_id, &[wrong_hash]).await,
        Err(InboxError::Denied)
    ));
}

async fn assert_independent_firm_receipts(f: &Fixture, asset_id: Uuid) {
    let firms: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM inbox_attachment_receipt WHERE asset_id=$1")
            .bind(asset_id)
            .fetch_one(f.pg.pool())
            .await
            .unwrap();
    assert_eq!(
        firms, 2,
        "each Firm has an independent durable byte receipt"
    );

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM inbox_attachment_receipt WHERE firm_id=$1 AND asset_id=$2",
    )
    .bind(f.firm)
    .bind(asset_id)
    .fetch_one(f.pg.pool())
    .await
    .unwrap();
    assert_eq!(count, 1, "retry preserves one durable Firm receipt");
}

async fn ensure_receipt_firm_entity(f: &Fixture) {
    let entity = crate::firms::find_by_id(&f.db, f.firm)
        .await
        .unwrap()
        .unwrap()
        .entity_id
        .unwrap();
    sqlx::query("INSERT INTO entity(id,name,entity_type_id,jurisdiction_id) SELECT $1,'Synthetic Source Firm',entity_type_id,jurisdiction_id FROM entity WHERE id=$2 ON CONFLICT(id) DO NOTHING")
        .bind(entity).bind(f.entity).execute(f.pg.pool()).await.unwrap();
}

#[tokio::test]
async fn native_triage_public_doors_preserve_canonical_inbox_authority() {
    const CHILD: &str = "NAVIGATOR_TEST_NATIVE_TRIAGE_CHILD";
    let Ok(case) = std::env::var(CHILD) else {
        if crate::postgres_mirror::tests::test_postgres().is_none() {
            return;
        }
        let name = format!(
            "{}::native_triage_public_doors_preserve_canonical_inbox_authority",
            module_path!().split_once("::").unwrap().1
        );
        for case in ["brand", "assignment", "mirror"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &name, "--nocapture"])
                .env(CHILD, case)
                .env(pg_inbox::AUTHORITY_ENV, "0")
                .env_remove("NAVIGATOR_POSTGRES_MIRROR")
                .output()
                .unwrap();
            assert!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "native triage {case}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return;
    };
    let Some(f) = fixture().await else { return };
    configure_native_triage_fixture(&f).await;
    let mut input = inbound(
        f.firm,
        channel::EMAIL,
        "triage@example.test",
        "triage-a",
        "a@example.test",
    );
    input.brand_key = Some("native-a");
    let one = pg_inbox::ingest_inbound_on(&f.pg, &input).await.unwrap();
    input.brand_key = Some("native-b");
    input.provider_message_id = "triage-b";
    input.external_address = "b@example.test";
    let two = pg_inbox::ingest_inbound_on(&f.pg, &input).await.unwrap();
    match case.as_str() {
        "brand" => {
            let rows = crate::inbox::list_intake_in_brand(&f.db, f.owner, "native-a")
                .await
                .unwrap();
            assert_eq!(
                rows.len(),
                1,
                "Brand filtering must read canonical native Inbox rows"
            );
            assert_eq!(rows[0].id, one.conversation_id);
            assert_ne!(rows[0].id, two.conversation_id);
            assert_eq!(
                crate::inbox::intake_brands(&f.db, f.owner).await.unwrap(),
                vec!["native-a", "native-b"]
            );
        }
        "assignment" => native_triage_admin_assignment(&f, one).await,
        "mirror" => native_triage_mirror_preserves_native_parent(&f, one).await,
        _ => panic!("unknown isolated native triage test case"),
    }
    let mut source = f.db.query("SELECT * FROM inbox_conversation; SELECT * FROM message; SELECT * FROM inbox_assignment").await.unwrap();
    for index in 0..3 {
        let rows: Vec<serde_json::Value> = source.take(index).unwrap();
        assert!(
            rows.is_empty(),
            "native triage must not create a stale Source Inbox shadow"
        );
    }
}

async fn configure_native_triage_fixture(f: &Fixture) {
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(f.pg.pool())
        .await
        .unwrap();
    for (key, value) in [
        ("NAVIGATOR_POSTGRES_HOST", "127.0.0.1".to_string()),
        (
            "NAVIGATOR_POSTGRES_PORT",
            std::env::var("NAVIGATOR_TEST_POSTGRES_PORT").unwrap(),
        ),
        ("NAVIGATOR_POSTGRES_USER", "postgres".to_string()),
        ("NAVIGATOR_POSTGRES_PASSWORD", "synthetic".to_string()),
        ("NAVIGATOR_POSTGRES_DATABASE", database),
        ("NAVIGATOR_POSTGRES_SSLMODE", "disable".to_string()),
        (pg_inbox::AUTHORITY_ENV, "1".to_string()),
    ] {
        std::env::set_var(key, value);
    }
}

async fn record_native_triage(f: &Fixture, message_id: Uuid) -> crate::inbox_triage::InboxTriage {
    use crate::inbox_triage::{NewInboxTriage, PrivilegeLabel, TriageDisposition};
    crate::inbox_triage::record(
        &f.db,
        &NewInboxTriage {
            message_id,
            firm_id: f.firm,
            suggested_project_id: Some(f.project),
            project_confidence: None,
            privilege: PrivilegeLabel::Uncertain,
            privilege_confidence: None,
            rationale: None,
            model_id: None,
            prompt_version: crate::inbox_triage::RULES_ONLY_PROMPT.into(),
            raw_output: None,
            disposition: TriageDisposition::NeedsHuman,
            labeled_example: false,
        },
    )
    .await
    .unwrap()
}

async fn native_triage_admin_assignment(f: &Fixture, one: crate::inbox::IngestedInbound) {
    let admin = persons::create(
        &f.db,
        &NewPerson::with_role("Triage Admin", "triage-admin@example.test", Role::Admin),
    )
    .await
    .unwrap();
    firms::add_membership(
        &f.db,
        &firms::NewPersonFirmRole {
            person_id: admin.id,
            firm_id: f.firm,
            membership: firms::FirmMembership::Admin,
            is_dri: false,
        },
    )
    .await
    .unwrap();
    // Match fixture()'s canonical identity rows for the additional actor.
    sqlx::query("INSERT INTO person (id,name,email,role) VALUES ($1,'Triage Admin','triage-admin@example.test','admin')")
        .bind(admin.id).execute(f.pg.pool()).await.unwrap();
    assert!(matches!(
        crate::inbox::assign(&f.db, admin.id, one.conversation_id, f.project).await,
        Err(InboxError::Denied)
    ));
    let triage = record_native_triage(f, one.message_id).await;
    crate::inbox_triage::confirm_project(&f.db, admin.id, triage.id, f.project)
        .await
        .unwrap();
    assert_eq!(
        pg_inbox::conversation_by_id_on(&f.pg, one.conversation_id)
            .await
            .unwrap()
            .unwrap()
            .project_id,
        Some(f.project)
    );
    assert_eq!(
        pg_inbox::assignments_on(&f.pg, one.conversation_id)
            .await
            .unwrap()
            .len(),
        1
    );
}

async fn native_triage_mirror_preserves_native_parent(
    f: &Fixture,
    one: crate::inbox::IngestedInbound,
) {
    use crate::postgres_mirror::{mirror, MirrorError, MirrorKey};
    seed_triage_mirror_catalogs(f).await;
    let triage = record_native_triage(f, one.message_id).await;
    Box::pin(mirror(&f.db, &f.pg, &MirrorKey::InboxTriage(triage.id)))
        .await
        .unwrap();
    let firm: Uuid = sqlx::query_scalar("SELECT firm_id FROM inbox_triage WHERE id=$1")
        .bind(triage.id)
        .fetch_one(f.pg.pool())
        .await
        .unwrap();
    assert_eq!(firm, f.firm);
    f.db.query("UPDATE $id SET firm_id=$firm")
        .bind(("id", crate::surreal::record_id("inbox_triage", triage.id)))
        .bind(("firm", crate::surreal::record_id("firm", Uuid::now_v7())))
        .await
        .unwrap()
        .check()
        .unwrap();
    assert!(
        Box::pin(mirror(&f.db, &f.pg, &MirrorKey::InboxTriage(triage.id)))
            .await
            .is_err()
    );
    let firm: Uuid = sqlx::query_scalar("SELECT firm_id FROM inbox_triage WHERE id=$1")
        .bind(triage.id)
        .fetch_one(f.pg.pool())
        .await
        .unwrap();
    assert_eq!(
        firm, f.firm,
        "forged Source metadata cannot replace canonical Firm evidence"
    );
    assert!(matches!(
        Box::pin(mirror(
            &f.db,
            &f.pg,
            &MirrorKey::InboxMessage(one.message_id)
        ))
        .await,
        Err(MirrorError::InboxAuthority)
    ));
    assert!(crate::postgres_mirror::inbox::reconcile(&f.db, &f.pg)
        .await
        .is_err());
    std::env::set_var(pg_inbox::AUTHORITY_ENV, "invalid-native-authority");
    assert!(matches!(
        Box::pin(mirror(&f.db, &f.pg, &MirrorKey::InboxTriage(triage.id))).await,
        Err(MirrorError::InboxAuthority)
    ));
}

async fn seed_triage_mirror_catalogs(f: &Fixture) {
    use crate::postgres_mirror::{mirror, MirrorKey, Mirrored};
    use crate::test_support::{SEED_ENTITY_JURISDICTION_ID, SEED_ENTITY_TYPE_ID};
    // seed_entity() deliberately uses unresolved catalog links. This case
    // actually projects its parents, so provide those two Source rows.
    f.db.query("CREATE $entity_type SET name='Triage Synthetic Type'; CREATE $jurisdiction SET name='Triage Synthetic Jurisdiction', code='ZZ', jurisdiction_type='state'")
        .bind(("entity_type", crate::surreal::record_id("entity_type", SEED_ENTITY_TYPE_ID)))
        .bind(("jurisdiction", crate::surreal::record_id("jurisdiction", SEED_ENTITY_JURISDICTION_ID)))
        .await.unwrap().check().unwrap();
    for key in [
        MirrorKey::EntityType(SEED_ENTITY_TYPE_ID),
        MirrorKey::Jurisdiction(SEED_ENTITY_JURISDICTION_ID),
    ] {
        assert_ne!(
            Box::pin(mirror(&f.db, &f.pg, &key)).await.unwrap(),
            Mirrored::Deleted
        );
    }
}

#[tokio::test]
async fn existing_x_rows_survive_the_linkedin_import_migration() {
    let Some(f) = fixture().await else { return };
    let input = inbound(
        f.firm,
        channel::X,
        "synthetic-x-account",
        "x-before-linkedin",
        "synthetic-x-sender",
    );
    let before = pg_inbox::ingest_inbound_on(&f.pg, &input).await.unwrap();
    // Model the deployed X schema before the new import column exists.
    sqlx::query("ALTER TABLE message DROP COLUMN raw_storage_key")
        .execute(f.pg.pool())
        .await
        .unwrap();
    sqlx::raw_sql(include_str!(
        "../../postgres/migrations/20261102020000_linkedin_channel_and_raw_storage.sql"
    ))
    .execute(f.pg.pool())
    .await
    .expect("the import migration must preserve existing X history");
    let stored = pg_inbox::conversation_by_id_on(&f.pg, before.conversation_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.channel, channel::X);
    let mut linkedin = input;
    linkedin.channel = channel::LINKEDIN;
    linkedin.provider_message_id = "linkedin-after-migration";
    linkedin.provider_account = "synthetic-linkedin-account";
    linkedin.external_address = "synthetic-linkedin-sender";
    let imported = pg_inbox::ingest_inbound_on(&f.pg, &linkedin).await.unwrap();
    assert!(pg_inbox::message_by_id_on(&f.pg, imported.message_id)
        .await
        .unwrap()
        .unwrap()
        .raw_storage_key
        .is_none());
}

#[tokio::test]
async fn native_voice_ingest_deduplicates_and_projects_to_the_call_spine() {
    let Some(f) = fixture().await else { return };
    let input = inbound(
        f.firm,
        channel::VOICE,
        "+15555550123",
        "CAnative-voice",
        "+15555550456",
    );
    let first = pg_inbox::ingest_inbound_on(&f.pg, &input).await.unwrap();
    let repeated = pg_inbox::ingest_inbound_on(&f.pg, &input).await.unwrap();
    assert_eq!(first.message_id, repeated.message_id);
    pg_inbox::assign_on(&f.pg, &f.db, f.owner, first.conversation_id, f.project)
        .await
        .unwrap();
    let source_ref = format!("inbox:{}", first.message_id);
    let projected = crate::communications::find_by_source(
        &f.db,
        crate::communications::channel::CALL,
        &source_ref,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(projected.project_id, f.project);
}

fn fi_mapping() -> crate::fi_import::Mapping {
    serde_json::from_value(serde_json::json!({
        "columns":{"kind":"type","direction":"way","counterparty":"phone","timestamp":["at"],"duration":"length","identity":"id","status":null},
        "kinds":{"C":"call","S":"sms"},"directions":{"I":"inbound","O":"outbound"},
        "timestamp_format":"%+","timestamp_separator":" ","timezone":{"mode":"embedded_offset"},"duration_unit":"seconds"
    })).unwrap()
}

async fn fi_scope(f: &Fixture) -> crate::fi_import::Scope {
    sqlx::query("INSERT INTO brand(name,brand_key,firm_id) VALUES('Fi Synthetic','fi-synthetic',$1) ON CONFLICT(brand_key) DO NOTHING")
        .bind(f.firm).execute(f.pg.pool()).await.unwrap();
    crate::fi_import::Scope {
        actor: f.owner,
        firm: f.firm,
        brand: "fi-synthetic".into(),
        account: crate::fi_import::account("+15555550123").unwrap(),
        project: None,
    }
}

fn fi_rows(rows: &str) -> Vec<crate::fi_import::Row> {
    let Ok(parsed) = crate::fi_import::parse(
        &fi_mapping(),
        "+15555550123",
        format!("type,way,phone,at,length,id\n{rows}\n").as_bytes(),
    ) else {
        panic!("the synthetic Fi CSV fixture must parse");
    };
    parsed
}

#[tokio::test]
async fn fi_history_is_native_idempotent_metadata_and_cannot_queue_or_dispatch() {
    let Some(f) = fixture().await else { return };
    let scope = fi_scope(&f).await;
    let rows=fi_rows("S,O,+15555550456,2026-10-30T18:00:00Z,,sms-one\nC,I,+15555550789,2026-10-30T14:01:00.123456-04:00,61,call-one");
    let mut saved = Vec::new();
    for row in &rows {
        assert_eq!(
            crate::fi_import::preview_on(&f.pg, &scope, row)
                .await
                .unwrap(),
            crate::fi_import::Existing::New
        );
        let first = crate::fi_import::ingest_on(&f.pg, &scope, row)
            .await
            .unwrap();
        let replay = crate::fi_import::ingest_on(&f.pg, &scope, row)
            .await
            .unwrap();
        assert_eq!(first.message_id, replay.message_id);
        assert!(replay.deduped);
        let message = pg_inbox::message_by_id_on(&f.pg, first.message_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.delivery_state, delivery::RECEIVED);
        assert!(message.raw_storage_key.is_none());
        assert!(message.attachment_asset_id.is_none());
        assert_eq!(message.direction, row.metadata.direction.as_str());
        assert!(
            message.author_person_id.is_none(),
            "direction does not prove a historical human author"
        );
        pg_inbox::assign_on(&f.pg, &f.db, f.owner, first.conversation_id, f.project)
            .await
            .unwrap();
        let view =
            pg_inbox::project_thread_on(&f.pg, &f.db, f.lawyer, f.project, first.conversation_id)
                .await
                .unwrap();
        assert!(!view.can_send);
        assert!(view.can_note);
        assert!(matches!(
            pg_inbox::queue_reply_on(
                &f.pg,
                &f.db,
                f.lawyer,
                first.conversation_id,
                "must not send",
                purpose::MATTER
            )
            .await,
            Err(InboxError::Denied)
        ));
        saved.push(first);
    }
    assert!(matches!(
        pg_inbox::queue_sms_reply_on(
            &f.pg,
            &f.db,
            f.lawyer,
            saved[0].conversation_id,
            "must not send"
        )
        .await,
        Err(InboxError::Denied)
    ));
    let report = pg_inbox::dispatch_queued_on(&f.pg, &f.db, &NoTransport)
        .await
        .unwrap();
    assert_eq!(report.submitted, 0);
    // Even an accidentally queued historical row cannot escape the source guard.
    sqlx::query("UPDATE message SET delivery_state='queued',author_person_id=$2 WHERE id=$1")
        .bind(saved[0].message_id)
        .bind(f.lawyer)
        .execute(f.pg.pool())
        .await
        .unwrap();
    let report = pg_inbox::dispatch_queued_on(&f.pg, &f.db, &NoTransport)
        .await
        .unwrap();
    assert_eq!(report.suppressed, 1);
    let bills: i64 = sqlx::query_scalar("SELECT count(*) FROM billed_call")
        .fetch_one(f.pg.pool())
        .await
        .unwrap();
    assert_eq!(bills, 0);
    let links: i64 = sqlx::query_scalar("SELECT count(*) FROM inbox_message_asset")
        .fetch_one(f.pg.pool())
        .await
        .unwrap();
    assert_eq!(links, 0);
}

#[tokio::test]
async fn raced_fi_native_id_payloads_cannot_split_across_channels_or_replace_the_winner() {
    let Some(f) = fixture().await else { return };
    let scope = fi_scope(&f).await;
    let Some(first) = fi_rows("C,I,+15555550456,2026-10-30T18:00:00Z,61,same-native-id")
        .into_iter()
        .next()
    else {
        panic!("the synthetic call fixture must contain a row");
    };
    let Some(second) = fi_rows("S,O,+15555550456,2026-10-30T18:00:00Z,,same-native-id")
        .into_iter()
        .next()
    else {
        panic!("the synthetic SMS fixture must contain a row");
    };
    assert!(
        first.provider_message_id == second.provider_message_id,
        "the native ID must identify the same imported event across channels"
    );
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(8));
    let mut tasks = Vec::new();
    for index in 0..8 {
        let pg = f.pg.clone();
        let scope = scope.clone();
        let barrier = barrier.clone();
        let row = if index % 2 == 0 {
            first.clone()
        } else {
            second.clone()
        };
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            crate::fi_import::ingest_on(&pg, &scope, &row).await
        }));
    }
    let mut successes = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(_) => successes += 1,
            Err(
                crate::fi_import::FiImportError::Busy | crate::fi_import::FiImportError::Conflict,
            ) => {}
            Err(error) => panic!("unexpected ingest error: {error}"),
        }
    }
    assert!(successes > 0);
    let a = crate::fi_import::preview_on(&f.pg, &scope, &first)
        .await
        .unwrap();
    let b = crate::fi_import::preview_on(&f.pg, &scope, &second)
        .await
        .unwrap();
    assert!(matches!(
        (a, b),
        (
            crate::fi_import::Existing::Identical,
            crate::fi_import::Existing::Conflict
        ) | (
            crate::fi_import::Existing::Conflict,
            crate::fi_import::Existing::Identical
        )
    ));
    let losing = if a == crate::fi_import::Existing::Conflict {
        &first
    } else {
        &second
    };
    assert_eq!(
        crate::fi_import::ingest_on(&f.pg, &scope, losing)
            .await
            .unwrap_err(),
        crate::fi_import::FiImportError::Conflict
    );
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM inbox_ingress WHERE firm_id=$1 AND provider_account=$2 AND provider_message_id=$3")
        .bind(f.firm).bind(&scope.account).bind(&first.provider_message_id).fetch_one(f.pg.pool()).await.unwrap();
    assert_eq!(count, 1);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM message")
        .fetch_one(f.pg.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn identical_fi_replays_preserve_reviewed_history_and_new_events_reopen_it() {
    let Some(f) = fixture().await else { return };
    for kind in ["C", "S"] {
        let mut scope = fi_scope(&f).await;
        let duration = if kind == "C" { "61" } else { "" };
        let rows = fi_rows(&format!("{kind},I,+15555550456,2026-10-30T18:00:00Z,{duration},read-{kind}-one\n{kind},I,+15555550456,2026-10-30T18:01:00Z,{duration},read-{kind}-two"));
        let saved = crate::fi_import::ingest_on(&f.pg, &scope, &rows[0])
            .await
            .unwrap();
        for project in [None, Some(f.project)] {
            scope.project = project;
            if project.is_some() {
                crate::fi_import::ingest_on(&f.pg, &scope, &rows[0])
                    .await
                    .unwrap();
                let assigned = pg_inbox::conversation_by_id_on(&f.pg, saved.conversation_id)
                    .await
                    .unwrap()
                    .unwrap();
                assert!(assigned.unread, "new Project attribution requires review");
                assert_eq!(assigned.project_id, project);
            }
            sqlx::query("UPDATE inbox_conversation SET unread=false,updated_at=now() WHERE id=$1")
                .bind(saved.conversation_id)
                .execute(f.pg.pool())
                .await
                .unwrap();
            let before = pg_inbox::conversation_by_id_on(&f.pg, saved.conversation_id)
                .await
                .unwrap()
                .unwrap();
            let replay = crate::fi_import::ingest_on(&f.pg, &scope, &rows[0])
                .await
                .unwrap();
            assert!(replay.deduped);
            assert_eq!(replay.message_id, saved.message_id);
            if let Some(project) = project {
                pg_inbox::assign_on(&f.pg, &f.db, f.owner, saved.conversation_id, project)
                    .await
                    .unwrap();
            }
            let after = pg_inbox::conversation_by_id_on(&f.pg, saved.conversation_id)
                .await
                .unwrap()
                .unwrap();
            assert!(
                !after.unread,
                "identical replay must preserve reviewed history"
            );
            assert_eq!(
                after.updated_at, before.updated_at,
                "identical replay must preserve Inbox ordering"
            );
        }
        let new_event = crate::fi_import::ingest_on(&f.pg, &scope, &rows[1])
            .await
            .unwrap();
        assert!(!new_event.deduped);
        assert_eq!(new_event.conversation_id, saved.conversation_id);
        let changed = pg_inbox::conversation_by_id_on(&f.pg, saved.conversation_id)
            .await
            .unwrap()
            .unwrap();
        assert!(
            changed.unread,
            "a new event must reopen its thread for review"
        );
    }
}

#[tokio::test]
async fn fi_import_lock_prevents_assignment_until_destination_verification_finishes() {
    let Some(f) = fixture().await else { return };
    let mut scope = fi_scope(&f).await;
    let rows = fi_rows("S,I,+15555550456,2026-10-30T18:00:00Z,,before-assignment\nS,I,+15555550456,2026-10-30T18:01:00Z,,during-import");
    let first = crate::fi_import::ingest_on(&f.pg, &scope, &rows[0])
        .await
        .unwrap();
    scope.project = Some(f.project);
    let mut import_guard = f.pg.pool().begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('google_fi:import:v1',168))")
        .execute(&mut *import_guard)
        .await
        .unwrap();
    for result in [
        pg_inbox::assign_on(&f.pg, &f.db, f.owner, first.conversation_id, f.project).await,
        pg_inbox::assign_for_triage_review_on(
            &f.pg,
            &f.db,
            f.owner,
            first.conversation_id,
            f.project,
        )
        .await,
    ] {
        assert!(
            matches!(result, Err(InboxError::Invalid(_))),
            "assignment must yield while an import is checking its destination"
        );
    }
    let conversation = pg_inbox::conversation_by_id_on(&f.pg, first.conversation_id)
        .await
        .unwrap()
        .unwrap();
    assert!(conversation.project_id.is_none());
    import_guard.rollback().await.unwrap();
    let saved = crate::fi_import::ingest_on(&f.pg, &scope, &rows[1])
        .await
        .unwrap();
    assert_eq!(saved.conversation_id, first.conversation_id);
    pg_inbox::assign_on(&f.pg, &f.db, f.owner, saved.conversation_id, f.project)
        .await
        .unwrap();
    assert_eq!(
        crate::fi_import::preview_on(&f.pg, &scope, &rows[1])
            .await
            .unwrap(),
        crate::fi_import::Existing::Identical
    );
}

#[tokio::test]
async fn native_relay_requires_a_current_person_eligible_role_and_native_thread() {
    let Some(f) = fixture().await else { return };
    let missing_thread = Uuid::now_v7();
    assert!(
        !pg_inbox::clerk_may_relay_email_thread_on(&f.pg, &f.db, Uuid::now_v7(), missing_thread)
            .await
            .unwrap(),
        "a missing Person cannot obtain relay authority"
    );
    assert!(
        !pg_inbox::clerk_may_relay_email_thread_on(&f.pg, &f.db, f.owner, missing_thread)
            .await
            .unwrap(),
        "Owner role alone is not Lawyer or Clerk relay participation"
    );
    assert!(
        !pg_inbox::clerk_may_relay_email_thread_on(&f.pg, &f.db, f.lawyer, missing_thread)
            .await
            .unwrap(),
        "Project participation cannot manufacture a native email thread"
    );
    assert_eq!(
        conversation_count(&f.pg).await,
        0,
        "denied reads cannot create a thread"
    );
}

/// The review door uses its own canonical Admin/participation admission, but
/// cannot bypass initial call attribution or leave a completed bill parked.
async fn assert_public_native_call_and_fi_triage_coordination(f: &Fixture) {
    let admin = persons::create(
        &f.db,
        &NewPerson::with_role(
            "Review Admin",
            "voice-review-admin@example.test",
            Role::Admin,
        ),
    )
    .await
    .unwrap()
    .id;
    sqlx::query("INSERT INTO person (id,name,email,role) VALUES ($1,'Review Admin','voice-review-admin@example.test','admin')")
        .bind(admin).execute(f.pg.pool()).await.unwrap();
    firms::ensure_membership(
        &f.db,
        &firms::NewPersonFirmRole {
            person_id: admin,
            firm_id: f.firm,
            membership: firms::FirmMembership::Admin,
            is_dri: false,
        },
    )
    .await
    .unwrap();
    let outsider = persons::create(
        &f.db,
        &NewPerson::with_role(
            "Other Admin",
            "voice-review-other@example.test",
            Role::Admin,
        ),
    )
    .await
    .unwrap()
    .id;
    let other = seed_project(
        &f.pg,
        &f.db,
        f.entity,
        f.firm,
        "voice-review-other",
        "Other review matter",
    )
    .await;
    assert_native_voice_review_attribution(f, admin, outsider, other).await;
    assert_native_fi_review_attribution(f, admin, other).await;
    assert_native_live_sms_review_moves(f, admin, other).await;
}

async fn assert_native_voice_review_attribution(
    f: &Fixture,
    admin: Uuid,
    outsider: Uuid,
    other: Uuid,
) {
    use crate::billed_calls::{self, NewBilledCall};
    let voice = inbound(
        f.firm,
        channel::VOICE,
        "+15555550144",
        "CAreview-native",
        "+15555550145",
    );
    let saved = crate::inbox::ingest_inbound(&f.db, &voice).await.unwrap();
    let input = NewBilledCall {
        call_sid: "CAreview-native".into(),
        conversation_id: saved.conversation_id,
        firm_id: f.firm,
        duration_seconds: 361,
        call_started_at: "2026-10-30T18:00:00Z".parse().unwrap(),
    };
    let before = billed_calls::record(&f.pg, &input).await.unwrap();
    assert!(matches!(
        crate::inbox::assign_for_triage_review(&f.db, outsider, saved.conversation_id, f.project)
            .await,
        Err(InboxError::Denied)
    ));
    assert!(billed_calls::find_by_call_sid(&f.pg, &input.call_sid)
        .await
        .unwrap()
        .unwrap()
        .project_id
        .is_none());
    let first =
        crate::inbox::assign_for_triage_review(&f.db, admin, saved.conversation_id, f.project)
            .await
            .unwrap();
    let again =
        crate::inbox::assign_for_triage_review(&f.db, admin, saved.conversation_id, f.project)
            .await
            .unwrap();
    assert_eq!(first.id, again.id);
    assert!(matches!(
        crate::inbox::assign_for_triage_review(&f.db, admin, saved.conversation_id, other).await,
        Err(InboxError::Denied)
    ));
    let bill = billed_calls::find_by_call_sid(&f.pg, &input.call_sid)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bill.project_id, Some(f.project));
    assert_eq!(
        (bill.duration_seconds, bill.tenths, bill.call_started_at),
        (
            before.duration_seconds,
            before.tenths,
            before.call_started_at
        )
    );
    assert_eq!(
        crate::inbox::conversation_by_id(&f.db, saved.conversation_id)
            .await
            .unwrap()
            .unwrap()
            .project_id,
        Some(f.project)
    );
}

async fn assert_native_fi_review_attribution(f: &Fixture, admin: Uuid, other: Uuid) {
    let scope = fi_scope(f).await;
    let rows = fi_rows("S,O,+15555550456,2026-10-30T18:00:00Z,,review-fi-sms");
    let history = crate::fi_import::ingest_on(&f.pg, &scope, &rows[0])
        .await
        .unwrap();
    let assignment =
        crate::inbox::assign_for_triage_review(&f.db, admin, history.conversation_id, f.project)
            .await
            .unwrap();
    assert_eq!(
        crate::inbox::assign_for_triage_review(&f.db, admin, history.conversation_id, f.project)
            .await
            .unwrap()
            .id,
        assignment.id
    );
    assert!(matches!(
        crate::inbox::assign_for_triage_review(&f.db, admin, history.conversation_id, other).await,
        Err(InboxError::Denied)
    ));
    let bills: i64 =
        sqlx::query_scalar("SELECT count(*) FROM billed_call WHERE conversation_id=$1")
            .bind(history.conversation_id)
            .fetch_one(f.pg.pool())
            .await
            .unwrap();
    assert_eq!(
        bills, 0,
        "Fi history never fabricates billable provider calls"
    );
    assert!(crate::inbox::message_by_id(&f.db, history.message_id)
        .await
        .unwrap()
        .unwrap()
        .author_person_id
        .is_none());
}

async fn assert_native_live_sms_review_moves(f: &Fixture, admin: Uuid, other: Uuid) {
    // Candidate behavior remains unchanged for ordinary live SMS review moves.
    let sms = inbound(
        f.firm,
        channel::SMS,
        "+15555550144",
        "SMreview-live",
        "+15555550146",
    );
    let live = crate::inbox::ingest_inbound(&f.db, &sms).await.unwrap();
    crate::inbox::assign_for_triage_review(&f.db, admin, live.conversation_id, f.project)
        .await
        .unwrap();
    crate::inbox::assign_for_triage_review(&f.db, admin, live.conversation_id, other)
        .await
        .unwrap();
    assert_eq!(
        crate::inbox::conversation_by_id(&f.db, live.conversation_id)
            .await
            .unwrap()
            .unwrap()
            .project_id,
        Some(other)
    );
}

async fn voice_reply_fixture() -> Option<(Fixture, Uuid, Uuid)> {
    let f = fixture().await?;
    let clerk = persons::create(
        &f.db,
        &NewPerson::with_role("Voice Clerk", "voice-clerk@example.com", Role::Clerk),
    )
    .await
    .unwrap()
    .id;
    projects::add_participation(
        &f.db,
        f.project,
        clerk,
        projects::participation_for_role(Role::Clerk),
    )
    .await
    .unwrap();
    sqlx::query("INSERT INTO person(id,name,email,role) VALUES($1,'Voice Clerk','voice-clerk@example.com','clerk')").bind(clerk).execute(f.pg.pool()).await.unwrap();
    let mut input = inbound(
        f.firm,
        channel::VOICE,
        "+12025550100",
        "CAvoice-reply-guard",
        "+12025550101",
    );
    input.project_id = Some(f.project);
    let saved = pg_inbox::ingest_inbound_on(&f.pg, &input).await.unwrap();
    Some((f, clerk, saved.conversation_id))
}

#[tokio::test]
async fn native_voice_threads_allow_internal_notes_without_offering_replies() {
    let Some((f, clerk, conversation)) = voice_reply_fixture().await else {
        return;
    };
    for actor in [f.lawyer, clerk] {
        let view = pg_inbox::project_thread_on(&f.pg, &f.db, actor, f.project, conversation)
            .await
            .unwrap();
        assert!(view.can_note, "participating firm people may note a call");
        assert!(!view.can_send, "VOICE has no outbound reply transport");
        assert_eq!(view.next_action, "Leave an internal note");
        let rows = pg_inbox::list_project_on(&f.pg, &f.db, actor, f.project)
            .await
            .unwrap();
        assert_eq!(
            rows.iter()
                .find(|r| r.id == conversation)
                .unwrap()
                .next_action,
            "Leave an internal note"
        );
        let note = pg_inbox::post_note_on(
            &f.pg,
            &f.db,
            actor,
            conversation,
            "Synthetic call follow-up",
        )
        .await
        .unwrap();
        assert_eq!(
            pg_inbox::message_by_id_on(&f.pg, note)
                .await
                .unwrap()
                .unwrap()
                .direction,
            direction::INTERNAL
        );
    }
}

#[tokio::test]
async fn native_voice_reply_refusal_creates_no_pending_outbound_message() {
    let Some((f, clerk, conversation)) = voice_reply_fixture().await else {
        return;
    };
    for actor in [f.lawyer, clerk] {
        assert!(
            matches!(
                pg_inbox::queue_reply_on(
                    &f.pg,
                    &f.db,
                    actor,
                    conversation,
                    "Synthetic reply",
                    purpose::MATTER
                )
                .await,
                Err(InboxError::Denied)
            ),
            "VOICE replies must be refused before queue creation"
        );
    }
    let outbound: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM message WHERE conversation_id=$1 AND direction='outbound'",
    )
    .bind(conversation)
    .fetch_one(f.pg.pool())
    .await
    .unwrap();
    assert_eq!(
        outbound, 0,
        "refused call replies leave no pending outbound Messages"
    );
}

#[tokio::test]
async fn native_existing_voice_outbound_rows_never_reach_the_transport() {
    let Some((f, _, conversation)) = voice_reply_fixture().await else {
        return;
    };
    let mut ids = Vec::new();
    for state in [delivery::FAILED, delivery::QUEUED] {
        let id = Uuid::now_v7();
        sqlx::query("INSERT INTO message(id,conversation_id,direction,body,author_person_id,delivery_state,purpose,occurred_at,inserted_at,updated_at) VALUES($1,$2,'outbound','Synthetic prior reply',$3,$4,'matter','2026-11-02T00:00:00Z',now(),now())")
            .bind(id).bind(conversation).bind(f.lawyer).bind(state).execute(f.pg.pool()).await.unwrap();
        ids.push(id);
    }
    let report = pg_inbox::dispatch_queued_on(&f.pg, &f.db, &NoTransport)
        .await
        .unwrap();
    assert_eq!(report.suppressed, 1);
    for (id, state) in ids
        .into_iter()
        .zip([delivery::FAILED, delivery::SUPPRESSED])
    {
        assert_eq!(
            pg_inbox::message_by_id_on(&f.pg, id)
                .await
                .unwrap()
                .unwrap()
                .delivery_state,
            state
        );
    }
}

#[tokio::test]
async fn fi_project_destination_is_reserved_before_any_history_commit() {
    const CHILD: &str = "NAVIGATOR_TEST_FI_ATOMIC_DESTINATION_CHILD";
    let Ok(case) = std::env::var(CHILD) else {
        if crate::postgres_mirror::tests::test_postgres().is_none() {
            return;
        }
        let name = format!(
            "{}::fi_project_destination_is_reserved_before_any_history_commit",
            module_path!().split_once("::").unwrap().1
        );
        for case in [
            "new_call",
            "new_sms",
            "replay_call",
            "replay_sms",
            "race_call",
            "race_sms",
            "group_call",
            "group_sms",
        ] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &name, "--nocapture"])
                .env(CHILD, case)
                .env(pg_inbox::AUTHORITY_ENV, "0")
                .env_remove("NAVIGATOR_POSTGRES_MIRROR")
                .output()
                .unwrap();
            assert!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "Fi destination {case}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return;
    };
    let Some(f) = fixture().await else {
        return;
    };
    configure_native_triage_fixture(&f).await;
    let kind = if case.ends_with("sms") { "S" } else { "C" };
    if case.starts_with("group") {
        assert_fi_project_batches_reuse_the_selected_thread(&f, kind).await;
    } else if case.starts_with("race") {
        assert_fi_assignment_cannot_cross_commit(&f, kind).await;
    } else {
        assert_fi_project_is_pinned_before_return(&f, kind, case.starts_with("replay")).await;
    }
}

async fn assert_fi_project_is_pinned_before_return(f: &Fixture, kind: &str, parked: bool) {
    let mut scope = fi_scope(f).await;
    scope.project = Some(f.project);
    let duration = if kind == "C" { "61" } else { "" };
    let row = fi_rows(&format!(
        "{kind},I,+15555550456,2026-10-30T18:00:00Z,{duration},pin-one"
    ))
    .remove(0);
    if parked {
        scope.project = None;
        crate::fi_import::ingest_on(&f.pg, &scope, &row)
            .await
            .unwrap();
        scope.project = Some(f.project);
    }
    let saved = crate::fi_import::ingest_on(&f.pg, &scope, &row)
        .await
        .unwrap();
    let conversation = pg_inbox::conversation_by_id_on(&f.pg, saved.conversation_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        conversation.project_id,
        Some(f.project),
        "requested Project must already be pinned before the API's later attribution call"
    );
    let audit:(Option<Uuid>,Uuid)=sqlx::query_as("SELECT from_project_id,actor_person_id FROM inbox_assignment WHERE conversation_id=$1 AND to_project_id=$2")
        .bind(saved.conversation_id).bind(f.project).fetch_one(f.pg.pool()).await.unwrap();
    assert_eq!(
        audit,
        (None, f.owner),
        "initial attribution keeps its authenticated actor and unassigned origin"
    );
    let replay = crate::fi_import::ingest_on(&f.pg, &scope, &row)
        .await
        .unwrap();
    assert_eq!(saved.message_id, replay.message_id);
    assert!(replay.deduped);
    let other = seed_project(&f.pg, &f.db, f.entity, f.firm, "fi-other", "Fi other").await;
    let mut wrong = scope.clone();
    wrong.project = Some(other);
    assert!(
        matches!(
            crate::fi_import::ingest_on(&f.pg, &wrong, &row).await,
            Err(crate::fi_import::FiImportError::Conflict)
        ),
        "a conflicting replay must leave history and its Project unchanged"
    );
    let mut unscoped = scope.clone();
    unscoped.project = None;
    assert!(
        crate::fi_import::ingest_on(&f.pg, &unscoped, &row)
            .await
            .unwrap()
            .deduped
    );
    assert_eq!(
        pg_inbox::conversation_by_id_on(&f.pg, saved.conversation_id)
            .await
            .unwrap()
            .unwrap()
            .project_id,
        Some(f.project),
        "unscoped replay must not unassign history"
    );
    assert!(
        crate::inbox::assign(&f.db, f.owner, saved.conversation_id, other)
            .await
            .is_err(),
        "a later competing destination cannot move committed Fi history"
    );
    let repaired = crate::inbox::assign(&f.db, f.owner, saved.conversation_id, f.project)
        .await
        .unwrap();
    assert_eq!(repaired.from_project_id, None);
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM inbox_assignment WHERE conversation_id=$1")
            .bind(saved.conversation_id)
            .fetch_one(f.pg.pool())
            .await
            .unwrap();
    assert_eq!(
        count, 1,
        "same-target projection repair reuses the atomic attribution audit"
    );
    assert_fi_history_has_no_author_or_billing(f, &saved).await;
}

async fn assert_fi_history_has_no_author_or_billing(
    f: &Fixture,
    saved: &crate::inbox::IngestedInbound,
) {
    let message = pg_inbox::message_by_id_on(&f.pg, saved.message_id)
        .await
        .unwrap()
        .unwrap();
    assert!(message.author_person_id.is_none());
    assert!(message.raw_storage_key.is_none());
    assert_eq!(message.delivery_state, delivery::RECEIVED);
    let bills: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM billed_call WHERE conversation_id=$1")
            .bind(saved.conversation_id)
            .fetch_one(f.pg.pool())
            .await
            .unwrap();
    assert_eq!(bills, 0);
}

async fn assert_fi_assignment_cannot_cross_commit(f: &Fixture, kind: &str) {
    let mut scope = fi_scope(f).await;
    let duration = if kind == "C" { "61" } else { "" };
    let rows=fi_rows(&format!("{kind},I,+15555550456,2026-10-30T18:00:00Z,{duration},race-first\n{kind},I,+15555550456,2026-10-30T18:01:00Z,{duration},race-second"));
    let first = crate::fi_import::ingest_on(&f.pg, &scope, &rows[0])
        .await
        .unwrap();
    let destination = seed_project(
        &f.pg,
        &f.db,
        f.entity,
        f.firm,
        "fi-destination",
        "Fi destination",
    )
    .await;
    scope.project = Some(destination);
    // Only this synthetic row pauses, after native thread selection and before INSERT commits.
    let trigger=format!("CREATE FUNCTION fi_insert_latch() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.provider_message_id='{}' THEN PERFORM pg_advisory_xact_lock(168765432); END IF; RETURN NEW; END $$; CREATE TRIGGER fi_insert_latch BEFORE INSERT ON message FOR EACH ROW EXECUTE FUNCTION fi_insert_latch();",rows[1].provider_message_id);
    // The interpolated identity is generated by the canonical Fi parser from synthetic fixture CSV.
    sqlx::raw_sql(sqlx::AssertSqlSafe(trigger.as_str()))
        .execute(f.pg.pool())
        .await
        .unwrap();
    let mut blocker = f.pg.pool().begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(168765432)")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let pg = f.pg.clone();
    let row = rows[1].clone();
    let importer =
        tokio::spawn(async move { crate::fi_import::ingest_on(&pg, &scope, &row).await });
    let paused=tokio::time::timeout(std::time::Duration::from_secs(10),async {
        loop {
            let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND database=(SELECT oid FROM pg_database WHERE datname=current_database()) AND classid=0 AND objid=168765432 AND objsubid=1 AND NOT granted)").fetch_one(f.pg.pool()).await.unwrap();
            if waiting{break;}
            tokio::task::yield_now().await;
        }
    }).await;
    if paused.is_err() {
        blocker.rollback().await.unwrap();
        let _ = importer.await;
        panic!("synthetic Fi INSERT did not reach its latch");
    }
    let assigned = crate::billed_calls::assign_conversation(
        &f.pg,
        &f.db,
        f.owner,
        first.conversation_id,
        f.project,
    )
    .await;
    blocker.rollback().await.unwrap();
    let result = importer.await.unwrap();
    sqlx::raw_sql("DROP TRIGGER fi_insert_latch ON message; DROP FUNCTION fi_insert_latch();")
        .execute(f.pg.pool())
        .await
        .unwrap();
    let persisted: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM message WHERE provider_message_id=$1")
            .bind(&rows[1].provider_message_id)
            .fetch_one(f.pg.pool())
            .await
            .unwrap();
    assert!(!(matches!(&result,Err(crate::fi_import::FiImportError::Conflict))&&persisted>0),"Conflict must roll back the current Fi row, not leave hidden history under the competing Project");
    assert!(
        assigned.is_err(),
        "assignment must share the import's Conversation fence"
    );
    let saved = result.unwrap();
    assert_eq!(persisted, 1);
    assert_eq!(saved.conversation_id, first.conversation_id);
    assert_eq!(
        pg_inbox::conversation_by_id_on(&f.pg, saved.conversation_id)
            .await
            .unwrap()
            .unwrap()
            .project_id,
        Some(destination)
    );
}

async fn assert_fi_project_batches_reuse_the_selected_thread(f: &Fixture, kind: &str) {
    let mut scope = fi_scope(f).await;
    scope.project = Some(f.project);
    let duration = if kind == "C" { "61" } else { "" };
    let csv = (0..7)
        .map(|index| {
            format!("{kind},I,+15555550456,2026-10-30T18:0{index}:00Z,{duration},group-{index}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let rows = fi_rows(&csv);
    let first = crate::fi_import::ingest_on(&f.pg, &scope, &rows[0])
        .await
        .unwrap();
    let second = crate::fi_import::ingest_on(&f.pg, &scope, &rows[1])
        .await
        .unwrap();
    assert_eq!(
        second.conversation_id, first.conversation_id,
        "new Fi events for the selected Project, phone and channel must share one thread"
    );
    assert_ne!(second.message_id, first.message_id);
    let replay = crate::fi_import::ingest_on(&f.pg, &scope, &rows[1])
        .await
        .unwrap();
    assert!(replay.deduped);
    assert_eq!(replay.message_id, second.message_id);
    scope.project = None;
    let parked = crate::fi_import::ingest_on(&f.pg, &scope, &rows[2])
        .await
        .unwrap();
    assert_ne!(
        parked.conversation_id, first.conversation_id,
        "unscoped new history cannot join a Project thread"
    );
    let other = seed_project(
        &f.pg,
        &f.db,
        f.entity,
        f.firm,
        "fi-group-other",
        "Fi group other",
    )
    .await;
    scope.project = Some(other);
    let other_saved = crate::fi_import::ingest_on(&f.pg, &scope, &rows[3])
        .await
        .unwrap();
    assert_eq!(
        other_saved.conversation_id, parked.conversation_id,
        "targeted ingestion can still claim an existing parked thread"
    );
    assert_ne!(
        other_saved.conversation_id, first.conversation_id,
        "another Project cannot capture the selected thread"
    );
    scope.project = Some(f.project);
    let third = crate::fi_import::ingest_on(&f.pg, &scope, &rows[4])
        .await
        .unwrap();
    assert_eq!(third.conversation_id, first.conversation_id);
    scope.project = None;
    let still_parked = crate::fi_import::ingest_on(&f.pg, &scope, &rows[5])
        .await
        .unwrap();
    scope.project = Some(f.project);
    let fourth = crate::fi_import::ingest_on(&f.pg, &scope, &rows[6])
        .await
        .unwrap();
    assert_eq!(
        fourth.conversation_id, first.conversation_id,
        "prefer the selected Project over an unrelated parked thread"
    );
    assert_eq!(
        pg_inbox::conversation_by_id_on(&f.pg, still_parked.conversation_id)
            .await
            .unwrap()
            .unwrap()
            .project_id,
        None
    );
    assert_fi_group_audit_and_messages(f, first.conversation_id).await;
}

async fn assert_fi_group_audit_and_messages(f: &Fixture, conversation: Uuid) {
    for _ in 0..2 {
        let assignment = crate::inbox::assign(&f.db, f.owner, conversation, f.project)
            .await
            .unwrap();
        assert_eq!(assignment.from_project_id, None);
        assert_eq!(assignment.actor_person_id, f.owner);
    }
    let audits: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM inbox_assignment WHERE conversation_id=$1")
            .bind(conversation)
            .fetch_one(f.pg.pool())
            .await
            .unwrap();
    assert_eq!(
        audits, 1,
        "grouping and projection repair keep one initial assignment audit"
    );
    let messages: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM message WHERE conversation_id=$1")
        .bind(conversation)
        .fetch_one(f.pg.pool())
        .await
        .unwrap();
    assert_eq!(
        messages, 4,
        "all distinct events remain separate Messages in the same thread"
    );
}

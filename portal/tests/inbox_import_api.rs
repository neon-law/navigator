//! `POST /app/api/inbox/import` — the LAW-238 D1 import door's HTTP contract.
//!
//! Authentication, source validation, payload validation, and the dry-run
//! preview are proven on the mem-Surreal fixture: the door parses before it
//! writes anything, and its ingest entry is postgres-authoritative, so a
//! mem-fixture replay fails closed (500) rather than half-importing. The
//! full write path — replay idempotency, direction and author resolution,
//! matter attribution, attachment filing — runs in a self-respawned child
//! against an isolated PostgreSQL database (the `source_imports_api.rs`
//! pattern), skipped silently when no test PostgreSQL is running.

use axum::body::Body;
use axum::http::{header, Request};
use axum::Router;
use base64::Engine as _;
use portal::api::ApiState;
use serde_json::{json, Value};
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use sqlx::PgPool;
use store::persons::{NewPerson, Role};
use store::surreal::SurrealDb;
use store::test_support::{ensure_person, mem_surreal, seed_entity};
use tower::ServiceExt;
use uuid::Uuid;

/// An RFC 5322 message with one attachment — counterparty → lawyer.
const INBOUND_EML: &[u8] = b"From: Opposing Counsel <counsel@opposing.example>\r\n\
To: nick@neonlaw.com\r\n\
Subject: Discovery schedule\r\n\
Date: Tue, 28 Oct 2026 14:03:00 +0000\r\n\
Message-ID: <notice-1@opposing.example>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"BOUND\"\r\n\
\r\n\
--BOUND\r\n\
Content-Type: text/plain\r\n\
\r\n\
See the attached schedule.\r\n\
--BOUND\r\n\
Content-Type: application/pdf; name=\"exhibit-a.pdf\"\r\n\
Content-Disposition: attachment; filename=\"exhibit-a.pdf\"\r\n\
\r\n\
synthetic pdf bytes\r\n\
--BOUND--\r\n";

/// The lawyer's own mail — the direction the webhook paths never carried.
const SENT_EML: &[u8] = b"From: Nick Shook <nick@neonlaw.com>\r\n\
To: counsel@opposing.example\r\n\
Subject: Re: Discovery schedule\r\n\
Date: Tue, 28 Oct 2026 16:20:00 +0000\r\n\
Message-ID: <reply-1@neonlaw.example>\r\n\
\r\n\
Agreed to the schedule.\r\n";

/// A `LinkedIn` export fragment: one row authored by the mailbox owner
/// (outbound) and one by the counterparty (inbound), one thread.
const LINKEDIN_CSV: &[u8] = b"CONVERSATION ID,FROM,SENDER PROFILE URL,TO,DATE,SUBJECT,CONTENT\r\n\
conv-88,Nick Shook,https://www.linkedin.com/in/nickshook,Other Counsel,2026-10-28 14:03:00,Referral,Happy to connect\r\n\
conv-88,Other Counsel,https://www.linkedin.com/in/other-counsel,Nick Shook,2026-10-28 15:00:00,Referral,Sending the intro\r\n";

/// A `LinkedIn` export with a shape the adapter does not recognize — the
/// `CONVERSATION ID` header renamed — which must fail loudly, naming the
/// offending headers (AC9).
const LINKEDIN_CSV_CORRUPT: &[u8] = b"CONVERSATION,FROM,SENDER PROFILE URL,TO,DATE,SUBJECT,CONTENT\r\n\
conv-88,Nick Shook,https://www.linkedin.com/in/nickshook,Other Counsel,2026-10-28 14:03:00,Referral,Hello\r\n";

const MAILBOX: &str = "nick@neonlaw.com";
const LINKEDIN_MAILBOX: &str = "https://www.linkedin.com/in/nickshook";

struct Fixture {
    app: Router,
    lawyer_session: portal::SessionData,
    surreal: SurrealDb,
    storage: std::sync::Arc<dyn cloud::StorageService>,
}

/// The mem-Surreal fixture: a lawyer, their practice Firm, and their
/// membership on it — everything the door resolves before it parses.
async fn fixture() -> Fixture {
    let db = mem_surreal().await;
    let lawyer = ensure_person(
        &db,
        &NewPerson {
            name: "Synthetic Lawyer".into(),
            email: MAILBOX.into(),
            role: Role::Lawyer,
            ..Default::default()
        },
    )
    .await;
    seed_firm_membership(&db, lawyer.id).await;

    build_app(db, lawyer.id).await
}

async fn seed_firm_membership(db: &SurrealDb, person_id: Uuid) -> Uuid {
    // Firm creation requires an Admin-tier admin DRI; the importing lawyer
    // joins as a plain Lawyer member.
    let admin = store::persons::create(
        db,
        &store::persons::NewPerson::with_role(
            "Test Firm Admin",
            format!("test-firm-admin-{}@example.com", Uuid::now_v7()),
            Role::Admin,
        ),
    )
    .await
    .expect("seed Firm Admin");
    let firm = store::firms::create(
        db,
        &store::firms::NewFirm {
            name: "Practice".into(),
            status: "active".into(),
            entity_id: seed_entity(db).await,
            admin_dri_person_id: admin.id,
        },
    )
    .await
    .expect("seed firm");
    store::firms::add_membership(
        db,
        &store::firms::NewPersonFirmRole {
            person_id,
            firm_id: firm.id,
            membership: store::firms::FirmMembership::Lawyer,
            is_dri: false,
        },
    )
    .await
    .expect("seed lawyer membership");
    firm.id
}

async fn build_app(db: SurrealDb, person_id: Uuid) -> Fixture {
    let state = portal::test_support::app_state(db).await;
    let api_state = ApiState {
        surreal: state.surreal.clone(),
        email: state.email.clone(),
        bootstrap_owner_email: state.bootstrap_owner_email.clone(),
        bootstrap_company: "Synthetic Firm".into(),
        questionnaire_runtime: state.questionnaire_runtime.clone(),
        storage: state.storage.clone(),
        workflow_runtime: state.workflow_runtime.clone(),
        assets_storage: state.assets_storage.clone(),
        forms_registry: state.forms_registry.clone(),
        signature_provider: state.signature_provider.clone(),
        contract_reviewer: state.contract_reviewer.clone(),
        integration_providers: state.integration_providers.clone(),
    };
    let lawyer_session = portal::SessionData {
        person_id: Some(person_id),
        ..portal::SessionData::fresh("synthetic-lawyer-sub", Role::Lawyer)
    };
    Fixture {
        app: portal::api::routes().with_state(api_state),
        lawyer_session,
        surreal: state.surreal,
        storage: state.storage,
    }
}

async fn post_json(
    app: &Router,
    session: Option<&portal::SessionData>,
    body: Value,
) -> (u16, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/app/api/inbox/import")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(session) = session {
        builder = builder.extension(session.clone());
    }
    let response = app
        .clone()
        .oneshot(
            builder
                .body(Body::from(body.to_string()))
                .expect("build request"),
        )
        .await
        .expect("run request");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    let json: Value = serde_json::from_slice(&bytes).expect("response is JSON");
    (status, json)
}

/// One request body for the email source.
fn email_body(eml: &[u8], extra: &Value) -> Value {
    merge_extra(
        json!({
            "source": "email",
            "mailbox": MAILBOX,
            "payload_base64": base64::engine::general_purpose::STANDARD.encode(eml),
        }),
        extra,
    )
}

/// One request body for the `LinkedIn` source.
fn linkedin_body(csv: &[u8], extra: &Value) -> Value {
    merge_extra(
        json!({
            "source": "linkedin",
            "mailbox": LINKEDIN_MAILBOX,
            "payload_base64": base64::engine::general_purpose::STANDARD.encode(csv),
        }),
        extra,
    )
}

/// Overlay optional fields (`source`, `project`, `dry_run`) onto a base body.
fn merge_extra(mut base: Value, extra: &Value) -> Value {
    if let (Some(base), Some(extra)) = (base.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            base.insert(key.clone(), value.clone());
        }
    }
    base
}

#[tokio::test]
async fn an_anonymous_caller_is_unauthenticated() {
    let fixture = fixture().await;
    let (status, _) = post_json(&fixture.app, None, email_body(INBOUND_EML, &json!({}))).await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn a_clerk_and_a_client_are_forbidden() {
    let fixture = fixture().await;
    for role in [Role::Clerk, Role::Client] {
        let session = portal::SessionData::fresh("synthetic-sub", role);
        let (status, _) = post_json(
            &fixture.app,
            Some(&session),
            email_body(INBOUND_EML, &json!({})),
        )
        .await;
        assert_eq!(status, 403, "{role:?} must be forbidden");
    }
}

#[tokio::test]
async fn an_unknown_source_is_rejected_before_the_payload_is_read() {
    let fixture = fixture().await;
    let (status, body) = post_json(
        &fixture.app,
        Some(&fixture.lawyer_session),
        email_body(INBOUND_EML, &json!({"source": "imap"})),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["error"], "unknown_source");
}

#[tokio::test]
async fn a_blank_mailbox_is_rejected() {
    let fixture = fixture().await;
    let (status, body) = post_json(
        &fixture.app,
        Some(&fixture.lawyer_session),
        json!({
            "source": "email",
            "mailbox": "  ",
            "payload_base64": base64::engine::general_purpose::STANDARD.encode(INBOUND_EML),
        }),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["error"], "mailbox_required");
}

#[tokio::test]
async fn invalid_base64_is_rejected() {
    let fixture = fixture().await;
    let (status, body) = post_json(
        &fixture.app,
        Some(&fixture.lawyer_session),
        json!({
            "source": "email",
            "mailbox": MAILBOX,
            "payload_base64": "not base64 ***",
        }),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["error"], "invalid_base64");
}

#[tokio::test]
async fn an_unparseable_email_fails_loudly() {
    let fixture = fixture().await;
    let (status, body) = post_json(
        &fixture.app,
        Some(&fixture.lawyer_session),
        email_body(b"this is not MIME", &json!({})),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["error"], "unparseable_payload");
}

#[tokio::test]
async fn a_corrupted_linkedin_export_fails_loudly_naming_the_headers() {
    let fixture = fixture().await;
    let (status, body) = post_json(
        &fixture.app,
        Some(&fixture.lawyer_session),
        linkedin_body(LINKEDIN_CSV_CORRUPT, &json!({})),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["error"], "unparseable_payload");
    let message = body["message"]
        .as_str()
        .expect("message carries the headers");
    assert!(
        message.contains("CONVERSATION"),
        "the offending header must be named: {message}"
    );
}

#[tokio::test]
async fn a_dry_run_previews_the_parse_and_writes_nothing() {
    let fixture = fixture().await;

    // The preview itself: parsed, counted, nothing archived or ingested.
    let (status, body) = post_json(
        &fixture.app,
        Some(&fixture.lawyer_session),
        email_body(INBOUND_EML, &json!({"dry_run": true})),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["dry_run"], true);
    assert_eq!(body["would_import"], 1);

    // The write path, un-gated: without the PostgreSQL Inbox authority the
    // ingest refuses — which is also proof the dry run wrote nothing, since
    // the same payload reaches the ingest here rather than being rejected
    // earlier as already-present.
    let (status, body) = post_json(
        &fixture.app,
        Some(&fixture.lawyer_session),
        email_body(INBOUND_EML, &json!({})),
    )
    .await;
    // The house Db mapping: the detail is logged, the wire says `internal`.
    assert_eq!(status, 500);
    assert_eq!(body["error"], "internal");
}

/// The full write path against real PostgreSQL: replay idempotency (AC3),
/// direction and author from the mailbox owner (AC4), verbatim archive
/// (AC6), attribution at import (AC7), and attachment filing (AC8).
#[tokio::test]
async fn an_import_walks_the_postgres_ingest_idempotently() {
    if std::env::var("NAV_INBOX_IMPORT_CHILD").as_deref() == Ok("1") {
        run_lifecycle().await;
        return;
    }
    let Some((port, database)) = isolated_database().await else {
        return;
    };
    let status = std::process::Command::new(std::env::current_exe().expect("current executable"))
        .args([
            "--exact",
            "an_import_walks_the_postgres_ingest_idempotently",
            "--nocapture",
        ])
        .env("NAV_INBOX_IMPORT_CHILD", "1")
        .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
        .env("NAVIGATOR_POSTGRES_PORT", &port)
        .env("NAVIGATOR_POSTGRES_USER", "postgres")
        .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
        .env("NAVIGATOR_POSTGRES_DATABASE", &database)
        .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
        // The attachment filing mirrors the Surreal asset row into
        // PostgreSQL; `set_attachment_asset`'s foreign key needs it there.
        .env("NAVIGATOR_POSTGRES_MIRROR", "1")
        .env("NAVIGATOR_POSTGRES_INBOX_AUTHORITY", "1")
        .status()
        .expect("spawn the postgres-backed child test");
    remove_isolated_database(&port, &database).await;
    assert!(status.success());
}

/// Reference rows the mirror cannot carry: the Surreal fixture links
/// entities at these constant ids without seeding their tables, and
/// PostgreSQL's foreign keys need them before the entity mirror fires.
async fn seed_postgres_references(pg: &store::postgres::PostgresDb) {
    sqlx::query(
        "INSERT INTO jurisdiction (id,name,code) VALUES ($1,'Synthetic Jurisdiction','TST')",
    )
    .bind(store::test_support::SEED_ENTITY_JURISDICTION_ID)
    .execute(pg.pool())
    .await
    .expect("seed jurisdiction");
    sqlx::query("INSERT INTO entity_type (id,name) VALUES ($1,'Synthetic Entity')")
        .bind(store::test_support::SEED_ENTITY_TYPE_ID)
        .execute(pg.pool())
        .await
        .expect("seed entity_type");
}

/// The lawyer, their Firm and membership, the matter, and their
/// participation on it — the practice the import door resolves.
async fn seed_practice(db: &store::surreal::SurrealDb) -> (Uuid, store::projects::Project) {
    let lawyer = ensure_person(
        db,
        &NewPerson {
            name: "Synthetic Lawyer".into(),
            email: MAILBOX.into(),
            role: Role::Lawyer,
            ..Default::default()
        },
    )
    .await;
    let firm_id = seed_firm_membership(db, lawyer.id).await;
    let entity_id = seed_entity(db).await;
    let project = store::projects::create(
        db,
        &store::projects::NewProject {
            code: "alpha-matter".into(),
            name: "Alpha Matter".into(),
            status: "open".into(),
            entity_id,
            firm_id: Some(firm_id),
            ..store::projects::NewProject::default()
        },
    )
    .await
    .expect("seed project");
    // The matter-scope check (`can_see_project_as_lawyer`) requires
    // participation, not only Firm membership.
    store::participation::add_participant(
        db,
        &store::participation::AddParticipantCommand {
            project_id: project.id,
            person_id: lawyer.id,
            dri: store::participation::DriRequest::Unchanged,
            actor: store::participation::DriActor::System,
        },
    )
    .await
    .expect("seed lawyer participation");
    (lawyer.id, project)
}

async fn run_lifecycle() {
    // Surface the door's error logs (the house Db mapping logs the detail,
    // never the wire) — the only way to diagnose a 500 in this child.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("debug"))
        .with_writer(std::io::stderr)
        .try_init();
    let pg = store::postgres::PostgresDb::from_env()
        .await
        .expect("connect the test database");
    pg.migrate_embedded(None)
        .await
        .expect("apply the compiled migrations");
    seed_postgres_references(&pg).await;

    // The Surreal side: the lawyer, their Firm and membership, the matter.
    // Person, firm, membership, entity, project, and participation rows all
    // reach PostgreSQL through the mirror their Surreal writes fire
    // (NAVIGATOR_POSTGRES_MIRROR=1 on the child) — seeding them again by
    // hand collides with the mirror's own rows.
    let db = mem_surreal().await;
    let (lawyer_id, project) = seed_practice(&db).await;

    let fixture = build_app(db, lawyer_id).await;
    let project_id = project.id;
    assert_email_lifecycle(&fixture, &pg, project_id).await;
    assert_linkedin_lifecycle(&fixture).await;
}

/// The email walk (AC3, AC4, AC6, AC7, AC8): first import attributed at
/// request time, the verbatim payload archived, the attachment filed as a
/// slugged revision, the replay deduping to the same ids, and the
/// lawyer's own mail arriving as outbound.
async fn assert_email_lifecycle(
    fixture: &Fixture,
    pg: &store::postgres::PostgresDb,
    project_id: Uuid,
) {
    // First import of the inbound .eml, attributed at import.
    let (status, first) = post_json(
        &fixture.app,
        Some(&fixture.lawyer_session),
        email_body(INBOUND_EML, &json!({"project": "alpha-matter"})),
    )
    .await;
    assert_eq!(status, 201, "{first}");
    assert_eq!(first["imported"].as_array().expect("imported").len(), 1);
    assert_eq!(first["imported"][0]["deduped"], false);
    assert_eq!(first["imported"][0]["direction"], "inbound");
    let conversation_id: Uuid = first["imported"][0]["conversation_id"]
        .as_str()
        .expect("conversation id")
        .parse()
        .expect("conversation id is a uuid");

    // The conversation landed on the matter (AC7, attribution at import).
    let attributed: Option<Uuid> =
        sqlx::query_scalar("SELECT project_id FROM inbox_conversation WHERE id = $1")
            .bind(conversation_id)
            .fetch_one(pg.pool())
            .await
            .expect("read attributed conversation");
    assert_eq!(attributed, Some(project_id));

    // Verbatim payload archived under the direction-neutral prefix, key
    // reported (AC6).
    let archived_key = first["archived_key"].as_str().expect("archive key");
    assert!(
        archived_key.starts_with("mail/"),
        "imports archive under the direction-neutral prefix"
    );
    let archived = fixture
        .storage
        .get(archived_key)
        .await
        .expect("the verbatim .eml is archived");
    assert_eq!(archived.bytes, INBOUND_EML);

    // The attachment filed as a slugged revision; the Message's slot holds
    // its asset id (AC8).
    let asset_id: Uuid = first["imported"][0]["attachments"][0]["asset_id"]
        .as_str()
        .expect("filed asset id")
        .parse()
        .expect("asset id is a uuid");
    let fetched = store::assets::fetch(&fixture.surreal, &fixture.storage, asset_id)
        .await
        .expect("fetch the filed attachment");
    assert_eq!(fetched, b"synthetic pdf bytes".as_slice());

    // Replay: the same payload reports the existing ids and dedups (AC3).
    let (status, second) = post_json(
        &fixture.app,
        Some(&fixture.lawyer_session),
        email_body(INBOUND_EML, &json!({"project": "alpha-matter"})),
    )
    .await;
    assert_eq!(status, 201, "{second}");
    assert_eq!(second["imported"][0]["deduped"], true);
    assert_eq!(
        second["imported"][0]["message_id"],
        first["imported"][0]["message_id"]
    );

    // The lawyer's own mail arrives as outbound (AC4).
    let (status, sent) = post_json(
        &fixture.app,
        Some(&fixture.lawyer_session),
        email_body(SENT_EML, &json!({})),
    )
    .await;
    assert_eq!(status, 201, "{sent}");
    assert_eq!(sent["imported"][0]["direction"], "outbound");
}

/// The `LinkedIn` walk (AC3, AC9): one request imports both directions
/// onto one Conversation, and replaying the export dedups every row.
async fn assert_linkedin_lifecycle(fixture: &Fixture) {
    // A LinkedIn export imports both directions in one request and threads
    // its rows onto one Conversation (AC4, AC9).
    let (status, linkedin) = post_json(
        &fixture.app,
        Some(&fixture.lawyer_session),
        linkedin_body(LINKEDIN_CSV, &json!({"project": "alpha-matter"})),
    )
    .await;
    assert_eq!(status, 201, "{linkedin}");
    let imported = linkedin["imported"].as_array().expect("imported");
    assert_eq!(imported.len(), 2);
    assert_eq!(imported[0]["direction"], "outbound");
    assert_eq!(imported[1]["direction"], "inbound");
    assert_eq!(
        imported[0]["conversation_id"],
        imported[1]["conversation_id"]
    );

    // Replaying the export dedups every row (AC3).
    let (status, replayed) = post_json(
        &fixture.app,
        Some(&fixture.lawyer_session),
        linkedin_body(LINKEDIN_CSV, &json!({"project": "alpha-matter"})),
    )
    .await;
    assert_eq!(status, 201, "{replayed}");
    for row in replayed["imported"].as_array().expect("imported") {
        assert_eq!(row["deduped"], true, "replayed export rows dedup: {row}");
    }
}

/// The test PostgreSQL's admin handle — the `source_imports_api` pattern.
fn admin_options(port: &str) -> PgConnectOptions {
    PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port.parse().expect("port is a number"))
        .username("postgres")
        .password("synthetic")
        .ssl_mode(PgSslMode::Disable)
}

async fn isolated_database() -> Option<(String, String)> {
    let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
        assert!(
            std::env::var("NAV_REQUIRE_POSTGRES").as_deref() != Ok("1"),
            "PostgreSQL fixture required"
        );
        return None;
    };
    let admin = PgPool::connect_with(admin_options(&port))
        .await
        .expect("connect admin");
    let database = format!("inbox_import_api_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&admin)
        .await
        .expect("create the isolated database");
    admin.close().await;
    Some((port, database))
}

async fn remove_isolated_database(port: &str, database: &str) {
    let admin = PgPool::connect_with(admin_options(port))
        .await
        .expect("connect admin");
    sqlx::query("SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname=$1")
        .bind(database)
        .execute(&admin)
        .await
        .expect("terminate backends");
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP DATABASE {database}")))
        .execute(&admin)
        .await
        .expect("drop the isolated database");
    admin.close().await;
}

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
    build_app_with_storage(db, person_id, None).await
}

async fn build_app_with_storage(
    db: SurrealDb,
    person_id: Uuid,
    storage: Option<std::sync::Arc<dyn cloud::StorageService>>,
) -> Fixture {
    let state = portal::test_support::app_state(db).await;
    let storage = storage.unwrap_or_else(|| state.storage.clone());
    let api_state = ApiState {
        surreal: state.surreal.clone(),
        canonical_base_url: None,
        email: state.email.clone(),
        bootstrap_owner_email: state.bootstrap_owner_email.clone(),
        bootstrap_company: "Synthetic Firm".into(),
        questionnaire_runtime: state.questionnaire_runtime.clone(),
        storage: storage.clone(),
        workflow_runtime: state.workflow_runtime.clone(),
        assets_storage: state.assets_storage.clone(),
        forms_registry: state.forms_registry.clone(),
        signature_provider: state.signature_provider.clone(),
        contract_reviewer: state.contract_reviewer.clone(),
        integration_providers: state.integration_providers.clone(),
        billing_provider: state.billing_provider.clone(),
    };
    let lawyer_session = portal::SessionData {
        person_id: Some(person_id),
        ..portal::SessionData::fresh("synthetic-lawyer-sub", Role::Lawyer)
    };
    Fixture {
        app: portal::api::routes().with_state(api_state),
        lawyer_session,
        surreal: state.surreal,
        storage,
    }
}

async fn post_json(
    app: &Router,
    session: Option<&portal::SessionData>,
    body: Value,
) -> (u16, Value) {
    post_json_at(app, session, "/app/api/inbox/import", body).await
}

async fn post_json_at(
    app: &Router,
    session: Option<&portal::SessionData>,
    uri: &str,
    body: Value,
) -> (u16, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
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
async fn a_project_message_attachment_download_keeps_its_name_and_scope() {
    let db = mem_surreal().await;
    let (lawyer_id, project) = seed_practice(&db).await;
    let fixture = build_app(db, lawyer_id).await;
    let (status, body) = post_json_at(
        &fixture.app,
        Some(&fixture.lawyer_session),
        &format!("/app/api/projects/{}/inbox-ingest", project.id),
        json!({"raw": base64::engine::general_purpose::STANDARD.encode(INBOUND_EML)}),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    let conversation_id = body["conversation_id"].as_str().unwrap();
    let message_id: Uuid = body["message_id"].as_str().unwrap().parse().unwrap();
    let attachments =
        store::inbox::message_assets(&fixture.surreal, project.firm_id.unwrap(), message_id)
            .await
            .unwrap();
    assert_eq!(attachments.len(), 1);
    let attachment = &attachments[0];
    let path = format!(
        "/app/api/projects/{}/inbox/{conversation_id}/messages/{message_id}/attachments/{}",
        project.id, attachment.asset_id
    );
    assert_downloaded_email_attachment(&fixture, &path).await;

    for denied in [
        path.replace(&project.id.to_string(), &Uuid::now_v7().to_string()),
        path.replace(conversation_id, &Uuid::now_v7().to_string()),
        path.replace(&message_id.to_string(), &Uuid::now_v7().to_string()),
        path.replace(
            &attachment.asset_id.to_string(),
            &Uuid::now_v7().to_string(),
        ),
    ] {
        assert_eq!(
            get_attachment(&fixture.app, &fixture.lawyer_session, &denied)
                .await
                .status(),
            404
        );
    }
    for role in [Role::Client, Role::Clerk] {
        let session = portal::SessionData {
            role,
            ..fixture.lawyer_session.clone()
        };
        assert_eq!(
            get_attachment(&fixture.app, &session, &path).await.status(),
            403
        );
    }
    let outsider = ensure_person(
        &fixture.surreal,
        &NewPerson {
            name: "Outside Lawyer".into(),
            email: "outside@example.test".into(),
            role: Role::Lawyer,
            ..Default::default()
        },
    )
    .await;
    let session = portal::SessionData {
        person_id: Some(outsider.id),
        ..fixture.lawyer_session.clone()
    };
    assert_eq!(
        get_attachment(&fixture.app, &session, &path).await.status(),
        404
    );
}

async fn get_attachment(
    app: &Router,
    session: &portal::SessionData,
    path: &str,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .extension(session.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn assert_downloaded_email_attachment(fixture: &Fixture, path: &str) {
    let response = get_attachment(&fixture.app, &fixture.lawyer_session, path).await;
    assert_eq!(
        response.status(),
        200,
        "download the synthetic email attachment"
    );
    assert_eq!(
        response.headers()[header::CONTENT_DISPOSITION],
        "attachment; filename=\"exhibit-a.pdf\""
    );
    assert_eq!(
        response.headers()[header::X_CONTENT_TYPE_OPTIONS],
        "nosniff"
    );
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "private, no-store"
    );
    assert_eq!(
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
        b"synthetic pdf bytes".as_slice()
    );
}

#[tokio::test]
async fn embedded_policy_admits_the_lawyer_fi_preview_to_validation() {
    let fixture = fixture().await;
    let actor = fixture.lawyer_session.person_id.unwrap();
    let firm = store::firms::memberships_for_person(&fixture.surreal, actor)
        .await
        .unwrap()[0]
        .firm_id;
    let admin = store::firms::memberships_for_firm(&fixture.surreal, firm)
        .await
        .unwrap()
        .into_iter()
        .find(|member| member.is_dri)
        .unwrap()
        .person_id;
    store::brands::create(
        &fixture.surreal,
        Role::Admin,
        Some(admin),
        &store::brands::NewBrand {
            key: "fi-http".into(),
            name: "Synthetic Fi Brand".into(),
            firm_id: Some(firm),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    store::firms::ensure_brand(&fixture.surreal, firm, "fi-http")
        .await
        .unwrap();
    let app = fixture
        .app
        .clone()
        .layer(axum::middleware::from_fn_with_state(
            (
                portal::SessionStore::new(vec![7_u8; 32]),
                portal::policy::PolicyClient::embedded().expect("compile deployed policy"),
            ),
            portal::policy::require_policy,
        ))
        .layer(tower_cookies::CookieManagerLayer::new());
    // The default preview must reach Fi validation through the deployed policy.
    // An invalid account produces 400 before PostgreSQL or any import write.
    let request = fi_body("", &json!({"account": "invalid-account"}));
    assert!(request.get("dry_run").is_none());
    let (status, body) = post_json_at(
        &app,
        Some(&fixture.lawyer_session),
        "/app/api/inbox/import/fi",
        request,
    )
    .await;
    assert_eq!(
        status, 400,
        "Lawyer Fi preview must reach validation: {body}"
    );
    assert_eq!(body["error"], "fi_mapping");
}

#[tokio::test]
async fn embedded_policy_admits_the_lawyer_import_door() {
    let fixture = fixture().await;
    let app = fixture
        .app
        .clone()
        .layer(axum::middleware::from_fn_with_state(
            (
                portal::SessionStore::new(vec![7_u8; 32]),
                portal::policy::PolicyClient::embedded().expect("compile deployed policy"),
            ),
            portal::policy::require_policy,
        ))
        .layer(tower_cookies::CookieManagerLayer::new());
    let (status, body) = post_json(
        &app,
        Some(&fixture.lawyer_session),
        email_body(INBOUND_EML, &json!({"dry_run": true})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["would_import"], 1);
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
        Box::pin(run_lifecycle()).await;
        return;
    }
    let Some((port, database)) = isolated_database().await else {
        return;
    };
    let output = std::process::Command::new(std::env::current_exe().expect("current executable"))
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
        .output()
        .expect("spawn the postgres-backed child test");
    remove_isolated_database(&port, &database).await;
    assert!(
        output.status.success(),
        "child failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
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
        .with_env_filter(tracing_subscriber::EnvFilter::new("warn,sqlx::query=error"))
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
    assert_native_provider_attachment_ingest(&fixture, &pg, &project).await;
    assert_linkedin_lifecycle(&fixture).await;
    assert_interrupted_import_recovers_attachments(&fixture, project.firm_id.unwrap()).await;
    assert_extensionless_attachment_filing(&fixture).await;
    Box::pin(assert_fi_lifecycle(&fixture, &pg, project_id)).await;
}

async fn assert_native_provider_attachment_ingest(
    fixture: &Fixture,
    pg: &store::postgres::PostgresDb,
    project: &store::projects::Project,
) {
    let raw = String::from_utf8(INBOUND_EML.to_vec()).unwrap().replace(
        "notice-1@opposing.example",
        "provider-attachment@opposing.example",
    );
    let path = format!("/app/api/projects/{}/inbox-ingest", project.id);
    let payload = json!({
        "raw": base64::engine::general_purpose::STANDARD.encode(raw.as_bytes()),
        "provider_account": "trusted-provider@example.com",
    });
    let (status, first) = post_json_at(
        &fixture.app,
        Some(&fixture.lawyer_session),
        &path,
        payload.clone(),
    )
    .await;
    assert_eq!(status, 201, "{first}");
    let id = Uuid::parse_str(first["message_id"].as_str().unwrap()).unwrap();
    let asset_projects: Vec<Option<Uuid>> = sqlx::query_scalar(
        "SELECT a.project_id FROM inbox_message_asset l JOIN asset a ON a.id=l.asset_id WHERE l.message_id=$1",
    ).bind(id).fetch_all(pg.pool()).await.unwrap();
    assert_eq!(
        asset_projects,
        vec![Some(project.id)],
        "the actual provider route binds attachment bytes to the attributed Project"
    );
    let firm: Uuid = sqlx::query_scalar(
        "SELECT c.firm_id FROM message m JOIN inbox_conversation c ON c.id=m.conversation_id WHERE m.id=$1",
    ).bind(id).fetch_one(pg.pool()).await.unwrap();
    assert_eq!(firm, project.firm_id.unwrap());
    let (status, repeat) =
        post_json_at(&fixture.app, Some(&fixture.lawyer_session), &path, payload).await;
    assert_eq!(status, 201, "{repeat}");
    assert_eq!(repeat["message_id"], first["message_id"]);
    assert_eq!(repeat["deduped"], true);
    let links: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM inbox_message_asset WHERE message_id=$1")
            .bind(id)
            .fetch_one(pg.pool())
            .await
            .unwrap();
    assert_eq!(links, 1);
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
    let asset = store::assets::find_by_id(&fixture.surreal, asset_id)
        .await
        .unwrap()
        .unwrap();
    store::documents::validate_document_slug(
        asset.filename.as_deref().unwrap(),
        asset.slug.as_deref().unwrap(),
    )
    .expect("an imported attachment must be representable by a repository pointer");
    let message_id: Uuid = serde_json::from_value(first["imported"][0]["message_id"].clone())
        .expect("imported Message UUID");
    let download_path = format!(
        "/app/api/projects/{project_id}/inbox/{conversation_id}/messages/{message_id}/attachments/{asset_id}"
    );
    assert_downloaded_email_attachment(fixture, &download_path).await;
    let project = store::projects::find_by_id(&fixture.surreal, project_id)
        .await
        .unwrap()
        .unwrap();
    assert_pg_process_reads(fixture, project.firm_id.unwrap(), message_id).await;

    // Replay: the same payload reports the existing ids and dedups (AC3).
    let (status, second) = post_json(
        &fixture.app,
        Some(&fixture.lawyer_session),
        email_body(INBOUND_EML, &json!({"project": "alpha-matter"})),
    )
    .await;
    assert_eq!(status, 201, "{second}");
    assert_eq!(second["imported"][0]["deduped"], true);
    assert_eq!(second["archived_key"], first["archived_key"]);
    assert_eq!(
        second["imported"][0]["attachments"],
        first["imported"][0]["attachments"]
    );
    assert_eq!(
        second["imported"][0]["message_id"],
        first["imported"][0]["message_id"]
    );

    let assignments: i64 =
        sqlx::query_scalar("SELECT count(*) FROM inbox_assignment WHERE conversation_id = $1")
            .bind(conversation_id)
            .fetch_one(pg.pool())
            .await
            .expect("count attribution events");
    assert_eq!(
        assignments, 1,
        "replay must not create another attribution event"
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

async fn assert_pg_process_reads(fixture: &Fixture, firm_id: Uuid, message_id: Uuid) {
    let actor = fixture.lawyer_session.person_id.unwrap();
    let state = store::inbox::process_state(&fixture.surreal, message_id)
        .await
        .unwrap();
    let links = store::inbox::message_assets(&fixture.surreal, firm_id, message_id)
        .await
        .unwrap();
    let pending = store::inbox::unprocessed_inbox(&fixture.surreal, actor)
        .await
        .unwrap();
    assert!(
        state.is_some() && links.len() == 1 && pending.iter().any(|row| row.id == message_id),
        "PostgreSQL authority must serve process state, attachments, and the backlog: {:?}",
        (state.is_some(), links.len(), pending.len())
    );
    let at = chrono::Utc::now();
    store::inbox::mark_processed(&fixture.surreal, message_id, store::inbox::route::NEF, at)
        .await
        .unwrap();
    let state = store::inbox::process_state(&fixture.surreal, message_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.route.as_deref(), Some(store::inbox::route::NEF));
    assert_eq!(state.processed_at, Some(at));
    let pending = store::inbox::unprocessed_inbox(&fixture.surreal, actor)
        .await
        .unwrap();
    assert!(
        !pending.iter().any(|row| row.id == message_id),
        "processing removes the Message from the backlog"
    );
}

async fn assert_interrupted_import_recovers_attachments(fixture: &Fixture, firm_id: Uuid) {
    let raw = String::from_utf8(INBOUND_EML.to_vec()).unwrap().replace(
        "notice-1@opposing.example",
        "interrupted-1@opposing.example",
    );
    let adapter = store::inbox_import::source_adapter("email").unwrap();
    let mut parsed = adapter.parse(MAILBOX, raw.as_bytes()).unwrap();
    fixture
        .storage
        .put("mail/interrupted.eml", raw.as_bytes(), "message/rfc822")
        .await
        .unwrap();
    parsed[0].raw_storage_key = Some("mail/interrupted.eml".into());
    let input = store::inbox_import::import_message_to_ingest(&parsed[0], firm_id, MAILBOX);
    let seeded = store::inbox::ingest_imported("inbound", None, &input)
        .await
        .unwrap();
    // This committed Message has no filed attachments, as after an interruption.
    let (status, response) = post_json(
        &fixture.app,
        Some(&fixture.lawyer_session),
        email_body(raw.as_bytes(), &json!({"project": "alpha-matter"})),
    )
    .await;
    assert_eq!(status, 201, "{response}");
    assert_eq!(
        response["imported"][0]["message_id"],
        seeded.message_id.to_string()
    );
    assert_eq!(response["imported"][0]["deduped"], true);
    assert_eq!(
        response["imported"][0]["attachments"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "a deduped Message must not suppress recovery of unfiled attachments"
    );
    let links = store::inbox::message_assets(&fixture.surreal, firm_id, seeded.message_id)
        .await
        .unwrap();
    assert_eq!(
        links.len(),
        1,
        "the recovered attachment must be linked to its Message"
    );
}

/// The `LinkedIn` walk (AC3, AC9): one request imports both directions
/// onto one Conversation, and replaying the export dedups every row.
async fn assert_extensionless_attachment_filing(fixture: &Fixture) {
    let raw = String::from_utf8(INBOUND_EML.to_vec())
        .unwrap()
        .replace(
            "notice-1@opposing.example",
            "extensionless@opposing.example",
        )
        .replace("exhibit-a.pdf", "README");
    let (status, response) = post_json(
        &fixture.app,
        Some(&fixture.lawyer_session),
        email_body(raw.as_bytes(), &json!({"project": "alpha-matter"})),
    )
    .await;
    assert_eq!(status, 201, "{response}");
    let attachment = &response["imported"][0]["attachments"][0];
    assert_eq!(
        attachment["filename"], "README",
        "retain the original provider name"
    );
    let asset_id = attachment["asset_id"].as_str().unwrap().parse().unwrap();
    let asset = store::assets::find_by_id(&fixture.surreal, asset_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(asset.filename.as_deref(), Some("README.bin"));
    store::documents::validate_document_slug(
        asset.filename.as_deref().unwrap(),
        asset.slug.as_deref().unwrap(),
    )
    .expect("extensionless provider files still need a valid repository identity");
}

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

struct NoFiArchive;
#[async_trait::async_trait]
impl cloud::StorageService for NoFiArchive {
    async fn put(
        &self,
        _key: &str,
        _bytes: &[u8],
        _content_type: &str,
    ) -> Result<(), cloud::StorageError> {
        panic!("Fi metadata import must not archive raw CSV or attachments");
    }
    async fn get(&self, _key: &str) -> Result<cloud::StoredObject, cloud::StorageError> {
        panic!("Fi metadata import must not read provider storage");
    }
    async fn delete(&self, _key: &str) -> Result<(), cloud::StorageError> {
        panic!("Fi metadata import must not delete provider storage");
    }
    async fn signed_url(
        &self,
        _key: &str,
        _expires_in: std::time::Duration,
    ) -> Result<String, cloud::StorageError> {
        panic!("Fi metadata import must not create storage URLs");
    }
}

fn fi_mapping_json() -> Value {
    json!({"columns":{"kind":"type","direction":"way","counterparty":"phone","timestamp":["at"],"duration":"length","identity":"id","status":null},
        "kinds":{"C":"call","S":"sms"},"directions":{"I":"inbound","O":"outbound"},
        "timestamp_format":"%+","timestamp_separator":" ","timezone":{"mode":"embedded_offset"},"duration_unit":"seconds"})
}

fn fi_body(csv: &str, extra: &Value) -> Value {
    merge_extra(
        json!({"account":"+15555550123","brand":"fi-http","mapping":fi_mapping_json(),
        "payload_base64":base64::engine::general_purpose::STANDARD.encode(csv.as_bytes())}),
        extra,
    )
}

async fn post_fi_json(
    app: &Router,
    session: Option<&portal::SessionData>,
    body: Value,
) -> (u16, Value) {
    // The Fi import lock is instance-wide on the shared CI PostgreSQL, so a
    // concurrently scheduled test process can answer `Busy` — the route maps
    // it to 503 with `fi_import::FiImportError::Busy`'s Display and asks the
    // caller to rerun the unchanged batch. Retry that one body briefly;
    // every other status returns as-is so tests asserting conflict or
    // storage failures keep their exact wire semantics.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let mut request = Request::builder()
            .method("POST")
            .uri("/app/api/inbox/import/fi")
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(session) = session {
            request = request.extension(session.clone());
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status().as_u16();
        let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
            .await
            .unwrap();
        let parsed: Value = serde_json::from_slice(&bytes).unwrap();
        let busy = status == 503
            && parsed["error"] == "another Fi import is committing; retry the unchanged batch";
        if !busy || std::time::Instant::now() >= deadline {
            return (status, parsed);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

async fn assert_fi_assignment_authorization(
    fixture: &Fixture,
    pg: &store::postgres::PostgresDb,
    project: &store::projects::Project,
    admin: Uuid,
) {
    let outsider = ensure_person(
        &fixture.surreal,
        &NewPerson::with_role(
            "Synthetic Outside Admin",
            "outside-admin@example.com",
            Role::Admin,
        ),
    )
    .await;
    let client = ensure_person(
        &fixture.surreal,
        &NewPerson::with_role("Synthetic Client", "fi-client@example.com", Role::Client),
    )
    .await;
    let csv = "type,way,phone,at,length,id\nC,I,+15555550456,2026-10-29T18:00:00Z,61,authorization-call\nS,O,+15555550789,2026-10-29T18:01:00Z,,authorization-sms\n";
    for (label, actor, session_role) in [
        ("own Firm Admin", admin, Role::Admin),
        ("foreign Firm Admin", outsider.id, Role::Admin),
        ("stale Owner session for stored Admin", admin, Role::Owner),
        (
            "stale Owner session for stored Client",
            client.id,
            Role::Owner,
        ),
    ] {
        let session = portal::SessionData {
            person_id: Some(actor),
            ..portal::SessionData::fresh("synthetic-fi-authorization", session_role)
        };
        let before = fi_counts(pg).await;
        let audits: i64 = sqlx::query_scalar("SELECT count(*) FROM inbox_assignment")
            .fetch_one(pg.pool())
            .await
            .unwrap();
        let (status, body) = post_fi_json(
            &fixture.app,
            Some(&session),
            fi_body(csv, &json!({"project":project.code,"dry_run":false})),
        )
        .await;
        assert_eq!(
            fi_counts(pg).await,
            before,
            "{label} must not persist history: {status} {body}"
        );
        let after_audits: i64 = sqlx::query_scalar("SELECT count(*) FROM inbox_assignment")
            .fetch_one(pg.pool())
            .await
            .unwrap();
        assert_eq!(
            after_audits, audits,
            "{label} must not pin or audit an assignment"
        );
        assert_eq!(status, 403, "{label}: {body}");
    }
    assert_fi_unscoped_admin_preview(fixture, admin, csv).await;
    let owner = ensure_person(
        &fixture.surreal,
        &NewPerson::with_role("Synthetic Fi Owner", "fi-owner@example.com", Role::Owner),
    )
    .await;
    assert!(
        store::firms::memberships_for_person(&fixture.surreal, owner.id)
            .await
            .unwrap()
            .is_empty()
    );
    let session = portal::SessionData {
        person_id: Some(owner.id),
        ..portal::SessionData::fresh("synthetic-fi-owner", Role::Owner)
    };
    let (status, body) = post_fi_json(
        &fixture.app,
        Some(&session),
        fi_body(csv, &json!({"project":project.code,"dry_run":false})),
    )
    .await;
    assert_eq!(status, 201, "Owner needs no Firm membership: {body}");
    for row in body["imported"].as_array().unwrap() {
        let id: Uuid = serde_json::from_value(row["conversation_id"].clone()).unwrap();
        assert_eq!(
            store::inbox::conversation_by_id(&fixture.surreal, id)
                .await
                .unwrap()
                .unwrap()
                .project_id,
            Some(project.id)
        );
    }
}

async fn assert_fi_unscoped_admin_preview(fixture: &Fixture, admin: Uuid, csv: &str) {
    let session = portal::SessionData {
        person_id: Some(admin),
        ..portal::SessionData::fresh("synthetic-fi-admin-preview", Role::Admin)
    };
    let (status, body) = post_fi_json(&fixture.app, Some(&session), fi_body(csv, &json!({}))).await;
    assert_eq!(
        status, 200,
        "membership-derived unscoped Admin preview: {body}"
    );
    assert_eq!(body["dry_run"], true);
}

async fn fi_counts(pg: &store::postgres::PostgresDb) -> Vec<i64> {
    sqlx::query_scalar("SELECT ARRAY[(SELECT count(*) FROM message),(SELECT count(*) FROM inbox_conversation),(SELECT count(*) FROM inbox_ingress),(SELECT count(*) FROM asset),(SELECT count(*) FROM billed_call)]")
        .fetch_one(pg.pool()).await.unwrap()
}

#[expect(
    clippy::too_many_lines,
    reason = "one isolated real-PostgreSQL lifecycle proves preview, apply, replay and attribution share exact state"
)]
async fn assert_fi_lifecycle(
    fixture: &Fixture,
    pg: &store::postgres::PostgresDb,
    project_id: Uuid,
) {
    let actor = fixture.lawyer_session.person_id.unwrap();
    let project = store::projects::find_by_id(&fixture.surreal, project_id)
        .await
        .unwrap()
        .unwrap();
    let firm = project.firm_id.unwrap();
    let admin = store::firms::memberships_for_firm(&fixture.surreal, firm)
        .await
        .unwrap()
        .into_iter()
        .find(|member| member.is_dri)
        .unwrap()
        .person_id;
    store::brands::create(
        &fixture.surreal,
        Role::Admin,
        Some(admin),
        &store::brands::NewBrand {
            key: "fi-http".into(),
            name: "Fi HTTP".into(),
            firm_id: Some(firm),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    store::firms::ensure_brand(&fixture.surreal, firm, "fi-http")
        .await
        .unwrap();
    let app = build_app_with_storage(
        fixture.surreal.clone(),
        actor,
        Some(std::sync::Arc::new(NoFiArchive)),
    )
    .await;
    let csv="type,way,phone,at,length,id,content\nC,I,+15555550456,2026-10-30T18:00:00Z,61,fi-call,never archive me\nS,O,+15555550789,2026-10-30T18:01:00Z,,fi-sms,never archive me\n";
    assert_fi_assignment_rights_before_writes(&app, pg, csv).await;
    let before = fi_counts(pg).await;
    let (status, _) = post_fi_json(&app.app, None, fi_body(csv, &json!({}))).await;
    assert_eq!(status, 401);
    let (status, preview) = post_fi_json(
        &app.app,
        Some(&app.lawyer_session),
        fi_body(csv, &json!({})),
    )
    .await;
    assert_eq!(status, 200, "{preview}");
    assert_eq!(preview["dry_run"], true);
    assert_eq!(preview["new"], 2);
    assert_eq!(
        fi_counts(pg).await,
        before,
        "default preview must write nothing"
    );
    let malformed = format!("{csv}C,UNKNOWN,+15555550456,2026-10-30T18:02:00Z,1,bad,ignored\n");
    let (status, _) = post_fi_json(
        &app.app,
        Some(&app.lawyer_session),
        fi_body(&malformed, &json!({"dry_run":false})),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(
        fi_counts(pg).await,
        before,
        "invalid last row must not commit an earlier valid row"
    );
    let (status, _) = post_fi_json(
        &app.app,
        Some(&app.lawyer_session),
        fi_body(csv, &json!({"brand":"neon","dry_run":false})),
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(fi_counts(pg).await, before);
    let (status, first) = post_fi_json(
        &app.app,
        Some(&app.lawyer_session),
        fi_body(csv, &json!({"dry_run":false})),
    )
    .await;
    assert_eq!(status, 201, "{first}");
    assert_eq!(first["imported"].as_array().unwrap().len(), 2);
    let after = fi_counts(pg).await;
    assert_eq!(after[0], before[0] + 2);
    assert_eq!(after[2], before[2] + 2);
    assert_eq!(after[3], before[3], "metadata adds no Assets");
    assert_eq!(after[4], before[4], "historical metadata adds no billing");
    for accepted in first["imported"].as_array().unwrap() {
        let id: Uuid = accepted["message_id"].as_str().unwrap().parse().unwrap();
        let message = store::inbox::message_by_id(&app.surreal, id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.delivery_state, store::inbox::delivery::RECEIVED);
        assert!(
            message.author_person_id.is_none(),
            "CSV direction cannot establish the original human author"
        );
        assert!(message.raw_storage_key.is_none());
        assert!(!message.body.contains("never archive me"));
        let conversation = store::inbox::conversation_by_id(&app.surreal, message.conversation_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(conversation.project_id, None);
        assert!(conversation.provider_account.starts_with("google_fi:"));
    }
    let parked = store::billed_calls::parked_calls(pg, &app.surreal, admin)
        .await
        .unwrap();
    let fi_call = parked
        .iter()
        .find(|call| call.provider_message_id.starts_with("google_fi:"))
        .unwrap();
    assert_eq!(fi_call.duration_seconds, Some(61));
    assert_eq!(fi_call.tenths, None);
    assert_fi_reporting_pages(&app, admin).await;
    let report = store::communication_dashboard::for_brand(pg, &app.surreal, admin, "fi-http")
        .await
        .unwrap();
    assert_eq!(
        (
            report.calls,
            report.sms,
            report.billed_tenths,
            report.parked_calls
        ),
        (1, 1, 0, 1)
    );
    assert!(
        report.registered_numbers.is_empty(),
        "Fi must not manufacture a Twilio registration"
    );
    let (status, replay) = post_fi_json(
        &app.app,
        Some(&app.lawyer_session),
        fi_body(csv, &json!({"project":"alpha-matter","dry_run":false})),
    )
    .await;
    assert_eq!(status, 201, "{replay}");
    assert!(replay["imported"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["deduped"] == true));
    assert_eq!(fi_counts(pg).await, after);
    let report = store::communication_dashboard::for_brand(pg, &app.surreal, admin, "fi-http")
        .await
        .unwrap();
    assert_eq!(
        (
            report.calls,
            report.sms,
            report.billed_tenths,
            report.parked_calls
        ),
        (1, 1, 0, 0)
    );
    let assignments:i64=sqlx::query_scalar("SELECT count(*) FROM inbox_assignment WHERE conversation_id IN (SELECT id FROM inbox_conversation WHERE provider_account='google_fi:+15555550123')")
        .fetch_one(pg.pool()).await.unwrap();
    let (status, _) = post_fi_json(
        &app.app,
        Some(&app.lawyer_session),
        fi_body(csv, &json!({"project":"alpha-matter","dry_run":false})),
    )
    .await;
    assert_eq!(status, 201);
    let repeated_assignments:i64=sqlx::query_scalar("SELECT count(*) FROM inbox_assignment WHERE conversation_id IN (SELECT id FROM inbox_conversation WHERE provider_account='google_fi:+15555550123')")
        .fetch_one(pg.pool()).await.unwrap();
    assert_eq!(assignments, repeated_assignments);
    let changed = csv.replace(",61,fi-call,", ",62,fi-call,");
    let (status, conflict) = post_fi_json(
        &app.app,
        Some(&app.lawyer_session),
        fi_body(&changed, &json!({"dry_run":false})),
    )
    .await;
    assert_eq!(status, 409, "{conflict}");
    assert_eq!(fi_counts(pg).await, after);
    // Both HTTP batches may have preflighted an unassigned native identity.
    // Their later Project writes still need one initial-attribution winner.
    let other = store::projects::create(
        &fixture.surreal,
        &store::projects::NewProject {
            code: "fi-race-other".into(),
            name: "Synthetic Other Project".into(),
            status: "open".into(),
            entity_id: project.entity_id,
            firm_id: Some(firm),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    store::participation::add_participant(
        &fixture.surreal,
        &store::participation::AddParticipantCommand {
            project_id: other.id,
            person_id: actor,
            dri: store::participation::DriRequest::Unchanged,
            actor: store::participation::DriActor::System,
        },
    )
    .await
    .unwrap();
    store::postgres_mirror::mirror(
        &fixture.surreal,
        pg,
        &store::postgres_mirror::MirrorKey::Project(other.id),
    )
    .await
    .unwrap();
    let race_csv="type,way,phone,at,length,id,content\nS,O,+15555550999,2026-10-30T18:09:00Z,,fi-race,unmapped-content\n";
    let (status, race) = post_fi_json(
        &app.app,
        Some(&app.lawyer_session),
        fi_body(race_csv, &json!({"dry_run":false})),
    )
    .await;
    assert_eq!(status, 201, "{race}");
    let conversation: Uuid = race["imported"][0]["conversation_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let mut tasks = Vec::new();
    for destination in [project_id, other.id] {
        let db = fixture.surreal.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store::inbox::assign(&db, actor, conversation, destination).await
        }));
    }
    let mut winners = Vec::new();
    for task in tasks {
        match task.await.unwrap() {
            Ok(assignment) => winners.push(assignment),
            Err(store::inbox::InboxError::Denied) => {}
            Err(store::inbox::InboxError::Db(error)) => assert!(
                error.contains("busy"),
                "unexpected attribution failure: {error}"
            ),
            Err(error) => panic!("unexpected attribution failure: {error}"),
        }
    }
    assert_eq!(
        winners.len(),
        1,
        "conflicting destinations cannot both acknowledge success"
    );
    let winner = &winners[0];
    let current = store::inbox::conversation_by_id(&fixture.surreal, conversation)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.project_id, Some(winner.to_project_id));
    let repeat = store::inbox::assign(&fixture.surreal, actor, conversation, winner.to_project_id)
        .await
        .unwrap();
    assert_eq!(
        repeat.id, winner.id,
        "same-Project repair reuses the assignment"
    );
    let loser = if winner.to_project_id == project_id {
        other.id
    } else {
        project_id
    };
    assert!(matches!(
        store::inbox::assign(&fixture.surreal, actor, conversation, loser).await,
        Err(store::inbox::InboxError::Denied)
    ));
    let assignments: i64 =
        sqlx::query_scalar("SELECT count(*) FROM inbox_assignment WHERE conversation_id=$1")
            .bind(conversation)
            .fetch_one(pg.pool())
            .await
            .unwrap();
    assert_eq!(assignments, 1);
    let bills: i64 = sqlx::query_scalar("SELECT count(*) FROM billed_call")
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(
        bills, after[4],
        "Fi SMS attribution creates no BilledCall rows"
    );
    assert_call_coordinators_do_not_starve_the_native_pool(&app, pg, actor, firm, project_id).await;
    Box::pin(assert_fi_assignment_authorization(
        &app, pg, &project, admin,
    ))
    .await;
}

async fn assert_fi_assignment_rights_before_writes(
    fixture: &Fixture,
    pg: &store::postgres::PostgresDb,
    csv: &str,
) {
    let admin = store::persons::create(
        &fixture.surreal,
        &NewPerson {
            name: "Synthetic Unrelated Admin".into(),
            email: "fi-outsider-admin@example.test".into(),
            role: Role::Admin,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut session = portal::SessionData::fresh("fi-outsider-admin", Role::Admin);
    session.person_id = Some(admin.id);
    let before = fi_counts(pg).await;
    for dry_run in [true, false] {
        let (status, _) = post_fi_json(
            &fixture.app,
            Some(&session),
            fi_body(csv, &json!({"project":"alpha-matter", "dry_run":dry_run})),
        )
        .await;
        assert_eq!(
            status, 403,
            "Admin cannot assign imported history to a Project"
        );
        assert_eq!(
            fi_counts(pg).await,
            before,
            "denied assignment must not persist a conversation, message, ingress, or audit"
        );
    }
}

/// More independent coordinators than the actual ten-connection cached pool.
/// Distinct legitimate callbacks and assignments must complete, not return Busy.
#[expect(
    clippy::too_many_lines,
    reason = "two barrier phases exercise both nested-pool coordinators against the exact isolated native authority"
)]
async fn assert_call_coordinators_do_not_starve_the_native_pool(
    fixture: &Fixture,
    pg: &store::postgres::PostgresDb,
    actor: Uuid,
    firm: Uuid,
    project: Uuid,
) {
    const CALLS: usize = 12;
    let started: chrono::DateTime<chrono::Utc> = "2026-10-30T20:00:00Z".parse().unwrap();
    let mut inputs = Vec::with_capacity(CALLS);
    for index in 0..CALLS {
        let sid = format!("CApool-liveness-{index}");
        let address = format!("+1555555{index:04}");
        let saved = store::inbox::ingest_inbound(
            &fixture.surreal,
            &store::inbox::IngestInbound {
                project_id: None,
                firm_id: firm,
                brand_key: Some("fi-http"),
                interaction_type: None,
                provider_post_id: None,
                parent_provider_message_id: None,
                visibility: Some("private"),
                channel: store::inbox::channel::VOICE,
                provider_account: "+15555550888",
                provider_message_id: &sid,
                provider_thread_ref: None,
                external_address: &address,
                external_name: None,
                subject: None,
                body: "Synthetic completed voice-call metadata",
                source_page: Some("+15555550888"),
                source_channel: Some(store::inbox::channel::VOICE),
                matter_consent: false,
                promotional_opt_in: false,
                email_conversation_id: None,
                attachments: Vec::new(),
                occurred_at: "2026-10-30T20:00:00Z",
                raw_storage_key: None,
            },
        )
        .await
        .unwrap();
        let input = store::billed_calls::NewBilledCall {
            call_sid: sid,
            conversation_id: saved.conversation_id,
            firm_id: firm,
            duration_seconds: 61,
            call_started_at: started,
        };
        store::billed_calls::record(pg, &input).await.unwrap();
        inputs.push(input);
    }
    let unique = inputs
        .iter()
        .map(|input| input.conversation_id)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        unique.len(),
        CALLS,
        "the saturation test must use distinct Conversation lock keys"
    );

    // Native Conversation lookup happens inside each callback's outer SQL tx.
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(CALLS));
    let mut tasks = Vec::with_capacity(CALLS);
    for input in &inputs {
        let input = input.clone();
        let db = fixture.surreal.clone();
        let pg = pg.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store::billed_calls::record_for_conversation(&pg, &db, &input).await
        }));
    }
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        for task in tasks {
            let row = task
                .await
                .unwrap()
                .expect("distinct legitimate callbacks must queue rather than fail pool admission");
            assert_eq!(row.duration_seconds, 61);
            assert_eq!(row.tenths, 1);
            assert_eq!(row.project_id, None);
        }
    })
    .await
    .expect("twelve distinct callbacks must not deadlock the ten-connection native pool");

    // Attribution also retains its own SQL transaction over native assign_on.
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(CALLS));
    let mut tasks = Vec::with_capacity(CALLS);
    for input in &inputs {
        let conversation = input.conversation_id;
        let db = fixture.surreal.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store::inbox::assign(&db, actor, conversation, project).await
        }));
    }
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        for task in tasks {
            let assignment = task
                .await
                .unwrap()
                .expect("distinct legitimate attributions must queue rather than return Busy");
            assert_eq!(assignment.to_project_id, project);
        }
    })
    .await
    .expect("twelve distinct attributions must not deadlock the ten-connection native pool");

    for input in &inputs {
        let row = store::billed_calls::find_by_call_sid(pg, &input.call_sid)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.project_id, Some(project));
        assert_eq!(row.duration_seconds, 61);
        assert_eq!(row.tenths, 1);
        let conversation =
            store::inbox::conversation_by_id(&fixture.surreal, input.conversation_id)
                .await
                .unwrap()
                .unwrap();
        assert_eq!(conversation.project_id, Some(project));
    }
    let conversations = unique.into_iter().collect::<Vec<_>>();
    let assignments: i64 =
        sqlx::query_scalar("SELECT count(*) FROM inbox_assignment WHERE conversation_id=ANY($1)")
            .bind(&conversations)
            .fetch_one(pg.pool())
            .await
            .unwrap();
    assert_eq!(assignments, i64::try_from(CALLS).unwrap());
}

/// Exercise the SSR loaders through authenticated HTTP requests.
async fn assert_fi_reporting_pages(fixture: &Fixture, admin: Uuid) {
    let sessions = portal::SessionStore::new(portal::test_support::TEST_SESSION_KEY);
    let router = portal::router(portal::test_support::app_state(fixture.surreal.clone()).await);
    let lawyer = fixture.lawyer_session.person_id.unwrap();
    for (role, actor, path, shows_report) in [
        (
            Role::Admin,
            admin,
            "/app/admin/communications?brand=fi-http",
            true,
        ),
        (
            Role::Admin,
            admin,
            "/app/admin/communications?brand=",
            false,
        ),
        (Role::Lawyer, lawyer, "/app/inbox/calls", false),
    ] {
        let session = portal::SessionData {
            person_id: Some(actor),
            ..portal::SessionData::fresh("synthetic-report-reader", role)
        };
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", sessions.encode(&session)),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "the report route must admit its reader"
        );
        let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
            .await
            .unwrap();
        let html = String::from_utf8_lossy(&bytes);
        assert!(
            html.contains("Parked calls"),
            "the parked queue must render"
        );
        assert!(
            html.contains("google_fi:"),
            "the queue must include the imported call"
        );
        assert!(
            html.contains("61"),
            "the queue must retain the provider duration"
        );
        assert!(
            html.contains("Not billed"),
            "Fi history must remain unbilled"
        );
        assert_eq!(html.contains("Registered numbers"), shows_report);
        assert_eq!(html.contains("communications-brand"), role == Role::Admin);
        if shows_report {
            assert!(html.contains("No registered numbers."));
            assert!(html.contains("Billed tenths"));
        }
    }
}

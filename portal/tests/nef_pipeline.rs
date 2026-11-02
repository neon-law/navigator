//! The portal-side NEF pipeline (LAW-171): when the `inbox process` door
//! applies a message that classified as a Notice of Electronic Filing on an
//! attributed matter, the email original, its attachment assets, and the
//! docket entry land on the matter — the same `store::nef` parsing the CLI's
//! `docket ingest` runs. Messages that cannot be filed without guessing (no
//! matter, an unparseable notice, no case number) stay in the triage
//! backlog unprocessed.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request};
use axum::Router;
use portal::api::ApiState;
use serde_json::{json, Value};
use store::firms::{FirmMembership, NewPersonFirmRole};
use store::persons::{NewPerson, Role};
use store::surreal::SurrealDb;
use store::test_support::{ensure_person, mem_surreal, seed_firm_with_brand};
use tower::ServiceExt;
use uuid::Uuid;

struct Fixture {
    app: Router,
    lawyer_session: portal::SessionData,
    surreal: SurrealDb,
    firm_id: Uuid,
    storage: Arc<dyn cloud::StorageService>,
}

async fn fixture() -> Fixture {
    let db = mem_surreal().await;
    let firm_id = seed_firm_with_brand(&db, "neon").await;
    let lawyer = ensure_person(
        &db,
        &NewPerson {
            name: "Synthetic Lawyer".into(),
            email: "synthetic-lawyer@example.com".into(),
            role: Role::Lawyer,
            ..Default::default()
        },
    )
    .await;
    store::firms::add_membership(
        &db,
        &NewPersonFirmRole {
            person_id: lawyer.id,
            firm_id,
            membership: FirmMembership::Lawyer,
            is_dri: false,
        },
    )
    .await
    .expect("enroll the synthetic lawyer in the seeded firm");
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
        canonical_base_url: None,
    };
    let session = portal::SessionData {
        person_id: Some(lawyer.id),
        ..portal::SessionData::fresh("synthetic-lawyer-sub", Role::Lawyer)
    };
    Fixture {
        app: portal::api::routes().with_state(api_state),
        lawyer_session: session,
        surreal: state.surreal,
        firm_id,
        storage: state.storage,
    }
}

async fn seed_matter(db: &SurrealDb, firm_id: Uuid, code: &str) -> Uuid {
    store::projects::create(
        db,
        &store::projects::NewProject {
            code: code.into(),
            name: code.to_string(),
            status: "open".into(),
            firm_id: Some(firm_id),
            ..Default::default()
        },
    )
    .await
    .expect("seed the synthetic matter")
    .id
}

/// Open an email conversation with one inbound hop whose raw `.eml` sits in
/// storage, the way the webhook's persist path leaves it, and ingest one
/// normalized inbox message linked to it.
async fn seed_notice(
    fixture: &Fixture,
    project_id: Option<Uuid>,
    raw: Option<(&str, &[u8])>,
    body: &str,
    attachments: Vec<store::inbox::IngestAttachment>,
) -> Uuid {
    let mut email_conversation_id = None;
    if let Some((raw_key, raw_bytes)) = raw {
        fixture
            .storage
            .put(raw_key, raw_bytes, "message/rfc822")
            .await
            .expect("archive the synthetic raw .eml");
        let opened = store::email_conversations::open(
            &fixture.surreal,
            &store::email_conversations::NewConversation {
                token: &format!("token-{}", Uuid::new_v4()),
                external_email: NEF_ADDRESS,
                external_name: None,
                subject: NEF_SUBJECT,
                person_id: None,
                notation_id: None,
            },
        )
        .await
        .expect("open the synthetic email conversation");
        store::email_conversations::append(
            &fixture.surreal,
            &store::email_conversations::NewMessage {
                conversation_id: opened,
                direction: store::email_conversations::DIRECTION_FROM_EXTERNAL,
                from_addr: NEF_ADDRESS,
                to_addr: "support@firm.example",
                subject: NEF_SUBJECT,
                body_text: body,
                raw_storage_key: Some(raw_key),
                ..Default::default()
            },
        )
        .await
        .expect("append the synthetic inbound hop");
        email_conversation_id = Some(opened);
    }
    let ingested = store::inbox::ingest_inbound(
        &fixture.surreal,
        &store::inbox::IngestInbound {
            project_id,
            firm_id: fixture.firm_id,
            brand_key: None,
            interaction_type: Some("message"),
            provider_post_id: None,
            parent_provider_message_id: None,
            visibility: None,
            channel: store::inbox::channel::EMAIL,
            provider_account: "support",
            provider_message_id: &format!("nef-{}", Uuid::new_v4()),
            provider_thread_ref: None,
            external_address: NEF_ADDRESS,
            external_name: None,
            subject: Some(NEF_SUBJECT),
            body,
            source_page: Some("support"),
            source_channel: Some(store::inbox::channel::EMAIL),
            matter_consent: false,
            promotional_opt_in: false,
            email_conversation_id,
            attachments,
            occurred_at: &chrono::Utc::now().to_rfc3339(),
        },
    )
    .await
    .expect("ingest the synthetic notice");
    ingested.message_id
}

async fn request(
    app: &Router,
    method: &str,
    uri: &str,
    session: Option<&portal::SessionData>,
    body: Option<Value>,
) -> (u16, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(session) = session {
        builder = builder.extension(session.clone());
    }
    let payload = body
        .map(|value| serde_json::to_vec(&value).expect("serialize synthetic request"))
        .unwrap_or_default();
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(payload)).expect("build request"))
        .await
        .expect("run request");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    let json: Value = serde_json::from_slice(&bytes).expect("response is JSON");
    (status, json)
}

const NEF_ADDRESS: &str = "United States Bankruptcy Court, N.D. Cal. <ECF_Reply@uscourts.gov>";
const NEF_SUBJECT: &str = "Notice of Electronic Filing";

/// The district-court fixture the CLI's docket-ingest tests parse: case
/// `9:26-cv-00042-XYZ`, document number 12, one main document plus two
/// notice-listed exhibits.
fn district_notice() -> Vec<u8> {
    std::fs::read("../cli/tests/fixtures/nef/district-motion.eml")
        .expect("read the synthetic NEF fixture")
}

#[tokio::test]
async fn a_notice_on_a_matter_files_the_original_its_attachment_and_the_entry() {
    let fixture = fixture().await;
    let project_id = seed_matter(&fixture.surreal, fixture.firm_id, "syn-nef-1").await;
    let attachment = store::assets::ingest_content(
        &fixture.surreal,
        &fixture.storage,
        b"%PDF-1.4 synthetic exhibit",
        "application/pdf",
    )
    .await
    .expect("seed the synthetic attachment asset");
    let raw = district_notice();
    let message = seed_notice(
        &fixture,
        Some(project_id),
        Some(("inbound/nef-pipeline.eml", &raw)),
        "This is an automatic email message from the CM/ECF system.",
        vec![store::inbox::IngestAttachment {
            asset_id: attachment,
            filename: "motion-to-dismiss.pdf".into(),
            sha256: store::assets::sha256_hex(b"%PDF-1.4 synthetic exhibit"),
        }],
    )
    .await;

    let (status, applied) = request(
        &fixture.app,
        "POST",
        "/app/api/inbox/process",
        Some(&fixture.lawyer_session),
        Some(json!({ "message": message, "apply": true })),
    )
    .await;

    assert_eq!(status, 200, "the apply answers 200: {applied}");
    assert_eq!(applied["route"], "nef");
    assert_eq!(
        applied["nef"]["outcome"], "filed",
        "the pipeline files the notice: {applied}"
    );
    assert_eq!(applied["nef"]["entry_number"], "12");
    assert_eq!(
        applied["nef"]["filed_documents"], 2,
        "the attachment asset plus the raw original were filed"
    );
    assert_eq!(
        applied["result"]["recorded_route"], "nef",
        "the recording still shows in the message view"
    );

    // The case is resolved (not guessed) by the docket number the notice
    // prints, and the entry is recorded with the filed document attached.
    let case =
        store::cases::find_case_by_docket_number(&fixture.surreal, project_id, "9:26-cv-00042-XYZ")
            .await
            .expect("read the resolved case")
            .expect("the case resolved from the notice's own masthead");
    let docket = store::cases::docket(&fixture.surreal, case.id)
        .await
        .expect("read the docket");
    assert_eq!(docket.len(), 1, "exactly one entry was recorded");
    assert_eq!(docket[0].entry_number, "12");
    let linked: Option<Uuid> = applied["nef"]["document_asset_id"]
        .as_str()
        .and_then(|id| Uuid::parse_str(id).ok());
    assert_eq!(
        docket[0].document_asset_id, linked,
        "the entry links the filed DKT document the outcome reports"
    );
    assert_ne!(
        docket[0].document_asset_id,
        Some(attachment),
        "the pipeline filed its own DKT copy rather than linking the pre-ingest asset"
    );

    // Every filed copy is internal: the pipeline never guesses a
    // client-visible release.
    let assets = store::assets::for_project(&fixture.surreal, project_id)
        .await
        .expect("read the matter's assets");
    assert!(
        assets.len() >= 2,
        "the attachment and the original are filed"
    );
    assert!(
        assets
            .iter()
            .all(|asset| asset.visibility == store::documents::visibility::INTERNAL),
        "a notice never files client-visible"
    );
}

#[tokio::test]
async fn a_rerun_reports_already_recorded_and_files_nothing_new() {
    let fixture = fixture().await;
    let project_id = seed_matter(&fixture.surreal, fixture.firm_id, "syn-nef-2").await;
    let raw = district_notice();
    let message = seed_notice(
        &fixture,
        Some(project_id),
        Some(("inbound/nef-rerun.eml", &raw)),
        "This is an automatic email message from the CM/ECF system.",
        Vec::new(),
    )
    .await;

    let (status, first) = request(
        &fixture.app,
        "POST",
        "/app/api/inbox/process",
        Some(&fixture.lawyer_session),
        Some(json!({ "message": message, "apply": true })),
    )
    .await;
    assert_eq!(status, 200, "the first run answers 200: {first}");
    assert_eq!(first["nef"]["outcome"], "filed", "{first}");

    let (_, second) = request(
        &fixture.app,
        "POST",
        "/app/api/inbox/process",
        Some(&fixture.lawyer_session),
        Some(json!({ "message": message, "apply": true })),
    )
    .await;
    assert_eq!(
        second["nef"]["outcome"], "already_recorded",
        "the rerun reports the recorded entry: {second}"
    );
    let case =
        store::cases::find_case_by_docket_number(&fixture.surreal, project_id, "9:26-cv-00042-XYZ")
            .await
            .expect("read the resolved case")
            .expect("the case persists");
    let docket = store::cases::docket(&fixture.surreal, case.id)
        .await
        .expect("read the docket");
    assert_eq!(docket.len(), 1, "the rerun filed no duplicate entry");
}

#[tokio::test]
async fn a_matterless_notice_stays_in_the_backlog() {
    let fixture = fixture().await;
    let raw = district_notice();
    let message = seed_notice(
        &fixture,
        None,
        Some(("inbound/nef-matterless.eml", &raw)),
        "This is an automatic email message from the CM/ECF system.",
        Vec::new(),
    )
    .await;

    let (status, applied) = request(
        &fixture.app,
        "POST",
        "/app/api/inbox/process",
        Some(&fixture.lawyer_session),
        Some(json!({ "message": message, "apply": true })),
    )
    .await;

    assert_eq!(status, 200, "the apply answers 200: {applied}");
    assert_eq!(applied["route"], "nef");
    assert_eq!(
        applied["nef"]["outcome"], "no_project",
        "a message with no matter is never filed by guessing: {applied}"
    );
    assert_eq!(
        applied["result"]["recorded_at"],
        Value::Null,
        "an unfiled notice records no processing time"
    );

    let (status, backlog) = request(
        &fixture.app,
        "GET",
        "/app/api/inbox/unprocessed",
        Some(&fixture.lawyer_session),
        None,
    )
    .await;
    assert_eq!(status, 200, "the backlog answers 200: {backlog}");
    let rows = backlog.as_array().expect("the backlog is a JSON array");
    assert!(
        rows.iter().any(|m| m["id"] == message.to_string()),
        "the unfiled notice stays in the triage backlog"
    );
}

#[tokio::test]
async fn an_unparseable_message_on_a_matter_stays_in_the_backlog() {
    let fixture = fixture().await;
    let project_id = seed_matter(&fixture.surreal, fixture.firm_id, "syn-nef-3").await;
    let message = seed_notice(
        &fixture,
        Some(project_id),
        None,
        "The clerk called about the hearing; no notice text follows.",
        Vec::new(),
    )
    .await;

    let (status, applied) = request(
        &fixture.app,
        "POST",
        "/app/api/inbox/process",
        Some(&fixture.lawyer_session),
        Some(json!({ "message": message, "apply": true })),
    )
    .await;

    assert_eq!(status, 200, "the apply answers 200: {applied}");
    assert_eq!(applied["route"], "nef", "the classifier still routes it");
    assert_eq!(
        applied["nef"]["outcome"], "not_a_notice",
        "an unparseable notice is never guessed into a docket entry: {applied}"
    );
    assert_eq!(
        applied["result"]["recorded_at"],
        Value::Null,
        "an unfiled notice records no processing time"
    );
}

#[tokio::test]
async fn a_notice_without_a_document_number_files_only_the_original() {
    let fixture = fixture().await;
    let project_id = seed_matter(&fixture.surreal, fixture.firm_id, "syn-nef-4").await;
    // A body-only notice: no raw .eml hop, so nothing can be filed for the
    // original either. The parser reads the stored body text.
    let body = "U.S. District Court\n\nDistrict of Nowhere\n\nNotice of Electronic Filing\n\n\
                The following transaction was entered on 3/6/2026 at 8:00 AM PST and filed on 3/6/2026\n\
                Case Name: Cruller v. Prine\nCase Number: 9:26-cv-00042-XYZ\n\
                Document Number: No document attached\n\nDocket Text:\n\
                MINUTE ORDER setting a hearing.\n\n";
    let message = seed_notice(&fixture, Some(project_id), None, body, Vec::new()).await;

    let (status, applied) = request(
        &fixture.app,
        "POST",
        "/app/api/inbox/process",
        Some(&fixture.lawyer_session),
        Some(json!({ "message": message, "apply": true })),
    )
    .await;

    assert_eq!(status, 200, "the apply answers 200: {applied}");
    assert_eq!(applied["nef"]["outcome"], "text_only", "{applied}");
    assert_ne!(
        applied["result"]["recorded_at"],
        Value::Null,
        "a notice with no ordinal to record is still finished"
    );
    let case =
        store::cases::find_case_by_docket_number(&fixture.surreal, project_id, "9:26-cv-00042-XYZ")
            .await
            .expect("read the resolved case")
            .expect("the case resolved from the body text");
    let docket = store::cases::docket(&fixture.surreal, case.id)
        .await
        .expect("read the docket");
    assert!(
        docket.is_empty(),
        "a notice with no document number records nothing on the docket"
    );
}

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
    let project = store::projects::create(
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
    .expect("seed the synthetic matter");
    let lawyer = store::persons::find_by_email_ci(db, "synthetic-lawyer@example.com")
        .await
        .expect("read fixture lawyer")
        .expect("fixture lawyer exists");
    store::projects::add_participation(
        db,
        project.id,
        lawyer.id,
        store::projects::participation_for_role(lawyer.role),
    )
    .await
    .expect("enroll fixture lawyer on matter");
    project.id
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
    let provider_message_id = format!("nef-{}", Uuid::new_v4());
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
        // A prior hop must never supply the source for the current message.
        let prior_key = format!("{raw_key}.prior");
        fixture
            .storage
            .put(
                &prior_key,
                b"Subject: prior\r\n\r\nUnrelated message",
                "message/rfc822",
            )
            .await
            .expect("archive an unrelated prior hop");
        store::email_conversations::append(
            &fixture.surreal,
            &store::email_conversations::NewMessage {
                conversation_id: opened,
                direction: store::email_conversations::DIRECTION_FROM_EXTERNAL,
                from_addr: NEF_ADDRESS,
                to_addr: "support@firm.example",
                subject: "Prior message",
                body_text: "Unrelated message",
                raw_storage_key: Some(&prior_key),
                provider_message_id: Some("unrelated-prior-message"),
                ..Default::default()
            },
        )
        .await
        .expect("append a prior hop before the notice");
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
                provider_message_id: Some(&provider_message_id),
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
            provider_message_id: &provider_message_id,
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
            raw_storage_key: None,
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
    std::fs::read("../store/tests/fixtures/nef/district-motion.eml")
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
    assert_ne!(
        applied["result"]["recorded_at"],
        Value::Null,
        "the run is recorded even when the pipeline filed nothing"
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
    let saved = rows
        .iter()
        .find(|row| row["id"] == message.to_string())
        .unwrap();
    assert_eq!(saved["recorded_route"], "nef");
    assert_eq!(saved["recorded_nef_outcome"], "no_project");
    assert_eq!(saved["recorded_at"], applied["result"]["recorded_at"]);
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
    assert_ne!(
        applied["result"]["recorded_at"],
        Value::Null,
        "the run is recorded even when the notice cannot be parsed"
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
        "the unparseable notice stays in the triage backlog"
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

#[tokio::test]
async fn firm_membership_without_matter_participation_cannot_file_a_notice() {
    let fixture = fixture().await;
    let project_id = seed_matter(&fixture.surreal, fixture.firm_id, "syn-nef-auth").await;
    let participation = store::projects::participation_for_person(
        &fixture.surreal,
        fixture.lawyer_session.person_id.unwrap(),
        project_id,
    )
    .await
    .unwrap()
    .unwrap();
    store::projects::remove_participation(&fixture.surreal, participation.id)
        .await
        .unwrap();
    let raw = district_notice();
    let message = seed_notice(
        &fixture,
        Some(project_id),
        Some(("inbound/denied.eml", &raw)),
        "This is an automatic email message from the CM/ECF system.",
        Vec::new(),
    )
    .await;
    let (status, _) = request(
        &fixture.app,
        "POST",
        "/app/api/inbox/process",
        Some(&fixture.lawyer_session),
        Some(json!({"message": message, "apply": true})),
    )
    .await;
    assert_eq!(status, 404);
    assert!(store::cases::for_project(&fixture.surreal, project_id)
        .await
        .unwrap()
        .is_empty());
    assert!(store::inbox::process_state(&fixture.surreal, message)
        .await
        .unwrap()
        .unwrap()
        .processed_at
        .is_none());
}

#[tokio::test]
async fn a_notice_missing_its_case_number_reports_the_specific_refusal() {
    let fixture = fixture().await;
    let project_id = seed_matter(&fixture.surreal, fixture.firm_id, "syn-nef-no-case").await;
    let body =
        "Notice of Electronic Filing\nDocument Number: 12\nDocket Text:\nMotion to dismiss.\n";
    let raw = format!("Subject: Notice of Electronic Filing\r\n\r\n{body}");
    for (raw, body) in [
        (None, body),
        (
            Some(("inbound/no-case.eml", raw.as_bytes())),
            "Truncated body",
        ),
    ] {
        let message = seed_notice(&fixture, Some(project_id), raw, body, Vec::new()).await;
        let (status, applied) = request(
            &fixture.app,
            "POST",
            "/app/api/inbox/process",
            Some(&fixture.lawyer_session),
            Some(json!({"message": message, "apply": true})),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(applied["nef"]["outcome"], "no_case_number");
    }
    assert!(store::cases::for_project(&fixture.surreal, project_id)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn a_later_notice_files_its_original_when_the_entry_is_already_recorded() {
    let fixture = fixture().await;
    let project_id = seed_matter(&fixture.surreal, fixture.firm_id, "syn-nef-existing").await;
    let raw = district_notice();
    let body = store::nef::parse_eml(&raw).unwrap();
    let text = format!("Notice of Electronic Filing\nCase Number: {}\nDocument Number: 12\nDocket Text:\nMotion to dismiss.\n", body.case_no);
    let first = seed_notice(&fixture, Some(project_id), None, &text, Vec::new()).await;
    let (_, applied) = request(
        &fixture.app,
        "POST",
        "/app/api/inbox/process",
        Some(&fixture.lawyer_session),
        Some(json!({"message": first, "apply": true})),
    )
    .await;
    assert_eq!(applied["nef"]["filed_documents"], 0);
    let later = seed_notice(
        &fixture,
        Some(project_id),
        Some(("inbound/later.eml", &raw)),
        &text,
        Vec::new(),
    )
    .await;
    let (status, applied) = request(
        &fixture.app,
        "POST",
        "/app/api/inbox/process",
        Some(&fixture.lawyer_session),
        Some(json!({"message": later, "apply": true})),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(applied["nef"]["outcome"], "already_recorded");
    let case =
        store::cases::find_case_by_docket_number(&fixture.surreal, project_id, &body.case_no)
            .await
            .unwrap()
            .unwrap();
    let entries = store::cases::docket(&fixture.surreal, case.id)
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert!(
        entries[0].document_asset_id.is_none(),
        "the email is not the court document"
    );
    let documents = store::assets::for_project(&fixture.surreal, project_id)
        .await
        .unwrap();
    assert_eq!(documents.len(), 1, "the later message original is filed");
    let attachment = store::assets::ingest_content(
        &fixture.surreal,
        &fixture.storage,
        b"%PDF-1.4 synthetic later court document",
        "application/pdf",
    )
    .await
    .unwrap();
    let final_notice = seed_notice(
        &fixture,
        Some(project_id),
        None,
        &text,
        vec![store::inbox::IngestAttachment {
            asset_id: attachment,
            filename: "court-document.pdf".into(),
            sha256: store::assets::sha256_hex(b"%PDF-1.4 synthetic later court document"),
        }],
    )
    .await;
    let (status, applied) = request(
        &fixture.app,
        "POST",
        "/app/api/inbox/process",
        Some(&fixture.lawyer_session),
        Some(json!({"message": final_notice, "apply": true})),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(applied["nef"]["outcome"], "already_recorded");
    let entries = store::cases::docket(&fixture.surreal, case.id)
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert!(
        entries[0].document_asset_id.is_some(),
        "a later court document repairs the pending source"
    );
}

#[tokio::test]
async fn support_intake_retains_the_original_without_threading_or_a_message_id() {
    let fixture = fixture().await;
    let email = portal::inbound_email::InboundEmail {
        from: "sender@example.com".into(),
        to: "support@neonlaw.com".into(),
        text: "Synthetic notice".into(),
        raw: district_notice(),
        ..Default::default()
    };
    let saved = portal::inbox::ingest_support_email(&fixture.surreal, &fixture.storage, &email)
        .await
        .unwrap()
        .unwrap();
    let message = store::inbox::message_by_id(&fixture.surreal, saved.message_id)
        .await
        .unwrap()
        .unwrap();
    let key = message
        .raw_storage_key
        .expect("the normalized message retains its source");
    assert_eq!(fixture.storage.get(&key).await.unwrap().bytes, email.raw);
    let conversation = store::inbox::conversation_by_id(&fixture.surreal, saved.conversation_id)
        .await
        .unwrap()
        .unwrap();
    assert!(conversation.email_conversation_id.is_none());
}

#[tokio::test]
async fn overlapping_applies_file_each_original_and_attachment_only_once() {
    let fixture = fixture().await;
    let project_id = seed_matter(&fixture.surreal, fixture.firm_id, "syn-nef-overlap").await;
    let bytes = b"%PDF-1.4 synthetic attachment";
    let asset =
        store::assets::ingest_content(&fixture.surreal, &fixture.storage, bytes, "application/pdf")
            .await
            .unwrap();
    let raw = district_notice();
    let message = seed_notice(
        &fixture,
        Some(project_id),
        Some(("inbound/overlap.eml", &raw)),
        "This is an automatic email message from the CM/ECF system.",
        vec![store::inbox::IngestAttachment {
            asset_id: asset,
            filename: "motion.pdf".into(),
            sha256: store::assets::sha256_hex(bytes),
        }],
    )
    .await;
    let gate = Arc::new(tokio::sync::Barrier::new(8));
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let app = fixture.app.clone();
        let session = fixture.lawyer_session.clone();
        let gate = gate.clone();
        tasks.spawn(async move {
            gate.wait().await;
            request(
                &app,
                "POST",
                "/app/api/inbox/process",
                Some(&session),
                Some(json!({"message": message, "apply": true})),
            )
            .await
        });
    }
    while let Some(result) = tasks.join_next().await {
        let (status, body) = result.unwrap();
        assert_eq!(status, 200, "overlapping apply succeeds: {body}");
    }
    let assets = store::assets::for_project(&fixture.surreal, project_id)
        .await
        .unwrap();
    assert_eq!(
        assets.len(),
        2,
        "one original and one attachment, with no duplicate revisions"
    );
    let cases = store::cases::for_project(&fixture.surreal, project_id)
        .await
        .unwrap();
    assert_eq!(cases.len(), 1);
    assert_eq!(
        store::cases::docket(&fixture.surreal, cases[0].id)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn an_archived_mail_slot_refuses_changed_bytes() {
    let fixture = fixture().await;
    let project_id = seed_matter(&fixture.surreal, fixture.firm_id, "syn-nef-immutable").await;
    let args = store::documents::IngestArgs {
        project_id,
        source: "email",
        filename: "original.eml",
        kind: "pleading",
        content_type: "message/rfc822",
        description: None,
        secondary_storage_key: None,
        visibility: "internal",
    };
    let identity = store::documents::DocumentIdentity {
        slug: Some("mail/synthetic/original"),
        ..Default::default()
    };
    let provenance = store::documents::MailProvenance {
        message_id: "synthetic",
        sender: "sender@example.com",
        received_at: "2026-11-03T12:00:00Z",
        subject: "Synthetic notice",
    };
    let first = store::documents::ingest_bytes_with_mail_provenance_once(
        &fixture.surreal,
        &fixture.storage,
        &args,
        &identity,
        &provenance,
        b"original bytes",
    )
    .await
    .unwrap();
    let error = store::documents::ingest_bytes_with_mail_provenance_once(
        &fixture.surreal,
        &fixture.storage,
        &args,
        &identity,
        &provenance,
        b"changed bytes",
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error,
        store::documents::IngestError::IdempotencyConflict(_)
    ));
    let stored = store::assets::for_project(&fixture.surreal, project_id)
        .await
        .unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].id, first.asset_id);
}

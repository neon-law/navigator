//! The litigation board on the sample-litigation fixture: a declared
//! `docket_deadline_board` page renders the case caption, its fact strip,
//! the docket timeline, the deadline board, key dates, the party roster, and
//! the e-filing receipts through the assembled router, and each reader sees
//! only their own lens. Every name and number is synthetic.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use store::persons::Role;
use store::test_support::mem_surreal;
use tower::ServiceExt;
use uuid::Uuid;

const CAPTION: &str = "Cruller v. Prine";
const COMPLAINT: &str = "COMPLAINT for trespass to land";
const MOTION: &str = "MOTION to Dismiss the synthetic complaint";
const FIRST_HEARING: &str = "Hearing on the synthetic motion";
const CLIENT_DATE: &str = "Notice of the synthetic hearing";
const FIRM_DATE: &str = "Draft the synthetic opposition";
const SERVED: &str = "Synthetic Opposing Counsel";
const REFERENCED: &str = "Synthetic Registered Agent";
const PUBLIC_TX: &str = "TX-SAMPLE-0001";
const INTERNAL_TX: &str = "TX-SAMPLE-0002";

struct Fixture {
    app: axum::Router,
    code: String,
    project_id: Uuid,
    client_cookie: String,
    lawyer_cookie: String,
    lawyer_csrf: String,
}

async fn person(surreal: &store::surreal::SurrealDb, name: &str, email: &str, role: Role) -> Uuid {
    store::persons::create(
        surreal,
        &store::persons::NewPerson::with_role(name, email, role),
    )
    .await
    .unwrap()
    .id
}

fn deadline(
    project_id: Uuid,
    authority: &str,
    calendar_role: &str,
    client_lead_days: Option<i64>,
) -> store::project_deadlines::NewDeadline {
    store::project_deadlines::NewDeadline {
        title: None,
        project_id,
        due_at: "2099-11-02T17:00:00-08:00".into(),
        authority_kind: "court_order".into(),
        authority: authority.into(),
        source: "lawyer".into(),
        source_document_id: None,
        owner_person_id: None,
        calendar_role: calendar_role.into(),
        internal_lead_days: Some(7),
        client_lead_days,
        replay_key: None,
    }
}

fn entry<'a>(
    case_id: Uuid,
    number: &'a str,
    kind: store::cases::EntryKind,
    title: &'a str,
    party: Option<&'a str>,
) -> store::cases::NewDocketEntry<'a> {
    store::cases::NewDocketEntry {
        case_id,
        entry_number: number,
        kind,
        title,
        party,
        filed_or_served_on: (!kind.is_appearance()).then_some("2026-03-04"),
        scheduled_on: None,
        supersedes: None,
        document_asset_id: None,
        notation_id: None,
    }
}

async fn fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let entity = store::entities::create(
        &surreal,
        &store::entities::NewEntity {
            name: "Dermot Cruller".into(),
            entity_type_id: store::test_support::SEED_ENTITY_TYPE_ID,
            jurisdiction_id: store::test_support::SEED_ENTITY_JURISDICTION_ID,
            phone: None,
            url: None,
            xero_id: None,
            firm_anchor_key: None,
        },
    )
    .await
    .unwrap();
    let project = store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: store::seed::SAMPLE_LITIGATION_CODE.into(),
            name: CAPTION.into(),
            status: "open".into(),
            entity_id: entity.id,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let client = person(
        &surreal,
        "Dermot Cruller",
        "timeline-client@example.com",
        Role::Client,
    )
    .await;
    store::projects::add_participation(&surreal, project.id, client, "client")
        .await
        .unwrap();
    let lawyer = person(
        &surreal,
        "Sample Counsel",
        "timeline-counsel@example.com",
        Role::Lawyer,
    )
    .await;
    store::projects::add_participation(&surreal, project.id, lawyer, "lawyer")
        .await
        .unwrap();

    let mut state = portal::test_support::app_state(surreal.clone()).await;
    let complaint_pdf = store::documents::ingest_bytes_as(
        &state.surreal,
        &state.assets_storage,
        &store::documents::IngestArgs {
            project_id: project.id,
            source: store::documents::source::UPLOAD,
            filename: "di-1-complaint.pdf",
            kind: "filing",
            content_type: "application/pdf",
            description: None,
            visibility: store::documents::visibility::CLIENT,
            secondary_storage_key: None,
        },
        &store::documents::DocumentIdentity {
            slug: Some("docket/a-26-000001/di-1-complaint.pdf"),
            ..Default::default()
        },
        b"%PDF-1.7 synthetic complaint",
    )
    .await
    .unwrap()
    .asset_id;
    seed_docket(&surreal, project.id, complaint_pdf).await;
    seed_records(&surreal, project.id, complaint_pdf).await;

    let sessions = portal::SessionStore::new(portal::test_support::TEST_SESSION_KEY);
    let session_for = |sub: &str, person_id: Uuid, role: Role| {
        let mut session = portal::SessionData::fresh(sub, role);
        session.person_id = Some(person_id);
        session
    };
    let cookie_for = |session: &portal::SessionData| {
        format!(
            "{}={}",
            portal::session::SESSION_COOKIE_NAME,
            sessions.encode(session)
        )
    };
    let lawyer_session = session_for("timeline-counsel", lawyer, Role::Lawyer);
    state.policy = portal::policy::PolicyClient::embedded().expect("embedded policy compiles");
    Fixture {
        app: portal::router(state),
        code: project.code,
        project_id: project.id,
        client_cookie: cookie_for(&session_for("timeline-client", client, Role::Client)),
        lawyer_cookie: cookie_for(&lawyer_session),
        lawyer_csrf: lawyer_session.csrf_token.clone(),
    }
}

/// The case, its docket, and two open Deadlines: one the client is warned
/// about and one that stays firm-side.
async fn seed_docket(surreal: &store::surreal::SurrealDb, project_id: Uuid, complaint_pdf: Uuid) {
    use store::cases::EntryKind;

    let case = store::cases::open_case(
        surreal,
        &store::cases::NewCase {
            project_id,
            caption: CAPTION,
            forum: Some("Synthetic County District Court"),
            jurisdiction: None,
            docket_number: Some("A-26-000001"),
            judge: None,
            posture: "plaintiff",
        },
    )
    .await
    .unwrap();
    let mut complaint = entry(
        case.id,
        "1",
        EntryKind::Pleading,
        COMPLAINT,
        Some("Plaintiff Dermot Cruller"),
    );
    complaint.document_asset_id = Some(complaint_pdf);
    store::cases::record_entry(surreal, &complaint)
        .await
        .unwrap();
    store::cases::record_entry(
        surreal,
        &entry(
            case.id,
            "5",
            EntryKind::Motion,
            MOTION,
            Some("Defendant Prine"),
        ),
    )
    .await
    .unwrap();
    let mut hearing = entry(case.id, "8", EntryKind::Hearing, FIRST_HEARING, None);
    hearing.scheduled_on = Some("2099-03-15T16:00:00Z".parse().unwrap());
    let first = store::cases::record_entry(surreal, &hearing).await.unwrap();
    let mut moved = entry(case.id, "9", EntryKind::Hearing, FIRST_HEARING, None);
    moved.scheduled_on = Some("2099-04-12T16:00:00Z".parse().unwrap());
    moved.supersedes = Some(first.id);
    store::cases::record_entry(surreal, &moved).await.unwrap();

    store::project_deadlines::create(
        surreal,
        &deadline(project_id, CLIENT_DATE, "hearing", Some(3)),
    )
    .await
    .unwrap();
    store::project_deadlines::create(surreal, &deadline(project_id, FIRM_DATE, "firm", None))
        .await
        .unwrap();
}

/// A Service, a Project Reference, and two e-filing submissions: one on the
/// client-visible complaint and one with no document.
async fn seed_records(surreal: &store::surreal::SurrealDb, project_id: Uuid, complaint_pdf: Uuid) {
    store::project_service::create(
        surreal,
        &store::project_service::NewService {
            project_id,
            document_id: complaint_pdf,
            recipient: SERVED.into(),
            counsel_for: Some("Defendant Prine".into()),
            method: "e_service".into(),
            served_on: "2026-03-05".into(),
            address: "counsel@example.com".into(),
            declarant: "Sample Counsel".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let agent = person(
        surreal,
        REFERENCED,
        "timeline-agent@example.com",
        Role::Client,
    )
    .await;
    store::project_references::reference(
        surreal,
        project_id,
        store::project_references::Party::person(agent),
        store::project_references::REASON_REGISTERED_AGENT,
    )
    .await
    .unwrap();
    for (transaction, document) in [(PUBLIC_TX, Some(complaint_pdf)), (INTERNAL_TX, None)] {
        store::efiling_transactions::record(
            surreal,
            &store::efiling_transactions::EfilingReport {
                project_id,
                transaction_id: transaction.into(),
                status: store::efiling_transactions::STATUS_SUBMITTED.into(),
                efsp: Some("synthetic".into()),
                court: Some("Synthetic County District Court".into()),
                case_number: Some("A-26-000001".into()),
                submitted_document_id: document,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }
}

fn pages() -> serde_json::Value {
    serde_json::json!({ "pages": [{
        "slug": "timeline",
        "kind": "docket_deadline_board",
        "title": "Case timeline",
        "lenses": {
            "lawyer": ["case_caption", "fact_strip", "docket_timeline", "deadline_board",
                       "party_roster", "efiling_receipts", "boundary_note",
                       "provenance_statement"],
            "client": ["case_caption", "fact_strip", "docket_timeline", "key_dates_strip",
                       "party_roster", "efiling_receipts", "boundary_note",
                       "provenance_statement"]
        }
    }]})
}

async fn synced() -> Fixture {
    let f = fixture().await;
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/app/api/projects/{}/pages", f.project_id))
                .header("cookie", &f.lawyer_cookie)
                .header("x-csrf-token", &f.lawyer_csrf)
                .header("content-type", "application/json")
                .body(Body::from(pages().to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    f
}

async fn get(f: &Fixture, path: &str, cookie: &str) -> (StatusCode, String) {
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn a_lawyer_reads_the_whole_docket_with_its_states_and_sides() {
    let f = synced().await;
    let (status, html) = get(
        &f,
        &format!("/app/projects/{}/timeline", f.code),
        &f.lawyer_cookie,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{html}");
    for section in ["case_caption", "fact_strip", "docket_timeline"] {
        assert!(
            html.contains(&format!("data-section=\"{section}\"")),
            "{section}: {html}"
        );
    }
    assert!(
        html.contains(CAPTION) && html.contains("A-26-000001"),
        "{html}"
    );
    assert!(html.contains(COMPLAINT), "{html}");
    assert!(html.contains("di-1-complaint.pdf"), "{html}");
    assert!(html.contains(MOTION), "{html}");
    assert!(html.contains("Source pending"), "{html}");
    assert!(html.contains("Link document"), "{html}");
    assert!(html.contains("matter-timeline__item--superseded"), "{html}");
    assert!(html.contains("Continues D.I. 8"), "{html}");
    assert!(html.contains(FIRM_DATE), "{html}");
    assert!(html.contains("matter-timeline__item--ahead"), "{html}");
    assert!(html.contains("data-side=\"ours\""), "{html}");
    assert!(html.contains("data-side=\"theirs\""), "{html}");
    assert!(html.contains("data-side=\"court\""), "{html}");
}

#[tokio::test]
async fn the_side_filter_is_a_query_link_the_server_honours() {
    let f = synced().await;
    let (status, html) = get(
        &f,
        &format!("/app/projects/{}/timeline?side=theirs", f.code),
        &f.lawyer_cookie,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(html.contains("data-filter=\"theirs\""), "{html}");
    assert!(html.contains(MOTION), "{html}");
    assert!(
        !html.contains(COMPLAINT),
        "our own filing is filtered out: {html}"
    );
}

#[tokio::test]
async fn a_client_sees_only_client_visible_entries_and_client_dates() {
    let f = synced().await;
    let (status, html) = get(
        &f,
        &format!("/app/projects/{}/timeline", f.code),
        &f.client_cookie,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(html.contains("data-lens=\"client\""), "{html}");
    assert!(html.contains(CAPTION), "{html}");
    assert!(html.contains(COMPLAINT), "{html}");
    assert!(html.contains(CLIENT_DATE), "{html}");
    assert!(
        !html.contains(MOTION),
        "an entry with no client-visible document stays firm-side"
    );
    assert!(
        !html.contains(FIRM_DATE),
        "a firm-only date stays firm-side"
    );
    assert!(!html.contains("Link document"), "{html}");
}

#[tokio::test]
async fn a_lawyer_reads_the_deadline_board_roster_and_receipts() {
    let f = synced().await;
    let (status, html) = get(
        &f,
        &format!("/app/projects/{}/timeline", f.code),
        &f.lawyer_cookie,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(html.contains("data-binding=\"Hard\""), "{html}");
    assert!(html.contains("data-section=\"party_roster\""), "{html}");
    assert!(html.contains("Defendant Prine"), "{html}");
    assert!(html.contains("Sample Counsel"), "{html}");
    assert!(html.contains(SERVED), "{html}");
    assert!(html.contains(REFERENCED), "{html}");
    assert!(
        html.contains(PUBLIC_TX) && html.contains(INTERNAL_TX),
        "{html}"
    );
}

#[tokio::test]
async fn a_client_reads_key_dates_and_no_conflicts_record_or_internal_receipt() {
    let f = synced().await;
    let (status, html) = get(
        &f,
        &format!("/app/projects/{}/timeline", f.code),
        &f.client_cookie,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(html.contains("data-section=\"key_dates_strip\""), "{html}");
    assert!(html.contains("Dermot Cruller"), "{html}");
    assert!(html.contains(PUBLIC_TX), "{html}");
    assert!(!html.contains(INTERNAL_TX), "{html}");
    assert!(!html.contains(SERVED), "the service list stays firm-side");
    assert!(
        !html.contains(REFERENCED),
        "a reference never reaches a client"
    );
    assert!(!html.contains("data-section=\"deadline_board\""), "{html}");
}

const SYNCED_CASE_NUMBER: &str = "00CV000999";
const SYNCED_JUDGE: &str = "Hon. Synthetic Jurist";
const SYNCED_DEFENDANT: &str = "Synthetic Prine Holdings";

fn matter() -> serde_json::Value {
    serde_json::json!({ "matter": {
        "court": ["SYNTHETIC SUPERIOR COURT", "COUNTY OF SAMPLE"],
        "case_number": SYNCED_CASE_NUMBER,
        "judicial_officer": SYNCED_JUDGE,
        "department": "12",
        "filed_on": "2026-02-02",
        "parties": [
            { "name": "Dermot Cruller", "role": "Plaintiff" },
            { "name": SYNCED_DEFENDANT, "role": "Defendant" }
        ],
        "counsel": {
            "name": "Sample Counsel",
            "bar_jurisdiction": "California",
            "bar_number": "000000",
            "address": ["1 Example Way"],
            "telephone": "(000) 555-0100",
            "email": "counsel@example.com",
            "appears_for": "Plaintiff Dermot Cruller"
        }
    }})
}

async fn put_matter(f: &Fixture, body: &serde_json::Value, cookie: &str) -> StatusCode {
    f.app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/app/api/projects/{}/matter", f.project_id))
                .header("cookie", cookie)
                .header("x-csrf-token", &f.lawyer_csrf)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

/// The `<section>` carrying `data-section="{name}"`, so an assertion reads
/// one section rather than the whole page.
fn section<'a>(html: &'a str, name: &str) -> &'a str {
    let start = html
        .find(&format!("data-section=\"{name}\""))
        .unwrap_or_else(|| panic!("{name} renders: {html}"));
    let rest = &html[start..];
    &rest[..rest.find("</section>").unwrap_or(rest.len())]
}

#[tokio::test]
async fn the_caption_and_roster_read_a_synced_matter_block_over_the_case_row() {
    let f = synced().await;
    assert_eq!(
        put_matter(&f, &matter(), &f.lawyer_cookie).await,
        StatusCode::OK
    );
    let (status, html) = get(
        &f,
        &format!("/app/projects/{}/timeline", f.code),
        &f.client_cookie,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{html}");
    let caption = section(&html, "case_caption");
    assert!(
        caption.contains("Dermot Cruller v. Synthetic Prine Holdings"),
        "{caption}"
    );
    assert!(caption.contains(SYNCED_CASE_NUMBER), "{caption}");
    assert!(caption.contains(SYNCED_JUDGE), "{caption}");
    assert!(caption.contains("SYNTHETIC SUPERIOR COURT"), "{caption}");
    assert!(
        !caption.contains("A-26-000001"),
        "the synced number wins: {caption}"
    );
    let roster = section(&html, "party_roster");
    assert!(roster.contains(SYNCED_DEFENDANT), "{roster}");
    assert!(
        roster.contains("Counsel of record for Plaintiff Dermot Cruller"),
        "{roster}"
    );

    assert_eq!(
        put_matter(&f, &serde_json::json!({ "matter": null }), &f.lawyer_cookie).await,
        StatusCode::OK
    );
    let (_, html) = get(
        &f,
        &format!("/app/projects/{}/timeline", f.code),
        &f.client_cookie,
    )
    .await;
    let caption = section(&html, "case_caption");
    assert!(
        caption.contains(CAPTION) && caption.contains("A-26-000001"),
        "{caption}"
    );
    assert!(
        !caption.contains(SYNCED_CASE_NUMBER),
        "a cleared block is gone: {caption}"
    );
}

#[tokio::test]
async fn a_client_cannot_store_a_matter_caption() {
    let f = synced().await;
    assert_ne!(
        put_matter(&f, &matter(), &f.client_cookie).await,
        StatusCode::OK
    );
    let (_, html) = get(
        &f,
        &format!("/app/projects/{}/timeline", f.code),
        &f.lawyer_cookie,
    )
    .await;
    assert!(!html.contains(SYNCED_CASE_NUMBER), "{html}");
}

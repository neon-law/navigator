use super::*;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::future::Future;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tower::ServiceExt;
use tower_cookies::CookieManagerLayer;
use tracing_subscriber::fmt::MakeWriter;

const LAWYER_EMAIL: &str = "lawyer@neonlaw.com";

#[derive(Clone)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("capture lock")
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Buffer {
    type Writer = Buffer;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

async fn capture_output<F, Fut, T>(action: F) -> (T, String)
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    crate::test_tracing::ensure_callsite_interest();
    let output = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(Buffer(output.clone()))
        .finish();
    let result = {
        let _guard = tracing::subscriber::set_default(subscriber);
        action().await
    };
    let output =
        String::from_utf8(output.lock().expect("capture lock").clone()).expect("capture is UTF-8");
    (result, output)
}

fn catalog() -> views::locales::services::ServicesCatalog {
    views::locales::services::ServicesCatalog::parse(include_str!(
        "../../neon/locales/en/neon/services-catalog.yaml"
    ))
    .expect("the shipped service catalog parses")
}

/// The shipped catalog with the offered service mapped to the onboarding
/// letter, the way a Firm's Library would publish it. The start door maps
/// the service id to its template and ignores any client-named template,
/// so the loop test reaches an onboarding-kind notation through the same
/// service mapping production uses.
pub(super) fn onboarding_catalog() -> views::locales::services::ServicesCatalog {
    let mut catalog = catalog();
    let service = catalog
        .services
        .iter_mut()
        .find(|s| s.id == "llc-file")
        .expect("the offered service is in the catalog");
    service.template = Some("onboarding__letter".to_string());
    catalog
}

async fn seeded_state() -> (AppState, store::surreal::SurrealDb) {
    let surreal = store::test_support::mem_surreal().await;
    let state = crate::test_support::app_state(surreal.clone()).await;
    store::seed::seed_canonical(&surreal, &state.storage)
        .await
        .expect("the canonical seed loads the service templates");
    let lawyer = store::persons::find_or_create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "Start Door Lawyer",
            LAWYER_EMAIL,
            store::persons::Role::Lawyer,
        ),
    )
    .await
    .expect("create configured lawyer");
    store::persons::set_admitted(&surreal, lawyer.id, true)
        .await
        .expect("admit configured lawyer");
    let mut state = state;
    state.on_call_lawyer_email = Some(LAWYER_EMAIL.to_string());
    (state, surreal)
}

pub(super) async fn client(
    surreal: &store::surreal::SurrealDb,
    name: &str,
    email: &str,
) -> store::persons::Person {
    let person = store::persons::create(
        surreal,
        &store::persons::NewPerson::with_role(name, email, store::persons::Role::Client),
    )
    .await
    .expect("create synthetic client");
    store::persons::set_admitted(surreal, person.id, true)
        .await
        .expect("admit synthetic client");
    store::persons::find_by_id(surreal, person.id)
        .await
        .expect("read synthetic client")
        .expect("synthetic client remains present")
}

async fn person_with_role(
    surreal: &store::surreal::SurrealDb,
    name: &str,
    email: &str,
    role: store::persons::Role,
) -> store::persons::Person {
    let person = store::persons::find_or_create(
        surreal,
        &store::persons::NewPerson::with_role(name, email, role),
    )
    .await
    .expect("create synthetic person");
    store::persons::set_admitted(surreal, person.id, true)
        .await
        .expect("admit synthetic person");
    store::persons::find_by_id(surreal, person.id)
        .await
        .expect("read synthetic person")
        .expect("synthetic person remains present")
}

pub(super) fn session(person: &store::persons::Person) -> SessionData {
    let mut session = SessionData::fresh(person.email.clone(), person.role);
    session.email = Some(person.email.clone());
    session.person_id = Some(person.id);
    session
}

fn post_direct<'a>(
    state: &'a AppState,
    catalog: views::locales::services::ServicesCatalog,
    session: SessionData,
    service_id: &'a str,
    template: Option<&'a str>,
) -> impl std::future::Future<Output = Response> + 'a {
    Box::pin(post_direct_with_headers(
        state,
        catalog,
        session,
        service_id,
        template,
        axum::http::HeaderMap::new(),
    ))
}

async fn post_direct_with_headers(
    state: &AppState,
    catalog: views::locales::services::ServicesCatalog,
    session: SessionData,
    service_id: &str,
    template: Option<&str>,
    headers: axum::http::HeaderMap,
) -> Response {
    post_start(
        State(DoorState {
            app: state.clone(),
            catalog: Some(catalog),
            notations: None,
        }),
        Extension(session),
        Path(service_id.to_string()),
        headers,
        Form(StartForm {
            _csrf: String::new(),
            _template: template.map(ToOwned::to_owned),
            ..Default::default()
        }),
    )
    .await
}

pub(super) fn location(response: &Response) -> String {
    response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
        .unwrap_or_default()
}

async fn body(response: Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("read response body");
    String::from_utf8(bytes.to_vec()).expect("response body is UTF-8")
}

fn session_cookie(state: &AppState, session: &SessionData) -> String {
    format!(
        "{}={}",
        crate::session::SESSION_COOKIE_NAME,
        state.sessions.encode(session)
    )
}

/// The Firm wearing the brand opens every client start on its Firm
/// Library retainer when the library names one; a Firm with no library
/// keeps the service's mapped template.
#[tokio::test]
async fn the_firm_library_retainer_replaces_the_service_template() {
    const DRAFT: &str = include_str!("../../templates/notations/neon_law/onboarding__letter.md");
    let (state, surreal) = seeded_state().await;
    let brand = views::brand::BrandKey::Neon;
    let dir = tempfile::tempdir().expect("temp repository root");
    let repo = repos::RepoStore::new(dir.path());

    let without = start_template(&state, brand, "nv__llc_formation", Some(&repo))
        .await
        .expect("the service template resolves");
    assert_eq!(without.code, "nv__llc_formation");

    let firm_id = store::firms::firm_id_for_brand_key(&surreal, brand.as_str())
        .await
        .expect("firm lookup")
        .expect("the neon brand is worn");
    let firm = store::firms::find_by_id(&surreal, firm_id)
        .await
        .expect("read firm")
        .expect("firm exists");
    repo.commit_as_code(
        &store::firm_library::repository_name(&firm.slug),
        repos::Author {
            name: "Tester",
            email: "tester@example.com",
        },
        "library",
        &[
            ("drafts/firm_retainer.md", DRAFT.as_bytes()),
            (
                store::firm_library::MANIFEST_FILE,
                b"retainer: firm_retainer\n",
            ),
        ],
    )
    .expect("commit the synthetic library");

    let with = start_template(&state, brand, "nv__llc_formation", Some(&repo))
        .await
        .expect("the library retainer resolves");
    assert_eq!(with.code, "firm_retainer");
    assert_eq!(with.firm_id, Some(firm_id));
}

#[tokio::test]
async fn t1_client_start_creates_a_pitch_matter_with_both_dris() {
    let (state, surreal) = seeded_state().await;
    let client = client(&surreal, "Start Door Client", "start-client@example.com").await;
    let lawyer = store::persons::find_by_email_ci(&surreal, LAWYER_EMAIL)
        .await
        .expect("find configured lawyer")
        .expect("canonical seed includes the configured lawyer");

    let (response, output) = capture_output(|| async {
        post_direct(
            &state,
            catalog(),
            session(&client),
            "llc-file",
            Some("onboarding__letter"),
        )
        .await
    })
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let location = location(&response);
    assert!(location.starts_with("/app/projects/"), "{location}");
    assert!(location.contains("/intake/"), "{location}");
    assert!(location.ends_with("?started=1"), "{location}");

    let projects = store::projects::all(&surreal).await.expect("list projects");
    assert_eq!(projects.len(), 1);
    let project = &projects[0];
    assert_eq!(project.status, "pitch");
    assert_eq!(project.brand, "neon");

    let roles = store::projects::participations_for_project(&surreal, project.id)
        .await
        .expect("list matter participation");
    assert_eq!(roles.len(), 2);
    assert!(roles.iter().any(|role| {
        role.person_id == client.id && role.participation == "client" && role.is_client_dri
    }));
    assert!(roles.iter().any(|role| {
        role.person_id == lawyer.id && role.participation == "lawyer" && role.is_lawyer_dri
    }));

    let notations = store::notations::list_by_project(&surreal, project.id)
        .await
        .expect("list matter notations");
    assert_eq!(notations.len(), 1);
    let template = store::templates::find_by_id(&surreal, notations[0].template_id)
        .await
        .expect("read notation template")
        .expect("notation template remains present");
    assert_eq!(template.code, "nv__llc_formation");

    let (engagements, closings) = store::projects::matter_lifecycle_sets(&surreal, &projects)
        .await
        .expect("compute matter lifecycle sets");
    assert!(!engagements.contains(&project.id));
    assert!(!closings.contains(&project.id));

    assert!(output.contains("funnel.offer_accepted"), "funnel: {output}");
    assert!(
        output.contains(&format!("person_id=\"{}\"", client.id)),
        "funnel: {output}"
    );
    assert!(
        output.contains(&format!("project_id=\"{}\"", project.id)),
        "funnel: {output}"
    );
    assert!(
        output.contains("service_id=\"llc-file\""),
        "funnel: {output}"
    );
    assert!(output.contains("brand=\"neon\""), "funnel: {output}");
    assert!(
        !output.contains("Start Door Client"),
        "name leaked: {output}"
    );
    assert!(
        !output.contains("start-client@example.com"),
        "email leaked: {output}"
    );
    assert!(
        !output.contains(&project.code),
        "Project code leaked: {output}"
    );
    for forbidden in ["email=", "phone=", "name=", "address=", "project_code="] {
        assert!(!output.contains(forbidden), "{forbidden} leaked: {output}");
    }
}

#[tokio::test]
async fn t2_service_mapping_controls_the_template_and_unmapped_service_writes_nothing() {
    let (state, surreal) = seeded_state().await;
    let client = client(
        &surreal,
        "Mapping Test Client",
        "mapping-client@example.com",
    )
    .await;
    let response = post_direct(
        &state,
        catalog(),
        session(&client),
        "llc-file",
        Some("onboarding__letter"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let notation = store::notations::list_all(&surreal)
        .await
        .expect("list mapped notation")
        .pop()
        .expect("mapped service opened one notation");
    let template = store::templates::find_by_id(&surreal, notation.template_id)
        .await
        .expect("read mapped template")
        .expect("mapped template exists");
    assert_eq!(template.code, "nv__llc_formation");

    let before_projects = store::projects::all(&surreal)
        .await
        .expect("list projects")
        .len();
    let before_notations = store::notations::list_all(&surreal)
        .await
        .expect("list notations")
        .len();
    let response = post_direct(
        &state,
        catalog(),
        session(&client),
        "nda",
        Some("nv__llc_formation"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        store::projects::all(&surreal)
            .await
            .expect("list projects")
            .len(),
        before_projects
    );
    assert_eq!(
        store::notations::list_all(&surreal)
            .await
            .expect("list notations")
            .len(),
        before_notations
    );
}

#[tokio::test]
async fn bootstrap_owner_fallback_opens_start_door_with_owner_dri() {
    let (mut state, surreal) = seeded_state().await;
    let owner = person_with_role(
        &surreal,
        "Bootstrap Owner",
        "bootstrap-owner@example.com",
        store::persons::Role::Owner,
    )
    .await;
    let client = client(&surreal, "Fallback Client", "fallback-client@example.com").await;
    state.on_call_lawyer_email = None;
    state.bootstrap_owner_email = Some(owner.email.clone());

    let (response, output) = capture_output(|| async {
        post_direct(&state, catalog(), session(&client), "llc-file", None).await
    })
    .await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let projects = store::projects::all(&surreal).await.expect("list projects");
    let roles = store::projects::participations_for_project(&surreal, projects[0].id)
        .await
        .expect("list matter participation");
    assert!(roles
        .iter()
        .any(|role| role.person_id == owner.id && role.is_lawyer_dri));
    assert!(
        output.contains("on_call_source=\"bootstrap_owner\""),
        "{output}"
    );
}

#[tokio::test]
async fn bootstrap_owner_fallback_requires_lawyer_tier() {
    let (mut state, surreal) = seeded_state().await;
    let owner = person_with_role(
        &surreal,
        "Bootstrap Client",
        "bootstrap-client@example.com",
        store::persons::Role::Client,
    )
    .await;
    let client = client(
        &surreal,
        "Rejected Fallback Client",
        "rejected-fallback@example.com",
    )
    .await;
    state.on_call_lawyer_email = None;
    state.bootstrap_owner_email = Some(owner.email);

    let response = post_direct(&state, catalog(), session(&client), "llc-file", None).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body(response).await, catalog().start.refusal);
    assert!(store::projects::all(&surreal)
        .await
        .expect("list projects")
        .is_empty());
}

#[tokio::test]
async fn configured_on_call_lawyer_wins_over_bootstrap_owner() {
    let (mut state, surreal) = seeded_state().await;
    let owner = person_with_role(
        &surreal,
        "Bootstrap Owner",
        "bootstrap-owner@example.com",
        store::persons::Role::Owner,
    )
    .await;
    let lawyer = store::persons::find_by_email_ci(&surreal, LAWYER_EMAIL)
        .await
        .expect("find configured lawyer")
        .expect("configured lawyer exists");
    let client = client(
        &surreal,
        "Configured Client",
        "configured-client@example.com",
    )
    .await;
    state.bootstrap_owner_email = Some(owner.email);

    let (response, output) = capture_output(|| async {
        post_direct(&state, catalog(), session(&client), "llc-file", None).await
    })
    .await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let projects = store::projects::all(&surreal).await.expect("list projects");
    let roles = store::projects::participations_for_project(&surreal, projects[0].id)
        .await
        .expect("list matter participation");
    assert!(roles
        .iter()
        .any(|role| role.person_id == lawyer.id && role.is_lawyer_dri));
    assert!(!roles
        .iter()
        .any(|role| role.person_id == owner.id && role.is_lawyer_dri));
    assert!(output.contains("on_call_source=\"configured\""), "{output}");
}

#[tokio::test]
async fn missing_start_door_lawyer_configuration_is_reported() {
    let (mut state, surreal) = seeded_state().await;
    let client = client(
        &surreal,
        "Configuration Client",
        "configuration-client@example.com",
    )
    .await;
    state.on_call_lawyer_email = None;
    state.bootstrap_owner_email = None;

    let (response, output) = capture_output(|| async {
        post_direct(&state, catalog(), session(&client), "llc-file", None).await
    })
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body(response).await, catalog().start.refusal);
    assert!(output.contains("configuration_missing"), "{output}");
    assert!(store::projects::all(&surreal)
        .await
        .expect("list projects")
        .is_empty());
    assert!(!output.contains("configuration-client@example.com"));
    assert!(!output.contains("Configuration Client"));
}

#[tokio::test]
async fn t4_anonymous_get_redirects_and_authenticated_get_carries_verbatim_disclosure() {
    let (state, surreal) = seeded_state().await;
    let catalog = catalog();
    let app = routes(&state, Some(catalog.clone()), Vec::new()).layer(CookieManagerLayer::new());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/start/llc-file")
                .body(Body::empty())
                .expect("anonymous request"),
        )
        .await
        .expect("anonymous response");
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/auth/login?return_to=/start/llc-file");

    let client = client(
        &surreal,
        "Disclosure Client",
        "disclosure-client@example.com",
    )
    .await;
    let session = session(&client);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/start/llc-file")
                .header("cookie", session_cookie(&state, &session))
                .body(Body::empty())
                .expect("authenticated request"),
        )
        .await
        .expect("authenticated response");
    assert_eq!(response.status(), StatusCode::OK);
    let html = body(response).await;
    assert!(html.contains(&catalog.start.disclosure), "{html}");
}

fn notation_offer() -> NotationOffer {
    NotationOffer {
        slug: "onboarding-letter".into(),
        title: "Onboarding Letter".into(),
        template_code: "onboarding__letter".into(),
    }
}

/// A published notation's Workflow door signs the reader in, then opens
/// intake on a Project whose code is random hex — nothing about the
/// client — and lands them in that intake.
#[tokio::test]
async fn the_notation_door_signs_in_then_opens_intake_under_a_random_code() {
    let (state, surreal) = seeded_state().await;
    let app = routes(&state, Some(onboarding_catalog()), vec![notation_offer()])
        .layer(CookieManagerLayer::new());
    let anonymous = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/notations/onboarding-letter/start")
                .body(Body::empty())
                .expect("anonymous request"),
        )
        .await
        .expect("anonymous response");
    assert_eq!(
        location(&anonymous),
        "/auth/login?return_to=/notations/onboarding-letter/start"
    );

    let client = client(&surreal, "Notation Client", "notation-client@example.com").await;
    let session = session(&client);
    let unknown = app
        .oneshot(
            Request::builder()
                .uri("/notations/not-a-notation/start")
                .header("cookie", session_cookie(&state, &session))
                .body(Body::empty())
                .expect("unknown request"),
        )
        .await
        .expect("unknown response");
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

    let response = post_start(
        State(DoorState {
            app: state.clone(),
            catalog: Some(onboarding_catalog()),
            notations: Some(std::sync::Arc::new(vec![notation_offer()])),
        }),
        Extension(session),
        Path("onboarding-letter".to_string()),
        axum::http::HeaderMap::new(),
        Form(StartForm::default()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let projects = store::projects::all(&surreal).await.expect("list projects");
    let code = &projects[0].code;
    assert!(
        code.len() == 16 && code.bytes().all(|b| b.is_ascii_hexdigit()),
        "{code}"
    );
    assert!(!code.contains("notation-client"), "{code}");
    assert!(
        location(&response).starts_with(&format!("/app/projects/{code}/intake/")),
        "{}",
        location(&response)
    );
}

#[tokio::test]
async fn t5_strict_csrf_and_rate_limit_protect_the_cookie_post() {
    let (mut state, surreal) = seeded_state().await;
    let client = client(
        &surreal,
        "Protection Client",
        "protection-client@example.com",
    )
    .await;
    state.rate_limit = crate::rate_limit::RateLimit::new(1, Duration::from_mins(1));
    let session = session(&client);
    let app = routes(&state, Some(catalog()), Vec::new()).layer(CookieManagerLayer::new());
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/start/llc-file")
            .header("cookie", session_cookie(&state, &session))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from("template=nv__annual_report"))
            .expect("cookie POST")
    };
    let first = app
        .clone()
        .oneshot(request())
        .await
        .expect("first response");
    let second = app.oneshot(request()).await.expect("second response");
    assert_eq!(first.status(), StatusCode::FORBIDDEN);
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        store::projects::all(&surreal)
            .await
            .expect("list projects")
            .len(),
        0
    );
}

#[tokio::test]
async fn t6_blocking_conflict_compensates_rows_but_keeps_client_admitted() {
    let (state, surreal) = seeded_state().await;
    let opponent = client(&surreal, "Current Client", "current-client@example.com").await;
    let opponent_entity = store::test_support::seed_entity(&surreal).await;
    let existing = store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: format!("existing-conflict-{}", Uuid::now_v7()),
            name: "Existing conflict matter".into(),
            status: "open".into(),
            brand: "neon".into(),
            entity_id: opponent_entity,
            ..Default::default()
        },
    )
    .await
    .expect("create blocking-conflict fixture");
    store::projects::designate_dri_in_surreal(
        &surreal,
        existing.id,
        opponent.id,
        store::projects::DriSide::Client,
    )
    .await
    .expect("designate current client");
    store::projects::designate_dri_in_surreal(
        &surreal,
        existing.id,
        store::persons::find_by_email_ci(&surreal, LAWYER_EMAIL)
            .await
            .expect("find lawyer")
            .expect("lawyer exists")
            .id,
        store::projects::DriSide::Lawyer,
    )
    .await
    .expect("designate current lawyer");

    let proposed = client(&surreal, "Proposed Client", "proposed-client@example.com").await;
    store::relationships::record(
        &surreal,
        &store::relationships::NewRelationship {
            from: store::relationships::Endpoint::Person,
            from_id: proposed.id,
            to: store::relationships::Endpoint::Person,
            to_id: opponent.id,
            kind: store::relationships::KIND_ADVERSE_TO.into(),
            confidence_pct: 100,
            source_kind: store::relationships::SOURCE_MANUAL.into(),
            source_id: None,
            detail: None,
        },
    )
    .await
    .expect("record blocking relationship");

    let before_notations = store::notations::list_all(&surreal)
        .await
        .expect("list initial notations")
        .len();
    let before_roles = store::projects::all_participations(&surreal)
        .await
        .expect("list initial participation")
        .len();
    let response = post_direct(&state, catalog(), session(&proposed), "llc-file", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body(response).await, catalog().start.refusal);
    assert_eq!(
        store::projects::all(&surreal)
            .await
            .expect("list projects")
            .len(),
        1
    );
    assert_eq!(
        store::notations::list_all(&surreal)
            .await
            .expect("list notations")
            .len(),
        before_notations
    );
    assert_eq!(
        store::projects::all_participations(&surreal)
            .await
            .expect("list participation")
            .len(),
        before_roles
    );
    assert!(store::persons::is_admitted(&surreal, proposed.id)
        .await
        .expect("read admission"));
}

#[tokio::test]
async fn starting_a_matter_fires_exactly_one_intake_conversion_with_no_personal_data() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/event"))
        .respond_with(ResponseTemplate::new(202))
        .expect(1)
        .mount(&server)
        .await;

    let (mut state, surreal) = seeded_state().await;
    state.plausible_conversions = crate::plausible::ConversionReporter::new(server.uri(), true);
    let client = client(
        &surreal,
        "Conversion Client",
        "conversion-client@example.com",
    )
    .await;

    let response = post_direct(
        &state,
        catalog(),
        session(&client),
        "llc-file",
        Some("onboarding__letter"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);

    let requests = server
        .received_requests()
        .await
        .expect("request recording is on by default");
    assert_eq!(requests.len(), 1, "exactly one intake conversion beacon");
    let sent: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(sent["name"], "Intake: Started");
    assert_eq!(sent["domain"], "neonlaw.com");
    assert_eq!(sent["url"], "https://neonlaw.com/start/llc-file");
    let raw = sent.to_string();
    assert!(
        !raw.contains("conversion-client@example.com"),
        "email leaked into the conversion payload: {raw}"
    );
    assert!(
        !raw.contains(&client.id.to_string()),
        "person id leaked into the conversion payload: {raw}"
    );
}

#[tokio::test]
async fn a_rejected_start_fires_no_conversion() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/event"))
        .respond_with(ResponseTemplate::new(202))
        .expect(0)
        .mount(&server)
        .await;

    let (mut state, surreal) = seeded_state().await;
    state.plausible_conversions = crate::plausible::ConversionReporter::new(server.uri(), true);
    state.on_call_lawyer_email = None;
    state.bootstrap_owner_email = None;
    let client = client(&surreal, "Rejected Client", "rejected-client@example.com").await;

    let response = post_direct(
        &state,
        catalog(),
        session(&client),
        "llc-file",
        Some("onboarding__letter"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body(response).await, catalog().start.refusal);
}

/// The admin state the boot conversion builds from [`crate::AppState`]
/// (mirrored from `lib.rs`'s registration wiring).
fn admin_state(app: &crate::AppState) -> crate::admin::AdminState {
    crate::admin::AdminState {
        surreal: app.surreal.clone(),
        workflow_runtime: app.workflow_runtime.clone(),
        signature_provider: app.signature_provider.clone(),
        retainer_intake_questionnaire: workflows::retainer_intake_questionnaire(),
        questionnaire_runtime: app.questionnaire_runtime.clone(),
        storage: app.storage.clone(),
        assets_storage: app.assets_storage.clone(),
        forms_registry: app.forms_registry.clone(),
        email: app.email.clone(),
        billing_provider: app.billing_provider.clone(),
        contract_reviewer: app.contract_reviewer.clone(),
        bootstrap_owner_email: app.bootstrap_owner_email.clone(),
        on_call_lawyer_email: app.on_call_lawyer_email.clone(),
        bootstrap_company: crate::admin::bootstrap_company_from_env(),
        sessions: app.sessions.clone(),
        secure_cookies: false,
        attachment_scanner: app.attachment_scanner.clone(),
        runtime_kms: app.runtime_kms.clone(),
        twilio_registry: None,
    }
}

/// The programme's proof, one test: a prospective client accepts the
/// engagement offer through the start door; the opened notation carries
/// the offer's terms as extracted proposals (the service's name proposes
/// the matter name, its blurb the scope); the client confirms them in
/// intake; the lawyer approves and sends; the local loop signs every
/// recipient, self-delivers the HMAC-checked completion webhook through
/// this portal's own receiver, and files the completion. The synthetic
/// envelope ID leaves the matter a pitch under the DocuSign UUID rule.
#[tokio::test]
// One continuous ceremony reads better as a single narrative —
// acceptance, confirmation, review, send, dual signing, filing —
// so the length lint is allowed per repo precedent (admin.rs, a2a.rs).
#[allow(clippy::too_many_lines)]
async fn an_accepted_offer_creates_the_notation_and_completes_the_signature_loop() {
    use crate::signature::{FakeSignatureProvider, WebhookTarget};

    const WEBHOOK_SECRET: &str = "loop-secret";
    let (state, surreal) = seeded_state().await;
    let lawyer = store::persons::find_by_email_ci(&surreal, LAWYER_EMAIL)
        .await
        .expect("read the configured lawyer")
        .expect("the configured lawyer exists");

    // --- Accept: the client takes the offer through the start door,
    // opening the matter on the onboarding letter (the kind that opens
    // a matter, so the loop's completion can make it official).
    let client_person = client(&surreal, "Libra", "libra@example.com").await;
    let response = post_direct(
        &state,
        onboarding_catalog(),
        session(&client_person),
        "llc-file",
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER, "{response:?}");
    assert!(location(&response).contains("/intake/"));

    let projects = store::projects::all(&surreal).await.expect("list projects");
    assert_eq!(projects.len(), 1);
    let project = &projects[0];
    let notations = store::notations::list_by_project(&surreal, project.id)
        .await
        .expect("list matter notations");
    assert_eq!(notations.len(), 1);
    let notation_id = notations[0].id;
    let template = store::templates::find_by_id(&surreal, notations[0].template_id)
        .await
        .expect("read notation template")
        .expect("notation template remains present");
    assert_eq!(template.code, "onboarding__letter");

    // The offer's terms are on record as extracted proposals — never
    // silently accepted: the source is `extracted`, awaiting the
    // client's confirmation in intake.
    let answers = store::answers::for_notation(&surreal, notation_id)
        .await
        .expect("read the notation's answers");
    let service = onboarding_catalog()
        .services
        .iter()
        .find(|s| s.id == "llc-file")
        .expect("the offered service is in the catalog")
        .clone();
    let proposal = |state_name: &str| {
        answers
            .iter()
            .find(|a| {
                a.state_name.as_deref() == Some(state_name)
                    && a.source == store::answers::SOURCE_EXTRACTED
            })
            .unwrap_or_else(|| panic!("no extracted proposal for {state_name}"))
    };
    assert_eq!(
        proposal("project__engagement")
            .value
            .get("name")
            .and_then(|v| v.as_str()),
        Some(service.name.as_str()),
        "the service's name proposes the matter name"
    );
    assert_eq!(
        store::answers::primitive_str(&proposal("custom_text__engagement_scope").value),
        Some(service.blurb.as_str()),
        "the service's blurb proposes the engagement scope"
    );

    // A matter whose loop has not completed reads `pitch` — even now,
    // with the offer accepted and the notation opened.
    assert_eq!(
        store::projects::onboarding_status(&surreal, project.id, &project.code)
            .await
            .expect("read onboarding status"),
        store::projects::OnboardingStatus::Pitch,
    );

    // --- Confirm: the client answers intake, confirming the proposed
    // terms (the same walk surface, client-authored).
    let steps = [
        ("entity", "Northstar Ventures LLC"),
        (
            "address__principal_office",
            "100 Innovation Way, Reno, NV 89501",
        ),
        ("person__client", "Libra"),
        ("project__engagement", service.name.as_str()),
        ("custom_datetime__engagement_start_date", "2026-11-06"),
        ("custom_text__engagement_scope", service.blurb.as_str()),
        ("custom_single_choice__governing_law", "nevada"),
    ];
    for (code, value) in steps {
        workflows::answer_step(
            &surreal,
            state.workflow_runtime.as_ref(),
            None,
            notation_id,
            code,
            value,
            workflows::AnswerAuthor::client(Some(client_person.id)),
        )
        .await
        .unwrap_or_else(|e| panic!("client step `{code}` refused: {e}"));
    }

    // --- Review: the walk parks the matter at lawyer review.
    let final_state = crate::retainer_walk::advance_to_lawyer_review(
        &surreal,
        state.workflow_runtime.as_ref(),
        &state.storage,
        notation_id,
        None,
    )
    .await
    .expect("the walk reaches lawyer review");
    assert_eq!(final_state.as_str(), "lawyer_review");

    // --- Serve: the portal itself, so the fake's completion webhook
    // self-delivers through this portal's own receiver — the same
    // loopback wiring `hosting` builds in dev.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a loopback port");
    let port = listener.local_addr().expect("the bound port").port();
    let hmac_key = b"loop-hmac-key".to_vec();
    let fake_root =
        std::env::temp_dir().join(format!("navigator-accept-loop-{}", uuid::Uuid::now_v7()));
    let fake = FakeSignatureProvider::new_in(fake_root.clone())
        .with_signing_base_url(format!("http://127.0.0.1:{port}/fake-signature"))
        .with_webhook(WebhookTarget {
            url: format!("http://127.0.0.1:{port}/webhook/esignature/{WEBHOOK_SECRET}"),
            hmac_key: hmac_key.clone(),
        });
    let fake = Arc::new(fake);
    let mut served = state.clone();
    served.signature_provider = fake.clone();
    served.fake_signature_dir = Some(fake_root.clone());
    served.esignature_webhook_secret = Some(WEBHOOK_SECRET.to_string());
    served.esignature_hmac_key = Some(String::from_utf8(hmac_key.clone()).expect("utf8 key"));
    // The handlers below run against the SERVED state: the same fake
    // provider instance the envelope is dispatched through, and the
    // same webhook secret its completion event is verified with.
    let admin = admin_state(&served);
    let app = crate::router(served);
    let router_for_serve = app.clone();
    tokio::spawn(async move {
        axum::serve(listener, router_for_serve)
            .await
            .expect("the loopback portal serves");
    });

    // --- Fees: the letter leaves the fee to its custom clauses, so
    // the attorney writes the fee terms at review — the send refuses
    // an empty clause slot (NRPC 1.5(b)).
    store::notation_clauses::append(
        &surreal,
        notation_id,
        "**Flat fee.** $1,000 for the formation engagement, payable on approval.",
        Some(lawyer.id),
    )
    .await
    .expect("the fee clause is appended");

    // --- Approve: the attorney approves the reviewed letter; the
    // handler renders and parks it for the send.
    let approve = crate::retainer_walk::approve_send_post(
        axum::extract::State(admin.clone()),
        axum::extract::Path(notation_id),
        Some(axum::Extension(session(&lawyer))),
    )
    .await;
    assert_eq!(approve.status(), StatusCode::SEE_OTHER, "approve redirects");

    // The worker's render, durably persisted: the PDF carries every
    // anchor the letter's signature block expands to — the same
    // expansion `send_notation_for_signature` runs to build the manifest.
    let template_body = store::templates::body(&surreal, &state.storage, &template)
        .await
        .expect("read the onboarding letter body");
    let (_expanded, fields) = crate::signature_render::expand_signatures(&template_body);
    assert!(
        !fields.is_empty(),
        "the onboarding letter declares signature fields"
    );
    let anchors = fields
        .iter()
        .map(|f| f.anchor.as_str())
        .collect::<Vec<_>>()
        .join(" \\ ");
    let pdf = pdf::render(&format!(
        "= Retainer\n\n#text(fill: white, size: 4pt)[{anchors}]\n"
    ))
    .expect("the anchor PDF renders");
    state
        .storage
        .put(
            &store::notations::document_pdf_storage_key(notation_id),
            &pdf,
            "application/pdf",
        )
        .await
        .expect("persist the rendered PDF");

    // --- Send: the deliberate send dispatches exactly one envelope.
    let send = crate::retainer_walk::send_post(
        axum::extract::State(admin.clone()),
        axum::extract::Path(notation_id),
        Some(axum::Extension(session(&lawyer))),
    )
    .await;
    assert_eq!(send.status(), StatusCode::SEE_OTHER, "send redirects");
    let request_id = store::signatures::request_id_for_notation(&surreal, notation_id)
        .await
        .expect("read the signature request")
        .expect("the send recorded the envelope");

    // --- Sign: the client's ceremony entry is a real local sign page
    // served by this portal, then both parties sign in routing order.
    let entry = crate::esign_view::sign_get(
        axum::extract::State(admin.clone()),
        axum::Extension(session(&client_person)),
        axum::http::HeaderMap::new(),
        axum::extract::Path(notation_id),
    )
    .await;
    assert_eq!(entry.status(), StatusCode::SEE_OTHER, "the ceremony starts");
    let sign_url = entry
        .headers()
        .get(axum::http::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .expect("a redirect to the provider's sign page")
        .to_string();
    assert!(
        sign_url.starts_with(&format!("http://127.0.0.1:{port}/fake-signature/")),
        "the client's sign page is a real local URL: {sign_url}"
    );
    let page_path = sign_url
        .splitn(4, '/')
        .last()
        .map(|rest| format!("/{rest}"))
        .expect("the sign page path");
    let page = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(page_path.clone())
                .body(Body::empty())
                .expect("a bodyless GET"),
        )
        .await
        .expect("the sign page serves");
    let page_status = page.status();
    let page_text = body(page).await;
    assert_eq!(
        page_status,
        StatusCode::OK,
        "the sign page exists at {page_path}"
    );
    assert!(page_text.contains("Sign here"));

    // The client signs first (routing 1) — one signature is not
    // completion; the firm countersigns and the envelope completes,
    // firing the completion webhook to the receiver above.
    let envelope_id = crate::signature::SignatureRequestId(request_id);
    let after_client = fake
        .sign(&envelope_id, "libra@example.com")
        .await
        .expect("the client signs");
    assert_eq!(
        after_client,
        crate::signature::EnvelopeStatus::Sent,
        "one signature is not completion"
    );
    let after_firm = fake
        .sign(&envelope_id, "support@neonlaw.com")
        .await
        .expect("the firm countersigns");
    assert_eq!(
        after_firm,
        crate::signature::EnvelopeStatus::Completed,
        "the envelope completes on the last signature"
    );

    // The fake envelope completes the signature loop and archives its documents.
    // Its synthetic provider ID cannot establish official DocuSign execution.
    assert_eq!(
        store::projects::onboarding_status(&surreal, project.id, &project.code)
            .await
            .expect("read onboarding status after the loop"),
        store::projects::OnboardingStatus::Pitch,
        "a synthetic provider ID leaves the matter a pitch"
    );
    let envelope = fake
        .envelope(&envelope_id)
        .expect("the envelope parses")
        .expect("present");
    assert_eq!(envelope.status, crate::signature::EnvelopeStatus::Completed);
    assert_eq!(
        envelope.webhook_status.as_deref(),
        Some("delivered"),
        "the completion webhook was delivered and verified"
    );
}

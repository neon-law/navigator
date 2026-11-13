//! Cucumber runner for `features/sign_before_case.feature` (LAW-344).
//!
//! Drives the completion webhook's matter file-back against an in-memory
//! runtime, on a matter whose `matter_caption.case_number` was never set:
//! a Benton County declaration (not `onboarding__letter` — the
//! generalization proof) is parked at `sent_for_signature__pending`, the
//! client signs on the fake provider, and the provider's completion
//! callback is POSTed — validly signed (the signed PDF and Certificate of
//! Completion are filed on the Project's document set exactly once, so a
//! Connect redelivery adds nothing), then forged (rejected, nothing
//! filed).

#![allow(clippy::unused_async)]
#![allow(clippy::doc_markdown)]

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use cloud::StorageService;
use cucumber::{given, then, when, World};
use features::{app_state, body_string, fs_storage};
use portal::signature::{
    FakeSignatureProvider, SendMode, SignatureManifest, SignatureProvider, SignatureRecipient,
};
use portal::webhook_auth::sign_hmac_sha256_b64;
use portal::AppState;
use portal::{policy::PolicyClient, SessionStore};
use store::seed;
use tower::ServiceExt;
use uuid::Uuid;
use workflows::{
    bundled_spec_yaml, workflow_spec_from_yaml, DispatchingRuntime, InMemoryRuntime, MachineKind,
    StateMachineRuntime,
};

// Cucumber drops Worlds inside its async runner. Retain each private engine
// until process exit so its internal runtime is never dropped there.
static SCENARIO_ENGINES: Mutex<Vec<store::surreal::SurrealDb>> = Mutex::new(Vec::new());

async fn scenario_store() -> store::surreal::SurrealDb {
    let db = store::surreal::test_support::mem().await;
    SCENARIO_ENGINES
        .lock()
        .expect("retain scenario engine")
        .push(db.clone());
    db
}

const TEMPLATE_CODE: &str = "wa__benton_county_declaration";
const HMAC_KEY: &str = "test-docusign-hmac-key";
const PARKED: &str = "sent_for_signature__pending";
const SIGNER_EMAIL: &str = "libra@example.com";
const PARKED_PDF: &[u8] = b"%PDF-1.4 parked declaration awaiting the provider's callback";
const SIGNED_FILENAME: &str = "declaration_signed.pdf";
const CERTIFICATE_FILENAME: &str = "declaration_certificate.pdf";

/// The DocuSign Connect completion payload for one envelope.
fn completion_body(envelope_id: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "event": "envelope-completed",
        "data": {
            "envelopeId": envelope_id,
            "envelopeSummary": { "status": "completed" },
        },
    }))
    .unwrap()
}

#[derive(Default, World)]
#[world(init = Self::default)]
struct SignBeforeCaseWorld {
    app: Option<axum::Router>,
    runtime: Option<Arc<InMemoryRuntime>>,
    notation_id: Option<Uuid>,
    project_id: Option<Uuid>,
    /// The envelope id actually recorded in `signatures`, minted by the
    /// held fake provider as `fake-{notation_id}-1`. Notation ids are
    /// unique per scenario, so the minted envelope ids never collide on
    /// the `(provider, provider_id)` unique index.
    envelope_id: Option<String>,
    /// The app's signature provider — the same instance the webhook's
    /// consumption path reads, held so the parked envelope can be sent
    /// and signed through the real provider seam.
    provider: Option<Arc<FakeSignatureProvider>>,
    /// The router's own state, held (`AppState` is clone-cheap) so the
    /// assertion steps read the same store the handler wrote.
    state: Option<AppState>,
    last_status: Option<StatusCode>,
    /// The exact bytes of the last posted callback, held so the
    /// redelivery step re-posts the same signed payload Connect would.
    last_callback: Option<(Vec<u8>, String)>,
    last_body: String,
}

impl std::fmt::Debug for SignBeforeCaseWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignBeforeCaseWorld")
            .field("notation_id", &self.notation_id)
            .field("project_id", &self.project_id)
            .field("last_status", &self.last_status)
            .finish_non_exhaustive()
    }
}

impl SignBeforeCaseWorld {
    fn store(&self) -> store::surreal::SurrealDb {
        self.app_state().surreal.clone()
    }
    fn app(&self) -> axum::Router {
        self.app.as_ref().expect("app not built").clone()
    }
    fn app_state(&self) -> AppState {
        self.state.as_ref().expect("app state not built").clone()
    }
    fn runtime(&self) -> &Arc<InMemoryRuntime> {
        self.runtime.as_ref().expect("runtime not built")
    }
    fn project_id(&self) -> Uuid {
        self.project_id.expect("matter not built")
    }
    fn envelope_id(&self) -> &str {
        self.envelope_id.as_deref().expect("envelope not built")
    }
}

#[given("a Neon Law Navigator app with an HMAC-secured e-signature webhook")]
async fn build_app(world: &mut SignBeforeCaseWorld) {
    let surreal = scenario_store().await;
    let storage = fs_storage("sign-before-case").await;
    seed::seed_canonical(&surreal, &storage)
        .await
        .expect("seed canonical");
    let runtime = Arc::new(InMemoryRuntime::new());
    let mut state = app_state(
        runtime.clone(),
        storage,
        PolicyClient::passthrough(),
        None,
        SessionStore::new("test-session-key-not-for-production"),
    )
    .await;
    state.surreal = surreal;
    let dispatch: Arc<dyn StateMachineRuntime> = Arc::new(
        DispatchingRuntime::new(runtime.clone(), state.email.clone(), state.storage.clone())
            .with_store(state.surreal.clone()),
    );
    state.workflow_runtime = dispatch;
    // Arm the HMAC gate so the webhook actually verifies signatures —
    // without this the dev posture would accept the forged callback.
    state.esignature_hmac_key = Some(HMAC_KEY.to_string());
    let provider = Arc::new(FakeSignatureProvider::new());
    state.signature_provider = provider.clone();
    let router = features::neon_router(
        state.clone(),
        std::path::Path::new(portal::DEFAULT_PUBLIC_DIR),
    );
    world.app = Some(router);
    world.runtime = Some(runtime);
    world.provider = Some(provider);
    world.state = Some(state);
}

/// The bundled declaration template as a store row. The Benton declaration
/// ships through the court-seed catalog, not `SEEDED_TEMPLATES`, so a test
/// that creates a notation from it writes the row itself: same shape the
/// canonical seed produces (frontmatter split off, body ingested as an
/// asset, kind carried on the row).
async fn seed_declaration_template(
    surreal: &store::surreal::SurrealDb,
    storage: &Arc<dyn StorageService>,
) -> store::templates::Template {
    let md = include_str!(
        "../../templates/notations/forms/united_states/washington/benton_county_superior_court/wa__benton_county_declaration.md"
    );
    // The body asset is the markdown after the `---` frontmatter block,
    // exactly what `seed_templates` ingests.
    let body = md
        .strip_prefix("---\n")
        .and_then(|rest| rest.split("\n---\n").nth(1))
        .unwrap_or(md);
    let asset_id =
        store::assets::ingest_content(surreal, storage, body.as_bytes(), "text/markdown")
            .await
            .expect("ingest the declaration body");
    store::templates::save_version(
        surreal,
        None,
        TEMPLATE_CODE,
        store::templates::Version {
            title: "Client Declaration — Benton County Superior Court".into(),
            respondent_type: "person".into(),
            asset_id: Some(asset_id),
            form_code: None,
            kind: Some("declaration".into()),
            source_commit_sha: None,
        },
    )
    .await
    .expect("save the declaration template version")
    .into_model()
}

#[given(
    regex = r"^a Benton County declaration parked at sent_for_signature__pending on a matter with no case number$"
)]
async fn park_declaration(world: &mut SignBeforeCaseWorld) {
    let surreal = world.store();
    let tmpl = seed_declaration_template(&surreal, &world.app_state().storage.clone()).await;
    let person = store::test_support::ensure_person(
        &surreal,
        &store::persons::NewPerson::new("Libra", SIGNER_EMAIL),
    )
    .await;
    let proj = store::test_support::seed_project(&surreal, "benton declaration matter").await;
    // The precondition the whole feature turns on: the caption is optional
    // metadata, and this matter has none — no case number was ever set.
    let caption = store::matter_captions::for_project(&surreal, proj.id)
        .await
        .unwrap();
    let case_number = caption.map(|c| c.matter.case_number).unwrap_or_default();
    assert!(
        case_number.is_empty(),
        "the matter must have no case number, got {case_number:?}"
    );
    let notation_id = store::notations::create(
        &surreal,
        &store::notations::NewNotation::new(tmpl.id, person.id, proj.id, "BEGIN"),
    )
    .await
    .unwrap()
    .id;

    // Drive the declaration workflow to the parked state through the same
    // in-memory runtime the webhook will later signal, following its
    // bundled spec: BEGIN → lawyer_review → approved →
    // generate_pdf__declaration_pdf → pdf_persisted → parked.
    let rt = world.runtime().as_ref();
    let yaml = bundled_spec_yaml(TEMPLATE_CODE).expect("the declaration spec is bundled");
    let spec = workflow_spec_from_yaml(yaml).expect("a valid declaration workflow");
    StateMachineRuntime::start(rt, MachineKind::Workflow, notation_id, &spec)
        .await
        .unwrap();
    // The `_` arm is the automatic BEGIN → lawyer_review entry; the
    // runtime's signal is a strict lookup, so the test fires it by name.
    for condition in ["_", "approved", "pdf_persisted"] {
        StateMachineRuntime::signal(rt, MachineKind::Workflow, notation_id, condition, None)
            .await
            .unwrap();
    }
    assert_eq!(
        StateMachineRuntime::current_state(rt, MachineKind::Workflow, notation_id)
            .await
            .unwrap()
            .as_str(),
        PARKED,
        "workflow should be parked before the callback"
    );

    // Make the provider-side half real: the held fake provider mints the
    // envelope and the client's signature completes it — exactly the state
    // DocuSign is in when its Connect callback posts. The declaration's
    // frontmatter declares one client signer (routing order 1).
    let manifest = SignatureManifest {
        recipients: vec![SignatureRecipient {
            role: "client".to_string(),
            email: SIGNER_EMAIL.to_string(),
            name: "Libra".to_string(),
            routing_order: 1,
            client_user_id: None,
        }],
        fields: Vec::new(),
    };
    let provider = world.provider.as_ref().expect("provider not built").clone();
    let request_id = provider
        .send_for_signature(notation_id, PARKED_PDF, &manifest, SendMode::Send)
        .await
        .expect("fake provider sends the parked envelope");
    provider
        .sign(&request_id, SIGNER_EMAIL)
        .await
        .expect("fake provider signs the parked envelope");

    store::notations::update_state(&surreal, notation_id, PARKED)
        .await
        .unwrap();
    store::signatures::record_request(
        &surreal,
        notation_id,
        store::signatures::SignatureProvider::DocuSign,
        &request_id.0,
    )
    .await
    .unwrap();

    world.notation_id = Some(notation_id);
    world.project_id = Some(proj.id);
    world.envelope_id = Some(request_id.0);
}

async fn post_callback(world: &mut SignBeforeCaseWorld, body: Vec<u8>, signature: &str) {
    let resp = world
        .app()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhook/esignature/any-path-token")
                .header("content-type", "application/json")
                .header("x-docusign-signature-1", signature)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    world.last_status = Some(resp.status());
    world.last_body = body_string(resp).await;
}

#[when("the provider posts a validly-signed completion callback")]
async fn post_valid(world: &mut SignBeforeCaseWorld) {
    let body = completion_body(world.envelope_id());
    let signature = sign_hmac_sha256_b64(HMAC_KEY.as_bytes(), &body);
    post_callback(world, body.clone(), &signature).await;
    world.last_callback = Some((body, signature));
}

#[when("the provider posts the same completion callback again")]
async fn repost_valid(world: &mut SignBeforeCaseWorld) {
    let (body, signature) = world
        .last_callback
        .clone()
        .expect("no callback was posted before");
    post_callback(world, body, &signature).await;
}

#[when("an attacker posts a completion callback with a forged signature")]
async fn post_forged(world: &mut SignBeforeCaseWorld) {
    // A plausible-looking but wrong base64 digest.
    post_callback(
        world,
        completion_body(world.envelope_id()),
        "Zm9yZ2VkLXNpZ25hdHVyZS1ub3QtdmFsaWQ=",
    )
    .await;
}

#[then(regex = r"^the response status is (\d+)$")]
async fn assert_status(world: &mut SignBeforeCaseWorld, code: u16) {
    assert_eq!(
        world.last_status.expect("no status captured").as_u16(),
        code,
        "body: {}",
        world.last_body
    );
}

/// The matter's filed documents — every asset row on the Project.
async fn filed_documents(world: &SignBeforeCaseWorld) -> Vec<store::assets::Asset> {
    store::assets::for_project(&world.store(), world.project_id())
        .await
        .expect("read the matter's filed documents")
}

#[then(regex = r"^the signed declaration is filed on the matter exactly once$")]
async fn assert_signed_filed_once(world: &mut SignBeforeCaseWorld) {
    let filed: Vec<_> = filed_documents(world)
        .await
        .into_iter()
        .filter(|asset| asset.filename.as_deref() == Some(SIGNED_FILENAME))
        .collect();
    assert_eq!(
        filed.len(),
        1,
        "exactly one signed declaration is filed on the matter"
    );
    let signed = &filed[0];
    assert_eq!(signed.project_id, Some(world.project_id()));
    assert_eq!(
        signed.source.as_deref(),
        Some("docusign"),
        "the executed copy names the channel that produced it"
    );
    assert_eq!(
        signed.visibility,
        store::documents::visibility::CLIENT,
        "the executed instrument is the client's own document"
    );
}

#[then(regex = r"^its certificate of completion is filed on the matter exactly once$")]
async fn assert_certificate_filed_once(world: &mut SignBeforeCaseWorld) {
    let filed: Vec<_> = filed_documents(world)
        .await
        .into_iter()
        .filter(|asset| asset.filename.as_deref() == Some(CERTIFICATE_FILENAME))
        .collect();
    assert_eq!(
        filed.len(),
        1,
        "exactly one certificate of completion is filed on the matter"
    );
    let certificate = &filed[0];
    assert_eq!(certificate.project_id, Some(world.project_id()));
    assert_eq!(
        certificate.visibility,
        store::documents::visibility::INTERNAL,
        "the ESIGN record is the firm's, not the client's"
    );
}

#[then(regex = r"^the filed signed copy records that the client signed by DocuSign$")]
async fn assert_signing_recorded(world: &mut SignBeforeCaseWorld) {
    let filed: Vec<_> = filed_documents(world)
        .await
        .into_iter()
        .filter(|asset| asset.filename.as_deref() == Some(SIGNED_FILENAME))
        .collect();
    let signed = filed
        .first()
        .expect("the signed declaration is filed (earlier step)");
    let signing = signed
        .signing
        .as_ref()
        .expect("the executed copy records how it was signed");
    assert_eq!(signing.method, store::documents::SigningMethod::Docusign);
    assert_eq!(
        signing.signed_on,
        chrono::Utc::now().date_naive(),
        "the signing date is the completion date"
    );
}

#[then("the matter has no filed documents")]
async fn assert_nothing_filed(world: &mut SignBeforeCaseWorld) {
    assert!(
        filed_documents(world).await.is_empty(),
        "a forged completion must not file anything on the matter"
    );
}

#[tokio::main]
async fn main() {
    // Each composed site owns SSR worker pools; bound concurrent app
    // construction.
    SignBeforeCaseWorld::cucumber()
        .max_concurrent_scenarios(Some(1))
        .run_and_exit("tests/features/sign_before_case.feature")
        .await;
}

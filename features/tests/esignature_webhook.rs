//! Cucumber runner for `features/esignature_webhook.feature`.
//!
//! Drives the inbound e-signature completion webhook
//! (`portal::esignature_webhook`) against an in-memory runtime: a retainer
//! is parked at `sent_for_signature__pending` with a known envelope id,
//! then the provider's completion callback is POSTed — once validly
//! signed (advances to END), once with a forged signature (rejected,
//! stays pending).

#![allow(clippy::unused_async)]
#![allow(clippy::doc_markdown)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use cucumber::{given, then, when, World};
use features::{app_state, body_string, fs_storage};
use portal::signature::{
    FakeSignatureProvider, SignatureManifest, SignatureProvider, SignatureRecipient,
};
use portal::webhook_auth::sign_hmac_sha256_b64;
use portal::{policy::PolicyClient, SessionStore};
use store::seed;
use tower::ServiceExt;
use uuid::Uuid;
use workflows::{InMemoryRuntime, MachineKind, StateMachineRuntime};

const TEMPLATE_CODE: &str = "onboarding__letter";
const HMAC_KEY: &str = "test-docusign-hmac-key";
const PARKED: &str = "sent_for_signature__pending";
const SIGNER_EMAIL: &str = "libra@example.com";
const PARKED_PDF: &[u8] = b"%PDF-1.4 parked retainer awaiting the provider's callback";

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

fn terminal_body(envelope_id: &str, event: &str, status: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "event": event,
        "data": {
            "envelopeId": envelope_id,
            "envelopeSummary": { "status": status },
        },
    }))
    .unwrap()
}

#[derive(Default, World)]
#[world(init = Self::default)]
struct WebhookWorld {
    app: Option<axum::Router>,
    runtime: Option<Arc<InMemoryRuntime>>,
    notation_id: Option<Uuid>,
    /// The envelope id actually recorded in `signatures`, minted by the
    /// held fake provider as `fake-{notation_id}-1`. Notation ids are
    /// unique per scenario in the process-shared
    /// [`features::shared_surreal`] engine, so the minted envelope ids
    /// never collide on the `(provider, provider_id)` unique index —
    /// two scenarios both parking on the feature file's literal prefix
    /// resolve to their own notation and their own envelope.
    envelope_id: Option<String>,
    /// The app's signature provider — the same instance the webhook's
    /// consumption path reads, held so the parked envelope can be sent
    /// and signed through the real provider seam.
    provider: Option<Arc<FakeSignatureProvider>>,
    last_status: Option<StatusCode>,
    last_body: String,
}

impl std::fmt::Debug for WebhookWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookWorld")
            .field("notation_id", &self.notation_id)
            .field("last_status", &self.last_status)
            .finish_non_exhaustive()
    }
}

impl WebhookWorld {
    fn app(&self) -> axum::Router {
        self.app.as_ref().expect("app not built").clone()
    }
    fn runtime(&self) -> &Arc<InMemoryRuntime> {
        self.runtime.as_ref().expect("runtime not built")
    }
    fn notation_id(&self) -> Uuid {
        self.notation_id.expect("notation not built")
    }
    fn envelope_id(&self) -> &str {
        self.envelope_id.as_deref().expect("envelope not built")
    }
    fn provider(&self) -> Arc<FakeSignatureProvider> {
        self.provider.as_ref().expect("provider not built").clone()
    }
}

#[given("a Neon Law Navigator app with an HMAC-secured e-signature webhook")]
async fn build_app(world: &mut WebhookWorld) {
    let surreal = features::shared_surreal().await;
    let storage = fs_storage("esignature-webhook").await;
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
    // Arm the HMAC gate so the webhook actually verifies signatures —
    // without this the dev posture would accept the forged callback.
    state.esignature_hmac_key = Some(HMAC_KEY.to_string());
    // The webhook's consumption path reads `state.signature_provider` to
    // archive completed documents; hand it the instance the world holds so
    // envelopes sent below are visible to the handler under test.
    let provider = Arc::new(FakeSignatureProvider::new());
    state.signature_provider = provider.clone();
    let router = features::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));
    world.app = Some(router);
    world.runtime = Some(runtime);
    world.provider = Some(provider);
}

#[given(regex = r#"^a retainer parked at sent_for_signature__pending with envelope id "([^"]+)"$"#)]
async fn park_retainer(world: &mut WebhookWorld, envelope_id: String) {
    // The feature's literal names the id scheme's prefix only; the fake
    // provider mints the real envelope id below.
    let _ = envelope_id;
    let surreal = features::shared_surreal().await;
    let tmpl = store::templates::resolve(&features::shared_surreal().await, None, TEMPLATE_CODE)
        .await
        .unwrap()
        .expect("seed_canonical inserts onboarding__letter");
    let person = store::test_support::ensure_person(
        &surreal,
        &store::persons::NewPerson::new("Libra", "libra@example.com"),
    )
    .await;
    let proj = store::test_support::seed_project(&surreal, "retainer matter").await;
    let notation_id = store::notations::create(
        &surreal,
        &store::notations::NewNotation::new(tmpl.id, person.id, proj.id, "BEGIN"),
    )
    .await
    .unwrap()
    .id;

    // Drive the workflow timeline to the parked state through the same
    // in-memory runtime the webhook will later signal. The retainer
    // engagement is attorney-reviewed at lawyer_review before reaching
    // the signature wait — mirror that path exactly.
    let rt = world.runtime().as_ref();
    let spec = workflows::retainer_intake_spec();
    StateMachineRuntime::start(rt, MachineKind::Workflow, notation_id, &spec)
        .await
        .unwrap();
    for condition in [
        "intake_submitted",
        "retainer_rendered",
        "approved",
        "pdf_persisted",
    ] {
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

    // Persist the parked state, then make the provider-side half of the
    // send real: the held fake provider mints the envelope and the client's
    // signature completes it — exactly the state DocuSign is in when its
    // Connect callback posts. The journaled webhook then archives the
    // completed document set from the same envelope. The row names
    // DocuSign because the fake is its DocuSign-shaped double.
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
    let provider = world.provider();
    let request_id = provider
        .send_for_signature(notation_id, PARKED_PDF, &manifest)
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
    world.envelope_id = Some(request_id.0);
}

async fn post_callback(world: &mut WebhookWorld, body: Vec<u8>, signature: &str) {
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

#[when(
    regex = r#"^the provider posts a validly-signed completion callback for envelope "([^"]+)"$"#
)]
async fn post_valid(world: &mut WebhookWorld, feature_envelope_id: String) {
    // `feature_envelope_id` is the feature file's literal, kept only as the
    // scenario-mangled envelope's un-mangled prefix; the real envelope id
    // recorded in `signatures` is `world.envelope_id()` — see the field doc.
    let envelope_id = world.envelope_id().to_string();
    assert!(envelope_id.starts_with(&feature_envelope_id));
    let body = completion_body(&envelope_id);
    let signature = sign_hmac_sha256_b64(HMAC_KEY.as_bytes(), &body);
    post_callback(world, body, &signature).await;
}

#[when(
    regex = r#"^the provider posts a validly-signed (decline|expiry) callback for envelope "([^"]+)"$"#
)]
async fn post_terminal(world: &mut WebhookWorld, terminal: String, feature_envelope_id: String) {
    let envelope_id = world.envelope_id().to_string();
    assert!(envelope_id.starts_with(&feature_envelope_id));
    let (event, status) = match terminal.as_str() {
        "decline" => ("envelope-declined", "declined"),
        "expiry" => ("envelope-expired", "expired"),
        _ => unreachable!("the feature regex only permits terminal callbacks"),
    };
    let body = terminal_body(&envelope_id, event, status);
    let signature = sign_hmac_sha256_b64(HMAC_KEY.as_bytes(), &body);
    post_callback(world, body, &signature).await;
}

#[when(
    regex = r#"^an attacker posts a completion callback with a forged signature for envelope "([^"]+)"$"#
)]
async fn post_forged(world: &mut WebhookWorld, feature_envelope_id: String) {
    let envelope_id = world.envelope_id().to_string();
    assert!(envelope_id.starts_with(&feature_envelope_id));
    // A plausible-looking but wrong base64 digest.
    post_callback(
        world,
        completion_body(&envelope_id),
        "Zm9yZ2VkLXNpZ25hdHVyZS1ub3QtdmFsaWQ=",
    )
    .await;
}

#[then(regex = r"^the response status is (\d+)$")]
async fn assert_status(world: &mut WebhookWorld, code: u16) {
    assert_eq!(
        world.last_status.expect("no status captured").as_u16(),
        code,
        "body: {}",
        world.last_body
    );
}

#[then(regex = r#"^the retainer workflow has advanced to "([^"]+)"$"#)]
async fn assert_advanced(world: &mut WebhookWorld, state: String) {
    let events = StateMachineRuntime::events(
        world.runtime().as_ref(),
        MachineKind::Workflow,
        world.notation_id(),
    )
    .await;
    let last = events.last().expect("at least one transition");
    assert_eq!(last.to.as_str(), state, "events: {events:?}");
}

#[then(regex = r#"^the retainer workflow is still at "([^"]+)"$"#)]
async fn assert_still_at(world: &mut WebhookWorld, state: String) {
    let current = StateMachineRuntime::current_state(
        world.runtime().as_ref(),
        MachineKind::Workflow,
        world.notation_id(),
    )
    .await
    .expect("workflow exists");
    assert_eq!(current.as_str(), state);
}

#[then(regex = r#"^the notation row state is "([^"]+)"$"#)]
async fn assert_row_state(world: &mut WebhookWorld, state: String) {
    let surreal = features::shared_surreal().await;
    let row = store::notations::find_by_id(&surreal, world.notation_id())
        .await
        .unwrap()
        .expect("notation row");
    assert_eq!(row.state, state);
}

#[then(regex = r#"^the signature row state is "([^"]+)"$"#)]
async fn assert_signature_state(world: &mut WebhookWorld, state: String) {
    let surreal = features::shared_surreal().await;
    let signature = store::signatures::by_provider(
        &surreal,
        store::signatures::SignatureProvider::DocuSign,
        world.envelope_id(),
    )
    .await
    .unwrap()
    .expect("signature row");
    assert_eq!(signature.state.as_str(), state);
}

#[tokio::main]
async fn main() {
    WebhookWorld::cucumber()
        .run_and_exit("tests/features/esignature_webhook.feature")
        .await;
}

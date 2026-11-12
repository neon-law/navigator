//! The door to the signing ceremony: `GET /app/notations/:id/sign` and
//! `GET /app/lawyer/notations/:id/sign`.
//!
//! After the retainer is sent for signature the client is a **captive**
//! DocuSign recipient (see [`crate::retainer_walk::client_user_id`]) —
//! DocuSign does not email them, so a short-lived [recipient view] URL is the
//! only way in. This route mints one and **redirects the browser to DocuSign**.
//! The ceremony happens on the provider's own site; Navigator does not frame
//! it.
//!
//! ## Two lenses, two gates
//!
//! One handler is registered at both paths, exactly as
//! [`crate::documents::download`] is. The lens is the session's role, not
//! the URL prefix, and the gate follows from it:
//!
//! **Participation says you may look at the matter; identity says you may
//! sign as this person.**
//!
//! 1. *Participation*, following [`crate::documents`] verbatim:
//!    [`store::access::can_see_project_as_lawyer`] for a firm-tier session
//!    (keeping the documented Owner/Admin project-scoping bypass),
//!    [`store::access::can_see_project_as_client`] otherwise.
//! 2. *Identity*, for a non-firm caller only: the session's person must be
//!    the notation's bound signer. A matter can carry several client
//!    participants, so participation alone would let one of them open the
//!    other's ceremony. The firm lens is deliberately exempt — a lawyer
//!    minting the view for the client in the room is the in-office signing
//!    this route was built for, and there the caller is never the signer.
//!
//! Both refusals answer `404`, never `403`, for the reason spelled out at
//! [`crate::documents`]: a `403` on the identity check would confirm that
//! this notation exists and that the caller is on the matter.
//!
//! ## Completion does not come back through this browser session
//!
//! Because the signer leaves, they may never return here — they close the tab,
//! or finish on their phone. Nothing legal may depend on the round trip, and
//! nothing does: [`crate::esignature_webhook`] is the authoritative return
//! path. DocuSign Connect posts the completion, its HMAC is verified over the
//! raw body, and that signal advances the retainer workflow to `END` and
//! archives the executed PDF plus the Certificate of Completion. The
//! `return_url` below is a courtesy for the signer who does come back, not a
//! mechanism.
//!
//! The signing URL is single-use and expires in minutes, so it is minted fresh
//! on each request and never cached. The handler is generic over the
//! [`crate::signature::SignatureProvider`] seam, so the stub returns a
//! deterministic fake URL in dev / KIND.
//!
//! [recipient view]: https://developers.docusign.com/docs/esign-rest-api/reference/envelopes/envelopeviews/createrecipient/

use axum::extract::{Path as AxumPath, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Extension;
use uuid::Uuid;

use crate::admin::AdminState;
use crate::retainer_walk::client_recipient_for_notation;
use crate::session::SessionData;
use crate::signature::RecipientView;
use crate::AppState;

/// `GET /app/notations/:id/sign` and `GET /app/lawyer/notations/:id/sign` —
/// mint a single-use recipient-view URL for the notation's captive client
/// and send the browser to DocuSign.
///
/// One handler serves both registrations; see the module docs for the two
/// gates it applies and why the identity gate is client-lens only.
pub async fn sign_get(
    State(state): State<AdminState>,
    Extension(session): Extension<SessionData>,
    headers: HeaderMap,
    AxumPath(notation_id): AxumPath<Uuid>,
) -> Response {
    let Some(notation_row) = store::notations::find_by_id(&state.surreal, notation_id)
        .await
        .ok()
        .flatten()
    else {
        return (StatusCode::NOT_FOUND, "notation not found").into_response();
    };

    let firm_lens = session.role.is_lawyer_tier();

    // (1) Project ACL follows the caller's tier, not the URL — the same fork
    // `crate::documents::download` applies, so the two notation surfaces read
    // one membership table the same way.
    let decision = if firm_lens {
        store::access::can_see_project_as_lawyer(
            &state.surreal,
            session.person_id,
            session.role,
            notation_row.project_id,
        )
        .await
    } else {
        store::access::can_see_project_as_client(
            &state.surreal,
            session.person_id,
            notation_row.project_id,
        )
        .await
    };
    match decision {
        Ok(true) => {}
        Ok(false) => return (StatusCode::NOT_FOUND, "notation not found").into_response(),
        Err(e) => {
            tracing::error!(error = %e, %notation_id, "esign_view: participation check failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    // (2) Identity, client lens only: a matter can carry several client
    // participants, and participation alone would let one open another's
    // ceremony. Keyed on `person_id` rather than on the resolved name or
    // email, because the name may come from a questionnaire answer while the
    // bound signer never moves.
    if !firm_lens && session.person_id != Some(notation_row.person_id) {
        return (StatusCode::NOT_FOUND, "notation not found").into_response();
    }

    // The envelope must already exist (the retainer walk records the id in
    // `signatures` when it parks at `sent_for_signature__pending`). No id →
    // there is nothing to sign yet.
    let Some(request_id) = store::signatures::request_id_for_notation(&state.surreal, notation_id)
        .await
        .ok()
        .flatten()
    else {
        return (
            StatusCode::CONFLICT,
            "this matter has not been sent for signature yet",
        )
            .into_response();
    };

    // The captive recipient is resolved on the email/name/clientUserId
    // triple, so it must match the envelope exactly. Resolved through the
    // *same* function the send path used, so the two cannot drift.
    let recipient =
        match client_recipient_for_notation(&state.surreal, &state.storage, &notation_row).await {
            Ok(recipient) => recipient,
            Err(e) => {
                tracing::error!(error = %e, %notation_id, "esign_view: recipient resolve failed");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        };
    if recipient.name.is_empty() || recipient.email.is_empty() {
        return (StatusCode::NOT_FOUND, "client not found").into_response();
    }
    // A recipient view is only valid for a recipient sent with a
    // `clientUserId`. An `emailed` notation has none: DocuSign mailed that
    // signer their own link and there is no embedded session to mint.
    let Some(client_user_id) = recipient.client_user_id else {
        return (
            StatusCode::CONFLICT,
            "this matter's envelope was emailed to the signer; there is no session to open here",
        )
            .into_response();
    };

    let view = RecipientView {
        return_url: return_url_for(&state, &headers, &notation_row, firm_lens).await,
        email: recipient.email,
        name: recipient.name,
        client_user_id,
    };

    match state
        .signature_provider
        .create_recipient_view(&crate::signature::SignatureRequestId(request_id), &view)
        .await
    {
        Ok(signing_url) => redirect_to_provider(&signing_url).unwrap_or_else(|| {
            tracing::error!(
                %notation_id,
                "esign_view: provider returned a signing URL that is not an absolute https URL"
            );
            (
                StatusCode::BAD_GATEWAY,
                "could not start the signing session; please retry",
            )
                .into_response()
        }),
        Err(e) => {
            tracing::error!(error = %e, %notation_id, "esign_view: recipient view failed");
            (
                StatusCode::BAD_GATEWAY,
                "could not start the signing session; please retry",
            )
                .into_response()
        }
    }
}

/// Where DocuSign sends the browser once the ceremony ends. A courtesy for the
/// signer who comes back, not a mechanism: the executed state arrives on
/// [`crate::esignature_webhook`] whether this redirect is ever followed or not.
///
/// **Absolute, and lens-aware.** Absolute because the provider resolves a
/// relative path against *its own* origin rather than Navigator's, so a
/// relative `return_url` strands the signer on DocuSign;
/// [`crate::openapi::base_url_for`] is the tree's one resolver for the public
/// authority (brand `base_url` → `NAV_BASE_URL` → request `Host`). Lens-aware
/// because the firm's step page lives under `/app/lawyer`, which the policy
/// refuses a client — handing a client an absolute URL into a `403` would be a
/// regression dressed as a fix. A client goes to their matter page instead,
/// and to the matter list when the Project cannot be read.
async fn return_url_for(
    state: &AdminState,
    headers: &HeaderMap,
    notation_row: &store::notations::Notation,
    firm_lens: bool,
) -> String {
    let base = crate::openapi::base_url_for(
        headers
            .get(header::HOST)
            .and_then(|value| value.to_str().ok()),
    );
    let base = base.trim_end_matches('/');
    if firm_lens {
        let notation_id = notation_row.id;
        return format!("{base}/app/lawyer/notations/{notation_id}/step");
    }
    match store::projects::find_by_id(&state.surreal, notation_row.project_id).await {
        Ok(Some(project)) => format!("{base}/app/projects/{}", project.code),
        _ => format!("{base}/app/projects"),
    }
}

/// Build the `303` that hands the signer to the provider, or `None` when the
/// URL is not one we are willing to send a browser to.
///
/// The destination comes from a third party over the network, so it is treated
/// as untrusted. Only an absolute `https` URL with a host is accepted, which
/// rules out an open redirect (`//evil.example`), a relative path back into
/// Navigator, a scheme that executes in the page (`javascript:`, `data:`), and
/// a downgrade to plaintext for a URL carrying a single-use signing
/// credential.
///
/// Header injection is handled by *parsing*, not by the accept/reject test: the
/// WHATWG parser strips CR, LF, and tab outright and percent-encodes the rest,
/// so what is emitted is the parser's normalized serialization rather than the
/// provider's bytes. Note the consequence — a hostile URL is not refused here,
/// it is defanged, and the redirect still happens to the sanitized target.
///
/// The header is nonetheless built fallibly rather than through
/// `axum::response::Redirect::to`, which panics on a value `HeaderValue`
/// rejects. Normalization should make that unreachable; a `502` is the right
/// answer if it ever is not, and a panic never is.
fn redirect_to_provider(signing_url: &str) -> Option<Response> {
    let parsed = url::Url::parse(signing_url).ok()?;
    // Production providers are https. The one http exception is the
    // loopback host the fake's sign pages are served from in dev — and
    // the fake cannot boot in production, so this never widens what a
    // real deployment forwards.
    let loopback_http = parsed.scheme() == "http"
        && parsed
            .host_str()
            .is_some_and(|h| matches!(h, "127.0.0.1" | "localhost" | "[::1]"));
    if (parsed.scheme() != "https" || !parsed.has_host()) && !loopback_http {
        return None;
    }

    let location = HeaderValue::from_str(parsed.as_str()).ok()?;
    Some((StatusCode::SEE_OTHER, [(header::LOCATION, location)]).into_response())
}

/// `GET /app/lawyer/notations/:id/sender-view` — mint the sender-view URL
/// for this notation's envelope and send the browser to it: the page where
/// the *sender* watches the envelope (its status, every recipient's signing
/// state, and — for the fake — what the webhook delivery did). The
/// sender-side observability seam: without it the sender learns an
/// envelope's state only from the completion webhook, or not at all.
///
/// Lawyer tier only: the sender is the lawyer who sent the envelope, and
/// the route is registered only under `/app/lawyer`. Participation gates
/// the lookup exactly as [`sign_get`] applies it — a lawyer not on the
/// matter reads a `404`. A notation never sent for signature is a `409`:
/// there is no envelope to observe yet. Like the recipient view, the URL
/// is minted fresh per request and never cached — a window, not a state.
pub async fn sender_view_get(
    State(state): State<AdminState>,
    Extension(session): Extension<SessionData>,
    headers: HeaderMap,
    AxumPath(notation_id): AxumPath<Uuid>,
) -> Response {
    // A client session has no sender view to open. Same refusal
    // discipline as the identity gate below: `404`, never `403` —
    // existence is not confirmed to the caller.
    if !session.role.is_lawyer_tier() {
        return (StatusCode::NOT_FOUND, "notation not found").into_response();
    }

    let Some(notation_row) = store::notations::find_by_id(&state.surreal, notation_id)
        .await
        .ok()
        .flatten()
    else {
        return (StatusCode::NOT_FOUND, "notation not found").into_response();
    };

    // (1) Participation, firm lens only — this route is never registered
    // for the client lens, so the check is the lawyer one verbatim.
    match store::access::can_see_project_as_lawyer(
        &state.surreal,
        session.person_id,
        session.role,
        notation_row.project_id,
    )
    .await
    {
        Ok(true) => {}
        Ok(false) => return (StatusCode::NOT_FOUND, "notation not found").into_response(),
        Err(e) => {
            tracing::error!(error = %e, %notation_id, "esign_view: participation check failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    // (2) The envelope must already exist (the retainer walk records the
    // id in `signatures` when it parks at `sent_for_signature__pending`).
    // No id → nothing has been sent, so there is nothing to observe.
    let Some(request_id) = store::signatures::request_id_for_notation(&state.surreal, notation_id)
        .await
        .ok()
        .flatten()
    else {
        return (
            StatusCode::CONFLICT,
            "this matter has not been sent for signature yet",
        )
            .into_response();
    };

    // Back to the lawyer's own step page when the console closes — a
    // courtesy, same as the signer's `return_url`, never a mechanism.
    let base = crate::openapi::base_url_for(
        headers
            .get(header::HOST)
            .and_then(|value| value.to_str().ok()),
    );
    let return_url = format!(
        "{}/app/lawyer/notations/{notation_id}/step",
        base.trim_end_matches('/')
    );

    match state
        .signature_provider
        .create_sender_view(
            &crate::signature::SignatureRequestId(request_id),
            &return_url,
        )
        .await
    {
        Ok(view_url) => redirect_to_provider(&view_url).unwrap_or_else(|| {
            tracing::error!(
                %notation_id,
                "esign_view: provider returned a sender-view URL that is not an absolute https URL"
            );
            (
                StatusCode::BAD_GATEWAY,
                "could not open the sender view; please retry",
            )
                .into_response()
        }),
        Err(e) => {
            tracing::error!(error = %e, %notation_id, "esign_view: sender view failed");
            (
                StatusCode::BAD_GATEWAY,
                "could not open the sender view; please retry",
            )
                .into_response()
        }
    }
}

/// The envelope ids the fake mints (`fake-<notation uuid>-<sequence>`) are
/// alphanumeric-plus-dash. Anything else never came from the fake — and
/// reading a page for it would serve a path that escapes the fake's
/// directory: the path extractor decodes `%2F` inside a segment, so a
/// hostile id can carry `/` (and `..`) into `join`. Refused before the
/// join, not after.
fn envelope_id_is_safe(envelope_id: &str) -> bool {
    !envelope_id.is_empty()
        && envelope_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// The fake provider's per-recipient sign page, served by this portal in
/// dev: the URL the signer is redirected to is one on this server, backed
/// by the page the fake wrote to disk. `404` when the fake is not the
/// running provider or no page exists for this envelope and recipient —
/// a missing page is an unknown envelope, not an error to bury.
pub(crate) async fn fake_signing_page(
    State(state): State<AppState>,
    AxumPath((envelope_id, email)): AxumPath<(String, String)>,
) -> Response {
    let Some(dir) = state.fake_signature_dir else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !envelope_id_is_safe(&envelope_id) {
        return StatusCode::NOT_FOUND.into_response();
    }
    // The same slug discipline the file was written under — idempotent,
    // so a URL carrying either the raw email or an already-slugged
    // segment resolves, and no path segment can escape the directory.
    let page = dir
        .join("signing")
        .join(&envelope_id)
        .join(format!("{}.html", crate::signature::recipient_slug(&email)));
    match tokio::fs::read_to_string(&page).await {
        Ok(html) => ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// The fake provider's sender page, served by this portal in dev: the URL
/// the lawyer is redirected to is one on this server, backed by the page
/// the fake (re)generated when the sender view was minted. Same refusal
/// discipline as [`fake_signing_page`] — `404` when the fake is not the
/// running provider or the envelope has no sender page.
pub(crate) async fn fake_sender_page(
    State(state): State<AppState>,
    AxumPath(envelope_id): AxumPath<String>,
) -> Response {
    let Some(dir) = state.fake_signature_dir else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !envelope_id_is_safe(&envelope_id) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let page = dir.join("signing").join(&envelope_id).join("sender.html");
    match tokio::fs::read_to_string(&page).await {
        Ok(html) => ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        fake_sender_page, fake_signing_page, redirect_to_provider, sender_view_get, sign_get,
    };
    use crate::admin::AdminState;
    use crate::session::SessionData;
    use crate::signature::{
        SendMode, SignatureManifest, SignatureProvider, SignatureRecipient, SignatureRequestId,
    };
    use crate::test_support::app_state;
    use axum::extract::{Path as AxumPath, State};
    use axum::http::{header, HeaderMap, StatusCode};
    use axum::Extension;
    use std::sync::Arc;
    use store::surreal::test_support;
    use uuid::Uuid;

    #[test]
    fn a_provider_url_becomes_a_redirect_to_docusign() {
        let url = "https://demo.docusign.net/signing/abc123";
        let response = redirect_to_provider(url).expect("an https provider URL is redirectable");
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response.headers().get(header::LOCATION).unwrap(),
            url,
            "the signer is sent to the provider's own page, unmodified"
        );
    }

    #[test]
    fn the_fakes_loopback_sign_page_redirects_too() {
        // Dev only: the fake serves its ceremony from this server's own
        // loopback. A production provider can never produce this URL —
        // the fake refuses to boot there.
        let url = "http://127.0.0.1:3001/fake-signature/fake-abc-1/libra-example-com";
        let response = redirect_to_provider(url).expect("a loopback sign page is redirectable");
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
    }

    #[test]
    fn a_plain_http_url_to_a_remote_host_is_refused() {
        let url = "http://evil.example.com/signing/abc123";
        assert!(
            redirect_to_provider(url).is_none(),
            "http to anything but loopback must not become a redirect"
        );
    }

    #[test]
    fn a_url_that_is_not_absolute_https_is_refused() {
        // The destination is a third party's network response, so every one of
        // these would be a way to point a signer somewhere we did not intend.
        for hostile in [
            "http://demo.docusign.net/signing/abc", // plaintext downgrade
            "//evil.example/signing",               // protocol-relative
            "/app/lawyer/notations",                // relative, back into Navigator
            "javascript:alert(1)",                  // executes in the page
            "data:text/html,<script>alert(1)</script>", // ditto
            "not a url at all",
            "",
        ] {
            assert!(
                redirect_to_provider(hostile).is_none(),
                "must refuse to redirect to {hostile:?}"
            );
        }
    }

    #[test]
    fn crlf_in_a_provider_url_cannot_forge_a_second_header() {
        // The parser strips CR/LF rather than rejecting the URL, so this one
        // *does* redirect — to a sanitized target. What must never happen is
        // the raw bytes reaching the header and splitting it in two.
        let response = redirect_to_provider("https://x/a\r\nX-Injected: 1")
            .expect("the parser normalizes rather than refusing");
        let location = response.headers().get(header::LOCATION).unwrap();
        assert!(
            !location.as_bytes().contains(&b'\r') && !location.as_bytes().contains(&b'\n'),
            "no bare CR/LF may survive into the Location header: {location:?}"
        );
        assert!(
            response.headers().get("x-injected").is_none(),
            "the crafted header name must not have become a real header"
        );
    }

    // ------------------------------------------------------------------
    // The view handlers, called the way the router calls them: the access
    // gates, the fresh-minted redirects, and the fake's on-disk pages.
    // ------------------------------------------------------------------

    /// A session holding `role`, bound to `person_id` — `SessionData::fresh`
    /// fills the rest.
    fn session_for(person_id: Uuid, role: store::persons::Role) -> SessionData {
        let mut session = SessionData::fresh(format!("test-{person_id}"), role);
        session.person_id = Some(person_id);
        session
    }

    /// The admin state the boot conversion builds from [`crate::AppState`]
    /// (mirrored from `lib.rs`'s registration wiring).
    fn admin_state(app: crate::AppState) -> AdminState {
        AdminState {
            surreal: app.surreal,
            workflow_runtime: app.workflow_runtime,
            signature_provider: app.signature_provider,
            retainer_intake_questionnaire: workflows::retainer_intake_questionnaire(),
            questionnaire_runtime: app.questionnaire_runtime,
            storage: app.storage,
            assets_storage: app.assets_storage,
            forms_registry: app.forms_registry,
            email: app.email,
            billing_provider: app.billing_provider,
            contract_reviewer: app.contract_reviewer,
            bootstrap_owner_email: app.bootstrap_owner_email,
            on_call_lawyer_email: app.on_call_lawyer_email,
            bootstrap_company: crate::admin::bootstrap_company_from_env(),
            sessions: app.sessions,
            secure_cookies: false,
            attachment_scanner: app.attachment_scanner,
            runtime_kms: app.runtime_kms,
            twilio_registry: None,
        }
    }

    /// AppState whose provider is the fake serving its pages from
    /// `http://127.0.0.1:8080` — the loopback the dev wiring uses — with the
    /// portal pointed at the fake's own on-disk root.
    async fn fake_app_state() -> (crate::AppState, std::path::PathBuf) {
        let mut state = app_state(test_support::mem().await).await;
        let root =
            std::env::temp_dir().join(format!("navigator-esign-view-{}", Uuid::now_v7().simple()));
        state.signature_provider = Arc::new(
            crate::signature::FakeSignatureProvider::new_in(root.clone())
                .with_signing_base_url("http://127.0.0.1:8080".into()),
        );
        state.fake_signature_dir = Some(root.clone());
        (state, root)
    }

    /// One client person, one Project, and one onboarding notation bound to
    /// them, with embedded delivery — the captive-recipient shape the sign
    /// route exists for.
    async fn seed_matter(
        surreal: &store::surreal::SurrealDb,
    ) -> (store::persons::Person, Uuid, Uuid) {
        seed_matter_with_delivery(surreal, store::notations::DELIVERY_EMBEDDED).await
    }

    async fn seed_matter_with_delivery(
        surreal: &store::surreal::SurrealDb,
        delivery: &str,
    ) -> (store::persons::Person, Uuid, Uuid) {
        let client = store::test_support::ensure_person(
            surreal,
            &store::persons::NewPerson::new("Libra", "libra@example.com"),
        )
        .await;
        let project_id =
            store::test_support::seed_project_surreal(surreal, "sender-view matter").await;
        let template = store::test_support::seed_unpinned_project_template(
            surreal,
            project_id,
            "engagement-letter",
            "Engagement Letter",
            "person",
            Some("onboarding"),
        )
        .await;
        let notation = store::notations::create(
            surreal,
            &store::notations::NewNotation::new(template.id, client.id, project_id, "BEGIN")
                .with_delivery(delivery),
        )
        .await
        .expect("seed the notation");
        (client, project_id, notation.id)
    }

    /// Send the notation for signature through the fake and record the
    /// request row the way `retainer_walk::send_for_signature` does — the
    /// envelope a view minting or a webhook needs to find.
    async fn seed_envelope(
        surreal: &store::surreal::SurrealDb,
        provider: &Arc<dyn SignatureProvider>,
        client: &store::persons::Person,
        notation_id: Uuid,
    ) -> SignatureRequestId {
        let manifest = SignatureManifest {
            recipients: vec![SignatureRecipient {
                role: "client".into(),
                email: client.email.clone(),
                name: client.name.clone(),
                routing_order: 1,
                client_user_id: Some(crate::retainer_walk::client_user_id(notation_id)),
            }],
            fields: Vec::new(),
        };
        let request_id = provider
            .send_for_signature(notation_id, b"%PDF-1.7 fake retainer", &manifest, SendMode::Send)
            .await
            .expect("the fake accepts an anchor-free manifest");
        store::signatures::record_request(
            surreal,
            notation_id,
            store::signatures::SignatureProvider::DocuSign,
            &request_id.0,
        )
        .await
        .expect("record the envelope request");
        request_id
    }

    /// A matter with its client participation and its envelope already sent:
    /// everything the view handlers and webhooks below read. Returns the
    /// admin state, the seeded ids, and the fake's on-disk root — the root
    /// the minted view URLs are backed by.
    #[allow(clippy::type_complexity)]
    async fn seed_signing_matter() -> (
        AdminState,
        store::persons::Person,
        Uuid,
        Uuid,
        std::path::PathBuf,
    ) {
        let (state, root) = fake_app_state().await;
        let (client, project_id, notation_id) = seed_matter(&state.surreal).await;
        store::projects::add_participation(&state.surreal, project_id, client.id, "client")
            .await
            .expect("bind the client to the matter");
        let admin = admin_state(state);
        let surreal = admin.surreal.clone();
        let provider = admin.signature_provider.clone();
        seed_envelope(&surreal, &provider, &client, notation_id).await;
        (admin, client, project_id, notation_id, root)
    }

    // --- the fake's on-disk pages, served by the portal ---

    #[tokio::test]
    async fn the_fakes_sign_page_serves_the_page_the_envelope_wrote() {
        let (state, _root) = fake_app_state().await;
        let (client, _project_id, notation_id) = seed_matter(&state.surreal).await;
        let request_id = seed_envelope(
            &state.surreal,
            &state.signature_provider,
            &client,
            notation_id,
        )
        .await;
        // Mint the recipient view the way the sign route does, so the page
        // exists on disk under the slug the URL carries.
        let view = crate::signature::RecipientView {
            return_url: "http://127.0.0.1:8080/app".into(),
            email: client.email.clone(),
            name: client.name.clone(),
            client_user_id: crate::retainer_walk::client_user_id(notation_id),
        };
        let url = state
            .signature_provider
            .create_recipient_view(&request_id, &view)
            .await
            .expect("the fake mints a recipient view");

        let response = fake_signing_page(
            State(state),
            AxumPath((request_id.0.clone(), client.email.clone())),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/html; charset=utf-8"
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read the page body");
        let html = String::from_utf8(body.to_vec()).expect("the page is utf-8");
        assert!(
            html.contains(&request_id.0),
            "the served page names its envelope: {url}"
        );
    }

    #[tokio::test]
    async fn a_signing_page_for_an_unknown_envelope_is_not_found() {
        let (state, _root) = fake_app_state().await;
        let response = fake_signing_page(
            State(state),
            AxumPath(("fake-never-sent-1".into(), "libra@example.com".into())),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_hostile_envelope_id_never_reaches_the_signing_directory() {
        let (state, root) = fake_app_state().await;
        // The page that must NOT be served if the id escaped its directory.
        let signing_dir = root.join("signing");
        std::fs::create_dir_all(&signing_dir).expect("create the signing directory");
        let escaped = signing_dir.join("secret.html");
        std::fs::write(&escaped, "<html>secret</html>").expect("write the decoy");

        let response = fake_signing_page(
            State(state),
            AxumPath(("..%2Fsecret".into(), "libra@example.com".into())),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(
            escaped.exists(),
            "the decoy's existence is irrelevant — the handler must refuse before reading"
        );
    }

    #[tokio::test]
    async fn the_sign_pages_answer_not_found_when_the_fake_is_not_the_provider() {
        let mut state = app_state(test_support::mem().await).await;
        state.fake_signature_dir = None;
        let response = fake_signing_page(
            State(state),
            AxumPath(("fake-abc-1".into(), "libra@example.com".into())),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_fakes_sender_page_serves_the_page_the_send_wrote() {
        let (state, _root) = fake_app_state().await;
        let (client, _project_id, notation_id) = seed_matter(&state.surreal).await;
        let request_id = seed_envelope(
            &state.surreal,
            &state.signature_provider,
            &client,
            notation_id,
        )
        .await;
        state
            .signature_provider
            .create_sender_view(&request_id, "http://127.0.0.1:8080/app")
            .await
            .expect("the fake mints a sender view");

        let response = fake_sender_page(State(state), AxumPath(request_id.0.clone())).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read the page body");
        let html = String::from_utf8(body.to_vec()).expect("the page is utf-8");
        assert!(
            html.contains(&request_id.0),
            "the sender page names the envelope it watches"
        );
    }

    #[tokio::test]
    async fn a_sender_page_for_an_unknown_envelope_is_not_found() {
        let (state, _root) = fake_app_state().await;
        let response = fake_sender_page(State(state), AxumPath("fake-never-sent-1".into())).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    // --- sender_view_get: the lawyer's window on the envelope ---

    #[tokio::test]
    async fn a_client_session_has_no_sender_view() {
        let (admin, client, _project_id, notation_id, _root) = seed_signing_matter().await;
        let response = sender_view_get(
            State(admin),
            Extension(session_for(client.id, store::persons::Role::Client)),
            HeaderMap::new(),
            AxumPath(notation_id),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_sender_view_for_an_unknown_notation_is_not_found() {
        let (admin, _client, _project_id, _notation_id, _root) = seed_signing_matter().await;
        let lawyer = store::test_support::ensure_person(
            &admin.surreal,
            &store::persons::NewPerson::new("Aries", "aries@example.com"),
        )
        .await;
        let response = sender_view_get(
            State(admin),
            Extension(session_for(lawyer.id, store::persons::Role::Admin)),
            HeaderMap::new(),
            AxumPath(Uuid::now_v7()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_sender_view_for_an_unsent_matter_conflicts() {
        let (state, _root) = fake_app_state().await;
        let (client, project_id, notation_id) = seed_matter(&state.surreal).await;
        store::projects::add_participation(&state.surreal, project_id, client.id, "client")
            .await
            .expect("bind the client to the matter");
        let admin = admin_state(state);
        // No envelope: the matter has not been sent for signature. An Owner
        // session passes the tier and participation gates, so the conflict
        // is the missing envelope, not a refusal.
        let owner = store::test_support::ensure_person(
            &admin.surreal,
            &store::persons::NewPerson::new("Aries", "aries@example.com"),
        )
        .await;
        let response = sender_view_get(
            State(admin),
            Extension(session_for(owner.id, store::persons::Role::Owner)),
            HeaderMap::new(),
            AxumPath(notation_id),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn a_lawyer_off_the_matter_has_no_sender_view() {
        let (admin, _client, _project_id, notation_id, _root) = seed_signing_matter().await;
        // A plain Lawyer with no participation row on this matter: the
        // firm-tier gate refuses before the envelope is ever consulted.
        let outsider = store::test_support::ensure_person(
            &admin.surreal,
            &store::persons::NewPerson::new("Aries", "aries@example.com"),
        )
        .await;
        let response = sender_view_get(
            State(admin),
            Extension(session_for(outsider.id, store::persons::Role::Lawyer)),
            HeaderMap::new(),
            AxumPath(notation_id),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_sender_opens_the_envelopes_watch_page() {
        let (admin, _client, _project_id, notation_id, root) = seed_signing_matter().await;
        let response = sender_view_get(
            State(admin),
            Extension(session_for(Uuid::now_v7(), store::persons::Role::Admin)),
            HeaderMap::new(),
            AxumPath(notation_id),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get(header::LOCATION)
            .expect("a redirect carries a Location")
            .to_str()
            .expect("the Location is ascii")
            .to_string();
        let segments: Vec<&str> = location
            .trim_start_matches("http://127.0.0.1:8080/")
            .split('/')
            .collect();
        assert_eq!(
            segments.len(),
            2,
            "the redirect targets the fake's sender page for one envelope"
        );
        assert_eq!(segments[1], "sender");
        let envelope_id = segments[0].to_string();

        // The URL is minted from the same state the page handler serves:
        // the sender page the fake (re)generated at mint time exists on
        // disk under the redirect's own envelope id, names the envelope,
        // and is served by the portal's fake-sender-page route.
        let page = root.join("signing").join(&envelope_id).join("sender.html");
        let html = tokio::fs::read_to_string(&page)
            .await
            .expect("the minted sender URL is backed by a page on disk");
        assert!(html.contains(&envelope_id), "the page names its envelope");
        // The portal serves the same directory when the fake's pages are
        // wired in — a state pointed at this root answers 200 for it.
        let mut served_state = app_state(test_support::mem().await).await;
        served_state.fake_signature_dir = Some(root.clone());
        let served = fake_sender_page(State(served_state), AxumPath(envelope_id.clone())).await;
        assert_eq!(served.status(), StatusCode::OK);
    }

    // --- sign_get: the captive client's door to the ceremony ---

    #[tokio::test]
    async fn a_signature_link_for_an_unknown_notation_is_not_found() {
        let (admin, _client, _project_id, _notation_id, _root) = seed_signing_matter().await;
        let response = sign_get(
            State(admin),
            Extension(session_for(Uuid::now_v7(), store::persons::Role::Client)),
            HeaderMap::new(),
            AxumPath(Uuid::now_v7()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_signature_link_before_any_send_conflicts() {
        let (state, _root) = fake_app_state().await;
        let (client, project_id, notation_id) = seed_matter(&state.surreal).await;
        store::projects::add_participation(&state.surreal, project_id, client.id, "client")
            .await
            .expect("bind the client to the matter");
        let admin = admin_state(state);
        // The client is on the matter and IS the notation's signer, but no
        // envelope exists yet — the refusal is the missing send, not access.
        let response = sign_get(
            State(admin),
            Extension(session_for(client.id, store::persons::Role::Client)),
            HeaderMap::new(),
            AxumPath(notation_id),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn one_client_cannot_open_anothers_signing_ceremony() {
        let (admin, client, project_id, notation_id, _root) = seed_signing_matter().await;
        // A second client ON the matter — participation holds, identity does
        // not: the notation's bound signer is the first client.
        let other = store::test_support::ensure_person(
            &admin.surreal,
            &store::persons::NewPerson::new("Scorpio", "scorpio@example.com"),
        )
        .await;
        store::projects::add_participation(&admin.surreal, project_id, other.id, "client")
            .await
            .expect("bind the second client to the matter");
        let response = sign_get(
            State(admin),
            Extension(session_for(other.id, store::persons::Role::Client)),
            HeaderMap::new(),
            AxumPath(notation_id),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_ne!(
            other.id, client.id,
            "the second client is a different person"
        );
    }

    #[tokio::test]
    async fn the_captive_client_is_redirected_to_their_signing_ceremony() {
        let (admin, client, _project_id, notation_id, root) = seed_signing_matter().await;
        let response = sign_get(
            State(admin),
            Extension(session_for(client.id, store::persons::Role::Client)),
            HeaderMap::new(),
            AxumPath(notation_id),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get(header::LOCATION)
            .expect("a redirect carries a Location")
            .to_str()
            .expect("the Location is ascii")
            .to_string();
        // The redirect carries the fake's own loopback page: one path
        // segment per envelope id, one per recipient slug.
        let segments: Vec<&str> = location
            .trim_start_matches("http://127.0.0.1:8080/")
            .split('/')
            .collect();
        assert_eq!(segments.len(), 2, "the redirect targets one sign page");
        assert_ne!(
            segments[1], "sender",
            "the client gets a sign page, not the sender page"
        );
        let envelope_id = segments[0].to_string();

        // The minted URL is backed by the page the fake wrote at mint time,
        // served by the portal's fake-signing-page route.
        let page = root.join("signing").join(&envelope_id).join(format!(
            "{}.html",
            crate::signature::recipient_slug(&client.email)
        ));
        let html = tokio::fs::read_to_string(&page)
            .await
            .expect("the minted sign URL is backed by a page on disk");
        assert!(html.contains(&envelope_id), "the page names its envelope");
        let mut served_state = app_state(test_support::mem().await).await;
        served_state.fake_signature_dir = Some(root.clone());
        let served = fake_signing_page(
            State(served_state),
            AxumPath((envelope_id, client.email.clone())),
        )
        .await;
        assert_eq!(served.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn an_emailed_envelope_has_no_session_to_open() {
        let (state, _root) = fake_app_state().await;
        // An *emailed* notation: the recipient resolver derives no
        // `clientUserId` for it — DocuSign mailed the signer their own
        // link, so there is no embedded session to mint.
        let (client, project_id, notation_id) =
            seed_matter_with_delivery(&state.surreal, store::notations::DELIVERY_EMAILED).await;
        store::projects::add_participation(&state.surreal, project_id, client.id, "client")
            .await
            .expect("bind the client to the matter");
        // The envelope's recipient carries no `clientUserId`, matching
        // the emailed notation above.
        let manifest = SignatureManifest {
            recipients: vec![SignatureRecipient {
                role: "client".into(),
                email: client.email.clone(),
                name: client.name.clone(),
                routing_order: 1,
                client_user_id: None,
            }],
            fields: Vec::new(),
        };
        let request_id = state
            .signature_provider
            .send_for_signature(notation_id, b"%PDF-1.7 fake retainer", &manifest, SendMode::Send)
            .await
            .expect("the fake accepts an emailed manifest");
        store::signatures::record_request(
            &state.surreal,
            notation_id,
            store::signatures::SignatureProvider::DocuSign,
            &request_id.0,
        )
        .await
        .expect("record the envelope request");
        let admin = admin_state(state);
        let response = sign_get(
            State(admin),
            Extension(session_for(client.id, store::persons::Role::Client)),
            HeaderMap::new(),
            AxumPath(notation_id),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn a_lawyer_opening_the_sign_page_gets_the_firms_return_url() {
        let (admin, _client, _project_id, notation_id, _root) = seed_signing_matter().await;
        // A lawyer-tier session passes the participation gate on the firm
        // lens and skips the identity gate: the ceremony is the client's,
        // but the firm's step page is where the provider returns them.
        let response = sign_get(
            State(admin),
            Extension(session_for(Uuid::now_v7(), store::persons::Role::Admin)),
            HeaderMap::new(),
            AxumPath(notation_id),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
    }

    /// A provider whose view URL points at a non-loopback http host — the
    /// redirect builder refuses it, and both routes answer `502` rather
    /// than send a browser to a URL they do not trust.
    async fn evil_base_app_state() -> (crate::AppState, std::path::PathBuf) {
        let mut state = app_state(test_support::mem().await).await;
        let root =
            std::env::temp_dir().join(format!("navigator-esign-view-{}", Uuid::now_v7().simple()));
        state.signature_provider = Arc::new(
            crate::signature::FakeSignatureProvider::new_in(root.clone())
                .with_signing_base_url("http://evil.example".into()),
        );
        state.fake_signature_dir = Some(root.clone());
        (state, root)
    }

    #[tokio::test]
    async fn a_refused_signing_url_answers_bad_gateway() {
        let (state, _root) = evil_base_app_state().await;
        let (client, project_id, notation_id) = seed_matter(&state.surreal).await;
        store::projects::add_participation(&state.surreal, project_id, client.id, "client")
            .await
            .expect("bind the client to the matter");
        let request_id = seed_envelope(
            &state.surreal,
            &state.signature_provider,
            &client,
            notation_id,
        )
        .await;
        let _ = request_id;
        let admin = admin_state(state);
        let response = sign_get(
            State(admin),
            Extension(session_for(client.id, store::persons::Role::Client)),
            HeaderMap::new(),
            AxumPath(notation_id),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn a_refused_sender_url_answers_bad_gateway() {
        let (state, _root) = evil_base_app_state().await;
        let (client, project_id, notation_id) = seed_matter(&state.surreal).await;
        store::projects::add_participation(&state.surreal, project_id, client.id, "client")
            .await
            .expect("bind the client to the matter");
        let request_id = seed_envelope(
            &state.surreal,
            &state.signature_provider,
            &client,
            notation_id,
        )
        .await;
        let _ = request_id;
        let admin = admin_state(state);
        let response = sender_view_get(
            State(admin),
            Extension(session_for(client.id, store::persons::Role::Owner)),
            HeaderMap::new(),
            AxumPath(notation_id),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }
}

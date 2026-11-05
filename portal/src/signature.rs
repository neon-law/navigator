//! Signature-provider seam for the retainer-intake workflow.
//!
//! Step 3 of the workflow sends the rendered retainer out for
//! external signature. Google Workspace eSignature has no public
//! API today, so the workflow targets this small trait rather than
//! a concrete vendor.
//!
//! Which provider runs is a boot decision, not an accident of the
//! environment: [`SignatureProviderKind::from_env`] reads
//! `NAVIGATOR_SIGNATURE_PROVIDER` — `fake` or `docusign`, nothing else,
//! no implicit default. Unset or misspelled refuses to boot (naming the
//! two accepted values), and `fake` refuses to boot in a production
//! environment, so a demo can never pass for the real thing. The
//! [`FakeSignatureProvider`] is what dev and tests use — it closes the
//! whole loop locally: it records every call so tests can assert the
//! step fired with the right inputs, envelopes persist to disk, anchors
//! are validated against the prepared PDF's text, each recipient signs
//! a real local page, and completion is delivered as a
//! DocuSign-Connect-shaped HMAC-signed webhook.
//!
//! Production plugs in a real provider behind the same trait — no
//! changes to the workflow or handler. [`DocuSignSignatureProvider`] is
//! the shipped concrete impl: it POSTs an envelope-create request to
//! the DocuSign eSignature REST API and returns the `envelopeId` as the
//! [`SignatureRequestId`]. The provider's later "completed" callback is
//! received by [`crate::esignature_webhook`], which resolves the
//! envelope id back to a notation and signals `signature_received`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

use async_trait::async_trait;
use base64::Engine;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;

use crate::docusign_auth::DocuSignJwtAuth;

/// The one environment value that selects the signature provider. There
/// is no default: an unset value fails boot, so no "send for signature"
/// button can look real while writing fake envelopes.
pub const SIGNATURE_PROVIDER_ENV: &str = "NAVIGATOR_SIGNATURE_PROVIDER";

/// Why a signature provider could not be selected at boot. Every
/// variant names the fix: the accepted values (and, for `fake` in
/// production, why it can never be the answer there).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SignatureProviderSelectionError {
    #[error(
        "{SIGNATURE_PROVIDER_ENV} is not set — the signature provider must be selected \
         explicitly: set `fake` for the local signature loop or `docusign` for DocuSign \
         eSignature (docs/docusign-esignature.md)"
    )]
    Unset,
    #[error(
        "{SIGNATURE_PROVIDER_ENV} must be `fake` or `docusign`, got `{0}` — there is no \
         implicit default (docs/docusign-esignature.md)"
    )]
    Unknown(String),
    #[error(
        "{SIGNATURE_PROVIDER_ENV}=fake refuses to boot in a production environment — the \
         fake provider creates envelopes with no legal weight; production must select \
         `docusign`"
    )]
    FakeInProduction,
}

/// Which [`SignatureProvider`] implementation this boot runs. Selected
/// once, at boot, from [`SIGNATURE_PROVIDER_ENV`]; there is no default
/// and no fallback — a missing or unknown value is a boot error, and the
/// fake refuses a production environment outright.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureProviderKind {
    /// The local signature loop: envelopes on disk, local sign pages,
    /// Connect-shaped HMAC webhooks. Dev and CI only — never production.
    Fake,
    /// The real DocuSign eSignature REST API.
    DocuSign,
}

impl SignatureProviderKind {
    /// Read [`SIGNATURE_PROVIDER_ENV`] from the process environment.
    ///
    /// # Errors
    ///
    /// Unset or unknown values, and `fake` under a production
    /// [`store::config::DeploymentEnvironment`], all refuse to boot.
    pub fn from_env() -> Result<Self, SignatureProviderSelectionError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Selection over an injectable lookup so tests never touch the
    /// process environment. The production-profile check mirrors
    /// `enforce_deployment_invariants` (which runs earlier in the same
    /// boot): an unset `NAVIGATOR_ENVIRONMENT` means production, so
    /// `fake` there refuses too.
    ///
    /// # Errors
    ///
    /// See [`SignatureProviderSelectionError`] — every refusal names the
    /// accepted values or the production invariant.
    pub fn from_lookup<F: Fn(&str) -> Option<String>>(
        get: F,
    ) -> Result<Self, SignatureProviderSelectionError> {
        // An empty value is as good as unset — a blank line in an env
        // file must not masquerade as a selection.
        let Some(value) = get(SIGNATURE_PROVIDER_ENV).filter(|v| !v.is_empty()) else {
            return Err(SignatureProviderSelectionError::Unset);
        };
        match value.as_str() {
            "fake" => {
                let environment = store::config::DeploymentEnvironment::from_lookup(get)
                    .map_err(|_| SignatureProviderSelectionError::FakeInProduction)?;
                if environment == store::config::DeploymentEnvironment::Production {
                    return Err(SignatureProviderSelectionError::FakeInProduction);
                }
                Ok(Self::Fake)
            }
            "docusign" => Ok(Self::DocuSign),
            other => Err(SignatureProviderSelectionError::Unknown(other.to_string())),
        }
    }
}

/// Opaque identifier returned by the signature provider for a sent
/// document. Used to correlate webhook callbacks later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignatureRequestId(pub String);

/// The executed document set for a completed signature request: the
/// signed PDF (all signatures applied) and the Certificate of
/// Completion — the ESIGN evidentiary record of who signed, when, and
/// from where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedDocuments {
    pub signed_pdf: Vec<u8>,
    pub certificate_pdf: Vec<u8>,
}

/// One captured `send_for_signature` invocation. Tests assert on
/// the contents of [`FakeSignatureProvider::calls`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureCall {
    pub notation_id: Uuid,
    pub pdf_bytes_len: usize,
    /// The manifest the step asked us to place — so tests can assert
    /// the right anchors/recipients threaded through.
    pub manifest: SignatureManifest,
}

/// The kind of signature field an anchor expands into — each maps to a
/// distinct DocuSign tab collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SignatureFieldKind {
    Signature,
    Initials,
    Date,
}

impl SignatureFieldKind {
    /// Every recognized field kind, in placement order.
    pub const ALL: [SignatureFieldKind; 3] = [
        SignatureFieldKind::Signature,
        SignatureFieldKind::Initials,
        SignatureFieldKind::Date,
    ];

    /// DocuSign's tab-collection key for this kind.
    #[must_use]
    pub fn tabs_key(self) -> &'static str {
        match self {
            SignatureFieldKind::Signature => "signHereTabs",
            SignatureFieldKind::Initials => "initialHereTabs",
            SignatureFieldKind::Date => "dateSignedTabs",
        }
    }
}

/// One person who signs the document, with their routing position.
/// Identities are resolved by the caller (the respondent Person, the
/// attorney of record, or `person__<role>` for any other declared
/// signer) — the provider never hardcodes who a role is. `routing_order`
/// is load-bearing: recipients sign in the template's `signers:`
/// declaration order (default client then firm).
///
/// `client_user_id`, when set, makes the recipient **captive** (embedded):
/// DocuSign does NOT email them a signing link; instead the app requests
/// a short-lived signing URL via [`SignatureProvider::create_recipient_view`]
/// and shows it inside Neon Law Navigator. The same id must be replayed on the
/// recipient-view request, so it is the caller's stable handle for this
/// signer (we key it on the notation). `None` keeps the recipient
/// non-captive — DocuSign emails them the usual link (the firm
/// countersignature path).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignatureRecipient {
    pub role: String,
    pub email: String,
    pub name: String,
    pub routing_order: u32,
    #[serde(default)]
    pub client_user_id: Option<String>,
}

/// One typed field anchored into the document text, bound to the role
/// that fills it. `anchor` is the exact string the renderer emitted as
/// invisible text in the PDF (see `signature_render`, added in step 2b).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignatureField {
    pub recipient_role: String,
    pub kind: SignatureFieldKind,
    pub anchor: String,
}

/// The full placement plan for one envelope: who signs (recipients) and
/// what typed fields anchor onto the document (fields). Empty means
/// "no anchored signing plan" — the provider falls back to its single
/// configured recipient with no tabs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignatureManifest {
    pub recipients: Vec<SignatureRecipient>,
    pub fields: Vec<SignatureField>,
}

impl SignatureManifest {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.recipients.is_empty()
    }
}

/// The inputs for an embedded-signing URL request. The
/// `email`/`name`/`client_user_id` triple must exactly match a captive
/// recipient created on the envelope (see [`SignatureRecipient::client_user_id`]) —
/// DocuSign resolves the recipient from those, then issues a one-shot
/// URL that redirects the browser to `return_url` once signing finishes
/// (or is declined / times out).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipientView {
    pub return_url: String,
    pub email: String,
    pub name: String,
    pub client_user_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SignatureError {
    #[error("provider error: {0}")]
    Provider(String),
    /// A missing `nlsig-` anchor, or an unreadable prepared PDF. Refused
    /// before anything is sent or persisted — a document that goes out
    /// without its declared fields is a silently unsigned page.
    #[error("anchor validation failed: {0}")]
    AnchorValidation(String),
}

#[async_trait]
pub trait SignatureProvider: Send + Sync {
    /// Submit the rendered retainer PDF for the given notation, placing
    /// the fields described by `manifest`. Returns a provider-issued id
    /// correlating future events.
    async fn send_for_signature(
        &self,
        notation_id: Uuid,
        pdf: &[u8],
        manifest: &SignatureManifest,
    ) -> Result<SignatureRequestId, SignatureError>;

    /// Request a short-lived, single-use **recipient-view URL** for a captive
    /// recipient of an already-created envelope. DocuSign suppresses the
    /// signing email for a captive recipient, so this URL is the only way in:
    /// Navigator redirects the signer to it and the ceremony happens on the
    /// provider's own site. Only valid for recipients sent with a
    /// `client_user_id`; the URL expires in minutes.
    async fn create_recipient_view(
        &self,
        request_id: &SignatureRequestId,
        view: &RecipientView,
    ) -> Result<String, SignatureError>;

    /// Fetch the executed document set for a completed request — the
    /// signed PDF and the Certificate of Completion. Called on the
    /// completion webhook so the ESIGN record is archived to storage.
    async fn fetch_completed_documents(
        &self,
        request_id: &SignatureRequestId,
    ) -> Result<CompletedDocuments, SignatureError>;
}

/// Lifecycle of one fake envelope. The JSON on disk is the envelope's
/// last true state: every state transition rewrites it, so a test that
/// kills the loop mid-way can assert on exactly what happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnvelopeStatus {
    /// `send_for_signature` accepted it. Anchors validated, PDF stored.
    Sent,
    /// Every recipient signed; completed documents exist on disk.
    Completed,
}

impl std::fmt::Display for EnvelopeStatus {
    /// The wire label the Connect payload and error messages carry —
    /// identical to the serde kebab-case representation.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Sent => "sent",
            Self::Completed => "completed",
        })
    }
}

/// One persisted fake envelope — the durable record of a
/// `send_for_signature` call. Serialized as JSON at
/// `<root>/<envelope id>.json` under the provider's root directory, so
/// the loop's later steps (sign page, webhook, filed documents) read the
/// exact bytes the sender approved rather than anything held in memory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FakeEnvelope {
    pub id: String,
    pub notation_id: Uuid,
    pub status: EnvelopeStatus,
    pub recipients: Vec<SignatureRecipient>,
    pub fields: Vec<SignatureField>,
    /// The prepared PDF, base64-encoded. Anchors were validated against
    /// this text before the envelope was accepted.
    pub pdf_base64: String,
    /// RFC 3339 timestamp of the send.
    pub created_at: String,
    /// Recipients who have signed, in signing order. A recipient appears
    /// at most once — `sign` is idempotent per email.
    #[serde(default)]
    pub signed: Vec<SignedBy>,
    /// Outcome of the last webhook delivery attempt: `None` when no
    /// delivery is configured or none has run yet, otherwise
    /// `delivered` or `failed: <why>`. A failed delivery never rolls the
    /// envelope back — it stays in its last true state.
    #[serde(default)]
    pub webhook_status: Option<String>,
}

/// One recipient's signature on a fake envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignedBy {
    /// The signer's email, as the manifest declared it.
    pub email: String,
    /// RFC 3339 timestamp of the signature.
    pub at: String,
}

impl FakeEnvelope {
    /// The prepared PDF, decoded.
    ///
    /// # Errors
    ///
    /// [`SignatureError::Provider`] when the stored base64 does not
    /// decode — a corrupted store, not a corrupt envelope.
    pub fn pdf_bytes(&self) -> Result<Vec<u8>, SignatureError> {
        Engine::decode(&base64::engine::general_purpose::STANDARD, &self.pdf_base64)
            .map_err(|e| SignatureError::Provider(format!("stored pdf does not decode: {e}")))
    }
}

/// Every declared field's anchor must appear in the prepared PDF's text.
/// The renderer expands placeholders to invisible white `nlsig-…` text
/// (`signature_render::anchor_string`), so an anchor absent from the
/// document's text layer means the document went out without that field
/// — a silently unsigned page. That is a hard error, before anything is
/// sent or persisted.
///
/// The search goes through [`pdf::passage::occurrence_count`], which
/// reads each font's `/ToUnicode` CMap — Typst's subset fonts carry
/// glyph ids, so a naive content-stream extractor sees mojibake where
/// the anchor lives.
///
/// # Errors
///
/// [`SignatureError::AnchorValidation`] when the PDF's text layer
/// cannot be read or any anchor is missing.
pub fn validate_anchors(pdf_bytes: &[u8], fields: &[SignatureField]) -> Result<(), SignatureError> {
    let mut missing: Vec<String> = Vec::new();
    for field in fields {
        match pdf::passage::occurrence_count(pdf_bytes, &field.anchor) {
            // Ok(0) is not reachable today (occurrence_count errors on
            // zero), but treating it as missing keeps the contract total.
            Ok(0) => missing.push(format!("{} (found 0 occurrences)", field.anchor)),
            Ok(_) => {}
            Err(pdf::PassageError::QuoteNotFound { quote }) => {
                missing.push(quote);
            }
            Err(e) => {
                return Err(SignatureError::AnchorValidation(format!(
                    "searching the prepared PDF's text for {}: {e}",
                    field.anchor
                )));
            }
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    Err(SignatureError::AnchorValidation(format!(
        "missing anchors (not present in the prepared PDF's text layer): {}",
        missing.join(", ")
    )))
}

/// The fake provider — what dev, KIND, and tests run. It closes the
/// whole loop locally without any DocuSign account:
///
/// - envelopes persist as durable JSON under its root directory
///   (`FakeSignatureProvider::from_dev_dir` → `~/.navigator/fake-signature`,
///   overridable with `NAVIGATOR_FAKE_SIGNATURE_DIR`),
/// - anchors are validated against the prepared PDF's text before an
///   envelope is accepted ([`validate_anchors`]),
/// - each recipient gets a real local sign page, and completion is
///   delivered as a DocuSign-Connect-shaped HMAC-signed webhook.
///
/// Tests construct it directly — `new()` for an isolated throwaway root,
/// `new_in` to name one — never through the environment.
pub struct FakeSignatureProvider {
    calls: Mutex<Vec<SignatureCall>>,
    recipient_views: Mutex<Vec<RecipientView>>,
    root: PathBuf,
    /// Where completion webhooks go, when configured. Tests and the local
    /// loop point this at the portal's own esignature webhook route; the
    /// delivery is HMAC-signed with the same scheme
    /// [`crate::esignature_webhook`] verifies fail-closed.
    webhook: Option<WebhookTarget>,
    /// The base URL the dev server serves this fake's sign pages from
    /// (`http://127.0.0.1:{port}/fake-signature`, wired by `hosting`).
    /// Set, `create_recipient_view` hands back a URL a browser can open;
    /// unset — direct construction in tests — it hands back the page's
    /// `file://` path.
    signing_base_url: Option<String>,
    /// Failures this fake produces on purpose, before any incidental
    /// network error could. Default: none armed.
    faults: Faults,
}

/// Where and how the fake delivers DocuSign-Connect-shaped webhooks.
#[derive(Debug, Clone)]
pub struct WebhookTarget {
    /// Full URL of the receiver, path secret included.
    pub url: String,
    /// HMAC-SHA256 key; the receiver's `esignature_hmac_key`.
    pub hmac_key: Vec<u8>,
}

/// A failure the fake is told to produce on purpose, so tests assert the
/// recovery paths, not just the sunny one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// `send_for_signature` refuses before anything is validated, written,
    /// or recorded — nothing half-sent exists to resume from.
    Send,
    /// `create_recipient_view` refuses; the envelope on disk is untouched.
    View,
    /// Completion webhook delivery fails after the durable write: the
    /// envelope is `completed` on disk with the delivery failure recorded,
    /// and the loop recovers by reading completion from the provider.
    Webhook,
}

/// The set of [`Fault`]s a fake carries. Default: none armed.
#[derive(Debug, Clone, Copy, Default)]
pub struct Faults {
    send: bool,
    view: bool,
    webhook: bool,
}

impl Faults {
    /// Arm exactly the faults in `faults`.
    #[must_use]
    pub fn inject(faults: &[Fault]) -> Self {
        let mut armed = Self::default();
        for fault in faults {
            match fault {
                Fault::Send => armed.send = true,
                Fault::View => armed.view = true,
                Fault::Webhook => armed.webhook = true,
            }
        }
        armed
    }

    fn armed(self, fault: Fault) -> bool {
        match fault {
            Fault::Send => self.send,
            Fault::View => self.view,
            Fault::Webhook => self.webhook,
        }
    }
}

/// A path- and URL-safe slug for a recipient's sign page: every
/// non-alphanumeric byte collapses to `-`, so the page's file name can
/// never traverse out of the fake's directory and the URL the signer
/// opens maps one-to-one onto the file. Idempotent, so the dev sign-page
/// route can slug an incoming segment again and land on the same file.
pub(crate) fn recipient_slug(email: &str) -> String {
    email
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

impl FakeSignatureProvider {
    /// A fake whose envelopes persist under a fresh throwaway directory.
    /// For tests that do not care where envelopes land.
    #[must_use]
    pub fn new() -> Self {
        Self::new_in(
            std::env::temp_dir().join(format!("navigator-fake-signature-{}", Uuid::new_v4())),
        )
    }

    /// A fake persisting under `root` — the explicit constructor.
    #[must_use]
    pub fn new_in(root: impl Into<PathBuf>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            recipient_views: Mutex::new(Vec::new()),
            root: root.into(),
            webhook: None,
            signing_base_url: None,
            faults: Faults::default(),
        }
    }

    /// Deliver DocuSign-Connect-shaped webhooks to `url`, HMAC-signed
    /// with `hmac_key`. Without this the fake completes envelopes on
    /// disk but notifies nobody — the loop then reads completion
    /// directly from the provider.
    #[must_use]
    pub fn with_webhook(mut self, target: WebhookTarget) -> Self {
        self.webhook = Some(target);
        self
    }

    /// Serve this fake's sign pages from `base` — the dev server's own
    /// loopback route (`hosting` wires this), so the URL the signer is
    /// redirected to is one a browser can actually open.
    #[must_use]
    pub fn with_signing_base_url(mut self, base: String) -> Self {
        self.signing_base_url = Some(base);
        self
    }

    /// Arm [`Fault`]s: failures this fake produces on purpose. An armed
    /// fault fires before any incidental network error could, so tests
    /// assert the recovery paths deterministically.
    #[must_use]
    pub fn with_faults(mut self, faults: Faults) -> Self {
        self.faults = faults;
        self
    }

    /// The fake a dev boot runs: `NAVIGATOR_FAKE_SIGNATURE_DIR` when set,
    /// else `$HOME/.navigator/fake-signature`. No silent fallback — a
    /// boot that cannot name a home directory has no place to persist
    /// envelopes and says so.
    ///
    /// # Errors
    ///
    /// When neither `NAVIGATOR_FAKE_SIGNATURE_DIR` nor `$HOME` is set.
    pub fn from_dev_dir() -> Result<Self, std::env::VarError> {
        if let Ok(dir) = std::env::var("NAVIGATOR_FAKE_SIGNATURE_DIR") {
            if !dir.is_empty() {
                return Ok(Self::new_in(dir));
            }
        }
        let home = std::env::var("HOME")?;
        Ok(Self::new_in(
            Path::new(&home).join(".navigator").join("fake-signature"),
        ))
    }

    /// Directory envelopes persist under, created on first send.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Snapshot of every call so far. Cheap clone — callers can
    /// hold onto it across `await` points.
    #[must_use]
    pub fn calls(&self) -> Vec<SignatureCall> {
        self.calls.lock().expect("fake provider lock").clone()
    }

    /// Snapshot of every [`RecipientView`] requested so far.
    ///
    /// The synthetic signing URL the fake returns is built from the request
    /// id and the `clientUserId` alone, so without this the `return_url` and
    /// the `email`/`userName` legs would leave no trace and a test could not
    /// tell an absolute return URL from a relative one — which is exactly how
    /// a relative one reached the wire unnoticed (ENG-557).
    #[must_use]
    pub fn recipient_views(&self) -> Vec<RecipientView> {
        self.recipient_views
            .lock()
            .expect("fake provider lock")
            .clone()
    }

    /// The persisted envelope for `request_id`, read back from disk.
    ///
    /// # Errors
    ///
    /// [`SignatureError::Provider`] when the envelope file exists but
    /// does not parse — a corrupted store is surfaced, never swallowed.
    pub fn envelope(
        &self,
        request_id: &SignatureRequestId,
    ) -> Result<Option<FakeEnvelope>, SignatureError> {
        let path = self.envelope_path(&request_id.0);
        let Ok(json) = std::fs::read(&path) else {
            return Ok(None);
        };
        serde_json::from_slice(&json).map(Some).map_err(|e| {
            SignatureError::Provider(format!("envelope {} does not parse: {e}", path.display()))
        })
    }

    /// Every persisted envelope, newest last. Envelopes whose files do
    /// not parse are surfaced, not skipped.
    ///
    /// # Errors
    ///
    /// [`SignatureError::Provider`] when the root cannot be read or any
    /// envelope file does not parse.
    pub fn envelopes(&self) -> Result<Vec<FakeEnvelope>, SignatureError> {
        let mut envelopes = Vec::new();
        // A store that has never been written is empty, not broken — the
        // same honesty `envelope`'s `Ok(None)` has for a missing file.
        if !self.root.exists() {
            return Ok(envelopes);
        }
        let entries = std::fs::read_dir(&self.root).map_err(|e| {
            SignatureError::Provider(format!("reading {}: {e}", self.root.display()))
        })?;
        for entry in entries {
            let path = entry
                .map_err(|e| {
                    SignatureError::Provider(format!("reading {}: {e}", self.root.display()))
                })?
                .path();
            if path.extension().is_some_and(|ext| ext == "json") {
                let json = std::fs::read(&path).map_err(|e| {
                    SignatureError::Provider(format!("reading {}: {e}", path.display()))
                })?;
                let envelope: FakeEnvelope = serde_json::from_slice(&json).map_err(|e| {
                    SignatureError::Provider(format!(
                        "envelope {} does not parse: {e}",
                        path.display()
                    ))
                })?;
                envelopes.push(envelope);
            }
        }
        envelopes.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        Ok(envelopes)
    }

    /// Durable write: the JSON on disk is the envelope's last true state.
    fn persist(&self, envelope: &FakeEnvelope) -> Result<(), SignatureError> {
        std::fs::create_dir_all(&self.root).map_err(|e| {
            SignatureError::Provider(format!("creating {}: {e}", self.root.display()))
        })?;
        let json = serde_json::to_vec_pretty(envelope)
            .map_err(|e| SignatureError::Provider(format!("serializing envelope: {e}")))?;
        let path = self.envelope_path(&envelope.id);
        std::fs::write(&path, json)
            .map_err(|e| SignatureError::Provider(format!("writing {}: {e}", path.display())))
    }

    fn envelope_path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.json"))
    }

    /// Record `email`'s signature on the envelope. Idempotent per email.
    /// The last signature completes the envelope: status flips to
    /// `Completed` on disk, and — when a webhook target is configured —
    /// a DocuSign-Connect-shaped, HMAC-signed callback is POSTed,
    /// exactly the shape [`crate::esignature_webhook`] verifies
    /// fail-closed. Intermediate signatures emit a `recipient-completed`
    /// event naming the signer's routing order, like the real provider.
    ///
    /// The durable envelope never rolls back: a failed webhook delivery
    /// is recorded on the envelope (`webhook_status`) and logged, and
    /// `sign` still reports the envelope's true status.
    ///
    /// # Errors
    ///
    /// [`SignatureError::Provider`] when the envelope does not exist,
    /// does not parse, or names no recipient with that email.
    pub async fn sign(
        &self,
        request_id: &SignatureRequestId,
        email: &str,
    ) -> Result<EnvelopeStatus, SignatureError> {
        let mut envelope = self.envelope(request_id)?.ok_or_else(|| {
            SignatureError::Provider(format!("no such envelope {}", request_id.0))
        })?;
        let already = envelope.signed.iter().any(|s| s.email == email);
        if !already {
            let signer = envelope
                .recipients
                .iter()
                .find(|r| r.email == email)
                .ok_or_else(|| {
                    SignatureError::Provider(format!("{} names no recipient {email}", request_id.0))
                })?;
            envelope.signed.push(SignedBy {
                email: signer.email.clone(),
                at: chrono::Utc::now().to_rfc3339(),
            });
            if envelope.signed.len() == envelope.recipients.len() {
                envelope.status = EnvelopeStatus::Completed;
            }
            self.persist(&envelope)?;
        }

        // Event delivery mirrors DocuSign Connect: one
        // `recipient-completed` per signature, `envelope-completed` when
        // the last signer finishes. Both are HMAC-signed over the exact
        // bytes POSTed.
        if !already {
            let recipient_id = envelope
                .recipients
                .iter()
                .find(|r| r.email == email)
                .map(|r| r.routing_order.to_string());
            self.deliver_webhook(&envelope, "recipient-completed", recipient_id.as_deref())
                .await;
        }
        if envelope.status == EnvelopeStatus::Completed {
            self.deliver_webhook(&envelope, "envelope-completed", None)
                .await;
        }
        Ok(envelope.status)
    }

    /// Build, sign, and POST one Connect-shaped callback; record the
    /// outcome on the envelope. A failed or unconfigured delivery is
    /// never fatal — the envelope's on-disk state is already true.
    async fn deliver_webhook(
        &self,
        envelope: &FakeEnvelope,
        event: &str,
        recipient_id: Option<&str>,
    ) {
        let Some(target) = &self.webhook else {
            return;
        };
        let body = serde_json::json!({
            "event": event,
            "data": {
                "envelopeId": envelope.id,
                "recipientId": recipient_id,
                "envelopeSummary": { "status": format!("{}", envelope.status) },
            },
        });
        let body = body.to_string().into_bytes();
        let status = if self.faults.armed(Fault::Webhook) {
            // Injected after the durable write: the envelope on disk is
            // already true (completed), the delivery fails on purpose,
            // and the failure is recorded — the loop recovers by reading
            // completion from the provider.
            "failed: injected webhook fault".to_string()
        } else {
            let signature = crate::webhook_auth::sign_hmac_sha256_b64(&target.hmac_key, &body);
            let outcome = reqwest::Client::new()
                .post(&target.url)
                .header("x-docusign-signature-1", signature)
                .header("content-type", "application/json")
                .body(body)
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await;
            match outcome {
                Ok(response) if response.status().is_success() => "delivered".to_string(),
                Ok(response) => format!("failed: receiver answered {}", response.status()),
                Err(e) => format!("failed: {e}"),
            }
        };
        if !status.starts_with("delivered") {
            tracing::warn!(envelope_id = %envelope.id, event, %status, "fake signature webhook delivery failed");
        }
        // Record the outcome on a fresh read: the envelope on disk may
        // have changed since `sign` persisted it (it has not — sign is
        // the only writer here — but the re-read keeps the record honest).
        if let Some(mut latest) = self
            .envelope(&SignatureRequestId(envelope.id.clone()))
            .expect("reading back a just-persisted envelope")
        {
            latest.webhook_status = Some(status.clone());
            if let Err(e) = self.persist(&latest) {
                tracing::warn!(envelope_id = %envelope.id, error = %e, "could not record the webhook outcome on the envelope");
            }
        }
    }

    /// The local sign page for one recipient, as `create_recipient_view`
    /// hands it out: a real file at a real path, so the URL the signer
    /// opens exists.
    fn signing_page_path(&self, envelope_id: &str, email: &str) -> PathBuf {
        self.root
            .join("signing")
            .join(envelope_id)
            .join(format!("{}.html", recipient_slug(email)))
    }

    /// Write the per-recipient sign page and a copy of the document it
    /// references. The page is plain HTML naming the envelope, the
    /// signer, and every field they sign; the document link points at
    /// the PDF stored beside it.
    fn write_signing_page(
        &self,
        envelope: &FakeEnvelope,
        email: &str,
    ) -> Result<PathBuf, SignatureError> {
        let path = self.signing_page_path(&envelope.id, email);
        std::fs::create_dir_all(path.parent().expect("signing page has a parent")).map_err(
            |e| {
                SignatureError::Provider(format!(
                    "creating {}: {e}",
                    path.parent().expect("signing page has a parent").display()
                ))
            },
        )?;
        let signer = envelope
            .recipients
            .iter()
            .find(|r| r.email == email)
            .ok_or_else(|| SignatureError::Provider(format!("no recipient {email}")))?;
        let mut field_rows = String::new();
        for field in &envelope.fields {
            if field.recipient_role == signer.role {
                // Infallible: writing to a String never errors.
                let _ = write!(
                    field_rows,
                    "<tr><td><code>{}</code></td><td>{:?}</td></tr>",
                    field.anchor, field.kind
                );
            }
        }
        let page = format!(
            "<!DOCTYPE html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
<title>Sign here — {}</title>\
<style>body{{font-family:system-ui,sans-serif;max-width:40rem;margin:3rem auto;padding:0 1rem;color:#1a1a2e}}\
table{{border-collapse:collapse;width:100%}}td,th{{border:1px solid #ddd;padding:.4rem;text-align:left}}\
code{{background:#f4f4f6;padding:.1rem .3rem}}</style></head><body>\
<h1>Sign here</h1>\
<p><strong>{}</strong> ({}), reviewing envelope <code>{}</code>.</p>\
<p>The prepared document is available as <a href=\"document.pdf\">document.pdf</a>.</p>\
<h2>Fields you sign</h2>\
<table><tr><th>Anchor</th><th>Kind</th></tr>{field_rows}</table>\
<p>This is Navigator's local signature page: signing is driven by the \
local loop (<code>navigator dev</code> tooling or the running workflow), \
which records your signature with the fake signature provider.</p>\
</body></html>\n",
            signer.name, signer.name, signer.email, envelope.id,
        );
        std::fs::write(&path, page)
            .map_err(|e| SignatureError::Provider(format!("writing {}: {e}", path.display())))?;
        // The document the page links to, stored beside it.
        let document = path
            .parent()
            .expect("signing page has a parent")
            .join("document.pdf");
        if !document.exists() {
            let pdf = envelope.pdf_bytes()?;
            std::fs::write(&document, pdf).map_err(|e| {
                SignatureError::Provider(format!("writing {}: {e}", document.display()))
            })?;
        }
        Ok(path)
    }

    /// Render the certificate of completion that accompanies the filed
    /// PDF: who signed, when, on which envelope. Every caller-supplied
    /// value is escaped into a Typst string literal, never markup.
    ///
    /// # Errors
    ///
    /// [`SignatureError::Provider`] when the Typst engine fails — an
    /// internal regression, not an envelope problem.
    fn certificate_of_completion(envelope: &FakeEnvelope) -> Result<Vec<u8>, SignatureError> {
        use pdf::escape_typst_markup as esc;
        // Signer rows are pre-built in Rust with every value passed
        // through the markup escape — this Typst build has no
        // `json.decode`, and dynamic list markup in-template leaves a
        // parse trap (`\` continuing into the closing bracket).
        let mut signer_rows = String::new();
        for signed in &envelope.signed {
            let signer = envelope
                .recipients
                .iter()
                .find(|r| r.email == signed.email)
                .map_or_else(|| signed.email.clone(), |r| r.name.clone());
            // Infallible: writing to a String never errors.
            let _ = writeln!(
                signer_rows,
                "- {} — signed {} (_{}_) \\",
                esc(&signer),
                esc(&signed.at),
                esc(&signed.email),
            );
        }
        let source = format!(
            "#set page(width: 8.5in, height: 11in, margin: 1in)\n\
#set text(font: \"Noto Serif\", size: 11pt)\n\
= Certificate of completion\n\n\
Envelope *{}* completed on {}.\n\n\
Rendered by Navigator's fake signature provider: every signature was \
recorded locally, with no DocuSign account involved.\n\n\
== Signers\n\n{signer_rows}\n\
== Document\n\n\
Notation {}.\n\n\
The filed document is the exact PDF the sender approved; its declared \
signature anchors were validated against the document's text layer \
before the envelope was accepted.\n",
            esc(&envelope.id),
            esc(&envelope.created_at),
            esc(&envelope.notation_id.to_string()),
        );
        pdf::render(&source)
            .map_err(|e| SignatureError::Provider(format!("rendering the certificate: {e}")))
    }
}

impl Default for FakeSignatureProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SignatureProvider for FakeSignatureProvider {
    async fn send_for_signature(
        &self,
        notation_id: Uuid,
        pdf: &[u8],
        manifest: &SignatureManifest,
    ) -> Result<SignatureRequestId, SignatureError> {
        // Nothing is sent unsigned: validate every anchor against the
        // prepared PDF's text first, and refuse the whole envelope on a
        // miss — before anything is persisted or recorded.
        if self.faults.armed(Fault::Send) {
            // The injected failure fires before validation, before any
            // write, before any call record: the disk's state — no
            // envelope — stays true, and re-running the loop resumes
            // rather than duplicates.
            return Err(SignatureError::Provider(
                "injected send failure".to_string(),
            ));
        }
        validate_anchors(pdf, &manifest.fields)?;

        let mut calls = self.calls.lock().expect("fake provider lock");
        let seq = calls.len() + 1;
        let id = format!("fake-{notation_id}-{seq}");
        let envelope = FakeEnvelope {
            id: id.clone(),
            notation_id,
            status: EnvelopeStatus::Sent,
            recipients: manifest.recipients.clone(),
            fields: manifest.fields.clone(),
            pdf_base64: Engine::encode(&base64::engine::general_purpose::STANDARD, pdf),
            created_at: chrono::Utc::now().to_rfc3339(),
            signed: Vec::new(),
            webhook_status: None,
        };
        self.persist(&envelope)?;
        // Every recipient's sign page exists the moment the envelope
        // does — the signer opens a URL that resolves, not a stub string.
        for recipient in &envelope.recipients {
            self.write_signing_page(&envelope, &recipient.email)?;
        }
        calls.push(SignatureCall {
            notation_id,
            pdf_bytes_len: pdf.len(),
            manifest: manifest.clone(),
        });
        Ok(SignatureRequestId(id))
    }

    async fn create_recipient_view(
        &self,
        request_id: &SignatureRequestId,
        view: &RecipientView,
    ) -> Result<String, SignatureError> {
        if self.faults.armed(Fault::View) {
            // Injected before the view is recorded or any page is
            // rewritten: the durable envelope keeps its last true state.
            return Err(SignatureError::Provider(
                "injected view failure".to_string(),
            ));
        }
        self.recipient_views
            .lock()
            .expect("fake provider lock")
            .push(view.clone());
        // A real local sign page for this recipient — the URL the
        // signer opens exists on disk.
        let page = self.signing_page_path(&request_id.0, &view.email);
        if !page.exists() {
            let envelope = self.envelope(request_id)?.ok_or_else(|| {
                SignatureError::Provider(format!("no such envelope {}", request_id.0))
            })?;
            self.write_signing_page(&envelope, &view.email)?;
        }
        match &self.signing_base_url {
            // When the dev server serves the fake's pages, the redirect
            // target is a URL on this server — a browser can open it.
            // Direct construction (tests, the loop harness) hands back
            // the on-disk page path instead: the caller opens it directly.
            Some(base) => Ok(format!(
                "{base}/{}/{}",
                request_id.0,
                recipient_slug(&view.email)
            )),
            None => Ok(format!("file://{}", page.display())),
        }
    }

    async fn fetch_completed_documents(
        &self,
        request_id: &SignatureRequestId,
    ) -> Result<CompletedDocuments, SignatureError> {
        // The real documents, read back from the durable envelope: the
        // signed PDF is exactly the bytes the sender approved, and the
        // certificate of completion accompanies it. Only a completed
        // envelope hands out documents — a pending one has none.
        let envelope = self.envelope(request_id)?.ok_or_else(|| {
            SignatureError::Provider(format!("no such envelope {}", request_id.0))
        })?;
        if envelope.status != EnvelopeStatus::Completed {
            return Err(SignatureError::Provider(format!(
                "envelope {} is {:?} — no completed documents until every recipient signs",
                request_id.0, envelope.status
            )));
        }
        let certificate = Self::certificate_of_completion(&envelope)?;
        Ok(CompletedDocuments {
            signed_pdf: envelope.pdf_bytes()?,
            certificate_pdf: certificate,
        })
    }
}

/// Build DocuSign's `tabs` object for one recipient `role` from the
/// manifest's fields, grouping each anchored field under its DocuSign
/// tab-collection key. Returns `None` when the role anchors no fields,
/// so the signer JSON simply omits `tabs`.
fn tabs_for_role(role: &str, fields: &[SignatureField]) -> Option<serde_json::Value> {
    let mut tabs = serde_json::Map::new();
    for kind in SignatureFieldKind::ALL {
        let placed: Vec<serde_json::Value> = fields
            .iter()
            .filter(|f| f.recipient_role == role && f.kind == kind)
            .map(|f| {
                serde_json::json!({
                    "anchorString": f.anchor,
                    "anchorUnits": "pixels",
                    "anchorYOffset": "-6",
                    "anchorIgnoreIfNotPresent": "false",
                })
            })
            .collect();
        if !placed.is_empty() {
            tabs.insert(
                kind.tabs_key().to_string(),
                serde_json::Value::Array(placed),
            );
        }
    }
    (!tabs.is_empty()).then_some(serde_json::Value::Object(tabs))
}

/// Re-mint a JWT-grant token this many seconds before DocuSign's stated
/// expiry, so an in-flight request never carries a token that lapses
/// mid-call.
const TOKEN_REFRESH_LEAD_SECS: u64 = 300;

/// Current unix time in seconds. The JWT assertion builder takes `now`
/// as a parameter for determinism; the provider's runtime token cache
/// reads the real clock here.
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A minted JWT-grant token and the unix second at/after which it must
/// be re-minted (DocuSign's stated expiry, less [`TOKEN_REFRESH_LEAD_SECS`]).
#[derive(Clone)]
struct CachedToken {
    token: String,
    refresh_at: u64,
}

impl CachedToken {
    /// The token if it is still fresh at `now` (strictly before
    /// `refresh_at`); `None` means the caller must re-mint.
    fn fresh_at(&self, now: u64) -> Option<&str> {
        (now < self.refresh_at).then_some(self.token.as_str())
    }
}

/// How [`DocuSignSignatureProvider`] obtains the OAuth bearer for each
/// REST call. `Static` is the simple local/demo path (a `DOCUSIGN_ACCESS_TOKEN`
/// that DocuSign expires in ~8h); `Jwt` is the durable production path
/// that mints + caches short-lived tokens via [`DocuSignJwtAuth`],
/// re-minting before expiry. The cache is shared across clones of the
/// provider so a cloned handle reuses a token already minted elsewhere.
#[derive(Clone)]
enum TokenSource {
    Static(String),
    Jwt {
        auth: Arc<DocuSignJwtAuth>,
        cached: Arc<AsyncMutex<Option<CachedToken>>>,
    },
}

/// Production [`SignatureProvider`] backed by the DocuSign eSignature
/// REST API. Creates one envelope per notation (the rendered retainer
/// PDF, sent to the client for signature) and returns DocuSign's
/// `envelopeId` as the [`SignatureRequestId`].
///
/// Config is `.env`-driven so OSS forks plug in their own DocuSign
/// account (or swap the whole impl): `base_url` is the account's
/// eSignature base path (e.g. `https://demo.docusign.net/restapi` for
/// the developer sandbox), `account_id` the API account GUID, and
/// `access_token` an OAuth bearer token. We hit the REST surface
/// directly with `reqwest` rather than pulling a DocuSign SDK — the
/// surface we need is one POST, mirroring [`workflows::email`]'s
/// SendGrid backend.
///
/// `signer_email` / `signer_name` address the envelope. v1 sends to a
/// single configured recipient (the firm's own signing inbox in the
/// reference deploy); per-client routing belongs to a later change.
#[derive(Clone)]
pub struct DocuSignSignatureProvider {
    http: reqwest::Client,
    base_url: String,
    account_id: String,
    token: TokenSource,
    signer_email: String,
    signer_name: String,
}

impl DocuSignSignatureProvider {
    /// Build a provider that authenticates with a static OAuth bearer
    /// (`DOCUSIGN_ACCESS_TOKEN`). The simple local/demo path; the token
    /// expires in ~8h and is not refreshed.
    #[must_use]
    pub fn new(
        base_url: impl Into<String>,
        account_id: impl Into<String>,
        access_token: impl Into<String>,
        signer_email: impl Into<String>,
        signer_name: impl Into<String>,
    ) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.into(),
            account_id: account_id.into(),
            token: TokenSource::Static(access_token.into()),
            signer_email: signer_email.into(),
            signer_name: signer_name.into(),
        }
    }

    /// Build a provider that authenticates via JWT grant — minting and
    /// caching short-lived tokens from `auth`, re-minting before expiry.
    /// The durable production token path.
    #[must_use]
    pub fn with_jwt_auth(
        base_url: impl Into<String>,
        account_id: impl Into<String>,
        auth: DocuSignJwtAuth,
        signer_email: impl Into<String>,
        signer_name: impl Into<String>,
    ) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.into(),
            account_id: account_id.into(),
            token: TokenSource::Jwt {
                auth: Arc::new(auth),
                cached: Arc::new(AsyncMutex::new(None)),
            },
            signer_email: signer_email.into(),
            signer_name: signer_name.into(),
        }
    }

    /// Build from the environment, returning `None` when the required
    /// vars are absent (dev / KIND, where the stub is used instead).
    ///
    /// Required: `DOCUSIGN_BASE_URL`, `DOCUSIGN_ACCOUNT_ID`. Optional:
    /// `DOCUSIGN_SIGNER_EMAIL` / `DOCUSIGN_SIGNER_NAME` (the envelope
    /// recipient), defaulting to the firm's support inbox.
    ///
    /// Prefers JWT grant when [`DocuSignJwtAuth::from_env`] is satisfied
    /// (integration key + impersonated user + RSA key present) — the
    /// self-refreshing production path — and otherwise falls back to a
    /// static `DOCUSIGN_ACCESS_TOKEN` for a quick local/demo smoke test.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let get = |k: &str| std::env::var(k).ok().filter(|s| !s.is_empty());
        let base_url = get("DOCUSIGN_BASE_URL")?;
        let account_id = get("DOCUSIGN_ACCOUNT_ID")?;
        let signer_email =
            get("DOCUSIGN_SIGNER_EMAIL").unwrap_or_else(|| "support@neonlaw.com".to_string());
        let signer_name = get("DOCUSIGN_SIGNER_NAME").unwrap_or_else(|| "Neon Law".to_string());
        if let Some(auth) = DocuSignJwtAuth::from_env() {
            Some(Self::with_jwt_auth(
                base_url,
                account_id,
                auth,
                signer_email,
                signer_name,
            ))
        } else {
            Some(Self::new(
                base_url,
                account_id,
                get("DOCUSIGN_ACCESS_TOKEN")?,
                signer_email,
                signer_name,
            ))
        }
    }

    /// The OAuth bearer for the next REST call. `Static` sources return
    /// the configured token; `Jwt` sources return a cached token,
    /// minting a fresh one when none is cached or the current one is
    /// within [`TOKEN_REFRESH_LEAD_SECS`] of expiry.
    async fn bearer_token(&self) -> Result<String, SignatureError> {
        match &self.token {
            TokenSource::Static(t) => Ok(t.clone()),
            TokenSource::Jwt { auth, cached } => {
                let now = now_unix();
                let mut guard = cached.lock().await;
                if let Some(token) = guard.as_ref().and_then(|c| c.fresh_at(now)) {
                    return Ok(token.to_string());
                }
                let minted = auth.mint(now).await?;
                *guard = Some(CachedToken {
                    token: minted.access_token.clone(),
                    refresh_at: now + minted.expires_in.saturating_sub(TOKEN_REFRESH_LEAD_SECS),
                });
                Ok(minted.access_token)
            }
        }
    }

    /// Build the DocuSign envelope-create JSON body. Pure — exposed for
    /// unit-testing the request shape without an HTTP round-trip.
    ///
    /// When `manifest` carries recipients, each becomes a signer with
    /// its anchored `tabs` and routing order. When it's empty, we fall
    /// back to the single configured recipient with no tabs (the
    /// pre-anchor behavior) — a deliberate transitional state until the
    /// caller supplies a real manifest.
    #[must_use]
    pub fn build_envelope_body(
        &self,
        notation_id: Uuid,
        pdf: &[u8],
        manifest: &SignatureManifest,
    ) -> serde_json::Value {
        let document_b64 = base64::engine::general_purpose::STANDARD.encode(pdf);
        let signers: Vec<serde_json::Value> = if manifest.is_empty() {
            vec![serde_json::json!({
                "email": self.signer_email,
                "name": self.signer_name,
                "recipientId": "1",
                "routingOrder": "1",
            })]
        } else {
            manifest
                .recipients
                .iter()
                .map(|r| {
                    let mut signer = serde_json::json!({
                        "email": r.email,
                        "name": r.name,
                        "recipientId": r.routing_order.to_string(),
                        "routingOrder": r.routing_order.to_string(),
                    });
                    // A captive recipient carries `clientUserId`: DocuSign
                    // then suppresses the email and the signer must be
                    // driven through an embedded recipient view.
                    if let Some(client_user_id) = &r.client_user_id {
                        signer["clientUserId"] = serde_json::Value::String(client_user_id.clone());
                    }
                    if let Some(tabs) = tabs_for_role(&r.role, &manifest.fields) {
                        signer["tabs"] = tabs;
                    }
                    signer
                })
                .collect()
        };
        serde_json::json!({
            "emailSubject": format!("Document for signature ({notation_id})"),
            "status": "sent",
            "documents": [{
                "documentBase64": document_b64,
                "name": "Document",
                "fileExtension": "pdf",
                "documentId": "1",
            }],
            "recipients": { "signers": signers },
        })
    }

    /// GET one envelope document as raw bytes. `which` is the DocuSign
    /// document selector — `combined` (all signed docs merged) or
    /// `certificate` (the Certificate of Completion).
    async fn get_envelope_document(
        &self,
        envelope_id: &str,
        which: &str,
    ) -> Result<Vec<u8>, SignatureError> {
        let url = format!(
            "{}/v2.1/accounts/{}/envelopes/{}/documents/{}",
            self.base_url.trim_end_matches('/'),
            self.account_id,
            envelope_id,
            which
        );
        let resp = self
            .http
            .get(&url)
            .bearer_auth(self.bearer_token().await?)
            .send()
            .await
            .map_err(|e| SignatureError::Provider(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(SignatureError::Provider(format!(
                "docusign document {which} responded {status}: {body}"
            )));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| SignatureError::Provider(e.to_string()))?;
        Ok(bytes.to_vec())
    }
}

/// DocuSign's envelope-create response. We need only the id.
#[derive(Deserialize)]
struct EnvelopeSummary {
    #[serde(rename = "envelopeId")]
    envelope_id: String,
}

/// DocuSign's `views/recipient` response. We need only the signing URL.
#[derive(Deserialize)]
struct RecipientViewResponse {
    url: String,
}

#[async_trait]
impl SignatureProvider for DocuSignSignatureProvider {
    async fn send_for_signature(
        &self,
        notation_id: Uuid,
        pdf: &[u8],
        manifest: &SignatureManifest,
    ) -> Result<SignatureRequestId, SignatureError> {
        let url = format!(
            "{}/v2.1/accounts/{}/envelopes",
            self.base_url.trim_end_matches('/'),
            self.account_id
        );
        let resp = self
            .http
            .post(&url)
            // Idempotency at the provider boundary: the same notation
            // never creates two envelopes, even under a concurrent
            // double-send. DocuSign honors this key for 24h.
            .header("X-DocuSign-Idempotency-Key", notation_id.to_string())
            .bearer_auth(self.bearer_token().await?)
            .json(&self.build_envelope_body(notation_id, pdf, manifest))
            .send()
            .await
            .map_err(|e| SignatureError::Provider(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(SignatureError::Provider(format!(
                "docusign responded {status}: {body}"
            )));
        }
        let summary: EnvelopeSummary = resp
            .json()
            .await
            .map_err(|e| SignatureError::Provider(e.to_string()))?;
        Ok(SignatureRequestId(summary.envelope_id))
    }

    async fn create_recipient_view(
        &self,
        request_id: &SignatureRequestId,
        view: &RecipientView,
    ) -> Result<String, SignatureError> {
        let url = format!(
            "{}/v2.1/accounts/{}/envelopes/{}/views/recipient",
            self.base_url.trim_end_matches('/'),
            self.account_id,
            request_id.0
        );
        // `authenticationMethod` is the audit label DocuSign records for
        // how the signer was authenticated upstream; "none" means the app
        // (Neon Law Navigator, behind its own OIDC login) vouched for them. The
        // email/userName/clientUserId triple resolves the captive
        // recipient created at envelope time.
        let body = serde_json::json!({
            "returnUrl": view.return_url,
            "authenticationMethod": "none",
            "email": view.email,
            "userName": view.name,
            "clientUserId": view.client_user_id,
        });
        let resp = self
            .http
            .post(&url)
            .bearer_auth(self.bearer_token().await?)
            .json(&body)
            .send()
            .await
            .map_err(|e| SignatureError::Provider(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(SignatureError::Provider(format!(
                "docusign recipient view responded {status}: {body}"
            )));
        }
        let view: RecipientViewResponse = resp
            .json()
            .await
            .map_err(|e| SignatureError::Provider(e.to_string()))?;
        Ok(view.url)
    }

    async fn fetch_completed_documents(
        &self,
        request_id: &SignatureRequestId,
    ) -> Result<CompletedDocuments, SignatureError> {
        // `combined` is every signed document merged into one PDF;
        // `certificate` is the Certificate of Completion.
        let signed_pdf = self
            .get_envelope_document(&request_id.0, "combined")
            .await?;
        let certificate_pdf = self
            .get_envelope_document(&request_id.0, "certificate")
            .await?;
        Ok(CompletedDocuments {
            signed_pdf,
            certificate_pdf,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        validate_anchors, CachedToken, DocuSignSignatureProvider, EnvelopeStatus,
        FakeSignatureProvider, Fault, Faults, RecipientView, SignatureError, SignatureField,
        SignatureFieldKind, SignatureManifest, SignatureProvider, SignatureProviderKind,
        SignatureProviderSelectionError, SignatureRecipient, SignatureRequestId, WebhookTarget,
        SIGNATURE_PROVIDER_ENV,
    };
    use crate::docusign_auth::{test_priv_pem, DocuSignJwtAuth};
    use base64::Engine;
    use uuid::Uuid;
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const ID1: Uuid = Uuid::from_u128(1);
    const ID2: Uuid = Uuid::from_u128(2);
    const ID42: Uuid = Uuid::from_u128(42);
    const ID43: Uuid = Uuid::from_u128(43);

    /// Empty manifest — the pre-anchor fallback path.
    fn no_manifest() -> SignatureManifest {
        SignatureManifest::default()
    }

    /// A real Typst-rendered PDF whose text layer carries every anchor of
    /// [`retainer_manifest`] — the same invisible-white-text expansion the
    /// retainer walk produces (`signature_render::anchor_string`), so the
    /// fake's anchor validation sees exactly what production sees.
    fn rendered_anchor_pdf() -> Vec<u8> {
        let anchors = retainer_manifest()
            .fields
            .iter()
            .map(|f| f.anchor.as_str())
            .collect::<Vec<_>>()
            .join(" \\ ");
        pdf::render(&format!(
            "= Test retainer\n\n#text(fill: white, size: 4pt)[{anchors}]\n"
        ))
        .expect("anchor PDF renders")
    }

    /// A two-party retainer manifest: the client (routing 1) signs and
    /// dates; the firm (routing 2) countersigns.
    fn retainer_manifest() -> SignatureManifest {
        SignatureManifest {
            recipients: vec![
                SignatureRecipient {
                    role: "client".into(),
                    email: "libra@example.com".into(),
                    name: "Libra".into(),
                    routing_order: 1,
                    // The client signs embedded (in-portal), so they are
                    // captive — keyed on a stable per-notation handle.
                    client_user_id: Some("notation-client".into()),
                },
                SignatureRecipient {
                    role: "firm".into(),
                    email: "attorney@neonlaw.com".into(),
                    name: "Attorney".into(),
                    routing_order: 2,
                    // The firm countersigns from its inbox (emailed).
                    client_user_id: None,
                },
            ],
            fields: vec![
                SignatureField {
                    recipient_role: "client".into(),
                    kind: SignatureFieldKind::Signature,
                    anchor: "nlsig-client-signature-1".into(),
                },
                SignatureField {
                    recipient_role: "client".into(),
                    kind: SignatureFieldKind::Date,
                    anchor: "nlsig-client-date-1".into(),
                },
                SignatureField {
                    recipient_role: "firm".into(),
                    kind: SignatureFieldKind::Signature,
                    anchor: "nlsig-firm-signature-1".into(),
                },
            ],
        }
    }

    // --- Explicit, fail-closed selection ---------------------------------
    //
    // The trap these tests pin shut: a "send for signature" button that
    // quietly wrote fake envelopes. Selection must be an explicit boot
    // decision with no default, and the fake must refuse production.

    fn selection(
        get: &[(&str, &str)],
    ) -> Result<SignatureProviderKind, SignatureProviderSelectionError> {
        let owned: Vec<(String, String)> = get
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        SignatureProviderKind::from_lookup(move |name| {
            owned
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        })
    }

    #[test]
    fn an_unset_provider_refuses_to_boot_naming_the_accepted_values() {
        let err = selection(&[]).unwrap_err();
        assert_eq!(err, SignatureProviderSelectionError::Unset);
        assert!(err.to_string().contains(SIGNATURE_PROVIDER_ENV));
        assert!(err.to_string().contains("`fake`"));
        assert!(err.to_string().contains("`docusign`"));
    }

    #[test]
    fn an_empty_provider_value_is_as_good_as_unset() {
        assert_eq!(
            selection(&[(SIGNATURE_PROVIDER_ENV, "")]).unwrap_err(),
            SignatureProviderSelectionError::Unset
        );
    }

    #[test]
    fn an_unknown_provider_value_refuses_to_boot_naming_what_arrived() {
        let err = selection(&[(SIGNATURE_PROVIDER_ENV, "stub")]).unwrap_err();
        assert_eq!(
            err,
            SignatureProviderSelectionError::Unknown("stub".to_string())
        );
        assert!(err.to_string().contains("stub"));
        assert!(err.to_string().contains("no implicit default"));
    }

    #[test]
    fn the_fake_refuses_a_production_environment() {
        // An unset NAVIGATOR_ENVIRONMENT IS the production profile, so
        // the fake refuses there too — no dev-only var gets a default.
        assert_eq!(
            selection(&[(SIGNATURE_PROVIDER_ENV, "fake")]).unwrap_err(),
            SignatureProviderSelectionError::FakeInProduction
        );
        assert_eq!(
            selection(&[
                (SIGNATURE_PROVIDER_ENV, "fake"),
                ("NAVIGATOR_ENVIRONMENT", "production"),
            ])
            .unwrap_err(),
            SignatureProviderSelectionError::FakeInProduction
        );
    }

    #[test]
    fn the_fake_selects_in_a_dev_environment() {
        assert_eq!(
            selection(&[
                (SIGNATURE_PROVIDER_ENV, "fake"),
                ("NAVIGATOR_ENVIRONMENT", "dev"),
            ])
            .unwrap(),
            SignatureProviderKind::Fake
        );
    }

    #[test]
    fn docusign_selects_without_checking_the_environment_profile() {
        // The real provider is valid in every profile, production
        // included — selection for it never inspects NAVIGATOR_ENVIRONMENT.
        assert_eq!(
            selection(&[(SIGNATURE_PROVIDER_ENV, "docusign")]).unwrap(),
            SignatureProviderKind::DocuSign
        );
        assert_eq!(
            selection(&[
                (SIGNATURE_PROVIDER_ENV, "docusign"),
                ("NAVIGATOR_ENVIRONMENT", "production"),
            ])
            .unwrap(),
            SignatureProviderKind::DocuSign
        );
    }

    #[tokio::test]
    async fn fake_records_each_call_with_notation_id_and_pdf_len() {
        let fake = FakeSignatureProvider::new();
        let pdf = rendered_anchor_pdf();
        let id1 = fake
            .send_for_signature(ID42, &pdf, &retainer_manifest())
            .await
            .expect("validated anchors pass");
        let id2 = fake
            .send_for_signature(ID43, b"<longer pdf bytes here>", &no_manifest())
            .await
            .unwrap();

        assert_ne!(id1, id2, "each call gets a distinct id");
        assert_eq!(id1.0, format!("fake-{ID42}-1"));
        assert_eq!(id2.0, format!("fake-{ID43}-2"));

        let calls = fake.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].notation_id, ID42);
        assert_eq!(calls[0].pdf_bytes_len, pdf.len());
        // The manifest threads through verbatim so the step can assert it.
        assert_eq!(calls[0].manifest, retainer_manifest());
        assert_eq!(calls[1].notation_id, ID43);
        assert_eq!(calls[1].pdf_bytes_len, b"<longer pdf bytes here>".len());
        assert!(calls[1].manifest.is_empty());
    }

    #[tokio::test]
    async fn fake_persists_the_envelope_and_reads_it_back() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let fake = FakeSignatureProvider::new_in(temp.path());
        let pdf = rendered_anchor_pdf();
        let id = fake
            .send_for_signature(ID42, &pdf, &retainer_manifest())
            .await
            .expect("send succeeds");

        // The envelope is durable JSON under the root, not just a call
        // record: read it back from a fresh provider over the same root.
        let reread = FakeSignatureProvider::new_in(temp.path())
            .envelope(&id)
            .expect("envelope parses")
            .expect("envelope exists on disk");
        assert_eq!(reread.id, id.0);
        assert_eq!(reread.notation_id, ID42);
        assert_eq!(reread.status, EnvelopeStatus::Sent);
        assert_eq!(reread.recipients, retainer_manifest().recipients);
        assert_eq!(reread.fields, retainer_manifest().fields);
        assert_eq!(
            reread.pdf_bytes().expect("decodes"),
            pdf,
            "the exact bytes the sender approved are stored"
        );

        // And the listing sees it too, newest last.
        let all = FakeSignatureProvider::new_in(temp.path())
            .envelopes()
            .expect("listing reads");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, id.0);
    }

    #[tokio::test]
    async fn a_missing_anchor_refuses_the_send_and_persists_nothing() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let fake = FakeSignatureProvider::new_in(temp.path());
        // A real PDF whose text layer does NOT carry the manifest's
        // anchors — exactly the silently-unsigned-document shape the
        // validation exists to refuse.
        let pdf = pdf::render("= No anchors here\n").expect("renders");

        let err = fake
            .send_for_signature(ID42, &pdf, &retainer_manifest())
            .await
            .expect_err("missing anchors are a hard error");
        let missing = match err {
            SignatureError::AnchorValidation(message) => message,
            other @ SignatureError::Provider(_) => {
                panic!("expected AnchorValidation, got {other:?}")
            }
        };
        assert!(missing.contains("nlsig-client-signature-1"), "{missing}");
        assert!(missing.contains("nlsig-firm-signature-1"), "{missing}");

        // Nothing was sent unsigned: no envelope on disk, no call record.
        assert!(
            fake.envelope(&SignatureRequestId(format!("fake-{ID42}-1")))
                .expect("read succeeds")
                .is_none(),
            "no envelope may exist for a refused send"
        );
        assert!(fake.envelopes().expect("listing reads").is_empty());
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn anchor_validation_passes_on_the_rendered_retainer() {
        // The same expansion the retainer walk ships: every declared
        // anchor is present in the rendered document's text layer.
        let pdf = rendered_anchor_pdf();
        validate_anchors(&pdf, &retainer_manifest().fields).expect("all anchors present");
    }

    #[tokio::test]
    async fn fake_calls_snapshot_is_shareable_after_more_calls() {
        let stub = FakeSignatureProvider::new();
        stub.send_for_signature(ID1, b"a", &no_manifest())
            .await
            .unwrap();
        let snap = stub.calls();
        stub.send_for_signature(ID2, b"b", &no_manifest())
            .await
            .unwrap();
        assert_eq!(snap.len(), 1, "snapshot doesn't see later calls");
        assert_eq!(stub.calls().len(), 2);
    }

    fn docusign(base_url: String) -> DocuSignSignatureProvider {
        DocuSignSignatureProvider::new(
            base_url,
            "acct-guid",
            "TOKEN",
            "signer@example.com",
            "Signer",
        )
    }

    #[test]
    fn cached_token_is_fresh_strictly_before_refresh_at() {
        let ct = CachedToken {
            token: "tok".into(),
            refresh_at: 100,
        };
        assert_eq!(ct.fresh_at(99), Some("tok"), "fresh before refresh_at");
        assert_eq!(ct.fresh_at(100), None, "must re-mint at the boundary");
        assert_eq!(ct.fresh_at(101), None, "must re-mint past expiry");
    }

    #[tokio::test]
    async fn jwt_backed_provider_mints_once_then_serves_from_cache() {
        let server = MockServer::start().await;
        // The OAuth token endpoint is hit exactly once across both sends:
        // the second send must reuse the cached, still-fresh token.
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "minted-abc",
                "token_type": "Bearer",
                "expires_in": 3600,
            })))
            .expect(1)
            .mount(&server)
            .await;
        // Every envelope POST must carry the minted token as its bearer.
        Mock::given(method("POST"))
            .and(path("/v2.1/accounts/acct-guid/envelopes"))
            .and(header("authorization", "Bearer minted-abc"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "envelopeId": "env-1",
            })))
            .mount(&server)
            .await;

        let auth = DocuSignJwtAuth::new("ik", "user", server.uri(), test_priv_pem().into_bytes());
        let provider = DocuSignSignatureProvider::with_jwt_auth(
            server.uri(),
            "acct-guid",
            auth,
            "signer@example.com",
            "Signer",
        );

        let id1 = provider
            .send_for_signature(ID1, b"%PDF-1.7 fake", &no_manifest())
            .await
            .expect("first send mints a token and creates the envelope");
        let id2 = provider
            .send_for_signature(ID2, b"%PDF-1.7 fake", &no_manifest())
            .await
            .expect("second send reuses the cached token");
        assert_eq!(id1, SignatureRequestId("env-1".into()));
        assert_eq!(id2, SignatureRequestId("env-1".into()));
        // The `.expect(1)` on the oauth mock, verified on server drop,
        // proves exactly one mint backed both sends.
    }

    #[test]
    fn docusign_envelope_body_base64s_the_pdf_and_falls_back_to_single_signer() {
        // Empty manifest → the pre-anchor behavior: one configured
        // signer, no tabs.
        let ds = docusign("https://demo.docusign.net/restapi".into());
        let body = ds.build_envelope_body(ID42, b"%PDF-1.7 fake", &no_manifest());
        assert_eq!(body["status"], "sent");
        let signers = body["recipients"]["signers"].as_array().unwrap();
        assert_eq!(signers.len(), 1);
        assert_eq!(signers[0]["email"], "signer@example.com");
        assert!(signers[0]["tabs"].is_null(), "no tabs without a manifest");
        let doc_b64 = body["documents"][0]["documentBase64"].as_str().unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(doc_b64)
            .unwrap();
        assert_eq!(decoded, b"%PDF-1.7 fake");
    }

    #[test]
    fn docusign_envelope_body_emits_anchored_tabs_and_routing_from_manifest() {
        let ds = docusign("https://demo.docusign.net/restapi".into());
        let body = ds.build_envelope_body(ID42, b"%PDF", &retainer_manifest());
        let signers = body["recipients"]["signers"].as_array().unwrap();
        assert_eq!(signers.len(), 2, "one signer per manifest recipient");

        // Client signs first (routingOrder 1) with a signHere + dateSigned tab.
        let client = &signers[0];
        assert_eq!(client["email"], "libra@example.com");
        assert_eq!(client["routingOrder"], "1");
        // Captive: the client signs embedded, so DocuSign gets a
        // clientUserId and suppresses their email.
        assert_eq!(client["clientUserId"], "notation-client");
        assert_eq!(
            client["tabs"]["signHereTabs"][0]["anchorString"],
            "nlsig-client-signature-1"
        );
        assert_eq!(
            client["tabs"]["dateSignedTabs"][0]["anchorString"],
            "nlsig-client-date-1"
        );
        assert!(
            client["tabs"]["initialHereTabs"].is_null(),
            "client placed no initials"
        );

        // Firm countersigns second (routingOrder 2) — engagement forms here.
        // Non-captive: no clientUserId, so DocuSign emails the firm a link.
        let firm = &signers[1];
        assert_eq!(firm["email"], "attorney@neonlaw.com");
        assert_eq!(firm["routingOrder"], "2");
        assert!(
            firm.get("clientUserId").is_none(),
            "the firm signer is emailed, not captive"
        );
        assert_eq!(
            firm["tabs"]["signHereTabs"][0]["anchorString"],
            "nlsig-firm-signature-1"
        );
    }

    #[tokio::test]
    async fn docusign_create_recipient_view_returns_the_signing_url() {
        // The embedded-signing path: POST views/recipient with the
        // captive recipient's identifying triple, get back a one-shot URL.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(
                "/v2.1/accounts/acct-guid/envelopes/env-1/views/recipient",
            ))
            .and(header("authorization", "Bearer TOKEN"))
            .and(body_string_contains("notation-client"))
            .and(body_string_contains("https://app.example/return"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "url": "https://demo.docusign.net/signing/abc123",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let ds = docusign(server.uri());
        let url = ds
            .create_recipient_view(
                &SignatureRequestId("env-1".into()),
                &RecipientView {
                    return_url: "https://app.example/return".into(),
                    email: "libra@example.com".into(),
                    name: "Libra".into(),
                    client_user_id: "notation-client".into(),
                },
            )
            .await
            .expect("recipient view succeeds");
        assert_eq!(url, "https://demo.docusign.net/signing/abc123");
    }

    #[tokio::test]
    async fn docusign_recipient_view_maps_non_2xx_to_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(
                "/v2.1/accounts/acct-guid/envelopes/env-1/views/recipient",
            ))
            .respond_with(ResponseTemplate::new(400).set_body_string("UNKNOWN_RECIPIENT"))
            .mount(&server)
            .await;

        let ds = docusign(server.uri());
        let err = ds
            .create_recipient_view(
                &SignatureRequestId("env-1".into()),
                &RecipientView {
                    return_url: "https://app.example/return".into(),
                    email: "libra@example.com".into(),
                    name: "Libra".into(),
                    client_user_id: "notation-client".into(),
                },
            )
            .await
            .expect_err("a 400 is a provider error");
        assert!(
            err.to_string().contains("400"),
            "error surfaces status: {err}"
        );
    }

    #[tokio::test]
    async fn a_recipient_view_is_a_local_page_that_exists() {
        let fake = FakeSignatureProvider::new();
        let id = fake
            .send_for_signature(ID42, &rendered_anchor_pdf(), &retainer_manifest())
            .await
            .expect("send succeeds");
        let url = fake
            .create_recipient_view(
                &id,
                &RecipientView {
                    return_url: "https://app.example/return".into(),
                    email: "libra@example.com".into(),
                    name: "Libra".into(),
                    client_user_id: "notation-client".into(),
                },
            )
            .await
            .expect("view succeeds");
        // The URL the signer opens resolves to a real file on disk.
        assert!(
            url.starts_with("file://"),
            "the fake hands out a local page, got: {url}"
        );
        let page = url.strip_prefix("file://").expect("file URL");
        let html = std::fs::read_to_string(page).expect("the sign page exists");
        assert!(html.contains("Sign here"));
        assert!(html.contains("libra@example.com"));
        assert!(html.contains("nlsig-client-signature-1"));
    }

    #[tokio::test]
    async fn docusign_posts_bearer_authed_envelope_and_returns_envelope_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2.1/accounts/acct-guid/envelopes"))
            .and(header("authorization", "Bearer TOKEN"))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_json(serde_json::json!({"envelopeId": "env-789", "status": "sent"})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let ds = docusign(server.uri());
        let id = ds
            .send_for_signature(ID42, b"<pdf>", &no_manifest())
            .await
            .expect("send succeeds");
        assert_eq!(id, SignatureRequestId("env-789".into()));
    }

    #[tokio::test]
    async fn docusign_posts_the_manifest_anchor_strings_to_the_wire() {
        // The contract layer: assert the anchored tab actually reaches
        // DocuSign in the POST body, not just our in-memory JSON.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2.1/accounts/acct-guid/envelopes"))
            .and(body_string_contains("nlsig-client-signature-1"))
            .and(body_string_contains("nlsig-firm-signature-1"))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_json(serde_json::json!({"envelopeId": "env-anchored"})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let ds = docusign(server.uri());
        let id = ds
            .send_for_signature(ID42, b"<pdf>", &retainer_manifest())
            .await
            .expect("send succeeds");
        assert_eq!(id, SignatureRequestId("env-anchored".into()));
    }

    #[tokio::test]
    async fn docusign_sends_an_idempotency_key_keyed_on_the_notation() {
        // Provider-level guard: the envelope POST carries
        // X-DocuSign-Idempotency-Key = notation id, so a concurrent
        // double-send never creates two envelopes.
        let idem_key = ID42.to_string();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2.1/accounts/acct-guid/envelopes"))
            .and(header("x-docusign-idempotency-key", idem_key.as_str()))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_json(serde_json::json!({"envelopeId": "env-idem"})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let ds = docusign(server.uri());
        let id = ds
            .send_for_signature(ID42, b"<pdf>", &no_manifest())
            .await
            .expect("send succeeds");
        assert_eq!(id, SignatureRequestId("env-idem".into()));
    }

    #[tokio::test]
    async fn docusign_maps_non_2xx_to_provider_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2.1/accounts/acct-guid/envelopes"))
            .respond_with(ResponseTemplate::new(401).set_body_string("bad token"))
            .mount(&server)
            .await;

        let ds = docusign(server.uri());
        let err = ds
            .send_for_signature(ID42, b"<pdf>", &no_manifest())
            .await
            .unwrap_err();
        match err {
            super::SignatureError::Provider(msg) => {
                assert!(msg.contains("401"), "expected 401 in error, got: {msg}");
            }
            other @ super::SignatureError::AnchorValidation(_) => {
                panic!("expected Provider, got {other:?}")
            }
        }
    }

    #[tokio::test]
    async fn docusign_fetches_signed_pdf_and_certificate() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/v2.1/accounts/acct-guid/envelopes/env-1/documents/combined",
            ))
            .and(header("authorization", "Bearer TOKEN"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"%PDF-signed".to_vec()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/v2.1/accounts/acct-guid/envelopes/env-1/documents/certificate",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"%PDF-cert".to_vec()))
            .mount(&server)
            .await;

        let ds = docusign(server.uri());
        let docs = ds
            .fetch_completed_documents(&SignatureRequestId("env-1".into()))
            .await
            .expect("fetch succeeds");
        assert_eq!(docs.signed_pdf, b"%PDF-signed");
        assert_eq!(docs.certificate_pdf, b"%PDF-cert");
    }

    #[tokio::test]
    async fn docusign_document_download_maps_non_2xx_to_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/v2.1/accounts/acct-guid/envelopes/env-x/documents/combined",
            ))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let ds = docusign(server.uri());
        let err = ds
            .fetch_completed_documents(&SignatureRequestId("env-x".into()))
            .await
            .unwrap_err();
        match err {
            super::SignatureError::Provider(msg) => {
                assert!(msg.contains("404"), "expected 404 in error, got: {msg}");
            }
            other @ super::SignatureError::AnchorValidation(_) => {
                panic!("expected Provider, got {other:?}")
            }
        }
    }

    #[tokio::test]
    async fn signing_completes_the_envelope_and_documents_read_back() {
        let fake = FakeSignatureProvider::new();
        let pdf = rendered_anchor_pdf();
        let id = fake
            .send_for_signature(ID42, &pdf, &retainer_manifest())
            .await
            .expect("send succeeds");

        // One signature leaves the envelope live with no documents.
        let status = fake
            .sign(&id, "libra@example.com")
            .await
            .expect("first signature records");
        assert_eq!(status, EnvelopeStatus::Sent);
        assert_eq!(fake.envelope(&id).expect("parses").unwrap().signed.len(), 1);

        // The last signature completes it, and the filed documents are
        // the real ones: the exact approved PDF plus a rendered
        // certificate of completion.
        let status = fake
            .sign(&id, "attorney@neonlaw.com")
            .await
            .expect("final signature records");
        assert_eq!(status, EnvelopeStatus::Completed);
        let docs = fake
            .fetch_completed_documents(&id)
            .await
            .expect("completed envelope hands out documents");
        assert_eq!(docs.signed_pdf, pdf);
        assert!(docs.certificate_pdf.starts_with(b"%PDF"));
        let certificate_text =
            pdf::passage::occurrence_count(&docs.certificate_pdf, "Certificate of completion")
                .expect("certificate parses");
        assert!(certificate_text > 0, "the certificate names itself");
        // An unsigned envelope hands out nothing.
        assert!(fake
            .fetch_completed_documents(&SignatureRequestId("fake-none".into()))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn completion_delivers_hmac_signed_connect_webhooks() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let key = b"loop-hmac-key".to_vec();
        let fake = FakeSignatureProvider::new().with_webhook(WebhookTarget {
            url: server.uri(),
            hmac_key: key.clone(),
        });
        let pdf = rendered_anchor_pdf();
        let id = fake
            .send_for_signature(ID42, &pdf, &retainer_manifest())
            .await
            .expect("send succeeds");
        fake.sign(&id, "libra@example.com").await.expect("signs");
        fake.sign(&id, "attorney@neonlaw.com")
            .await
            .expect("completes");

        // Three deliveries: recipient-completed for each signature,
        // envelope-completed when the last lands — every one HMAC-signed
        // over its exact body with the receiver's own key.
        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests.len(), 3);
        let mut events = Vec::new();
        for request in &requests {
            let payload: serde_json::Value = serde_json::from_slice(&request.body).expect("json");
            events.push(payload["event"].as_str().expect("event").to_string());
            let provided = request
                .headers
                .get("x-docusign-signature-1")
                .and_then(|v| v.to_str().ok())
                .expect("HMAC header present");
            assert!(
                crate::webhook_auth::verify_hmac_sha256_b64(&key, &request.body, provided),
                "the receiver must verify the delivery fail-closed"
            );
        }
        assert_eq!(
            events,
            vec![
                "recipient-completed",
                "recipient-completed",
                "envelope-completed"
            ]
        );
        // The receiver's own parser accepts the completion payload.
        let completion: serde_json::Value =
            serde_json::from_slice(&requests[2].body).expect("json");
        assert_eq!(completion["data"]["envelopeSummary"]["status"], "completed");
        assert_eq!(
            completion["data"]["envelopeId"], id.0,
            "the envelope id threads through the callback"
        );
        assert_eq!(
            fake.envelope(&id)
                .expect("parses")
                .unwrap()
                .webhook_status
                .as_deref(),
            Some("delivered")
        );
    }

    #[tokio::test]
    async fn a_failed_webhook_delivery_is_recorded_and_not_fatal() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let fake = FakeSignatureProvider::new().with_webhook(WebhookTarget {
            url: server.uri(),
            hmac_key: b"loop-hmac-key".to_vec(),
        });
        let pdf = rendered_anchor_pdf();
        let id = fake
            .send_for_signature(ID42, &pdf, &retainer_manifest())
            .await
            .expect("send succeeds");

        // The delivery fails; the signature still records and the
        // envelope stays in its last true state.
        let status = fake
            .sign(&id, "libra@example.com")
            .await
            .expect("signing never depends on delivery");
        assert_eq!(status, EnvelopeStatus::Sent);
        let envelope = fake.envelope(&id).expect("parses").unwrap();
        assert_eq!(envelope.signed.len(), 1);
        assert!(
            envelope
                .webhook_status
                .as_deref()
                .unwrap_or_default()
                .starts_with("failed"),
            "the failed delivery is recorded on the envelope"
        );
    }

    // --- The local signature loop ------------------------------------------
    //
    // The headline: one continuous loop, no DocuSign credentials. Render →
    // send → sign → webhook → file. The dedicated CI job runs exactly this
    // test; green means the local story is true.

    #[tokio::test]
    async fn the_local_signature_loop_renders_sends_signs_webhooks_and_files() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let key = b"loop-hmac-key".to_vec();
        let fake = FakeSignatureProvider::new().with_webhook(WebhookTarget {
            url: server.uri(),
            hmac_key: key.clone(),
        });

        // Render: the prepared PDF carries every anchor, exactly as the
        // retainer walk produces it.
        let pdf = rendered_anchor_pdf();

        // Send: the envelope exists on disk, and every recipient's sign
        // page resolves.
        let id = fake
            .send_for_signature(ID42, &pdf, &retainer_manifest())
            .await
            .expect("the loop sends");
        let client_view = fake
            .create_recipient_view(
                &id,
                &RecipientView {
                    return_url: "https://app.example/return".into(),
                    email: "libra@example.com".into(),
                    name: "Libra".into(),
                    client_user_id: "notation-client".into(),
                },
            )
            .await
            .expect("the client's sign page exists");
        assert!(client_view.starts_with("file://"), "a real local URL");

        // Sign: the client first (routing 1), the firm last — the last
        // signature completes the envelope and fires the completion
        // webhook.
        let after_client = fake
            .sign(&id, "libra@example.com")
            .await
            .expect("client signs");
        assert_eq!(
            after_client,
            EnvelopeStatus::Sent,
            "one signature is not completion"
        );
        let after_firm = fake
            .sign(&id, "attorney@neonlaw.com")
            .await
            .expect("firm countersigns");
        assert_eq!(after_firm, EnvelopeStatus::Completed);

        // Webhook: three Connect-shaped deliveries, every one HMAC-signed
        // over its exact body with the receiver's own key.
        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests.len(), 3);
        for request in &requests {
            let provided = request
                .headers
                .get("x-docusign-signature-1")
                .and_then(|v| v.to_str().ok())
                .expect("HMAC header present");
            assert!(
                crate::webhook_auth::verify_hmac_sha256_b64(&key, &request.body, provided),
                "the receiver must verify the delivery fail-closed"
            );
        }

        // File: the signed PDF is exactly the bytes the sender approved,
        // the certificate of completion accompanies it, and the envelope
        // on disk is true.
        let documents = fake
            .fetch_completed_documents(&id)
            .await
            .expect("the loop files the documents");
        assert_eq!(documents.signed_pdf, pdf, "the filed PDF is the sent PDF");
        assert!(
            !documents.certificate_pdf.is_empty(),
            "the certificate of completion accompanies the filed PDF"
        );
        let envelope = fake.envelope(&id).expect("parses").unwrap();
        assert_eq!(envelope.status, EnvelopeStatus::Completed);
        assert_eq!(envelope.webhook_status.as_deref(), Some("delivered"));
    }

    // --- Fault injection --------------------------------------------------
    //
    // The ugly paths as first-class outcomes: an armed fault fires on
    // purpose, deterministically, before any incidental network error
    // could — and the durable envelope keeps its last true state.

    #[tokio::test]
    async fn an_injected_send_fault_writes_nothing_and_resumes_without_duplicating() {
        let root = std::env::temp_dir().join(format!("navigator-fault-{}", Uuid::new_v4()));
        let fake = FakeSignatureProvider::new_in(&root).with_faults(Faults::inject(&[Fault::Send]));
        let err = fake
            .send_for_signature(ID42, &rendered_anchor_pdf(), &retainer_manifest())
            .await
            .expect_err("the injected send failure fires");
        assert!(err.to_string().contains("injected send failure"));
        // Nothing half-sent exists: no envelope on disk, no call recorded.
        assert!(fake.envelopes().expect("reads").is_empty());
        assert!(fake.calls().is_empty());

        // Re-running the loop without the fault resumes: exactly one
        // envelope, not a duplicate.
        let resumed = FakeSignatureProvider::new_in(&root);
        let id = resumed
            .send_for_signature(ID42, &rendered_anchor_pdf(), &retainer_manifest())
            .await
            .expect("send succeeds once the fault is gone");
        assert_eq!(
            resumed.envelopes().expect("reads").len(),
            1,
            "re-running the loop resumes rather than duplicates"
        );
        assert_eq!(id.0, "fake-00000000-0000-0000-0000-00000000002a-1");
    }

    #[tokio::test]
    async fn an_injected_view_fault_leaves_the_envelope_untouched() {
        let fake = FakeSignatureProvider::new().with_faults(Faults::inject(&[Fault::View]));
        let id = fake
            .send_for_signature(ID42, &rendered_anchor_pdf(), &retainer_manifest())
            .await
            .expect("send succeeds");
        let err = fake
            .create_recipient_view(
                &id,
                &RecipientView {
                    return_url: "https://app.example/return".into(),
                    email: "libra@example.com".into(),
                    name: "Libra".into(),
                    client_user_id: "notation-client".into(),
                },
            )
            .await
            .expect_err("the injected view failure fires");
        assert!(err.to_string().contains("injected view failure"));
        // The durable envelope keeps its last true state, and no view was
        // recorded.
        let envelope = fake.envelope(&id).expect("parses").unwrap();
        assert_eq!(envelope.status, EnvelopeStatus::Sent);
        assert!(fake.recipient_views().is_empty());
    }

    #[tokio::test]
    async fn an_injected_webhook_fault_records_failure_and_the_loop_recovers() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let fake = FakeSignatureProvider::new()
            .with_webhook(WebhookTarget {
                url: server.uri(),
                hmac_key: b"loop-hmac-key".to_vec(),
            })
            .with_faults(Faults::inject(&[Fault::Webhook]));
        let id = fake
            .send_for_signature(ID42, &rendered_anchor_pdf(), &retainer_manifest())
            .await
            .expect("send succeeds");

        // Signing to completion never depends on delivery, and the armed
        // fault fails every delivery on purpose.
        fake.sign(&id, "libra@example.com").await.expect("signs");
        let status = fake
            .sign(&id, "attorney@neonlaw.com")
            .await
            .expect("completes");
        assert_eq!(status, EnvelopeStatus::Completed);
        let requests = server.received_requests().await.expect("requests");
        assert!(
            requests.is_empty(),
            "the armed fault fails delivery before anything reaches the receiver"
        );

        // The durable envelope is true: completed on disk, the failure
        // recorded — and the loop recovers by reading completion directly
        // from the provider.
        let envelope = fake.envelope(&id).expect("parses").unwrap();
        assert_eq!(envelope.status, EnvelopeStatus::Completed);
        assert_eq!(
            envelope.webhook_status.as_deref(),
            Some("failed: injected webhook fault")
        );
        let documents = fake
            .fetch_completed_documents(&id)
            .await
            .expect("completion is recoverable from the provider");
        assert!(
            !documents.signed_pdf.is_empty() && !documents.certificate_pdf.is_empty(),
            "the loop recovers the real bytes, not just a status"
        );
    }
}

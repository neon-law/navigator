//! Inbound e-signature completion webhook.
//!
//! The retainer workflow parks at `sent_for_signature__pending` after
//! the rendered engagement letter is sent for signature
//! (`portal::signature::SignatureProvider::send_for_signature`). The
//! signature provider (DocuSign in the reference deploy) calls this
//! endpoint when the client finishes signing; we map that to a
//! `signature_received` signal on the notation's workflow object, which
//! advances it to `END` (the executed retainer).
//!
//! ## Trust model
//!
//! This endpoint *advances workflow state* from the public internet, so
//! it must not be trusted on URL alone — an unauthenticated POST here
//! would be a state-advancing forgery: the firm asserting a client
//! signed when they did not. Two layers gate it:
//!
//! - **Path secret** (`/webhook/esignature/:secret`) — coarse "is this
//!   our endpoint", mirroring the SendGrid webhooks. Optional
//!   defense-in-depth; `None` in dev/tests accepts any token.
//! - **HMAC-SHA256 over the raw body** (`X-DocuSign-Signature-1`) — the
//!   real gate. Verified *before* the JSON is parsed, over the exact
//!   bytes received, so the envelope id the body later names is
//!   trustworthy. When `esignature_hmac_key` is configured the header
//!   must be present and valid or the request is rejected (fail
//!   closed). `None` in dev/tests skips it. Production sets it via
//!   `enforce_deployment_invariants`.
//!
//! Terminal events advance the wait: completion, decline, void, and expiry.
//! A `recipient-completed` event advances nothing: it records which declared
//! signer role finished its own step on the still-live envelope, so the
//! matter page can tell a client who has signed apart from one who has not
//! while the firm's countersignature is outstanding. DocuSign also fires
//! `sent`, `delivered`, …; those return 200 without signalling so the
//! provider stops retrying and no half-states leak. An
//! unknown envelope id (no notation matches) is likewise a 200 no-op — the
//! callback is idempotent.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;

use workflows::{MachineKind, StateMachineRuntime};

use crate::inbound_email::constant_time_eq;
use crate::webhook_auth::verify_hmac_sha256_b64;

/// DocuSign Connect's HMAC header. The "-1" suffix is the configured
/// HMAC key slot; DocuSign supports several, we use the first.
const SIGNATURE_HEADER: &str = "x-docusign-signature-1";

/// The `signature_received` condition the retainer workflow waits on at
/// `sent_for_signature__pending`.
const SIGNATURE_RECEIVED: &str = "signature_received";

/// The `signature_declined` condition — a declined or voided envelope
/// that will never be executed. Advances the same wait state to a
/// terminal end so the matter doesn't dead-end at
/// `sent_for_signature__pending`, and the journal records the decline.
const SIGNATURE_DECLINED: &str = "signature_declined";

#[derive(Debug, thiserror::Error)]
pub enum WebhookError {
    #[error("unauthorized: webhook path secret mismatch")]
    UnauthorizedPath,
    #[error("unauthorized: missing or invalid signature")]
    UnauthorizedSignature,
    #[error("malformed callback payload: {0}")]
    Malformed(String),
    #[error("workflow runtime: {0}")]
    Runtime(String),
    #[error("database: {0}")]
    Database(String),
}

impl IntoResponse for WebhookError {
    fn into_response(self) -> axum::response::Response {
        let code = match &self {
            Self::UnauthorizedPath | Self::UnauthorizedSignature => StatusCode::UNAUTHORIZED,
            Self::Malformed(_) => StatusCode::BAD_REQUEST,
            Self::Runtime(_) | Self::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (code, self.to_string()).into_response()
    }
}

/// DocuSign Connect JSON callback. Tolerant subset — we read only the
/// envelope id and its status. The completion signal is either the
/// top-level `event` (`envelope-completed`) or
/// `data.envelopeSummary.status == "completed"`.
#[derive(serde::Deserialize)]
struct ConnectPayload {
    #[serde(default)]
    event: Option<String>,
    data: ConnectData,
}

#[derive(serde::Deserialize)]
struct ConnectData {
    #[serde(rename = "envelopeId")]
    envelope_id: String,
    /// Present on recipient events: the recipient id the manifest sent,
    /// which is that signer's routing order.
    #[serde(rename = "recipientId", default)]
    recipient_id: Option<String>,
    #[serde(rename = "envelopeSummary", default)]
    envelope_summary: Option<EnvelopeSummary>,
}

#[derive(serde::Deserialize)]
struct EnvelopeSummary {
    #[serde(default)]
    status: Option<String>,
}

impl ConnectPayload {
    /// True when this callback reports the envelope as fully executed.
    fn is_completed(&self) -> bool {
        self.event.as_deref() == Some("envelope-completed")
            || self
                .envelope_summary_status()
                .is_some_and(|s| s.eq_ignore_ascii_case("completed"))
    }

    /// True when the envelope will never be executed — the client
    /// declined, or the envelope was voided. Either way the engagement
    /// did not form; the workflow leaves the wait state on
    /// `signature_declined`.
    fn is_declined(&self) -> bool {
        matches!(
            self.event.as_deref(),
            Some("envelope-declined" | "recipient-declined")
        ) || self
            .envelope_summary_status()
            .is_some_and(|s| s.eq_ignore_ascii_case("declined"))
    }

    /// True when the provider voided the envelope before completion.
    fn is_voided(&self) -> bool {
        self.event.as_deref() == Some("envelope-voided")
            || self
                .envelope_summary_status()
                .is_some_and(|s| s.eq_ignore_ascii_case("voided"))
    }

    /// True when the provider's envelope expiry event/status is present.
    fn is_expired(&self) -> bool {
        self.event.as_deref() == Some("envelope-expired")
            || self
                .envelope_summary_status()
                .is_some_and(|s| s.eq_ignore_ascii_case("expired"))
    }

    /// The recipient id when this callback reports one recipient finishing
    /// their own step on an envelope that is not itself terminal.
    fn completed_recipient(&self) -> Option<&str> {
        if self.event.as_deref() != Some("recipient-completed") || self.terminal_state().is_some() {
            return None;
        }
        self.data
            .recipient_id
            .as_deref()
            .filter(|id| !id.is_empty())
    }

    fn terminal_state(&self) -> Option<store::signatures::SignatureState> {
        if self.is_completed() {
            Some(store::signatures::SignatureState::Completed)
        } else if self.is_declined() {
            Some(store::signatures::SignatureState::Declined)
        } else if self.is_voided() {
            Some(store::signatures::SignatureState::Voided)
        } else if self.is_expired() {
            Some(store::signatures::SignatureState::Expired)
        } else {
            None
        }
    }

    fn envelope_summary_status(&self) -> Option<&str> {
        self.data
            .envelope_summary
            .as_ref()
            .and_then(|s| s.status.as_deref())
    }
}

/// Webhook handler. Verifies the path secret, then the raw-body HMAC,
/// then — only for a `completed` event whose envelope id matches a
/// notation — signals `signature_received`.
pub async fn webhook(
    State(state): State<crate::AppState>,
    Path(provided): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, WebhookError> {
    // 1. Path secret (coarse). Constant-time when configured.
    if let Some(configured) = state.esignature_webhook_secret.as_deref() {
        if !constant_time_eq(&provided, configured) {
            tracing::warn!("esignature webhook: path secret mismatch");
            return Err(WebhookError::UnauthorizedPath);
        }
    }

    // 2. HMAC over the raw body (the real gate). Verify BEFORE parsing
    //    so the digest covers the exact bytes that name the envelope.
    if let Some(key) = state.esignature_hmac_key.as_deref() {
        let provided_sig = headers
            .get(SIGNATURE_HEADER)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                tracing::warn!("esignature webhook: signature header absent");
                WebhookError::UnauthorizedSignature
            })?;
        if !verify_hmac_sha256_b64(key.as_bytes(), &body, provided_sig) {
            tracing::warn!("esignature webhook: signature verification failed");
            return Err(WebhookError::UnauthorizedSignature);
        }
    }

    // 3. Parse — now that the bytes are trusted.
    let payload: ConnectPayload =
        serde_json::from_slice(&body).map_err(|e| WebhookError::Malformed(e.to_string()))?;

    // Map the terminal envelope events to workflow conditions. A
    // completion executes the retainer; a decline/void ends the wait
    // without an engagement. Everything else (sent / delivered /
    // viewed / …) is acked so the provider stops retrying, but does not
    // advance state.
    if let Some(recipient_id) = payload.completed_recipient() {
        record_recipient_completion(&state, &payload.data.envelope_id, recipient_id).await?;
        return Ok(StatusCode::OK);
    }
    let Some(terminal_state) = payload.terminal_state() else {
        tracing::info!(
            envelope_id = %payload.data.envelope_id,
            event = ?payload.event,
            "esignature webhook: non-terminal event ignored"
        );
        return Ok(StatusCode::OK);
    };
    let condition = if terminal_state == store::signatures::SignatureState::Completed {
        SIGNATURE_RECEIVED
    } else {
        SIGNATURE_DECLINED
    };

    advance(&state, &payload.data.envelope_id, condition, terminal_state).await?;
    Ok(StatusCode::OK)
}

/// Record which declared signer role finished its own step on a live
/// envelope. The recipient id is the routing order the manifest sent, read
/// back against the notation's template signer set
/// ([`crate::retainer_walk::signer_role_for_recipient`]). An unknown
/// envelope, a missing notation or template, or an id that names no
/// declared role is a logged no-op, like every other unmatched callback.
async fn record_recipient_completion(
    state: &crate::AppState,
    envelope_id: &str,
    recipient_id: &str,
) -> Result<(), WebhookError> {
    let provider = store::signatures::SignatureProvider::DocuSign;
    let db = |e: &dyn std::fmt::Display| WebhookError::Database(e.to_string());
    let Some(signature) = store::signatures::by_provider(&state.surreal, provider, envelope_id)
        .await
        .map_err(|e| db(&e))?
    else {
        tracing::warn!(%envelope_id, "esignature webhook: no signature for envelope id");
        return Ok(());
    };
    let Some(notation) = store::notations::find_by_id(&state.surreal, signature.notation_id)
        .await
        .map_err(|e| db(&e))?
    else {
        tracing::warn!(%envelope_id, "esignature webhook: signature envelope points at a missing notation");
        return Ok(());
    };
    let Some(template) = store::templates::find_by_id(&state.surreal, notation.template_id)
        .await
        .map_err(|e| db(&e))?
    else {
        tracing::warn!(%envelope_id, "esignature webhook: notation points at a missing template");
        return Ok(());
    };
    let source = store::templates::body(&state.surreal, &state.storage, &template)
        .await
        .map_err(|e| db(&e))?;
    let Some(role) = crate::retainer_walk::signer_role_for_recipient(&source, recipient_id) else {
        tracing::warn!(%envelope_id, %recipient_id, "esignature webhook: recipient names no declared signer");
        return Ok(());
    };
    let recorded =
        store::signatures::record_signer_completion(&state.surreal, provider, envelope_id, &role)
            .await
            .map_err(|e| db(&e))?;
    tracing::info!(%envelope_id, %role, recorded, "esignature webhook: signer completed");
    Ok(())
}

/// Resolve an envelope id to its notation and signal `condition`
/// (`signature_received` or `signature_declined`). An unknown envelope
/// id is a logged no-op (the callback may arrive for an envelope we
/// never tracked, or twice).
async fn advance(
    state: &crate::AppState,
    envelope_id: &str,
    condition: &str,
    terminal_state: store::signatures::SignatureState,
) -> Result<(), WebhookError> {
    let provider = store::signatures::SignatureProvider::DocuSign;
    let Some(signature) = store::signatures::by_provider(&state.surreal, provider, envelope_id)
        .await
        .map_err(|e| WebhookError::Database(e.to_string()))?
    else {
        tracing::warn!(%envelope_id, "esignature webhook: no signature for envelope id");
        return Ok(());
    };
    if signature.state != store::signatures::SignatureState::Requested {
        tracing::info!(
            %envelope_id,
            existing_state = signature.state.as_str(),
            incoming_state = terminal_state.as_str(),
            "esignature webhook: terminal event ignored after state was recorded"
        );
        return Ok(());
    }
    let Some(row) = store::notations::find_by_id(&state.surreal, signature.notation_id)
        .await
        .map_err(|e| WebhookError::Database(e.to_string()))?
    else {
        tracing::warn!(%envelope_id, "esignature webhook: signature envelope points at a missing notation");
        return Ok(());
    };

    let notation_id = row.id;
    let next = StateMachineRuntime::signal(
        state.workflow_runtime.as_ref(),
        MachineKind::Workflow,
        notation_id,
        condition,
        None,
    )
    .await
    .map_err(|e| WebhookError::Runtime(e.to_string()))?;

    // Mirror the workflow state into the `notations` row (the same
    // pattern the retainer walk uses) so the admin UI reflects the end.
    store::notations::update_state(&state.surreal, notation_id, next.as_str())
        .await
        .map_err(|e| WebhookError::Database(e.to_string()))?;

    tracing::info!(%envelope_id, %condition, next_state = %next.as_str(), "esignature webhook: signature event");

    match terminal_state {
        store::signatures::SignatureState::Completed => {
            // Stamp the completion time on the signature so the executed record
            // carries when it was signed. Best-effort: the workflow already
            // advanced, so a stamp failure must not fail the webhook.
            if let Err(e) = store::signatures::stamp_signed(
                &state.surreal,
                provider,
                envelope_id,
                &chrono::Utc::now().to_rfc3339(),
            )
            .await
            {
                tracing::error!(
                    %envelope_id, error = %e,
                    "esignature webhook: stamping signed_at failed (signature still recorded)"
                );
            }

            // On completion, archive the executed document set (signed PDF +
            // Certificate of Completion — the ESIGN record) to object storage.
            // Best-effort: the signature is already recorded, so an archive
            // failure must NOT fail the webhook (DocuSign would retry forever);
            // the GCS source-of-truth can be backfilled.
            if let Err(e) = archive_completed_documents(state, envelope_id, notation_id).await {
                tracing::error!(
                    %envelope_id, error = %e,
                    "esignature webhook: archiving signed documents failed (signature still recorded)"
                );
            }
        }
        store::signatures::SignatureState::Declined => {
            if let Err(e) =
                store::signatures::stamp_declined(&state.surreal, provider, envelope_id).await
            {
                tracing::error!(%envelope_id, error = %e, "esignature webhook: stamping declined failed");
            }
        }
        store::signatures::SignatureState::Voided => {
            if let Err(e) =
                store::signatures::stamp_voided(&state.surreal, provider, envelope_id).await
            {
                tracing::error!(%envelope_id, error = %e, "esignature webhook: stamping voided failed");
            }
        }
        store::signatures::SignatureState::Expired => {
            if let Err(e) =
                store::signatures::stamp_expired(&state.surreal, provider, envelope_id).await
            {
                tracing::error!(%envelope_id, error = %e, "esignature webhook: stamping expired failed");
            }
        }
        store::signatures::SignatureState::Requested => {
            tracing::warn!(%envelope_id, "esignature webhook: requested state had no terminal stamp");
        }
    }
    Ok(())
}

/// Download the signed PDF + Certificate of Completion from the provider
/// and store both in object storage (the source of truth; the Drive
/// mirror is the existing sync's job). Returns the provider/storage
/// error so the caller can log it without failing the webhook.
async fn archive_completed_documents(
    state: &crate::AppState,
    envelope_id: &str,
    notation_id: uuid::Uuid,
) -> Result<(), crate::signature::SignatureError> {
    use crate::signature::{SignatureError, SignatureRequestId};

    let docs = state
        .signature_provider
        .fetch_completed_documents(&SignatureRequestId(envelope_id.to_string()))
        .await?;

    let put = |key: String, bytes: Vec<u8>| async move {
        state
            .storage
            .put(&key, &bytes, "application/pdf")
            .await
            .map_err(|e| SignatureError::Provider(e.to_string()))
    };
    put(
        store::notations::signed_document_storage_key(notation_id),
        docs.signed_pdf,
    )
    .await?;
    put(
        store::notations::certificate_of_completion_storage_key(notation_id),
        docs.certificate_pdf,
    )
    .await?;
    tracing::info!(%envelope_id, "esignature webhook: archived signed retainer + certificate");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::ConnectPayload;

    fn parse(json: serde_json::Value) -> ConnectPayload {
        serde_json::from_value(json).expect("connect payload parses")
    }

    #[test]
    fn completion_event_classifies_as_completed_only() {
        let p = parse(serde_json::json!({
            "event": "envelope-completed",
            "data": { "envelopeId": "e1" }
        }));
        assert!(p.is_completed());
        assert!(!p.is_declined());
    }

    #[test]
    fn completed_summary_status_classifies_as_completed() {
        let p = parse(serde_json::json!({
            "data": { "envelopeId": "e1", "envelopeSummary": { "status": "completed" } }
        }));
        assert!(p.is_completed());
        assert!(!p.is_declined());
    }

    #[test]
    fn declined_events_classify_as_declined() {
        for ev in ["envelope-declined", "recipient-declined"] {
            let p = parse(serde_json::json!({ "event": ev, "data": { "envelopeId": "e1" } }));
            assert!(p.is_declined(), "{ev} should be declined");
            assert!(!p.is_completed(), "{ev} is not completed");
        }
    }

    #[test]
    fn voided_events_and_status_classify_as_voided() {
        for payload in [
            serde_json::json!({ "event": "envelope-voided", "data": { "envelopeId": "e1" } }),
            serde_json::json!({ "data": { "envelopeId": "e1", "envelopeSummary": { "status": "voided" } } }),
        ] {
            let p = parse(payload);
            assert!(p.is_voided());
            assert_eq!(
                p.terminal_state(),
                Some(store::signatures::SignatureState::Voided)
            );
        }
    }

    #[test]
    fn declined_summary_status_classifies_as_declined() {
        for st in ["declined", "Declined"] {
            let p = parse(serde_json::json!({
                "data": { "envelopeId": "e1", "envelopeSummary": { "status": st } }
            }));
            assert!(p.is_declined(), "status {st} should be declined");
        }
    }

    #[test]
    fn expiry_event_and_status_classify_as_expired() {
        for payload in [
            serde_json::json!({ "event": "envelope-expired", "data": { "envelopeId": "e1" } }),
            serde_json::json!({ "data": { "envelopeId": "e1", "envelopeSummary": { "status": "Expired" } } }),
        ] {
            let p = parse(payload);
            assert!(p.is_expired());
            assert_eq!(
                p.terminal_state(),
                Some(store::signatures::SignatureState::Expired)
            );
        }
    }

    #[test]
    fn a_recipient_completion_names_its_recipient_and_is_not_terminal() {
        let p = parse(serde_json::json!({
            "event": "recipient-completed",
            "data": { "envelopeId": "e1", "recipientId": "1" }
        }));
        assert_eq!(p.completed_recipient(), Some("1"));
        assert_eq!(p.terminal_state(), None);

        let without_id = parse(serde_json::json!({
            "event": "recipient-completed",
            "data": { "envelopeId": "e1" }
        }));
        assert_eq!(without_id.completed_recipient(), None);

        // The last signer's recipient event can carry the envelope's own
        // completed summary; envelope completion wins.
        let last = parse(serde_json::json!({
            "event": "recipient-completed",
            "data": {
                "envelopeId": "e1",
                "recipientId": "2",
                "envelopeSummary": { "status": "completed" }
            }
        }));
        assert_eq!(last.completed_recipient(), None);
        assert_eq!(
            last.terminal_state(),
            Some(store::signatures::SignatureState::Completed)
        );

        let envelope = parse(serde_json::json!({
            "event": "envelope-completed",
            "data": { "envelopeId": "e1", "recipientId": "2" }
        }));
        assert_eq!(envelope.completed_recipient(), None);
    }

    #[test]
    fn intermediate_events_classify_as_neither() {
        for ev in ["envelope-sent", "envelope-delivered", "recipient-completed"] {
            let p = parse(serde_json::json!({ "event": ev, "data": { "envelopeId": "e1" } }));
            assert!(!p.is_completed(), "{ev} is not completed");
            assert!(!p.is_declined(), "{ev} is not declined");
        }
    }
}

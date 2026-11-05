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
//!
//! ## Journal first, consume second (ENG-787 phases 2–4)
//!
//! A verified terminal-event receipt is journaled BEFORE it is consumed,
//! and consumption runs through [`consume_receipt`] — the same function
//! [`replay_pending_receipts`] (the boot-time sweep) runs. A crash
//! mid-delivery leaves the receipt pending in the journal, and the next
//! boot recovers the event from the journal instead of losing it:
//! delivery is replayable, not best-effort. A consumption phase that
//! fails records the failure on the receipt and answers 5xx so the
//! provider redelivers; a redelivery of an already-consumed event is
//! answered from the journal with 200, and the workflow is never
//! signaled twice — every phase checks the last durable state and
//! re-runs only the unfinished tail. Only verified payloads are
//! journaled: HMAC verification refuses an unverified delivery before
//! any write, and a refusal is not an event.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;

use workflows::runtime::WorkflowRuntimeError;
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

/// The webhook's two gates, in order: the coarse path secret, then the
/// HMAC over the raw body. Verified before any parse, so the digest
/// covers the exact bytes that name the envelope.
fn verify_webhook_request(
    state: &crate::AppState,
    provided: &str,
    headers: &HeaderMap,
    body: &Bytes,
) -> Result<(), WebhookError> {
    // 1. Path secret (coarse). Constant-time when configured.
    if let Some(configured) = state.esignature_webhook_secret.as_deref() {
        if !constant_time_eq(provided, configured) {
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
        if !verify_hmac_sha256_b64(key.as_bytes(), body, provided_sig) {
            tracing::warn!("esignature webhook: signature verification failed");
            return Err(WebhookError::UnauthorizedSignature);
        }
    }
    Ok(())
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
    verify_webhook_request(&state, &provided, &headers, &body)?;
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
    // Resolve the envelope to its signature row first, unchanged: an
    // unknown envelope id is a logged 200 no-op. This happens BEFORE the
    // journal write on purpose — a receipt for an envelope the signature
    // table has never named cannot be replayed (correlation is the
    // signature row), so journaling it would be an unrecoverable row.
    // A terminal event cannot precede the send that records that row.
    let signature_provider = store::signatures::SignatureProvider::DocuSign;
    let Some(signature) = store::signatures::by_provider(
        &state.surreal,
        signature_provider,
        &payload.data.envelope_id,
    )
    .await
    .map_err(|e| WebhookError::Database(e.to_string()))?
    else {
        tracing::warn!(
            envelope_id = %payload.data.envelope_id,
            "esignature webhook: no signature for envelope id"
        );
        return Ok(StatusCode::OK);
    };

    // Journal the verified receipt BEFORE consuming it: the journal is
    // the delivery record. A crash after this write leaves the event
    // recoverable by the boot-time sweep, not lost. A database failure
    // here answers 500 while the provider still holds the event —
    // nothing is lost before the receipt exists.
    let receipt = store::signature_webhook_journal::journal_verified_receipt(
        &state.surreal,
        signature_provider,
        &payload.data.envelope_id,
        terminal_state.as_str(),
        &String::from_utf8_lossy(&body),
    )
    .await
    .map_err(|e| WebhookError::Database(e.to_string()))?;

    // A redelivery of an already-consumed event is answered from the
    // journal — the workflow is not signaled twice.
    if receipt.consumed_at.is_some() {
        tracing::info!(
            envelope_id = %payload.data.envelope_id,
            receipt_id = %receipt.id,
            "esignature webhook: event already consumed (answered from the journal)"
        );
        return Ok(StatusCode::OK);
    }

    // Consume through the same function the boot-time replay sweep runs.
    // A failure records on the receipt and answers 5xx so the provider
    // redelivers; the sweep retries regardless.
    match consume_receipt(&state, &signature, terminal_state).await {
        Ok(()) => {
            store::signature_webhook_journal::mark_consumed(&state.surreal, receipt.id)
                .await
                .map_err(|e| WebhookError::Database(e.to_string()))?;
            tracing::info!(
                envelope_id = %payload.data.envelope_id,
                receipt_id = %receipt.id,
                state = terminal_state.as_str(),
                "esignature webhook: receipt consumed"
            );
            Ok(StatusCode::OK)
        }
        Err(failure) => {
            tracing::error!(
                envelope_id = %payload.data.envelope_id,
                receipt_id = %receipt.id,
                error = %failure,
                "esignature webhook: consumption failed; receipt stays pending for replay"
            );
            if let Err(e) = store::signature_webhook_journal::record_attempt_failure(
                &state.surreal,
                receipt.id,
                &failure.to_string(),
            )
            .await
            {
                tracing::error!(
                    receipt_id = %receipt.id,
                    error = %e,
                    "esignature webhook: recording the consumption failure on the receipt failed"
                );
            }
            Err(WebhookError::Runtime(failure.to_string()))
        }
    }
}

/// A consumption attempt that did not complete. Retryable is the only
/// kind: everything that can fail mid-consumption is worth a provider
/// redelivery (a 5xx) and a sweep pass. Dead ends — an envelope the
/// signature table never named, a payload naming no declared signer —
/// are resolved before consumption and answered as no-ops, the handler's
/// idempotent semantics.
#[derive(Debug, thiserror::Error)]
#[error("webhook consumption failed, receipt stays pending for replay: {0}")]
struct ConsumeFailure(String);

/// One consumption attempt for a verified terminal-event receipt: advance
/// the workflow wait, mirror the notation row, stamp the signature row,
/// and — for a completed envelope — archive the executed document set.
/// The webhook handler and the boot-time replay sweep both call this, so
/// recovery re-runs exactly what first delivery ran.
///
/// Resumable, not blind-reentrant: every phase checks whether the last
/// durable state already reflects it, so a replay after a partial
/// consumption re-runs only the unfinished tail — the wait is signaled
/// once, the stamp lands once, the archive is an idempotent re-put.
/// Re-running the loop resumes; it does not duplicate.
async fn consume_receipt(
    state: &crate::AppState,
    signature: &store::signatures::Signature,
    terminal_state: store::signatures::SignatureState,
) -> Result<(), ConsumeFailure> {
    let provider = store::signatures::SignatureProvider::DocuSign;
    let envelope_id = &signature.provider_id;
    let notation_id = signature.notation_id;

    // Phase 1: the workflow wait, when the signature row still reads
    // `requested`. A gone instance (`UnknownNotation`) or one already
    // past the wait (`AlreadyTerminal`, `NoTransition` on one of these
    // fixed conditions) has consumed this signal — the tail below is
    // what a replay re-runs.
    if signature.state == store::signatures::SignatureState::Requested {
        let condition = if terminal_state == store::signatures::SignatureState::Completed {
            SIGNATURE_RECEIVED
        } else {
            SIGNATURE_DECLINED
        };
        match StateMachineRuntime::signal(
            state.workflow_runtime.as_ref(),
            MachineKind::Workflow,
            notation_id,
            condition,
            None,
        )
        .await
        {
            Ok(next) => {
                // Mirror the workflow state into the `notations` row (the
                // same pattern the retainer walk uses) so the admin UI
                // reflects the end.
                store::notations::update_state(&state.surreal, notation_id, next.as_str())
                    .await
                    .map_err(|e| ConsumeFailure(format!("mirroring notation state: {e}")))?;
                tracing::info!(
                    %envelope_id,
                    %condition,
                    next_state = %next.as_str(),
                    "esignature webhook: signature event"
                );
            }
            Err(
                error @ (WorkflowRuntimeError::UnknownNotation(_)
                | WorkflowRuntimeError::AlreadyTerminal(_)
                | WorkflowRuntimeError::NoTransition { .. }),
            ) => {
                tracing::info!(
                    %envelope_id,
                    error = %error,
                    "esignature webhook: workflow wait already consumed; resuming the tail"
                );
            }
            Err(error) => return Err(ConsumeFailure(format!("workflow signal: {error}"))),
        }
    }

    // Phase 2: stamp the signature row with its terminal outcome. The
    // stamps are the idempotence authority — each re-reads the row and a
    // missing envelope or an already-terminal row is a no-op — so a
    // replay after a partial consumption cannot re-stamp or flip a
    // terminal row.
    let stamped = match terminal_state {
        store::signatures::SignatureState::Completed => {
            store::signatures::stamp_signed(
                &state.surreal,
                provider,
                envelope_id,
                &chrono::Utc::now().to_rfc3339(),
            )
            .await
        }
        store::signatures::SignatureState::Declined => {
            store::signatures::stamp_declined(&state.surreal, provider, envelope_id).await
        }
        store::signatures::SignatureState::Voided => {
            store::signatures::stamp_voided(&state.surreal, provider, envelope_id).await
        }
        store::signatures::SignatureState::Expired => {
            store::signatures::stamp_expired(&state.surreal, provider, envelope_id).await
        }
        // `terminal_state` never yields `requested` (only terminal events
        // are consumed); the no-op keeps the match exhaustive.
        store::signatures::SignatureState::Requested => Ok(false),
    };
    stamped.map_err(|e| ConsumeFailure(format!("stamping the signature: {e}")))?;

    // Phase 3: on completion, archive the executed document set (signed
    // PDF + Certificate of Completion — the ESIGN record) to object
    // storage. Always re-run: the put is idempotent, so a replay whose
    // retryable consumption failure — the ESIGN record is not optional,
    // and the journal is how it stops being best-effort.
    if terminal_state == store::signatures::SignatureState::Completed {
        archive_completed_documents(state, envelope_id, notation_id)
            .await
            .map_err(|e| ConsumeFailure(format!("archiving completed documents: {e}")))?;
    }
    Ok(())
}

/// The boot-time replay sweep: drain the journal's pending receipts,
/// oldest first, through the same [`consume_receipt`] the handler used.
/// Bounded by `limit` so a restart with a large backlog converges over
/// several boots instead of holding the boot host; never fatal — a sweep
/// that cannot run must not block serving.
///
/// # Errors
///
/// [`WebhookError::Database`] when the pending read itself fails. Per-
/// receipt failures are recorded on the receipt, not raised: one bad
/// receipt never stops the rest of the backlog.
pub async fn replay_pending_receipts(
    state: &crate::AppState,
    limit: i64,
) -> Result<(), WebhookError> {
    let pending = store::signature_webhook_journal::pending_receipts(&state.surreal, limit)
        .await
        .map_err(|e| WebhookError::Database(e.to_string()))?;
    if pending.is_empty() {
        return Ok(());
    }
    tracing::info!(
        count = pending.len(),
        limit,
        "esignature webhook journal: replaying pending receipts"
    );
    for receipt in pending {
        replay_one(state, &receipt).await;
    }
    Ok(())
}

/// Replay one pending receipt. Correlation (the signature row) is
/// resolved again at replay time — the row may have landed since the
/// delivery — and the payload re-classifies from its own bytes, so the
/// sweep re-runs the event, never a memory of it.
async fn replay_one(
    state: &crate::AppState,
    receipt: &store::signature_webhook_journal::WebhookReceipt,
) {
    let Some(provider) = store::signatures::SignatureProvider::from_stored(&receipt.provider)
    else {
        record_replay_failure(
            state,
            receipt,
            &format!("receipt names an unknown provider {:?}", receipt.provider),
        )
        .await;
        return;
    };
    let signature = match store::signatures::by_provider(
        &state.surreal,
        provider,
        &receipt.provider_id,
    )
    .await
    {
        Ok(Some(signature)) => signature,
        Ok(None) => {
            record_replay_failure(state, receipt, "no signature row names this envelope yet").await;
            return;
        }
        Err(e) => {
            tracing::error!(
                receipt_id = %receipt.id,
                error = %e,
                "esignature webhook journal: correlating a pending receipt failed"
            );
            return;
        }
    };
    let payload: ConnectPayload = match serde_json::from_str(&receipt.payload) {
        Ok(payload) => payload,
        Err(e) => {
            record_replay_failure(
                state,
                receipt,
                &format!("stored payload no longer parses: {e}"),
            )
            .await;
            return;
        }
    };
    let Some(terminal_state) = payload.terminal_state() else {
        // A receipt whose payload carries no terminal event has nothing
        // to consume — acknowledged, and marked so the sweep stops
        // seeing it. Only terminal events are journaled today; this is
        // the honest default if that ever changes.
        match store::signature_webhook_journal::mark_consumed(&state.surreal, receipt.id).await {
            Ok(()) => {}
            Err(e) => {
                tracing::error!(receipt_id = %receipt.id, error = %e, "esignature webhook journal: marking a non-terminal receipt consumed failed");
            }
        }
        return;
    };
    match consume_receipt(state, &signature, terminal_state).await {
        Ok(()) => {
            match store::signature_webhook_journal::mark_consumed(&state.surreal, receipt.id).await
            {
                Ok(()) => tracing::info!(
                    receipt_id = %receipt.id,
                    envelope_id = %receipt.provider_id,
                    state = terminal_state.as_str(),
                    "esignature webhook journal: pending receipt recovered"
                ),
                Err(e) => tracing::error!(
                    receipt_id = %receipt.id,
                    error = %e,
                    "esignature webhook journal: consumption recovered but marking consumed failed"
                ),
            }
        }
        Err(failure) => record_replay_failure(state, receipt, &failure.to_string()).await,
    }
}

/// Record a replay failure on the receipt and log it: the row stays
/// pending with the failure named, so the next sweep retries it and the
/// journal reports what happened.
async fn record_replay_failure(
    state: &crate::AppState,
    receipt: &store::signature_webhook_journal::WebhookReceipt,
    reason: &str,
) {
    tracing::error!(
        receipt_id = %receipt.id,
        envelope_id = %receipt.provider_id,
        reason,
        "esignature webhook journal: replay failed; receipt stays pending"
    );
    if let Err(e) =
        store::signature_webhook_journal::record_attempt_failure(&state.surreal, receipt.id, reason)
            .await
    {
        tracing::error!(
            receipt_id = %receipt.id,
            error = %e,
            "esignature webhook journal: recording the replay failure failed"
        );
    }
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

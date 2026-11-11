//! Inbound Forgejo push webhook.
//!
//! Forgejo calls this endpoint when a push lands on a repository it
//! hosts — for a jj Project, a `push/<change-id>` bookmark reaching the
//! Git backend. This module is the push pipeline's receiver: verify the
//! delivery, journal it, and hand it to the consumption tail (per-change
//! analysis, then Slack delivery — behind [`PushConsumer`]) without ever
//! losing a verified event.
//!
//! ## Trust model
//!
//! This endpoint feeds the pipeline that will notify a Firm's Slack of
//! Project changes, so it must not be trusted on URL alone. Two layers
//! gate it, the same two the e-signature receiver
//! ([`crate::esignature_webhook`]) established:
//!
//! - **Path secret** (`/webhook/forgejo/:secret`) — coarse "is this our
//!   endpoint". Optional defense-in-depth; `None` in dev/tests accepts
//!   any token.
//! - **Hex HMAC-SHA256 over the raw body** (`X-Forgejo-Signature`) —
//!   the real gate. Verified *before* the JSON is parsed, over the exact
//!   bytes received, so the ref update the body later names is
//!   trustworthy. When `forgejo_webhook_hmac_key` is configured the
//!   header must be present and valid or the request is rejected (fail
//!   closed). `None` in dev/tests skips it.
//!
//! ## Journal first, consume second
//!
//! A verified delivery is journaled BEFORE it is consumed — into
//! `store::postgres::forgejo_push_webhook_journal` — and consumption runs
//! through [`consume_receipt`], the same function [`replay_pending_receipts`]
//! (the boot-time sweep) runs. A crash mid-consumption leaves the receipt
//! `pending` in the journal, and the next boot recovers the event instead
//! of losing it. A consumption phase that fails records the failure on the
//! receipt and answers 5xx so Forgejo redelivers; a redelivery of an
//! already-consumed event is answered from the journal with 200 and
//! re-runs nothing — every phase checks the last durable state and
//! re-runs only the unfinished tail. Only verified payloads are
//! journaled: HMAC verification refuses an unverified delivery before any
//! write, and a refusal is not an event.
//!
//! A delivery whose payload names no `push/*` ref update — Forgejo also
//! fires pushes of ordinary branches, and the ping handshake — is
//! journaled with status `skipped`: observable, never analyzed.
//! Deduplication is content-based (the journal's event key), because a
//! Forgejo redelivery re-sends the same body under a new delivery id.

use async_trait::async_trait;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;

use store::postgres::forgejo_push_webhook_journal::{self, ForgejoPushReceipt, NewPushReceipt};
use store::postgres::PostgresDb;

use crate::inbound_email::constant_time_eq;
use crate::webhook_auth::verify_hmac_sha256_hex;

/// The header Forgejo signs its deliveries with: the HMAC-SHA256 digest
/// over the raw body, hex-encoded.
const SIGNATURE_HEADER: &str = "x-forgejo-signature";

/// The ref namespace a jj push creates: one bookmark per change,
/// `push/<change-id>` (cli's `PUSH_BOOKMARK_TEMPLATE`), addressed by the
/// Git backend as `refs/heads/push/<change-id>`. Only updates on this
/// namespace are analyzed and delivered; everything else is skipped.
const PUSH_REF_PREFIX: &str = "refs/heads/push/";

#[derive(Debug, thiserror::Error)]
pub enum WebhookError {
    #[error("unauthorized: webhook path secret mismatch")]
    UnauthorizedPath,
    #[error("unauthorized: missing or invalid signature")]
    UnauthorizedSignature,
    #[error("malformed push payload: {0}")]
    Malformed(String),
    /// The journal store is not configured. A verified delivery is
    /// never dropped on this: 503 makes Forgejo redeliver it once the
    /// deployment has its PostgreSQL lane.
    #[error("forgejo push journal unavailable: the deployment has no PostgreSQL store configured")]
    Unavailable,
    #[error("database: {0}")]
    Database(String),
    #[error("consumption failed: {0}")]
    Runtime(String),
}

impl IntoResponse for WebhookError {
    fn into_response(self) -> axum::response::Response {
        let code = match &self {
            Self::UnauthorizedPath | Self::UnauthorizedSignature => StatusCode::UNAUTHORIZED,
            Self::Malformed(_) => StatusCode::BAD_REQUEST,
            Self::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::Runtime(_) | Self::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (code, self.to_string()).into_response()
    }
}

/// Tolerant subset of Forgejo's push payload — we read only the pushed
/// ref and the repository it belongs to. Other Forgejo event types
/// (issues, ping, …) either name no `ref` or name one outside the push
/// namespace, and both classify as [`PushPayload::names_a_push_ref`] =
/// `false`.
#[derive(serde::Deserialize)]
struct PushPayload {
    /// The pushed ref — `refs/heads/push/<change-id>` for a jj push.
    #[serde(rename = "ref", default)]
    ref_name: Option<String>,
    #[serde(default)]
    before: Option<String>,
    #[serde(default)]
    after: Option<String>,
    #[serde(default)]
    repository: Option<PushRepository>,
}

#[derive(serde::Deserialize)]
struct PushRepository {
    #[serde(default)]
    full_name: Option<String>,
}

impl PushPayload {
    /// True when the delivery updates a ref in the `push/*` namespace —
    /// the only deliveries the pipeline analyzes.
    fn names_a_push_ref(&self) -> bool {
        self.ref_name
            .as_deref()
            .is_some_and(|r| r.starts_with(PUSH_REF_PREFIX))
    }

    /// The journal's `ref_updates` shape: an array of `{ref, before,
    /// after}` updates. A Forgejo push carries exactly one at the top
    /// level, so the receiver normalizes it into that array — the shape
    /// the journal, the analysis, and the replay sweep all share.
    fn ref_updates(&self) -> serde_json::Value {
        match &self.ref_name {
            Some(ref_name) => serde_json::json!([{
                "ref": ref_name,
                "before": self.before,
                "after": self.after,
            }]),
            None => serde_json::json!([]),
        }
    }
}

/// The webhook's two gates, in order: the coarse path secret, then the
/// hex HMAC over the raw body. Verified before any parse, so the digest
/// covers the exact bytes that name the ref update.
fn verify_webhook_request(
    state: &crate::AppState,
    provided: &str,
    headers: &HeaderMap,
    body: &Bytes,
) -> Result<(), WebhookError> {
    // 1. Path secret (coarse). Constant-time when configured.
    if let Some(configured) = state.forgejo_webhook_secret.as_deref() {
        if !constant_time_eq(provided, configured) {
            tracing::warn!("forgejo webhook: path secret mismatch");
            return Err(WebhookError::UnauthorizedPath);
        }
    }

    // 2. Hex HMAC over the raw body (the real gate). Verify BEFORE
    //    parsing so the digest covers the exact bytes that name the
    //    pushed ref.
    if let Some(key) = state.forgejo_webhook_hmac_key.as_deref() {
        let provided_sig = headers
            .get(SIGNATURE_HEADER)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                tracing::warn!("forgejo webhook: signature header absent");
                WebhookError::UnauthorizedSignature
            })?;
        if !verify_hmac_sha256_hex(key.as_bytes(), body, provided_sig) {
            tracing::warn!("forgejo webhook: signature verification failed");
            return Err(WebhookError::UnauthorizedSignature);
        }
    }
    Ok(())
}

/// What a verified push receipt flows into after the journal write: the
/// push pipeline's consumption tail — per-change analysis read through
/// the Forgejo API, then Slack delivery. The receiver owns the
/// journal-first durability around this seam and knows nothing of what
/// the tail does.
#[async_trait]
pub trait PushConsumer: Send + Sync {
    /// Consume one verified, journaled push receipt. Idempotent is the
    /// contract: a replay after a partial consumption re-runs the tail,
    /// so a consumer must check the last durable state it made rather
    /// than duplicate it. A `Err` names a retryable failure — the
    /// receipt stays re-consumable and Forgejo (or the boot sweep)
    /// redelivers it.
    async fn consume(&self, receipt: &ForgejoPushReceipt) -> Result<(), String>;
}

/// The consumer tests default to when they do not inject a tail: it
/// succeeds immediately, so a verified push is journaled, marked
/// consumed, and a redelivery re-runs nothing. Production mounts
/// [`crate::forgejo_push_pipeline::JjPushPipeline`] — only that
/// construction changes, no receiver code.
pub struct UnwiredPushConsumer;

#[async_trait]
impl PushConsumer for UnwiredPushConsumer {
    async fn consume(&self, _receipt: &ForgejoPushReceipt) -> Result<(), String> {
        Ok(())
    }
}

/// A consumption attempt that did not complete. Retryable is the only
/// kind: a failure leaves the receipt re-consumable, worth a Forgejo
/// redelivery (the 5xx) and a sweep pass. Dead ends — a payload naming
/// no repository — are resolved before journaling, not here.
#[derive(Debug, thiserror::Error)]
#[error("push consumption failed, receipt stays pending for replay: {0}")]
struct ConsumeFailure(String);

impl From<String> for ConsumeFailure {
    fn from(error: String) -> Self {
        Self(error)
    }
}

/// One consumption attempt for a verified push receipt: hand it to the
/// pipeline's consumer, then mark it delivered through the journal's
/// guarded transition. The webhook handler and the boot-time replay
/// sweep both call this, so recovery re-runs exactly what first delivery
/// ran; a replay whose consumer already did the work re-runs only the
/// unfinished tail.
async fn consume_receipt(
    state: &crate::AppState,
    store: &PostgresDb,
    receipt: &ForgejoPushReceipt,
) -> Result<(), ConsumeFailure> {
    state
        .forgejo_push_consumer
        .consume(receipt)
        .await
        .map_err(ConsumeFailure::from)?;

    // `None` answers only when the row is already terminal or gone — a
    // concurrent redelivery won that transition. Nothing left to do.
    forgejo_push_webhook_journal::mark_delivered(store, receipt.id)
        .await
        .map_err(|e| ConsumeFailure(format!("marking the receipt delivered: {e}")))?;
    Ok(())
}

/// Webhook handler. Verifies the path secret, then the raw-body hex
/// HMAC, then journals the verified delivery and consumes it — or
/// records it `skipped` when it names no `push/*` ref update.
pub async fn webhook(
    State(state): State<crate::AppState>,
    Path(provided): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, WebhookError> {
    verify_webhook_request(&state, &provided, &headers, &body)?;

    // A verified but unparseable body is the forge misbehaving, not an
    // event: refuse with 400, having written nothing.
    let payload: PushPayload =
        serde_json::from_slice(&body).map_err(|e| WebhookError::Malformed(e.to_string()))?;

    // An event that names no repository cannot be keyed, journaled, or
    // replayed — Forgejo's `ping` handshake, for instance. Ack so the
    // forge stops retrying; nothing about a Project was said.
    let Some(full_name) = payload
        .repository
        .as_ref()
        .and_then(|r| r.full_name.as_deref())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
    else {
        tracing::info!("forgejo webhook: event names no repository; acknowledged");
        return Ok(StatusCode::OK);
    };

    // The journal is the delivery record. Without it the receiver cannot
    // honor journal-first, so the verified delivery is refused — 503
    // makes Forgejo redeliver once the store exists; nothing is lost.
    let Some(store) = state.forgejo_push_store.as_ref() else {
        tracing::error!(
            repo = %full_name,
            "forgejo webhook: no push journal configured; refusing a verified delivery"
        );
        return Err(WebhookError::Unavailable);
    };

    // Journal the verified receipt BEFORE consuming it. A crash after
    // this write leaves the event recoverable by the boot-time sweep,
    // not lost. A database failure here answers 500 while Forgejo still
    // holds the event — nothing is lost before the receipt exists.
    let receipt = forgejo_push_webhook_journal::journal_verified_push(
        store,
        &NewPushReceipt {
            delivered_at: chrono::Utc::now(),
            repo_full_name: full_name,
            ref_updates: payload.ref_updates(),
            payload: String::from_utf8_lossy(&body).into_owned(),
        },
    )
    .await
    .map_err(|e| WebhookError::Database(e.to_string()))?;

    // A redelivery of an already-consumed event is answered from the
    // journal — the pipeline is not signaled twice.
    if receipt.status.is_terminal() {
        tracing::info!(
            receipt_id = %receipt.id,
            repo = %receipt.repo_full_name,
            status = receipt.status.as_str(),
            "forgejo webhook: event already consumed (answered from the journal)"
        );
        return Ok(StatusCode::OK);
    }

    // A delivery that names no push/* ref update is observable, never
    // analyzed. Marked skipped from the journal — a redelivery of it is
    // terminal and re-runs nothing.
    if !payload.names_a_push_ref() {
        match forgejo_push_webhook_journal::mark_skipped(store, receipt.id).await {
            Ok(_) => tracing::info!(
                receipt_id = %receipt.id,
                repo = %receipt.repo_full_name,
                "forgejo webhook: no push/* ref update; receipt skipped"
            ),
            Err(e) => {
                tracing::error!(
                    receipt_id = %receipt.id,
                    error = %e,
                    "forgejo webhook: marking a non-push receipt skipped failed"
                );
                return Err(WebhookError::Database(e.to_string()));
            }
        }
        return Ok(StatusCode::OK);
    }

    // Consume through the same function the boot-time replay sweep runs.
    // A failure records on the receipt and answers 5xx so Forgejo
    // redelivers; the sweep retries regardless.
    match consume_receipt(&state, store, &receipt).await {
        Ok(()) => {
            tracing::info!(
                receipt_id = %receipt.id,
                repo = %receipt.repo_full_name,
                "forgejo webhook: push receipt consumed"
            );
            Ok(StatusCode::OK)
        }
        Err(failure) => {
            tracing::error!(
                receipt_id = %receipt.id,
                repo = %receipt.repo_full_name,
                error = %failure,
                "forgejo webhook: consumption failed; receipt stays pending for replay"
            );
            record_failure(store, receipt.id, &failure.to_string()).await;
            Err(WebhookError::Runtime(failure.to_string()))
        }
    }
}

/// Record a replay failure on the receipt and log it: the row stays
/// re-consumable with the failure named, so the next sweep retries it
/// and the journal reports what happened. Used by both the handler's
/// failed consumption and the sweep.
async fn record_failure(store: &PostgresDb, receipt_id: uuid::Uuid, reason: &str) {
    if let Err(e) = forgejo_push_webhook_journal::record_failure(store, receipt_id, reason).await {
        tracing::error!(
            receipt_id = %receipt_id,
            error = %e,
            "forgejo webhook journal: recording the consumption failure failed"
        );
    }
}

/// The boot-time replay sweep: drain the journal's re-consumable
/// receipts — `pending` or `failed` — oldest first, through the same
/// [`consume_receipt`] the handler used. Bounded by `limit` so a restart
/// with a large backlog converges over several boots instead of holding
/// the boot host; never fatal — a sweep that cannot run must not block
/// serving. Without a configured store there is no journal to sweep.
///
/// # Errors
///
/// [`WebhookError::Database`] when the re-consumable read itself fails.
/// Per-receipt failures are recorded on the receipt, not raised: one bad
/// receipt never stops the rest of the backlog.
pub async fn replay_pending_receipts(
    state: &crate::AppState,
    limit: i64,
) -> Result<(), WebhookError> {
    let Some(store) = state.forgejo_push_store.as_ref() else {
        return Ok(());
    };
    let pending = forgejo_push_webhook_journal::reconsumable_receipts(store, limit)
        .await
        .map_err(|e| WebhookError::Database(e.to_string()))?;
    if pending.is_empty() {
        return Ok(());
    }
    tracing::info!(
        count = pending.len(),
        limit,
        "forgejo push webhook journal: replaying re-consumable receipts"
    );
    for receipt in pending {
        replay_one(state, store, &receipt).await;
    }
    Ok(())
}

/// Replay one re-consumable receipt. The payload re-classifies from its
/// own stored bytes — the sweep re-runs the event, never a memory of it
/// — so a receipt journaled before a skip-classification fix is skipped
/// now, and one whose delivery was finished is left alone.
async fn replay_one(state: &crate::AppState, store: &PostgresDb, receipt: &ForgejoPushReceipt) {
    let payload: PushPayload = match serde_json::from_str(&receipt.payload) {
        Ok(payload) => payload,
        Err(e) => {
            record_failure(
                store,
                receipt.id,
                &format!("stored payload no longer parses: {e}"),
            )
            .await;
            return;
        }
    };
    if !payload.names_a_push_ref() {
        // Nothing to consume — the delivery names no push/* ref update.
        // Marked skipped so the sweep stops seeing it; only push
        // deliveries are analyzed today, and this is the honest default
        // if that ever changes.
        match forgejo_push_webhook_journal::mark_skipped(store, receipt.id).await {
            Ok(_) => tracing::info!(
                receipt_id = %receipt.id,
                "forgejo push webhook journal: non-push receipt marked skipped at replay"
            ),
            Err(e) => tracing::error!(
                receipt_id = %receipt.id,
                error = %e,
                "forgejo push webhook journal: marking a non-push receipt skipped failed"
            ),
        }
        return;
    }
    match consume_receipt(state, store, receipt).await {
        Ok(()) => tracing::info!(
            receipt_id = %receipt.id,
            repo = %receipt.repo_full_name,
            "forgejo push webhook journal: receipt recovered"
        ),
        Err(failure) => {
            tracing::error!(
                receipt_id = %receipt.id,
                error = %failure,
                "forgejo push webhook journal: replay failed; receipt stays re-consumable"
            );
            record_failure(store, receipt.id, &failure.to_string()).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PushPayload, PUSH_REF_PREFIX};

    fn parse(json: serde_json::Value) -> PushPayload {
        serde_json::from_value(json).expect("forgejo push payload parses")
    }

    /// The payload Forgejo sends for a jj push: one top-level ref update
    /// naming a `push/<change-id>` bookmark.
    fn jj_push_payload() -> serde_json::Value {
        serde_json::json!({
            "ref": "refs/heads/push/qvrpxyq",
            "before": "0000000000000000000000000000000000000000",
            "after": "9f3ba2c7114e0d5689c0b1a2f4e5d6c7b8a9f0e1",
            "total_commits": 1,
            "commits": [ { "id": "9f3ba2c7" } ],
            "repository": { "id": 7, "full_name": "firm-slug/project-code" },
            "pusher": { "login": "tbot" }
        })
    }

    #[test]
    fn a_jj_push_names_a_push_ref() {
        let payload = parse(jj_push_payload());
        assert!(payload.names_a_push_ref());
        assert_eq!(
            payload.ref_updates(),
            serde_json::json!([{
                "ref": "refs/heads/push/qvrpxyq",
                "before": "0000000000000000000000000000000000000000",
                "after": "9f3ba2c7114e0d5689c0b1a2f4e5d6c7b8a9f0e1"
            }])
        );
    }

    #[test]
    fn an_ordinary_branch_push_is_not_a_push_ref() {
        let payload = parse(serde_json::json!({
            "ref": "refs/heads/main",
            "repository": { "full_name": "firm-slug/project-code" }
        }));
        assert!(!payload.names_a_push_ref());
    }

    #[test]
    fn a_refless_event_names_no_push_ref() {
        // Forgejo's ping handshake carries neither a ref nor, reliably,
        // a repository.
        let payload = parse(serde_json::json!({ "zen": "nonvisualization", "hook_id": 7 }));
        assert!(!payload.names_a_push_ref());
        assert_eq!(payload.ref_updates(), serde_json::json!([]));
    }

    #[test]
    fn the_push_ref_prefix_matches_the_bookmark_namespace() {
        // The receiver and cli's PUSH_BOOKMARK_TEMPLATE must agree on
        // where a jj change lands; this pins the receiver's half.
        assert!(PUSH_REF_PREFIX.starts_with("refs/heads/push/"));
    }
}

//! Inbox-process doors.
//!
//! The process backlog read, the body-carried process door behind
//! `inbox process`, and the attachment door behind `inbox attachments`. The
//! route itself is classified server-side at apply time from the same
//! transport facts the SendGrid webhook sees — a client never posts a
//! route. The message body never leaves the server: plans carry
//! classification metadata and the attachment list only.

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::{ApiError, ApiState, LawyerSession};
use crate::inbound_email::classify_inbound_facts;
use crate::session::SessionData;

fn map_inbox(error: store::inbox::InboxError) -> ApiError {
    match error {
        store::inbox::InboxError::NotFound => ApiError::NotFound,
        store::inbox::InboxError::Denied => ApiError::Forbidden,
        other => ApiError::Db(other.to_string()),
    }
}

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, Weak};

/// Serialize local applies while store transactions and immutable source
/// identities provide cross-replica convergence. Weak entries retain no
/// completed message locks and are pruned on the next apply.
type ApplyLocks = Mutex<HashMap<Uuid, Weak<tokio::sync::Mutex<()>>>>;
static APPLY_LOCKS: LazyLock<ApplyLocks> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn message_lock(
    registry: &ApplyLocks,
    message: Uuid,
) -> Result<Arc<tokio::sync::Mutex<()>>, ApiError> {
    let mut locks = registry
        .lock()
        .map_err(|_| ApiError::Db("inbox apply-lock registry unavailable".into()))?;
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(&message).and_then(Weak::upgrade) {
        return Ok(lock);
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(message, Arc::downgrade(&lock));
    Ok(lock)
}

/// The process view: classification metadata for one inbox message, plus
/// what a previous process run recorded. Every door serves this same shape,
/// so the CLI prints one plan from either a backlog listing, a status read,
/// or an apply.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProcessView {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub channel: String,
    pub provider_message_id: Option<String>,
    pub external_address: String,
    pub subject: Option<String>,
    /// The route a fresh classification assigns right now.
    pub computed_route: &'static str,
    pub computed_label: &'static str,
    /// The exact marker the classifier matched.
    pub reason: String,
    /// The route a previous process run recorded, if any.
    pub recorded_route: Option<String>,
    pub recorded_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The NEF-pipeline outcome a previous run recorded, if any.
    pub recorded_nef_outcome: Option<String>,
    /// The matter the conversation is assigned to, for the plan's context.
    pub project_id: Option<Uuid>,
    pub occurred_at: String,
    pub inserted_at: chrono::DateTime<chrono::Utc>,
    /// The message's linked attachments, in provider order: what the
    /// attachment door serves and what an apply with a kind files.
    pub attachments: Vec<store::inbox::MessageAttachment>,
}

/// The attachments linked to one message, in provider order.
async fn linked_attachments(
    state: &ApiState,
    row: &store::inbox::UnprocessedRow,
) -> Result<Vec<store::inbox::MessageAttachment>, ApiError> {
    store::inbox::message_assets(&state.surreal, row.firm_id, row.id)
        .await
        .map_err(map_inbox)
}

fn view(
    row: store::inbox::UnprocessedRow,
    state: store::inbox::ProcessState,
    attachments: Vec<store::inbox::MessageAttachment>,
) -> ProcessView {
    let decision = classify_inbound_facts(
        &row.external_address,
        row.subject.as_deref().unwrap_or_default(),
        &row.body,
    );
    ProcessView {
        id: row.id,
        conversation_id: row.conversation_id,
        channel: row.channel,
        provider_message_id: row.provider_message_id,
        external_address: row.external_address,
        subject: row.subject,
        computed_route: decision.route.as_str(),
        computed_label: decision.route.label(),
        reason: decision.reason,
        recorded_route: state.route,
        recorded_at: state.processed_at,
        recorded_nef_outcome: state.nef_outcome,
        project_id: row.project_id,
        occurred_at: row.occurred_at,
        inserted_at: row.inserted_at,
        attachments,
    }
}

/// The acting person's id. A session without one cannot act on the store.
fn actor_id(session: &SessionData) -> Result<Uuid, ApiError> {
    session.person_id.ok_or(ApiError::NotFound)
}

/// `GET /app/api/inbox/unprocessed` — the backlog, oldest first.
pub(crate) async fn unprocessed(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
) -> Result<Response, ApiError> {
    let actor_id = actor_id(&session)?;
    let rows = store::inbox::unprocessed_inbox(&state.surreal, actor_id)
        .await
        .map_err(map_inbox)?;
    let mut views = Vec::with_capacity(rows.len());
    for row in rows {
        let saved = store::inbox::process_state(&state.surreal, row.id)
            .await
            .map_err(map_inbox)?
            .ok_or(ApiError::NotFound)?;
        let attachments = linked_attachments(&state, &row).await?;
        views.push(view(row, saved, attachments));
    }
    Ok(Json(views).into_response())
}

/// The body the process door reads: which message, and whether to record.
#[derive(Debug, Deserialize)]
pub(crate) struct ProcessRequest {
    /// The inbox message to plan or record.
    pub message: Uuid,
    /// False serves the plan only; true records the classification.
    #[serde(default)]
    pub apply: bool,
    /// Asset-lane kind an attributed message's attachments file under, as
    /// internal documents, on apply. Absent files none and reports how many
    /// wait: the kind is a decision, never a default. A court notice files
    /// through its own pipeline and ignores this.
    #[serde(default)]
    pub kind: Option<String>,
}

/// `POST /app/api/inbox/process` — the plan (apply: false) and the
/// recording (apply: true) behind `inbox process`, one constant-path door:
/// the parsed message id is body data, never URL material. The apply
/// classifies the message server-side from the same transport facts the
/// SendGrid webhook sees and records the route and processing time.
/// Idempotent: a re-run overwrites with the same truth and reports whether
/// the message was already recorded.
pub(crate) async fn process(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Json(request): Json<ProcessRequest>,
) -> Result<Response, ApiError> {
    let actor_id = actor_id(&session)?;
    let context = store::inbox::process_context(&state.surreal, actor_id, request.message)
        .await
        .map_err(map_inbox)?;
    if !request.apply {
        let attachments = linked_attachments(&state, &context.row).await?;
        return Ok(Json(view(context.row, context.state, attachments)).into_response());
    }
    let kind = request
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|kind| !kind.is_empty());
    if let Some(kind) = kind {
        let accepted = crate::api::accepted_asset_kinds();
        if !accepted.contains(&kind) {
            return Ok(crate::api::bad_request(
                "invalid_kind",
                &format!("Accepted values are: {}.", accepted.join(", ")),
            ));
        }
    }
    // Admit the caller before allocating a lock, then read the recorded
    // truth again after any preceding apply has finished.
    let _apply_guard = message_lock(&APPLY_LOCKS, request.message)?
        .lock_owned()
        .await;
    let context = store::inbox::process_context(&state.surreal, actor_id, request.message)
        .await
        .map_err(map_inbox)?;
    let decision = classify_inbound_facts(
        &context.row.external_address,
        context.row.subject.as_deref().unwrap_or_default(),
        &context.row.body,
    );
    let already_recorded = context.state.processed_at.is_some();
    let recorded_at = Utc::now();
    // LAW-171: a NEF-classified message on an attributed matter is not
    // merely routed — the pipeline files the notice onto the matter. Its
    // outcome rides beside the recording; a triage outcome (no matter, an
    // unparseable notice, no case number) keeps the message in the backlog
    // through the recorded `nef_outcome`.
    let nef_outcome = if decision.route == crate::inbound_email::InboxRoute::Nef {
        if let Some(project_id) = context.row.project_id {
            require_firm_side(&state, &session, project_id).await?;
        }
        let outcome = crate::nef_pipeline::process_nef(&state, &context.row).await?;
        let spelling = outcome.spelling();
        Some((outcome, spelling))
    } else {
        None
    };
    // Every other route on an attributed matter files the message's
    // attachments when the lawyer named their kind, and counts them when not.
    let linked = linked_attachments(&state, &context.row).await?;
    let attachments = apply_attachments(
        &state,
        &session,
        &context.row,
        decision.route,
        kind,
        linked.len(),
    )
    .await?;
    // LAW-166: apply always records the route and the processing time, so
    // a re-run reports idempotency instead of duplicating work.
    store::inbox::mark_processed(
        &state.surreal,
        request.message,
        decision.route.as_str(),
        recorded_at,
        nef_outcome.as_ref().map(|(_, spelling)| *spelling),
    )
    .await
    .map_err(map_inbox)?;
    let slack_notice = match context.row.project_id {
        Some(project_id) => notify_inbox_routed(&state, project_id).await,
        None => SlackNotice::NoProject,
    };
    Ok(Json(ProcessApplyView {
        result: view(
            context.row,
            store::inbox::ProcessState {
                route: Some(decision.route.as_str().to_owned()),
                processed_at: Some(recorded_at),
                nef_outcome: nef_outcome
                    .as_ref()
                    .map(|(_, spelling)| (*spelling).to_owned()),
            },
            linked,
        ),
        route: decision.route.as_str(),
        route_label: decision.route.label(),
        reason: decision.reason,
        recorded_at,
        already_recorded,
        nef: nef_outcome.map(|(outcome, _)| outcome),
        attachments,
        slack_notice,
    })
    .into_response())
}

/// The apply response: the recorded verdict plus the view it landed on.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProcessApplyView {
    /// The message's plan, with the recording already applied.
    pub result: ProcessView,
    /// The storage spelling that was recorded.
    pub route: &'static str,
    pub route_label: &'static str,
    pub reason: String,
    pub recorded_at: chrono::DateTime<chrono::Utc>,
    /// Whether a previous run had already recorded this message.
    pub already_recorded: bool,
    /// What the NEF pipeline filed, when the message classified as a
    /// CM/ECF notice (LAW-171). `None` for every other route.
    pub nef: Option<crate::nef_pipeline::NefOutcome>,
    /// What filed from an attributed, non-NEF message's attachments.
    pub attachments: AttachmentFiling,
    /// The best-effort Slack notice outcome.
    pub slack_notice: SlackNotice,
}

/// An apply's attachment outcome: parts filed as matter documents, and
/// parts left waiting because no kind was named.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(crate) struct AttachmentFiling {
    pub filed: usize,
    pub unfiled: usize,
}

/// Every route but a court notice, on an attributed matter, files the
/// message's attachments when the lawyer named their kind and counts them
/// when not. A court notice files through its own pipeline.
async fn apply_attachments(
    state: &ApiState,
    session: &SessionData,
    row: &store::inbox::UnprocessedRow,
    route: crate::inbound_email::InboxRoute,
    kind: Option<&str>,
    linked: usize,
) -> Result<AttachmentFiling, ApiError> {
    let Some(project_id) = row.project_id else {
        return Ok(AttachmentFiling::default());
    };
    if route == crate::inbound_email::InboxRoute::Nef {
        return Ok(AttachmentFiling::default());
    }
    require_firm_side(state, session, project_id).await?;
    Ok(match kind {
        Some(kind) => AttachmentFiling {
            filed: crate::nef_pipeline::file_message_attachments(state, row, project_id, kind)
                .await?,
            unfiled: 0,
        },
        None => AttachmentFiling {
            filed: 0,
            unfiled: linked,
        },
    })
}

/// The body the attachment door reads: which message, and which of its
/// linked attachments.
#[derive(Debug, Deserialize)]
pub(crate) struct AttachmentRequest {
    /// The inbox message that carries the attachment.
    pub message: Uuid,
    /// The attachment's asset id, as the message's plan lists it.
    pub asset: Uuid,
}

/// `POST /app/api/inbox/attachment` — one linked attachment's bytes, behind
/// `inbox attachments`. A message in triage has no matter, so authority
/// comes from the message itself: the process door's Firm scope, plus the
/// firm-side matter gate once the conversation is attributed. An asset the
/// message does not link is 404, the same as a missing message.
pub(crate) async fn attachment(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Json(request): Json<AttachmentRequest>,
) -> Result<Response, ApiError> {
    let actor_id = actor_id(&session)?;
    let row = store::inbox::process_context(&state.surreal, actor_id, request.message)
        .await
        .map_err(map_inbox)?
        .row;
    if let Some(project_id) = row.project_id {
        require_firm_side(&state, &session, project_id).await?;
    }
    let link = linked_attachments(&state, &row)
        .await?
        .into_iter()
        .find(|link| link.asset_id == request.asset)
        .ok_or(ApiError::NotFound)?;
    let asset = store::assets::find_by_id(&state.surreal, link.asset_id)
        .await
        .map_err(ApiError::Asset)?
        .ok_or(ApiError::NotFound)?;
    if asset
        .project_id
        .is_some_and(|id| Some(id) != row.project_id)
    {
        return Err(ApiError::NotFound);
    }
    let bytes = match state.storage.get(&asset.storage_key).await {
        Ok(object) => object.bytes,
        Err(cloud::StorageError::NotFound(_)) => return Err(ApiError::NotFound),
        Err(error) => return Err(ApiError::Db(error.to_string())),
    };
    Ok(crate::project_documents::attachment_bytes_response(
        bytes,
        &asset.content_type,
        &link.filename,
    ))
}

/// Out-of-scope for a matter that the caller cannot see firm-side: 404,
/// the same refusal every matter-scoped door gives.
async fn require_firm_side(
    state: &ApiState,
    session: &SessionData,
    project_id: Uuid,
) -> Result<(), ApiError> {
    let viewer =
        store::access::matter_viewer(&state.surreal, session.person_id, session.role, project_id)
            .await
            .map_err(ApiError::Db)?;
    if viewer.is_some_and(store::access::MatterViewer::is_firm_side) {
        Ok(())
    } else {
        Err(ApiError::NotFound)
    }
}

/// The best-effort Slack notice outcome. The recording succeeds whether or
/// not the notice lands; a notice failure never fails an apply. Like the
/// notify door, the notice is closed-vocabulary and mechanism-only —
/// `Navigator integration event: inbox_routed.` — never a subject, an
/// address, or a body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SlackNotice {
    /// Posted to the matter's Firm-private channel.
    Notified,
    /// The message belongs to no matter, so there is no channel to tell.
    NoProject,
    /// The matter has no recorded Firm-private channel.
    NoChannel,
    /// The Firm's Slack credential was missing or refused the call.
    ProviderUnavailable,
}

/// Tell the matter's Firm-private channel that a message was routed. The
/// matter channel is never provisioned here — a missing channel is
/// reported so an operator runs `slack ensure` deliberately.
async fn notify_inbox_routed(state: &ApiState, project_id: Uuid) -> SlackNotice {
    let project = match store::projects::find_by_id(&state.surreal, project_id).await {
        Ok(Some(project)) => project,
        Ok(None) => return SlackNotice::NoProject,
        Err(error) => {
            tracing::warn!(error = %error, "inbox process: read the matter for the Slack notice");
            return SlackNotice::ProviderUnavailable;
        }
    };
    let slack = match state
        .integration_providers
        .slack(&state.surreal, project_id)
        .await
    {
        Ok(slack) => slack,
        Err(error) => {
            tracing::warn!(error = %error, "inbox process: resolve the Firm Slack provider");
            return SlackNotice::ProviderUnavailable;
        }
    };
    let channel = match slack.find_private_channel(&project.code).await {
        Ok(Some(channel)) => channel,
        Ok(None) => return SlackNotice::NoChannel,
        Err(error) => {
            tracing::warn!(error = %error, "inbox process: find the Firm-private channel");
            return SlackNotice::ProviderUnavailable;
        }
    };
    match workflows::post_slack_notice(
        slack.as_ref(),
        &channel.id,
        workflows::SlackNoticeEvent::InboxRouted,
    )
    .await
    {
        Ok(()) => SlackNotice::Notified,
        Err(error) => {
            tracing::warn!(error = %error, "inbox process: post the Slack notice");
            SlackNotice::ProviderUnavailable
        }
    }
}

/// The body the assign-matches door reads: serve the proposals, or make them.
#[derive(Debug, Deserialize)]
pub(crate) struct AssignMatchesRequest {
    /// False lists what the address-match would assign; true assigns each
    /// uniquely matched thread.
    #[serde(default)]
    pub apply: bool,
}

/// One thread's address-match proposal: a plan lists every candidate
/// thread, an apply reports what it did.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AssignMatchView {
    pub message: Uuid,
    pub conversation_id: Uuid,
    pub from: String,
    pub subject: Option<String>,
    pub decision: store::inbox::assign_match::MatchOutcome,
    /// Whether this run assigned (apply: true) or only proposed (plan).
    pub applied: bool,
}

/// Decide one thread by LAW-347's address-match: the From/To/Cc candidate
/// set comes from the archived raw `.eml` when the message carries one,
/// falling back to the stored From; each candidate resolves against the
/// Person CRM, and its open-Project participations — same Firm as the
/// thread, `status = 'open'` — feed the pure matcher. Entities carry no
/// email field in the schema, so the candidate set is Persons today; when
/// Entities gain email storage they join `ResolvedAddress` unchanged.
async fn decide_assign_match(
    state: &ApiState,
    row: &store::inbox::UnprocessedRow,
) -> Result<store::inbox::assign_match::MatchOutcome, ApiError> {
    let stored_from = row.external_address.trim().to_owned();
    let mut candidates = vec![stored_from.clone()];
    if let Some(raw) = crate::nef_pipeline::raw_source(state, row).await? {
        let envelope = crate::inbound_email::envelope_addresses_from_raw(&raw);
        if !envelope.is_empty() {
            candidates.clear();
            candidates.extend(envelope);
            // The stored From always candidates: keep it first when the
            // envelope spells the mailbox differently.
            if candidates
                .iter()
                .all(|seen| !seen.eq_ignore_ascii_case(&stored_from))
            {
                candidates.insert(0, stored_from.clone());
            }
        }
    }
    let mut resolved = Vec::with_capacity(candidates.len());
    for address in &candidates {
        let person = store::persons::find_by_any_email(&state.surreal, address)
            .await
            .map_err(ApiError::Person)?;
        let open_projects = match &person {
            Some(person) => {
                let mut projects = Vec::new();
                for participation in
                    store::projects::participations_for_person(&state.surreal, person.id)
                        .await
                        .map_err(|error| ApiError::Db(error.to_string()))?
                {
                    let Some(project) =
                        store::projects::find_by_id(&state.surreal, participation.project_id)
                            .await
                            .map_err(|error| ApiError::Db(error.to_string()))?
                    else {
                        continue;
                    };
                    if project.status == "open" && project.firm_id == Some(row.firm_id) {
                        projects.push(participation.project_id);
                    }
                }
                projects
            }
            None => Vec::new(),
        };
        resolved.push(store::inbox::assign_match::ResolvedAddress {
            address: address.clone(),
            person_id: person.map(|person| person.id),
            open_projects,
        });
    }
    Ok(store::inbox::assign_match::decide(&stored_from, &resolved))
}

/// Every decision — assigned or left — logs the method, the Project it
/// matched (or none), and that an unmatched thread stayed in the inbox.
/// The entry's `model_id` and `probabilities` fields stay `None` here:
/// they belong to the TypeSafe/Jev method, which is unreachable until the
/// LAW-346 zero-data-retention gate confirms, and ride this same shape.
fn log_assign_decision(message: Uuid, outcome: &store::inbox::assign_match::MatchOutcome) {
    let entry = outcome.decision_log(message);
    tracing::info!(
        decision = %serde_json::to_string(&entry).expect("a decision log always serializes"),
        "inbox assignment decision"
    );
}

const MAX_ASSIGN_MATCH_THREADS: usize = 50;

/// A large backlog must not turn one door call into unbounded serial
/// round-trips against the client timeout; each run consumes a capped
/// slice oldest-first and the next run resumes where this one left off.
fn cap_run(mut rows: Vec<store::inbox::UnprocessedRow>) -> Vec<store::inbox::UnprocessedRow> {
    if rows.len() <= MAX_ASSIGN_MATCH_THREADS {
        return rows;
    }
    tracing::warn!(
        deferred = rows.len() - MAX_ASSIGN_MATCH_THREADS,
        cap = MAX_ASSIGN_MATCH_THREADS,
        "inbox assign-matches run capped oldest-first; re-run to continue"
    );
    rows.truncate(MAX_ASSIGN_MATCH_THREADS);
    rows
}

/// `POST /app/api/inbox/assign-matches` — the LAW-347 address-match door
/// behind `navigator inbox assign-matches`. Every unprocessed, unattributed
/// email thread proposes an assignment from its From/To/Cc addresses
/// (`store::inbox::assign_match`); apply: true makes each unique match
/// through the same `inbox assign` door the triage surface uses, so the
/// existing authorization, Firm, and billing gates all hold. Filing and
/// archiving stay where they live — the process door's thresholds and
/// confirmations — and nothing here touches them. Every decision logs
/// server-side; unmatched and ambiguous threads stay in the inbox.
pub(crate) async fn assign_matches(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Json(request): Json<AssignMatchesRequest>,
) -> Result<Response, ApiError> {
    let actor_id = actor_id(&session)?;
    let rows = cap_run(
        store::inbox::unprocessed_inbox(&state.surreal, actor_id)
            .await
            .map_err(map_inbox)?,
    );
    let mut views = Vec::new();
    for row in rows {
        // An already-attributed thread needs no match; a non-email channel
        // has no envelope addresses to read.
        if row.project_id.is_some() || row.channel != store::inbox::channel::EMAIL {
            continue;
        }
        let outcome = decide_assign_match(&state, &row).await?;
        log_assign_decision(row.id, &outcome);
        let applied =
            if let (true, store::inbox::assign_match::MatchOutcome::Assigned { project_id }) =
                (request.apply, &outcome)
            {
                store::inbox::assign(&state.surreal, actor_id, row.conversation_id, *project_id)
                    .await
                    .map_err(map_inbox)?;
                true
            } else {
                false
            };
        views.push(AssignMatchView {
            message: row.id,
            conversation_id: row.conversation_id,
            from: row.external_address,
            subject: row.subject,
            decision: outcome,
            applied,
        });
    }
    Ok(Json(views).into_response())
}

#[cfg(test)]
mod tests {
    use super::{message_lock, ApplyLocks};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use uuid::Uuid;

    #[test]
    fn apply_registry_reuses_active_locks_and_prunes_completed_messages() {
        let registry: ApplyLocks = Mutex::new(HashMap::new());
        let message = Uuid::now_v7();
        let first = message_lock(&registry, message).unwrap();
        let second = message_lock(&registry, message).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        let other = message_lock(&registry, Uuid::now_v7()).unwrap();
        assert!(!Arc::ptr_eq(&first, &other));
        drop(first);
        drop(second);
        drop(other);
        let _next = message_lock(&registry, Uuid::now_v7()).unwrap();
        assert_eq!(registry.lock().unwrap().len(), 1);
    }
}

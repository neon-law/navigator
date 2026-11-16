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
use std::collections::BTreeMap;
use uuid::Uuid;

use crate::api::{ApiError, ApiState, LawyerSession};
use crate::inbound_email::classify_inbound_facts;
use crate::session::SessionData;
use typesafe::{JevAssigner, JevOutcome};

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

/// The body the assign-matches preview reads.
#[derive(Debug, Deserialize)]
pub(crate) struct AssignMatchesRequest {
    /// Explicitly reject a write requested by an older client.
    #[serde(default)]
    pub apply: bool,
    pub after: Option<(chrono::DateTime<Utc>, Uuid)>,
}

/// One thread's address-match proposal.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AssignMatchView {
    pub message: Uuid,
    pub conversation_id: Uuid,
    pub from: String,
    pub subject: Option<String>,
    pub decision: store::inbox::assign_match::MatchOutcome,
    /// Always false. Retained for clients that display the proposal status.
    pub applied: bool,
}

/// Decide one thread by LAW-347's address-match: the From/To/Cc candidate
/// set comes from the archived raw `.eml` when the message carries one,
/// falling back to the stored From; each candidate resolves against the
/// Person CRM, and its open-Project participations — same Firm as the
/// thread, `status = 'open'` — feed the pure matcher. Entities carry no
/// email field in the schema, so the candidate set is Persons today; when
/// Entities gain email storage they join `ResolvedAddress` unchanged.
/// Returns the candidate set too: it is the Jev question's header state
/// when the model method takes the thread.
async fn decide_assign_match(
    state: &ApiState,
    row: &store::inbox::UnprocessedRow,
) -> Result<(Vec<String>, store::inbox::assign_match::MatchOutcome), ApiError> {
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
    Ok((
        candidates,
        store::inbox::assign_match::decide(&stored_from, &resolved),
    ))
}

/// What the Jev method recorded for one thread — filled into the decision
/// log's reserved fields when it ran. A gate refusal carries neither: the
/// model never saw the thread, so the address-match reason stands and the
/// fill alone marks the refusal in the audit trail.
#[derive(Debug, Clone, PartialEq)]
struct JevFill {
    model_id: Option<String>,
    probabilities: Option<serde_json::Value>,
}

/// The Choice question's instruction — the Firm's model of the decision,
/// constant across threads; the rows live in the criteria.
const JEV_INSTRUCTIONS: &str = "Assign this thread to the open project the legal matter it belongs to is proceeding on; choose none if it is not matter mail.";

/// How many characters of the message body the below-threshold retry
/// carries — the body is context, never the whole mailbox.
const JEV_BODY_EXCERPT_CHARS: usize = 2_000;

/// One Choice option per open Project — descriptions generated from the
/// Navigator rows (code, name, description), never literals — plus the
/// final `none` option. `None` when the Firm's open Projects exceed the
/// wire's 255-option ceiling; that thread degrades to the inbox.
fn choice_question(projects: &[store::inbox::ProjectChoice]) -> Option<typesafe::ChoiceQuestion> {
    if projects.len() > 254 {
        return None;
    }
    let mut criteria = BTreeMap::new();
    for project in projects {
        let description = match project.description.as_deref() {
            Some(description) if !description.trim().is_empty() => {
                format!("{} — {}", project.name, description.trim())
            }
            _ => project.name.clone(),
        };
        criteria.insert(project.id.to_string(), description);
    }
    criteria.insert(
        typesafe::NONE_OPTION.to_owned(),
        "Not associated with any open project".to_owned(),
    );
    Some(typesafe::ChoiceQuestion::new(
        JEV_INSTRUCTIONS.to_owned(),
        criteria,
    ))
}

/// The thread's header state for a Jev question: the envelope candidates
/// other than the stored From ride as `to`. The body excerpt joins only
/// on the below-threshold retry.
fn thread_state(
    row: &store::inbox::UnprocessedRow,
    candidates: &[String],
    body: Option<String>,
) -> typesafe::ThreadState {
    let from = row.external_address.trim().to_owned();
    typesafe::ThreadState {
        to: candidates
            .iter()
            .filter(|address| !address.eq_ignore_ascii_case(&from))
            .cloned()
            .collect(),
        cc: Vec::new(),
        subject: row.subject.clone(),
        date: Some(row.occurred_at.clone()),
        attachments: Vec::new(),
        body,
        from,
    }
}

/// The body excerpt a below-threshold retry sends: quoted history and the
/// signature stripped, capped at the excerpt length. Char-boundary safe.
fn body_excerpt(body: &str) -> String {
    crate::email_threads::strip_quoted(body)
        .chars()
        .take(JEV_BODY_EXCERPT_CHARS)
        .collect()
}

/// Map a chosen option id back onto the Project it named. An id that is
/// not `none` and not a Project uuid this run offered is a model echo the
/// bands refuse — it degrades exactly like a failed call, never guesses.
fn chosen_project(chosen: &str) -> Option<Uuid> {
    if chosen == typesafe::NONE_OPTION {
        return None;
    }
    Uuid::parse_str(chosen).ok()
}

/// The Jev method (LAW-347): for a thread the address-match left
/// unresolved and the LAW-346 gate opened, ask one TypeSafe Choice
/// question — headers first, the short body excerpt only when the
/// headers-only answer could not even propose — and band the answer:
/// ≥ 0.9 assigns, 0.6–0.9 proposes for lawyer confirmation, anything
/// else stays in the inbox. Every failure degrades to the inbox with the
/// reason logged; a Jev outage can never fail a run or mis-assign.
/// Returns the fill for the decision log's reserved fields, `None` when
/// Jev never ran.
async fn jev_step(
    jev: Option<&typesafe::JevClient>,
    projects: impl std::future::Future<Output = Result<Vec<store::inbox::ProjectChoice>, ApiError>>,
    row: &store::inbox::UnprocessedRow,
    candidates: &[String],
    outcome: &mut store::inbox::assign_match::MatchOutcome,
) -> Result<Option<JevFill>, ApiError> {
    if !matches!(
        outcome,
        store::inbox::assign_match::MatchOutcome::LeftInInbox { .. }
    ) {
        return Ok(None);
    }
    let Some(jev) = jev else {
        // Gate closed: the address-match reason stands; the fill alone
        // records that the model method was gated off.
        return Ok(Some(JevFill {
            model_id: None,
            probabilities: Some(serde_json::json!({ "jev": "gate_closed" })),
        }));
    };
    let Some(question) = choice_question(&projects.await?) else {
        *outcome = store::inbox::assign_match::MatchOutcome::LeftInInbox {
            reason: store::inbox::assign_match::LeftReason::JevTooManyProjects,
        };
        return Ok(None);
    };
    // Headers first; the body excerpt joins only when the headers-only
    // answer could not even propose. At most one retry: the second
    // answer's band decides whatever it says.
    let mut question_state = thread_state(row, candidates, None);
    let mut body_used = false;
    let answer = loop {
        match jev.choose(&question, &question_state).await {
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    message = %row.id,
                    "Jev assignment call failed; the thread stays in the inbox"
                );
                *outcome = store::inbox::assign_match::MatchOutcome::LeftInInbox {
                    reason: store::inbox::assign_match::LeftReason::JevUnavailable,
                };
                return Ok(Some(JevFill {
                    model_id: Some(typesafe::MODEL_ID.to_owned()),
                    probabilities: Some(serde_json::json!({ "jev": "unavailable" })),
                }));
            }
            Ok(answer) if !body_used && matches!(typesafe::band(&answer), JevOutcome::Leave(_)) => {
                question_state.body = Some(body_excerpt(&row.body));
                body_used = true;
            }
            Ok(answer) => break answer,
        }
    };
    let banded = typesafe::band(&answer);
    let project_id = chosen_project(&answer.choice);
    let left = |reason| store::inbox::assign_match::MatchOutcome::LeftInInbox { reason };
    let store_reason = |reason| match reason {
        typesafe::LeaveReason::NoProbabilities => {
            store::inbox::assign_match::LeftReason::JevNoProbabilities
        }
        typesafe::LeaveReason::NoneChosen => store::inbox::assign_match::LeftReason::JevNotMatter,
        typesafe::LeaveReason::BelowThreshold => {
            store::inbox::assign_match::LeftReason::JevBelowThreshold
        }
        typesafe::LeaveReason::TwoProjectTie => store::inbox::assign_match::LeftReason::JevTied,
    };
    *outcome = match banded {
        JevOutcome::AutoAssign(_) => match project_id {
            Some(project_id) => store::inbox::assign_match::MatchOutcome::Assigned { project_id },
            // The model echoed an option this run never offered — the
            // bands refuse it exactly like a failed call.
            None => left(store::inbox::assign_match::LeftReason::JevUnavailable),
        },
        JevOutcome::Propose(_) => match project_id {
            Some(project_id) => store::inbox::assign_match::MatchOutcome::Proposed { project_id },
            None => left(store::inbox::assign_match::LeftReason::JevUnavailable),
        },
        JevOutcome::Leave(reason) => left(store_reason(reason)),
    };
    Ok(Some(JevFill {
        model_id: Some(answer.model),
        probabilities: Some(serde_json::json!({
            "chosen": answer.choice,
            "confidence": answer.confidence,
            "body_used": body_used,
            "probabilities": answer.probabilities,
        })),
    }))
}

/// Every decision — assigned or left — logs the method, the Project it
/// matched (or none), and that an unmatched thread stayed in the inbox.
/// The Jev fill rides the reserved `model_id` and `probabilities` fields
/// the shape already carries: a gate refusal marks them without replacing
/// the address-match reason, and a Jev decision fills both.
fn log_assign_decision(
    message: Uuid,
    outcome: &store::inbox::assign_match::MatchOutcome,
    jev: Option<&JevFill>,
) {
    let mut entry = outcome.decision_log(message);
    if let Some(fill) = jev {
        entry.model_id.clone_from(&fill.model_id);
        entry.probabilities.clone_from(&fill.probabilities);
    }
    tracing::info!(
        decision = %serde_json::to_string(&entry).expect("a decision log always serializes"),
        "inbox assignment decision"
    );
}

const MAX_ASSIGN_MATCH_THREADS: usize = 50;

fn assignment_page(
    mut rows: Vec<store::inbox::UnprocessedRow>,
    after: Option<(chrono::DateTime<Utc>, Uuid)>,
) -> Vec<store::inbox::UnprocessedRow> {
    rows.retain(|row| {
        row.project_id.is_none()
            && row.channel == store::inbox::channel::EMAIL
            && after.is_none_or(|cursor| (row.inserted_at, row.id) > cursor)
    });
    rows.sort_unstable_by_key(|row| (row.inserted_at, row.id));
    rows.truncate(MAX_ASSIGN_MATCH_THREADS + 1);
    rows
}

/// `POST /app/api/inbox/assign-matches` — the LAW-347 address-match door
/// Every unprocessed, unattributed email thread proposes a Project. An
/// assignment can create Communications, so this batch door only previews.
/// Applying a proposed filing requires per-Message lawyer confirmation.
pub(crate) async fn assign_matches(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Json(request): Json<AssignMatchesRequest>,
) -> Result<Response, ApiError> {
    let actor_id = actor_id(&session)?;
    if request.apply {
        return Ok(crate::api::bad_request(
            "filing_confirmation_required",
            "Review and confirm each proposed filing individually.",
        ));
    }
    let mut rows = assignment_page(
        store::inbox::unprocessed_inbox(&state.surreal, actor_id)
            .await
            .map_err(map_inbox)?,
        request.after,
    );
    let next = if rows.len() > MAX_ASSIGN_MATCH_THREADS {
        rows.pop();
        rows.last().map(|row| (row.inserted_at, row.id))
    } else {
        None
    };
    let mut views = Vec::new();
    for row in rows {
        let (candidates, mut outcome) = decide_assign_match(&state, &row).await?;
        let projects = async {
            store::inbox::projects_in_firm(&state.surreal, row.firm_id)
                .await
                .map_err(map_inbox)
        };
        let jev_fill = jev_step(
            state.jev.as_ref(),
            projects,
            &row,
            &candidates,
            &mut outcome,
        )
        .await?;
        log_assign_decision(row.id, &outcome, jev_fill.as_ref());
        views.push(AssignMatchView {
            message: row.id,
            conversation_id: row.conversation_id,
            from: row.external_address,
            subject: row.subject,
            decision: outcome,
            applied: false,
        });
    }
    let mut response = Json(views).into_response();
    if let Some(cursor) = next {
        let encoded =
            serde_json::to_string(&cursor).map_err(|error| ApiError::Db(error.to_string()))?;
        let value = axum::http::HeaderValue::from_str(&encoded)
            .map_err(|error| ApiError::Db(error.to_string()))?;
        response
            .headers_mut()
            .insert("x-navigator-next-cursor", value);
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::{message_lock, ApplyLocks};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use uuid::Uuid;

    use store::inbox::assign_match::{LeftReason, MatchOutcome};
    use store::inbox::{ProjectChoice, UnprocessedRow};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn preview_row() -> UnprocessedRow {
        UnprocessedRow {
            id: Uuid::from_u128(1),
            conversation_id: Uuid::from_u128(2),
            firm_id: Uuid::from_u128(3),
            project_id: None,
            channel: store::inbox::channel::EMAIL.into(),
            provider_message_id: None,
            external_address: "sender@example.test".into(),
            external_name: None,
            subject: Some("Synthetic inquiry".into()),
            body: "Synthetic context".into(),
            occurred_at: "2026-11-01T00:00:00Z".into(),
            inserted_at: chrono::Utc::now(),
        }
    }

    fn project(id: u128) -> ProjectChoice {
        ProjectChoice {
            id: Uuid::from_u128(id),
            code: format!("synthetic-{id}"),
            name: "Synthetic Matter".into(),
            description: None,
        }
    }

    fn left(reason: LeftReason) -> MatchOutcome {
        MatchOutcome::LeftInInbox { reason }
    }

    #[tokio::test]
    async fn jev_bands_assign_propose_and_leave_the_unconfident() {
        let server = MockServer::start().await;
        let client = typesafe::JevClient::new(&server.uri(), "test-key");
        let offered = project(4);
        let project_id = offered.id;
        for (choice, confidence, expected, body_used) in [
            (
                project_id,
                0.95,
                MatchOutcome::Assigned { project_id },
                false,
            ),
            (
                project_id,
                0.7,
                MatchOutcome::Proposed { project_id },
                false,
            ),
            (project_id, 0.4, left(LeftReason::JevBelowThreshold), true),
            (
                Uuid::from_u128(5),
                0.95,
                left(LeftReason::JevUnavailable),
                false,
            ),
        ] {
            let choice = choice.to_string();
            Mock::given(method("POST"))
                .and(path("/v1/systemone"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "model": typesafe::MODEL_ID,
                    "answers": { typesafe::QUESTION_ID: {
                        "type": "choice",
                        "choice": choice,
                        "confidence": confidence,
                        "probabilities": { (choice.as_str()): confidence, "none": 1.0 - confidence },
                    }}
                })))
                .expect(if body_used { 2 } else { 1 })
                .mount(&server)
                .await;
            let mut outcome = left(LeftReason::NoMatch);
            let fill = super::jev_step(
                Some(&client),
                std::future::ready(Ok(vec![offered.clone()])),
                &preview_row(),
                &[],
                &mut outcome,
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(outcome, expected);
            assert_eq!(fill.model_id.as_deref(), Some(typesafe::MODEL_ID));
            let probabilities = fill.probabilities.unwrap();
            if choice == project_id.to_string() {
                assert_eq!(probabilities["chosen"], choice);
                assert_eq!(probabilities["body_used"], body_used);
            } else {
                assert_eq!(probabilities, serde_json::json!({"jev": "unavailable"}));
            }
            let requests = server.received_requests().await.unwrap();
            let first: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
            assert!(first["state"].get("body").is_none());
            if body_used {
                let retry: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
                assert_eq!(retry["state"]["body"], "Synthetic context");
            }
            server.verify().await;
            server.reset().await;
        }
    }

    #[tokio::test]
    async fn a_closed_jev_gate_keeps_the_address_match_without_loading_projects() {
        let mut outcome = left(LeftReason::NoMatch);
        let fill = super::jev_step(
            None,
            std::future::poll_fn(|_| panic!("the closed gate must not load Projects")),
            &preview_row(),
            &[],
            &mut outcome,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(outcome, left(LeftReason::NoMatch));
        assert!(fill.model_id.is_none());
        assert_eq!(
            fill.probabilities,
            Some(serde_json::json!({"jev": "gate_closed"}))
        );
    }

    #[tokio::test]
    async fn a_jev_failure_degrades_to_the_inbox_and_names_the_model() {
        let server = MockServer::start().await;
        let client = typesafe::JevClient::new(&server.uri(), "test-key");
        for status in [401, 500] {
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(status))
                .expect(1)
                .mount(&server)
                .await;
            let mut outcome = left(LeftReason::NoMatch);
            let fill = super::jev_step(
                Some(&client),
                std::future::ready(Ok(vec![project(4)])),
                &preview_row(),
                &[],
                &mut outcome,
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(outcome, left(LeftReason::JevUnavailable));
            assert_eq!(fill.model_id.as_deref(), Some(typesafe::MODEL_ID));
            assert_eq!(
                fill.probabilities,
                Some(serde_json::json!({"jev": "unavailable"}))
            );
            server.verify().await;
            server.reset().await;
        }
    }

    #[tokio::test]
    async fn an_option_roster_over_the_ceiling_never_reaches_the_model() {
        let server = MockServer::start().await;
        let client = typesafe::JevClient::new(&server.uri(), "test-key");
        let mut outcome = left(LeftReason::NoMatch);
        let fill = super::jev_step(
            Some(&client),
            std::future::ready(Ok((1..=255).map(project).collect())),
            &preview_row(),
            &[],
            &mut outcome,
        )
        .await
        .unwrap();
        assert_eq!(outcome, left(LeftReason::JevTooManyProjects));
        assert!(fill.is_none());
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[test]
    fn assignment_page_advances_past_unmatched_rows_at_the_same_time() {
        let inserted_at = chrono::Utc::now();
        let rows = || -> Vec<store::inbox::UnprocessedRow> {
            (1..=51)
                .rev()
                .map(|id| store::inbox::UnprocessedRow {
                    id: Uuid::from_u128(id),
                    conversation_id: Uuid::from_u128(id),
                    firm_id: Uuid::nil(),
                    project_id: None,
                    channel: store::inbox::channel::EMAIL.into(),
                    provider_message_id: None,
                    external_address: String::new(),
                    external_name: None,
                    subject: None,
                    body: String::new(),
                    occurred_at: String::new(),
                    inserted_at,
                })
                .collect()
        };
        let mut candidates = rows();
        candidates.push(store::inbox::UnprocessedRow {
            project_id: Some(Uuid::nil()),
            ..rows().pop().unwrap()
        });
        let first = super::assignment_page(candidates, None);
        assert_eq!(first.len(), 51);
        let boundary = &first[49];
        assert_eq!(boundary.id, Uuid::from_u128(50));

        let second = super::assignment_page(rows(), Some((boundary.inserted_at, boundary.id)));
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].id, Uuid::from_u128(51));
    }

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

//! Attorney review surface for an inbound contract review —
//! `/app/lawyer/contract-reviews/:id`.
//!
//! After the web-side analysis ([`crate::contract_review_walk`]) opens a
//! `contract_reviews` row of machine-proposed findings, the matter parks at
//! `lawyer_review`. Here a licensed attorney (`lawyer` tier — `lawyer` includes
//! attorneys) acts on the review:
//!
//! - **edits and decides each finding** — `attorney_note`, `suggested_redline`,
//!   `severity`, and an explicit *accept* or *reject*. There is **no
//!   bulk-accept**: every save is a per-finding decision, and nothing is
//!   accepted until the attorney acts. Each decision is written to
//!   `notation_events` (the immutable audit trail) so the memo is provably
//!   attorney-reviewed;
//! - **edits the risk summary**;
//! - **approves** — only once *every* finding has been acted on. Approval
//!   assembles the review memo from the exact signed-off findings + risk
//!   summary + the load-bearing disclaimers, renders it to a PDF filed into
//!   the Project, and drives the workflow `approved` →
//!   `generate_pdf__review_memo` → `memo_rendered` → `END`;
//! - **rejects** — `lawyer_review --rejected--> END`, no memo.
//!
//! Authorization: the route lives under `/app/lawyer/*`, so embedded Rego's
//! `lawyer_tier` rule gates it; the handlers add a per-matter row scope (a
//! client role, or a lawyer not disclosed to the project, gets `404`).

use axum::extract::{Extension, Form, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;

use store::contract_reviews::{self, ContractReview, Finding, MACHINE_CONTRACT_REVIEW};
use store::notation_events::{append_event, TransitionRecord};
use store::persons::Role;
use store::playbooks::{self, Playbook, SEVERITY_HIGH, SEVERITY_LOW, SEVERITY_MEDIUM};
use workflows::{DocumentPayload, MachineKind, StateMachineRuntime};

use crate::admin::AdminState;
use crate::contract_review_walk::INBOUND_CONTRACT_KIND;
use crate::session::SessionData;

const FINDING_ACCEPTED: &str = "finding_accepted";
const FINDING_REJECTED: &str = "finding_rejected";

/// Storage-key convention for a review memo PDF.
#[must_use]
pub fn memo_storage_key(notation_id: Uuid) -> String {
    format!("notations/{notation_id}/review-memo.pdf")
}

/// `documents.kind` the rendered memo is filed under in the Project —
/// `rules::kind::Kind::Memo`'s `as_str()`. The ingest boundary now validates
/// this against `Kind::valid_for(Lane::Asset)` (ENG-370), so it must be the
/// real enum value rather than the `review_memo` step-name-adjacent spelling
/// this constant carried before that gate existed.
const MEMO_KIND: &str = "memo";

// --- the loaded review + its matter ---------------------------------------

struct Loaded {
    review: ContractReview,
    notation: store::notations::Notation,
    playbook: Playbook,
}

/// Why a contract-review action failed. Shared by the lawyer review surface and
/// the `/app/api/contract-reviews/{id}/*` doors so both adapters agree.
#[derive(Debug)]
pub enum ReviewActionError {
    /// No such review, or the caller may not see its matter (non-disclosing).
    NotFoundOrScoped,
    /// The review or its notation is not at the gate this action needs.
    NotOpen,
    /// The finding index names no finding on this review.
    FindingNotFound,
    /// Approval blocked: not every finding has an accept/reject decision.
    FindingsUnacted,
    /// Approval blocked: not every anchored document comment has a decision.
    DocumentCommentsUnacted,
    /// A store write or the workflow signal failed.
    Db(String),
}

/// Errors returned by the full-document review mutations. Error redirects use
/// only these stable codes; document text and comment bodies never enter a
/// response, log, or workflow payload.
#[derive(Debug, thiserror::Error)]
pub enum DocumentReviewError {
    #[error("review not found or not authorized")]
    NotFound,
    #[error("document review is not open")]
    NotOpen,
    #[error("expected parent version has moved on")]
    Conflict,
    #[error("unsupported or missing document anchor")]
    Anchor,
    #[error("protected token changed")]
    Protected,
    #[error("invalid decision")]
    Decision,
    #[error("document operation failed")]
    Store(#[from] store::notation_documents::NotationDocumentError),
    #[error("document operation failed")]
    Serde(#[from] serde_json::Error),
    #[error("document operation failed")]
    Parse(#[from] word::BlockEditError),
}

#[derive(Deserialize)]
pub struct DocumentEditForm {
    expected_parent_version_id: String,
    anchor: String,
    text: String,
    #[serde(default)]
    locked_kinds: Vec<String>,
}

#[derive(Deserialize)]
pub struct DocumentCommentForm {
    anchor: String,
    body: String,
}

#[derive(Deserialize)]
pub struct DocumentDecisionForm {
    decision: String,
}

impl std::fmt::Display for ReviewActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFoundOrScoped => write!(f, "contract review not found"),
            Self::NotOpen => write!(f, "contract review is not at the required gate"),
            Self::FindingNotFound => write!(f, "no such finding"),
            Self::FindingsUnacted => write!(
                f,
                "every finding must be accepted or rejected before the memo can be approved"
            ),
            Self::DocumentCommentsUnacted => write!(
                f,
                "every document comment must be accepted, rejected, or edited before the memo can be approved"
            ),
            Self::Db(e) => write!(f, "database: {e}"),
        }
    }
}

impl std::error::Error for ReviewActionError {}

/// Load the review, its notation, and its playbook, enforcing the per-matter row
/// scope from the caller's `(person_id, role)`. The command-level form of the
/// old `load_scoped`: a missing review, a client caller, or a lawyer not on the
/// matter all collapse to [`ReviewActionError::NotFoundOrScoped`].
async fn load_review_scoped(
    surreal: &store::surreal::SurrealDb,
    review_id: Uuid,
    person_id: Option<Uuid>,
    role: Role,
) -> Result<Loaded, ReviewActionError> {
    let review = contract_reviews::by_id(surreal, review_id)
        .await
        .ok()
        .flatten()
        .ok_or(ReviewActionError::NotFoundOrScoped)?;
    let notation = store::notations::find_by_id(surreal, review.notation_id)
        .await
        .ok()
        .flatten()
        .ok_or(ReviewActionError::NotFoundOrScoped)?;
    // A client never reaches a lawyer surface; a lawyer must be assigned to the
    // matter (admin bypasses in the lawyer-lens helper).
    if matches!(role, Role::Client) {
        return Err(ReviewActionError::NotFoundOrScoped);
    }
    if !store::access::can_see_project_as_lawyer(surreal, person_id, role, notation.project_id)
        .await
        .unwrap_or(false)
    {
        return Err(ReviewActionError::NotFoundOrScoped);
    }
    let playbook = playbooks::by_id(surreal, review.playbook_id)
        .await
        .ok()
        .flatten()
        .ok_or(ReviewActionError::NotFoundOrScoped)?;
    Ok(Loaded {
        review,
        notation,
        playbook,
    })
}

#[derive(Deserialize)]
pub struct FindingEdit {
    /// `accept` or `reject` — the submit button the attorney clicked.
    decision: String,
    severity: String,
    suggested_redline: String,
    attorney_note: String,
}

/// Record the attorney's edits and accept/reject decision on one finding. The
/// one command behind both the lawyer review form and the REST door.
#[allow(clippy::too_many_arguments)]
pub async fn save_review_finding(
    surreal: &store::surreal::SurrealDb,
    review_id: Uuid,
    idx: usize,
    person_id: Option<Uuid>,
    role: Role,
    accept: bool,
    severity: &str,
    suggested_redline: &str,
    attorney_note: &str,
) -> Result<(), ReviewActionError> {
    let loaded = load_review_scoped(surreal, review_id, person_id, role).await?;
    // Only an open (analyzed) review takes edits.
    if !matches!(
        loaded.review.status.as_str(),
        contract_reviews::STATUS_ANALYZED
    ) {
        return Err(ReviewActionError::NotOpen);
    }
    let mut findings = contract_reviews::findings_of(&loaded.review).unwrap_or_default();
    let Some(finding) = findings.get_mut(idx) else {
        return Err(ReviewActionError::FindingNotFound);
    };
    finding.accepted = accept;
    finding.attorney_note = non_empty(attorney_note);
    finding.suggested_redline = non_empty(suggested_redline);
    if is_severity(severity) {
        finding.severity = severity.to_lowercase();
    }
    let clause_ref = finding.clause_ref.clone();
    contract_reviews::update_findings(surreal, review_id, &findings)
        .await
        .map_err(|e| ReviewActionError::Db(e.to_string()))?;
    // Immutable per-finding attribution — who decided what, when.
    record_finding_decision(
        surreal,
        loaded.notation.id,
        idx,
        &clause_ref,
        accept,
        person_id,
    )
    .await;
    Ok(())
}

/// `POST /app/lawyer/contract-reviews/:id/findings/:idx` — save the edits
/// to one finding and record the accept / reject decision.
pub async fn save_finding(
    State(state): State<AdminState>,
    Path((review_id, idx)): Path<(Uuid, usize)>,
    session: Option<Extension<SessionData>>,
    Form(input): Form<FindingEdit>,
) -> Response {
    let Some(Extension(session)) = session else {
        return not_found();
    };
    match save_review_finding(
        &state.surreal,
        review_id,
        idx,
        session.person_id,
        session.role,
        input.decision == "accept",
        &input.severity,
        &input.suggested_redline,
        &input.attorney_note,
    )
    .await
    {
        // A closed review is a no-op back to the surface, not an error.
        Ok(()) | Err(ReviewActionError::NotOpen) => redirect_to(review_id),
        Err(ReviewActionError::NotFoundOrScoped | ReviewActionError::FindingNotFound) => {
            not_found()
        }
        Err(e) => {
            tracing::error!(error = %e, %review_id, idx, "save finding failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal").into_response()
        }
    }
}

#[derive(Deserialize)]
pub struct SummaryEdit {
    risk_summary: String,
}

/// Edit a review's risk summary. The one command behind both the lawyer form and
/// the REST door.
pub async fn save_review_summary(
    surreal: &store::surreal::SurrealDb,
    review_id: Uuid,
    person_id: Option<Uuid>,
    role: Role,
    risk_summary: &str,
) -> Result<(), ReviewActionError> {
    load_review_scoped(surreal, review_id, person_id, role).await?;
    contract_reviews::update_risk_summary(surreal, review_id, risk_summary.trim())
        .await
        .map_err(|e| ReviewActionError::Db(e.to_string()))?;
    Ok(())
}

/// `POST /app/lawyer/contract-reviews/:id/summary` — edit the risk summary.
pub async fn save_summary(
    State(state): State<AdminState>,
    Path(review_id): Path<Uuid>,
    session: Option<Extension<SessionData>>,
    Form(input): Form<SummaryEdit>,
) -> Response {
    let Some(Extension(session)) = session else {
        return not_found();
    };
    match save_review_summary(
        &state.surreal,
        review_id,
        session.person_id,
        session.role,
        &input.risk_summary,
    )
    .await
    {
        Ok(()) => redirect_to(review_id),
        Err(ReviewActionError::NotFoundOrScoped) => not_found(),
        Err(e) => {
            tracing::error!(error = %e, %review_id, "save risk summary failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal").into_response()
        }
    }
}

fn document_redirect(review_id: Uuid, code: &str) -> Response {
    Redirect::to(&format!(
        "/app/lawyer/contract-reviews/{review_id}/document?error={code}"
    ))
    .into_response()
}

fn protected_kinds(values: &[String]) -> Vec<store::notation_documents::ProtectedTokenKind> {
    values
        .iter()
        .filter_map(|value| match value.as_str() {
            "number" => Some(store::notation_documents::ProtectedTokenKind::Number),
            "percentage" => Some(store::notation_documents::ProtectedTokenKind::Percentage),
            "currency" => Some(store::notation_documents::ProtectedTokenKind::Currency),
            "date" => Some(store::notation_documents::ProtectedTokenKind::Date),
            _ => None,
        })
        .collect()
}

/// Append one child source version from the supported block editor. The
/// parent compare-and-swap is the authoritative concurrent-save gate.
#[allow(clippy::too_many_arguments)]
pub async fn save_document_edit(
    surreal: &store::surreal::SurrealDb,
    storage: &std::sync::Arc<dyn cloud::StorageService>,
    review_id: Uuid,
    person_id: Option<Uuid>,
    role: Role,
    input: &DocumentEditForm,
) -> Result<(), DocumentReviewError> {
    let loaded = load_review_scoped(surreal, review_id, person_id, role)
        .await
        .map_err(|_| DocumentReviewError::NotFound)?;
    if loaded.review.status != contract_reviews::STATUS_ANALYZED
        || loaded.notation.state != "lawyer_review"
    {
        return Err(DocumentReviewError::NotOpen);
    }
    let expected_parent_version_id = input
        .expected_parent_version_id
        .parse::<Uuid>()
        .map_err(|_| DocumentReviewError::Conflict)?;
    let current = store::notation_documents::current_version(
        surreal,
        loaded.notation.project_id,
        loaded.notation.id,
    )
    .await?
    .ok_or(DocumentReviewError::NotFound)?;
    if current.id != expected_parent_version_id {
        return Err(DocumentReviewError::Conflict);
    }
    let markdown = store::notation_documents::markdown_of(surreal, storage, &current).await?;
    let trusted = word::TrustedNotationMarkdown::from_governed_store(markdown.clone());
    let document = word::CanonicalDocument::from_markdown(&trusted);
    let edited = document.with_block_text(&input.anchor, &input.text)?;
    let edited_markdown = edited.to_markdown();
    if store::notation_documents::protected_tokens_changed(
        &markdown,
        edited_markdown.as_ref(),
        &protected_kinds(&input.locked_kinds),
    ) {
        return Err(DocumentReviewError::Protected);
    }
    let manifest = serde_json::to_vec(&edited.block_manifest())?;
    let Some(person_id) = person_id else {
        return Err(DocumentReviewError::NotFound);
    };
    let recorded_at = Utc::now().to_rfc3339();
    let child = store::notation_documents::append_edit(
        surreal,
        storage,
        store::notation_documents::AppendEdit {
            project_id: loaded.notation.project_id,
            notation_id: loaded.notation.id,
            expected_parent_version_id,
            markdown: edited_markdown.as_ref().as_bytes(),
            anchor_manifest: &manifest,
            parser_version: &current.parser_version,
            schema_version: current.schema_version,
            authored_by_person_id: person_id,
            recorded_at: &recorded_at,
        },
    )
    .await
    .map_err(|error| match error {
        store::notation_documents::NotationDocumentError::Conflict { .. } => {
            DocumentReviewError::Conflict
        }
        other => DocumentReviewError::Store(other),
    })?;
    let anchors = edited
        .block_manifest()
        .into_iter()
        .map(|entry| entry.anchor)
        .collect::<Vec<_>>();
    store::notation_documents::carry_comments_to_version(
        surreal,
        loaded.notation.project_id,
        loaded.notation.id,
        current.id,
        child.id,
        &anchors,
    )
    .await?;
    Ok(())
}

/// `POST /app/lawyer/contract-reviews/{id}/document/edit`.
pub async fn save_document_edit_route(
    State(state): State<AdminState>,
    Path(review_id): Path<Uuid>,
    session: Option<Extension<SessionData>>,
    Form(input): Form<DocumentEditForm>,
) -> Response {
    let Some(Extension(session)) = session else {
        return document_redirect(review_id, "not-found");
    };
    let result = save_document_edit(
        &state.surreal,
        &state.storage,
        review_id,
        session.person_id,
        session.role,
        &input,
    )
    .await;
    match result {
        Ok(()) => document_redirect(review_id, "saved"),
        Err(DocumentReviewError::Conflict) => document_redirect(review_id, "conflict"),
        Err(DocumentReviewError::Protected) => document_redirect(review_id, "protected"),
        Err(DocumentReviewError::NotOpen) => document_redirect(review_id, "closed"),
        Err(DocumentReviewError::Anchor | DocumentReviewError::Parse(_)) => {
            document_redirect(review_id, "unsupported")
        }
        Err(DocumentReviewError::Decision) => document_redirect(review_id, "unsupported"),
        Err(
            DocumentReviewError::NotFound
            | DocumentReviewError::Store(_)
            | DocumentReviewError::Serde(_),
        ) => document_redirect(review_id, "not-found"),
    }
}

/// Add one comment to the current source version after checking that its
/// anchor is present in the governed manifest.
pub async fn add_document_comment_route(
    State(state): State<AdminState>,
    Path(review_id): Path<Uuid>,
    session: Option<Extension<SessionData>>,
    Form(input): Form<DocumentCommentForm>,
) -> Response {
    let Some(Extension(session)) = session else {
        return document_redirect(review_id, "not-found");
    };
    let result = async {
        let loaded = load_review_scoped(&state.surreal, review_id, session.person_id, session.role)
            .await
            .map_err(|_| DocumentReviewError::NotFound)?;
        if loaded.review.status != contract_reviews::STATUS_ANALYZED
            || loaded.notation.state != "lawyer_review"
        {
            return Err(DocumentReviewError::NotOpen);
        }
        let Some(person_id) = session.person_id else {
            return Err(DocumentReviewError::NotFound);
        };
        let current = store::notation_documents::current_version(
            &state.surreal,
            loaded.notation.project_id,
            loaded.notation.id,
        )
        .await?
        .ok_or(DocumentReviewError::NotFound)?;
        let manifest =
            store::notation_documents::anchor_manifest_of(&state.surreal, &state.storage, &current)
                .await?;
        let entries: Vec<word::BlockManifestEntry> = serde_json::from_slice(&manifest)?;
        if !entries.iter().any(|entry| entry.anchor == input.anchor) {
            return Err(DocumentReviewError::Anchor);
        }
        if input.body.trim().is_empty() {
            return Err(DocumentReviewError::Anchor);
        }
        store::notation_documents::add_comment(
            &state.surreal,
            loaded.notation.project_id,
            loaded.notation.id,
            current.id,
            &input.anchor,
            person_id,
            input.body.trim(),
        )
        .await?;
        Ok::<(), DocumentReviewError>(())
    }
    .await;
    match result {
        Ok(()) => document_redirect(review_id, "comment-added"),
        Err(DocumentReviewError::NotOpen) => document_redirect(review_id, "closed"),
        Err(DocumentReviewError::Anchor) => document_redirect(review_id, "unsupported"),
        Err(_) => document_redirect(review_id, "not-found"),
    }
}

/// Decide exactly one anchored comment; the store rejects duplicate decisions
/// and also verifies that the comment belongs to this scoped Notation.
pub async fn decide_document_comment_route(
    State(state): State<AdminState>,
    Path((review_id, comment_id)): Path<(Uuid, Uuid)>,
    session: Option<Extension<SessionData>>,
    Form(input): Form<DocumentDecisionForm>,
) -> Response {
    let Some(Extension(session)) = session else {
        return document_redirect(review_id, "not-found");
    };
    let result = async {
        let loaded = load_review_scoped(&state.surreal, review_id, session.person_id, session.role)
            .await
            .map_err(|_| DocumentReviewError::NotFound)?;
        if loaded.review.status != contract_reviews::STATUS_ANALYZED
            || loaded.notation.state != "lawyer_review"
        {
            return Err(DocumentReviewError::NotOpen);
        }
        let Some(person_id) = session.person_id else {
            return Err(DocumentReviewError::NotFound);
        };
        let recorded_at = Utc::now().to_rfc3339();
        store::notation_documents::decide_comment(
            &state.surreal,
            loaded.notation.project_id,
            loaded.notation.id,
            comment_id,
            &input.decision,
            person_id,
            &recorded_at,
        )
        .await
        .map_err(|_| DocumentReviewError::Decision)
    }
    .await;
    match result {
        Ok(()) => document_redirect(review_id, "decided"),
        Err(DocumentReviewError::NotOpen) => document_redirect(review_id, "closed"),
        Err(DocumentReviewError::Decision) => document_redirect(review_id, "decision"),
        Err(_) => document_redirect(review_id, "not-found"),
    }
}

/// Assemble + deliver the review memo and approve. The one command behind both
/// the lawyer approve control and the REST door. Refuses until every finding has
/// an accept/reject decision.
pub async fn approve_review(
    surreal: &store::surreal::SurrealDb,
    storage: &std::sync::Arc<dyn cloud::StorageService>,
    runtime: &dyn StateMachineRuntime,
    review_id: Uuid,
    person_id: Option<Uuid>,
    role: Role,
) -> Result<(), ReviewActionError> {
    let loaded = load_review_scoped(surreal, review_id, person_id, role).await?;
    if loaded.notation.state != "lawyer_review" {
        return Err(ReviewActionError::NotOpen);
    }
    let findings = contract_reviews::findings_of(&loaded.review).unwrap_or_default();
    // Force per-finding action: every finding must have a recorded decision.
    let acted = contract_reviews::acted_finding_indices(surreal, loaded.notation.id)
        .await
        .unwrap_or_default();
    if (0..findings.len()).any(|i| !acted.contains(&i)) {
        return Err(ReviewActionError::FindingsUnacted);
    }
    if let Some(current) = store::notation_documents::current_version(
        surreal,
        loaded.notation.project_id,
        loaded.notation.id,
    )
    .await
    .map_err(|error| ReviewActionError::Db(error.to_string()))?
    {
        let all_decided = store::notation_documents::all_comments_decided(
            surreal,
            loaded.notation.project_id,
            loaded.notation.id,
            current.id,
        )
        .await
        .map_err(|error| ReviewActionError::Db(error.to_string()))?;
        if !all_decided {
            return Err(ReviewActionError::DocumentCommentsUnacted);
        }
    }
    deliver_memo(surreal, storage, runtime, &loaded, &findings)
        .await
        .map_err(|e| ReviewActionError::Db(e.to_string()))?;
    Ok(())
}

/// `POST /app/lawyer/contract-reviews/:id/approve` — assemble + deliver the
/// memo and approve.
pub async fn approve(
    State(state): State<AdminState>,
    Path(review_id): Path<Uuid>,
    session: Option<Extension<SessionData>>,
) -> Response {
    let Some(Extension(session)) = session else {
        return not_found();
    };
    match approve_review(
        &state.surreal,
        &state.storage,
        state.workflow_runtime.as_ref(),
        review_id,
        session.person_id,
        session.role,
    )
    .await
    {
        Ok(()) | Err(ReviewActionError::NotOpen) => redirect_to(review_id),
        Err(ReviewActionError::NotFoundOrScoped) => not_found(),
        Err(ReviewActionError::FindingsUnacted) => Redirect::to(&format!(
            "/app/lawyer/contract-reviews/{review_id}?error={}",
            crate::admin::encode_query_value(
                "Every finding must be accepted or rejected before the memo can be approved."
            )
        ))
        .into_response(),
        Err(ReviewActionError::DocumentCommentsUnacted) => Redirect::to(&format!(
            "/app/lawyer/contract-reviews/{review_id}?error={}",
            crate::admin::encode_query_value(
                "Every document comment must be accepted, rejected, or edited before the memo can be approved."
            )
        ))
        .into_response(),
        Err(e) => {
            tracing::error!(error = %e, %review_id, "approve / memo delivery failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal").into_response()
        }
    }
}

/// Reject a review without a memo. The one command behind both the lawyer reject
/// control and the REST door.
pub async fn reject_review(
    surreal: &store::surreal::SurrealDb,
    runtime: &dyn StateMachineRuntime,
    review_id: Uuid,
    person_id: Option<Uuid>,
    role: Role,
) -> Result<(), ReviewActionError> {
    let loaded = load_review_scoped(surreal, review_id, person_id, role).await?;
    if loaded.notation.state != "lawyer_review" {
        return Err(ReviewActionError::NotOpen);
    }
    let next = StateMachineRuntime::signal(
        runtime,
        MachineKind::Workflow,
        loaded.notation.id,
        "rejected",
        None,
    )
    .await
    .map_err(|e| ReviewActionError::Db(e.to_string()))?;
    let _ = sync_notation_state(surreal, loaded.notation.id, next.as_str()).await;
    let _ =
        contract_reviews::set_status(surreal, review_id, contract_reviews::STATUS_REJECTED).await;
    Ok(())
}

/// `POST /app/lawyer/contract-reviews/:id/reject` — reject without a memo.
pub async fn reject(
    State(state): State<AdminState>,
    Path(review_id): Path<Uuid>,
    session: Option<Extension<SessionData>>,
) -> Response {
    let Some(Extension(session)) = session else {
        return not_found();
    };
    match reject_review(
        &state.surreal,
        state.workflow_runtime.as_ref(),
        review_id,
        session.person_id,
        session.role,
    )
    .await
    {
        Ok(()) | Err(ReviewActionError::NotOpen) => redirect_to(review_id),
        Err(ReviewActionError::NotFoundOrScoped) => not_found(),
        Err(e) => {
            tracing::error!(error = %e, %review_id, "reject signal failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal").into_response()
        }
    }
}

// --- memo delivery ---------------------------------------------------------

/// Assemble the memo from the exact signed-off findings, render + file it into
/// the Project, and drive the workflow to `END`.
async fn deliver_memo(
    surreal: &store::surreal::SurrealDb,
    storage: &std::sync::Arc<dyn cloud::StorageService>,
    runtime: &dyn StateMachineRuntime,
    loaded: &Loaded,
    findings: &[Finding],
) -> anyhow::Result<()> {
    let notation_id = loaded.notation.id;
    export_and_verify_review_document(surreal, storage, loaded).await?;
    let risk_summary = loaded.review.risk_summary.clone().unwrap_or_default();
    let accepted: Vec<&Finding> = findings.iter().filter(|f| f.accepted).collect();
    let typst_source = assemble_memo_typst(&MemoInput {
        playbook_name: &loaded.playbook.name,
        risk_summary: &risk_summary,
        accepted_findings: &accepted,
    });

    // Render web-side and file the PDF into the Project (a `documents` row +
    // git commit, the per-Project system of record). The worker also persists
    // it to the storage key on the `generate_pdf__review_memo` step below —
    // the two writes are idempotent (same Typst → same bytes).
    let bytes = pdf::render(&typst_source)?;
    let args = store::documents::IngestArgs {
        project_id: loaded.notation.project_id,
        source: store::documents::source::UPLOAD,
        filename: "review-memo.pdf",
        kind: MEMO_KIND,
        content_type: "application/pdf",
        description: Some("Inbound contract review memo"),
        secondary_storage_key: None,
        // Attorney work product — never listed in the client's documents (#782).
        visibility: store::documents::visibility::INTERNAL,
    };
    store::documents::ingest_bytes(surreal, storage, &args, &bytes).await?;

    // approved → generate_pdf__review_memo (worker renders + persists),
    // then memo_rendered → END.
    let payload = serde_json::to_string(&DocumentPayload::Typst {
        storage_key: memo_storage_key(notation_id),
        typst_source,
    })?;
    let s = StateMachineRuntime::signal(
        runtime,
        MachineKind::Workflow,
        notation_id,
        "approved",
        Some(&payload),
    )
    .await?;
    sync_notation_state(surreal, notation_id, s.as_str()).await?;
    contract_reviews::set_status(surreal, loaded.review.id, contract_reviews::STATUS_APPROVED)
        .await?;
    let s = StateMachineRuntime::signal(
        runtime,
        MachineKind::Workflow,
        notation_id,
        "memo_rendered",
        None,
    )
    .await?;
    sync_notation_state(surreal, notation_id, s.as_str()).await?;
    Ok(())
}

/// Reapply the governed Notation edit projection to the immutable Word
/// baseline, then independently validate the resulting package before filing
/// it as internal attorney work product. The managed exporter refuses a
/// missing or ambiguous embedded anchor; it never falls back to text matching.
async fn export_and_verify_review_document(
    surreal: &store::surreal::SurrealDb,
    storage: &std::sync::Arc<dyn cloud::StorageService>,
    loaded: &Loaded,
) -> anyhow::Result<()> {
    let Some(document) = store::notation_documents::find_document(
        surreal,
        loaded.notation.project_id,
        loaded.notation.id,
    )
    .await?
    else {
        return Ok(());
    };
    let Some(current) = store::notation_documents::current_version(
        surreal,
        loaded.notation.project_id,
        loaded.notation.id,
    )
    .await?
    else {
        return Ok(());
    };
    let original = store::assets::fetch(surreal, storage, document.original_asset_id).await?;
    let adapter = word::ManagedAdapter::from_env();
    let parsed = word::parse_with_adapter(&adapter, "source.docx", &original).await?;
    let governed = store::notation_documents::markdown_of(surreal, storage, &current).await?;
    let edited = word::CanonicalDocument::from_markdown(
        &word::TrustedNotationMarkdown::from_governed_store(governed),
    );
    let changes = changed_blocks(&edited, &parsed.canonical_outline());
    let exported = word::export_with_adapter(&adapter, "reviewed.docx", &original, changes).await?;
    word::verify_with_adapter(&adapter, "reviewed.docx", &exported).await?;
    let exported_document = word::parse_with_adapter(&adapter, "reviewed.docx", &exported).await?;
    word::verify_outline_preserved(&parsed, &exported_document)?;
    store::documents::ingest_bytes_exactly_once(
        surreal,
        storage,
        &store::documents::IngestArgs {
            project_id: loaded.notation.project_id,
            source: store::documents::source::GENERATED,
            filename: "reviewed-contract.docx",
            kind: INBOUND_CONTRACT_KIND,
            content_type: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            description: Some("Attorney-reviewed Word contract"),
            secondary_storage_key: None,
            visibility: store::documents::visibility::INTERNAL,
        },
        &exported,
    )
    .await?;
    Ok(())
}

fn changed_blocks(
    edited: &word::CanonicalDocument,
    original: &word::CanonicalDocument,
) -> Vec<word::ExportChange> {
    let mut original_by_anchor = std::collections::BTreeMap::new();
    collect_block_text(&original.stories, &mut original_by_anchor);
    let mut changes = Vec::new();
    collect_changes(&edited.stories, &original_by_anchor, &mut changes);
    changes
}

fn collect_block_text(
    stories: &[word::CanonicalStory],
    output: &mut std::collections::BTreeMap<String, (String, bool)>,
) {
    for story in stories {
        collect_block_texts(&story.blocks, output);
    }
}

fn collect_block_texts(
    blocks: &[word::CanonicalBlock],
    output: &mut std::collections::BTreeMap<String, (String, bool)>,
) {
    for block in blocks {
        output.insert(
            block.anchor.clone(),
            (block.text.clone(), block.is_editable()),
        );
        collect_block_texts(&block.children, output);
    }
}

fn collect_changes(
    stories: &[word::CanonicalStory],
    original: &std::collections::BTreeMap<String, (String, bool)>,
    output: &mut Vec<word::ExportChange>,
) {
    for story in stories {
        collect_changes_from_blocks(&story.blocks, original, output);
    }
}

fn collect_changes_from_blocks(
    blocks: &[word::CanonicalBlock],
    original: &std::collections::BTreeMap<String, (String, bool)>,
    output: &mut Vec<word::ExportChange>,
) {
    for block in blocks {
        if let Some((old_text, editable)) = original.get(&block.anchor) {
            if *editable && block.is_editable() && old_text != &block.text {
                output.push(word::ExportChange {
                    anchor: block.anchor.clone(),
                    replacement_text: block.text.clone(),
                    author: "Neon Law attorney".to_string(),
                });
            }
        }
        collect_changes_from_blocks(&block.children, original, output);
    }
}

/// What the memo is assembled from.
pub struct MemoInput<'a> {
    pub playbook_name: &'a str,
    pub risk_summary: &'a str,
    pub accepted_findings: &'a [&'a Finding],
}

/// Assemble the review-memo Typst source from the signed-off findings + risk
/// summary + the load-bearing disclaimers (named playbook; not a full audit;
/// attorney accountable; zero-retention AI). Every dynamic value is inserted
/// as a Typst string literal (`#"…"`) so arbitrary attorney/finding text can
/// never break the markup.
#[must_use]
pub fn assemble_memo_typst(input: &MemoInput<'_>) -> String {
    let mut out = String::new();
    out.push_str("#set page(paper: \"us-letter\", margin: 1in)\n");
    out.push_str("#set text(size: 11pt)\n");
    out.push_str("#set par(justify: true)\n\n");
    out.push_str(
        "#align(center)[#text(size: 16pt, weight: \"bold\")[Inbound Contract Review Memo]]\n\n",
    );
    out.push_str("*Measured against playbook:* ");
    out.push_str(&typ_str(input.playbook_name));
    out.push_str("\n\n== Risk summary\n");
    out.push_str(&typ_str(input.risk_summary));
    out.push_str("\n\n== Findings\n");
    if input.accepted_findings.is_empty() {
        out.push_str(&typ_str(
            "No deviations were flagged for delivery against this playbook.",
        ));
        out.push('\n');
    } else {
        for f in input.accepted_findings {
            out.push_str("\n=== ");
            out.push_str(&typ_str(&f.clause_ref));
            out.push_str(" — ");
            out.push_str(&typ_str(&severity_label(&f.severity)));
            out.push_str("\n\n");
            out.push_str(&typ_str(&f.deviation));
            out.push_str("\n\n");
            if let Some(redline) = f.suggested_redline.as_deref().filter(|s| !s.is_empty()) {
                out.push_str("*Suggested redline:* ");
                out.push_str(&typ_str(redline));
                out.push_str("\n\n");
            }
            if let Some(note) = f.attorney_note.as_deref().filter(|s| !s.is_empty()) {
                out.push_str("*Attorney note:* ");
                out.push_str(&typ_str(note));
                out.push_str("\n\n");
            }
        }
    }
    out.push_str("\n== Scope and disclaimers\n");
    out.push_str("This memo measures the contract against the ");
    out.push_str(&typ_str(input.playbook_name));
    out.push_str(
        " playbook only — it is not a full audit. A clause this memo does not flag is not \
         thereby approved; silence means the clause was outside the playbook's scope. A \
         licensed Neon Law attorney has reviewed and is accountable for every finding above. \
         To produce the review, the contract text was processed through a zero-retention AI \
         service that is not trained on Company data; the contract and this memo are \
         confidential.\n",
    );
    out
}

/// Insert `s` as a Typst string-literal expression (`#"…"`), escaping the two
/// characters that are significant inside a Typst string. In content (markup)
/// context this displays the string's characters verbatim, with no markup
/// interpretation — so arbitrary text is safe.
fn typ_str(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("#\"{escaped}\"")
}

fn severity_label(severity: &str) -> String {
    match severity {
        SEVERITY_HIGH => "High severity".to_string(),
        SEVERITY_MEDIUM => "Medium severity".to_string(),
        SEVERITY_LOW => "Low severity".to_string(),
        other => other.to_string(),
    }
}

// --- attribution -----------------------------------------------------------

/// Append one immutable per-finding decision to `notation_events`.
async fn record_finding_decision(
    surreal: &store::surreal::SurrealDb,
    notation_id: Uuid,
    idx: usize,
    clause_ref: &str,
    accepted: bool,
    acting_person_id: Option<Uuid>,
) {
    let payload = serde_json::json!({
        "index": idx,
        "clause_ref": clause_ref,
        "accepted": accepted,
        "acting_person_id": acting_person_id,
    })
    .to_string();
    let condition = if accepted {
        FINDING_ACCEPTED
    } else {
        FINDING_REJECTED
    };
    let recorded_at = chrono::Utc::now().to_rfc3339();
    let event = TransitionRecord {
        notation_id,
        acting_person_id,
        machine_kind: MACHINE_CONTRACT_REVIEW,
        from_state: "lawyer_review",
        to_state: "lawyer_review",
        condition,
        payload_json: Some(payload),
        recorded_at: &recorded_at,
    };
    if let Err(e) = append_event(surreal, event).await {
        tracing::error!(error = %e, %notation_id, idx, "record finding decision failed");
    }
}

// --- small helpers ---------------------------------------------------------

fn redirect_to(review_id: Uuid) -> Response {
    Redirect::to(&format!("/app/lawyer/contract-reviews/{review_id}")).into_response()
}

fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

fn is_severity(s: &str) -> bool {
    matches!(
        s.to_lowercase().as_str(),
        SEVERITY_LOW | SEVERITY_MEDIUM | SEVERITY_HIGH
    )
}

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        webapp::error_pages::not_found_signed_in(),
    )
        .into_response()
}

async fn sync_notation_state(
    surreal: &store::surreal::SurrealDb,
    notation_id: Uuid,
    new_state: &str,
) -> Result<(), store::notations::NotationError> {
    store::notations::update_state(surreal, notation_id, new_state).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header::LOCATION;
    use store::contract_reviews::Finding;
    use store::playbooks::SEVERITY_HIGH;

    fn finding(clause: &str, deviation: &str) -> Finding {
        Finding {
            clause_ref: clause.into(),
            deviation: deviation.into(),
            severity: SEVERITY_HIGH.into(),
            suggested_redline: Some("Add a mutual cap.".into()),
            attorney_note: Some("Push this.".into()),
            accepted: true,
        }
    }

    #[test]
    fn typ_str_escapes_quotes_and_backslashes() {
        assert_eq!(typ_str("a\"b\\c"), "#\"a\\\"b\\\\c\"");
    }

    #[test]
    fn memo_includes_playbook_summary_and_findings_and_disclaimers() {
        let f = finding("§7.2 Liability", "Liability is uncapped.");
        let refs = [&f];
        let typ = assemble_memo_typst(&MemoInput {
            playbook_name: "Vendor MSA",
            risk_summary: "One high-severity deviation.",
            accepted_findings: &refs,
        });
        assert!(typ.contains("Vendor MSA"));
        assert!(typ.contains("One high-severity deviation."));
        assert!(typ.contains("§7.2 Liability"));
        assert!(typ.contains("Suggested redline:"));
        assert!(typ.contains("not a full audit"));
        assert!(typ.contains("zero-retention AI"));
    }

    #[test]
    fn memo_with_markup_chars_renders_to_a_real_pdf() {
        // Arbitrary attorney text full of Typst metacharacters must not break
        // the compile — proving the `#"…"` insertion is safe.
        let f = Finding {
            clause_ref: "#1 *Indemnity* [draft]".into(),
            deviation: "Caps at $0; see § 9.1 _and_ <Exhibit A> @ref `code`.".into(),
            severity: SEVERITY_HIGH.into(),
            suggested_redline: Some("Replace with = mutual cap #here".into()),
            attorney_note: None,
            accepted: true,
        };
        let refs = [&f];
        let typ = assemble_memo_typst(&MemoInput {
            playbook_name: "Edge \"quoted\" \\ playbook",
            risk_summary: "Summary with # and * and $.",
            accepted_findings: &refs,
        });
        let pdf = pdf::render(&typ).expect("memo Typst compiles to a PDF");
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn memo_with_no_accepted_findings_states_so() {
        let typ = assemble_memo_typst(&MemoInput {
            playbook_name: "P",
            risk_summary: "Nothing material.",
            accepted_findings: &[],
        });
        assert!(typ.contains("No deviations were flagged"));
        assert!(pdf::render(&typ).is_ok());
    }

    // --- pure helpers --------------------------------------------------------

    #[test]
    fn review_action_errors_name_the_gate_that_refused() {
        assert_eq!(
            ReviewActionError::NotFoundOrScoped.to_string(),
            "contract review not found"
        );
        assert_eq!(
            ReviewActionError::NotOpen.to_string(),
            "contract review is not at the required gate"
        );
        assert_eq!(
            ReviewActionError::FindingNotFound.to_string(),
            "no such finding"
        );
        assert!(ReviewActionError::FindingsUnacted
            .to_string()
            .starts_with("every finding must be accepted or rejected"));
        assert!(ReviewActionError::DocumentCommentsUnacted
            .to_string()
            .starts_with("every document comment must be accepted"));
        assert_eq!(
            ReviewActionError::Db("boom".into()).to_string(),
            "database: boom"
        );
    }

    #[test]
    fn protected_kinds_maps_known_names_and_drops_the_rest() {
        use store::notation_documents::ProtectedTokenKind;
        let kinds = protected_kinds(&[
            "number".into(),
            "percentage".into(),
            "currency".into(),
            "date".into(),
            "signature".into(),
        ]);
        assert_eq!(
            kinds,
            vec![
                ProtectedTokenKind::Number,
                ProtectedTokenKind::Percentage,
                ProtectedTokenKind::Currency,
                ProtectedTokenKind::Date,
            ]
        );
        assert!(protected_kinds(&[]).is_empty());
    }

    #[test]
    fn small_helpers_normalize_attorney_input() {
        assert_eq!(non_empty("  keep  "), Some("keep".into()));
        assert_eq!(non_empty("   "), None);
        assert!(is_severity("HIGH"));
        assert!(is_severity("medium"));
        assert!(is_severity("Low"));
        assert!(!is_severity("critical"));
        assert_eq!(severity_label(SEVERITY_HIGH), "High severity");
        assert_eq!(severity_label(SEVERITY_MEDIUM), "Medium severity");
        assert_eq!(severity_label(SEVERITY_LOW), "Low severity");
        assert_eq!(severity_label("custom"), "custom");
        let id = Uuid::nil();
        assert_eq!(
            memo_storage_key(id),
            format!("notations/{id}/review-memo.pdf")
        );
    }

    #[test]
    fn redirects_point_back_at_the_review_surface() {
        let id = Uuid::now_v7();
        let back = redirect_to(id);
        assert_eq!(back.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            location(&back),
            format!("/app/lawyer/contract-reviews/{id}")
        );
        let doc = document_redirect(id, "saved");
        assert_eq!(
            location(&doc),
            format!("/app/lawyer/contract-reviews/{id}/document?error=saved")
        );
        assert_eq!(not_found().status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn memo_omits_empty_redline_and_note_and_labels_severity() {
        let f = Finding {
            clause_ref: "§3 Term".into(),
            deviation: "Auto-renews.".into(),
            severity: SEVERITY_LOW.into(),
            suggested_redline: Some(String::new()),
            attorney_note: Some("Client accepts renewal.".into()),
            accepted: true,
        };
        let refs = [&f];
        let typ = assemble_memo_typst(&MemoInput {
            playbook_name: "P",
            risk_summary: "Minor.",
            accepted_findings: &refs,
        });
        assert!(typ.contains("Low severity"));
        assert!(!typ.contains("Suggested redline:"));
        assert!(typ.contains("Attorney note:"));
        assert!(typ.contains("Client accepts renewal."));
    }

    #[test]
    fn changed_blocks_reports_only_edited_editable_blocks() {
        let original = clause_document();
        assert!(changed_blocks(&original, &original).is_empty());
        let edited = original
            .with_block_text("a1", "The fee is 200 dollars.")
            .expect("paragraph a1 is editable");
        let changes = changed_blocks(&edited, &original);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].anchor, "a1");
        assert_eq!(changes[0].replacement_text, "The fee is 200 dollars.");
        assert_eq!(changes[0].author, "Neon Law attorney");
    }

    // --- review commands against an embedded store -------------------------

    const CLAUSE_MARKDOWN: &str = concat!(
        "<!-- navigator-story kind=\"main_document\" part=\"/word/document.xml\" -->\n",
        "<!-- navigator-block kind=\"paragraph\" anchor=\"a1\" -->\n",
        "The fee is 100 dollars.\n",
        "\n",
        "<!-- navigator-block kind=\"paragraph\" anchor=\"a2\" -->\n",
        "Either party may terminate.\n",
    );

    fn clause_document() -> word::CanonicalDocument {
        word::CanonicalDocument::from_markdown(&word::TrustedNotationMarkdown::from_governed_store(
            CLAUSE_MARKDOWN.to_string(),
        ))
    }

    struct Fixture {
        state: AdminState,
        review_id: Uuid,
        notation_id: Uuid,
        project_id: Uuid,
        admin_id: Uuid,
    }

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

    fn two_findings() -> Vec<Finding> {
        vec![
            Finding {
                clause_ref: "§7.2 Liability".into(),
                deviation: "Liability is uncapped.".into(),
                severity: SEVERITY_HIGH.into(),
                suggested_redline: None,
                attorney_note: None,
                accepted: false,
            },
            Finding {
                clause_ref: "§3 Term".into(),
                deviation: "Auto-renews for five years.".into(),
                severity: SEVERITY_MEDIUM.into(),
                suggested_redline: None,
                attorney_note: None,
                accepted: false,
            },
        ]
    }

    /// One analyzed review whose notation sits at `lawyer_review` on the
    /// bundled contract-review workflow, plus an admin reviewer.
    async fn fixture() -> Fixture {
        let db = store::surreal::test_support::mem().await;
        let notation_id = store::test_support::seed_notation(&db).await;
        let project_id = store::notations::find_by_id(&db, notation_id)
            .await
            .unwrap()
            .unwrap()
            .project_id;
        let entity_id = store::test_support::seed_entity(&db).await;
        let playbook_id = playbooks::create(
            &db,
            &playbooks::NewPlaybook {
                entity_id,
                name: "Vendor MSA",
                positions: &[],
            },
        )
        .await
        .unwrap();
        let review_id = contract_reviews::create(
            &db,
            &contract_reviews::NewContractReview {
                notation_id,
                playbook_id,
                asset_id: None,
            },
        )
        .await
        .unwrap();
        contract_reviews::record_analysis(&db, review_id, "Two deviations.", &two_findings())
            .await
            .unwrap();
        let admin_id = store::persons::create(
            &db,
            &store::persons::NewPerson::with_role(
                "Synthetic Admin",
                format!("reviewer-{}@example.com", Uuid::now_v7()),
                Role::Admin,
            ),
        )
        .await
        .unwrap()
        .id;
        let state = admin_state(crate::test_support::app_state(db).await);

        let yaml = workflows::catalog_spec_yaml(
            crate::contract_review_walk::CONTRACT_REVIEW_TEMPLATE_CODE,
        )
        .expect("bundled contract-review spec");
        let spec = workflows::workflow_spec_from_yaml(yaml).unwrap();
        let runtime = state.workflow_runtime.as_ref();
        StateMachineRuntime::start(runtime, MachineKind::Workflow, notation_id, &spec)
            .await
            .unwrap();
        for condition in [
            crate::contract_review_walk::CONTRACT_UPLOADED,
            crate::contract_review_walk::INTAKE_FILED,
            crate::contract_review_walk::ANALYSIS_READY,
        ] {
            StateMachineRuntime::signal(
                runtime,
                MachineKind::Workflow,
                notation_id,
                condition,
                None,
            )
            .await
            .unwrap();
        }
        store::notations::update_state(&state.surreal, notation_id, "lawyer_review")
            .await
            .unwrap();
        Fixture {
            state,
            review_id,
            notation_id,
            project_id,
            admin_id,
        }
    }

    impl Fixture {
        fn session(&self) -> Extension<SessionData> {
            let mut session = SessionData::fresh("reviewer@example.com", Role::Admin);
            session.person_id = Some(self.admin_id);
            Extension(session)
        }

        async fn review(&self) -> ContractReview {
            contract_reviews::by_id(&self.state.surreal, self.review_id)
                .await
                .unwrap()
                .unwrap()
        }

        async fn notation_state(&self) -> String {
            store::notations::find_by_id(&self.state.surreal, self.notation_id)
                .await
                .unwrap()
                .unwrap()
                .state
        }

        async fn decide(&self, idx: usize, accept: bool) -> Result<(), ReviewActionError> {
            save_review_finding(
                &self.state.surreal,
                self.review_id,
                idx,
                Some(self.admin_id),
                Role::Admin,
                accept,
                "",
                "",
                "",
            )
            .await
        }

        async fn approve(&self) -> Result<(), ReviewActionError> {
            approve_review(
                &self.state.surreal,
                &self.state.storage,
                self.state.workflow_runtime.as_ref(),
                self.review_id,
                Some(self.admin_id),
                Role::Admin,
            )
            .await
        }

        /// Import a governed Word source for the notation and return the
        /// root version id.
        async fn import_document(&self) -> Uuid {
            let document = clause_document();
            let manifest = serde_json::to_vec(&document.block_manifest()).unwrap();
            let markdown = document.to_markdown();
            let original_asset_id = store::assets::ingest_content(
                &self.state.surreal,
                &self.state.storage,
                b"PK\x03\x04 synthetic docx",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            )
            .await
            .unwrap();
            store::notation_documents::import_root(
                &self.state.surreal,
                &self.state.storage,
                store::notation_documents::ImportRoot {
                    project_id: self.project_id,
                    notation_id: self.notation_id,
                    original_asset_id,
                    markdown: markdown.as_ref().as_bytes(),
                    anchor_manifest: &manifest,
                    parser_version: "word-protocol-1",
                    schema_version: 1,
                    authored_by_person_id: self.admin_id,
                    recorded_at: "2026-11-02T10:00:00+00:00",
                },
            )
            .await
            .unwrap()
            .into_model()
            .id
        }

        async fn current_version_id(&self) -> Uuid {
            store::notation_documents::current_version(
                &self.state.surreal,
                self.project_id,
                self.notation_id,
            )
            .await
            .unwrap()
            .expect("a current version")
            .id
        }

        async fn edit(&self, parent: &str, anchor: &str, text: &str, locked: &[&str]) -> Response {
            save_document_edit_route(
                State(self.state.clone()),
                Path(self.review_id),
                Some(self.session()),
                Form(DocumentEditForm {
                    expected_parent_version_id: parent.to_string(),
                    anchor: anchor.to_string(),
                    text: text.to_string(),
                    locked_kinds: locked.iter().map(|k| (*k).to_string()).collect(),
                }),
            )
            .await
        }

        async fn comment(&self, anchor: &str, body: &str) -> Response {
            add_document_comment_route(
                State(self.state.clone()),
                Path(self.review_id),
                Some(self.session()),
                Form(DocumentCommentForm {
                    anchor: anchor.to_string(),
                    body: body.to_string(),
                }),
            )
            .await
        }

        async fn decide_comment(&self, comment_id: Uuid, decision: &str) -> Response {
            decide_document_comment_route(
                State(self.state.clone()),
                Path((self.review_id, comment_id)),
                Some(self.session()),
                Form(DocumentDecisionForm {
                    decision: decision.to_string(),
                }),
            )
            .await
        }
    }

    fn location(response: &Response) -> &str {
        response
            .headers()
            .get(LOCATION)
            .expect("a redirect location")
            .to_str()
            .unwrap()
    }

    fn document_error(response: &Response) -> &str {
        location(response)
            .rsplit_once("error=")
            .map(|(_, code)| code)
            .expect("an error code")
    }

    fn finding_edit(decision: &str, severity: &str) -> FindingEdit {
        FindingEdit {
            decision: decision.to_string(),
            severity: severity.to_string(),
            suggested_redline: "  Cap at fees paid.  ".to_string(),
            attorney_note: "   ".to_string(),
        }
    }

    #[tokio::test]
    async fn a_client_an_unassigned_lawyer_and_a_missing_review_all_read_as_not_found() {
        let f = fixture().await;
        let lawyer_id = store::persons::create(
            &f.state.surreal,
            &store::persons::NewPerson::with_role(
                "Synthetic Lawyer",
                format!("lawyer-{}@example.com", Uuid::now_v7()),
                Role::Lawyer,
            ),
        )
        .await
        .unwrap()
        .id;
        for (review_id, person_id, role) in [
            (f.review_id, Some(f.admin_id), Role::Client),
            (f.review_id, Some(lawyer_id), Role::Lawyer),
            (Uuid::now_v7(), Some(f.admin_id), Role::Admin),
        ] {
            let result =
                save_review_summary(&f.state.surreal, review_id, person_id, role, "nothing").await;
            assert!(
                matches!(result, Err(ReviewActionError::NotFoundOrScoped)),
                "{role:?}"
            );
        }
        assert_eq!(
            f.review().await.risk_summary.as_deref(),
            Some("Two deviations.")
        );
    }

    #[tokio::test]
    async fn saving_a_finding_records_the_decision_and_normalizes_the_edit() {
        let f = fixture().await;
        save_review_finding(
            &f.state.surreal,
            f.review_id,
            0,
            Some(f.admin_id),
            Role::Admin,
            true,
            "MEDIUM",
            "  Cap at fees paid.  ",
            "   ",
        )
        .await
        .unwrap();
        // An unknown severity keeps the machine-proposed one.
        save_review_finding(
            &f.state.surreal,
            f.review_id,
            1,
            Some(f.admin_id),
            Role::Admin,
            false,
            "critical",
            "",
            " Client accepts the renewal. ",
        )
        .await
        .unwrap();

        let findings = contract_reviews::findings_of(&f.review().await).unwrap();
        assert!(findings[0].accepted);
        assert_eq!(findings[0].severity, SEVERITY_MEDIUM);
        assert_eq!(
            findings[0].suggested_redline.as_deref(),
            Some("Cap at fees paid.")
        );
        assert_eq!(findings[0].attorney_note, None);
        assert!(!findings[1].accepted);
        assert_eq!(findings[1].severity, SEVERITY_MEDIUM);
        assert_eq!(
            findings[1].attorney_note.as_deref(),
            Some("Client accepts the renewal.")
        );

        let acted = contract_reviews::acted_finding_indices(&f.state.surreal, f.notation_id)
            .await
            .unwrap();
        assert!(acted.contains(&0) && acted.contains(&1));

        assert!(matches!(
            f.decide(2, true).await,
            Err(ReviewActionError::FindingNotFound)
        ));
    }

    #[tokio::test]
    async fn the_summary_is_trimmed_before_it_is_stored() {
        let f = fixture().await;
        save_review_summary(
            &f.state.surreal,
            f.review_id,
            Some(f.admin_id),
            Role::Admin,
            "  One material deviation.  ",
        )
        .await
        .unwrap();
        assert_eq!(
            f.review().await.risk_summary.as_deref(),
            Some("One material deviation.")
        );
    }

    #[tokio::test]
    async fn approval_waits_for_every_finding_then_files_the_memo_and_ends() {
        let f = fixture().await;
        assert!(matches!(
            f.approve().await,
            Err(ReviewActionError::FindingsUnacted)
        ));
        f.decide(0, true).await.unwrap();
        assert!(matches!(
            f.approve().await,
            Err(ReviewActionError::FindingsUnacted)
        ));
        f.decide(1, false).await.unwrap();

        f.approve().await.unwrap();
        assert_eq!(f.review().await.status, contract_reviews::STATUS_APPROVED);
        assert_eq!(f.notation_state().await, "END");
        // A second approval finds the gate closed.
        assert!(matches!(f.approve().await, Err(ReviewActionError::NotOpen)));
        // A closed review takes no more finding edits.
        assert!(matches!(
            f.decide(0, false).await,
            Err(ReviewActionError::NotOpen)
        ));
    }

    #[tokio::test]
    async fn rejection_ends_the_workflow_without_a_memo() {
        let f = fixture().await;
        reject_review(
            &f.state.surreal,
            f.state.workflow_runtime.as_ref(),
            f.review_id,
            Some(f.admin_id),
            Role::Admin,
        )
        .await
        .unwrap();
        assert_eq!(f.review().await.status, contract_reviews::STATUS_REJECTED);
        assert_eq!(f.notation_state().await, "END");
        let again = reject_review(
            &f.state.surreal,
            f.state.workflow_runtime.as_ref(),
            f.review_id,
            Some(f.admin_id),
            Role::Admin,
        )
        .await;
        assert!(matches!(again, Err(ReviewActionError::NotOpen)));
    }

    #[tokio::test]
    async fn the_form_routes_read_a_missing_session_or_review_as_not_found() {
        let f = fixture().await;
        let missing = Uuid::now_v7();

        // No session: every lawyer form reads as not found.
        let r = save_finding(
            State(f.state.clone()),
            Path((f.review_id, 0)),
            None,
            Form(finding_edit("accept", "high")),
        )
        .await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        let r = save_summary(
            State(f.state.clone()),
            Path(f.review_id),
            None,
            Form(SummaryEdit {
                risk_summary: "x".into(),
            }),
        )
        .await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            approve(State(f.state.clone()), Path(f.review_id), None)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            reject(State(f.state.clone()), Path(f.review_id), None)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );

        // Unknown review or finding: not found.
        let r = save_finding(
            State(f.state.clone()),
            Path((missing, 0)),
            Some(f.session()),
            Form(finding_edit("accept", "high")),
        )
        .await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        let r = save_finding(
            State(f.state.clone()),
            Path((f.review_id, 9)),
            Some(f.session()),
            Form(finding_edit("accept", "high")),
        )
        .await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        let r = save_summary(
            State(f.state.clone()),
            Path(missing),
            Some(f.session()),
            Form(SummaryEdit {
                risk_summary: "x".into(),
            }),
        )
        .await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            approve(State(f.state.clone()), Path(missing), Some(f.session()))
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            reject(State(f.state.clone()), Path(missing), Some(f.session()))
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn the_form_routes_redirect_back_after_each_save_and_once_closed() {
        let f = fixture().await;

        // Approving with an unacted finding redirects with the reason.
        let r = approve(State(f.state.clone()), Path(f.review_id), Some(f.session())).await;
        assert_eq!(r.status(), StatusCode::SEE_OTHER);
        assert!(location(&r).contains("?error=Every%20finding"));

        // Saves redirect back to the review.
        let back = format!("/app/lawyer/contract-reviews/{}", f.review_id);
        let r = save_finding(
            State(f.state.clone()),
            Path((f.review_id, 0)),
            Some(f.session()),
            Form(finding_edit("accept", "low")),
        )
        .await;
        assert_eq!(location(&r), back);
        assert_eq!(
            contract_reviews::findings_of(&f.review().await).unwrap()[0].severity,
            SEVERITY_LOW
        );
        let r = save_summary(
            State(f.state.clone()),
            Path(f.review_id),
            Some(f.session()),
            Form(SummaryEdit {
                risk_summary: " Edited. ".into(),
            }),
        )
        .await;
        assert_eq!(location(&r), back);
        assert_eq!(f.review().await.risk_summary.as_deref(), Some("Edited."));

        // Reject, then every later form is a no-op redirect back.
        let r = reject(State(f.state.clone()), Path(f.review_id), Some(f.session())).await;
        assert_eq!(location(&r), back);
        let r = reject(State(f.state.clone()), Path(f.review_id), Some(f.session())).await;
        assert_eq!(location(&r), back);
        let r = approve(State(f.state.clone()), Path(f.review_id), Some(f.session())).await;
        assert_eq!(location(&r), back);
        let r = save_finding(
            State(f.state.clone()),
            Path((f.review_id, 1)),
            Some(f.session()),
            Form(finding_edit("reject", "high")),
        )
        .await;
        assert_eq!(location(&r), back);
    }

    #[tokio::test]
    async fn the_approve_route_files_the_memo_once_every_finding_is_decided() {
        let f = fixture().await;
        f.decide(0, true).await.unwrap();
        f.decide(1, true).await.unwrap();
        let r = approve(State(f.state.clone()), Path(f.review_id), Some(f.session())).await;
        assert_eq!(
            location(&r),
            format!("/app/lawyer/contract-reviews/{}", f.review_id)
        );
        assert_eq!(f.review().await.status, contract_reviews::STATUS_APPROVED);
        assert_eq!(f.notation_state().await, "END");
    }

    #[tokio::test]
    async fn document_routes_without_a_session_or_a_source_read_as_not_found() {
        let f = fixture().await;
        let parent = Uuid::now_v7().to_string();
        let r = save_document_edit_route(
            State(f.state.clone()),
            Path(f.review_id),
            None,
            Form(DocumentEditForm {
                expected_parent_version_id: parent.clone(),
                anchor: "a1".into(),
                text: "x".into(),
                locked_kinds: Vec::new(),
            }),
        )
        .await;
        assert_eq!(document_error(&r), "not-found");
        let r = add_document_comment_route(
            State(f.state.clone()),
            Path(f.review_id),
            None,
            Form(DocumentCommentForm {
                anchor: "a1".into(),
                body: "x".into(),
            }),
        )
        .await;
        assert_eq!(document_error(&r), "not-found");
        let r = decide_document_comment_route(
            State(f.state.clone()),
            Path((f.review_id, Uuid::now_v7())),
            None,
            Form(DocumentDecisionForm {
                decision: "accepted".into(),
            }),
        )
        .await;
        assert_eq!(document_error(&r), "not-found");

        // A session but no imported Word source.
        assert_eq!(
            document_error(&f.edit(&parent, "a1", "x", &[]).await),
            "not-found"
        );
        assert_eq!(
            document_error(&f.edit("not-a-uuid", "a1", "x", &[]).await),
            "conflict"
        );
        assert_eq!(
            document_error(&f.comment("a1", "Look here.").await),
            "not-found"
        );
        assert_eq!(
            document_error(&f.decide_comment(Uuid::now_v7(), "accepted").await),
            "decision"
        );
    }

    #[tokio::test]
    async fn a_document_edit_appends_a_version_and_guards_locked_tokens() {
        let f = fixture().await;
        let root = f.import_document().await.to_string();

        assert_eq!(
            document_error(&f.edit(&Uuid::now_v7().to_string(), "a1", "x", &[]).await),
            "conflict"
        );
        assert_eq!(
            document_error(
                &f.edit(&root, "a1", "The fee is 900 dollars.", &["number"])
                    .await
            ),
            "protected"
        );
        assert_eq!(
            document_error(&f.edit(&root, "missing-anchor", "x", &[]).await),
            "unsupported"
        );
        assert_eq!(f.current_version_id().await.to_string(), root);

        assert_eq!(
            document_error(
                &f.edit(
                    &root,
                    "a2",
                    "Either party may terminate on notice.",
                    &["number"]
                )
                .await
            ),
            "saved"
        );
        let child = f.current_version_id().await;
        assert_ne!(child.to_string(), root);
        let version = store::notation_documents::current_version(
            &f.state.surreal,
            f.project_id,
            f.notation_id,
        )
        .await
        .unwrap()
        .unwrap();
        let markdown =
            store::notation_documents::markdown_of(&f.state.surreal, &f.state.storage, &version)
                .await
                .unwrap();
        assert!(markdown.contains("Either party may terminate on notice."));
        // The root is no longer the parent a save may name.
        assert_eq!(
            document_error(&f.edit(&root, "a1", "x", &[]).await),
            "conflict"
        );
    }

    #[tokio::test]
    async fn document_comments_must_be_decided_before_approval() {
        let f = fixture().await;
        f.import_document().await;
        f.decide(0, true).await.unwrap();
        f.decide(1, false).await.unwrap();

        assert_eq!(
            document_error(&f.comment("missing-anchor", "Look here.").await),
            "unsupported"
        );
        assert_eq!(document_error(&f.comment("a1", "   ").await), "unsupported");
        assert_eq!(
            document_error(&f.comment("a1", "  Confirm the fee.  ").await),
            "comment-added"
        );

        let version = f.current_version_id().await;
        let comments = store::notation_documents::comments_for_version(
            &f.state.surreal,
            f.project_id,
            f.notation_id,
            version,
        )
        .await
        .unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].body, "Confirm the fee.");
        let comment_id = comments[0].id;

        assert!(matches!(
            f.approve().await,
            Err(ReviewActionError::DocumentCommentsUnacted)
        ));
        let r = approve(State(f.state.clone()), Path(f.review_id), Some(f.session())).await;
        assert!(location(&r).contains("?error=Every%20document%20comment"));

        assert_eq!(
            document_error(&f.decide_comment(comment_id, "maybe").await),
            "decision"
        );
        assert_eq!(
            document_error(&f.decide_comment(comment_id, "accepted").await),
            "decided"
        );
        assert_eq!(
            document_error(&f.decide_comment(comment_id, "rejected").await),
            "decision"
        );
    }

    #[tokio::test]
    async fn document_routes_report_a_closed_review() {
        let f = fixture().await;
        let root = f.import_document().await.to_string();
        reject_review(
            &f.state.surreal,
            f.state.workflow_runtime.as_ref(),
            f.review_id,
            Some(f.admin_id),
            Role::Admin,
        )
        .await
        .unwrap();
        assert_eq!(
            document_error(&f.edit(&root, "a1", "x", &[]).await),
            "closed"
        );
        assert_eq!(document_error(&f.comment("a1", "Late.").await), "closed");
        assert_eq!(
            document_error(&f.decide_comment(Uuid::now_v7(), "accepted").await),
            "closed"
        );
    }
}

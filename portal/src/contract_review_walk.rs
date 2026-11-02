//! Inbound contract review — the web-driven upload + analysis pipeline.
//!
//! A client (or lawyer acting for the client) uploads a third-party contract
//! into an existing Project; this module then:
//!
//!   1. opens a `memo__contract_review` notation and files the contract
//!      through the workflow — `contract_uploaded` lands on
//!      `document_intake__inbound_contract`, and the worker writes the
//!      content-addressed blob + `documents` row (the same
//!      [`store::documents::ingest_bytes`] seam the transcript intake uses);
//!   2. drives the analysis web-side — `intake_filed` lands on
//!      `analysis__contract_deviations`, where it loads the client Entity's
//!      [playbook](store::playbooks), runs the
//!      [`ContractReviewer`](crate::contract_review::ContractReviewer)
//!      against the contract text, opens a `contract_reviews` row and records
//!      the findings, then signals `analysis_ready` to land the matter at
//!      `lawyer_review` for an attorney.
//!
//! Analysis runs here, not in `workflows-service`, because the LLM seam
//! lives in `web` only — KIND and the tests use the deterministic
//! [`StubContractReviewer`](crate::contract_review::StubContractReviewer).
//! `web` drives the post-intake transitions once the provided artifact is
//! filed.
//!
//! Authorization: the upload route is row-scoped to the Project (a
//! non-participant gets `404`, never `403`); the admin review surface lives
//! under `/app/lawyer/*` and is gated by embedded Rego policy's `lawyer_tier` rule.

use std::sync::Arc;

use axum::extract::{Extension, Multipart, Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use base64::Engine as _;
use chrono::Utc;
use tower_cookies::Cookies;
use uuid::Uuid;
use workflows::{IntakeArtifact, IntakePayload, MachineKind, StateMachineRuntime};

use store::playbooks::{self, Playbook};

use crate::admin::AdminState;
use crate::contract_review::ReviewError;
use crate::session::SessionData;

/// The condition that, fired from `BEGIN`, lands the contract-review
/// workflow on `document_intake__inbound_contract`. Kept in one place so the
/// upload handler and its test agree.
pub(crate) const CONTRACT_UPLOADED: &str = "contract_uploaded";
/// The condition out of `document_intake__inbound_contract` — the worker has
/// filed the blob; web now drives the analysis.
pub(crate) const INTAKE_FILED: &str = "intake_filed";
/// The condition out of `analysis__contract_deviations` — web has produced
/// the findings; the matter lands at `lawyer_review`.
pub(crate) const ANALYSIS_READY: &str = "analysis_ready";
/// Template code for the inbound-contract-review matter.
pub(crate) const CONTRACT_REVIEW_TEMPLATE_CODE: &str = "memo__contract_review";
/// `documents.kind` (and the `document_intake__<slug>` slug) for the filed
/// inbound contract.
pub(crate) const INBOUND_CONTRACT_KIND: &str = "inbound_contract";

/// Failure of the contract-review pipeline. The upload handler maps each to
/// an HTTP response.
#[derive(Debug, thiserror::Error)]
pub enum ContractReviewError {
    #[error("database: {0}")]
    Db(String),
    #[error("asset: {0}")]
    Asset(#[from] store::assets::AssetError),
    #[error("document ingest: {0}")]
    DocumentIngest(#[from] store::documents::IngestError),
    #[error("decode document bytes: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("contract artifact: {0}")]
    Artifact(String),
    #[error("Word package: {0}")]
    Word(#[from] word::WordError),
    #[error("template: {0}")]
    Template(#[from] store::templates::TemplateError),
    #[error("workflow runtime: {0}")]
    Runtime(#[from] workflows::WorkflowRuntimeError),
    #[error("contract-review template not seeded")]
    TemplateMissing,
    #[error("workflow spec: {0}")]
    Spec(String),
    #[error("questionnaire snapshot: {0}")]
    Snapshot(String),
    #[error("serialize intake payload: {0}")]
    Payload(serde_json::Error),
    #[error("serialize Word anchor manifest: {0}")]
    Manifest(serde_json::Error),
    #[error("Notation document: {0}")]
    NotationDocument(#[from] store::notation_documents::NotationDocumentError),
    #[error("no active playbook on file for this Entity")]
    NoPlaybook,
    #[error("playbook positions malformed: {0}")]
    Positions(serde_json::Error),
    #[error("reviewer: {0}")]
    Reviewer(#[from] ReviewError),
    #[error("notation {0} vanished mid-pipeline")]
    NotationMissing(Uuid),
}

impl From<String> for ContractReviewError {
    fn from(message: String) -> Self {
        Self::Db(message)
    }
}

/// `POST /app/projects/{project_code}/contract-review` — upload an inbound contract
/// for playbook review.
///
/// Opens a `memo__contract_review` notation on the project, files the
/// uploaded contract, runs the deviation analysis against the client
/// Entity's playbook, and lands the matter at `lawyer_review`. Row-scoped: a
/// caller who can't see the project gets `404`.
pub async fn upload(
    State(state): State<AdminState>,
    AxumPath(project_code): AxumPath<String>,
    cookies: Cookies,
    session: Option<Extension<SessionData>>,
    mut multipart: Multipart,
) -> Response {
    let Some(Extension(session)) = session else {
        return not_found();
    };
    let Some(project_id) = store::projects::id_for_code(&state.surreal, &project_code).await else {
        return not_found();
    };

    let Some(person_id) = session.person_id else {
        return not_found();
    };
    if !store::access::can_see_project(&state.surreal, Some(person_id), session.role, project_id)
        .await
        .unwrap_or(false)
    {
        return not_found();
    }

    let redirect_back = crate::dioxus_app::project_show_path(&state.surreal, project_id).await;

    // Verify the `_csrf` first field before reading the contract bytes —
    // the multipart equivalent of the middleware's form-field check.
    // Bearer callers carry no cookie and are exempt.
    if let Err(status) =
        crate::csrf::require_multipart_csrf(&cookies, &session, &mut multipart).await
    {
        return status.into_response();
    }

    // The contract bytes + text. We capture the text now (for analysis)
    // rather than reading the blob back, so the analysis never races the
    // worker's durable file write (mirrors the transcript path).
    let Some(form) = parse_form(multipart).await else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(artifact) = form.into_artifact() else {
        return Redirect::to(&redirect_back).into_response();
    };
    let Ok(contract_text) = contract_text_for_review(&artifact).await else {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            "This document could not be read for review.",
        )
            .into_response();
    };
    let filename = artifact.default_filename();

    let deps = ReviewDeps {
        surreal: &state.surreal,
        workflow_runtime: state.workflow_runtime.as_ref(),
        storage: &state.storage,
        contract_reviewer: state.contract_reviewer.as_ref(),
    };
    match drive_contract_review(
        &deps,
        project_id,
        person_id,
        &filename,
        &contract_text,
        artifact.into_workflow_artifact(),
    )
    .await
    {
        Ok(review_id) => {
            Redirect::to(&format!("/app/lawyer/contract-reviews/{review_id}")).into_response()
        }
        Err(ContractReviewError::NoPlaybook) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "This Company has no contract-review playbook on file yet. An attorney must \
             create one under Admin → Playbooks before a contract can be reviewed.",
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, %project_id, "contract-review upload failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal").into_response()
        }
    }
}

async fn contract_text_for_review(
    artifact: &ParsedArtifact,
) -> Result<String, ContractReviewError> {
    if let ParsedArtifact::File {
        bytes: _, filename, ..
    } = artifact
    {
        if word::is_docx_filename(filename) {
            let adapter = word::ManagedAdapter::from_env();
            return contract_text_for_review_with_adapter(artifact, &adapter).await;
        }
    }
    Ok(artifact.contract_text())
}

async fn contract_text_for_review_with_adapter<A: word::WordAdapter + ?Sized>(
    artifact: &ParsedArtifact,
    adapter: &A,
) -> Result<String, ContractReviewError> {
    match artifact {
        ParsedArtifact::File {
            bytes, filename, ..
        } if word::is_docx_filename(filename) => {
            Ok(word::parse_with_adapter(adapter, filename, bytes)
                .await?
                .accepted_view_text())
        }
        _ => Ok(artifact.contract_text()),
    }
}

/// Open the notation, file the contract, run the analysis, and land at
/// `lawyer_review`. Returns the new `contract_reviews` row id.
///
/// Public so the integration tests can drive the pipeline without crafting
/// a multipart HTTP request.
///
/// # Errors
///
/// [`ContractReviewError`] when the Entity has no playbook, the template
/// isn't seeded, the reviewer fails, or a database/runtime call errors.
/// The dependencies the contract-review pipeline ([`drive_contract_review`])
/// borrows from whichever web state drives it — the lawyer/portal `AdminState`
/// or the REST `ApiState`. Decoupling the core from either concrete state lets
/// both doors run the deviation analysis identically.
pub struct ReviewDeps<'a> {
    pub surreal: &'a store::surreal::SurrealDb,
    pub workflow_runtime: &'a dyn StateMachineRuntime,
    pub storage: &'a Arc<dyn cloud::StorageService>,
    pub contract_reviewer: &'a dyn crate::contract_review::ContractReviewer,
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub async fn drive_contract_review(
    deps: &ReviewDeps<'_>,
    project_id: Uuid,
    person_id: Uuid,
    filename: &str,
    contract_text: &str,
    artifact: IntakeArtifact,
) -> Result<Uuid, ContractReviewError> {
    let runtime = deps.workflow_runtime;

    // The Entity (the Company) the contract is reviewed for, and its
    // playbook. Resolve the playbook first so we fail fast — before opening
    // a notation — when there is nothing to measure the contract against.
    let project_row = store::projects::find_by_id(deps.surreal, project_id)
        .await
        .map_err(|error| ContractReviewError::Db(error.to_string()))?
        .ok_or(ContractReviewError::NotationMissing(project_id))?;
    let entity_id = project_row.entity_id;
    let playbook_row = active_playbook(deps.surreal, entity_id)
        .await
        .map_err(|error| ContractReviewError::Db(error.to_string()))?
        .ok_or(ContractReviewError::NoPlaybook)?;
    let positions =
        playbooks::positions_of(&playbook_row).map_err(ContractReviewError::Positions)?;

    let (template_row, questionnaire_snapshot) =
        contract_review_template_and_snapshot(deps.surreal, deps.storage, project_id).await?;

    // File the immutable source asset before signaling the workflow. The
    // durable signal carries only this asset id; bytes, text, and the caller's
    // filename never enter the journal payload.
    let filed_artifact =
        stage_contract_artifact(deps.surreal, deps.storage, project_id, filename, &artifact)
            .await?;
    let original_asset_id = match (&artifact, &filed_artifact) {
        (IntakeArtifact::File { .. }, IntakeArtifact::Stored { asset_id }) => Some(*asset_id),
        _ => None,
    };

    // Open the notation at BEGIN, bound to the client Entity.
    let new_notation = store::notations::NewNotation::new(
        template_row.id,
        person_id,
        project_id,
        workflows::StateName::BEGIN,
    )
    .with_entity(entity_id)
    .with_delivery(store::notations::DELIVERY_EMBEDDED)
    .with_questionnaire_snapshot(questionnaire_snapshot);
    let notation_id = store::notations::create(deps.surreal, &new_notation)
        .await
        .map_err(|error| ContractReviewError::Db(error.to_string()))?
        .id;

    // A DOCX upload owns a governed Notation source before the workflow is
    // started. A parse/import failure must leave the workflow at BEGIN rather
    // than advancing a notation whose source could not be understood.
    if filename
        .rsplit_once('.')
        .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("docx"))
    {
        let IntakeArtifact::File { bytes_base64, .. } = &artifact else {
            return Err(ContractReviewError::Artifact(
                "DOCX source was not supplied as a file".to_string(),
            ));
        };
        let asset_id = original_asset_id.ok_or_else(|| {
            ContractReviewError::Artifact("DOCX source was not filed".to_string())
        })?;
        let adapter = word::ManagedAdapter::from_env();
        let bytes = base64::engine::general_purpose::STANDARD.decode(bytes_base64)?;
        let parsed = word::parse_with_adapter(&adapter, filename, &bytes).await?;
        let markdown = parsed.notation_markdown();
        let manifest = parsed
            .anchor_manifest()
            .map_err(ContractReviewError::Manifest)?;
        let recorded_at = Utc::now().to_rfc3339();
        store::notation_documents::import_root(
            deps.surreal,
            deps.storage,
            store::notation_documents::ImportRoot {
                project_id,
                notation_id,
                original_asset_id: asset_id,
                markdown: markdown.as_ref().as_bytes(),
                anchor_manifest: &manifest,
                parser_version: "word-protocol-1",
                schema_version: 1,
                authored_by_person_id: person_id,
                recorded_at: &recorded_at,
            },
        )
        .await?;
    }

    // Start the workflow and file the contract: `contract_uploaded` lands on
    // `document_intake__inbound_contract`, whose document-intake side effect
    // writes the blob + `documents` row.
    let yaml = workflows::catalog_spec_yaml(&template_row.code)
        .ok_or(ContractReviewError::TemplateMissing)?;
    let spec = workflows::workflow_spec_from_yaml(yaml)
        .map_err(|e| ContractReviewError::Spec(e.to_string()))?;
    StateMachineRuntime::start(runtime, MachineKind::Workflow, notation_id, &spec).await?;

    let payload = IntakePayload {
        kind: INBOUND_CONTRACT_KIND.to_string(),
        filename: None,
        artifact: filed_artifact,
    };
    let value = serde_json::to_string(&payload).map_err(ContractReviewError::Payload)?;
    let s = StateMachineRuntime::signal(
        runtime,
        MachineKind::Workflow,
        notation_id,
        CONTRACT_UPLOADED,
        Some(&value),
    )
    .await?;
    sync_notation_state(deps.surreal, notation_id, s.as_str())
        .await
        .map_err(|error| ContractReviewError::Db(error.to_string()))?;

    // intake_filed → analysis__contract_deviations.
    let s = StateMachineRuntime::signal(
        runtime,
        MachineKind::Workflow,
        notation_id,
        INTAKE_FILED,
        None,
    )
    .await?;
    sync_notation_state(deps.surreal, notation_id, s.as_str())
        .await
        .map_err(|error| ContractReviewError::Db(error.to_string()))?;

    // Run the deviation analysis web-side. The contract text was captured at
    // upload; the document id is a best-effort link (the worker's file write
    // is durable but may lag the analysis in prod).
    let report = deps
        .contract_reviewer
        .review(&playbook_row.name, &positions, contract_text)
        .await?;
    let asset_id = latest_document(deps.surreal, project_id, INBOUND_CONTRACT_KIND).await?;
    let review_id = store::contract_reviews::create(
        deps.surreal,
        &store::contract_reviews::NewContractReview {
            notation_id,
            playbook_id: playbook_row.id,
            asset_id,
        },
    )
    .await
    .map_err(|error| ContractReviewError::Db(error.to_string()))?;
    store::contract_reviews::record_analysis(
        deps.surreal,
        review_id,
        &report.risk_summary,
        &report.findings,
    )
    .await
    .map_err(|error| ContractReviewError::Db(error.to_string()))?;

    // analysis_ready → lawyer_review (the attorney gate).
    let s = StateMachineRuntime::signal(
        runtime,
        MachineKind::Workflow,
        notation_id,
        ANALYSIS_READY,
        None,
    )
    .await?;
    sync_notation_state(deps.surreal, notation_id, s.as_str())
        .await
        .map_err(|error| ContractReviewError::Db(error.to_string()))?;

    tracing::info!(
        %notation_id, %review_id, findings = report.findings.len(),
        "contract review: analysis complete, parked at lawyer_review"
    );
    Ok(review_id)
}

async fn stage_contract_artifact(
    surreal: &store::surreal::SurrealDb,
    storage: &Arc<dyn cloud::StorageService>,
    project_id: Uuid,
    filename: &str,
    artifact: &IntakeArtifact,
) -> Result<IntakeArtifact, ContractReviewError> {
    let (bytes, content_type) = match artifact {
        IntakeArtifact::Text { text } => (text.as_bytes().to_vec(), "text/plain".to_string()),
        IntakeArtifact::File {
            bytes_base64,
            content_type,
        } => (
            base64::engine::general_purpose::STANDARD.decode(bytes_base64)?,
            content_type.clone(),
        ),
        IntakeArtifact::Link { url } => (url.as_bytes().to_vec(), "text/uri-list".to_string()),
        IntakeArtifact::Stored { asset_id } => {
            return Ok(IntakeArtifact::Stored {
                asset_id: *asset_id,
            });
        }
    };
    let ingested = store::documents::ingest_bytes(
        surreal,
        storage,
        &store::documents::IngestArgs {
            project_id,
            source: store::documents::source::UPLOAD,
            filename,
            kind: INBOUND_CONTRACT_KIND,
            content_type: &content_type,
            description: None,
            secondary_storage_key: None,
            visibility: store::documents::visibility::INTERNAL,
        },
        &bytes,
    )
    .await?;
    Ok(IntakeArtifact::Stored {
        asset_id: ingested.asset_id,
    })
}

async fn contract_review_template_and_snapshot(
    surreal: &store::surreal::SurrealDb,
    storage: &Arc<dyn cloud::StorageService>,
    project_id: Uuid,
) -> Result<(store::templates::Template, serde_json::Value), ContractReviewError> {
    let template_row =
        store::templates::resolve(surreal, Some(project_id), CONTRACT_REVIEW_TEMPLATE_CODE)
            .await?
            .ok_or(ContractReviewError::TemplateMissing)?;
    let questionnaire_snapshot = workflows::notation_session::questionnaire_snapshot_for_template(
        surreal,
        Some(storage),
        &template_row,
    )
    .await
    .map_err(|e| ContractReviewError::Snapshot(e.to_string()))?;
    Ok((template_row, questionnaire_snapshot))
}

/// The most recent active playbook for an Entity, if any.
async fn active_playbook(
    surreal: &store::surreal::SurrealDb,
    entity_id: Uuid,
) -> Result<Option<Playbook>, store::playbooks::PlaybookError> {
    let mut rows = playbooks::for_entity(surreal, entity_id).await?;
    rows.retain(|p| p.active);
    // `for_entity` returns most-recent-first; keep that order.
    Ok(rows.into_iter().next())
}

/// The most recent document `assets` row of `kind` on the project — the
/// just-filed inbound contract. Best-effort: `None` when the worker's file
/// write hasn't landed yet (prod), in which case the review opens unlinked.
async fn latest_document(
    surreal: &store::surreal::SurrealDb,
    project_id: Uuid,
    kind: &str,
) -> Result<Option<Uuid>, store::assets::AssetError> {
    Ok(store::assets::latest_of_kind(surreal, project_id, kind)
        .await?
        .map(|d| d.id))
}

/// Mirror the runtime's resulting state onto the `notations` row (the
/// runtime is the source of truth; the row is a convenience read).
async fn sync_notation_state(
    surreal: &store::surreal::SurrealDb,
    notation_id: Uuid,
    new_state: &str,
) -> Result<(), store::notations::NotationError> {
    store::notations::update_state(surreal, notation_id, new_state).await?;
    Ok(())
}

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        webapp::error_pages::not_found_signed_in(),
    )
        .into_response()
}

// --- multipart capture ----------------------------------------------------
//
// The phone-friendly capture shape from the transcript intake, narrowed to
// what a contract upload needs: a file or a pasted text body.

/// One provided capture mode.
pub(crate) enum ParsedArtifact {
    Text(String),
    File {
        bytes: Vec<u8>,
        content_type: String,
        filename: String,
    },
}

impl ParsedArtifact {
    pub(crate) fn default_filename(&self) -> String {
        match self {
            Self::Text(_) => "inbound-contract.txt".to_string(),
            Self::File { filename, .. } => filename.clone(),
        }
    }

    /// The fallback text for non-DOCX captures: a paste directly, or a file
    /// decoded as UTF-8. DOCX captures are parsed through
    /// `contract_text_for_review`, which preserves accepted revision semantics
    /// instead of treating the package as binary text.
    pub(crate) fn contract_text(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::File { bytes, .. } => String::from_utf8(bytes.clone()).unwrap_or_default(),
        }
    }

    pub(crate) fn into_workflow_artifact(self) -> IntakeArtifact {
        match self {
            Self::Text(text) => IntakeArtifact::Text { text },
            Self::File {
                bytes,
                content_type,
                ..
            } => IntakeArtifact::File {
                bytes_base64: base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    bytes,
                ),
                content_type,
            },
        }
    }
}

#[derive(Default)]
pub(crate) struct ContractForm {
    text: Option<String>,
    file: Option<(Vec<u8>, String, String)>,
}

impl ContractForm {
    /// Prefer a file, then a text paste; blank fields are ignored.
    pub(crate) fn into_artifact(self) -> Option<ParsedArtifact> {
        if let Some((bytes, content_type, filename)) = self.file {
            if !bytes.is_empty() {
                return Some(ParsedArtifact::File {
                    bytes,
                    content_type,
                    filename,
                });
            }
        }
        if let Some(text) = self
            .text
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
        {
            return Some(ParsedArtifact::Text(text));
        }
        None
    }
}

pub(crate) async fn parse_form(mut multipart: Multipart) -> Option<ContractForm> {
    let mut form = ContractForm::default();
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(_) => return None,
        };
        match field.name().map(String::from).as_deref() {
            Some("contract_text") => form.text = field.text().await.ok(),
            Some("file") => {
                let filename = field
                    .file_name()
                    .map_or_else(|| "inbound-contract".to_string(), String::from);
                let content_type = field
                    .content_type()
                    .map_or_else(|| "application/octet-stream".to_string(), String::from);
                if let Ok(bytes) = field.bytes().await {
                    form.file = Some((bytes.to_vec(), content_type, filename));
                }
            }
            _ => {}
        }
    }
    Some(form)
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::{contract_text_for_review_with_adapter, ContractForm, ParsedArtifact};
    use word::{
        Block, DocumentModel, Inline, Paragraph, RevisionKind, RevisionNode, Story, StoryKind,
        WordAdapter,
    };

    struct FixtureAdapter;

    #[async_trait::async_trait]
    impl WordAdapter for FixtureAdapter {
        async fn parse(
            &self,
            _request: word::protocol::AdapterRequest,
        ) -> Result<word::protocol::AdapterReply, word::AdapterError> {
            let mut model = DocumentModel::empty();
            model.stories.push(Story {
                kind: StoryKind::MainDocument,
                part_uri: "/word/document.xml".into(),
                blocks: vec![Block::Paragraph(Paragraph {
                    anchor: "document:paragraph:1".into(),
                    style_id: None,
                    numbering: None,
                    nodes: vec![
                        Inline::Text {
                            text: "kept".into(),
                            style_id: None,
                            revision: None,
                        },
                        Inline::Revision {
                            revision: RevisionNode {
                                kind: RevisionKind::Insertion,
                                id: Some("1".into()),
                                anchor: "document:ins".into(),
                            },
                            children: vec![Inline::Text {
                                text: " added".into(),
                                style_id: None,
                                revision: None,
                            }],
                        },
                        Inline::Revision {
                            revision: RevisionNode {
                                kind: RevisionKind::Deletion,
                                id: Some("2".into()),
                                anchor: "document:del".into(),
                            },
                            children: vec![Inline::Text {
                                text: " removed".into(),
                                style_id: None,
                                revision: None,
                            }],
                        },
                    ],
                    revisions: Vec::new(),
                })],
            });
            Ok(word::protocol::AdapterReply::success(model))
        }
    }

    #[test]
    fn file_capture_wins_over_text_and_decodes_utf8() {
        let form = ContractForm {
            text: Some("ignored paste".into()),
            file: Some((
                b"MASTER SERVICES AGREEMENT".to_vec(),
                "text/plain".into(),
                "msa.txt".into(),
            )),
        };
        let artifact = form.into_artifact().expect("a file is present");
        assert_eq!(artifact.default_filename(), "msa.txt");
        assert_eq!(artifact.contract_text(), "MASTER SERVICES AGREEMENT");
    }

    #[test]
    fn text_paste_is_used_when_no_file() {
        let form = ContractForm {
            text: Some("  pasted contract body  ".into()),
            file: None,
        };
        let artifact = form.into_artifact().expect("a paste is present");
        assert!(matches!(artifact, ParsedArtifact::Text(_)));
        assert_eq!(artifact.contract_text(), "pasted contract body");
    }

    #[test]
    fn binary_file_decodes_to_empty_text_but_still_files() {
        let form = ContractForm {
            text: None,
            file: Some((
                vec![0xff, 0xfe, 0x00],
                "application/pdf".into(),
                "c.pdf".into(),
            )),
        };
        let artifact = form.into_artifact().expect("a file is present");
        assert_eq!(artifact.contract_text(), "");
        assert_eq!(artifact.default_filename(), "c.pdf");
    }

    #[test]
    fn empty_form_yields_nothing() {
        assert!(ContractForm::default().into_artifact().is_none());
    }

    #[tokio::test]
    async fn docx_review_uses_the_loss_aware_accepted_view() {
        let artifact = ParsedArtifact::File {
            bytes: valid_docx_container(),
            content_type: "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
                .into(),
            filename: "synthetic.docx".into(),
        };
        let text = contract_text_for_review_with_adapter(&artifact, &FixtureAdapter)
            .await
            .expect("synthetic DOCX parses through the boundary");
        assert_eq!(text, "kept added\n");
        assert!(!text.contains("removed"));
    }

    fn valid_docx_container() -> Vec<u8> {
        let mut output = std::io::Cursor::new(Vec::new());
        let mut archive = zip::ZipWriter::new(&mut output);
        let options = zip::write::SimpleFileOptions::default();
        archive.start_file("[Content_Types].xml", options).unwrap();
        archive.write_all(b"<Types/>").unwrap();
        archive.start_file("word/document.xml", options).unwrap();
        archive.write_all(b"<document/>").unwrap();
        archive.finish().unwrap();
        output.into_inner()
    }

    #[tokio::test]
    async fn a_non_docx_capture_is_reviewed_as_its_own_text() {
        let file = ParsedArtifact::File {
            bytes: b"SUPPLY AGREEMENT".to_vec(),
            content_type: "text/plain".into(),
            filename: "supply.txt".into(),
        };
        assert_eq!(
            super::contract_text_for_review(&file).await.unwrap(),
            "SUPPLY AGREEMENT"
        );
        let paste = ParsedArtifact::Text("pasted body".into());
        assert_eq!(
            super::contract_text_for_review(&paste).await.unwrap(),
            "pasted body"
        );
        assert_eq!(paste.default_filename(), "inbound-contract.txt");
        assert!(matches!(
            paste.into_workflow_artifact(),
            workflows::IntakeArtifact::Text { text } if text == "pasted body"
        ));
        let workflows::IntakeArtifact::File {
            bytes_base64,
            content_type,
        } = file.into_workflow_artifact()
        else {
            panic!("a file capture stays a file");
        };
        assert_eq!(content_type, "text/plain");
        assert_eq!(
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, bytes_base64)
                .unwrap(),
            b"SUPPLY AGREEMENT"
        );
        assert!(matches!(
            super::ContractReviewError::from("lost".to_string()),
            super::ContractReviewError::Db(message) if message == "lost"
        ));
    }

    // --- the pipeline and its upload door ---

    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use store::persons::{NewPerson, Role};
    use tower::ServiceExt;
    use uuid::Uuid;
    use workflows::IntakeArtifact;

    use super::{drive_contract_review, ContractReviewError, ReviewDeps};
    use crate::session::{now_unix_secs, SessionSource, SessionStore};

    const BOUNDARY: &str = "synthetic-contract-boundary";

    /// A matter on a fresh store, its client (a participant) and the
    /// client Entity the playbook belongs to.
    struct Matter {
        surreal: store::surreal::SurrealDb,
        code: String,
        project_id: Uuid,
        client_id: Uuid,
        entity_id: Uuid,
    }

    async fn matter() -> Matter {
        let surreal = store::test_support::mem_surreal().await;
        let entity_id = store::test_support::seed_entity(&surreal).await;
        let code = format!("contract-review-{}", Uuid::now_v7().simple());
        let project_id = store::projects::create(
            &surreal,
            &store::projects::NewProject {
                code: code.clone(),
                name: "Synthetic contract review".into(),
                status: "open".into(),
                entity_id,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .id;
        let client_id = store::persons::create(
            &surreal,
            &NewPerson::with_role(
                "Synthetic client",
                format!("contract-client-{}@example.com", Uuid::now_v7()),
                Role::Client,
            ),
        )
        .await
        .unwrap()
        .id;
        store::projects::add_participation(&surreal, project_id, client_id, "client")
            .await
            .unwrap();
        Matter {
            surreal,
            code,
            project_id,
            client_id,
            entity_id,
        }
    }

    async fn seed_template(surreal: &store::surreal::SurrealDb) {
        store::templates::save_version(
            surreal,
            None,
            super::CONTRACT_REVIEW_TEMPLATE_CODE,
            store::templates::Version {
                title: "Inbound Contract Review".into(),
                respondent_type: "person_and_entity".into(),
                asset_id: None,
                form_code: None,
                kind: None,
                source_commit_sha: None,
            },
        )
        .await
        .unwrap();
    }

    async fn seed_playbook(surreal: &store::surreal::SurrealDb, entity_id: Uuid) -> Uuid {
        let positions = vec![store::playbooks::Position {
            topic: "Limitation of liability".into(),
            preferred: "Mutual cap at 12 months' fees".into(),
            fallback: "Cap at 2x fees paid".into(),
            walkaway: "Uncapped liability".into(),
            severity: store::playbooks::SEVERITY_HIGH.into(),
        }];
        store::playbooks::create(
            surreal,
            &store::playbooks::NewPlaybook {
                entity_id,
                name: "Synthetic vendor playbook",
                positions: &positions,
            },
        )
        .await
        .unwrap()
    }

    fn bearer(person_id: Uuid, role: Role) -> String {
        let token =
            SessionStore::new(crate::test_support::TEST_SESSION_KEY).encode(&crate::SessionData {
                sub: "synthetic-contract-uploader".into(),
                email: None,
                person_id: Some(person_id),
                exp: now_unix_secs() + 60,
                role,
                csrf_token: "synthetic-csrf".into(),
                source: SessionSource::Cli,
                provider: None,
                viewing_as_dri: None,
                scope: None,
            });
        format!("Bearer {token}")
    }

    /// A multipart body of `(name, filename, content)` parts.
    fn multipart(parts: &[(&str, Option<&str>, &[u8])]) -> Vec<u8> {
        let mut body = Vec::new();
        for (name, filename, content) in parts {
            body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
            match filename {
                Some(filename) => body.extend_from_slice(
                    format!(
                        "Content-Disposition: form-data; name=\"{name}\"; \
                         filename=\"{filename}\"\r\nContent-Type: text/plain\r\n\r\n"
                    )
                    .as_bytes(),
                ),
                None => body.extend_from_slice(
                    format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
                ),
            }
            body.extend_from_slice(content);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
        body
    }

    async fn upload(
        surreal: &store::surreal::SurrealDb,
        code: &str,
        auth: &str,
        body: Vec<u8>,
    ) -> (StatusCode, String, String) {
        let app = crate::router(crate::test_support::app_state(surreal.clone()).await);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/app/projects/{code}/contract-review"))
                    .header(header::AUTHORIZATION, auth)
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={BOUNDARY}"),
                    )
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            location,
            String::from_utf8_lossy(&bytes).into_owned(),
        )
    }

    #[tokio::test]
    async fn the_upload_door_hides_a_matter_from_a_non_participant() {
        let matter = matter().await;
        let body = || multipart(&[("contract_text", None, b"A contract")]);
        let client = bearer(matter.client_id, Role::Client);
        let (status, _, _) = upload(&matter.surreal, "no-such-matter", &client, body()).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "an unknown matter");
        let stranger = store::persons::create(
            &matter.surreal,
            &NewPerson::with_role(
                "Synthetic stranger",
                format!("contract-stranger-{}@example.com", Uuid::now_v7()),
                Role::Client,
            ),
        )
        .await
        .unwrap();
        let (status, _, _) = upload(
            &matter.surreal,
            &matter.code,
            &bearer(stranger.id, Role::Client),
            body(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "a non-participant");
    }

    #[tokio::test]
    async fn the_upload_door_returns_an_empty_form_and_refuses_a_broken_one() {
        let matter = matter().await;
        let client = bearer(matter.client_id, Role::Client);
        let (status, location, _) = upload(
            &matter.surreal,
            &matter.code,
            &client,
            multipart(&[
                ("contract_text", None, b"   "),
                ("file", Some("empty.txt"), b""),
                ("ignored", None, b"value"),
            ]),
        )
        .await;
        assert!(status.is_redirection(), "{status}");
        assert_eq!(location, format!("/app/projects/{}", matter.code));
        assert!(
            store::notations::list_by_project(&matter.surreal, matter.project_id)
                .await
                .unwrap()
                .is_empty()
        );

        let truncated = format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"contract_text\"\r\n\r\nunterminated"
        );
        let (status, _, _) = upload(
            &matter.surreal,
            &matter.code,
            &client,
            truncated.into_bytes(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn the_upload_door_explains_a_missing_playbook() {
        let matter = matter().await;
        seed_template(&matter.surreal).await;
        let (status, _, body) = upload(
            &matter.surreal,
            &matter.code,
            &bearer(matter.client_id, Role::Client),
            multipart(&[("contract_text", None, b"MASTER SERVICES AGREEMENT")]),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body.contains("no contract-review playbook"), "{body}");
    }

    #[tokio::test]
    async fn an_uploaded_file_is_filed_analysed_and_parked_for_lawyer_review() {
        let matter = matter().await;
        seed_template(&matter.surreal).await;
        let playbook_id = seed_playbook(&matter.surreal, matter.entity_id).await;
        let (status, location, body) = upload(
            &matter.surreal,
            &matter.code,
            &bearer(matter.client_id, Role::Client),
            multipart(&[(
                "file",
                Some("vendor-msa.txt"),
                b"MASTER SERVICES AGREEMENT\nLiability is uncapped.",
            )]),
        )
        .await;
        assert!(status.is_redirection(), "{status}: {body}");
        let review_id: Uuid = location
            .strip_prefix("/app/lawyer/contract-reviews/")
            .expect("redirects to the review")
            .parse()
            .unwrap();
        let review = store::contract_reviews::by_id(&matter.surreal, review_id)
            .await
            .unwrap()
            .expect("the review row exists");
        assert_eq!(review.playbook_id, playbook_id);
        assert_eq!(
            store::contract_reviews::findings_of(&review).unwrap().len(),
            1,
            "one finding per playbook position"
        );
        let notation = store::notations::list_by_project(&matter.surreal, matter.project_id)
            .await
            .unwrap()
            .into_iter()
            .next()
            .expect("a notation was opened");
        assert_eq!(notation.state, "lawyer_review");
        assert_eq!(notation.entity_id, Some(matter.entity_id));
    }

    #[tokio::test]
    async fn the_pipeline_refuses_before_any_workflow_step_it_cannot_take() {
        let matter = matter().await;
        let state = crate::test_support::app_state(matter.surreal.clone()).await;
        let deps = ReviewDeps {
            surreal: &state.surreal,
            workflow_runtime: state.workflow_runtime.as_ref(),
            storage: &state.storage,
            contract_reviewer: state.contract_reviewer.as_ref(),
        };
        let text = || IntakeArtifact::Text {
            text: "contract body".into(),
        };
        let missing = Uuid::now_v7();
        let error = drive_contract_review(
            &deps,
            missing,
            matter.client_id,
            "c.txt",
            "contract body",
            text(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ContractReviewError::NotationMissing(id) if id == missing));

        seed_playbook(&matter.surreal, matter.entity_id).await;
        let error = drive_contract_review(
            &deps,
            matter.project_id,
            matter.client_id,
            "c.txt",
            "contract body",
            text(),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error, ContractReviewError::TemplateMissing),
            "{error}"
        );

        seed_template(&matter.surreal).await;
        let error = drive_contract_review(
            &deps,
            matter.project_id,
            matter.client_id,
            "c.docx",
            "contract body",
            text(),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&error, ContractReviewError::Artifact(message) if message.contains("not supplied as a file")),
            "{error}"
        );
        let error = drive_contract_review(
            &deps,
            matter.project_id,
            matter.client_id,
            "c.txt",
            "contract body",
            IntakeArtifact::File {
                bytes_base64: "not base64!".into(),
                content_type: "text/plain".into(),
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ContractReviewError::Base64(_)), "{error}");
    }

    #[tokio::test]
    async fn a_linked_or_already_stored_contract_reaches_lawyer_review() {
        let matter = matter().await;
        seed_template(&matter.surreal).await;
        seed_playbook(&matter.surreal, matter.entity_id).await;
        let state = crate::test_support::app_state(matter.surreal.clone()).await;
        let deps = ReviewDeps {
            surreal: &state.surreal,
            workflow_runtime: state.workflow_runtime.as_ref(),
            storage: &state.storage,
            contract_reviewer: state.contract_reviewer.as_ref(),
        };
        let linked = drive_contract_review(
            &deps,
            matter.project_id,
            matter.client_id,
            "linked-contract",
            "contract body",
            IntakeArtifact::Link {
                url: "https://example.com/contract".into(),
            },
        )
        .await
        .expect("a linked contract is filed as its address");
        let stored = Uuid::now_v7();
        let restaged = drive_contract_review(
            &deps,
            matter.project_id,
            matter.client_id,
            "stored-contract.txt",
            "contract body",
            IntakeArtifact::Stored { asset_id: stored },
        )
        .await
        .expect("an already-stored contract is not filed twice");
        assert_ne!(linked, restaged);
        let notations = store::notations::list_by_project(&matter.surreal, matter.project_id)
            .await
            .unwrap();
        assert_eq!(notations.len(), 2);
        assert!(notations.iter().all(|row| row.state == "lawyer_review"));
    }
}

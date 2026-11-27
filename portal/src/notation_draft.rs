//! `POST /app/projects/{project_code}/notations/draft` (LAW-29) — push one
//! template as a **draft**: stored against the Project, addressable, and —
//! since LAW-203 — backed by a **real notation run**.
//!
//! The run is what makes the preview answerable: the push persists the
//! pushed source as an immutable project-scoped Template version (the same
//! `validate → persist` seam [`store::template_source::persist_contents`]
//! runs for a checkout-read template — bad paper refuses loudly, with the
//! same verdict `navigator project gate` reports), enforces the same
//! engagement-first rule every other run-opening door enforces, then
//! [`workflows::start_notation`] opens the real questionnaire state machine
//! for the acting person. The door does not go through
//! [`workflows::create_notation_from_repo`] because that engine reads the
//! template from the hosted repository at HEAD — a preview's whole point is
//! to run the source under the author's cursor, which may not be committed.
//! The run is bound to the acting session's Person: a preview is a
//! lawyer's tool, and the lawyer is who answers it.
//!
//! Matter-scoped the same way `notation create` is: the acting lawyer must
//! participate in the target Project (`store::access::matter_viewer`;
//! firm-side participation required), so a miss and an out-of-scope project both read as "not
//! found" rather than disclosing which.
//!
//! The route lives under `/app/projects`, wrapped (with the rest of that
//! router) in `require_auth` + `require_policy` + the bearer-session
//! injector, exactly like `notation create`'s route — so `navigator
//! notation preview`, authenticated the same way `navigator site login`
//! leaves every other CLI command authenticated, can call it. The CSRF
//! layer's bearer exemption (no session cookie → pass through) is what lets
//! a JSON body reach here with no `_csrf` field.

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};

use crate::admin::AdminState;
use crate::session::SessionData;

/// POST body — the template's own slug, its file stem, and raw Markdown
/// source. Nothing else: the title, the questionnaire, and the workflow are
/// all read back out of `source` itself, the same projection
/// [`crate::notation_preview_doc::from_markdown`] runs for every other
/// caller, so a client cannot claim a title the document itself disagrees
/// with.
#[derive(Debug, Deserialize)]
pub struct NewNotationDraftBody {
    pub slug: String,
    /// The template `code`: the file stem the author's checkout names it
    /// by, so the server lints and stores it under the name the gate saw.
    /// The slug cannot stand in for it — `to_url` folds every underscore
    /// to a hyphen, and no inverse recovers which were which. Absent from
    /// an older CLI, which falls back to [`template_code_for_slug`].
    #[serde(default)]
    pub code: Option<String>,
    pub source: String,
}

/// Server response to a notation-draft POST: the draft's id and where to
/// find it, plus the run the push opened.
#[derive(Debug, Serialize, Deserialize)]
pub struct NotationDraftCreated {
    pub id: uuid::Uuid,
    /// Relative path — the caller (the CLI) already knows its own base URL
    /// and joins the two, so this handler never has to guess its own
    /// public hostname.
    pub path: String,
    /// The notation run the push opened, when it opened one.
    pub notation_id: Option<uuid::Uuid>,
    /// The lawyer-side path of the run's questionnaire — the answerable
    /// surface the draft backs. Present exactly when `notation_id` is.
    pub run_path: Option<String>,
}

pub async fn create_notation_draft_post(
    State(state): State<AdminState>,
    AxumPath(project_code): AxumPath<String>,
    session: Option<Extension<SessionData>>,
    Json(body): Json<NewNotationDraftBody>,
) -> Response {
    let project_id =
        match scoped_project(&state.application_store, &project_code, session.as_deref()).await {
            Ok(id) => id,
            Err(response) => return response,
        };

    let slug = body.slug.trim();
    // The source is persisted as submitted: a template's bytes are its
    // identity, and trimming here once cost M047's "file must end with a
    // newline" to a perfectly valid push.
    let source = body.source.as_str();
    if slug.is_empty() || source.is_empty() {
        return (StatusCode::BAD_REQUEST, "slug and source are required").into_response();
    }

    // The same projection every other reader of a template's Markdown
    // runs — the title recorded is the document's own, not whatever a
    // caller happened to send.
    let doc = crate::notation_preview_doc::from_markdown(slug, "", source);

    let code = match body.code.as_deref().map(str::trim) {
        None | Some("") => template_code_for_slug(slug),
        Some(code) if code.contains(['/', '\\']) || code.starts_with('.') => {
            return (StatusCode::BAD_REQUEST, "code must be a bare file stem").into_response();
        }
        Some(code) => code.to_string(),
    };
    let template = match persist_pushed_template(&state, project_id, &code, source).await {
        Ok(template) => template,
        Err(response) => return response,
    };

    let outcome = match open_preview_run(&state, project_id, &template, session.as_deref()).await {
        Ok(outcome) => outcome,
        Err(response) => return response,
    };

    match store::notation_drafts::create(
        &state.application_store,
        &store::notation_drafts::NewNotationDraft {
            project_id,
            slug,
            title: &doc.title,
            source,
            notation_id: Some(outcome.notation_id),
        },
    )
    .await
    {
        Ok(draft) => {
            let run_path = format!("/app/lawyer/notations/{}/step", outcome.notation_id);
            Json(NotationDraftCreated {
                id: draft.id,
                path: format!("/notations/drafts/{}", draft.id),
                notation_id: Some(outcome.notation_id),
                run_path: Some(run_path),
            })
        }
        .into_response(),
        Err(e) => {
            tracing::error!(
                error = %e,
                %project_id,
                notation_id = %outcome.notation_id,
                "notation draft create failed — the run opened but the draft row did not"
            );
            (StatusCode::INTERNAL_SERVER_ERROR, "internal").into_response()
        }
    }
}

/// The Project a pushed draft targets, only when the acting session may see
/// it. "No such matter" and "not yours" collapse to the same NOT_FOUND so
/// this door never discloses a project outside the caller's scope, same as
/// notation create. The route lives under `/app/projects` (require_auth
/// already ran); the command re-checks matter scope from the session,
/// matching `project_notation_new_post`. No session → no scope.
async fn scoped_project(
    application_store: &store::application_store::ApplicationStore,
    project_code: &str,
    session: Option<&SessionData>,
) -> Result<uuid::Uuid, Response> {
    let Some(project_id) = store::projects::id_for_code(application_store, project_code).await
    else {
        return Err((StatusCode::NOT_FOUND, "matter not found").into_response());
    };
    let (acting_person_id, acting_role) = match session {
        Some(s) => (s.person_id, s.role),
        None => (None, store::persons::Role::Client),
    };
    let in_scope =
        store::access::matter_viewer(application_store, acting_person_id, acting_role, project_id)
            .await
            .is_ok_and(|viewer| viewer.is_some_and(store::access::MatterViewer::is_firm_side));
    if !in_scope {
        return Err((StatusCode::NOT_FOUND, "matter not found").into_response());
    }
    Ok(project_id)
}

/// Persist the pushed source through the same `validate → persist` seam a
/// checkout-read template goes through — bad paper refuses here, with the
/// gate's own verdict.
async fn persist_pushed_template(
    state: &AdminState,
    project_id: uuid::Uuid,
    code: &str,
    source: &str,
) -> Result<store::templates::Template, Response> {
    match store::template_source::persist_contents(
        &state.application_store,
        &state.storage,
        project_id,
        code,
        source.to_string(),
        PREVIEW_PROVENANCE.to_string(),
    )
    .await
    {
        Ok(persisted) => Ok(persisted.template),
        Err(store::template_source::TemplateSourceError::Invalid { code, violations }) => {
            tracing::warn!(%code, count = violations.len(), "notation draft push: template refused");
            Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                refusal_body(&code, &violations),
            )
                .into_response())
        }
        Err(store::template_source::TemplateSourceError::Composition(reason)) => Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "template `{code}` did not compose its Clauses: {reason}. The server resolves \
                 Clause references against the Clause Library at push time, which a local \
                 lint cannot see."
            ),
        )
            .into_response()),
        Err(e) => {
            tracing::error!(error = %e, %project_id, "notation draft push: template persist failed");
            Err((StatusCode::INTERNAL_SERVER_ERROR, "internal").into_response())
        }
    }
}

/// The 422 body for a refused template: a summary line, then every blocking
/// violation as `path:line CODE: message`, the line `navigator project gate`
/// prints for the same finding. The path is the bare `<code>.md` the server
/// linted, so it names the author's file by its stem.
fn refusal_body(code: &str, violations: &[rules::Violation]) -> String {
    let mut body = format!(
        "template `{code}` has {} blocking rule violation(s); fix it and retry — or lint it \
         with `navigator notation preview --offline`",
        violations.len()
    );
    for violation in violations {
        body.push('\n');
        body.push_str(&violation.to_string());
    }
    body
}

/// Open the real notation run a pushed preview backs: the same
/// engagement-first rule every other run-opening door enforces — a preview
/// is not a way around it — then the run, bound to the acting person.
async fn open_preview_run(
    state: &AdminState,
    project_id: uuid::Uuid,
    template: &store::templates::Template,
    session: Option<&SessionData>,
) -> Result<workflows::StartOutcome, Response> {
    if let Err(e) =
        workflows::enforce_engagement_first_pinned(&state.application_store, project_id, template)
            .await
    {
        return Err(match e {
            workflows::NotationSessionError::EngagementMustBeFirst { code, kind } => (
                StatusCode::UNPROCESSABLE_ENTITY,
                format!(
                    "the first notation on this matter must be the engagement that opens it — \
                     a retainer or an onboarding — `{code}` is kind `{kind}`. Open the \
                     engagement first, then preview this."
                ),
            )
                .into_response(),
            e => {
                tracing::error!(error = %e, %project_id, "notation draft push: engagement gate failed");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal").into_response()
            }
        });
    }

    let Some(person_id) = acting_person(&state.application_store, session).await else {
        return Err((
            StatusCode::BAD_REQUEST,
            "the session names no person to run the preview as",
        )
            .into_response());
    };

    match workflows::start_notation_pinned(
        &state.application_store,
        state.questionnaire_runtime.as_ref(),
        Some(&state.storage),
        workflows::StartPinnedNotation {
            template,
            person_id,
            project_id,
            entity_id: None,
        },
    )
    .await
    {
        Ok(outcome) => Ok(outcome),
        Err(e) => {
            tracing::error!(error = %e, %project_id, "notation draft push: run failed to start");
            Err((StatusCode::INTERNAL_SERVER_ERROR, "internal").into_response())
        }
    }
}

/// Provenance recorded on a template version persisted by a preview push:
/// the push carries a working-tree source, not a commit, so the content
/// hash is the identity and this marker is the provenance.
const PREVIEW_PROVENANCE: &str = "preview";

/// The template `code` a draft's `{slug}` names: the slug folded back to
/// the `code` convention, where the shelf/document separator doubles
/// (`sample__letter` pushes as the slug `sample--letter`). A slug with no
/// doubled separator folds every hyphen to an underscore, which is what a
/// single-underscore filename's slug inverts to.
fn template_code_for_slug(slug: &str) -> String {
    match slug.split_once("--") {
        Some((shelf, rest)) => format!("{shelf}__{}", rest.replace('-', "_")),
        None => slug.replace('-', "_"),
    }
}

/// The Person the preview run is bound to: the session's own person when
/// the session carries one, else the person its email resolves to
/// (created on first sight, never added to the matter's participation —
/// the scope check above already proved the actor belongs to the matter).
async fn acting_person(
    application_store: &store::application_store::ApplicationStore,
    session: Option<&SessionData>,
) -> Option<uuid::Uuid> {
    let session = session?;
    if let Some(person_id) = session.person_id {
        return Some(person_id);
    }
    let email = session.email.as_deref()?;
    match store::persons::find_or_create(
        application_store,
        &store::persons::NewPerson::with_role(email, email, session.role),
    )
    .await
    {
        Ok(person) => Some(person.id),
        Err(e) => {
            tracing::error!(error = %e, "notation draft push: person resolve failed");
            None
        }
    }
}

#[cfg(test)]
mod tests {}

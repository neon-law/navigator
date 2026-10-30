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
//! participate in the target Project (`store::access::can_see_project_as_lawyer`;
//! admin bypasses), so a miss and an out-of-scope project both read as "not
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

/// POST body — the template's own slug and raw Markdown source. Nothing
/// else: the title, the questionnaire, and the workflow are all read back
/// out of `source` itself, the same projection
/// [`crate::notation_preview_doc::from_markdown`] runs for every other
/// caller, so a client cannot claim a title the document itself disagrees
/// with.
#[derive(Debug, Deserialize)]
pub struct NewNotationDraftBody {
    pub slug: String,
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
    let project_id = match scoped_project(&state.surreal, &project_code, session.as_deref()).await {
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

    let code = template_code_for_slug(slug);
    if let Err(response) = persist_pushed_template(&state, project_id, &code, source).await {
        return response;
    }

    let outcome = match open_preview_run(&state, project_id, &code, session.as_deref()).await {
        Ok(outcome) => outcome,
        Err(response) => return response,
    };

    match store::notation_drafts::create(
        &state.surreal,
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
    surreal: &store::surreal::SurrealDb,
    project_code: &str,
    session: Option<&SessionData>,
) -> Result<uuid::Uuid, Response> {
    let Some(project_id) = store::projects::id_for_code(surreal, project_code).await else {
        return Err((StatusCode::NOT_FOUND, "matter not found").into_response());
    };
    let (acting_person_id, acting_role) = match session {
        Some(s) => (s.person_id, s.role),
        None => (None, store::persons::Role::Client),
    };
    let in_scope = store::access::can_see_project_as_lawyer(
        surreal,
        acting_person_id,
        acting_role,
        project_id,
    )
    .await
    .unwrap_or(false);
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
) -> Result<(), Response> {
    match store::template_source::persist_contents(
        &state.surreal,
        &state.storage,
        project_id,
        code,
        source.to_string(),
        PREVIEW_PROVENANCE.to_string(),
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(store::template_source::TemplateSourceError::Invalid { code, violations }) => {
            tracing::warn!(%code, count = violations.len(), "notation draft push: template refused");
            Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                format!(
                    "template `{code}` has {} blocking rule violation(s); fix it and retry — \
                     or lint it with `navigator notation preview --offline`",
                    violations.len()
                ),
            )
                .into_response())
        }
        Err(e) => {
            tracing::error!(error = %e, %project_id, "notation draft push: template persist failed");
            Err((StatusCode::INTERNAL_SERVER_ERROR, "internal").into_response())
        }
    }
}

/// Open the real notation run a pushed preview backs: the same
/// engagement-first rule every other run-opening door enforces — a preview
/// is not a way around it — then the run, bound to the acting person.
async fn open_preview_run(
    state: &AdminState,
    project_id: uuid::Uuid,
    code: &str,
    session: Option<&SessionData>,
) -> Result<workflows::StartOutcome, Response> {
    if let Err(e) = workflows::enforce_engagement_first(&state.surreal, project_id, code).await {
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

    let Some(person_id) = acting_person(&state.surreal, session).await else {
        return Err((
            StatusCode::BAD_REQUEST,
            "the session names no person to run the preview as",
        )
            .into_response());
    };

    match workflows::start_notation(
        &state.surreal,
        state.questionnaire_runtime.as_ref(),
        Some(&state.storage),
        code,
        person_id,
        project_id,
        None,
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
    surreal: &store::surreal::SurrealDb,
    session: Option<&SessionData>,
) -> Option<uuid::Uuid> {
    let session = session?;
    if let Some(person_id) = session.person_id {
        return Some(person_id);
    }
    let email = session.email.as_deref()?;
    match store::persons::find_or_create(
        surreal,
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
mod tests {
    use super::*;
    use crate::admin::AdminState;
    use axum::body::to_bytes;
    use axum::routing::post;
    use axum::Router;
    use store::surreal::test_support::mem;
    use store::test_support::seed_project_surreal;

    /// An [`AdminState`] wired against a fresh embedded engine, plus the one
    /// matter to push a draft against. Mirrors `project_documents::tests::fixtures`.
    async fn fixtures(code: &str) -> (AdminState, uuid::Uuid) {
        let db = mem().await;
        let project_id = seed_project_surreal(&db, code).await;
        let app = crate::test_support::app_state(db).await;
        let state = AdminState {
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
        };
        (state, project_id)
    }

    fn router(state: AdminState) -> Router {
        Router::new()
            .route(
                "/app/projects/{project_code}/notations/draft",
                post(create_notation_draft_post),
            )
            .with_state(state)
    }

    /// A valid, catalog-grade onboarding template — the same fixture the
    /// retainer walk's tests run through `persist_contents`, so it passes
    /// the full rule set and its `onboarding` kind opens a matter.
    const ONBOARDING_SOURCE: &str =
        include_str!("../../workflows/tests/fixtures/onboarding_implicit_end.txt");

    /// A valid, clause-free non-engagement template for the
    /// engagement-first refusal: the full rule set accepts it, and its
    /// `letter` kind does not open a matter.
    const FIELD_LETTER_SOURCE: &str = "---\nkind: letter\ntitle: Field Letter\noutput: letter\njurisdiction: US\nrespondent_type: person\nconfidential: false\n---\n\nDear {{client_name}},\n\nThis is the letter body, which explains the observation the firm is sharing with you today.\n\nSincerely, The Firm\n";

    /// An unknown project code refuses before touching the store any
    /// further — the same "not found" shape an out-of-scope project gets,
    /// so the two are indistinguishable from outside.
    #[tokio::test]
    async fn an_unknown_project_code_is_refused() {
        let (state, _project_id) = fixtures("draft-unknown").await;

        let response = tower::ServiceExt::oneshot(
            router(state),
            axum::http::Request::builder()
                .method("POST")
                .uri("/app/projects/no-such-matter/notations/draft")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    "{\"slug\":\"sample-letter\",\"source\":\"Sample body\"}",
                ))
                .expect("request"),
        )
        .await
        .expect("response");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"matter not found");
    }

    /// Blank `slug`/`source` is refused before any store write. Carries an
    /// admin session (which bypasses matter scope, same as
    /// `create_project_notation`) so the request reaches the field
    /// validation rather than being turned away earlier by the scope check.
    #[tokio::test]
    async fn blank_fields_are_refused() {
        let (state, project_id) = fixtures("draft-refuse").await;
        let project = store::projects::find_by_id(&state.surreal, project_id)
            .await
            .expect("read project")
            .expect("project exists");
        let code = project.code.clone();
        let session = crate::session::SessionData::fresh(
            "admin@example.com".to_string(),
            store::persons::Role::Admin,
        );

        let response = tower::ServiceExt::oneshot(
            router(state),
            axum::http::Request::builder()
                .method("POST")
                .uri(format!("/app/projects/{code}/notations/draft"))
                .header("content-type", "application/json")
                .extension(session)
                .body(axum::body::Body::from(r#"{"slug":"","source":""}"#))
                .expect("request"),
        )
        .await
        .expect("response");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    /// No session (no `Authorization: Bearer …`, no cookie) is refused —
    /// with no auth or policy layer mounted in this focused router, the
    /// handler's own `can_see_project_as_lawyer` check is what stands in
    /// for the full `/app/projects` stack's `require_auth`.
    #[tokio::test]
    async fn no_session_is_refused_even_for_a_real_project() {
        let (state, project_id) = fixtures("draft-no-session").await;
        let project = store::projects::find_by_id(&state.surreal, project_id)
            .await
            .expect("read project")
            .expect("project exists");
        let code = project.code.clone();

        let response = tower::ServiceExt::oneshot(
            router(state),
            axum::http::Request::builder()
                .method("POST")
                .uri(format!("/app/projects/{code}/notations/draft"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    "{\"slug\":\"sample-letter\",\"source\":\"Sample body\"}",
                ))
                .expect("request"),
        )
        .await
        .expect("response");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    /// The happy path, and the acceptance test for LAW-203: a pushed
    /// preview opens a **real** notation run — a `notation` row, a started
    /// questionnaire state machine, and a run path the lawyer answers at —
    /// and the draft row carries the run it opened, which is the link the
    /// draft page's hand-off reads. The template is auto-saved first, so
    /// the run resolves the pushed source, not whatever the repo last
    /// committed.
    #[tokio::test]
    async fn pushing_a_draft_opens_a_real_notation_run() {
        let (state, project_id) = fixtures("draft-run-backed").await;
        let surreal = state.surreal.clone();
        let project = store::projects::find_by_id(&surreal, project_id)
            .await
            .expect("read project")
            .expect("project exists");
        let code = project.code.clone();
        let person = store::persons::create(
            &surreal,
            &store::persons::NewPerson::new("Previewing Lawyer", "lawyer@example.com"),
        )
        .await
        .expect("person");
        // The acting lawyer participates in the matter — a firm-side row,
        // which is what `can_see_project_as_lawyer` scopes the door by.
        store::projects::add_participation(&surreal, project_id, person.id, "lawyer")
            .await
            .expect("participation");
        let mut session = crate::session::SessionData::fresh(
            "lawyer@example.com".to_string(),
            store::persons::Role::Lawyer,
        );
        session.person_id = Some(person.id);

        assert!(
            !store::notations::exists_for_project(&surreal, project_id)
                .await
                .expect("exists check"),
            "no notation exists before the preview is pushed"
        );

        let response = tower::ServiceExt::oneshot(
            router(state),
            axum::http::Request::builder()
                .method("POST")
                .uri(format!("/app/projects/{code}/notations/draft"))
                .header("content-type", "application/json")
                .extension(session)
                .body(axum::body::Body::from(format!(
                    "{{\"slug\":\"onboarding\",\"source\":{}}}",
                    serde_json::to_string(ONBOARDING_SOURCE).expect("json source")
                )))
                .expect("request"),
        )
        .await
        .expect("response");

        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            status,
            StatusCode::OK,
            "the push succeeds: {}",
            String::from_utf8(body.to_vec()).expect("utf-8 body")
        );
        let created: NotationDraftCreated = serde_json::from_slice(&body).expect("json body");

        // The whole point: the push opened a real run — a notation row on
        // the matter, in BEGIN, with a state-machine instance behind it (a
        // failed start is a 500, so a 200 with a run path proves it).
        let notation_id = created
            .notation_id
            .expect("the push reports the run it opened");
        assert!(
            store::notations::exists_for_project(&surreal, project_id)
                .await
                .expect("exists check"),
            "the push created the notation run"
        );
        let notation = store::notations::find_by_id(&surreal, notation_id)
            .await
            .expect("read notation")
            .expect("the run's notation row exists");
        assert_eq!(notation.project_id, project_id);
        assert_eq!(notation.person_id, person.id);
        assert!(
            created
                .run_path
                .as_deref()
                .is_some_and(|path| path == format!("/app/lawyer/notations/{notation_id}/step")),
            "the run path is the lawyer's answerable questionnaire: {:?}",
            created.run_path
        );

        // The draft row carries the run, and stays addressable.
        let draft = store::notation_drafts::find_live(&surreal, created.id)
            .await
            .expect("lookup")
            .expect("the pushed draft is stored and addressable");
        assert_eq!(draft.notation_id, Some(notation_id));
        assert_eq!(draft.title, "Synthetic onboarding");
    }

    /// The engagement-first rule holds at the preview door exactly as it
    /// holds at `notation create`: a non-engagement template pushed first
    /// on a matter with no notation is refused with the same guidance, not
    /// a silent bypass through a preview.
    #[tokio::test]
    async fn a_preview_of_a_non_engagement_template_is_refused_first_on_a_matter() {
        let (state, project_id) = fixtures("draft-engagement-first").await;
        let surreal = state.surreal.clone();
        let project = store::projects::find_by_id(&surreal, project_id)
            .await
            .expect("read project")
            .expect("project exists");
        let code = project.code.clone();
        let person = store::persons::create(
            &surreal,
            &store::persons::NewPerson::new("Previewing Lawyer", "lawyer@example.com"),
        )
        .await
        .expect("person");
        // Firm-side participation, so the door's scope check admits the
        // lawyer before the engagement rule is what refuses the push.
        store::projects::add_participation(&surreal, project_id, person.id, "lawyer")
            .await
            .expect("participation");
        let mut session = crate::session::SessionData::fresh(
            "lawyer@example.com".to_string(),
            store::persons::Role::Lawyer,
        );
        session.person_id = Some(person.id);

        let response = tower::ServiceExt::oneshot(
            router(state),
            axum::http::Request::builder()
                .method("POST")
                .uri(format!("/app/projects/{code}/notations/draft"))
                .header("content-type", "application/json")
                .extension(session)
                .body(axum::body::Body::from(format!(
                    "{{\"slug\":\"field-letter\",\"source\":{}}}",
                    serde_json::to_string(FIELD_LETTER_SOURCE).expect("json source")
                )))
                .expect("request"),
        )
        .await
        .expect("response");

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let message = String::from_utf8(body.to_vec()).expect("utf-8 body");
        assert!(
            message.contains("the first notation on this matter must be the engagement"),
            "the refusal carries the engagement-first guidance: {message}"
        );
        assert!(
            !store::notations::exists_for_project(&surreal, project_id)
                .await
                .expect("exists check"),
            "a refused push opens no run"
        );
    }
}

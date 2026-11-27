//! Project-scoped court-date proposals. Only a participating lawyer in a browser session
//! can release a reviewed revision for durable publication.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use model::deadline_proposals::{Decision, Proposal};
use serde::Deserialize;
use serde_json::{json, Value};
use store::deadline_proposals as proposals;
use store::postgres::PostgresDb;
use uuid::Uuid;

use crate::api::{ApiError, ApiState, AuthedSession};
use crate::session::{SessionData, SessionSource};

pub(crate) async fn project_reviewer(
    state: &ApiState,
    session: &SessionData,
    project_id: Uuid,
) -> Result<Uuid, ApiError> {
    if session.source == SessionSource::Ci {
        return Err(ApiError::Forbidden);
    }
    let actor = session.person_id.ok_or(ApiError::Forbidden)?;
    let member = if session.role.is_lawyer_tier() {
        store::projects::has_firm_participation(&state.application_store, actor, project_id)
            .await
            .map_err(|e| ApiError::Db(e.to_string()))?
    } else if session.role.is_clerk() {
        store::access::can_see_project_as_clerk(
            &state.application_store,
            Some(actor),
            session.role,
            project_id,
        )
        .await
        .map_err(ApiError::Db)?
    } else {
        return Err(ApiError::Forbidden);
    };
    if member {
        Ok(actor)
    } else {
        Err(ApiError::NotFound)
    }
}

fn can_confirm(session: &SessionData) -> bool {
    session.source == SessionSource::Browser && session.role.is_lawyer_tier()
}

pub(crate) async fn database() -> Result<PostgresDb, ApiError> {
    PostgresDb::application()
        .await
        .map_err(|e| ApiError::Db(e.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Submission {
    request_id: Uuid,
    target_deadline_id: Option<Uuid>,
    draft: model::deadline_proposals::DraftInput,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Correction {
    revision: i64,
    draft: model::deadline_proposals::DraftInput,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Revision {
    revision: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Return {
    revision: i64,
    reason: String,
}

fn reviewed_proposal(proposal: &Proposal, can_read_authorities: bool) -> Value {
    let mut row = json!(proposal);
    if !can_read_authorities {
        for (index, evidence) in proposal.draft.evidence.iter().enumerate() {
            if evidence.role == model::deadline_proposals::Role::Authority {
                row["evidence"][index] = json!({"role":"authority"});
            }
        }
    }
    row
}

pub(crate) async fn list(
    State(state): State<ApiState>,
    AuthedSession(session): AuthedSession,
    Path(project): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    project_reviewer(&state, &session, project).await?;
    let pg = database().await?;
    let rows = proposals::list(&pg, project).await.map_err(map_error)?;
    let labels = proposals::source_labels(&pg, project, &rows, session.role.is_lawyer_tier())
        .await
        .map_err(map_error)?;
    let available = proposals::available_evidence(&pg, project, session.role.is_lawyer_tier())
        .await
        .map_err(map_error)?;
    let rows: Vec<_> = rows
        .into_iter()
        .map(|proposal| {
            let mut row = reviewed_proposal(&proposal, session.role.is_lawyer_tier());
            for (index, evidence) in proposal.draft.evidence.iter().enumerate() {
                let item = &mut row["evidence"][index];
                if evidence.role == model::deadline_proposals::Role::Authority
                    && !session.role.is_lawyer_tier()
                {
                    continue;
                }
                let availability = available.get(&(proposal.id, proposal.revision, index));
                if availability != Some(&true) {
                    if let Some(fields) = item.as_object_mut() {
                        fields.remove("forgejo");
                    }
                }
                if let Some(verified) = availability {
                    let base = format!(
                        "/app/api/projects/{project}/court-date-proposals/{}/evidence/{index}",
                        proposal.id
                    );
                    item["archive_url"] = json!(format!("{base}?revision={}", proposal.revision));
                    if *verified {
                        item["forgejo_url"] =
                            json!(format!("{base}/forgejo?revision={}", proposal.revision));
                    }
                }
                if let Some(label) = labels.get(&(
                    evidence.role == model::deadline_proposals::Role::Authority,
                    evidence.id,
                    evidence.sha256_hex.clone(),
                )) {
                    item["label"] = json!(label);
                }
            }
            row["review_error"] = json!(proposal.draft.reviewed_due().err());
            row
        })
        .collect();
    Ok(Json(
        json!({"proposals":rows,"can_confirm":can_confirm(&session)}),
    ))
}

pub(crate) async fn stage(
    State(state): State<ApiState>,
    AuthedSession(session): AuthedSession,
    Path(project): Path<Uuid>,
    Json(input): Json<Submission>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    project_reviewer(&state, &session, project).await?;
    let pg = database().await?;
    let draft = proposals::resolve_draft(
        &pg,
        &state.application_store,
        project,
        input.request_id,
        None,
        input.draft,
    )
    .await
    .map_err(map_error)?;
    let verified =
        crate::deadline_evidence::verify(&state, project, &draft, session.role.is_lawyer_tier())
            .await?;
    let proposal = proposals::stage_verified(
        &pg,
        project,
        input.request_id,
        input.target_deadline_id,
        &draft,
        &verified,
    )
    .await
    .map_err(map_error)?;
    Ok((
        StatusCode::CREATED,
        Json(reviewed_proposal(&proposal, session.role.is_lawyer_tier())),
    ))
}

pub(crate) async fn correct(
    State(state): State<ApiState>,
    AuthedSession(session): AuthedSession,
    Path((project, id)): Path<(Uuid, Uuid)>,
    Json(input): Json<Correction>,
) -> Result<Json<Value>, ApiError> {
    project_reviewer(&state, &session, project).await?;
    let pg = database().await?;
    let draft = proposals::resolve_draft(
        &pg,
        &state.application_store,
        project,
        id,
        Some(input.revision),
        input.draft,
    )
    .await
    .map_err(map_error)?;
    let verified =
        crate::deadline_evidence::verify(&state, project, &draft, session.role.is_lawyer_tier())
            .await?;
    proposals::correct_verified(&pg, project, id, input.revision, &draft, &verified)
        .await
        .map(|proposal| Json(reviewed_proposal(&proposal, session.role.is_lawyer_tier())))
        .map_err(map_error)
}

pub(crate) async fn confirm(
    State(state): State<ApiState>,
    AuthedSession(session): AuthedSession,
    Path((project, id)): Path<(Uuid, Uuid)>,
    Json(input): Json<Revision>,
) -> Result<Json<Proposal>, ApiError> {
    if !can_confirm(&session) {
        return Err(ApiError::Forbidden);
    }
    let actor = project_reviewer(&state, &session, project).await?;
    let pg = database().await?;
    let proposal = proposals::get(&pg, project, id)
        .await
        .map_err(map_error)?
        .ok_or(ApiError::NotFound)?;
    if proposal.revision != input.revision {
        return Err(ApiError::CourtDateConflict);
    }
    if proposal.status == model::deadline_proposals::Status::Pending {
        crate::deadline_evidence::verify_confirmation(&state, project, &proposal).await?;
        check_appearance_proposal(&state, &session, &proposal).await?;
    }
    proposals::decide(&pg, project, id, actor, input.revision, Decision::Confirm)
        .await
        .map(Json)
        .map_err(map_error)
}

async fn check_appearance_proposal(
    state: &ApiState,
    session: &crate::session::SessionData,
    proposal: &Proposal,
) -> Result<(), ApiError> {
    use model::project_deadlines::{Appearance, CalendarRole, Category, Hearing};
    let draft = &proposal.draft;
    let Some(start) = draft.due_at.filter(|_| {
        draft.category.is_some_and(Category::is_appearance)
            || draft.calendar_role == Some(CalendarRole::Hearing)
    }) else {
        return Ok(());
    };
    let mut starts = vec![start];
    if let Some(Appearance::TrialDay {
        additional_starts,
        cancel,
        ..
    }) = draft
        .hearing
        .as_ref()
        .and_then(|details| details.appearance.as_ref())
    {
        if *cancel {
            return Ok(());
        }
        starts.extend(additional_starts);
    }
    let policy = crate::deadlines_api::policy(state, proposal.project_id).await?;
    crate::deadline_scheduling::check_appearance(
        state,
        session,
        proposal.project_id,
        &starts,
        draft.hearing.as_ref().map_or(60, Hearing::minutes),
        policy,
        proposal.deadline_id.is_some(),
    )
    .await
}

pub(crate) async fn return_for_correction(
    State(state): State<ApiState>,
    AuthedSession(session): AuthedSession,
    Path((project, id)): Path<(Uuid, Uuid)>,
    Json(input): Json<Return>,
) -> Result<Json<Proposal>, ApiError> {
    if !can_confirm(&session) {
        return Err(ApiError::Forbidden);
    }
    let actor = project_reviewer(&state, &session, project).await?;
    proposals::decide(
        &database().await?,
        project,
        id,
        actor,
        input.revision,
        Decision::Return {
            reason: input.reason,
        },
    )
    .await
    .map(Json)
    .map_err(map_error)
}

pub(crate) fn map_error(e: proposals::Error) -> ApiError {
    match e {
        proposals::Error::Forbidden => ApiError::Forbidden,
        proposals::Error::NotFound => ApiError::NotFound,
        proposals::Error::Conflict => ApiError::CourtDateConflict,
        proposals::Error::Invalid(reason) => ApiError::DeadlineInvalid(reason.to_string()),
        other => ApiError::Db(other.to_string()),
    }
}

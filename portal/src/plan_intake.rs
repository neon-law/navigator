//! Lawyer review doors for staged plan assignments.
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Extension;
use serde::Deserialize;
use store::plan_intake::{self, PlanIntakeError};
use store::postgres::PostgresDb;
use store::surreal::SurrealDb;
use uuid::Uuid;

use crate::session::SessionData;
use webapp::plan_intake::PLAN_INTAKE_PATH;

#[derive(Debug, Deserialize)]
pub struct PlanAcceptForm {
    attestation: Option<String>,
    plan_case_number: Option<String>,
    plan_matter_type: String,
    plan_state: Option<String>,
    name: Option<String>,
    code: String,
}

#[derive(Debug, Deserialize)]
pub struct PlanMemberForm {
    member_name: Option<String>,
    member_email: String,
}

#[derive(Debug, Deserialize)]
pub struct PlanDeclineForm {
    reason: String,
}

/// Confirm the member before displaying the conflict review and take form.
pub async fn plan_resolve_member_post(
    State(db): State<SurrealDb>,
    session: Option<Extension<SessionData>>,
    Path(staged_id): Path<Uuid>,
    axum::Form(form): axum::Form<PlanMemberForm>,
) -> Response {
    let Some(actor_id) = crate::inbox::actor_id(session) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let pg = match PostgresDb::from_env().await {
        Ok(pg) => pg,
        Err(error) => return review_error(&error.into()),
    };
    let brand = views::brand::brand_key();
    match plan_intake::resolve_member(
        &db,
        &pg,
        staged_id,
        actor_id,
        brand.as_str(),
        form.member_name.as_deref().map(str::trim),
        &form.member_email,
    )
    .await
    {
        Ok(()) => Redirect::to(PLAN_INTAKE_PATH).into_response(),
        Err(error) => review_error(&error),
    }
}

/// Take the referral and open the Pitch after the lawyer's conflict attestation.
pub async fn plan_accept_post(
    State(db): State<SurrealDb>,
    session: Option<Extension<SessionData>>,
    Path(staged_id): Path<Uuid>,
    axum::Form(form): axum::Form<PlanAcceptForm>,
) -> Response {
    let Some(actor_id) = crate::inbox::actor_id(session) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let pg = match PostgresDb::from_env().await {
        Ok(pg) => pg,
        Err(error) => return review_error(&error.into()),
    };
    let input = plan_intake::AcceptInput {
        attestation: form.attestation.as_deref() == Some("yes"),
        acting_person_id: actor_id,
        brand: views::brand::brand_key().as_str().to_owned(),
        code: form.code.trim().to_owned(),
        name: form.name.as_deref().and_then(nonblank),
        plan_case_number: form.plan_case_number.as_deref().and_then(nonblank),
        plan_matter_type: form.plan_matter_type.trim().to_owned(),
        plan_state: form
            .plan_state
            .as_deref()
            .and_then(nonblank)
            .map(|state| state.to_ascii_lowercase()),
    };
    let accepted = match plan_intake::accept(&db, &pg, staged_id, &input).await {
        Ok(accepted) => accepted,
        Err(error) => return review_error(&error),
    };
    match store::projects::find_by_id(&db, accepted.project_id).await {
        Ok(Some(project)) => {
            Redirect::to(&format!("/app/projects/{}", project.code)).into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => review_error(&error.into()),
    }
}

/// Record the lawyer's decision to pass on a staged referral.
pub async fn plan_decline_post(
    State(db): State<SurrealDb>,
    session: Option<Extension<SessionData>>,
    Path(staged_id): Path<Uuid>,
    axum::Form(form): axum::Form<PlanDeclineForm>,
) -> Response {
    let Some(actor_id) = crate::inbox::actor_id(session) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let pg = match PostgresDb::from_env().await {
        Ok(pg) => pg,
        Err(error) => return review_error(&error.into()),
    };
    match plan_intake::decline_from_review(
        &db,
        &pg,
        staged_id,
        actor_id,
        views::brand::brand_key().as_str(),
        &form.reason,
    )
    .await
    {
        Ok(_) => Redirect::to(PLAN_INTAKE_PATH).into_response(),
        Err(error) => review_error(&error),
    }
}

fn review_error(error: &PlanIntakeError) -> Response {
    use store::projects::OpenMatterError;

    let message = match error {
        PlanIntakeError::NotFound => return StatusCode::NOT_FOUND.into_response(),
        PlanIntakeError::Open(OpenMatterError::BlockingConflict(_)) => {
            "The conflict check found a blocking conflict. Review the assignment before proceeding."
                .to_owned()
        }
        PlanIntakeError::Conflict(_) => {
            "The conflict check could not finish. The referral is still staged for review."
                .to_owned()
        }
        PlanIntakeError::Db(_)
        | PlanIntakeError::Postgres(_)
        | PlanIntakeError::Sql(_)
        | PlanIntakeError::Open(OpenMatterError::Db(_) | OpenMatterError::Attestation(_))
        | PlanIntakeError::ProjectStore(_)
        | PlanIntakeError::ProjectCommand(_)
        | PlanIntakeError::Firm(_)
        | PlanIntakeError::Person(_)
        | PlanIntakeError::Entity(_)
        | PlanIntakeError::EntityType(_)
        | PlanIntakeError::Jurisdiction(_)
        | PlanIntakeError::Deadline(_) => {
            "The intake operation could not finish. Try again from the review queue.".to_owned()
        }
        _ => error.to_string(),
    };
    crate::inbox::redirect_error(PLAN_INTAKE_PATH, &message)
}

fn nonblank(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

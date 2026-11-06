//! The lawyer-only doors for a contingent matter's firm work product: its
//! dated Contingency Terms, its Outcome Estimates, and its Matter Outcome.
//!
//! Route admission is lawyer tier (the `LawyerSession` extractor plus an
//! embedded-Rego `route_allow` rule). The matter-scope question is asked once
//! through [`store::access::matter_viewer`], so a caller with no firm-side
//! participation row — a client, a clerk, an unassigned lawyer, Owner and
//! Admin included — gets the non-disclosing 404. The store asks its own
//! PostgreSQL firm-side question again ([`store::firm_side`]), so no other
//! caller can reach the rows either. Nothing here reaches a client or clerk
//! lens.
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use store::{contingency_terms, matter_outcomes, outcome_estimates, postgres_mirror};
use uuid::Uuid;

use crate::api::{ApiError, ApiState, LawyerSession};

fn refused(status: StatusCode) -> Response {
    (
        status,
        Json(serde_json::json!({"error": "contingency_command_refused"})),
    )
        .into_response()
}

fn store_failed(operation: &'static str, project_id: Uuid, error: &dyn std::fmt::Display) {
    tracing::warn!(
        event = "contingency_operation_failed",
        operation,
        outcome = "store_error",
        project_id = %project_id,
        store_error = %error
    );
}

fn term_error(
    error: &contingency_terms::ContingencyTermError,
    operation: &'static str,
    project_id: Uuid,
) -> Result<Response, ApiError> {
    use contingency_terms::ContingencyTermError as E;
    match error {
        E::NotFirmSide => Err(ApiError::NotFound),
        E::Invalid(_) | E::FeeAgreementNotOnMatter | E::StageNotInFirmVocabulary => {
            Ok(refused(StatusCode::UNPROCESSABLE_ENTITY))
        }
        E::Database(_) => {
            store_failed(operation, project_id, error);
            Ok(refused(StatusCode::INTERNAL_SERVER_ERROR))
        }
    }
}

fn estimate_error(
    error: &outcome_estimates::OutcomeEstimateError,
    operation: &'static str,
    project_id: Uuid,
) -> Result<Response, ApiError> {
    use outcome_estimates::OutcomeEstimateError as E;
    match error {
        E::NotFirmSide => Err(ApiError::NotFound),
        E::Invalid(_) => Ok(refused(StatusCode::UNPROCESSABLE_ENTITY)),
        E::Database(_) => {
            store_failed(operation, project_id, error);
            Ok(refused(StatusCode::INTERNAL_SERVER_ERROR))
        }
    }
}

fn outcome_error(
    error: &matter_outcomes::MatterOutcomeError,
    operation: &'static str,
    project_id: Uuid,
) -> Result<Response, ApiError> {
    use matter_outcomes::MatterOutcomeError as E;
    match error {
        E::NotFirmSide => Err(ApiError::NotFound),
        E::Invalid(_) => Ok(refused(StatusCode::UNPROCESSABLE_ENTITY)),
        E::AlreadyRecorded | E::NotRecorded => Ok(refused(StatusCode::CONFLICT)),
        E::Database(_) => {
            store_failed(operation, project_id, error);
            Ok(refused(StatusCode::INTERNAL_SERVER_ERROR))
        }
    }
}

/// The acting Person, admitted only as a firm-side participant on the matter.
async fn firm_side_actor(
    state: &ApiState,
    session: &crate::session::SessionData,
    project_id: Uuid,
) -> Result<Uuid, ApiError> {
    let actor = session.person_id.ok_or(ApiError::Forbidden)?;
    let viewer =
        store::access::matter_viewer(&state.surreal, session.person_id, session.role, project_id)
            .await
            .map_err(ApiError::Db)?;
    match viewer {
        Some(viewer) if viewer.is_firm_side() => Ok(actor),
        _ => Err(ApiError::NotFound),
    }
}

/// Open PostgreSQL with the rows a firm-side write references mirrored: the
/// Project, its participations (and so their Persons, the actor among
/// them), and any `notations` the write names.
async fn prepared_store(
    state: &ApiState,
    operation: &'static str,
    project_id: Uuid,
    notations: &[Uuid],
) -> Result<store::postgres::PostgresDb, Response> {
    let Ok(pg) = store::postgres::PostgresDb::from_env().await else {
        tracing::warn!(
            event = "contingency_store_unavailable",
            operation,
            outcome = "unavailable",
            project_id = %project_id
        );
        return Err(refused(StatusCode::SERVICE_UNAVAILABLE));
    };
    let surreal = &state.surreal;
    let mirrored = async {
        postgres_mirror::mirror(
            surreal,
            &pg,
            &postgres_mirror::MirrorKey::Project(project_id),
        )
        .await?;
        let participations = store::projects::participations_for_project(surreal, project_id)
            .await
            .map_err(|_| postgres_mirror::MirrorError::Source)?;
        for role in participations {
            postgres_mirror::mirror(
                surreal,
                &pg,
                &postgres_mirror::MirrorKey::ProjectParticipation(role.id),
            )
            .await?;
        }
        for notation in notations {
            postgres_mirror::mirror(
                surreal,
                &pg,
                &postgres_mirror::MirrorKey::Notation(*notation),
            )
            .await?;
        }
        Ok::<_, postgres_mirror::MirrorError>(())
    }
    .await;
    if let Err(error) = mirrored {
        store_failed(operation, project_id, &error);
        return Err(refused(StatusCode::SERVICE_UNAVAILABLE));
    }
    Ok(pg)
}

/// `GET /app/api/projects/{id}/contingency-terms` — the matter's dated
/// Contingency Terms, oldest effective date first.
pub(crate) async fn list_terms(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let actor = firm_side_actor(&state, &session, project_id).await?;
    let pg = match prepared_store(&state, "list_terms", project_id, &[]).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    match contingency_terms::history(&pg, actor, project_id).await {
        Ok(terms) => Ok(Json(serde_json::json!({ "terms": terms })).into_response()),
        Err(e) => term_error(&e, "list_terms", project_id),
    }
}

/// `POST /app/api/projects/{id}/contingency-terms` — record a new dated
/// Contingency Term. Rows are never edited: a change is a new effective date.
pub(crate) async fn record_term(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project_id): Path<Uuid>,
    Json(input): Json<contingency_terms::NewContingencyTerm>,
) -> Result<Response, ApiError> {
    let actor = firm_side_actor(&state, &session, project_id).await?;
    let pg = match prepared_store(
        &state,
        "record_term",
        project_id,
        &[input.fee_agreement_notation_id],
    )
    .await
    {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    match contingency_terms::record(&pg, project_id, actor, &input).await {
        Ok(term) => Ok((StatusCode::CREATED, Json(term)).into_response()),
        Err(e) => term_error(&e, "record_term", project_id),
    }
}

/// `GET /app/api/projects/{id}/outcome-estimates` — every Outcome Estimate on
/// the matter, oldest first; the newest is the current estimate.
pub(crate) async fn list_estimates(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let actor = firm_side_actor(&state, &session, project_id).await?;
    let pg = match prepared_store(&state, "list_estimates", project_id, &[]).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    match outcome_estimates::history(&pg, actor, project_id).await {
        Ok(estimates) => Ok(Json(serde_json::json!({ "estimates": estimates })).into_response()),
        Err(e) => estimate_error(&e, "list_estimates", project_id),
    }
}

/// `POST /app/api/projects/{id}/outcome-estimates` — record a new estimate,
/// which becomes the current one.
pub(crate) async fn record_estimate(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project_id): Path<Uuid>,
    Json(input): Json<outcome_estimates::NewOutcomeEstimate>,
) -> Result<Response, ApiError> {
    let actor = firm_side_actor(&state, &session, project_id).await?;
    let pg = match prepared_store(&state, "record_estimate", project_id, &[]).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    match outcome_estimates::record(&pg, project_id, actor, &input).await {
        Ok(estimate) => Ok((StatusCode::CREATED, Json(estimate)).into_response()),
        Err(e) => estimate_error(&e, "record_estimate", project_id),
    }
}

/// `GET /app/api/projects/{id}/matter-outcome` — the matter's outcome chain,
/// original first; the last row is the outcome.
pub(crate) async fn list_outcomes(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let actor = firm_side_actor(&state, &session, project_id).await?;
    let pg = match prepared_store(&state, "list_outcomes", project_id, &[]).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    match matter_outcomes::history(&pg, actor, project_id).await {
        Ok(outcomes) => Ok(Json(serde_json::json!({ "outcomes": outcomes })).into_response()),
        Err(e) => outcome_error(&e, "list_outcomes", project_id),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordOutcomeInput {
    outcome: matter_outcomes::NewMatterOutcome,
    /// Present on a correction: why the current outcome is superseded.
    #[serde(default)]
    correction_reason: Option<String>,
}

/// `POST /app/api/projects/{id}/matter-outcome` — record the matter's
/// outcome, or, with a `correction_reason`, supersede the current one.
pub(crate) async fn record_outcome(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project_id): Path<Uuid>,
    Json(input): Json<RecordOutcomeInput>,
) -> Result<Response, ApiError> {
    let actor = firm_side_actor(&state, &session, project_id).await?;
    let pg = match prepared_store(&state, "record_outcome", project_id, &[]).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    let written = match input.correction_reason.as_deref() {
        Some(reason) => {
            matter_outcomes::correct(&pg, project_id, actor, &input.outcome, reason).await
        }
        None => matter_outcomes::record(&pg, project_id, actor, &input.outcome).await,
    };
    match written {
        Ok(outcome) => Ok((StatusCode::CREATED, Json(outcome)).into_response()),
        Err(e) => outcome_error(&e, "record_outcome", project_id),
    }
}

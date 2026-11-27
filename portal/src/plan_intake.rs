//! Lawyer review doors for staged plan assignments.
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Extension;
use serde::Deserialize;
use store::application_store::ApplicationStore;
use store::plan_intake::{self, PlanIntakeError, PlanOnboarding};
use store::postgres::PostgresDb;
use uuid::Uuid;

use crate::admin::AdminState;
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
    State(db): State<ApplicationStore>,
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
    State(state): State<AdminState>,
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
    let db = &state.application_store;
    let accepted = match plan_intake::accept_with_onboarding(db, &pg, staged_id, &input, |plan| {
        start_plan_onboarding(
            db,
            state.questionnaire_runtime.as_ref(),
            &state.storage,
            plan,
            actor_id,
        )
    })
    .await
    {
        Ok(accepted) => accepted,
        Err(error) => return review_error(&error),
    };
    match store::projects::find_by_id(db, accepted.project_id).await {
        Ok(Some(project)) => {
            Redirect::to(&format!("/app/projects/{}", project.code)).into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => review_error(&error.into()),
    }
}

fn plan_onboarding_code(provider: &str) -> Option<&'static str> {
    use store::plan_assignment::PlanProvider;
    if provider == PlanProvider::Arag.as_str() {
        Some("onboarding__arag")
    } else if provider == PlanProvider::LegalEase.as_str() {
        Some("onboarding__legalease")
    } else {
        None
    }
}

async fn start_plan_onboarding(
    db: &ApplicationStore,
    runtime: &dyn workflows::StateMachineRuntime,
    storage: &Arc<dyn cloud::StorageService>,
    plan: PlanOnboarding,
    actor_id: Uuid,
) -> Result<(), PlanIntakeError> {
    let Some(code) = plan_onboarding_code(&plan.provider) else {
        return Ok(());
    };
    let notations = store::notations::list_by_project(db, plan.project_id)
        .await
        .map_err(|error| PlanIntakeError::Onboarding(error.to_string()))?;
    let template_ids: Vec<_> = notations
        .iter()
        .map(|notation| notation.template_id)
        .collect();
    let templates = store::templates::find_by_ids(db, &template_ids)
        .await
        .map_err(|error| PlanIntakeError::Onboarding(error.to_string()))?;
    let existing = notations.iter().find_map(|notation| {
        templates
            .iter()
            .find(|template| {
                template.id == notation.template_id
                    && template.kind.as_deref() == Some("onboarding")
            })
            .map(|template| (notation.id, template.code.as_str()))
    });
    let notation_id = match existing {
        Some((id, existing_code)) if existing_code == code => id,
        Some(_) => return Ok(()),
        None => {
            workflows::start_notation(
                db,
                runtime,
                Some(storage),
                code,
                plan.person_id,
                plan.project_id,
                None,
            )
            .await
            .map_err(|error| PlanIntakeError::Onboarding(error.to_string()))?
            .notation_id
        }
    };
    if let Some(fee) = plan.fee.filter(|_| code == "onboarding__arag") {
        prefill_arag_fee(
            db,
            runtime,
            storage,
            notation_id,
            actor_id,
            fee.plan_fee_cents,
        )
        .await?;
    }
    Ok(())
}

async fn prefill_arag_fee(
    db: &ApplicationStore,
    runtime: &dyn workflows::StateMachineRuntime,
    storage: &Arc<dyn cloud::StorageService>,
    notation_id: Uuid,
    actor_id: Uuid,
    cents: i32,
) -> Result<(), PlanIntakeError> {
    const FEE: &str = "custom_usd__plan_fee";
    let storage = Some(storage);
    let step = workflows::notation_session::current_step(db, runtime, storage, notation_id)
        .await
        .map_err(|error| PlanIntakeError::Onboarding(error.to_string()))?;
    if matches!(step, workflows::NextStep::NeedsAnswer { question } if question.code == FEE) {
        let amount = format!("{}.{:02}", cents / 100, cents % 100);
        workflows::answer_step(
            db,
            runtime,
            storage,
            notation_id,
            FEE,
            &amount,
            workflows::AnswerAuthor::lawyer(Some(actor_id)),
        )
        .await
        .map_err(|error| PlanIntakeError::Onboarding(error.to_string()))?;
    }
    Ok(())
}

/// Record the lawyer's decision to pass on a staged referral.
pub async fn plan_decline_post(
    State(db): State<ApplicationStore>,
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
        PlanIntakeError::Postgres(_)
        | PlanIntakeError::Sql(_)
        | PlanIntakeError::Open(OpenMatterError::Db(_) | OpenMatterError::Attestation(_))
        | PlanIntakeError::ProjectStore(_)
        | PlanIntakeError::ProjectCommand(_)
        | PlanIntakeError::Firm(_)
        | PlanIntakeError::Person(_)
        | PlanIntakeError::Entity(_)
        | PlanIntakeError::EntityType(_)
        | PlanIntakeError::Jurisdiction(_)
        | PlanIntakeError::Onboarding(_)
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

#[cfg(test)]
mod tests {
    use super::*;
    use store::plan_assignment::PlanProvider;

    fn synthetic_fee() -> store::legal_plans::PlanFeeSchedule {
        let now = chrono::Utc::now();
        store::legal_plans::PlanFeeSchedule {
            id: Uuid::now_v7(),
            firm_id: Uuid::now_v7(),
            legal_plan_id: Uuid::now_v7(),
            matter_type: store::legal_plans::PlanMatterType::DocumentReview,
            state: "nv".into(),
            covered_benefit: "Synthetic document review".into(),
            plan_fee_cents: 9050,
            supplemental_hourly_cents: None,
            supplemental_flat_cents: None,
            effective_on: now.date_naive(),
            inserted_at: now,
        }
    }

    #[tokio::test]
    async fn plan_onboarding_pins_provider_clauses_and_reuses_the_existing_notation() {
        store::test_support::with_postgres(async {
            let db = ApplicationStore::uninitialized();
            let app = crate::test_support::app_state(db.clone()).await;
            store::seed::seed_canonical(&db, &app.storage)
                .await
                .unwrap();
            let person = store::persons::create(
                &db,
                &store::persons::NewPerson::new("Synthetic Member", "member@example.test"),
            )
            .await
            .unwrap();
            let lawyer = store::persons::create(
                &db,
                &store::persons::NewPerson::with_role(
                    "Synthetic Lawyer",
                    "lawyer@example.test",
                    store::persons::Role::Lawyer,
                ),
            )
            .await
            .unwrap();

            for (provider, code, clause) in [
                (PlanProvider::Arag, "onboarding__arag", "legal_plan_arag@1"),
                (
                    PlanProvider::LegalEase,
                    "onboarding__legalease",
                    "legal_plan_legalease@1",
                ),
            ] {
                let project = store::test_support::seed_project(&db, code).await;
                let request = || PlanOnboarding {
                    provider: provider.as_str().into(),
                    project_id: project.id,
                    person_id: person.id,
                    fee: Some(synthetic_fee()),
                };
                let open = || {
                    start_plan_onboarding(
                        &db,
                        app.questionnaire_runtime.as_ref(),
                        &app.storage,
                        request(),
                        lawyer.id,
                    )
                };
                open().await.unwrap();

                let original = store::notations::list_by_project(&db, project.id)
                    .await
                    .unwrap();
                let [notation] = original.as_slice() else {
                    panic!("one onboarding notation per referral");
                };
                let template = store::templates::find_by_id(&db, notation.template_id)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(template.code, code);
                let body = store::templates::body(&db, &app.storage, &template)
                    .await
                    .unwrap();
                let sources = store::seed::seeded_clause_sources(&template.code, &body).unwrap();
                for source in [
                    clause,
                    "engagement_technology@1",
                    "engagement_arbitration@1",
                ] {
                    assert!(sources.iter().any(|pinned| pinned == source));
                }

                if provider == PlanProvider::Arag {
                    let answers = store::answers::for_notation(&db, notation.id)
                        .await
                        .unwrap();
                    assert!(answers.iter().any(|answer| {
                        answer.state_name.as_deref() == Some("custom_usd__plan_fee")
                            && store::answers::primitive_str(&answer.value) == Some("90.50")
                    }));
                }
                open().await.unwrap();
                assert_eq!(
                    store::notations::list_by_project(&db, project.id)
                        .await
                        .unwrap(),
                    original
                );
            }
        })
        .await;
    }
}

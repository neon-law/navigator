//! The plan-intake doors: an attorney's prepare-and-confirm decisions on a
//! staged plan assignment.
//!
//! The review screen (`webapp::plan_intake`) presents the parsed fields, the
//! conflict result, and the fee quote and stops; these doors are the only
//! way it acts. Both re-check the session server-side — the forms are
//! conveniences, never authorities — and the accept door forwards the
//! attorney's attestation to `store::plan_intake::accept`, which forwards it
//! to `store::projects::open_matter`. `open_matter` refuses to open without
//! the attestation, so a missing checkbox can only land back on the queue
//! with an error — never as a matter.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Extension;
use serde::Deserialize;
use uuid::Uuid;

use crate::session::SessionData;
use webapp::plan_intake::PLAN_INTAKE_PATH;

/// The accept form. `attestation` is the checkbox — absent from the form
/// unless the attorney checked it.
#[derive(Debug, Deserialize)]
pub struct PlanAcceptForm {
    #[serde(rename = "_csrf")]
    _csrf: Option<String>,
    attestation: Option<String>,
    plan_matter_type: String,
    plan_state: Option<String>,
    name: Option<String>,
    code: String,
    questions: Option<String>,
}

/// The decline form.
#[derive(Debug, Deserialize)]
pub struct PlanDeclineForm {
    #[serde(rename = "_csrf")]
    _csrf: Option<String>,
    reason: String,
}

/// `POST /app/plan-intake/{staged_id}/accept`
///
/// The attorney's attested acceptance: the store opens the matter on the
/// attestation, fills its plan columns from the email-landing date, and
/// creates the plan's SLA deadlines. The member intro and the plan's
/// confirmation reply go out from here — the intro through the
/// reserve-then-send acknowledgment anchor (at most one send, replay-safe),
/// the reply only for plans whose acceptance is not confirmed on the
/// provider's portal.
pub async fn plan_accept_post(
    State(db): State<store::surreal::SurrealDb>,
    State(email): State<Arc<dyn crate::email::EmailService>>,
    session: Option<Extension<SessionData>>,
    Path(staged_id): Path<Uuid>,
    axum::Form(form): axum::Form<PlanAcceptForm>,
) -> Response {
    let Some(actor_id) = crate::inbox::actor_id(session) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // Acceptance works the firm's plan data — plans and fee schedules are
    // PostgreSQL-authoritative, so without the handle there is nothing to
    // accept against.
    let Some(pg) = store::postgres::PostgresDb::from_env().await.ok() else {
        return crate::inbox::redirect_error(
            PLAN_INTAKE_PATH,
            "Acceptance needs the firm's plan data (PostgreSQL), which is not configured.",
        );
    };
    let input = store::plan_intake::AcceptInput {
        attestation: form.attestation.as_deref() == Some("yes"),
        acting_person_id: actor_id,
        brand: views::brand::brand_key().as_str().to_string(),
        code: form.code.trim().to_string(),
        name: nonblank(form.name.as_ref()),
        // The staged record's resolved plan is what the fee quote was read
        // from; an unresolved record (no matching plan) is refused here
        // rather than re-picked, so the fee the attorney saw is always the
        // plan the matter opens under.
        legal_plan_id: match store::plan_intake::find_by_id(&db, staged_id).await {
            Ok(Some(staged)) => match staged.legal_plan_id {
                Some(plan_id) => plan_id,
                None => {
                    return crate::inbox::redirect_error(
                        PLAN_INTAKE_PATH,
                        "The notice's plan did not match a firm plan — resolve the plan before accepting.",
                    );
                }
            },
            Ok(None) => return StatusCode::NOT_FOUND.into_response(),
            Err(error) => {
                return crate::inbox::redirect_error(PLAN_INTAKE_PATH, &error.to_string())
            }
        },
        plan_matter_type: form.plan_matter_type.trim().to_string(),
        plan_state: nonblank(form.plan_state.as_ref()).map(|state| state.to_lowercase()),
        questions: form
            .questions
            .as_deref()
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|question| !question.is_empty())
            .map(ToOwned::to_owned)
            .collect(),
    };
    let accepted = match store::plan_intake::accept(&db, &pg, staged_id, &input).await {
        Ok(accepted) => accepted,
        Err(error) => {
            return crate::inbox::redirect_error(PLAN_INTAKE_PATH, &error.to_string());
        }
    };
    let staged = &accepted.staged;
    if let Err(error) = send_member_intro(&db, email.as_ref(), staged, &input.questions).await {
        tracing::warn!(error = %error, staged_id = %staged_id, "member intro send failed");
    }
    if !staged.plan_portal_only {
        if let Err(error) = send_plan_reply(&db, email.as_ref(), staged).await {
            tracing::warn!(error = %error, staged_id = %staged_id, "plan reply send failed");
        }
    }
    Redirect::to(&format!("/app/projects/{}", input.code)).into_response()
}

/// `POST /app/plan-intake/{staged_id}/decline`
///
/// The attorney passes on the assignment; the reason is recorded on the
/// staged record for the plan's audit trail.
pub async fn plan_decline_post(
    State(db): State<store::surreal::SurrealDb>,
    session: Option<Extension<SessionData>>,
    Path(staged_id): Path<Uuid>,
    axum::Form(form): axum::Form<PlanDeclineForm>,
) -> Response {
    if crate::inbox::actor_id(session).is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    match store::plan_intake::decline(&db, staged_id, &form.reason).await {
        Ok(_) => Redirect::to(PLAN_INTAKE_PATH).into_response(),
        Err(error) => crate::inbox::redirect_error(PLAN_INTAKE_PATH, &error.to_string()),
    }
}

/// The member's intro, reserved against the notice's inbound message before
/// anything is sent — the reservation is idempotent per message, so a
/// replayed accept (or a retry after a provider timeout) can never send a
/// second intro.
async fn send_member_intro(
    db: &store::surreal::SurrealDb,
    service: &dyn crate::email::EmailService,
    staged: &store::plan_intake::StagedAssignment,
    questions: &[String],
) -> Result<(), String> {
    let Some(member_email) = staged.member_email.clone() else {
        return Ok(());
    };
    let conversation = match store::inbox::conversation_by_id(db, staged.conversation_id).await {
        Ok(Some(conversation)) => conversation,
        Ok(None) => return Err("assignment conversation missing".to_string()),
        Err(error) => return Err(error.to_string()),
    };
    let firm_name = store::firms::find_by_id(db, staged.firm_id)
        .await
        .map_err(|error| error.to_string())?
        .map_or_else(|| "our firm".to_string(), |firm| firm.name);
    let matter_type = store::legal_plans::PlanMatterType::parse(&staged.plan_matter_type)
        .map_err(|_| "unknown matter type".to_string())?;
    let body = store::plan_intake::member_intro_body(
        &staged.member_name,
        &firm_name,
        store::plan_intake::matter_label(matter_type),
        questions,
    );
    let inbound = store::inbox::IngestedInbound {
        conversation_id: staged.conversation_id,
        message_id: staged.source_message_id,
        deduped: false,
        person_linked: false,
    };
    let Some(reserved) = store::inbox::reserve_acknowledgment(db, &inbound, &body)
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(()); // Already reserved — the intro went out or is queued.
    };
    let mut outbound = crate::email::OutboundEmail::new(
        member_email,
        format!(
            "Your {} matter — next steps",
            store::plan_intake::matter_label(matter_type)
        ),
        body.clone(),
    )
    .with_from(conversation.provider_account.clone())
    .with_reply_to(conversation.provider_account.clone());
    outbound.auto_submitted = true;
    if let Ok(Some(message)) = store::inbox::message_by_id(db, staged.source_message_id).await {
        if let Some(provider_message_id) = message.provider_message_id {
            outbound = outbound.with_thread_refs(&[provider_message_id]);
        }
    }
    match service.send(outbound).await {
        Ok(receipt) => store::inbox::acknowledgment_submitted(db, reserved, receipt.message_id)
            .await
            .map_err(|error| error.to_string()),
        // The reservation stands: a later replay of this door sees the
        // reserved row and refuses to send twice, and intake succeeds
        // without the external send.
        Err(_) => Ok(()),
    }
}

/// The plan provider's acceptance reply, threaded under the original notice.
/// Portal-only plans never reach here — their acceptance is confirmed on the
/// provider's site, and the manual reminder deadline covers it.
async fn send_plan_reply(
    db: &store::surreal::SurrealDb,
    service: &dyn crate::email::EmailService,
    staged: &store::plan_intake::StagedAssignment,
) -> Result<(), String> {
    let conversation = match store::inbox::conversation_by_id(db, staged.conversation_id).await {
        Ok(Some(conversation)) => conversation,
        Ok(None) => return Err("assignment conversation missing".to_string()),
        Err(error) => return Err(error.to_string()),
    };
    let matter_type = store::legal_plans::PlanMatterType::parse(&staged.plan_matter_type)
        .map_err(|_| "unknown matter type".to_string())?;
    let body = format!(
        "Accepted: the {} matter for {} (member {}, plan {}) is open with our firm. \
         We will follow up directly with the member.\n",
        store::plan_intake::matter_label(matter_type),
        staged.member_name,
        staged.plan_member_id.as_deref().unwrap_or("number pending"),
        staged.plan_number.as_deref().unwrap_or("plan pending"),
    );
    let subject = conversation.subject.clone().map_or_else(
        || "Re: legal plan assignment".to_string(),
        |subject| format!("Re: {subject}"),
    );
    let mut outbound =
        crate::email::OutboundEmail::new(conversation.external_address.clone(), subject, body)
            .with_from(conversation.provider_account.clone())
            .with_reply_to(conversation.provider_account.clone());
    outbound.auto_submitted = true;
    if let Ok(Some(message)) = store::inbox::message_by_id(db, staged.source_message_id).await {
        if let Some(provider_message_id) = message.provider_message_id {
            outbound = outbound.with_thread_refs(&[provider_message_id]);
        }
    }
    // The plan reply is a courtesy confirmation, not an acknowledgment with
    // a reservation: providers tolerate duplicates, and a replayed accept
    // refuses earlier at the store's NotStaged gate before reaching here.
    service
        .send(outbound)
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn nonblank(value: Option<&String>) -> Option<String> {
    value
        .map(|s| s.trim())
        .filter(|trimmed| !trimmed.is_empty())
        .map(ToOwned::to_owned)
}

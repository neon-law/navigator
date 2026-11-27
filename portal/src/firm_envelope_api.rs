//! `/app/api/envelopes/{envelope_id}` (LAW-501, LAW-430): read or void one
//! envelope in the caller's Firm DocuSign account by its ID, whether or not
//! a Project records it — an envelope sent from the DocuSign web app
//! included.
//!
//! The scope is Firm-wide: an Admin or Lawyer of a Firm with DocuSign
//! configured holds `firm_envelopes`, as a seat in that account would. A
//! read never registers, files, or advances anything. Its `navigator` block
//! says which Project records or matches the envelope, naming the Project
//! only to a caller with firm-side access to it, or why it matches none.
use super::{participates_as_firm, ApiError, ApiState, LawyerSession};
use crate::signature::{DocuSignSignatureProvider, ExternalEnvelope};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;
use store::firm_capability::FirmCapability;
use uuid::Uuid;

/// DocuSign's limit on `voidedReason`.
const VOID_REASON_MAX: usize = 200;

pub(super) async fn read(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(envelope_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    use crate::signature_adoption::internal;
    let (firm, provider, envelope) = match locate(&state, &lawyer, envelope_id).await? {
        Ok(found) => found,
        Err(refusal) => return Ok(refusal),
    };
    let db = &state.application_store;
    let notation = match store::signatures::by_provider(
        db,
        store::signatures::SignatureProvider::DocuSign,
        &envelope_id.to_string(),
    )
    .await
    .map_err(internal)?
    {
        Some(signature) => store::notations::find_by_id(db, signature.notation_id).await?,
        None => None,
    };
    let project = if let Some(notation) = notation.as_ref() {
        Ok(notation.project_id)
    } else {
        let (fields, names) = provider
            .envelope_project_hints(envelope_id)
            .await
            .map_err(internal)?;
        crate::docusign_completion::identify_project(db, firm, &envelope, &fields, &names)
            .await
            .map_err(internal)?
            .map(|project| project.id)
    };
    let (code, reason) = match project {
        Ok(id) if participates_as_firm(&state, &lawyer, id).await => (
            store::projects::find_by_id(db, id)
                .await
                .map_err(internal)?
                .map(|project| project.code),
            None,
        ),
        Ok(_) => (None, Some("project_not_visible")),
        Err(reason) => (None, Some(reason)),
    };
    let visible = code.is_some();
    let mut body = crate::signature_status::project_envelope_json(
        db,
        &state.storage,
        notation.as_ref().filter(|_| visible),
        &envelope,
    )
    .await
    .map_err(internal)?;
    body["navigator"] = json!({
        "recorded": notation.is_some(),
        "project": code,
        "notation_id": notation.filter(|_| visible).map(|notation| notation.id),
        "reason": reason,
    });
    Ok(Json(body).into_response())
}

#[derive(Deserialize)]
pub(super) struct VoidRequest {
    reason: String,
}

/// Void a sent envelope, or move a held draft to DocuSign's recycle bin.
pub(super) async fn void(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(envelope_id): Path<Uuid>,
    Json(input): Json<VoidRequest>,
) -> Result<Response, ApiError> {
    let reason = input.reason.trim();
    if reason.is_empty() || reason.chars().count() > VOID_REASON_MAX {
        return Ok(refusal(
            StatusCode::UNPROCESSABLE_ENTITY,
            "void_reason",
            &format!("Give a reason of 1 to {VOID_REASON_MAX} characters; DocuSign shows it to the recipients"),
        ));
    }
    let (_, provider, envelope) = match locate(&state, &lawyer, envelope_id).await? {
        Ok(found) => found,
        Err(refusal) => return Ok(refusal),
    };
    let outcome = match envelope.status.as_str() {
        "created" => provider.delete_draft(envelope_id).await.map(|()| "deleted"),
        "sent" | "delivered" | "correct" => provider
            .void_envelope(envelope_id, reason)
            .await
            .map(|()| "voided"),
        status => {
            return Ok(refusal(
                StatusCode::CONFLICT,
                "envelope_closed",
                &format!("Envelope {envelope_id} is {status}; only a draft or an envelope out for signature can be voided"),
            ))
        }
    };
    match outcome {
        Ok(status) => {
            Ok(Json(json!({"envelopeId": envelope_id, "status": status})).into_response())
        }
        Err(error) => Err(ApiError::SignatureUnavailable(error)),
    }
}

/// Find the envelope in the first Firm account the caller may reach that
/// returns it. The inner `Err` is the refusal to answer with.
async fn locate(
    state: &ApiState,
    lawyer: &LawyerSession,
    envelope_id: Uuid,
) -> Result<Result<(Uuid, DocuSignSignatureProvider, ExternalEnvelope), Response>, ApiError> {
    use crate::signature::SignatureProvider as _;
    let db = &state.application_store;
    let firms = store::firm_capability::allowed_firm_ids(
        db,
        lawyer.0.role,
        lawyer.0.person_id,
        FirmCapability::FirmEnvelopes,
    )
    .await
    .map_err(crate::signature_adoption::internal)?;
    let mut provider_errors = Vec::new();
    for firm in firms {
        let Some(provider) = state
            .signature_provider
            .for_firm(db, firm)
            .await
            .ok()
            .flatten()
        else {
            continue;
        };
        match provider.read_external_envelope(envelope_id).await {
            Ok(envelope) => return Ok(Ok((firm, provider, envelope))),
            Err(error) => provider_errors.push(error.to_string()),
        }
    }
    Ok(Err(if provider_errors.is_empty() {
        refusal(
            StatusCode::SERVICE_UNAVAILABLE,
            "docusign_not_configured",
            "No Firm you can reach has a DocuSign account configured",
        )
    } else {
        refusal(
            StatusCode::NOT_FOUND,
            "envelope_not_found",
            &format!(
                "Envelope {envelope_id} is not in your Firm's DocuSign account: {}",
                provider_errors.join("; ")
            ),
        )
    }))
}

fn refusal(status: StatusCode, error: &str, message: &str) -> Response {
    (status, Json(json!({"error": error, "message": message}))).into_response()
}

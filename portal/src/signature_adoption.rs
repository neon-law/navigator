//! Register an envelope sent from the Firm's DocuSign account.

use std::sync::Arc;

use cloud::StorageService;
use store::signatures::SignatureProvider::DocuSign;
use uuid::Uuid;

use crate::api::ApiError;
use crate::signature::{ExternalEnvelope, SignatureProvider, SignatureRequestId};

fn evidence_key(notation_id: Uuid) -> String {
    format!("notations/{notation_id}/docusign-envelope.json")
}

pub(crate) fn internal(error: impl std::fmt::Display) -> ApiError {
    ApiError::Db(error.to_string())
}

pub(crate) async fn evidence(
    storage: &dyn StorageService,
    notation_id: Uuid,
) -> Result<Option<ExternalEnvelope>, ApiError> {
    match storage.get(&evidence_key(notation_id)).await {
        Ok(object) => serde_json::from_slice(&object.bytes)
            .map(Some)
            .map_err(internal),
        Err(cloud::StorageError::NotFound(_)) => Ok(None),
        Err(error) => Err(internal(error)),
    }
}

/// File both documents before the Signature can make an engagement official.
pub(crate) async fn reconcile(
    db: &store::application_store::ApplicationStore,
    storage: &Arc<dyn StorageService>,
    provider: &dyn SignatureProvider,
    notation_id: Uuid,
    envelope: &ExternalEnvelope,
) -> Result<(), ApiError> {
    let envelope_id = envelope.envelope_id;
    let request = SignatureRequestId(envelope_id.to_string());
    validate(envelope)?;
    let completed = envelope
        .completed_date_time
        .filter(|_| envelope.status == "completed");

    if evidence(storage.as_ref(), notation_id)
        .await?
        .is_some_and(|previous| previous.envelope_id != envelope_id)
    {
        return Err(ApiError::EnvelopeAdoption(
            "This Notation already names another envelope",
        ));
    }

    // Evidence is written before the correlation row so a Connect delivery
    // arriving as the row lands can use the same reconciliation path.
    let bytes = serde_json::to_vec(envelope).map_err(internal)?;
    storage
        .put(&evidence_key(notation_id), &bytes, "application/json")
        .await
        .map_err(internal)?;
    let signature = store::signatures::record_request(db, notation_id, DocuSign, &request.0)
        .await
        .map_err(internal)?;
    if signature.notation_id != notation_id {
        return Err(ApiError::EnvelopeAdoption(
            "This envelope is already linked to another Notation",
        ));
    }

    if let Some(completed) = completed {
        if signature.state == store::signatures::SignatureState::Completed {
            return Ok(());
        }
        let docs = provider
            .fetch_completed_documents(&request)
            .await
            .map_err(internal)?;
        if !docs.signed_pdf.starts_with(b"%PDF-") || !docs.certificate_pdf.starts_with(b"%PDF-") {
            return Err(internal("DocuSign did not return two PDFs"));
        }
        crate::esignature_webhook::file_completed_documents(
            db,
            storage,
            &request.0,
            notation_id,
            completed.date_naive(),
            Some(&request.0),
            docs,
        )
        .await
        .map_err(internal)?;
        if !store::signatures::stamp_signed(db, DocuSign, &request.0, &completed.to_rfc3339())
            .await
            .map_err(internal)?
        {
            return Err(ApiError::EnvelopeAdoption(
                "The recorded Signature has a conflicting outcome",
            ));
        }
    } else {
        match envelope.status.as_str() {
            "declined" => store::signatures::stamp_declined(db, DocuSign, &request.0)
                .await
                .map_err(internal)?,
            "expired" => store::signatures::stamp_expired(db, DocuSign, &request.0)
                .await
                .map_err(internal)?,
            "voided" => store::signatures::stamp_voided(db, DocuSign, &request.0)
                .await
                .map_err(internal)?,
            _ => false,
        };
    }
    Ok(())
}

pub(crate) fn validate(envelope: &ExternalEnvelope) -> Result<(), ApiError> {
    match envelope.status.as_str() {
        "completed" if envelope.completed_date_time.is_none() => Err(ApiError::EnvelopeAdoption(
            "DocuSign did not return a completion date",
        )),
        "sent" | "delivered" | "signed" | "completed" | "declined" | "voided" | "expired" => Ok(()),
        _ => Err(ApiError::EnvelopeAdoption(
            "DocuSign has not sent this envelope",
        )),
    }
}

// One lane covers both competing bindings: the same DocuSign envelope must
// never attach to two Projects, and the same Notation must never adopt two
// envelopes. Sharing it with Connect keeps provider snapshots in order.
pub(crate) async fn lock() -> Result<store::notation_machine_state::LaneGuard, ApiError> {
    const ADOPTION_LANE: Uuid = Uuid::from_u128(0xd9de8b8b_e2f4_4dd2_81b0_638ff2348530);
    let pg = store::postgres::PostgresDb::from_env()
        .await
        .map_err(internal)?;
    store::notation_machine_state::acquire_lane(&pg, ADOPTION_LANE)
        .await
        .map_err(internal)
}

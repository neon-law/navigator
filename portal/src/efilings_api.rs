//! E-Filing Transaction status and independently verified fee settlement.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use store::efiling_transactions::{self as efiling, fees};
use uuid::Uuid;

use crate::api::{ApiError, ApiState, LawyerSession};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordEfilingRequest {
    transaction_id: String,
    status: String,
    efsp: Option<String>,
    court: Option<String>,
    case_number: Option<String>,
    submitted_document_id: Option<Uuid>,
    filed_on: Option<String>,
    accepted_on: Option<String>,
    conformed_document_id: Option<Uuid>,
    needs_entered_order: Option<bool>,
    note: Option<String>,
    fee_receipt_document_id: Option<Uuid>,
}

async fn require_participant(
    state: &ApiState,
    session: &crate::SessionData,
    project_id: Uuid,
) -> Result<store::access::MatterViewer, ApiError> {
    store::access::matter_viewer(
        &state.application_store,
        session.person_id,
        session.role,
        project_id,
    )
    .await
    .map_err(ApiError::Db)?
    .ok_or(ApiError::NotFound)
}

fn map_efiling(error: efiling::EfilingError) -> ApiError {
    use store::project_costs::ProjectCostError;
    match error {
        efiling::EfilingError::Invalid(message) => ApiError::EfilingInvalid(message),
        efiling::EfilingError::Cost(
            ProjectCostError::SourceTaken | ProjectCostError::ReceiptTaken,
        ) => ApiError::EfilingInvalid("the charge or receipt is already allocated".into()),
        other => ApiError::Db(other.to_string()),
    }
}

/// `GET /app/api/projects/{id}/efilings`
pub(crate) async fn list_efilings(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let viewer = require_participant(&state, &session, id).await?;
    let rows = efiling::list(&state.application_store, id)
        .await
        .map_err(map_efiling)?;
    let mut observations = if viewer.is_firm_side() {
        fees::observations(id).await.map_err(map_efiling)?
    } else {
        std::collections::HashMap::new()
    };
    let filings: Vec<_> = rows
        .into_iter()
        .map(|filing| fees::FilingWithFee {
            fee: observations.remove(&filing.id),
            filing,
        })
        .collect();
    Ok(Json(filings).into_response())
}

/// `POST /app/api/projects/{id}/efilings`
pub(crate) async fn record_efiling(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(id): Path<Uuid>,
    Json(input): Json<RecordEfilingRequest>,
) -> Result<Response, ApiError> {
    let viewer = require_participant(&state, &session, id).await?;
    if input.fee_receipt_document_id.is_some() && !viewer.is_firm_side() {
        return Err(ApiError::NotFound);
    }
    let row = efiling::record_with_fee(
        &state.application_store,
        &efiling::EfilingReport {
            project_id: id,
            transaction_id: input.transaction_id,
            status: input.status,
            efsp: input.efsp,
            court: input.court,
            case_number: input.case_number,
            submitted_document_id: input.submitted_document_id,
            filed_on: input.filed_on,
            accepted_on: input.accepted_on,
            conformed_document_id: input.conformed_document_id,
            needs_entered_order: input.needs_entered_order,
            note: input.note,
        },
        input.fee_receipt_document_id,
    )
    .await
    .map_err(map_efiling)?;
    Ok(Json(row).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SettleFilingFeeRequest {
    envelope: String,
    account_id: Uuid,
    transaction_id: Uuid,
    confirm: bool,
}

/// Resolve the Firm's existing read-only Mercury credential after both scope
/// gates. The provider read names exactly one account and transaction.
async fn read_charge(
    state: &ApiState,
    project_id: Uuid,
    bank_account: Uuid,
    transaction: Uuid,
) -> anyhow::Result<cloud::mercury::Transaction> {
    use store::firm_secrets::{
        resolve_project_credential, IntegrationProvider, IntegrationSecretKind,
    };
    let kms = crate::integrations::runtime_kms_from_env().await;
    let credential = resolve_project_credential(
        &state.application_store,
        project_id,
        IntegrationProvider::Mercury,
        IntegrationSecretKind::MercuryReadToken,
        kms.as_ref(),
    )
    .await?;
    let client = cloud::mercury::Mercury::new(credential.expose_for_provider_call())?;
    Ok(client.transaction(bank_account, transaction).await?.0)
}

fn verified_charge(
    transaction: &cloud::mercury::Transaction,
) -> Result<fees::SettledCharge, ApiError> {
    let refused =
        || ApiError::EfilingInvalid("select a posted, settled Mercury card charge".into());
    if transaction.status != "sent"
        || !matches!(
            transaction.kind.as_deref(),
            Some("creditCardTransaction" | "debitCardTransaction")
        )
    {
        return Err(refused());
    }
    let posted_on = transaction
        .posted_at
        .as_deref()
        .and_then(|date| date.get(..10))
        .and_then(|date| chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok())
        .ok_or_else(refused)?;
    // Card charges come from a typed debit or credit-card kind; the amount
    // uses the account's posting sign. Refund kinds are refused above.
    let amount_cents = store::project_costs::cents_from_exact_decimal(transaction.amount.get())
        .ok()
        .and_then(i64::checked_abs)
        .filter(|amount| *amount > 0)
        .ok_or_else(refused)?;
    Ok(fees::SettledCharge {
        bank_account: transaction.account_id,
        transaction: transaction.id,
        amount_cents,
        posted_on,
    })
}

/// `POST /app/api/projects/{id}/efilings/settle-fee`
pub(crate) async fn settle_fee(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(id): Path<Uuid>,
    Json(input): Json<SettleFilingFeeRequest>,
) -> Result<Response, ApiError> {
    let viewer = require_participant(&state, &session, id).await?;
    if !viewer.is_firm_side() {
        return Err(ApiError::NotFound);
    }
    let actor = session.person_id.ok_or(ApiError::NotFound)?;
    let firm_id = store::projects::find_by_id(&state.application_store, id)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?
        .and_then(|project| project.firm_id)
        .ok_or(ApiError::NotFound)?;
    if !store::firm_capability::FirmCapability::ManageJournal
        .resolve(&state.application_store, session.role, Some(actor), firm_id)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?
        .is_allowed()
    {
        return Err(ApiError::NotFound);
    }
    if !input.confirm {
        return Err(ApiError::EfilingInvalid(
            "confirm the reviewed match between the envelope receipt and the selected bank charge"
                .into(),
        ));
    }
    let envelope =
        efiling::normalize_transaction_id(&input.envelope).map_err(ApiError::EfilingInvalid)?;
    let pg = store::postgres::PostgresDb::from_env()
        .await
        .map_err(|_| ApiError::Db("PostgreSQL billing unavailable".into()))?;
    if let Some(id) =
        fees::existing_settlement(&pg, id, &envelope, input.account_id, input.transaction_id)
            .await
            .map_err(map_efiling)?
    {
        return Ok(Json(serde_json::json!({"project_cost_id":id})).into_response());
    }
    let Ok(provider) = read_charge(&state, id, input.account_id, input.transaction_id).await else {
        return Ok((
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error":"bank_evidence_unavailable"})),
        )
            .into_response());
    };
    let transaction = verified_charge(&provider)?;
    let cost = Box::pin(fees::settle(&pg, id, actor, &envelope, &transaction))
        .await
        .map_err(map_efiling)?;
    Ok(Json(serde_json::json!({"project_cost_id":cost})).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn card_transaction(kind: &str, status: &str, amount: f64) -> cloud::mercury::Transaction {
        serde_json::from_str(
            &json!({
                "id": Uuid::new_v4(),
                "accountId": Uuid::new_v4(),
                "kind": kind,
                "status": status,
                "postedAt": "2026-11-16T12:00:00Z",
                "amount": amount,
            })
            .to_string(),
        )
        .unwrap()
    }

    #[test]
    fn filing_fee_requires_an_exact_posted_card_purchase() {
        for kind in ["creditCardTransaction", "debitCardTransaction"] {
            let transaction = card_transaction(kind, "sent", -12.25);
            let charge = verified_charge(&transaction).unwrap();
            assert_eq!(charge.amount_cents, 1_225);
            assert_eq!(charge.bank_account, transaction.account_id);
            assert_eq!(charge.transaction, transaction.id);
        }
        for (kind, status, amount) in [
            ("debitCardCredit", "sent", 12.25),
            ("externalTransfer", "sent", -12.25),
            ("debitCardTransaction", "pending", -12.25),
            ("debitCardTransaction", "sent", -12.251),
            ("debitCardTransaction", "sent", 0.0),
        ] {
            assert!(verified_charge(&card_transaction(kind, status, amount)).is_err());
        }
        let mut unposted = card_transaction("debitCardTransaction", "sent", -12.25);
        unposted.posted_at = None;
        assert!(verified_charge(&unposted).is_err());
    }
}

//! Where an invoice's payment lands (ENG-1343): the matter's state IOLTA, the
//! Firm's Nevada IOLTA where that is permitted, or — only by a lawyer's
//! recorded approval for one invoice — the Firm's Stripe Operating account.
//! A missing trust route never selects Operating on its own.
//!
//! The approval is the audit record: who approved, when, why, which
//! destination, and which invoice or Matter Billing Batch it was for. It is
//! retained and never edited, and a draft and every retry stay pinned to it.
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::{firm_secrets::TrustRoute, postgres::PostgresDb};

#[derive(Debug, thiserror::Error)]
pub enum DestinationError {
    /// No trust account receives this matter's payments, and no lawyer
    /// approved Operating for this invoice.
    #[error(
        "no trust account receives this matter's payments; link the state's IOLTA, or approve \
         the Operating account for this invoice with a reason"
    )]
    TrustRouteMissing,
    /// Only the matter's responsible lawyer approves Operating collection.
    #[error("only the matter's responsible lawyer may approve the Operating account")]
    NotResponsibleLawyer,
    /// Unearned money goes to trust: an invoice for an advance fee, deposit,
    /// or flat fee may not collect into Operating.
    #[error("an invoice for unearned fees collects into trust, never Operating")]
    NotEligible,
    #[error("{0}")]
    Invalid(&'static str),
    #[error("destination storage failed")]
    Database(#[from] sqlx::Error),
}

/// A lawyer's request, on the invoice command, to collect into Operating.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatingRequest {
    pub reason: String,
}

/// The retained audit record of one Operating approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OperatingApproval {
    pub id: Uuid,
    pub firm_id: Uuid,
    pub project_id: Uuid,
    pub approved_by: Uuid,
    pub approved_at: DateTime<Utc>,
    pub reason: String,
    /// The reviewed invoice digest the approval was given for.
    pub preview_digest: String,
}

/// Choose the destination: an approved Operating request first, then the
/// trust route (exact state, else the permitted Nevada default), else a
/// refusal. Eligibility and the approver's authority are checked here, at
/// the server boundary, before any work is reserved.
///
/// # Errors
/// [`DestinationError::TrustRouteMissing`] with neither a route nor an
/// approval; [`DestinationError::NotResponsibleLawyer`] or
/// [`DestinationError::NotEligible`] for an approval the matter does not
/// permit.
pub fn choose(
    route: Option<TrustRoute>,
    operating: Option<&OperatingRequest>,
    responsible_lawyer: bool,
    unearned: bool,
) -> Result<Option<TrustRoute>, DestinationError> {
    match operating {
        Some(request) => {
            let reason = request.reason.trim();
            if reason.is_empty() || reason.len() > 1000 {
                return Err(DestinationError::Invalid(
                    "an Operating approval needs a reason of 1 to 1000 characters",
                ));
            }
            if !responsible_lawyer {
                return Err(DestinationError::NotResponsibleLawyer);
            }
            if unearned {
                return Err(DestinationError::NotEligible);
            }
            Ok(None)
        }
        None => route.map(Some).ok_or(DestinationError::TrustRouteMissing),
    }
}

/// Record one approval inside the caller's transaction, before the invoice
/// row that references it.
///
/// # Errors
/// A database failure.
pub async fn record_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    firm_id: Uuid,
    project_id: Uuid,
    approved_by: Uuid,
    reason: &str,
    preview_digest: &str,
) -> Result<OperatingApproval, DestinationError> {
    let (id, approved_at): (Uuid, DateTime<Utc>) = sqlx::query_as(
        "INSERT INTO invoice_destination_approval
         (id,firm_id,project_id,destination,reason,preview_digest,approved_by)
         VALUES ($1,$2,$3,'operating',$4,$5,$6) RETURNING id,approved_at",
    )
    .bind(Uuid::now_v7())
    .bind(firm_id)
    .bind(project_id)
    .bind(reason.trim())
    .bind(preview_digest)
    .bind(approved_by)
    .fetch_one(&mut **tx)
    .await?;
    Ok(OperatingApproval {
        id,
        firm_id,
        project_id,
        approved_by,
        approved_at,
        reason: reason.trim().to_owned(),
        preview_digest: preview_digest.to_owned(),
    })
}

/// Read one approval, scoped to its Firm and Project.
///
/// # Errors
/// A database failure.
pub async fn find(
    pg: &PostgresDb,
    firm_id: Uuid,
    project_id: Uuid,
    id: Uuid,
) -> Result<Option<OperatingApproval>, DestinationError> {
    let row: Option<(Uuid, DateTime<Utc>, String, String)> = sqlx::query_as(
        "SELECT approved_by,approved_at,reason,preview_digest FROM invoice_destination_approval
         WHERE id=$1 AND firm_id=$2 AND project_id=$3",
    )
    .bind(id)
    .bind(firm_id)
    .bind(project_id)
    .fetch_optional(&pg.0)
    .await?;
    Ok(row.map(
        |(approved_by, approved_at, reason, preview_digest)| OperatingApproval {
            id,
            firm_id,
            project_id,
            approved_by,
            approved_at,
            reason,
            preview_digest,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route() -> TrustRoute {
        serde_json::from_value(serde_json::json!({
            "firm": Uuid::nil(), "state": "NV", "account": "acct_Nevada"
        }))
        .unwrap()
    }

    #[test]
    fn operating_is_chosen_only_by_the_responsible_lawyer_for_earned_work() {
        let request = OperatingRequest {
            reason: "Earned fees for completed work".into(),
        };
        assert!(choose(Some(route()), None, false, false).unwrap().is_some());
        assert!(matches!(
            choose(None, None, true, false),
            Err(DestinationError::TrustRouteMissing)
        ));
        assert!(choose(None, Some(&request), true, false).unwrap().is_none());
        assert!(choose(Some(route()), Some(&request), true, false)
            .unwrap()
            .is_none());
        assert!(matches!(
            choose(None, Some(&request), false, false),
            Err(DestinationError::NotResponsibleLawyer)
        ));
        assert!(matches!(
            choose(None, Some(&request), true, true),
            Err(DestinationError::NotEligible)
        ));
        let blank = OperatingRequest { reason: " ".into() };
        assert!(matches!(
            choose(None, Some(&blank), true, false),
            Err(DestinationError::Invalid(_))
        ));
    }
}

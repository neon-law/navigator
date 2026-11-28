//! Read-only billing views used by Navigator and the portal.
use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Claims with verified fee terms awaiting a lawyer's filing review.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct FilingReviewTotal {
    pub eligible_unsubmitted_count: usize,
    pub eligible_unsubmitted_cents: i64,
}

/// Reporting both carriers makes an empty queue visible in the requested scope.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct FilingReview {
    #[serde(rename = "ARAG")]
    pub arag: FilingReviewTotal,
    #[serde(rename = "LegalEASE")]
    pub legal_ease: FilingReviewTotal,
    #[serde(flatten)]
    pub other_plans: BTreeMap<String, FilingReviewTotal>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct BillingReport {
    pub needs_my_action: Vec<BillingAction>,
    pub my_time: Vec<BillingTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_time: Option<Vec<BillingTime>>,
    pub verified_projected_fees: Vec<FeeProjection>,
    pub unverified_fees: Vec<UnverifiedFee>,
    pub contingency: Vec<ContingencyBilling>,
    pub other_claims: Vec<ClaimBilling>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BillingAction {
    Claim(ClaimBilling),
    PendingAnnotation {
        project: String,
        annotation_id: Uuid,
        change_id: String,
        performed_on: NaiveDate,
        next_action: String,
    },
    UnallocatedWork {
        project: String,
        next_action: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ClaimBilling {
    pub project: String,
    pub id: Uuid,
    pub benefit: String,
    pub performed_on: NaiveDate,
    pub status: String,
    pub next_action: Option<String>,
    pub projected_fee_cents: Option<i64>,
    pub filed_cents: Option<i32>,
    pub approved_outstanding_cents: Option<i64>,
    pub net_cash_received_cents: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BillingTime {
    pub project: String,
    pub id: Uuid,
    pub performed_on: NaiveDate,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timekeeper: Option<Uuid>,
    pub minutes: i32,
    pub description: String,
    pub status: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FeeProjection {
    pub project: String,
    pub basis: String,
    pub projected_fee_cents: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UnverifiedFee {
    pub project: String,
    pub basis: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ContingencyBilling {
    pub project: String,
    pub fee_agreement_notation_id: Uuid,
    pub effective_on: NaiveDate,
    pub costs_deducted: String,
    pub recovery_basis: String,
    pub tiers: Vec<ContingencyTier>,
    pub hybrid_rate_cents_per_hour: Option<i32>,
    pub estimate_recorded_on: Option<NaiveDate>,
    pub stale_estimate: bool,
    pub outcomes: Vec<ContingencyScenario>,
    pub projected_fee_cents: Option<i64>,
    pub expected_cash_cents: Option<i64>,
    pub fee_received_cents: Option<i64>,
    pub costs_recovered_cents: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ContingencyScenario {
    pub outcome: String,
    pub probability_bps: i32,
    pub gross_recovery_cents: Option<i64>,
    pub fee_bps: i32,
    pub fee_cents: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ContingencyTier {
    pub stage: String,
    pub fee_bps: i32,
}

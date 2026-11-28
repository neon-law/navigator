//! Read-only fee projections and work attribution for the billing commands.
use std::collections::HashSet;

use chrono::NaiveDate;
use model::billing_report::{
    BillingAction, BillingReport, BillingTime, ClaimBilling, ContingencyBilling,
    ContingencyScenario, ContingencyTier, FeeProjection, UnverifiedFee,
};
use uuid::Uuid;

use crate::{
    attorney_rates, billing_annotations, billing_batches,
    contingency_terms::ContingencyTerm,
    matter_projections::{self, ContingentProjection, OddsSource},
    plan_claims,
    postgres::PostgresDb,
    projects::Project,
    time_entries::{self, BillingStatus, TimeEntry},
    time_proposals,
};

#[derive(Debug, thiserror::Error)]
pub enum BillingReportError {
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Claims(#[from] plan_claims::ClaimError),
    #[error(transparent)]
    Time(#[from] time_entries::TimeEntryError),
    #[error(transparent)]
    ContingencyTerm(#[from] crate::contingency_terms::ContingencyTermError),
    #[error(transparent)]
    MatterProjection(#[from] Box<matter_projections::MatterProjectionError>),
    #[error(transparent)]
    Rate(#[from] attorney_rates::FeeStructureError),
    #[error(transparent)]
    BillingBatch(#[from] billing_batches::BillingBatchError),
}

struct MatterTime {
    entries: Vec<TimeEntry>,
    claim_entries: HashSet<Uuid>,
    has_plan_claims: bool,
    has_completed_work: bool,
}

/// Add a Project already scoped to the caller's firm-side participation. The
/// claim scope is supplied only after the caller's Firm capability is verified.
pub async fn add_project(
    report: &mut BillingReport,
    pg: &PostgresDb,
    project: &Project,
    attorney: Uuid,
    on: NaiveDate,
    scope: ProjectScope,
) -> Result<(), BillingReportError> {
    let time = collect_time_and_claims(report, pg, project, attorney, on, &scope).await?;
    let term = crate::contingency_terms::resolve(pg, attorney, project.id, on).await?;
    if let Some(term) = term
        .as_ref()
        .filter(|_| scope.responsible || scope.project_view)
    {
        let (projection, tiers) = matter_projections::for_contingency_term(pg, term)
            .await
            .map_err(Box::new)?;
        let completed =
            time.has_completed_work && project.legal_plan_id.is_none() && !time.has_plan_claims;
        report
            .contingency
            .push(contingency(project, term, tiers, projection, completed));
    }
    record_fee_projection(report, pg, project, &scope, &time, term.as_ref()).await
}

pub struct ProjectScope {
    pub responsible: bool,
    pub project_view: bool,
    pub claims_firm: Option<Uuid>,
}

async fn collect_time_and_claims(
    report: &mut BillingReport,
    pg: &PostgresDb,
    project: &Project,
    attorney: Uuid,
    on: NaiveDate,
    scope: &ProjectScope,
) -> Result<MatterTime, BillingReportError> {
    let mut tx = pg.pool().begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let mut claim_entries = HashSet::new();
    let mut has_plan_claims = false;
    if let Some(firm) = scope.claims_firm {
        for (claim, entries) in plan_claims::billing_rows(&mut tx, project, firm).await? {
            has_plan_claims = true;
            claim_entries.extend(entries);
            if scope.responsible || scope.project_view {
                add_claim(report, claim, scope.responsible);
            }
        }
    }
    let timekeeper = (!scope.project_view).then_some(attorney);
    let entries = time_entries::for_billing(&mut tx, project.id, timekeeper, on).await?;
    let backfilled = time_proposals::backfilled_entries(&mut tx, project.id).await?;
    let work = if scope.project_view {
        report.project_time.get_or_insert_with(Vec::new)
    } else {
        &mut report.my_time
    };
    work.extend(entries.iter().map(|entry| BillingTime {
        project: project.code.clone(),
        id: entry.id,
        performed_on: entry.performed_on,
        minutes: entry.minutes,
        timekeeper: scope.project_view.then_some(entry.attorney_id),
        description: entry.description.clone(),
        status: entry.billing_status.as_str().to_owned(),
        backfilled: backfilled.contains(&entry.id),
    }));
    add_pending_time(
        report,
        &mut tx,
        project,
        timekeeper,
        attorney,
        scope.project_view,
    )
    .await?;
    if scope.responsible && scope.claims_firm.is_some() && project.legal_plan_id.is_some() {
        let allocated_entries: Vec<_> = claim_entries.iter().copied().collect();
        let unallocated: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM time_entry
             WHERE project_id=$1 AND performed_on<=$2 AND billable
               AND billing_status IN ('unbilled','reserved') AND id <> ALL($3::uuid[]))",
        )
        .bind(project.id)
        .bind(on)
        .bind(&allocated_entries)
        .fetch_one(&mut *tx)
        .await?;
        if unallocated {
            report.needs_my_action.push(BillingAction::UnallocatedWork {
                project: project.code.clone(),
                next_action: "Review completed time for a claim".to_owned(),
            });
        }
    }
    let has_completed_work = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM time_entry
         WHERE project_id=$1 AND performed_on<=$2 AND billing_status<>'written_off')",
    )
    .bind(project.id)
    .bind(on)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(MatterTime {
        entries,
        claim_entries,
        has_plan_claims,
        has_completed_work,
    })
}

/// Staged Jujutsu time and pending Time Entry Proposals: review actions for
/// the caller's own, and each pending block in the time list.
async fn add_pending_time(
    report: &mut BillingReport,
    connection: &mut sqlx::PgConnection,
    project: &Project,
    timekeeper: Option<Uuid>,
    attorney: Uuid,
    project_view: bool,
) -> Result<(), BillingReportError> {
    let mut pending = Vec::new();
    for (id, owner, change, tenths, performed_on) in
        billing_annotations::pending_for_billing(&mut *connection, project.id, timekeeper).await?
    {
        if owner == attorney {
            report
                .needs_my_action
                .push(BillingAction::PendingAnnotation {
                    project: project.code.clone(),
                    annotation_id: id,
                    change_id: change.clone(),
                    performed_on,
                    next_action: "Review your staged Jujutsu time: `navigator project time approve` or the Project's Billing tab"
                        .into(),
                });
        }
        pending.push(BillingTime {
            project: project.code.clone(),
            id,
            performed_on,
            minutes: tenths * 6,
            timekeeper: project_view.then_some(owner),
            description: change,
            status: "pending_review".to_owned(),
            backfilled: false,
        });
    }
    let proposals = time_proposals::pending_for_billing(&mut *connection, project.id, timekeeper)
        .await
        .map_err(|error| match error {
            time_proposals::ProposalError::Database(error) => BillingReportError::Database(error),
            other => BillingReportError::Database(sqlx::Error::Protocol(other.to_string())),
        })?;
    let mine = proposals
        .iter()
        .filter(|proposal| proposal.timekeeper_id == attorney)
        .count();
    if mine > 0 {
        report.needs_my_action.push(BillingAction::PendingProposals {
            project: project.code.clone(),
            pending_proposals: mine,
            next_action: format!(
                "Review Time Entry Proposals: `navigator project time list --project {} --status pending-review`",
                project.code
            ),
        });
    }
    let work = if project_view {
        report.project_time.get_or_insert_with(Vec::new)
    } else {
        &mut report.my_time
    };
    work.extend(pending);
    work.extend(proposals.into_iter().map(|proposal| BillingTime {
        project: project.code.clone(),
        id: proposal.id,
        performed_on: proposal.performed_on,
        minutes: proposal.minutes,
        timekeeper: project_view.then_some(proposal.timekeeper_id),
        description: proposal.narrative,
        status: "pending_review".to_owned(),
        backfilled: proposal.backfilled,
    }));
    Ok(())
}

fn add_claim(report: &mut BillingReport, claim: ClaimBilling, responsible: bool) {
    if claim.status != "denied" {
        match claim.projected_fee_cents {
            Some(cents) => {
                let remaining_verified = (claim.filed_cents.map_or(cents, i64::from).min(cents)
                    - claim.net_cash_received_cents)
                    .max(0);
                let remainder = claim
                    .approved_outstanding_cents
                    .map_or(remaining_verified, |outstanding| {
                        outstanding.min(remaining_verified)
                    });
                push_fee(
                    report,
                    &claim.project,
                    format!("Legal Plan Claim {}", claim.id),
                    remainder,
                );
            }
            None if claim.approved_outstanding_cents.is_none() => {
                push_unknown(
                    report,
                    &claim.project,
                    format!(
                        "Legal Plan Claim {} needs verified fee terms or completed time",
                        claim.id
                    ),
                );
            }
            None => {}
        }
    }
    if responsible && claim.next_action.is_some() {
        report.needs_my_action.push(BillingAction::Claim(claim));
    } else {
        report.other_claims.push(claim);
    }
}

fn push_fee(report: &mut BillingReport, code: &str, basis: String, cents: i64) {
    if cents > 0 {
        report.verified_projected_fees.push(FeeProjection {
            project: code.to_owned(),
            basis,
            projected_fee_cents: cents,
        });
    }
}

fn push_unknown(report: &mut BillingReport, code: &str, basis: String) {
    report.unverified_fees.push(UnverifiedFee {
        project: code.to_owned(),
        basis,
    });
}

fn contingency(
    project: &Project,
    term: &ContingencyTerm,
    tiers: Vec<ContingencyTier>,
    projection: ContingentProjection,
    completed: bool,
) -> ContingencyBilling {
    let actual = projection.source == Some(OddsSource::Actual);
    let verified = projection.source == Some(OddsSource::Lawyer) && !projection.stale && completed;
    ContingencyBilling {
        project: project.code.clone(),
        fee_agreement_notation_id: term.fee_agreement_notation_id,
        effective_on: term.effective_on,
        costs_deducted: term.costs_deducted.as_str().to_owned(),
        recovery_basis: match projection.source {
            Some(OddsSource::Lawyer) => "Outcome Estimate",
            Some(OddsSource::StageDefault) => "Case Stage default odds",
            Some(OddsSource::Actual) => "Matter Outcome",
            None => "unknown",
        }
        .to_owned(),
        tiers,
        hybrid_rate_cents_per_hour: term.rate_cents_per_hour,
        estimate_recorded_on: projection.estimate_recorded_at.map(|at| at.date_naive()),
        stale_estimate: projection.stale,
        projected_fee_cents: if verified {
            projection.expected_fee_cents
        } else {
            None
        },
        expected_cash_cents: if verified {
            projection.expected_cash_cents()
        } else {
            None
        },
        fee_received_cents: if actual {
            projection.expected_fee_cents
        } else {
            None
        },
        costs_recovered_cents: actual
            .then(|| {
                projection
                    .outcomes
                    .iter()
                    .find(|outcome| outcome.probability_bps > 0)
                    .and_then(|outcome| outcome.costs_recovered_cents)
            })
            .flatten(),
        outcomes: projection
            .outcomes
            .into_iter()
            .map(|outcome| ContingencyScenario {
                outcome: outcome.outcome.as_str().to_owned(),
                probability_bps: outcome.probability_bps,
                gross_recovery_cents: outcome.gross_recovery_cents,
                fee_bps: outcome.fee_bps,
                fee_cents: outcome.fee_cents,
            })
            .collect(),
    }
}

async fn record_fee_projection(
    report: &mut BillingReport,
    pg: &PostgresDb,
    project: &Project,
    scope: &ProjectScope,
    time: &MatterTime,
    term: Option<&ContingencyTerm>,
) -> Result<(), BillingReportError> {
    let eligible = time.entries.iter().filter(|entry| {
        entry.billable
            && matches!(
                entry.billing_status,
                BillingStatus::Unbilled | BillingStatus::Reserved
            )
            && !time.claim_entries.contains(&entry.id)
    });
    let code = &project.code;
    if project.legal_plan_id.is_some() || time.has_plan_claims {
        if eligible.count() > 0 {
            let basis = if scope.claims_firm.is_some() {
                "Unallocated billable time on a Project with Legal Plan work"
            } else {
                "Fee terms for completed billable time need verification"
            };
            push_unknown(report, code, basis.into());
        }
    } else if let Some(term) = term {
        let mut total = 0;
        let mut unverified = false;
        for entry in eligible {
            let entry_term =
                crate::contingency_terms::resolve_unchecked(pg, project.id, entry.performed_on)
                    .await?;
            match entry_term.and_then(|term| term.rate_cents_per_hour) {
                Some(rate) if rate == entry.rate_cents_per_hour => {
                    total += time_entries::entry_amount_cents(entry.minutes, rate);
                }
                _ => unverified = true,
            }
        }
        push_fee(
            report,
            code,
            "Time Entries at the verified hybrid hourly rate".into(),
            total,
        );
        if unverified && term.rate_cents_per_hour.is_some() {
            push_unknown(
                report,
                code,
                "Hybrid hourly rate for completed work needs verification".into(),
            );
        }
    } else if let Some(flat_fee) = attorney_rates::flat_fee_cents(pg, project.id).await? {
        let completed = matches!(project.status.as_str(), "closed" | "archived")
            && project.closure_reason.as_deref()
                == Some(model::projects::ClosureReason::EngagementCompleted.as_str());
        if (scope.responsible || scope.project_view) && (time.has_completed_work || completed) {
            let billed = billing_batches::flat_fee_billed_cents(pg, project.id).await?;
            let remainder = flat_fee.saturating_sub(billed);
            if completed {
                push_fee(
                    report,
                    code,
                    "Unbilled agreed flat fee for completed engagement".into(),
                    remainder,
                );
            } else if remainder > 0 {
                push_unknown(
                    report,
                    code,
                    "Agreed flat fee needs confirmation of completed scope".into(),
                );
            }
        }
    } else {
        let mut total = 0;
        let mut unverified = false;
        for entry in eligible {
            if entry.rate_cents_per_hour > 0 {
                total += time_entries::entry_amount_cents(entry.minutes, entry.rate_cents_per_hour);
            } else {
                unverified = true;
            }
        }
        push_fee(
            report,
            code,
            "Billable Time Entries at captured hourly rates".into(),
            total,
        );
        if unverified {
            push_unknown(
                report,
                code,
                "Billable Time Entries need a verified hourly rate".into(),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "billing_report_tests.rs"]
mod tests;

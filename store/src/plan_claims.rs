//! Private Legal Plan Claims and their lawyer-confirmed evidence.
use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, Utc};
use model::billing_report::{FilingReview, FilingReviewTotal};
use serde::{Deserialize, Serialize};
use sqlx::{types::Json, Postgres, Transaction};
use uuid::Uuid;

use crate::{
    plan_assignment::{provider_for_plan_name, PlanProvider},
    postgres::PostgresDb,
};

#[derive(Debug, thiserror::Error)]
pub enum ClaimError {
    #[error("{0}")]
    Invalid(&'static str),
    #[error("claim or plan case not found")]
    NotFound,
    #[error("claim key or provider reference already used")]
    Duplicate,
    #[error("claim storage failed")]
    Database(#[from] sqlx::Error),
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Service {
    AragAdviceOnly,
    AragRepresentation,
    LegalEaseLegalGuard,
    LegalEasePaymentAuthorization,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Authorization {
    FixedFee {
        reference: String,
        cents: i32,
    },
    CappedHours {
        reference: String,
        rate_cents_per_hour: i32,
        covered_minutes: i32,
        pending_minutes: i32,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Work {
    pub service: Service,
    pub benefit: String,
    pub performed_on: NaiveDate,
    pub attorney_minutes: Option<i32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub time_entry_ids: Vec<Uuid>,
    pub authorization: Option<Authorization>,
}

fn label(value: &str) -> bool {
    !value.is_empty()
        && value == value.trim()
        && value.len() <= 128
        && !value.chars().any(char::is_control)
}

impl Work {
    fn validate(&self, plan: &str, today: NaiveDate) -> Result<(), ClaimError> {
        let provider = match self.service {
            Service::AragAdviceOnly | Service::AragRepresentation => PlanProvider::Arag,
            Service::LegalEaseLegalGuard | Service::LegalEasePaymentAuthorization => {
                PlanProvider::LegalEase
            }
        };
        if provider_for_plan_name(plan) != Some(provider) {
            return Err(ClaimError::Invalid(
                "service path does not match the Project's Legal Plan",
            ));
        }
        if !label(&self.benefit)
            || self.performed_on > today
            || self.attorney_minutes.is_some_and(|minutes| minutes <= 0)
        {
            return Err(ClaimError::Invalid(
                "check benefit, service date, and actual attorney minutes",
            ));
        }
        self.estimate()?;
        Ok(())
    }

    fn estimate(&self) -> Result<Option<i64>, ClaimError> {
        match &self.authorization {
            None => Ok(None),
            Some(Authorization::FixedFee { reference, cents })
                if label(reference) && *cents > 0 =>
            {
                let time_confirmed =
                    self.service != Service::AragAdviceOnly || self.attorney_minutes.is_some();
                Ok(time_confirmed.then_some(i64::from(*cents)))
            }
            Some(Authorization::CappedHours {
                reference,
                rate_cents_per_hour,
                covered_minutes,
                pending_minutes,
            }) if label(reference)
                && *rate_cents_per_hour > 0
                && *covered_minutes > 0
                && *pending_minutes >= 0 =>
            {
                let Some(actual) = self.attorney_minutes else {
                    return Ok(None);
                };
                let eligible = actual.min(*covered_minutes);
                let cents = crate::time_entries::entry_amount_cents(eligible, *rate_cents_per_hour);
                Ok((cents > 0).then_some(cents))
            }
            _ => Err(ClaimError::Invalid(
                "invalid verified fee or authorization reference",
            )),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    WorkRecorded {
        work: Work,
    },
    Prepared,
    AuthorizationConfirmed {
        authorization: Authorization,
    },
    Submission {
        provider_reference: String,
        claimed_cents: Option<i32>,
    },
    Approval {
        reference: String,
        cents: i32,
    },
    Denial {
        reference: String,
    },
    Correction {
        provider_reference: String,
        claimed_cents: Option<i32>,
    },
    Payment {
        reference: String,
        cents: i32,
    },
    Reversal {
        reference: String,
        cents: i32,
    },
}

impl Event {
    fn reference(&self) -> Result<Option<(&'static str, String)>, ClaimError> {
        let (kind, value) = match self {
            Self::Submission {
                provider_reference, ..
            }
            | Self::Correction {
                provider_reference, ..
            } => ("claim", provider_reference),
            Self::Payment { reference, .. } | Self::Reversal { reference, .. } => {
                ("payment", reference)
            }
            Self::Denial { reference } | Self::Approval { reference, .. } if !label(reference) => {
                return Err(ClaimError::Invalid(
                    "enter a provider decision evidence reference",
                ))
            }
            _ => return Ok(None),
        };
        if !label(value) {
            return Err(ClaimError::Invalid(
                "enter a distinct provider or payment evidence reference",
            ));
        }
        Ok(Some((kind, value.to_ascii_lowercase())))
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Stage {
    Completed,
    Prepared,
    Submitted,
    Approved,
    Denied,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ClaimState {
    work: Work,
    stage: Stage,
    provider_reference: Option<String>,
    submitted_on: Option<NaiveDate>,
    claimed_cents: Option<i32>,
    approved_cents: Option<i32>,
    net_cents: i64,
}

impl ClaimState {
    fn new(work: Work) -> Self {
        Self {
            work,
            stage: Stage::Completed,
            provider_reference: None,
            submitted_on: None,
            claimed_cents: None,
            approved_cents: None,
            net_cents: 0,
        }
    }

    fn outstanding(&self) -> Option<i64> {
        self.approved_cents
            .map(|cents| (i64::from(cents) - self.net_cents).max(0))
    }

    fn apply(&mut self, on: NaiveDate, event: &Event) -> Result<(), ClaimError> {
        if on < self.work.performed_on || self.submitted_on.is_some_and(|submitted| on < submitted)
        {
            return Err(ClaimError::Invalid(
                "event date precedes service or submission",
            ));
        }
        match event {
            Event::WorkRecorded { work } if self.stage == Stage::Completed => {
                self.work = work.clone();
            }
            Event::AuthorizationConfirmed { authorization } => {
                self.work.authorization = Some(authorization.clone());
                self.work.estimate()?;
            }
            Event::Prepared
                if self.stage == Stage::Completed && self.work.estimate()?.is_some() =>
            {
                self.stage = Stage::Prepared;
            }
            Event::Submission {
                provider_reference,
                claimed_cents,
            } if self.stage == Stage::Prepared
                || (self.stage == Stage::Completed && claimed_cents.is_none()) =>
            {
                self.submit(on, provider_reference, *claimed_cents)?;
            }
            Event::Correction {
                provider_reference,
                claimed_cents,
            } if self.submitted_on.is_some() => {
                self.submit(on, provider_reference, *claimed_cents)?;
            }
            Event::Approval { cents, .. } if self.stage == Stage::Submitted && *cents > 0 => {
                self.stage = Stage::Approved;
                self.approved_cents = Some(*cents);
            }
            Event::Denial { .. } if matches!(self.stage, Stage::Submitted | Stage::Approved) => {
                self.stage = Stage::Denied;
                self.approved_cents = None;
            }
            Event::Payment { cents, .. } if self.submitted_on.is_some() && *cents > 0 => {
                self.net_cents = self
                    .net_cents
                    .checked_add(i64::from(*cents))
                    .ok_or(ClaimError::Invalid("payment total is too large"))?;
            }
            Event::Reversal { cents, .. } if *cents > 0 && self.net_cents >= i64::from(*cents) => {
                self.net_cents -= i64::from(*cents);
            }
            _ => {
                return Err(ClaimError::Invalid(
                    "claim stage, evidence, or amount does not permit this action",
                ))
            }
        }
        Ok(())
    }

    fn submit(
        &mut self,
        on: NaiveDate,
        reference: &str,
        cents: Option<i32>,
    ) -> Result<(), ClaimError> {
        if let Some(cents) = cents {
            let estimate = self.work.estimate()?;
            if cents <= 0 || estimate.is_none_or(|estimate| i64::from(cents) > estimate) {
                return Err(ClaimError::Invalid(
                    "claimed amount exceeds verified terms or is not positive",
                ));
            }
        }
        self.stage = Stage::Submitted;
        self.provider_reference = Some(reference.to_owned());
        self.submitted_on.get_or_insert(on);
        self.claimed_cents = cents;
        self.approved_cents = None;
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    pub claim_id: Uuid,
    pub event_id: Uuid,
    pub occurred_on: NaiveDate,
    pub event: Event,
}

#[derive(Debug, Serialize)]
struct Evidence {
    id: Uuid,
    claim_id: Uuid,
    occurred_on: NaiveDate,
    recorded_at: DateTime<Utc>,
    actor_id: Uuid,
    event: Json<Event>,
}

#[derive(Debug, Serialize)]
pub struct Claim {
    id: Uuid,
    project_id: Uuid,
    legal_plan_id: Uuid,
    plan_case_number: String,
    state: Json<ClaimState>,
    estimated_cents: Option<i64>,
    approved_outstanding_cents: Option<i64>,
    evidence: Vec<Evidence>,
}

type ClaimRow = (Uuid, Uuid, Uuid, String, Json<ClaimState>);
type EvidenceRow = (Uuid, Uuid, NaiveDate, DateTime<Utc>, Uuid, Json<Event>);
type WorklistRow = (Uuid, String, String, Json<ClaimState>, bool, i64);

impl From<ClaimRow> for Claim {
    fn from((id, project_id, legal_plan_id, plan_case_number, state): ClaimRow) -> Self {
        Self {
            id,
            project_id,
            legal_plan_id,
            plan_case_number,
            state,
            estimated_cents: None,
            approved_outstanding_cents: None,
            evidence: Vec::new(),
        }
    }
}

async fn read_evidence(
    tx: &mut Transaction<'_, Postgres>,
    claim: &mut Claim,
) -> Result<(), ClaimError> {
    claim.estimated_cents = claim.state.work.estimate()?;
    claim.approved_outstanding_cents = claim.state.outstanding();
    let rows: Vec<EvidenceRow> = sqlx::query_as(
        "SELECT id, claim_id, occurred_on, recorded_at, actor_id, event FROM legal_plan_claim_event
         WHERE claim_id = $1 ORDER BY recorded_at, id",
    )
    .bind(claim.id)
    .fetch_all(&mut **tx)
    .await?;
    claim.evidence = rows
        .into_iter()
        .map(
            |(id, claim_id, occurred_on, recorded_at, actor_id, event)| Evidence {
                id,
                claim_id,
                occurred_on,
                recorded_at,
                actor_id,
                event,
            },
        )
        .collect();
    Ok(())
}

pub async fn list(pg: &PostgresDb, project: Uuid, firm: Uuid) -> Result<Vec<Claim>, ClaimError> {
    let mut tx = pg.pool().begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let rows = claim_rows(&mut tx, project, firm).await?;
    let mut claims = rows
        .into_iter()
        .map(|(row, _)| Claim::from(row))
        .collect::<Vec<_>>();
    for claim in &mut claims {
        read_evidence(&mut tx, claim).await?;
    }
    tx.commit().await?;
    Ok(claims)
}

async fn claim_rows(
    connection: &mut sqlx::PgConnection,
    project: Uuid,
    firm: Uuid,
) -> Result<Vec<(ClaimRow, String)>, ClaimError> {
    use sqlx::{FromRow, Row};

    Ok(sqlx::query(
        "SELECT c.id, c.project_id, c.legal_plan_id, c.plan_case_number, c.state, l.name AS plan_name
         FROM legal_plan_claim c JOIN legal_plan l ON l.id = c.legal_plan_id
         JOIN project p ON p.id = c.project_id
         WHERE c.project_id = $1 AND l.firm_id = $2 AND p.firm_id = $2
         ORDER BY c.id",
    )
    .bind(project)
    .bind(firm)
    .try_map(|row| Ok((ClaimRow::from_row(&row)?, row.try_get("plan_name")?)))
    .fetch_all(connection)
    .await?)
}

/// Recorded claim amounts and the Time Entries already included in its actual minutes.
pub struct MatterClaim {
    pub plan_name: String,
    pub plan_case_number: String,
    pub attorney_minutes: Option<i32>,
    pub time_entry_ids: Vec<Uuid>,
    pub billing: model::billing_report::ClaimBilling,
}

/// Read claim snapshots for a Project after its participation and Firm capability checks.
pub async fn for_matter_billing(
    connection: &mut sqlx::PgConnection,
    project: &crate::projects::Project,
    firm: Uuid,
) -> Result<Vec<MatterClaim>, ClaimError> {
    claim_rows(connection, project.id, firm)
        .await?
        .into_iter()
        .map(|((id, _, _, plan_case_number, state), plan_name)| {
            Ok(MatterClaim {
                billing: state.billing(id, &project.code)?,
                plan_name,
                plan_case_number,
                attorney_minutes: state.work.attorney_minutes,
                time_entry_ids: state.0.work.time_entry_ids,
            })
        })
        .collect()
}

pub(crate) async fn billing_rows(
    connection: &mut sqlx::PgConnection,
    project: &crate::projects::Project,
    firm: Uuid,
) -> Result<Vec<(model::billing_report::ClaimBilling, Vec<Uuid>)>, ClaimError> {
    Ok(for_matter_billing(connection, project, firm)
        .await?
        .into_iter()
        .map(|row| (row.billing, row.time_entry_ids))
        .collect())
}

impl ClaimState {
    fn billing(
        &self,
        id: Uuid,
        project: &str,
    ) -> Result<model::billing_report::ClaimBilling, ClaimError> {
        let estimate = self.work.estimate()?;
        let expected = self
            .approved_cents
            .or(self.claimed_cents)
            .map(i64::from)
            .or(estimate);
        let follow_up = if self.stage == Stage::Denied {
            Some("Review denial")
        } else if expected.is_some_and(|cents| self.net_cents >= cents) {
            None
        } else if estimate.is_none() && !matches!(self.stage, Stage::Approved) {
            Some("Verify fee terms and completed work")
        } else {
            Some(match self.stage {
                Stage::Completed => "Prepare claim",
                Stage::Prepared => "File claim and record submission",
                Stage::Submitted => "Check provider decision or payment",
                Stage::Approved => "Check payment",
                Stage::Denied => "Review denial",
            })
        };
        let status = match self.stage {
            Stage::Completed => "completed",
            Stage::Prepared => "prepared",
            Stage::Submitted => "submitted",
            Stage::Approved => "approved",
            Stage::Denied => "denied",
        };
        Ok(model::billing_report::ClaimBilling {
            project: project.to_owned(),
            id,
            benefit: self.work.benefit.clone(),
            performed_on: self.work.performed_on,
            status: status.to_owned(),
            next_action: follow_up.map(str::to_owned),
            projected_fee_cents: estimate,
            filed_cents: self.claimed_cents,
            approved_outstanding_cents: self.outstanding(),
            net_cash_received_cents: self.net_cents,
        })
    }
}

#[allow(clippy::too_many_lines)] // Transaction keeps claim snapshot, evidence, and retry checks together.
pub async fn record(
    pg: &PostgresDb,
    project: Uuid,
    firm: Uuid,
    actor: Uuid,
    change: Change,
) -> Result<Claim, ClaimError> {
    let today = Utc::now().date_naive();
    if change.occurred_on > today {
        return Err(ClaimError::Invalid("event date is in the future"));
    }
    let mut reference = change.event.reference()?;
    let mut tx = pg.pool().begin().await?;
    // Serialize creation and changes against this Project and its plan link.
    let link: (Option<Uuid>, Option<String>) = sqlx::query_as(
        "SELECT legal_plan_id, plan_case_number FROM project WHERE id = $1 AND firm_id = $2 FOR UPDATE",
    ).bind(project).bind(firm).fetch_optional(&mut *tx).await?.ok_or(ClaimError::NotFound)?;
    let existing: Option<ClaimRow> = sqlx::query_as(
        "SELECT c.id, c.project_id, c.legal_plan_id, c.plan_case_number, c.state
         FROM legal_plan_claim c JOIN legal_plan l ON l.id = c.legal_plan_id
         WHERE c.id = $1 AND c.project_id = $2 AND l.firm_id = $3",
    )
    .bind(change.claim_id)
    .bind(project)
    .bind(firm)
    .fetch_optional(&mut *tx)
    .await?;
    let mut claim = if let Some(row) = existing {
        let mut claim = Claim::from(row);
        read_evidence(&mut tx, &mut claim).await?;
        if let Some(event) = claim
            .evidence
            .iter()
            .find(|event| event.id == change.event_id)
        {
            if event.occurred_on == change.occurred_on && event.event.0 == change.event {
                return Ok(claim);
            }
            return Err(ClaimError::Duplicate);
        }
        let applied_event = if let Event::WorkRecorded { work } = &change.event {
            let plan = plan_name(&mut tx, firm, claim.legal_plan_id).await?;
            let work = verified_work(&mut tx, project, claim.id, work, &plan, today).await?;
            Event::WorkRecorded { work }
        } else {
            change.event.clone()
        };
        if matches!(&applied_event, Event::WorkRecorded { work } if change.occurred_on < work.performed_on)
        {
            return Err(ClaimError::Invalid("work record predates service"));
        }
        if reference.as_ref().is_some_and(|(kind, value)| {
            *kind == "claim" && claim.plan_case_number.eq_ignore_ascii_case(value)
        }) {
            return Err(ClaimError::Invalid(
                "a claim reference must identify the filing, not the plan case",
            ));
        }
        if matches!(change.event, Event::Correction { .. })
            && reference.is_some()
            && claim
                .evidence
                .iter()
                .any(|item| item.event.reference().ok().flatten() == reference)
        {
            // The reference is already exclusively reserved to this claim.
            reference = None;
        }
        claim.state.apply(change.occurred_on, &applied_event)?;
        sqlx::query("UPDATE legal_plan_claim SET state = $1 WHERE id = $2")
            .bind(&claim.state)
            .bind(claim.id)
            .execute(&mut *tx)
            .await?;
        claim
    } else {
        let Event::WorkRecorded { work } = &change.event else {
            return Err(ClaimError::NotFound);
        };
        let (Some(plan_id), Some(case_number)) = link else {
            return Err(ClaimError::NotFound);
        };
        let plan = plan_name(&mut tx, firm, plan_id).await?;
        let work = verified_work(&mut tx, project, change.claim_id, work, &plan, today).await?;
        if change.occurred_on < work.performed_on {
            return Err(ClaimError::Invalid("work record predates service"));
        }
        let state = Json(ClaimState::new(work));
        sqlx::query(
            "INSERT INTO legal_plan_claim (id, project_id, legal_plan_id, plan_case_number, state)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(change.claim_id)
        .bind(project)
        .bind(plan_id)
        .bind(&case_number)
        .bind(&state)
        .execute(&mut *tx)
        .await
        .map_err(write_error)?;
        Claim::from((change.claim_id, project, plan_id, case_number, state))
    };
    let (kind, reference) = reference.unzip();
    sqlx::query(
        "INSERT INTO legal_plan_claim_event
         (id, claim_id, legal_plan_id, actor_id, occurred_on, event, reference_kind, reference)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(change.event_id)
    .bind(claim.id)
    .bind(claim.legal_plan_id)
    .bind(actor)
    .bind(change.occurred_on)
    .bind(Json(change.event))
    .bind(kind)
    .bind(reference)
    .execute(&mut *tx)
    .await
    .map_err(write_error)?;
    read_evidence(&mut tx, &mut claim).await?;
    tx.commit().await?;
    Ok(claim)
}

async fn verified_work(
    tx: &mut Transaction<'_, Postgres>,
    project: Uuid,
    claim: Uuid,
    work: &Work,
    plan: &str,
    today: NaiveDate,
) -> Result<Work, ClaimError> {
    let mut work = work.clone();
    if !work.time_entry_ids.is_empty() {
        if work.attorney_minutes.is_some() || work.time_entry_ids.len() > 1_000 {
            return Err(ClaimError::Invalid(
                "supply Time Entry IDs or attested minutes",
            ));
        }
        let (entries, minutes): (i64, Option<i64>) = sqlx::query_as(
            "SELECT count(*), sum(t.minutes) FROM time_entry t
             WHERE t.id = ANY($1) AND t.project_id = $2 AND t.performed_on <= $3
             AND NOT EXISTS (
                 SELECT 1 FROM legal_plan_claim c WHERE c.project_id = $2 AND c.id <> $4
                 AND (c.state->'work'->'time_entry_ids') ? t.id::text
             )",
        )
        .bind(&work.time_entry_ids)
        .bind(project)
        .bind(work.performed_on)
        .bind(claim)
        .fetch_one(&mut **tx)
        .await?;
        if usize::try_from(entries).ok() != Some(work.time_entry_ids.len()) {
            return Err(ClaimError::Invalid(
                "Time Entries must be distinct, unused, and on this Project",
            ));
        }
        work.attorney_minutes = minutes.and_then(|minutes| i32::try_from(minutes).ok());
    }
    work.validate(plan, today)?;
    Ok(work)
}

fn write_error(error: sqlx::Error) -> ClaimError {
    if error
        .as_database_error()
        .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
    {
        ClaimError::Duplicate
    } else {
        error.into()
    }
}

async fn plan_name(
    tx: &mut Transaction<'_, Postgres>,
    firm: Uuid,
    id: Uuid,
) -> Result<String, ClaimError> {
    sqlx::query_scalar("SELECT name FROM legal_plan WHERE id = $1 AND firm_id = $2")
        .bind(id)
        .bind(firm)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(ClaimError::NotFound)
}

#[derive(Debug)]
struct ReviewRow {
    id: Uuid,
    code: String,
    plan: String,
    state: Json<ClaimState>,
    recorded_this_week: bool,
    net_collected_this_week_cents: i64,
}

#[derive(Debug, Serialize)]
struct FollowUp {
    claim_id: Uuid,
    project: String,
    status: Stage,
    service_date: NaiveDate,
    days_since_submission: Option<i64>,
    provider_reference: Option<String>,
    net_collected_cents: i64,
    estimated_cents: Option<i64>,
    claimed_cents: Option<i32>,
    approved_outstanding_cents: Option<i64>,
    pending_minutes: Option<i32>,
}

#[derive(Debug, Default, Serialize)]
struct PlanTotals {
    work_count: usize,
    touched_this_week: usize,
    attorney_minutes: i64,
    claims_missing_attorney_time: usize,
    net_collected_cents: i64,
    net_collected_this_week_cents: i64,
    net_collected_without_attorney_time_cents: i64,
    net_collected_per_attorney_hour_cents: Option<i64>,
    #[serde(flatten)]
    filing_review: FilingReviewTotal,
    needs_verification_count: usize,
    submitted_unpaid_count: usize,
    denied_count: usize,
    approved_outstanding_cents: i64,
    pending_attorney_minutes: i64,
    items: Vec<FollowUp>,
    closed_projects_without_claim_count: usize,
    closed_projects_without_claim: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Worklist {
    as_of: NaiveDate,
    plans: BTreeMap<String, PlanTotals>,
    filing_review: FilingReview,
}

pub async fn worklist(
    pg: &PostgresDb,
    firm: Uuid,
    visible_projects: Option<&[Uuid]>,
) -> Result<Worklist, ClaimError> {
    let mut tx = pg.pool().begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let rows: Vec<WorklistRow> = sqlx::query_as(
        "SELECT c.id, p.code, l.name AS plan, c.state,
         EXISTS (SELECT 1 FROM legal_plan_claim_event e WHERE e.claim_id = c.id
         AND e.recorded_at >= now() - interval '7 days') AS recorded_this_week,
         COALESCE((SELECT sum(CASE e.event->>'type'
             WHEN 'payment' THEN (e.event->>'cents')::integer
             WHEN 'reversal' THEN -(e.event->>'cents')::integer END)
         FROM legal_plan_claim_event e WHERE e.claim_id = c.id
         AND e.occurred_on > CURRENT_DATE - 7), 0) AS net_collected_this_week_cents
         FROM legal_plan_claim c JOIN project p ON p.id = c.project_id
         JOIN legal_plan l ON l.id = c.legal_plan_id
         WHERE p.firm_id = $1 AND l.firm_id = $1 AND ($2::uuid[] IS NULL OR p.id = ANY($2))
         ORDER BY (c.state->>'submitted_on') NULLS FIRST, c.state->'work'->>'performed_on', c.id",
    )
    .bind(firm)
    .bind(visible_projects)
    .fetch_all(&mut *tx)
    .await?;
    let closed: Vec<(String, String)> = sqlx::query_as(
        "SELECT l.name, p.code FROM project p JOIN legal_plan l ON l.id = p.legal_plan_id
         WHERE p.firm_id = $1 AND l.firm_id = $1 AND ($2::uuid[] IS NULL OR p.id = ANY($2))
         AND (p.status = 'closed' OR p.plan_closed_on IS NOT NULL)
         AND NOT EXISTS (SELECT 1 FROM legal_plan_claim c WHERE c.project_id = p.id
             AND c.legal_plan_id = l.id AND c.plan_case_number = p.plan_case_number)
         ORDER BY l.name, p.code",
    )
    .bind(firm)
    .bind(visible_projects)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let rows = rows
        .into_iter()
        .map(
            |(id, code, plan, state, recorded_this_week, net_collected_this_week_cents)| {
                ReviewRow {
                    id,
                    code,
                    plan,
                    state,
                    recorded_this_week,
                    net_collected_this_week_cents,
                }
            },
        )
        .collect();
    let mut report = summarize(Utc::now().date_naive(), rows, visible_projects.is_some())?;
    for (plan, code) in closed {
        if !matches!(
            provider_for_plan_name(&plan),
            Some(PlanProvider::Arag | PlanProvider::LegalEase)
        ) {
            continue;
        }
        let totals = report.plans.entry(plan).or_default();
        totals.closed_projects_without_claim_count += 1;
        if visible_projects.is_some() {
            totals.closed_projects_without_claim.push(code);
        }
    }
    Ok(report)
}

fn summarize(
    today: NaiveDate,
    rows: Vec<ReviewRow>,
    show_projects: bool,
) -> Result<Worklist, ClaimError> {
    let mut plans = BTreeMap::<String, PlanTotals>::new();
    let mut filing_review = FilingReview::default();
    for row in rows {
        let totals = plans.entry(row.plan).or_default();
        let state = row.state.0;
        totals.work_count += 1;
        totals.touched_this_week += usize::from(row.recorded_this_week);
        totals.net_collected_cents += state.net_cents;
        totals.net_collected_this_week_cents += row.net_collected_this_week_cents;
        if let Some(minutes) = state.work.attorney_minutes {
            totals.attorney_minutes += i64::from(minutes);
        } else {
            totals.claims_missing_attorney_time += 1;
            totals.net_collected_without_attorney_time_cents += state.net_cents;
        }
        let estimate = state.work.estimate()?;
        let outstanding = state.outstanding();
        let pending_minutes = match state.work.authorization {
            Some(Authorization::CappedHours {
                pending_minutes, ..
            }) => pending_minutes,
            _ => 0,
        };
        totals.pending_attorney_minutes += i64::from(pending_minutes);
        match state.stage {
            Stage::Completed | Stage::Prepared => {
                if let Some(cents) = estimate {
                    let carrier = match state.work.service {
                        Service::AragAdviceOnly | Service::AragRepresentation => {
                            &mut filing_review.arag
                        }
                        Service::LegalEaseLegalGuard | Service::LegalEasePaymentAuthorization => {
                            &mut filing_review.legal_ease
                        }
                    };
                    for subtotal in [&mut totals.filing_review, carrier] {
                        subtotal.eligible_unsubmitted_count += 1;
                        subtotal.eligible_unsubmitted_cents = subtotal
                            .eligible_unsubmitted_cents
                            .checked_add(cents)
                            .ok_or(ClaimError::Invalid("filing review total is too large"))?;
                    }
                } else {
                    totals.needs_verification_count += 1;
                }
            }
            Stage::Denied => totals.denied_count += 1,
            Stage::Submitted
                if state.net_cents == 0
                    || state
                        .claimed_cents
                        .is_none_or(|claimed| state.net_cents < i64::from(claimed)) =>
            {
                totals.submitted_unpaid_count += 1;
            }
            Stage::Approved => totals.approved_outstanding_cents += outstanding.unwrap_or_default(),
            Stage::Submitted => {}
        }
        if show_projects && (outstanding != Some(0) || pending_minutes > 0) {
            totals.items.push(FollowUp {
                claim_id: row.id,
                project: row.code,
                status: state.stage,
                service_date: state.work.performed_on,
                days_since_submission: state.submitted_on.map(|date| (today - date).num_days()),
                provider_reference: state.provider_reference,
                net_collected_cents: state.net_cents,
                estimated_cents: estimate,
                claimed_cents: state.claimed_cents,
                approved_outstanding_cents: outstanding,
                pending_minutes: (pending_minutes > 0).then_some(pending_minutes),
            });
        }
    }
    for totals in plans.values_mut() {
        if totals.attorney_minutes > 0 {
            let known_cash =
                totals.net_collected_cents - totals.net_collected_without_attorney_time_cents;
            let numerator = known_cash
                .checked_mul(60)
                .ok_or(ClaimError::Invalid("collection total is too large"))?;
            totals.net_collected_per_attorney_hour_cents = Some(
                crate::plan_metrics::round_half_away_from_zero(numerator, totals.attorney_minutes),
            );
        }
    }
    Ok(Worklist {
        as_of: today,
        plans,
        filing_review,
    })
}

#[cfg(test)]
mod tests;

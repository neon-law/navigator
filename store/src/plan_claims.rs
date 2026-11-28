//! Private Legal Plan Claims and their lawyer-confirmed evidence.
use std::collections::BTreeMap;

use chrono::{DateTime, Days, NaiveDate, Utc};
use model::billing_report::{FilingReview, FilingReviewTotal};
use model::plan_filing::{FilingRule, FilingTrigger};
use serde::{Deserialize, Serialize};
use sqlx::{types::Json, Postgres, Transaction};
use uuid::Uuid;

use crate::{
    legal_plans::filing_rule,
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
    #[error("claim filing Deadline could not be recorded")]
    Deadline(#[from] crate::project_deadlines::DeadlineError),
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Service {
    AragAdviceOnly,
    AragRepresentation,
    LegalEaseLegalGuard,
    LegalEasePaymentAuthorization,
    Manual,
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
    fn validate(&self, plan: &str, today: NaiveDate, configured: bool) -> Result<(), ClaimError> {
        let provider = provider_for_plan_name(plan);
        let valid_service = match self.service {
            Service::AragAdviceOnly | Service::AragRepresentation => {
                provider == Some(PlanProvider::Arag)
            }
            Service::LegalEaseLegalGuard | Service::LegalEasePaymentAuthorization => {
                provider == Some(PlanProvider::LegalEase)
            }
            Service::Manual => {
                configured
                    && !matches!(provider, Some(PlanProvider::Arag | PlanProvider::LegalEase))
            }
        };
        if !valid_service {
            return Err(ClaimError::Invalid(
                "service path does not match the Project's Legal Plan or benefit configuration",
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

    fn filing(
        &self,
        rule: Option<&FilingRule>,
        closed_on: Option<NaiveDate>,
        as_of: NaiveDate,
    ) -> Result<Filing, ClaimError> {
        let Some(rule) = rule else {
            let status = if self.service == Service::Manual || self.estimate()?.is_none() {
                FilingStatus::VerifyTerms
            } else {
                FilingStatus::Ready
            };
            return Ok(Filing {
                status,
                due_on: None,
            });
        };
        let hours_exhausted = matches!(
            self.authorization,
            Some(Authorization::CappedHours { covered_minutes, .. })
                if self.attorney_minutes.is_some_and(|actual| actual >= covered_minutes)
        );
        let completed_on = Some(self.performed_on);
        let hours_on = completed_on.filter(|_| hours_exhausted);
        let closed_on = closed_on.map(|date| date.max(self.performed_on));
        let trigger_on = match rule.trigger {
            FilingTrigger::ServiceCompleted => completed_on,
            FilingTrigger::HoursExhausted => hours_on,
            FilingTrigger::MatterClosed => closed_on,
            FilingTrigger::HoursExhaustedOrMatterClosed => hours_on.or(closed_on),
        }
        .filter(|date| *date <= as_of);
        let due_on = match (trigger_on, rule.filing_window_days) {
            (Some(start), Some(days)) => {
                Some(start.checked_add_days(Days::new(u64::from(days))).ok_or(
                    ClaimError::Invalid("claim filing date is outside the supported range"),
                )?)
            }
            _ => None,
        };
        let status = if due_on.is_some_and(|date| date < as_of) {
            FilingStatus::Expired
        } else if self.estimate()?.is_none() {
            FilingStatus::VerifyTerms
        } else if trigger_on.is_none() {
            FilingStatus::Accruing
        } else {
            FilingStatus::Ready
        };
        Ok(Filing { status, due_on })
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum FilingStatus {
    VerifyTerms,
    Accruing,
    Ready,
    Expired,
}

struct Filing {
    status: FilingStatus,
    due_on: Option<NaiveDate>,
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
    /// The lawyer filed the claim. A claim filed by email may carry only the
    /// sent message's id; its provider reference is recorded later.
    Submission {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_reference: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sent_message_id: Option<String>,
        claimed_cents: Option<i32>,
    },
    /// The provider's claim reference for a claim filed without one.
    ProviderReference {
        provider_reference: String,
    },
    /// The claim packet filed on the Project for this claim. Recorded by the
    /// packet command, never from the wire.
    PacketFiled {
        asset_id: Uuid,
        sha256: String,
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
        let message;
        let (kind, value) = match self {
            Self::Submission {
                provider_reference: None,
                sent_message_id: None,
                ..
            } => {
                return Err(ClaimError::Invalid(
                    "enter the provider reference or the sent message id",
                ))
            }
            Self::Submission {
                provider_reference: Some(provider_reference),
                ..
            }
            | Self::ProviderReference { provider_reference }
            | Self::Correction {
                provider_reference, ..
            } => ("claim", provider_reference),
            Self::Submission {
                sent_message_id: Some(sent),
                ..
            } => {
                // The message is the filing's evidence until the provider
                // answers with a reference; it may evidence one claim only.
                message = format!("message:{}", sent.trim());
                if !label(sent) {
                    return Err(ClaimError::Invalid("enter the sent message id as sent"));
                }
                ("claim", &message)
            }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sent_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    packet_asset_id: Option<Uuid>,
    submitted_on: Option<NaiveDate>,
    claimed_cents: Option<i32>,
    approved_cents: Option<i32>,
    net_cents: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    filing_rule: Option<FilingRule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    filing_due_on: Option<NaiveDate>,
}

impl ClaimState {
    fn new(work: Work) -> Self {
        Self {
            work,
            stage: Stage::Completed,
            provider_reference: None,
            sent_message_id: None,
            packet_asset_id: None,
            submitted_on: None,
            claimed_cents: None,
            approved_cents: None,
            net_cents: 0,
            filing_rule: None,
            filing_due_on: None,
        }
    }

    fn filing(
        &self,
        current_rule: Option<&FilingRule>,
        closed_on: Option<NaiveDate>,
        as_of: NaiveDate,
    ) -> Result<Filing, ClaimError> {
        if let Some(due_on) = self.filing_due_on {
            let status = if due_on < as_of {
                FilingStatus::Expired
            } else if self.work.estimate()?.is_none() {
                FilingStatus::VerifyTerms
            } else {
                FilingStatus::Ready
            };
            return Ok(Filing {
                status,
                due_on: Some(due_on),
            });
        }
        self.work
            .filing(self.filing_rule.as_ref().or(current_rule), closed_on, as_of)
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
                sent_message_id,
                claimed_cents,
            } if self.stage == Stage::Prepared
                || (self.stage == Stage::Completed && claimed_cents.is_none()) =>
            {
                self.submit(on, provider_reference.as_deref(), *claimed_cents)?;
                self.sent_message_id.clone_from(sent_message_id);
            }
            Event::ProviderReference { provider_reference }
                if self.submitted_on.is_some() && self.provider_reference.is_none() =>
            {
                self.provider_reference = Some(provider_reference.clone());
            }
            Event::PacketFiled { asset_id, .. }
                if matches!(
                    self.stage,
                    Stage::Completed | Stage::Prepared | Stage::Submitted
                ) =>
            {
                self.packet_asset_id = Some(*asset_id);
            }
            Event::Correction {
                provider_reference,
                claimed_cents,
            } if self.submitted_on.is_some() => {
                self.submit(on, Some(provider_reference), *claimed_cents)?;
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
        reference: Option<&str>,
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
        self.provider_reference = reference.map(str::to_owned);
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
type WorklistRow = (
    Uuid,
    String,
    String,
    Json<ClaimState>,
    bool,
    i64,
    Option<NaiveDate>,
    Option<Json<FilingRule>>,
);

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
    pub covered_minutes: Option<i32>,
    /// The verified hourly rate for approval, with zero for an authorized fixed fee.
    pub annotation_rate: Option<i32>,
    pub billing: model::billing_report::ClaimBilling,
}

/// Read claim snapshots for a Project after its participation and Firm capability checks.
pub async fn for_matter_billing(
    connection: &mut sqlx::PgConnection,
    project: &crate::projects::Project,
    firm: Uuid,
) -> Result<Vec<MatterClaim>, ClaimError> {
    let mut claims = Vec::new();
    let today = Utc::now().date_naive();
    for ((id, _, plan_id, plan_case_number, state), plan_name) in
        claim_rows(connection, project.id, firm).await?
    {
        let current_case = project.legal_plan_id == Some(plan_id)
            && project.plan_case_number.as_deref() == Some(plan_case_number.as_str());
        let mut billing = state.billing(id, &project.code)?;
        if matches!(state.stage, Stage::Completed | Stage::Prepared) {
            let work = &state.work;
            let rule = filing_rule(connection, plan_id, &work.benefit, work.performed_on).await?;
            if rule.is_some() || state.filing_rule.is_some() {
                let closed_on = project
                    .plan_closed_on
                    .as_deref()
                    .filter(|_| current_case)
                    .and_then(|date| date.parse().ok());
                let filing = state.filing(rule.as_ref(), closed_on, today)?;
                billing.next_action = match filing.status {
                    FilingStatus::Accruing => {
                        Some("Wait for the configured filing milestone".to_owned())
                    }
                    FilingStatus::Expired => Some("Review the expired filing window".to_owned()),
                    _ => billing.next_action,
                };
            }
        }
        claims.push(MatterClaim {
            billing,
            plan_name,
            plan_case_number,
            attorney_minutes: state.work.attorney_minutes,
            covered_minutes: match state.work.authorization.as_ref() {
                Some(Authorization::CappedHours {
                    covered_minutes, ..
                }) => Some(*covered_minutes),
                _ => None,
            },
            annotation_rate: current_case.then(|| state.annotation_rate()).flatten(),
            time_entry_ids: state.0.work.time_entry_ids,
        });
    }
    Ok(claims)
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
    fn annotation_rate(&self) -> Option<i32> {
        if self.stage != Stage::Completed || !self.work.time_entry_ids.is_empty() {
            return None;
        }
        match self.work.authorization.as_ref()? {
            Authorization::FixedFee { .. } => Some(0),
            Authorization::CappedHours {
                rate_cents_per_hour,
                ..
            } => Some(*rate_cents_per_hour),
        }
    }

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

/// Lock the current plan case before loading a claim for explicit approval.
pub(crate) async fn for_annotation(
    tx: &mut Transaction<'_, Postgres>,
    project: Uuid,
    claim_id: Uuid,
    date: NaiveDate,
    minutes: i32,
) -> Result<(Claim, i32), crate::time_entries::TimeEntryError> {
    use crate::time_entries::TimeEntryError;
    sqlx::query("SELECT id FROM project WHERE id=$1 FOR UPDATE")
        .bind(project)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(TimeEntryError::NotFound)?;
    let row: ClaimRow = sqlx::query_as(
        "SELECT c.id, c.project_id, c.legal_plan_id, c.plan_case_number, c.state
         FROM legal_plan_claim c JOIN project p ON p.id=c.project_id
         JOIN legal_plan l ON l.id=c.legal_plan_id AND l.firm_id=p.firm_id
         WHERE c.id=$1 AND p.id=$2 AND c.legal_plan_id=p.legal_plan_id
           AND c.plan_case_number=p.plan_case_number",
    )
    .bind(claim_id)
    .bind(project)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(TimeEntryError::NotFound)?;
    let claim = Claim::from(row);
    let rate = claim
        .state
        .annotation_rate()
        .filter(|_| {
            claim.state.work.performed_on == date
                && claim
                    .state
                    .work
                    .attorney_minutes
                    .is_none_or(|attested| attested == minutes)
        })
        .ok_or(TimeEntryError::Invalid("claim does not match this time"))?;
    Ok((claim, rate))
}

/// Replace attested claim minutes with their Time Entry evidence in the same transaction.
pub(crate) async fn link_annotation(
    tx: &mut Transaction<'_, Postgres>,
    mut claim: Claim,
    actor: Uuid,
    entry: Uuid,
    minutes: i32,
) -> Result<(), sqlx::Error> {
    let work = &mut claim.state.0.work;
    work.time_entry_ids.push(entry);
    work.attorney_minutes = Some(minutes);
    let mut evidence = work.clone();
    evidence.attorney_minutes = None;
    sqlx::query("UPDATE legal_plan_claim SET state=$1 WHERE id=$2")
        .bind(&claim.state)
        .bind(claim.id)
        .execute(&mut **tx)
        .await?;
    sqlx::query(
        "INSERT INTO legal_plan_claim_event (id,claim_id,legal_plan_id,actor_id,occurred_on,event)
         VALUES ($1,$2,$3,$4,$5,$6)",
    )
    .bind(Uuid::new_v4())
    .bind(claim.id)
    .bind(claim.legal_plan_id)
    .bind(actor)
    .bind(Utc::now().date_naive())
    .bind(Json(Event::WorkRecorded { work: evidence }))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Record one lawyer-confirmed claim event. A retry with the same event id
/// and payload returns the claim unchanged.
///
/// # Errors
/// [`ClaimError`] for an invalid transition, a reused reference, or a
/// database failure.
pub async fn record(
    pg: &PostgresDb,
    project: Uuid,
    firm: Uuid,
    actor: Uuid,
    timezone: chrono_tz::Tz,
    change: Change,
) -> Result<Claim, ClaimError> {
    if matches!(change.event, Event::PacketFiled { .. }) {
        return Err(ClaimError::Invalid(
            "file a claim packet with `navigator project billing packet`",
        ));
    }
    record_change(pg, project, firm, actor, timezone, change).await
}

#[allow(clippy::too_many_lines)] // Transaction keeps claim snapshot, evidence, and retry checks together.
async fn record_change(
    pg: &PostgresDb,
    project: Uuid,
    firm: Uuid,
    actor: Uuid,
    timezone: chrono_tz::Tz,
    change: Change,
) -> Result<Claim, ClaimError> {
    let today = Utc::now().date_naive();
    if change.occurred_on > today {
        return Err(ClaimError::Invalid("event date is in the future"));
    }
    let mut reference = change.event.reference()?;
    let mut tx = pg.pool().begin().await?;
    // Serialize creation and changes against this Project and its plan link.
    let link: (Option<Uuid>, Option<String>, Option<NaiveDate>) = sqlx::query_as(
        "SELECT legal_plan_id, plan_case_number, plan_closed_on FROM project WHERE id = $1 AND firm_id = $2 FOR UPDATE",
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
    let creating = existing.is_none();
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
            let changed_identity = work.benefit != claim.state.work.benefit
                || work.performed_on != claim.state.work.performed_on;
            if changed_identity && claim.state.filing_due_on.is_some() {
                return Err(ClaimError::Invalid(
                    "the docketed claim's benefit and service date are fixed",
                ));
            }
            if changed_identity || claim.state.filing_rule.is_none() {
                claim.state.filing_rule = filing_rule(
                    &mut tx,
                    claim.legal_plan_id,
                    &work.benefit,
                    work.performed_on,
                )
                .await?;
            }
            let work = verified_work(
                &mut tx,
                project,
                claim.id,
                work,
                &plan,
                today,
                claim.state.filing_rule.is_some(),
            )
            .await?;
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
        if matches!(applied_event, Event::Prepared) {
            adopt_filing_rule(&mut tx, &mut claim).await?;
            let closed_on = plan_case_closed(&link, &claim);
            let filing = claim.state.filing(None, closed_on, change.occurred_on)?;
            match filing.status {
                FilingStatus::Ready => {}
                FilingStatus::VerifyTerms => {
                    return Err(ClaimError::Invalid(
                        "verify the claim's fee and filing terms before preparing",
                    ))
                }
                FilingStatus::Accruing => {
                    return Err(ClaimError::Invalid(
                        "the configured claim filing milestone has not occurred",
                    ))
                }
                FilingStatus::Expired => {
                    return Err(ClaimError::Invalid(
                        "the configured claim filing window has elapsed",
                    ))
                }
            }
        }
        claim.state.apply(change.occurred_on, &applied_event)?;
        claim
    } else {
        let Event::WorkRecorded { work } = &change.event else {
            return Err(ClaimError::NotFound);
        };
        let (Some(plan_id), Some(ref case_number), _) = link else {
            return Err(ClaimError::NotFound);
        };
        let plan = plan_name(&mut tx, firm, plan_id).await?;
        let rule = filing_rule(&mut tx, plan_id, &work.benefit, work.performed_on).await?;
        let work = verified_work(
            &mut tx,
            project,
            change.claim_id,
            work,
            &plan,
            today,
            rule.is_some(),
        )
        .await?;
        if change.occurred_on < work.performed_on {
            return Err(ClaimError::Invalid("work record predates service"));
        }
        let mut state = ClaimState::new(work);
        state.filing_rule = rule;
        Claim::from((
            change.claim_id,
            project,
            plan_id,
            case_number.clone(),
            Json(state),
        ))
    };
    let filed = matches!(&change.event, Event::Submission { .. });
    if filed || matches!(claim.state.stage, Stage::Completed | Stage::Prepared) {
        adopt_filing_rule(&mut tx, &mut claim).await?;
        if claim.state.filing_rule.is_some() {
            let filing =
                claim
                    .state
                    .filing(None, plan_case_closed(&link, &claim), change.occurred_on)?;
            claim.state.filing_due_on = filing.due_on;
            let due_on = filing.due_on.map(|date| crate::project_deadlines::DueOn {
                date,
                timezone,
                cutoff: None,
            });
            crate::project_deadlines::record_claim_filing(
                &mut tx, project, claim.id, due_on, filed,
            )
            .await?;
        }
    }
    if creating {
        sqlx::query(
            "INSERT INTO legal_plan_claim (id, project_id, legal_plan_id, plan_case_number, state)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(claim.id)
        .bind(project)
        .bind(claim.legal_plan_id)
        .bind(&claim.plan_case_number)
        .bind(&claim.state)
        .execute(&mut *tx)
        .await
        .map_err(write_error)?;
    } else {
        sqlx::query("UPDATE legal_plan_claim SET state = $1 WHERE id = $2")
            .bind(&claim.state)
            .bind(claim.id)
            .execute(&mut *tx)
            .await?;
    }
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

fn plan_case_closed(
    link: &(Option<Uuid>, Option<String>, Option<NaiveDate>),
    claim: &Claim,
) -> Option<NaiveDate> {
    link.2.filter(|_| {
        link.0 == Some(claim.legal_plan_id)
            && link.1.as_deref() == Some(claim.plan_case_number.as_str())
    })
}

async fn adopt_filing_rule(
    connection: &mut sqlx::PgConnection,
    claim: &mut Claim,
) -> Result<(), ClaimError> {
    if claim.state.filing_rule.is_none() {
        let work = &claim.state.work;
        claim.state.filing_rule = filing_rule(
            connection,
            claim.legal_plan_id,
            &work.benefit,
            work.performed_on,
        )
        .await?;
    }
    Ok(())
}

/// Link a filed claim packet to its claim. Filing the same packet again
/// changes nothing.
///
/// # Errors
/// [`ClaimError`] when the claim is not on the Project or its stage is past
/// filing.
pub async fn record_packet(
    pg: &PostgresDb,
    project: Uuid,
    firm: Uuid,
    actor: Uuid,
    claim: Uuid,
    (asset_id, sha256): (Uuid, &str),
    timezone: chrono_tz::Tz,
) -> Result<Claim, ClaimError> {
    let event = Event::PacketFiled {
        asset_id,
        sha256: sha256.to_owned(),
    };
    // One event per (claim, packet): a repeated filing replays it.
    let digest = <sha2::Sha256 as sha2::Digest>::digest(format!("packet:{claim}:{asset_id}"));
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest[..16]);
    let event_id = uuid::Builder::from_random_bytes(bytes).into_uuid();
    record_change(
        pg,
        project,
        firm,
        actor,
        timezone,
        Change {
            claim_id: claim,
            event_id,
            occurred_on: Utc::now().date_naive(),
            event,
        },
    )
    .await
}

/// The claim and its plan for a packet: the verified work, its plan case,
/// and the stage it stands at.
pub struct PacketClaim {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub plan_name: String,
    pub plan_case_number: String,
    pub work: Work,
    pub stage: &'static str,
    pub covered_cents: Option<i64>,
    pub claimed_cents: Option<i32>,
}

/// Read one claim on `project` for its packet.
///
/// # Errors
/// [`ClaimError::NotFound`] outside the Project and Firm.
pub async fn for_packet(
    pg: &PostgresDb,
    project: Uuid,
    firm: Uuid,
    claim: Uuid,
) -> Result<PacketClaim, ClaimError> {
    let mut connection = pg.pool().acquire().await?;
    let ((id, _, plan_id, plan_case_number, state), plan_name) =
        claim_rows(&mut connection, project, firm)
            .await?
            .into_iter()
            .find(|((id, ..), _)| *id == claim)
            .ok_or(ClaimError::NotFound)?;
    let state = state.0;
    Ok(PacketClaim {
        id,
        plan_id,
        plan_name,
        plan_case_number,
        covered_cents: state.work.estimate()?,
        claimed_cents: state.claimed_cents,
        stage: match state.stage {
            Stage::Completed => "completed",
            Stage::Prepared => "prepared",
            Stage::Submitted => "submitted",
            Stage::Approved => "approved",
            Stage::Denied => "denied",
        },
        work: state.work,
    })
}

async fn verified_work(
    tx: &mut Transaction<'_, Postgres>,
    project: Uuid,
    claim: Uuid,
    work: &Work,
    plan: &str,
    today: NaiveDate,
    configured: bool,
) -> Result<Work, ClaimError> {
    let mut work = work.clone();
    if !work.time_entry_ids.is_empty() {
        if work.attorney_minutes.is_some() || work.time_entry_ids.len() > 1_000 {
            return Err(ClaimError::Invalid(
                "supply Time Entry IDs or attested minutes",
            ));
        }
        let minutes: Vec<i32> = sqlx::query_scalar(
            "SELECT t.minutes FROM time_entry t
             WHERE t.id = ANY($1) AND t.project_id = $2 AND t.performed_on <= $3
             AND t.billing_status = 'unbilled'
             AND NOT EXISTS (
                 SELECT 1 FROM legal_plan_claim c WHERE c.project_id = $2 AND c.id <> $4
                 AND (c.state->'work'->'time_entry_ids') ? t.id::text
             ) ORDER BY t.performed_on, t.id FOR UPDATE OF t",
        )
        .bind(&work.time_entry_ids)
        .bind(project)
        .bind(work.performed_on)
        .bind(claim)
        .fetch_all(&mut **tx)
        .await?;
        if minutes.len() != work.time_entry_ids.len() {
            return Err(ClaimError::Invalid(
                "Time Entries must be distinct, unused, and on this Project",
            ));
        }
        work.attorney_minutes = Some(minutes.into_iter().sum());
    }
    work.validate(plan, today, configured)?;
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
    closed_on: Option<NaiveDate>,
    filing_rule: Option<FilingRule>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    filing_status: Option<FilingStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    filing_due_on: Option<NaiveDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    filing_destination: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    filer: Option<String>,
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
    accruing_count: usize,
    expired_count: usize,
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
         AND e.occurred_on > CURRENT_DATE - 7), 0) AS net_collected_this_week_cents,
         CASE WHEN c.legal_plan_id = p.legal_plan_id AND c.plan_case_number = p.plan_case_number
              THEN p.plan_closed_on END,
         to_jsonb(rule) - 'legal_plan_id'
         FROM legal_plan_claim c JOIN project p ON p.id = c.project_id
         JOIN legal_plan l ON l.id = c.legal_plan_id
         LEFT JOIN LATERAL (
             SELECT r.* FROM legal_plan_filing_rule r
             WHERE r.legal_plan_id=c.legal_plan_id
               AND r.benefit_code=c.state->'work'->>'benefit'
               AND r.effective_on <= (c.state->'work'->>'performed_on')::date
             ORDER BY r.effective_on DESC LIMIT 1
         ) rule ON TRUE
         WHERE p.firm_id = $1 AND l.firm_id = $1 AND ($2::uuid[] IS NULL OR p.id = ANY($2))
         ORDER BY (c.state->>'submitted_on') NULLS FIRST, c.state->'work'->>'performed_on', c.id",
    )
    .bind(firm)
    .bind(visible_projects)
    .fetch_all(&mut *tx)
    .await?;
    let closed: Vec<(String, String, bool)> = sqlx::query_as(
        "SELECT l.name, p.code,
                EXISTS (SELECT 1 FROM legal_plan_filing_rule r WHERE r.legal_plan_id = l.id)
         FROM project p JOIN legal_plan l ON l.id = p.legal_plan_id
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
            |(
                id,
                code,
                plan,
                state,
                recorded_this_week,
                net_collected_this_week_cents,
                closed_on,
                rule,
            )| {
                ReviewRow {
                    id,
                    code,
                    plan,
                    state,
                    recorded_this_week,
                    net_collected_this_week_cents,
                    closed_on,
                    filing_rule: rule.map(|rule| rule.0),
                }
            },
        )
        .collect();
    let mut report = summarize(Utc::now().date_naive(), rows, visible_projects.is_some())?;
    for (plan, code, configured) in closed {
        if !configured
            && !matches!(
                provider_for_plan_name(&plan),
                Some(PlanProvider::Arag | PlanProvider::LegalEase)
            )
        {
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

fn tally_filing(
    totals: &mut PlanTotals,
    review: &mut FilingReview,
    plan: &str,
    service: Service,
    filing: Option<&Filing>,
    estimate: Option<i64>,
) -> Result<(), ClaimError> {
    match (filing.map(|item| item.status), estimate) {
        (Some(FilingStatus::Ready), Some(cents)) => {
            let carrier = match service {
                Service::AragAdviceOnly | Service::AragRepresentation => &mut review.arag,
                Service::LegalEaseLegalGuard | Service::LegalEasePaymentAuthorization => {
                    &mut review.legal_ease
                }
                Service::Manual => review.other_plans.entry(plan.to_owned()).or_default(),
            };
            for subtotal in [&mut totals.filing_review, carrier] {
                subtotal.eligible_unsubmitted_count += 1;
                subtotal.eligible_unsubmitted_cents = subtotal
                    .eligible_unsubmitted_cents
                    .checked_add(cents)
                    .ok_or(ClaimError::Invalid("filing review total is too large"))?;
            }
        }
        (Some(FilingStatus::Accruing), _) => totals.accruing_count += 1,
        (Some(FilingStatus::Expired), _) => totals.expired_count += 1,
        _ => totals.needs_verification_count += 1,
    }
    Ok(())
}

fn summarize(
    today: NaiveDate,
    rows: Vec<ReviewRow>,
    show_projects: bool,
) -> Result<Worklist, ClaimError> {
    let mut plans = BTreeMap::<String, PlanTotals>::new();
    let mut filing_review = FilingReview::default();
    for row in rows {
        let totals = plans.entry(row.plan.clone()).or_default();
        let state = row.state.0;
        let selected_rule = state.filing_rule.as_ref().or(row.filing_rule.as_ref());
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
        let filing = matches!(state.stage, Stage::Completed | Stage::Prepared)
            .then(|| state.filing(row.filing_rule.as_ref(), row.closed_on, today))
            .transpose()?;
        match state.stage {
            Stage::Completed | Stage::Prepared => tally_filing(
                totals,
                &mut filing_review,
                &row.plan,
                state.work.service,
                filing.as_ref(),
                estimate,
            )?,
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
                filing_status: filing.as_ref().map(|item| item.status),
                filing_due_on: filing.and_then(|item| item.due_on),
                filing_destination: selected_rule.map(|rule| rule.filing_destination.clone()),
                filer: selected_rule.map(|rule| rule.filer.clone()),
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

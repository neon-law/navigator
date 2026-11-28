//! Time Entry Proposals: time a lawyer records or Navigator proposes from a
//! Project's dated record, held for review before it becomes a priced Time
//! Entry. See `docs/glossary/time-entry-proposal.md`.
//!
//! A proposal prices nothing. Approval resolves the rate the way staged
//! Jujutsu time does ([`crate::billing_annotations`]) and creates the Time
//! Entry in the same transaction; the reviewed proposal is retained with its
//! decision. The review queue also lists staged Jujutsu time, so one set of
//! verbs reviews both.
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::{
    billing_annotations::{self, Decision},
    postgres::PostgresDb,
    time_entries::{TimeEntryError, TimeEntrySource, MAX_MINUTES_PER_DAY},
};

#[derive(Debug, thiserror::Error)]
pub enum ProposalError {
    #[error("{0}")]
    Invalid(&'static str),
    #[error("the timekeeper must be a firm-side participant on the Project")]
    NotParticipant,
    #[error("no pending Time Entry Proposal or staged time with that id on this Project")]
    NotFound,
    #[error("{0}")]
    Decided(&'static str),
    #[error("the request id was already used for different time")]
    RequestReused,
    #[error(transparent)]
    Time(#[from] TimeEntryError),
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
}

/// Where a proposal came from. `Lawyer` is time recorded by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Lawyer,
    Communication,
    Document,
    Deadline,
    Efiling,
}

impl SourceKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lawyer => "lawyer",
            Self::Communication => "communication",
            Self::Document => "document",
            Self::Deadline => "deadline",
            Self::Efiling => "efiling",
        }
    }

    fn parse(raw: &str) -> Result<Self, ProposalError> {
        Ok(match raw {
            "lawyer" => Self::Lawyer,
            "communication" => Self::Communication,
            "document" => Self::Document,
            "deadline" => Self::Deadline,
            "efiling" => Self::Efiling,
            _ => return Err(ProposalError::Invalid("unknown proposal source")),
        })
    }
}

/// One row of a Project's time review list: a proposal, staged Jujutsu
/// time, or an approved Time Entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeRow {
    pub id: Uuid,
    pub performed_on: NaiveDate,
    pub minutes: i32,
    pub timekeeper_id: Uuid,
    pub narrative: String,
    /// `pending_review`, `discarded`, or an approved entry's billing status:
    /// `unbilled`, `reserved`, `invoiced`, or `written_off`.
    pub status: String,
    /// Recorded after the day the work was done.
    pub backfilled: bool,
    /// `lawyer`, `communication`, `document`, `deadline`, `efiling`,
    /// `jujutsu`, `timer`, or `manual`.
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_id: Option<Uuid>,
    /// The Time Entry an approved proposal created.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recorded_at: Option<DateTime<Utc>>,
}

/// Which rows [`list`] returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusFilter {
    /// Proposals and staged Jujutsu time awaiting review.
    PendingReview,
    /// Every Time Entry, whatever its billing status.
    Approved,
    /// Approved and not yet invoiced or written off.
    Unbilled,
    /// Invoiced.
    Billed,
    /// Proposals the lawyer discarded.
    Discarded,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewProposal {
    pub request_id: Uuid,
    pub timekeeper_id: Uuid,
    pub performed_on: NaiveDate,
    pub minutes: i32,
    pub narrative: String,
}

/// Changes to a pending proposal. An absent field keeps its value.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalEdit {
    pub performed_on: Option<NaiveDate>,
    pub minutes: Option<i32>,
    pub narrative: Option<String>,
}

/// The lawyer's decision on a proposal or on staged Jujutsu time.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Approval {
    /// `bill` (default), `no_charge`, or `plan_claim`.
    #[serde(default = "bill")]
    pub decision: Decision,
    /// The Legal Plan Claim the time belongs to, for `plan_claim`.
    #[serde(default)]
    pub claim_id: Option<Uuid>,
    /// Invoice wording. Required for staged Jujutsu time; replaces a
    /// proposal's narrative when given.
    #[serde(default)]
    pub narrative: Option<String>,
    /// Reviewed minutes for staged Jujutsu time, at most the staged time.
    #[serde(default)]
    pub minutes: Option<i32>,
}

const fn bill() -> Decision {
    Decision::Bill
}

fn validate(
    performed_on: NaiveDate,
    minutes: i32,
    narrative: &str,
    today: NaiveDate,
) -> Result<(), ProposalError> {
    if !(1..=MAX_MINUTES_PER_DAY).contains(&minutes) {
        return Err(ProposalError::Invalid("minutes must be between 1 and 1440"));
    }
    let narrative = narrative.trim();
    if narrative.is_empty() || narrative.len() > 2000 {
        return Err(ProposalError::Invalid(
            "narrative must be 1 to 2000 characters",
        ));
    }
    if performed_on > today {
        return Err(ProposalError::Invalid("work cannot be dated in the future"));
    }
    Ok(())
}

/// The columns [`proposal_row`] reads, spliced into static SQL.
macro_rules! proposal_columns {
    () => {
        "id,performed_on,minutes,timekeeper_id,narrative,status,backfilled,source_kind,source_id,entry_id,inserted_at"
    };
}

fn proposal_row(row: &sqlx::postgres::PgRow) -> Result<TimeRow, ProposalError> {
    Ok(TimeRow {
        id: row.try_get("id")?,
        performed_on: row.try_get("performed_on")?,
        minutes: row.try_get("minutes")?,
        timekeeper_id: row.try_get("timekeeper_id")?,
        narrative: row.try_get("narrative")?,
        status: row.try_get("status")?,
        backfilled: row.try_get("backfilled")?,
        source: SourceKind::parse(&row.try_get::<String, _>("source_kind")?)?
            .as_str()
            .to_owned(),
        source_id: row.try_get("source_id")?,
        entry_id: row.try_get("entry_id")?,
        recorded_at: Some(row.try_get("inserted_at")?),
    })
}

async fn find(pg: &PostgresDb, project: Uuid, id: Uuid) -> Result<Option<TimeRow>, ProposalError> {
    sqlx::query(concat!(
        "SELECT ",
        proposal_columns!(),
        " FROM time_entry_proposal WHERE project_id=$1 AND id=$2"
    ))
    .bind(project)
    .bind(id)
    .fetch_optional(&pg.0)
    .await?
    .as_ref()
    .map(proposal_row)
    .transpose()
}

/// A firm-side participant: the only people whose time a matter records.
async fn require_timekeeper(
    pg: &PostgresDb,
    project: Uuid,
    person: Uuid,
) -> Result<(), ProposalError> {
    let firm_side: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM person_project_role WHERE project_id=$1 AND person_id=$2
         AND participation NOT IN ('client','counterparty'))",
    )
    .bind(project)
    .bind(person)
    .fetch_one(&pg.0)
    .await?;
    if firm_side {
        Ok(())
    } else {
        Err(ProposalError::NotParticipant)
    }
}

/// Record time for review. A past date is allowed and marks the proposal
/// backfilled; its insertion time is kept. Replaying a `request_id` returns
/// the row it first created.
///
/// # Errors
/// [`ProposalError::Invalid`] for bad minutes, narrative, or a future date;
/// [`ProposalError::NotParticipant`] when the timekeeper is not firm-side on
/// the Project; [`ProposalError::RequestReused`] when the request id was used
/// for another timekeeper or source.
pub async fn add(
    pg: &PostgresDb,
    project: Uuid,
    actor: Uuid,
    input: &NewProposal,
) -> Result<TimeRow, ProposalError> {
    validate(
        input.performed_on,
        input.minutes,
        &input.narrative,
        Utc::now().date_naive(),
    )?;
    require_timekeeper(pg, project, input.timekeeper_id).await?;
    let inserted = sqlx::query(concat!(
        "INSERT INTO time_entry_proposal (id,project_id,timekeeper_id,performed_on,minutes,narrative,
         source_kind,request_id,status,created_by)
         VALUES ($1,$2,$3,$4,$5,$6,'lawyer',$7,'pending_review',$8)
         ON CONFLICT (project_id, request_id) DO NOTHING RETURNING ",
        proposal_columns!(),
        ""
    ))
    .bind(Uuid::now_v7())
    .bind(project)
    .bind(input.timekeeper_id)
    .bind(input.performed_on)
    .bind(input.minutes)
    .bind(input.narrative.trim())
    .bind(input.request_id)
    .bind(actor)
    .fetch_optional(&pg.0)
    .await?;
    if let Some(row) = inserted {
        return proposal_row(&row);
    }
    let existing = sqlx::query(concat!(
        "SELECT ",
        proposal_columns!(),
        " FROM time_entry_proposal WHERE project_id=$1 AND request_id=$2"
    ))
    .bind(project)
    .bind(input.request_id)
    .fetch_one(&pg.0)
    .await?;
    let existing = proposal_row(&existing)?;
    if existing.timekeeper_id != input.timekeeper_id || existing.source != "lawyer" {
        return Err(ProposalError::RequestReused);
    }
    Ok(existing)
}

/// Change a pending proposal's date, minutes, or narrative.
///
/// # Errors
/// [`ProposalError::Decided`] once the proposal is approved or discarded,
/// and for staged Jujutsu time, whose attested minutes are lowered at
/// approval instead.
pub async fn edit(
    pg: &PostgresDb,
    project: Uuid,
    id: Uuid,
    change: &ProposalEdit,
) -> Result<TimeRow, ProposalError> {
    let Some(current) = find(pg, project, id).await? else {
        if billing_annotations::is_pending(pg, project, id).await? {
            return Err(ProposalError::Decided(
                "staged Jujutsu time keeps its attested minutes; approve it with --minutes to record less",
            ));
        }
        return Err(ProposalError::NotFound);
    };
    if current.status != "pending_review" {
        return Err(ProposalError::Decided(
            "only a pending Time Entry Proposal can be edited",
        ));
    }
    let performed_on = change.performed_on.unwrap_or(current.performed_on);
    let minutes = change.minutes.unwrap_or(current.minutes);
    let narrative = change
        .narrative
        .as_deref()
        .unwrap_or(&current.narrative)
        .trim()
        .to_owned();
    validate(performed_on, minutes, &narrative, Utc::now().date_naive())?;
    let row = sqlx::query(concat!(
        "UPDATE time_entry_proposal SET performed_on=$3,minutes=$4,narrative=$5,updated_at=now()
         WHERE project_id=$1 AND id=$2 AND status='pending_review' RETURNING ",
        proposal_columns!(),
        ""
    ))
    .bind(project)
    .bind(id)
    .bind(performed_on)
    .bind(minutes)
    .bind(&narrative)
    .fetch_optional(&pg.0)
    .await?
    .ok_or(ProposalError::Decided(
        "only a pending Time Entry Proposal can be edited",
    ))?;
    proposal_row(&row)
}

/// Discard a pending proposal or deny staged Jujutsu time. Discarding a
/// discarded proposal returns it unchanged.
///
/// # Errors
/// [`ProposalError::Decided`] for an approved proposal.
pub async fn discard(
    pg: &PostgresDb,
    project: Uuid,
    actor: Uuid,
    id: Uuid,
    reason: Option<&str>,
) -> Result<TimeRow, ProposalError> {
    let Some(current) = find(pg, project, id).await? else {
        if billing_annotations::is_pending(pg, project, id).await? {
            let note = reason.unwrap_or("Discarded in review");
            billing_annotations::review_annotation(
                pg,
                project,
                actor,
                id,
                Decision::Deny,
                note,
                None,
                None,
            )
            .await?;
            return reviewed_annotation(pg, project, id).await;
        }
        return Err(ProposalError::NotFound);
    };
    match current.status.as_str() {
        "discarded" => return Ok(current),
        "approved" => {
            return Err(ProposalError::Decided(
                "an approved proposal is a Time Entry; write it off instead",
            ))
        }
        _ => {}
    }
    let row = sqlx::query(concat!(
        "UPDATE time_entry_proposal SET status='discarded',reviewed_by=$3,reviewed_at=now(),updated_at=now()
         WHERE project_id=$1 AND id=$2 AND status='pending_review' RETURNING ",
        proposal_columns!(),
        ""
    ))
    .bind(project)
    .bind(id)
    .bind(actor)
    .fetch_optional(&pg.0)
    .await?;
    match row {
        Some(row) => proposal_row(&row),
        // A concurrent review won; report the decision it recorded.
        None => find(pg, project, id).await?.ok_or(ProposalError::NotFound),
    }
}

/// Approve a pending proposal or staged Jujutsu time. The Time Entry is
/// priced and created in the same transaction that records the decision.
/// Approving an approved proposal returns it unchanged.
///
/// # Errors
/// [`ProposalError::Decided`] for a discarded proposal;
/// [`ProposalError::Invalid`] for a missing rate, an unmatched claim, or
/// minutes above staged time; [`ProposalError::NotFound`] for an unknown id.
pub async fn approve(
    pg: &PostgresDb,
    project: Uuid,
    actor: Uuid,
    id: Uuid,
    approval: &Approval,
) -> Result<TimeRow, ProposalError> {
    if matches!(approval.decision, Decision::Deny) {
        return Err(ProposalError::Invalid("use discard to deny time"));
    }
    let Some(current) = find(pg, project, id).await? else {
        return approve_staged(pg, project, actor, id, approval).await;
    };
    match current.status.as_str() {
        "approved" => return Ok(current),
        "discarded" => {
            return Err(ProposalError::Decided(
                "a discarded proposal stays discarded; add the time again",
            ))
        }
        _ => {}
    }
    if approval.minutes.is_some() {
        return Err(ProposalError::Invalid(
            "edit the proposal's minutes before approving it",
        ));
    }
    let narrative = approval
        .narrative
        .as_deref()
        .unwrap_or(&current.narrative)
        .trim()
        .to_owned();
    validate(
        current.performed_on,
        current.minutes,
        &narrative,
        Utc::now().date_naive(),
    )?;
    let rate = if matches!(approval.decision, Decision::Bill) {
        billing_annotations::billable_rate(pg, project, current.timekeeper_id, current.performed_on)
            .await?
    } else {
        0
    };
    let mut tx = pg.0.begin().await?;
    let locked: Option<(String, i32, NaiveDate)> = sqlx::query_as(
        "SELECT status,minutes,performed_on FROM time_entry_proposal WHERE project_id=$1 AND id=$2 FOR UPDATE",
    )
    .bind(project)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    if locked
        != Some((
            "pending_review".into(),
            current.minutes,
            current.performed_on,
        ))
    {
        drop(tx);
        // A concurrent edit or review changed the row; read its decision.
        let now = find(pg, project, id)
            .await?
            .ok_or(ProposalError::NotFound)?;
        return if now.status == "approved" {
            Ok(now)
        } else {
            Err(ProposalError::Decided(
                "the proposal changed while it was being approved; review it again",
            ))
        };
    }
    let entry = billing_annotations::record_reviewed_time(
        &mut tx,
        project,
        actor,
        current.timekeeper_id,
        current.performed_on,
        current.minutes,
        &narrative,
        approval.decision,
        rate,
        approval.claim_id,
        TimeEntrySource::Manual,
    )
    .await?;
    let row = sqlx::query(concat!(
        "UPDATE time_entry_proposal SET status='approved',narrative=$3,reviewed_by=$4,reviewed_at=now(),
         entry_id=$5,updated_at=now() WHERE project_id=$1 AND id=$2 RETURNING ",
        proposal_columns!(),
        ""
    ))
    .bind(project)
    .bind(id)
    .bind(&narrative)
    .bind(actor)
    .bind(entry)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    proposal_row(&row)
}

/// Approve staged Jujutsu time, which needs the invoice wording.
async fn approve_staged(
    pg: &PostgresDb,
    project: Uuid,
    actor: Uuid,
    id: Uuid,
    approval: &Approval,
) -> Result<TimeRow, ProposalError> {
    if !billing_annotations::is_pending(pg, project, id).await? {
        return Err(ProposalError::NotFound);
    }
    let note = approval.narrative.as_deref().ok_or(ProposalError::Invalid(
        "staged Jujutsu time needs --narrative, the invoice wording",
    ))?;
    billing_annotations::review_annotation(
        pg,
        project,
        actor,
        id,
        approval.decision,
        note,
        approval.claim_id,
        approval.minutes,
    )
    .await?;
    reviewed_annotation(pg, project, id).await
}

async fn reviewed_annotation(
    pg: &PostgresDb,
    project: Uuid,
    id: Uuid,
) -> Result<TimeRow, ProposalError> {
    let row = sqlx::query(
        "SELECT a.id,a.performed_on,coalesce(t.minutes,a.tenths*6) AS minutes,a.attorney_id,
         coalesce(a.review_note,a.change_id) AS narrative,a.entry_id,a.staged_at,
         CASE WHEN a.entry_id IS NULL THEN 'discarded' ELSE t.billing_status END AS status
         FROM billing_annotation a LEFT JOIN time_entry t ON t.id=a.entry_id
         WHERE a.project_id=$1 AND a.id=$2",
    )
    .bind(project)
    .bind(id)
    .fetch_one(&pg.0)
    .await?;
    Ok(TimeRow {
        id,
        performed_on: row.try_get("performed_on")?,
        minutes: row.try_get("minutes")?,
        timekeeper_id: row.try_get("attorney_id")?,
        narrative: row.try_get("narrative")?,
        status: row.try_get("status")?,
        backfilled: false,
        source: TimeEntrySource::Jujutsu.as_str().to_owned(),
        source_id: None,
        entry_id: row.try_get("entry_id")?,
        recorded_at: Some(row.try_get("staged_at")?),
    })
}

/// The Project's time review list, oldest work first.
///
/// # Errors
/// A database failure.
pub async fn list(
    pg: &PostgresDb,
    project: Uuid,
    status: Option<StatusFilter>,
) -> Result<Vec<TimeRow>, ProposalError> {
    let mut rows = Vec::new();
    let proposals: Option<&[&str]> = match status {
        None => Some(&["pending_review", "discarded"]),
        Some(StatusFilter::PendingReview) => Some(&["pending_review"]),
        Some(StatusFilter::Discarded) => Some(&["discarded"]),
        Some(_) => None,
    };
    if let Some(statuses) = proposals {
        for row in sqlx::query(concat!(
            "SELECT ",
            proposal_columns!(),
            " FROM time_entry_proposal WHERE project_id=$1 AND status = ANY($2)"
        ))
        .bind(project)
        .bind(statuses)
        .fetch_all(&pg.0)
        .await?
        {
            rows.push(proposal_row(&row)?);
        }
    }
    if matches!(status, None | Some(StatusFilter::PendingReview)) {
        for pending in billing_annotations::pending(pg, project).await? {
            rows.push(TimeRow {
                id: pending.id,
                performed_on: pending.performed_on,
                minutes: pending.tenths * 6,
                timekeeper_id: pending.attorney_id,
                narrative: pending.change_id,
                status: "pending_review".into(),
                backfilled: false,
                source: TimeEntrySource::Jujutsu.as_str().to_owned(),
                source_id: None,
                entry_id: None,
                recorded_at: None,
            });
        }
    }
    let entries: Option<&[&str]> = match status {
        None | Some(StatusFilter::Approved) => {
            Some(&["unbilled", "reserved", "invoiced", "written_off"])
        }
        Some(StatusFilter::Unbilled) => Some(&["unbilled", "reserved"]),
        Some(StatusFilter::Billed) => Some(&["invoiced"]),
        Some(StatusFilter::PendingReview | StatusFilter::Discarded) => None,
    };
    if let Some(statuses) = entries {
        for row in sqlx::query(
            "SELECT t.id,t.performed_on,t.minutes,t.attorney_id,t.description,t.billing_status,
             t.source,t.inserted_at,p.source_kind,p.source_id,
             coalesce(p.backfilled, t.source='manual' AND t.performed_on < timezone('UTC',t.inserted_at)::date)
               AS backfilled
             FROM time_entry t LEFT JOIN time_entry_proposal p ON p.entry_id=t.id
             WHERE t.project_id=$1 AND t.billing_status = ANY($2)",
        )
        .bind(project)
        .bind(statuses)
        .fetch_all(&pg.0)
        .await?
        {
            let source: Option<String> = row.try_get("source_kind")?;
            rows.push(TimeRow {
                id: row.try_get("id")?,
                performed_on: row.try_get("performed_on")?,
                minutes: row.try_get("minutes")?,
                timekeeper_id: row.try_get("attorney_id")?,
                narrative: row.try_get("description")?,
                status: row.try_get("billing_status")?,
                backfilled: row.try_get("backfilled")?,
                source: source.map_or_else(|| row.try_get("source"), Ok)?,
                source_id: row.try_get("source_id")?,
                entry_id: None,
                recorded_at: Some(row.try_get("inserted_at")?),
            });
        }
    }
    rows.sort_by_key(|row| (row.performed_on, row.id));
    Ok(rows)
}

/// The Time Entries on `project` recorded after the day of the work.
pub(crate) async fn backfilled_entries(
    connection: &mut sqlx::PgConnection,
    project: Uuid,
) -> Result<std::collections::HashSet<Uuid>, sqlx::Error> {
    Ok(sqlx::query_scalar::<_, Uuid>(
        "SELECT t.id FROM time_entry t LEFT JOIN time_entry_proposal p ON p.entry_id=t.id
         WHERE t.project_id=$1 AND coalesce(p.backfilled,
           t.source='manual' AND t.performed_on < timezone('UTC',t.inserted_at)::date)",
    )
    .bind(project)
    .fetch_all(connection)
    .await?
    .into_iter()
    .collect())
}

/// Pending proposals for the billing view.
pub(crate) async fn pending_for_billing(
    connection: &mut sqlx::PgConnection,
    project: Uuid,
    timekeeper: Option<Uuid>,
) -> Result<Vec<TimeRow>, ProposalError> {
    sqlx::query(concat!(
        "SELECT ",
        proposal_columns!(),
        " FROM time_entry_proposal WHERE project_id=$1
         AND ($2::uuid IS NULL OR timekeeper_id=$2) AND status='pending_review'
         ORDER BY performed_on,id"
    ))
    .bind(project)
    .bind(timekeeper)
    .fetch_all(connection)
    .await?
    .iter()
    .map(proposal_row)
    .collect()
}

#[path = "time_proposals/backfill.rs"]
mod backfill;
pub use backfill::{backfill, Backfill, Candidate, DefaultMinutes, DEFAULT_MINUTES};

#[cfg(test)]
#[path = "time_proposals/tests.rs"]
mod tests;

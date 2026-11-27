//! Stores time submitted for lawyer review.
use chrono::{NaiveDate, Utc};
use serde::Deserialize;
use sqlx::Row;
use uuid::Uuid;

use crate::{
    postgres::PostgresDb,
    time_entries::{self, NewTimeEntry, TimeEntryError, TimeEntrySource},
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Annotation {
    pub id: Uuid,
    pub change_id: String,
    pub commit_id: String,
    pub tenths: i32,
    pub performed_on: NaiveDate,
}

pub struct Pending {
    pub id: Uuid,
    pub attorney_id: Uuid,
    pub change_id: String,
    pub commit_id: String,
    pub tenths: i32,
    pub performed_on: NaiveDate,
}

/// Identical replays and rebases retain the originally attested commit.
/// A copied UUID, changed time, or second annotation for a change requires correction.
pub async fn stage(
    pg: &PostgresDb,
    project: Uuid,
    actor: Uuid,
    a: &Annotation,
) -> Result<(), TimeEntryError> {
    if a.id.is_nil()
        || a.change_id.len() != 32
        || !a.change_id.bytes().all(|b| (b'k'..=b'z').contains(&b))
        || !matches!(a.commit_id.len(), 40 | 64)
        || !a
            .commit_id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        || !(1..=240).contains(&a.tenths)
        || a.performed_on > Utc::now().date_naive()
    {
        return Err(TimeEntryError::Invalid("billing annotation"));
    }
    let mut tx = pg.0.begin().await?;
    sqlx::query("INSERT INTO billing_annotation(id,project_id,attorney_id,change_id,commit_id,tenths,performed_on)
        VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT DO NOTHING")
        .bind(a.id).bind(project).bind(actor).bind(&a.change_id).bind(&a.commit_id).bind(a.tenths).bind(a.performed_on)
        .execute(&mut *tx).await?;
    let matches = sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM billing_annotation
        WHERE id=$1 AND project_id=$2 AND attorney_id=$3 AND change_id=$4 AND tenths=$5 AND performed_on=$6)")
        .bind(a.id).bind(project).bind(actor).bind(&a.change_id).bind(a.tenths).bind(a.performed_on)
        .fetch_one(&mut *tx).await?;
    if !matches {
        return Err(TimeEntryError::Conflict);
    }
    tx.commit().await?;
    Ok(())
}

pub async fn pending(pg: &PostgresDb, project: Uuid) -> Result<Vec<Pending>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id,attorney_id,change_id,commit_id,tenths,performed_on FROM billing_annotation
        WHERE project_id=$1 AND reviewed_at IS NULL ORDER BY performed_on,staged_at,id",
    )
    .bind(project)
    .fetch_all(&pg.0)
    .await?;
    Ok(rows
        .iter()
        .map(|r| Pending {
            id: r.get("id"),
            attorney_id: r.get("attorney_id"),
            change_id: r.get("change_id"),
            commit_id: r.get("commit_id"),
            tenths: r.get("tenths"),
            performed_on: r.get("performed_on"),
        })
        .collect())
}

pub(crate) async fn pending_for_billing(
    connection: &mut sqlx::PgConnection,
    project_id: Uuid,
    attorney_id: Option<Uuid>,
) -> Result<Vec<(Uuid, Uuid, String, i32, NaiveDate)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id,attorney_id,change_id,tenths,performed_on FROM billing_annotation
         WHERE project_id=$1 AND ($2::uuid IS NULL OR attorney_id=$2) AND reviewed_at IS NULL",
    )
    .bind(project_id)
    .bind(attorney_id)
    .fetch_all(connection)
    .await
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Bill,
    PlanClaim,
    NoCharge,
    Deny,
}

/// Approval and its Time Entry commit together, including the attorney's invoice wording.
pub async fn review(
    pg: &PostgresDb,
    project: Uuid,
    actor: Uuid,
    id: Uuid,
    decision: Decision,
    note: &str,
) -> Result<(), TimeEntryError> {
    review_with_claim(pg, project, actor, id, decision, note, None).await
}

/// Review staged time with an explicit Legal Plan Claim selection.
pub async fn review_with_claim(
    pg: &PostgresDb,
    project: Uuid,
    actor: Uuid,
    id: Uuid,
    decision: Decision,
    note: &str,
    claim_id: Option<Uuid>,
) -> Result<(), TimeEntryError> {
    let note = note.trim();
    if note.is_empty() || note.len() > 2000 {
        return Err(TimeEntryError::Invalid("review note"));
    }
    let source = sqlx::query("SELECT attorney_id,performed_on,tenths FROM billing_annotation WHERE project_id=$1 AND id=$2 AND reviewed_at IS NULL")
        .bind(project).bind(id).fetch_optional(&pg.0).await?.ok_or(TimeEntryError::Conflict)?;
    let attorney = source.get("attorney_id");
    let date = source.get("performed_on");
    let mut rate = if matches!(decision, Decision::Bill) {
        match crate::contingency_terms::resolve_unchecked(pg, project, date)
            .await
            .map_err(|_| TimeEntryError::Invalid("contingency terms unavailable"))?
        {
            Some(term) => term.rate_cents_per_hour.ok_or(TimeEntryError::Invalid(
                "contingency agreement has no hourly fee",
            ))?,
            None => crate::attorney_rates::resolve_hourly_rate_cents(pg, attorney, project, date)
                .await
                .map_err(|_| TimeEntryError::Invalid("hourly rate unavailable"))?
                .ok_or(TimeEntryError::Invalid("missing hourly rate"))?,
        }
    } else {
        0
    };
    let mut tx = pg.0.begin().await?;
    let claimed = sqlx::query("SELECT id FROM billing_annotation WHERE project_id=$1 AND id=$2 AND reviewed_at IS NULL FOR UPDATE")
        .bind(project).bind(id).fetch_optional(&mut *tx).await?;
    if claimed.is_none() {
        return Err(TimeEntryError::Conflict);
    }
    let minutes = source.get::<i32, _>("tenths") * 6;
    let claim = if matches!(decision, Decision::PlanClaim) {
        let claim_id = claim_id.ok_or(TimeEntryError::Invalid("select a legal plan claim"))?;
        let (claim, claim_rate) =
            crate::plan_claims::for_annotation(&mut tx, project, claim_id, date, minutes).await?;
        rate = claim_rate;
        Some(claim)
    } else {
        None
    };
    let entry = if matches!(decision, Decision::Deny) {
        None
    } else {
        Some(
            time_entries::create_in_transaction(
                &mut tx,
                project,
                actor,
                &NewTimeEntry {
                    attorney_id: attorney,
                    performed_on: date,
                    minutes,
                    description: note.to_owned(),
                    billable: matches!(decision, Decision::Bill),
                    rate_cents_per_hour: rate,
                    source: TimeEntrySource::Jujutsu,
                },
            )
            .await?,
        )
    };
    if let (Some(claim), Some(entry)) = (claim, entry) {
        crate::plan_claims::link_annotation(&mut tx, claim, actor, entry, minutes).await?;
    }
    sqlx::query("UPDATE billing_annotation SET reviewed_by=$1,reviewed_at=now(),review_note=$2,entry_id=$3 WHERE project_id=$4 AND id=$5")
        .bind(actor).bind(note).bind(entry).bind(project).bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

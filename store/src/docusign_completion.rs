//! Durable work and review receipts for completed DocuSign envelopes.

use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::postgres::PostgresDb;

pub struct Receipt {
    pub firm_id: Uuid,
    pub account_id: Uuid,
    pub envelope_id: Uuid,
    pub project_id: Option<Uuid>,
}

pub async fn enqueue(
    db: &PostgresDb,
    firm: Uuid,
    account: Uuid,
    envelopes: &[Uuid],
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO docusign_completion (firm_id, account_id, envelope_id) \
         SELECT $1, $2, unnest($3::uuid[]) ON CONFLICT DO NOTHING",
    )
    .bind(firm)
    .bind(account)
    .bind(envelopes)
    .execute(db.pool())
    .await?;
    Ok(())
}

pub async fn claim(tx: &mut Transaction<'_, Postgres>) -> Result<Option<Receipt>, sqlx::Error> {
    let row: Option<(Uuid, Uuid, Uuid, Option<Uuid>)> = sqlx::query_as(
        "SELECT firm_id, account_id, envelope_id, project_id FROM docusign_completion \
         WHERE status = 'pending' AND next_attempt_at <= now() \
         ORDER BY next_attempt_at, envelope_id LIMIT 1 FOR UPDATE SKIP LOCKED",
    )
    .fetch_optional(&mut **tx)
    .await?;
    Ok(
        row.map(|(firm_id, account_id, envelope_id, project_id)| Receipt {
            firm_id,
            account_id,
            envelope_id,
            project_id,
        }),
    )
}

pub enum Outcome {
    Assigned(Uuid),
    Filed(Uuid),
    Review(&'static str),
    Retry,
}

pub async fn finish(
    tx: &mut Transaction<'_, Postgres>,
    receipt: &Receipt,
    outcome: Outcome,
) -> Result<(), sqlx::Error> {
    let (status, project, reason, delay) = match outcome {
        Outcome::Assigned(project) => ("pending", Some(project), None, 0),
        Outcome::Filed(project) => ("filed", Some(project), None, 0),
        Outcome::Review(reason) => ("review", receipt.project_id, Some(reason), 0),
        Outcome::Retry => ("pending", receipt.project_id, None, 300),
    };
    sqlx::query(
        "UPDATE docusign_completion \
         SET status = $4, project_id = $5, review_reason = $6, \
         next_attempt_at = now() + make_interval(secs => $7) \
         WHERE firm_id = $1 AND account_id = $2 AND envelope_id = $3",
    )
    .bind(receipt.firm_id)
    .bind(receipt.account_id)
    .bind(receipt.envelope_id)
    .bind(status)
    .bind(project)
    .bind(reason)
    .bind(f64::from(delay))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn cursor(
    db: &PostgresDb,
    firm: Uuid,
    account: Uuid,
) -> Result<Option<DateTime<Utc>>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT checked_through FROM docusign_completion_cursor WHERE firm_id = $1 AND account_id = $2",
    )
    .bind(firm)
    .bind(account)
    .fetch_optional(db.pool())
    .await
}

pub async fn checkpoint(
    db: &PostgresDb,
    firm: Uuid,
    account: Uuid,
    through: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO docusign_completion_cursor (firm_id, account_id, checked_through) \
         VALUES ($1, $2, $3) ON CONFLICT (firm_id, account_id) DO UPDATE SET \
         checked_through = greatest(docusign_completion_cursor.checked_through, excluded.checked_through)",
    )
    .bind(firm)
    .bind(account)
    .bind(through)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// Review data contains only provider IDs and the reason matching stopped.
pub async fn review_queue(db: &PostgresDb, firm: Uuid) -> Result<Vec<(Uuid, String)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT envelope_id, coalesce(review_reason, 'review_required') FROM docusign_completion \
         WHERE firm_id = $1 AND status = 'review' ORDER BY created_at DESC LIMIT 100",
    )
    .bind(firm)
    .fetch_all(db.pool())
    .await
}

pub async fn claim_notice(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<Option<(Uuid, Uuid, Uuid, Uuid)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT firm_id, account_id, envelope_id, project_id FROM docusign_completion \
         WHERE status = 'filed' AND project_id IS NOT NULL AND notified_at IS NULL \
         AND next_attempt_at <= now() ORDER BY next_attempt_at, envelope_id \
         LIMIT 1 FOR UPDATE SKIP LOCKED",
    )
    .fetch_optional(&mut **tx)
    .await
}

pub async fn finish_notice(
    tx: &mut Transaction<'_, Postgres>,
    firm: Uuid,
    account: Uuid,
    envelope: Uuid,
    sent: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE docusign_completion \
         SET notified_at = CASE WHEN $4 THEN now() ELSE NULL END, \
         next_attempt_at = now() + interval '30 minutes' \
         WHERE firm_id = $1 AND account_id = $2 AND envelope_id = $3",
    )
    .bind(firm)
    .bind(account)
    .bind(envelope)
    .bind(sent)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// After a lawyer resolves a review item by adopting it, resume completion
/// from the Signature they linked to the Project.
pub async fn resume_adopted(
    db: &PostgresDb,
    firm: Uuid,
    account: Uuid,
    envelope: Uuid,
    project: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE docusign_completion SET status = 'pending', review_reason = NULL, \
         project_id = $4, next_attempt_at = now() \
         WHERE firm_id = $1 AND account_id = $2 AND envelope_id = $3 AND status = 'review'",
    )
    .bind(firm)
    .bind(account)
    .bind(envelope)
    .bind(project)
    .execute(db.pool())
    .await?;
    Ok(())
}

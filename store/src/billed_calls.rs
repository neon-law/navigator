//! Billed time for completed voice calls, stored relationally in PostgreSQL.
//!
//! One row per `CallSid`, written only from Twilio's final
//! `CallStatus=completed` event: the call's provider-reported duration
//! rounds UP to six-minute tenths of an hour — a one-second call bills one
//! tenth, a 6:01 call bills two. The row parks at the Firm level
//! (`project_id` is null) until a lawyer attributes the conversation to a
//! Project; attribution is the only mutation after the row lands and never
//! rewrites the call's facts. No foreign keys on purpose — the conversation
//! is authored in the store the webhook serves and attribution happens at
//! intake; either can lag the completed event, and billing must not fail
//! closed on a mirror race.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::postgres::PostgresDb;

// Both coordinators retain a SQL transaction while authority reads and Project
// projection acquire the same pool again. Admit locally before acquiring ANY
// connection, so distinct calls queue without exhausting all ten outer slots.
// Replicas retain their independent pool and existing per-Conversation SQL fence.
static CALL_COORDINATOR: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, thiserror::Error)]
pub enum BilledCallError {
    #[error("invalid billed call: {0}")]
    Invalid(&'static str),
    #[error("the call is already billed with different content")]
    Conflict,
    #[error("billed call was not found")]
    NotFound,
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
    #[error("PostgreSQL is unavailable")]
    Postgres(#[from] crate::postgres::PostgresError),
}

/// A completed call's billing row. Immutable except `project_id`:
/// attribution links the row to a Project and nothing else ever moves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BilledCall {
    /// Twilio's call identifier — the unique idempotency key.
    pub call_sid: String,
    /// The voice-channel [conversation](crate::inbox) the call ingested into.
    pub conversation_id: Uuid,
    /// The tenant the receiving number's Brand resolves to.
    pub firm_id: Uuid,
    /// The Project the call is attributed to; `None` parks the row at the
    /// Firm level, visible but unbillable.
    pub project_id: Option<Uuid>,
    /// Twilio's reported duration in seconds, from the completed event.
    pub duration_seconds: i64,
    /// Billed tenths of an hour — [`billed_tenths`] of `duration_seconds`.
    pub tenths: i64,
    /// The provider's own start time for the call, never this process's clock.
    pub call_started_at: DateTime<Utc>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewBilledCall {
    pub call_sid: String,
    pub conversation_id: Uuid,
    pub firm_id: Uuid,
    /// The provider's reported duration; a completed event without one is a
    /// zero-duration call and still bills — at zero tenths.
    pub duration_seconds: i64,
    pub call_started_at: DateTime<Utc>,
}

/// Billed tenths of an hour for a call of `duration_seconds`, rounding UP
/// to the six-minute unit: 0→0, 1→1, 359→1, 360→1, 361→2. A non-positive
/// duration bills zero — a missed or zero-duration call is recorded, never
/// skipped, and never bills negative time.
#[must_use]
pub fn billed_tenths(duration_seconds: i64) -> i64 {
    let secs = duration_seconds.max(0);
    secs / 360 + i64::from(secs % 360 != 0)
}

fn validate(input: &NewBilledCall) -> Result<(), BilledCallError> {
    if input.call_sid.trim().is_empty() || input.call_sid.len() > 64 {
        return Err(BilledCallError::Invalid("call_sid"));
    }
    if input.duration_seconds < 0 {
        return Err(BilledCallError::Invalid("duration_seconds"));
    }
    Ok(())
}

fn from_row(row: &sqlx::postgres::PgRow) -> Result<BilledCall, sqlx::Error> {
    Ok(BilledCall {
        call_sid: row.try_get("call_sid")?,
        conversation_id: row.try_get("conversation_id")?,
        firm_id: row.try_get("firm_id")?,
        project_id: row.try_get("project_id")?,
        duration_seconds: row.try_get("duration_seconds")?,
        tenths: row.try_get("tenths")?,
        call_started_at: row.try_get("call_started_at")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// The row's call facts: everything a redelivered completed event must agree
/// on. `project_id` is deliberately outside — attribution is the record's
/// only legal mutation, so a redelivery after attribution is a no-op that
/// returns the attributed row, not a conflict and not an un-attribution.
fn same_call(row: &BilledCall, input: &NewBilledCall) -> bool {
    row.conversation_id == input.conversation_id
        && row.firm_id == input.firm_id
        && row.duration_seconds == input.duration_seconds
        && row.tenths == billed_tenths(input.duration_seconds)
        && row.call_started_at == input.call_started_at
}

/// Record the billing for one completed call. A repeated identical event
/// returns the original row; the same `CallSid` with different call facts
/// is a loud conflict, never a silent overwrite — the original tenths stay
/// on the record and the mismatch surfaces to Twilio's redelivery.
pub async fn record(pg: &PostgresDb, input: &NewBilledCall) -> Result<BilledCall, BilledCallError> {
    let mut tx = pg.pool().begin().await?;
    let saved = record_on(&mut tx, input).await?;
    tx.commit().await?;
    Ok(saved)
}

async fn record_on(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    input: &NewBilledCall,
) -> Result<BilledCall, BilledCallError> {
    validate(input)?;
    let existing = sqlx::query("SELECT * FROM billed_call WHERE call_sid = $1 FOR UPDATE")
        .bind(input.call_sid.trim())
        .fetch_optional(&mut **tx)
        .await?;
    if let Some(row) = existing {
        let billed = from_row(&row)?;
        if same_call(&billed, input) {
            return Ok(billed);
        }
        return Err(BilledCallError::Conflict);
    }
    let row = sqlx::query(
        "INSERT INTO billed_call (call_sid, conversation_id, firm_id, project_id, duration_seconds, tenths, call_started_at) \
         VALUES ($1, $2, $3, NULL, $4, $5, $6) ON CONFLICT (call_sid) DO NOTHING RETURNING *",
    )
    .bind(input.call_sid.trim())
    .bind(input.conversation_id)
    .bind(input.firm_id)
    .bind(input.duration_seconds)
    .bind(billed_tenths(input.duration_seconds))
    .bind(input.call_started_at)
    .fetch_optional(&mut **tx)
    .await?;
    let row = match row {
        Some(row) => row,
        None => {
            sqlx::query("SELECT * FROM billed_call WHERE call_sid=$1 FOR UPDATE")
                .bind(input.call_sid.trim())
                .fetch_one(&mut **tx)
                .await?
        }
    };
    let billed = from_row(&row)?;
    if !same_call(&billed, input) {
        return Err(BilledCallError::Conflict);
    }
    Ok(billed)
}

/// Attribute a parked row to a Project. The call's facts — duration, tenths,
/// provider start time — are never touched: the audit trail shows the billing
/// as Twilio reported it and only the Project linkage moves.
pub async fn attribute(
    pg: &PostgresDb,
    call_sid: &str,
    project_id: Uuid,
) -> Result<BilledCall, BilledCallError> {
    let row = sqlx::query(
        "UPDATE billed_call SET project_id = $2, updated_at = now() WHERE call_sid = $1 RETURNING *",
    )
    .bind(call_sid.trim())
    .bind(project_id)
    .fetch_optional(&pg.0)
    .await?
    .ok_or(BilledCallError::NotFound)?;
    from_row(&row).map_err(BilledCallError::Database)
}

/// The billing row for a call, or `None` when that call never completed.
pub async fn find_by_call_sid(
    pg: &PostgresDb,
    call_sid: &str,
) -> Result<Option<BilledCall>, BilledCallError> {
    let row = sqlx::query("SELECT * FROM billed_call WHERE call_sid = $1")
        .bind(call_sid)
        .fetch_optional(&pg.0)
        .await?;
    row.map(|row| from_row(&row))
        .transpose()
        .map_err(BilledCallError::Database)
}

/// Record a completed provider callback against the Conversation's current
/// Project while serialized with initial attribution. A callback arriving after
/// attribution inherits that linkage rather than creating a new parked bill.
pub async fn record_for_conversation(
    pg: &PostgresDb,
    db: &crate::surreal::SurrealDb,
    input: &NewBilledCall,
) -> Result<BilledCall, BilledCallError> {
    let _coordinator = CALL_COORDINATOR.lock().await;
    let mut tx = pg.pool().begin().await?;
    sqlx::query("SET LOCAL lock_timeout = '5s'")
        .execute(&mut *tx)
        .await?;
    let acquired: bool =
        sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1, 168))")
            .bind(input.conversation_id.to_string())
            .fetch_one(&mut *tx)
            .await?;
    if !acquired {
        return Err(BilledCallError::Conflict);
    }
    let conversation = crate::inbox::conversation_by_id(db, input.conversation_id)
        .await
        .map_err(|_| BilledCallError::Invalid("conversation"))?
        .ok_or(BilledCallError::NotFound)?;
    if conversation.firm_id != input.firm_id || conversation.channel != crate::inbox::channel::VOICE
    {
        return Err(BilledCallError::Invalid("conversation"));
    }
    let mut saved = record_on(&mut tx, input).await?;
    if let Some(project) = conversation.project_id {
        let row = sqlx::query("UPDATE billed_call SET project_id=$2,updated_at=now() WHERE call_sid=$1 AND firm_id=$3 AND (project_id IS NULL OR project_id=$2) RETURNING *")
            .bind(&saved.call_sid).bind(project).bind(input.firm_id)
            .fetch_optional(&mut *tx).await?.ok_or(BilledCallError::Conflict)?;
        saved = from_row(&row)?;
    }
    tx.commit().await?;
    Ok(saved)
}

/// Initial Project attribution of completed calls or read-only Fi SMS history.
/// The advisory lock serializes retries and conflicting Project choices across
/// workers. The Inbox's same-Firm participation gate remains authoritative.
/// Billing follows a saved Inbox assignment, so an interrupted source-store
/// write is repairable by repeating the same Project; no new duration is inferred.
pub(crate) async fn assign_conversation(
    pg: &PostgresDb,
    db: &crate::surreal::SurrealDb,
    actor_id: Uuid,
    conversation_id: Uuid,
    project_id: Uuid,
) -> Result<crate::inbox::Assignment, crate::inbox::InboxError> {
    Box::pin(assign_conversation_inner(
        pg,
        db,
        actor_id,
        conversation_id,
        project_id,
        false,
    ))
    .await
}

pub(crate) async fn assign_conversation_for_triage_review(
    pg: &PostgresDb,
    db: &crate::surreal::SurrealDb,
    actor_id: Uuid,
    conversation_id: Uuid,
    project_id: Uuid,
) -> Result<crate::inbox::Assignment, crate::inbox::InboxError> {
    Box::pin(assign_conversation_inner(
        pg,
        db,
        actor_id,
        conversation_id,
        project_id,
        true,
    ))
    .await
}

async fn assign_conversation_inner(
    pg: &PostgresDb,
    db: &crate::surreal::SurrealDb,
    actor_id: Uuid,
    conversation_id: Uuid,
    project_id: Uuid,
    triage_review: bool,
) -> Result<crate::inbox::Assignment, crate::inbox::InboxError> {
    use crate::inbox::InboxError;
    let failed = |error: sqlx::Error| InboxError::Db(error.to_string());
    let _coordinator = CALL_COORDINATOR.lock().await;
    let mut tx = pg.pool().begin().await.map_err(failed)?;
    sqlx::query("SET LOCAL lock_timeout = '5s'")
        .execute(&mut *tx)
        .await
        .map_err(failed)?;
    let acquired: bool =
        sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1, 168))")
            .bind(conversation_id.to_string())
            .fetch_one(&mut *tx)
            .await
            .map_err(failed)?;
    if !acquired {
        return Err(InboxError::Db(
            "call attribution is busy; retry the same Project".into(),
        ));
    }
    let conversation = crate::inbox::conversation_by_id(db, conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    let project = crate::projects::find_by_id(db, project_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    let fi_sms = conversation.channel == crate::inbox::channel::SMS
        && crate::fi_import::is_account(&conversation.provider_account);
    if (conversation.channel != crate::inbox::channel::VOICE && !fi_sms)
        || project.firm_id != Some(conversation.firm_id)
    {
        return Err(InboxError::CrossFirm);
    }
    if conversation.project_id.is_some_and(|id| id != project_id) {
        return Err(InboxError::Denied);
    }
    let rows = sqlx::query(
        "SELECT firm_id,project_id FROM billed_call WHERE conversation_id=$1 FOR UPDATE",
    )
    .bind(conversation_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(failed)?;
    for row in rows {
        if row.try_get::<Uuid, _>("firm_id").map_err(failed)? != conversation.firm_id
            || row
                .try_get::<Option<Uuid>, _>("project_id")
                .map_err(failed)?
                .is_some_and(|id| id != project_id)
        {
            return Err(InboxError::Denied);
        }
    }
    // This retains actor, destination Project, participation and same-Firm checks
    // for both source and PostgreSQL-authoritative Inbox modes. Repeating it also
    // repairs a partially saved Communication projection before billing completes.
    let assignment = if triage_review {
        Box::pin(crate::inbox::assign_for_triage_review_unbilled(
            db,
            actor_id,
            conversation_id,
            project_id,
        ))
        .await?
    } else {
        Box::pin(crate::inbox::assign_unbilled(
            db,
            actor_id,
            conversation_id,
            project_id,
        ))
        .await?
    };
    sqlx::query("UPDATE billed_call SET project_id=$2,updated_at=now() WHERE conversation_id=$1 AND firm_id=$3 AND project_id IS NULL")
        .bind(conversation_id).bind(project_id).bind(conversation.firm_id)
        .execute(&mut *tx).await.map_err(failed)?;
    tx.commit().await.map_err(failed)?;
    Ok(assignment)
}

/// Billing rows whose Conversations are visible in the actor's Firm Inbox.
/// The filtered ID list comes from the authority dispatcher, never a caller's
/// arbitrary Firm or Brand key.
pub async fn visible_calls(
    pg: &PostgresDb,
    db: &crate::surreal::SurrealDb,
    actor_id: Uuid,
) -> Result<Vec<BilledCall>, crate::inbox::InboxError> {
    let conversations = crate::inbox::list_intake(db, actor_id).await?;
    let mut calls = Vec::new();
    for conversation in conversations
        .into_iter()
        .filter(|c| c.channel == crate::inbox::channel::VOICE)
    {
        let rows = sqlx::query("SELECT * FROM billed_call WHERE conversation_id=$1 AND firm_id=$2 ORDER BY call_started_at DESC,call_sid")
            .bind(conversation.id).bind(conversation.firm_id).fetch_all(pg.pool()).await
            .map_err(|error| crate::inbox::InboxError::Db(error.to_string()))?;
        for row in rows {
            calls.push(
                from_row(&row).map_err(|error| crate::inbox::InboxError::Db(error.to_string()))?,
            );
        }
    }
    calls.sort_by(|a, b| {
        b.call_started_at
            .cmp(&a.call_started_at)
            .then_with(|| a.call_sid.cmp(&b.call_sid))
    });
    Ok(calls)
}

/// One unassigned terminal call, including missed calls with no billing row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParkedCallMetadata {
    pub conversation_id: Uuid,
    pub provider_message_id: String,
    pub occurred_at: DateTime<Utc>,
    pub duration_seconds: Option<i64>,
    pub tenths: Option<i64>,
}

/// Firm-visible parked terminal calls. Missing billing facts stay absent;
/// missed calls are never converted into zero-duration completed calls.
pub async fn parked_calls(
    pg: &PostgresDb,
    db: &crate::surreal::SurrealDb,
    actor: Uuid,
) -> Result<Vec<ParkedCallMetadata>, crate::inbox::InboxError> {
    let mut parked = Vec::new();
    for conversation in crate::inbox::list_intake(db, actor)
        .await?
        .into_iter()
        .filter(|c| c.channel == crate::inbox::channel::VOICE)
    {
        let firm_id = conversation.firm_id;
        let conversation = crate::inbox::conversation_by_id(db, conversation.id)
            .await?
            .ok_or(crate::inbox::InboxError::NotFound)?;
        if conversation.firm_id != firm_id {
            return Err(crate::inbox::InboxError::Denied);
        }
        for message in crate::inbox::messages_for_reporting(db, conversation.id)
            .await?
            .into_iter()
            .filter(|m| m.direction != crate::inbox::direction::INTERNAL)
        {
            let Some(sid) = message.provider_message_id else {
                continue;
            };
            let bill = if crate::fi_import::is_account(&conversation.provider_account) {
                None
            } else {
                find_by_call_sid(pg, &sid)
                    .await
                    .map_err(|error| crate::inbox::InboxError::Db(error.to_string()))?
            };
            if bill.as_ref().is_some_and(|b| {
                b.firm_id != conversation.firm_id || b.conversation_id != conversation.id
            }) {
                return Err(crate::inbox::InboxError::Denied);
            }
            if conversation.project_id.is_some()
                && bill.as_ref().is_none_or(|b| b.project_id.is_some())
            {
                continue;
            }
            parked.push(ParkedCallMetadata {
                conversation_id: conversation.id,
                provider_message_id: sid,
                occurred_at: DateTime::parse_from_rfc3339(&message.occurred_at)
                    .map_err(|_| {
                        crate::inbox::InboxError::Db("invalid call occurrence time".into())
                    })?
                    .with_timezone(&Utc),
                duration_seconds: bill.as_ref().map(|b| b.duration_seconds).or_else(|| {
                    crate::fi_import::is_account(&conversation.provider_account)
                        .then(|| crate::fi_import::metadata(&message.body))
                        .flatten()
                        .and_then(|metadata| metadata.duration_seconds)
                }),
                tenths: bill.as_ref().map(|b| b.tenths),
            });
        }
    }
    parked.sort_by(|a, b| {
        b.occurred_at
            .cmp(&a.occurred_at)
            .then_with(|| a.provider_message_id.cmp(&b.provider_message_id))
    });
    Ok(parked)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenths_round_up_to_the_six_minute_unit() {
        let cases: &[(i64, i64)] = &[
            (0, 0),
            (1, 1),
            (42, 1),
            (359, 1),
            (360, 1),
            (361, 2),
            (720, 2),
            (721, 3),
            (-1, 0),
            (-600, 0),
            (i64::MIN, 0),
            (i64::MAX, i64::MAX / 360 + 1),
        ];
        for (seconds, tenths) in cases {
            assert_eq!(
                billed_tenths(*seconds),
                *tenths,
                "billed_tenths({seconds}) must be {tenths}"
            );
        }
        assert_eq!(billed_tenths(0), 0, "a zero-duration call bills zero");
        assert_eq!(
            billed_tenths(-1),
            0,
            "a negative duration clamps to zero, never bills negative"
        );
        assert_eq!(
            billed_tenths(361),
            2,
            "one second past the unit rounds up: 6:01 bills two tenths"
        );
    }

    #[test]
    fn validation_rejects_an_unidentifiable_or_impossible_call() {
        let call = |duration_seconds: i64, call_sid: &str| NewBilledCall {
            call_sid: call_sid.into(),
            conversation_id: Uuid::now_v7(),
            firm_id: Uuid::now_v7(),
            duration_seconds,
            call_started_at: Utc::now(),
        };
        assert!(validate(&call(42, "CAacc0123456789abcdef0123456789abcdef")).is_ok());
        assert!(matches!(
            validate(&call(42, "  ")),
            Err(BilledCallError::Invalid("call_sid"))
        ));
        assert!(matches!(
            validate(&call(42, &"C".repeat(65))),
            Err(BilledCallError::Invalid("call_sid"))
        ));
        assert!(matches!(
            validate(&call(-1, "CA1")),
            Err(BilledCallError::Invalid("duration_seconds"))
        ));
    }
}

#[cfg(test)]
#[path = "billed_calls_attribution_tests.rs"]
mod attribution_tests;

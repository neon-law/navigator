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
    secs.saturating_add(359) / 360
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
    validate(input)?;
    let mut tx = pg.0.begin().await?;
    let existing = sqlx::query("SELECT * FROM billed_call WHERE call_sid = $1 FOR UPDATE")
        .bind(input.call_sid.trim())
        .fetch_optional(&mut *tx)
        .await?;
    if let Some(row) = existing {
        let billed = from_row(&row)?;
        if same_call(&billed, input) {
            tx.commit().await?;
            return Ok(billed);
        }
        return Err(BilledCallError::Conflict);
    }
    let row = sqlx::query(
        "INSERT INTO billed_call (call_sid, conversation_id, firm_id, project_id, duration_seconds, tenths, call_started_at) \
         VALUES ($1, $2, $3, NULL, $4, $5, $6) RETURNING *",
    )
    .bind(input.call_sid.trim())
    .bind(input.conversation_id)
    .bind(input.firm_id)
    .bind(input.duration_seconds)
    .bind(billed_tenths(input.duration_seconds))
    .bind(input.call_started_at)
    .fetch_one(&mut *tx)
    .await?;
    let billed = from_row(&row)?;
    tx.commit().await?;
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
            (i64::MAX, i64::MAX / 360),
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

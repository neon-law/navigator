//! Attributed attorney time on a matter, stored relationally in PostgreSQL.
//!
//! A Time Entry records work one Person performed on a Project — billable or
//! non-billable, logged by hand or captured by a Timer (`crate::timers`) —
//! together with the hourly rate snapshotted at creation,
//! so a later fee-structure edit can never reprice captured work (the
//! `notation_meter_entry` precedent). Minutes are integers and money is
//! integer cents, never floats. A Time Entry is work done and priced; a
//! Project Task (`crate::tasks`) is work still to do. See
//! `docs/glossary/time-entry.md`.
//!
//! The model follows the `project_costs.rs` pattern: PostgreSQL-native, typed
//! errors, and an event-sourced audit trail whose writes the database itself
//! enforces (a mutation without an actor and event kind is refused; events
//! are immutable; rows are retained — a write-off, never a delete).
use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Row, Transaction};
use uuid::Uuid;

use crate::postgres::PostgresDb;

/// One day of work, in whole minutes — the single-entry ceiling. A Timer
/// left running past it is refused at stop; discard it or log by hand.
pub const MAX_MINUTES_PER_DAY: i32 = 1_440;

#[derive(Debug, thiserror::Error)]
pub enum TimeEntryError {
    #[error("invalid Time Entry: {0}")]
    Invalid(&'static str),
    #[error("the attorney must be a participant on the Project")]
    NotParticipant,
    #[error("the entries are already invoiced under a different Xero invoice")]
    Conflict,
    #[error("Time Entry was not found")]
    NotFound,
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
    #[error("PostgreSQL is unavailable")]
    Postgres(#[from] crate::postgres::PostgresError),
}

/// Where a Time Entry came from: captured by a durable Timer or logged by
/// hand. Stored as its word; the distinction never changes the math.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeEntrySource {
    Timer,
    Manual,
}

impl TimeEntrySource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Timer => "timer",
            Self::Manual => "manual",
        }
    }
}

/// What an entry owes: nothing yet, a Xero invoice, or a deliberate write-off
/// that keeps its reason. The database enforces the evidence columns agree
/// with the status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BillingStatus {
    Unbilled,
    Invoiced { xero_invoice_id: String },
    WrittenOff { reason: String },
}

impl BillingStatus {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Unbilled => "unbilled",
            Self::Invoiced { .. } => "invoiced",
            Self::WrittenOff { .. } => "written_off",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeEntry {
    pub id: Uuid,
    pub project_id: Uuid,
    pub attorney_id: Uuid,
    pub performed_on: NaiveDate,
    pub minutes: i32,
    pub description: String,
    pub billable: bool,
    /// Snapshotted from the attorney's fee structure at creation; a later
    /// rate edit never reprices this row.
    pub rate_cents_per_hour: i32,
    pub source: TimeEntrySource,
    pub billing_status: BillingStatus,
    pub inserted_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeEntryEvent {
    pub id: Uuid,
    pub actor_person_id: Uuid,
    pub event_kind: String,
    pub before_state: Option<serde_json::Value>,
    pub after_state: serde_json::Value,
    pub occurred_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewTimeEntry {
    /// A participant on the same Project; refused otherwise.
    pub attorney_id: Uuid,
    pub performed_on: NaiveDate,
    /// Whole minutes of work, 1..=1440 — one day's work on one entry.
    pub minutes: i32,
    pub description: String,
    pub billable: bool,
    /// The snapshot: integer cents per hour, frozen at creation.
    pub rate_cents_per_hour: i32,
    pub source: TimeEntrySource,
}

#[must_use]
pub fn entry_amount_cents(minutes: i32, rate_cents_per_hour: i32) -> i64 {
    // i32 * i32 fits i64 with room to spare; the fractional cent truncates so
    // the composed invoice sums the same per-entry amounts WIP shows.
    (i64::from(minutes) * i64::from(rate_cents_per_hour)) / 60
}

fn parse_source(raw: &str) -> Result<TimeEntrySource, TimeEntryError> {
    match raw {
        "timer" => Ok(TimeEntrySource::Timer),
        "manual" => Ok(TimeEntrySource::Manual),
        _ => Err(TimeEntryError::Invalid("source")),
    }
}

fn parse_billing_status(
    status: &str,
    xero_invoice_id: Option<String>,
    written_off_reason: Option<String>,
) -> Result<BillingStatus, TimeEntryError> {
    match status {
        "unbilled" => Ok(BillingStatus::Unbilled),
        "invoiced" => xero_invoice_id
            .map(|xero_invoice_id| BillingStatus::Invoiced { xero_invoice_id })
            .ok_or(TimeEntryError::Invalid("billing status")),
        "written_off" => written_off_reason
            .map(|reason| BillingStatus::WrittenOff { reason })
            .ok_or(TimeEntryError::Invalid("billing status")),
        _ => Err(TimeEntryError::Invalid("billing status")),
    }
}

fn from_row(row: &sqlx::postgres::PgRow) -> Result<TimeEntry, TimeEntryError> {
    let status: String = row.try_get("billing_status")?;
    let xero_invoice_id: Option<String> = row.try_get("xero_invoice_id")?;
    let written_off_reason: Option<String> = row.try_get("written_off_reason")?;
    Ok(TimeEntry {
        id: row.try_get("id")?,
        project_id: row.try_get("project_id")?,
        attorney_id: row.try_get("attorney_id")?,
        performed_on: row.try_get("performed_on")?,
        minutes: row.try_get("minutes")?,
        description: row.try_get("description")?,
        billable: row.try_get("billable")?,
        rate_cents_per_hour: row.try_get("rate_cents_per_hour")?,
        source: parse_source(&row.try_get::<String, _>("source")?)?,
        billing_status: parse_billing_status(&status, xero_invoice_id, written_off_reason)?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn validate(input: &NewTimeEntry) -> Result<(), TimeEntryError> {
    if !(1..=MAX_MINUTES_PER_DAY).contains(&input.minutes) {
        return Err(TimeEntryError::Invalid("minutes"));
    }
    if input.description.trim().is_empty() || input.description.trim().len() > 2000 {
        return Err(TimeEntryError::Invalid("description"));
    }
    if input.rate_cents_per_hour < 0 {
        return Err(TimeEntryError::Invalid("rate"));
    }
    Ok(())
}

async fn set_audit_context(
    tx: &mut Transaction<'_, sqlx::Postgres>,
    actor_id: Uuid,
    kind: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "SELECT set_config('navigator.time_entry_actor',$1,true), \
         set_config('navigator.time_entry_event_kind',$2,true)",
    )
    .bind(actor_id.to_string())
    .bind(kind)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// The assignee must already hold a participation row on the Project. The
/// composite foreign key refuses an outsider at the database too; this check
/// gives the caller a typed denial instead of a constraint violation.
async fn require_participant(
    tx: &mut Transaction<'_, sqlx::Postgres>,
    project_id: Uuid,
    person_id: Uuid,
) -> Result<(), TimeEntryError> {
    sqlx::query("SELECT id FROM person_project_role WHERE project_id=$1 AND person_id=$2")
        .bind(project_id)
        .bind(person_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(TimeEntryError::NotParticipant)?;
    Ok(())
}

/// Insert one entry inside a caller's transaction, starting `unbilled`, and
/// write its `created` audit event. Callers that already hold a transaction
/// (the Timer's atomic stop) compose this so the entry and their own writes
/// commit together. The participation check is the caller's: `create` makes
/// it typed, and a Timer row already carries the composite foreign key.
pub(crate) async fn create_in_transaction(
    tx: &mut Transaction<'_, sqlx::Postgres>,
    project_id: Uuid,
    actor_id: Uuid,
    input: &NewTimeEntry,
) -> Result<Uuid, TimeEntryError> {
    validate(input)?;
    let id = Uuid::new_v4();
    set_audit_context(tx, actor_id, "created").await?;
    sqlx::query(
        "INSERT INTO time_entry(id,project_id,attorney_id,performed_on,minutes,description,billable,\
         rate_cents_per_hour,source,billing_status,inserted_at,updated_at) \
         VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,'unbilled',$10,$10)",
    )
    .bind(id)
    .bind(project_id)
    .bind(input.attorney_id)
    .bind(input.performed_on)
    .bind(input.minutes)
    .bind(input.description.trim())
    .bind(input.billable)
    .bind(input.rate_cents_per_hour)
    .bind(input.source.as_str())
    .bind(Utc::now())
    .execute(&mut **tx)
    .await?;
    Ok(id)
}

/// Log time on a Project. The Project must already be mirrored to PostgreSQL;
/// the row starts `unbilled` with its rate frozen.
pub async fn create(
    pg: &PostgresDb,
    project_id: Uuid,
    actor_id: Uuid,
    input: &NewTimeEntry,
) -> Result<TimeEntry, TimeEntryError> {
    let mut tx = pg.0.begin().await?;
    sqlx::query("SELECT id FROM project WHERE id=$1")
        .bind(project_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(TimeEntryError::Invalid(
            "Project is not mirrored to PostgreSQL",
        ))?;
    require_participant(&mut tx, project_id, input.attorney_id).await?;
    let id = create_in_transaction(&mut tx, project_id, actor_id, input).await?;
    tx.commit().await?;
    fetch(pg, id).await
}

/// Flip unbilled entries to `invoiced` under one Xero invoice id. Replaying
/// the same call is a no-op — an entry already invoiced under the same id is
/// left alone — so a retried generation cannot double-mark; the same entries
/// under a different invoice id is a `Conflict`. Returns every entry named,
/// in the order given.
pub async fn mark_invoiced(
    pg: &PostgresDb,
    entry_ids: &[Uuid],
    actor_id: Uuid,
    xero_invoice_id: &str,
) -> Result<Vec<TimeEntry>, TimeEntryError> {
    if entry_ids.is_empty() {
        return Err(TimeEntryError::Invalid("entries"));
    }
    if xero_invoice_id.trim().is_empty() || xero_invoice_id.len() > 100 {
        return Err(TimeEntryError::Invalid("invoice id"));
    }
    let mut tx = pg.0.begin().await?;
    let before = sqlx::query(
        "SELECT id,project_id,attorney_id,performed_on,minutes,description,billable,\
         rate_cents_per_hour,source,billing_status,xero_invoice_id,written_off_reason,\
         inserted_at,updated_at FROM time_entry WHERE id = ANY($1) \
         ORDER BY performed_on, id FOR UPDATE",
    )
    .bind(entry_ids)
    .fetch_all(&mut *tx)
    .await?;
    if before.len() != entry_ids.len() {
        return Err(TimeEntryError::NotFound);
    }
    let rows = before
        .iter()
        .map(from_row)
        .collect::<Result<Vec<_>, _>>()?;
    for entry in &rows {
        match &entry.billing_status {
            // Unbilled: nothing to check yet.
            BillingStatus::Unbilled => {}
            // Already invoiced under the same invoice id: the replay no-op.
            BillingStatus::Invoiced { xero_invoice_id: id } if id.as_str() == xero_invoice_id => {}
            // Invoiced under a different invoice, or written off: the caller
            // is composing a different batch than the one that marked these.
            BillingStatus::Invoiced { .. } => return Err(TimeEntryError::Conflict),
            BillingStatus::WrittenOff { .. } => {
                return Err(TimeEntryError::Invalid("entry is already resolved"));
            }
        }
    }
    let markable = rows
        .iter()
        .filter(|entry| entry.billing_status.as_str() == "unbilled")
        .count();
    if markable > 0 {
        set_audit_context(&mut tx, actor_id, "invoiced").await?;
        sqlx::query(
            "UPDATE time_entry SET billing_status='invoiced',xero_invoice_id=$2,updated_at=$3 \
             WHERE id = ANY($1) AND billing_status='unbilled'",
        )
        .bind(entry_ids)
        .bind(xero_invoice_id)
        .bind(Utc::now())
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    let mut entries = Vec::with_capacity(entry_ids.len());
    for entry_id in entry_ids {
        entries.push(fetch(pg, *entry_id).await?);
    }
    Ok(entries)
}

/// Resolve an unbilled entry as a deliberate write-off, retaining the reason.
pub async fn write_off(
    pg: &PostgresDb,
    id: Uuid,
    actor_id: Uuid,
    reason: &str,
) -> Result<TimeEntry, TimeEntryError> {
    if reason.trim().is_empty() || reason.trim().len() > 1000 {
        return Err(TimeEntryError::Invalid("write-off reason"));
    }
    let mut tx = pg.0.begin().await?;
    let before = fetch_for_update(&mut tx, id).await?;
    if before.billing_status.as_str() != "unbilled" {
        return Err(TimeEntryError::Invalid("entry is already resolved"));
    }
    set_audit_context(&mut tx, actor_id, "written_off").await?;
    sqlx::query(
        "UPDATE time_entry SET billing_status='written_off',written_off_reason=$2,updated_at=$3 \
         WHERE id=$1",
    )
    .bind(id)
    .bind(reason.trim())
    .bind(Utc::now())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    fetch(pg, id).await
}

/// A Project's unbilled billable time — what an invoice would draw from,
/// oldest work first.
pub async fn unbilled_billable(
    pg: &PostgresDb,
    project_id: Uuid,
) -> Result<Vec<TimeEntry>, TimeEntryError> {
    sqlx::query(
        "SELECT id,project_id,attorney_id,performed_on,minutes,description,billable,\
         rate_cents_per_hour,source,billing_status,xero_invoice_id,written_off_reason,\
         inserted_at,updated_at FROM time_entry \
         WHERE project_id=$1 AND billable AND billing_status='unbilled' \
         ORDER BY performed_on, id",
    )
    .bind(project_id)
    .fetch_all(&pg.0)
    .await?
    .iter()
    .map(from_row)
    .collect()
}

/// A Project's entries, newest work first.
pub async fn list_for_project(
    pg: &PostgresDb,
    project_id: Uuid,
) -> Result<Vec<TimeEntry>, TimeEntryError> {
    sqlx::query(
        "SELECT id,project_id,attorney_id,performed_on,minutes,description,billable,\
         rate_cents_per_hour,source,billing_status,xero_invoice_id,written_off_reason,\
         inserted_at,updated_at FROM time_entry WHERE project_id=$1 \
         ORDER BY performed_on DESC, id",
    )
    .bind(project_id)
    .fetch_all(&pg.0)
    .await?
    .iter()
    .map(from_row)
    .collect()
}

pub async fn find_by_id(pg: &PostgresDb, id: Uuid) -> Result<TimeEntry, TimeEntryError> {
    fetch(pg, id).await
}

/// Read the immutable audit history of one Time Entry.
pub async fn list_events(pg: &PostgresDb, id: Uuid) -> Result<Vec<TimeEntryEvent>, TimeEntryError> {
    let rows = sqlx::query(
        "SELECT id,actor_person_id,event_kind,before_state,after_state,occurred_at \
         FROM time_entry_events WHERE time_entry_id=$1 ORDER BY occurred_at,id",
    )
    .bind(id)
    .fetch_all(&pg.0)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(TimeEntryEvent {
                id: row.try_get("id")?,
                actor_person_id: row.try_get("actor_person_id")?,
                event_kind: row.try_get("event_kind")?,
                before_state: row
                    .try_get::<Option<sqlx::types::Json<serde_json::Value>>, _>("before_state")?
                    .map(|json| json.0),
                after_state: row
                    .try_get::<sqlx::types::Json<serde_json::Value>, _>("after_state")?
                    .0,
                occurred_at: row.try_get("occurred_at")?,
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(Into::into)
}

async fn fetch_for_update(
    tx: &mut Transaction<'_, sqlx::Postgres>,
    id: Uuid,
) -> Result<TimeEntry, TimeEntryError> {
    sqlx::query(
        "SELECT id,project_id,attorney_id,performed_on,minutes,description,billable,\
         rate_cents_per_hour,source,billing_status,xero_invoice_id,written_off_reason,\
         inserted_at,updated_at FROM time_entry WHERE id=$1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?
    .as_ref()
    .map(from_row)
    .transpose()?
    .ok_or(TimeEntryError::NotFound)
}

async fn fetch(pg: &PostgresDb, id: Uuid) -> Result<TimeEntry, TimeEntryError> {
    sqlx::query(
        "SELECT id,project_id,attorney_id,performed_on,minutes,description,billable,\
         rate_cents_per_hour,source,billing_status,xero_invoice_id,written_off_reason,\
         inserted_at,updated_at FROM time_entry WHERE id=$1",
    )
    .bind(id)
    .fetch_optional(&pg.0)
    .await?
    .as_ref()
    .map(from_row)
    .transpose()?
    .ok_or(TimeEntryError::NotFound)
}

#[cfg(test)]
mod tests {
    use super::{entry_amount_cents, parse_source, validate, NewTimeEntry, TimeEntryError, TimeEntrySource};
    use chrono::NaiveDate;
    use uuid::Uuid;

    fn input() -> NewTimeEntry {
        NewTimeEntry {
            attorney_id: Uuid::new_v4(),
            performed_on: NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(),
            minutes: 90,
            description: "Drafted the motion to compel".into(),
            billable: true,
            rate_cents_per_hour: 25_000,
            source: TimeEntrySource::Manual,
        }
    }

    #[test]
    fn accepts_a_valid_entry() {
        assert!(validate(&input()).is_ok());
    }

    #[test]
    fn refuses_minutes_outside_one_day() {
        for minutes in [0, -1, 1441] {
            let mut value = input();
            value.minutes = minutes;
            assert!(matches!(validate(&value), Err(TimeEntryError::Invalid("minutes"))));
        }
        let mut value = input();
        value.minutes = 1440;
        assert!(validate(&value).is_ok());
    }

    #[test]
    fn refuses_a_blank_or_oversized_description() {
        let mut value = input();
        value.description = "   ".into();
        assert!(matches!(
            validate(&value),
            Err(TimeEntryError::Invalid("description"))
        ));
        let mut value = input();
        value.description = "x".repeat(2001);
        assert!(matches!(
            validate(&value),
            Err(TimeEntryError::Invalid("description"))
        ));
    }

    #[test]
    fn refuses_a_negative_rate() {
        let mut value = input();
        value.rate_cents_per_hour = -1;
        assert!(matches!(validate(&value), Err(TimeEntryError::Invalid("rate"))));
        let mut value = input();
        value.rate_cents_per_hour = 0;
        assert!(validate(&value).is_ok(), "a pro bono rate is legitimate");
    }

    #[test]
    fn line_amounts_stay_in_integer_cents() {
        assert_eq!(entry_amount_cents(60, 25_000), 25_000);
        assert_eq!(entry_amount_cents(90, 25_000), 37_500);
        assert_eq!(entry_amount_cents(1, 25_000), 416, "the fractional cent truncates");
        assert_eq!(entry_amount_cents(30, 12_345), 6_172);
        assert_eq!(entry_amount_cents(120, 0), 0);
    }

    #[test]
    fn source_round_trips_through_its_stored_word() {
        for source in [TimeEntrySource::Timer, TimeEntrySource::Manual] {
            assert!(matches!(parse_source(source.as_str()), Ok(parsed) if parsed == source));
        }
        assert!(matches!(
            parse_source("stopwatch"),
            Err(TimeEntryError::Invalid("source"))
        ));
    }

    // The remaining assertions need a live PostgreSQL: participant-only
    // attribution, the invoice replay contract, and the trigger-written audit
    // rows. They share the canonical-schema fixture used across the store
    // crate.
    mod database {
        use super::super::{
            create, list_events, list_for_project, mark_invoiced, unbilled_billable, write_off,
            BillingStatus, NewTimeEntry, TimeEntryError, TimeEntrySource,
        };
        use crate::postgres::PostgresDb;
        use crate::postgres_mirror::tests::{connect, fresh_schema};
        use sqlx::Row;
        use uuid::Uuid;

        struct Seed {
            project_id: Uuid,
            attorney: Uuid,
            outsider: Uuid,
        }

        async fn person(pg: &PostgresDb, name: &str) -> Uuid {
            let unique = Uuid::new_v4().simple().to_string();
            let id: Uuid = sqlx::query(
                "INSERT INTO person (id,name,email,role) VALUES ($1,$2,$3,'lawyer') RETURNING id",
            )
            .bind(Uuid::new_v4())
            .bind(format!("Synthetic {name}"))
            .bind(format!("{unique}@example.com"))
            .fetch_one(pg.pool())
            .await
            .unwrap()
            .get(0);
            id
        }

        async fn seed(pg: &PostgresDb) -> Seed {
            let jurisdiction: Uuid = sqlx::query(
                "INSERT INTO jurisdiction (id,name,code) VALUES ($1,'Synthetic Time Jurisdiction','TME') RETURNING id",
            )
            .bind(Uuid::new_v4())
            .fetch_one(pg.pool())
            .await
            .unwrap()
            .get(0);
            let entity_type: Uuid = sqlx::query(
                "INSERT INTO entity_type (id,name) VALUES ($1,'Synthetic Time Entity Type') RETURNING id",
            )
            .bind(Uuid::new_v4())
            .fetch_one(pg.pool())
            .await
            .unwrap()
            .get(0);
            let entity: Uuid = sqlx::query(
                "INSERT INTO entity (id,name,entity_type_id,jurisdiction_id) VALUES ($1,'Synthetic Time Entity',$2,$3) RETURNING id",
            )
            .bind(Uuid::new_v4())
            .bind(entity_type)
            .bind(jurisdiction)
            .fetch_one(pg.pool())
            .await
            .unwrap()
            .get(0);
            let project_id: Uuid = sqlx::query(
                "INSERT INTO project (id,code,name,status,entity_id) VALUES ($1,$2,'Synthetic Time Matter','open',$3) RETURNING id",
            )
            .bind(Uuid::new_v4())
            .bind(format!("time-{}", Uuid::new_v4().simple()))
            .bind(entity)
            .fetch_one(pg.pool())
            .await
            .unwrap()
            .get(0);
            let attorney = person(pg, "Attorney").await;
            let outsider = person(pg, "Outsider").await;
            sqlx::query(
                "INSERT INTO person_project_role (person_id,project_id,participation) VALUES ($1,$2,'lawyer')",
            )
            .bind(attorney)
            .bind(project_id)
            .execute(pg.pool())
            .await
            .unwrap();
            Seed {
                project_id,
                attorney,
                outsider,
            }
        }

        fn entry(attorney: Uuid, minutes: i32, billable: bool) -> NewTimeEntry {
            NewTimeEntry {
                attorney_id: attorney,
                performed_on: chrono::NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(),
                minutes,
                description: "Drafted the motion to compel".into(),
                billable,
                rate_cents_per_hour: 25_000,
                source: TimeEntrySource::Manual,
            }
        }

        #[tokio::test]
        async fn attribution_requires_a_project_participant() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let seed = seed(&pg).await;
            let actor = person(&pg, "Actor").await;

            assert!(matches!(
                create(&pg, seed.project_id, actor, &entry(seed.outsider, 60, true)).await,
                Err(TimeEntryError::NotParticipant)
            ));

            let created = create(&pg, seed.project_id, actor, &entry(seed.attorney, 90, true))
                .await
                .unwrap();
            assert_eq!(created.attorney_id, seed.attorney);
            assert_eq!(created.minutes, 90);
            assert_eq!(created.rate_cents_per_hour, 25_000, "the rate is snapshotted");
            assert_eq!(created.source, TimeEntrySource::Manual);
            assert_eq!(created.billing_status, BillingStatus::Unbilled);
        }

        #[tokio::test]
        async fn unbilled_billable_omits_resolved_and_non_billable_work() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let seed = seed(&pg).await;
            let actor = person(&pg, "Actor").await;

            let kept = create(&pg, seed.project_id, actor, &entry(seed.attorney, 60, true))
                .await
                .unwrap();
            let invoiced = create(&pg, seed.project_id, actor, &entry(seed.attorney, 30, true))
                .await
                .unwrap();
            let non_billable = create(&pg, seed.project_id, actor, &entry(seed.attorney, 45, false))
                .await
                .unwrap();
            let written_off = create(&pg, seed.project_id, actor, &entry(seed.attorney, 15, true))
                .await
                .unwrap();

            mark_invoiced(&pg, &[invoiced.id], actor, "inv-1").await.unwrap();
            write_off(&pg, written_off.id, actor, "Included in the flat fee")
                .await
                .unwrap();
            drop(non_billable);

            let outstanding = unbilled_billable(&pg, seed.project_id).await.unwrap();
            assert_eq!(outstanding.len(), 1, "only the unbilled billable entry remains");
            assert_eq!(outstanding[0].id, kept.id);
        }

        #[tokio::test]
        async fn invoicing_flips_with_the_xero_id_and_replays_cleanly() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let seed = seed(&pg).await;
            let actor = person(&pg, "Actor").await;
            let first = create(&pg, seed.project_id, actor, &entry(seed.attorney, 60, true))
                .await
                .unwrap();
            let second = create(&pg, seed.project_id, actor, &entry(seed.attorney, 30, true))
                .await
                .unwrap();

            let marked = mark_invoiced(&pg, &[first.id, second.id], actor, "inv-42")
                .await
                .unwrap();
            assert_eq!(marked.len(), 2);
            for entry in &marked {
                assert_eq!(
                    entry.billing_status,
                    BillingStatus::Invoiced {
                        xero_invoice_id: "inv-42".into()
                    }
                );
            }

            // A replay under the same invoice id changes nothing.
            let events_after_first_pass = list_events(&pg, first.id).await.unwrap().len();
            let replayed = mark_invoiced(&pg, &[first.id, second.id], actor, "inv-42")
                .await
                .unwrap();
            assert_eq!(replayed.len(), 2);
            assert_eq!(
                list_events(&pg, first.id).await.unwrap().len(),
                events_after_first_pass,
                "the replay appends no event"
            );

            // The same entries under a different invoice id are a conflict.
            assert!(matches!(
                mark_invoiced(&pg, &[first.id], actor, "inv-43").await,
                Err(TimeEntryError::Conflict)
            ));

            // An unknown entry id is not found.
            assert!(matches!(
                mark_invoiced(&pg, &[Uuid::new_v4()], actor, "inv-44").await,
                Err(TimeEntryError::NotFound)
            ));
        }

        #[tokio::test]
        async fn a_write_off_keeps_its_reason_and_cannot_reopen() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let seed = seed(&pg).await;
            let actor = person(&pg, "Actor").await;
            let created = create(&pg, seed.project_id, actor, &entry(seed.attorney, 15, true))
                .await
                .unwrap();

            let written_off = write_off(&pg, created.id, actor, "Absorbed by the flat fee")
                .await
                .unwrap();
            assert_eq!(
                written_off.billing_status,
                BillingStatus::WrittenOff {
                    reason: "Absorbed by the flat fee".into()
                }
            );

            assert!(matches!(
                write_off(&pg, created.id, actor, "Again").await,
                Err(TimeEntryError::Invalid("entry is already resolved"))
            ));
            assert!(matches!(
                mark_invoiced(&pg, &[created.id], actor, "inv-7").await,
                Err(TimeEntryError::Invalid("entry is already resolved"))
            ));
        }

        #[tokio::test]
        async fn every_mutation_appends_an_audited_event() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let seed = seed(&pg).await;
            let actor = person(&pg, "Actor").await;
            let created = create(&pg, seed.project_id, actor, &entry(seed.attorney, 60, true))
                .await
                .unwrap();
            mark_invoiced(&pg, &[created.id], actor, "inv-9").await.unwrap();

            let events = list_events(&pg, created.id).await.unwrap();
            let kinds: Vec<&str> = events
                .iter()
                .map(|event| event.event_kind.as_str())
                .collect();
            assert_eq!(kinds, ["created", "invoiced"]);
            assert!(
                events[0].before_state.is_none(),
                "creation has no prior state"
            );
            assert_eq!(events[0].actor_person_id, actor, "every event names its actor");
            assert_eq!(events[1].after_state["xero_invoice_id"], serde_json::json!("inv-9"));

            // The database itself refuses a mutation that carries no audit
            // context, so the event log cannot be bypassed.
            let bypass = sqlx::query("UPDATE time_entry SET minutes=61 WHERE id=$1")
                .bind(created.id)
                .execute(pg.pool())
                .await;
            assert!(bypass.is_err(), "an unaudited update must be refused");

            // The audit log itself is immutable.
            let tamper = sqlx::query("DELETE FROM time_entry_events WHERE id=$1")
                .bind(events[0].id)
                .execute(pg.pool())
                .await;
            assert!(tamper.is_err(), "audit events must be immutable");

            // The row itself is retained — a write-off, never a delete.
            let removed = sqlx::query("DELETE FROM time_entry WHERE id=$1")
                .bind(created.id)
                .execute(pg.pool())
                .await;
            assert!(removed.is_err(), "a Time Entry can never be deleted");
        }

        #[tokio::test]
        async fn entries_list_newest_work_first() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let seed = seed(&pg).await;
            let actor = person(&pg, "Actor").await;
            let mut earlier = entry(seed.attorney, 60, true);
            earlier.performed_on = chrono::NaiveDate::from_ymd_opt(2026, 10, 30).unwrap();
            let first = create(&pg, seed.project_id, actor, &earlier).await.unwrap();
            let second = create(&pg, seed.project_id, actor, &entry(seed.attorney, 30, true))
                .await
                .unwrap();

            let listed = list_for_project(&pg, seed.project_id).await.unwrap();
            let ids: Vec<Uuid> = listed.iter().map(|entry| entry.id).collect();
            assert_eq!(ids, [second.id, first.id]);
        }
    }
}

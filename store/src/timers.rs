//! Durable running Timers, one per attorney firm-wide, stored relationally in
//! PostgreSQL.
//!
//! A Timer row exists only while it runs: starting one inserts the row, and
//! stopping either captures the elapsed work as a Time Entry
//! (`crate::time_entries`) or discards it for nothing billed. The durable row
//! is the source of truth — a browser page only animates the display, so a
//! closed tab loses no work: on return the header recomputes the elapsed time
//! from `started_at` (`stop_timer` stamps `performed_on` from the same
//! instant). The database enforces the firm-wide invariant: at most one open
//! Timer per attorney (a unique index on `person_id`), so starting a second
//! requires stopping or discarding the first. Whole-minute precision; a stop
//! shorter than a minute records one minute. Only the attorney whose timer
//! runs may stop or discard it. See `docs/glossary/timer.md`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Row, Transaction};
use uuid::Uuid;

use crate::postgres::PostgresDb;
use crate::time_entries::{
    create_in_transaction, find_by_id, NewTimeEntry, TimeEntry, TimeEntryError, TimeEntrySource,
};

const NOTE_LIMIT: usize = 500;

/// A durable running Timer on a matter. The row is deleted the moment its
/// work is captured or discarded — it never rests in a stopped state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Timer {
    pub id: Uuid,
    pub person_id: Uuid,
    pub project_id: Uuid,
    pub started_at: DateTime<Utc>,
    pub note: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum TimerError {
    #[error("invalid Timer: {0}")]
    Invalid(&'static str),
    #[error("the attorney must be a participant on the Project")]
    NotParticipant,
    #[error("the attorney already has an open Timer")]
    Conflict,
    #[error("only the running attorney may stop or discard the Timer")]
    NotOwner,
    #[error("Timer was not found")]
    NotFound,
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
    #[error("PostgreSQL is unavailable")]
    Postgres(#[from] crate::postgres::PostgresError),
    #[error("the captured entry was refused: {0}")]
    Entry(#[from] TimeEntryError),
}

/// Whole minutes between two instants, floored, never zero: a stop shorter
/// than a minute records one minute. Immune to negative clock skew.
#[must_use]
pub fn elapsed_minutes(started_at: DateTime<Utc>, now: DateTime<Utc>) -> i32 {
    let minutes = (now - started_at).num_seconds().max(0) / 60;
    i32::try_from(minutes.clamp(1, i64::from(i32::MAX))).unwrap_or(i32::MAX)
}

/// Start the calling attorney's single firm-wide Timer on a matter. Refused
/// while another of their Timers is open, on or off this matter.
pub async fn start_timer(
    pg: &PostgresDb,
    person_id: Uuid,
    project_id: Uuid,
    note: Option<String>,
) -> Result<Timer, TimerError> {
    let note = match note {
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.len() > NOTE_LIMIT {
                return Err(TimerError::Invalid("note"));
            }
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        None => None,
    };
    require_participant(pg, project_id, person_id).await?;
    if running_for_person(pg, person_id).await?.is_some() {
        return Err(TimerError::Conflict);
    }
    sqlx::query(
        "INSERT INTO timer (person_id,project_id,started_at,note) \
         VALUES ($1,$2,now(),$3) \
         ON CONFLICT (person_id) DO NOTHING \
         RETURNING id,person_id,project_id,started_at,note",
    )
    .bind(person_id)
    .bind(project_id)
    .bind(note)
    .fetch_optional(&pg.0)
    .await?
    .map(|row| from_row(&row))
    // The unique index catches an attorney who raced a second start from
    // another tab; the pre-check above made the same refusal friendly.
    .ok_or(TimerError::Conflict)
}

/// The calling attorney's open Timer, if any — the recovery read behind the
/// matter header on every page.
pub async fn running_for_person(
    pg: &PostgresDb,
    person_id: Uuid,
) -> Result<Option<Timer>, TimerError> {
    sqlx::query(
        "SELECT id,person_id,project_id,started_at,note FROM timer \
         WHERE person_id=$1 ORDER BY started_at DESC LIMIT 1",
    )
    .bind(person_id)
    .fetch_optional(&pg.0)
    .await?
    .map(|row| Ok(from_row(&row)))
    .transpose()
}

/// Stop the Timer, capture its elapsed minutes as a Time Entry, and remove it
/// — one transaction, so a stop never bills nothing (or bills twice). The
/// caller pre-fills `entry` from the matter's rate resolution and the
/// attorney's description; `stop_timer` itself stamps the computed minutes,
/// the Timer source, and the date the Timer started. A Timer left running
/// past one day is refused: discard it or log the time by hand.
pub async fn stop_timer(
    pg: &PostgresDb,
    timer_id: Uuid,
    actor_id: Uuid,
    mut entry: NewTimeEntry,
) -> Result<TimeEntry, TimerError> {
    let mut tx = pg.0.begin().await?;
    let timer = fetch_for_update(&mut tx, timer_id).await?;
    if timer.person_id != actor_id {
        return Err(TimerError::NotOwner);
    }
    let minutes = elapsed_minutes(timer.started_at, Utc::now());
    if minutes > crate::time_entries::MAX_MINUTES_PER_DAY {
        return Err(TimerError::Invalid(
            "timer ran longer than a single day; discard it or log the time by hand",
        ));
    }
    entry.minutes = minutes;
    entry.source = TimeEntrySource::Timer;
    entry.performed_on = timer.started_at.date_naive();
    let entry_id = create_in_transaction(&mut tx, timer.project_id, actor_id, &entry).await?;
    sqlx::query("DELETE FROM timer WHERE id=$1")
        .bind(timer_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(find_by_id(pg, entry_id).await?)
}

/// Discard the Timer without recording anything. Deliberate, owner-only, and
/// transactional — the row is simply gone.
pub async fn discard_timer(pg: &PostgresDb, timer_id: Uuid, actor_id: Uuid) -> Result<(), TimerError> {
    let mut tx = pg.0.begin().await?;
    let timer = fetch_for_update(&mut tx, timer_id).await?;
    if timer.person_id != actor_id {
        return Err(TimerError::NotOwner);
    }
    sqlx::query("DELETE FROM timer WHERE id=$1")
        .bind(timer_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

async fn require_participant(
    pg: &PostgresDb,
    project_id: Uuid,
    person_id: Uuid,
) -> Result<(), TimerError> {
    sqlx::query("SELECT id FROM person_project_role WHERE project_id=$1 AND person_id=$2")
        .bind(project_id)
        .bind(person_id)
        .fetch_optional(&pg.0)
        .await?
        .map(|_| ())
        .ok_or(TimerError::NotParticipant)
}

async fn fetch_for_update(
    tx: &mut Transaction<'_, sqlx::Postgres>,
    id: Uuid,
) -> Result<Timer, TimerError> {
    sqlx::query(
        "SELECT id,person_id,project_id,started_at,note FROM timer WHERE id=$1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?
    .as_ref()
    .map(from_row)
    .ok_or(TimerError::NotFound)
}

fn from_row(row: &sqlx::postgres::PgRow) -> Timer {
    Timer {
        id: row.get(0),
        person_id: row.get(1),
        project_id: row.get(2),
        started_at: row.get(3),
        note: row.get(4),
    }
}

#[cfg(test)]
mod tests {
    use super::elapsed_minutes;
    use chrono::{DateTime, Utc};

    #[test]
    fn elapsed_minutes_are_whole_and_never_zero() {
        let start = DateTime::parse_from_rfc3339("2026-11-01T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let at = |secs: i64| start + chrono::Duration::seconds(secs);
        assert_eq!(elapsed_minutes(start, at(0)), 1, "a stop shorter than a minute still records a minute");
        assert_eq!(elapsed_minutes(start, at(59)), 1);
        assert_eq!(elapsed_minutes(start, at(60)), 1);
        assert_eq!(elapsed_minutes(start, at(61)), 1);
        assert_eq!(elapsed_minutes(start, at(125)), 2, "whole minutes, floored");
        assert_eq!(elapsed_minutes(at(30), start), 1, "clock skew cannot go negative");
    }

    // The remaining assertions need a live PostgreSQL: the firm-wide
    // single-open-Timer invariant is a database constraint, and the
    // stop-must-be-atomic contract spans two tables.
    mod database {
        use super::super::{discard_timer, running_for_person, start_timer, stop_timer, TimerError};
        use super::super::super::time_entries::{list_for_project, NewTimeEntry, TimeEntrySource};
        use crate::postgres::PostgresDb;
        use crate::postgres_mirror::tests::{connect, fresh_schema};
        use chrono::Utc;
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
                "INSERT INTO jurisdiction (id,name,code) VALUES ($1,'Synthetic Timer Jurisdiction','TMR') RETURNING id",
            )
            .bind(Uuid::new_v4())
            .fetch_one(pg.pool())
            .await
            .unwrap()
            .get(0);
            let entity_type: Uuid = sqlx::query(
                "INSERT INTO entity_type (id,name) VALUES ($1,'Synthetic Timer Entity Type') RETURNING id",
            )
            .bind(Uuid::new_v4())
            .fetch_one(pg.pool())
            .await
            .unwrap()
            .get(0);
            let entity: Uuid = sqlx::query(
                "INSERT INTO entity (id,name,entity_type_id,jurisdiction_id) VALUES ($1,'Synthetic Timer Entity',$2,$3) RETURNING id",
            )
            .bind(Uuid::new_v4())
            .bind(entity_type)
            .bind(jurisdiction)
            .fetch_one(pg.pool())
            .await
            .unwrap()
            .get(0);
            let project_id: Uuid = sqlx::query(
                "INSERT INTO project (id,code,name,status,entity_id) VALUES ($1,$2,'Synthetic Timer Matter','open',$3) RETURNING id",
            )
            .bind(Uuid::new_v4())
            .bind(format!("timer-{}", Uuid::new_v4().simple()))
            .bind(entity)
            .fetch_one(pg.pool())
            .await
            .unwrap()
            .get(0);
            let attorney = person(pg, "Timer Attorney").await;
            let outsider = person(pg, "Timer Outsider").await;
            sqlx::query(
                "INSERT INTO person_project_role (person_id,project_id,participation) VALUES ($1,$2,'lawyer')",
            )
            .bind(attorney)
            .bind(project_id)
            .execute(pg.pool())
            .await
            .unwrap();
            Seed { project_id, attorney, outsider }
        }

        async fn seed_participant(pg: &PostgresDb, project_id: Uuid, who: Uuid) {
            sqlx::query(
                "INSERT INTO person_project_role (person_id,project_id,participation) VALUES ($1,$2,'lawyer')",
            )
            .bind(who)
            .bind(project_id)
            .execute(pg.pool())
            .await
            .unwrap();
        }

        fn entry_input(attorney: Uuid, performed_on: chrono::NaiveDate) -> NewTimeEntry {
            NewTimeEntry {
                attorney_id: attorney,
                performed_on,
                minutes: 0,
                description: "Reviewed the production set".into(),
                billable: true,
                rate_cents_per_hour: 25_000,
                source: TimeEntrySource::Timer,
            }
        }

        #[tokio::test]
        async fn one_open_timer_per_attorney_firm_wide() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let seed = seed(&pg).await;
            let started = start_timer(&pg, seed.attorney, seed.project_id, Some("  drafting  ".into()))
                .await
                .unwrap();
            assert_eq!(started.note.as_deref(), Some("drafting"), "the note is trimmed");
            // A second timer, even on another matter of the firm, is refused.
            assert!(
                matches!(
                    start_timer(&pg, seed.attorney, seed.project_id, None).await,
                    Err(TimerError::Conflict)
                ),
                "the firm-wide invariant is enforced"
            );
            // A different attorney may run their own timer on the same matter.
            seed_participant(&pg, seed.project_id, seed.outsider).await;
            assert!(start_timer(&pg, seed.outsider, seed.project_id, None).await.is_ok());
        }

        #[tokio::test]
        async fn only_a_participant_may_start_a_timer() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let seed = seed(&pg).await;
            assert!(matches!(
                start_timer(&pg, seed.outsider, seed.project_id, None).await,
                Err(TimerError::NotParticipant)
            ));
        }

        #[tokio::test]
        async fn stop_creates_the_entry_and_clears_the_timer_in_one_transaction() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let seed = seed(&pg).await;
            let started = start_timer(&pg, seed.attorney, seed.project_id, None).await.unwrap();
            let captured = stop_timer(&pg, started.id, seed.attorney, entry_input(seed.attorney, Utc::now().date_naive()))
                .await
                .unwrap();
            assert_eq!(captured.source, TimeEntrySource::Timer);
            assert_eq!(captured.minutes, 1, "a sub-minute stop records one minute");
            assert_eq!(captured.rate_cents_per_hour, 25_000, "the snapshot is the caller's resolution");
            assert_eq!(captured.performed_on, started.started_at.date_naive(), "performed_on follows the start");
            assert!(running_for_person(&pg, seed.attorney).await.unwrap().is_none(), "the timer is gone");
            // One audit event: the capture is an ordinary Time Entry creation.
            let events = crate::time_entries::list_events(&pg, captured.id).await.unwrap();
            assert_eq!(events.len(), 1);
        }

        #[tokio::test]
        async fn only_the_running_attorney_may_stop_or_discard() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let seed = seed(&pg).await;
            seed_participant(&pg, seed.project_id, seed.outsider).await;
            let started = start_timer(&pg, seed.attorney, seed.project_id, None).await.unwrap();
            assert!(matches!(
                stop_timer(&pg, started.id, seed.outsider, entry_input(seed.attorney, Utc::now().date_naive())).await,
                Err(TimerError::NotOwner)
            ));
            assert!(matches!(
                discard_timer(&pg, started.id, seed.outsider).await,
                Err(TimerError::NotOwner)
            ));
            assert!(running_for_person(&pg, seed.attorney).await.unwrap().is_some());
        }

        #[tokio::test]
        async fn discard_removes_the_timer_without_an_entry() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let seed = seed(&pg).await;
            let started = start_timer(&pg, seed.attorney, seed.project_id, None).await.unwrap();
            discard_timer(&pg, started.id, seed.attorney).await.unwrap();
            assert!(running_for_person(&pg, seed.attorney).await.unwrap().is_none());
            assert!(
                list_for_project(&pg, seed.project_id).await.unwrap().is_empty(),
                "a discard never bills"
            );
        }

        #[tokio::test]
        async fn a_timer_left_running_past_a_day_is_refused() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let seed = seed(&pg).await;
            // Seed a stale timer directly: started two days ago.
            let stale: Uuid = sqlx::query(
                "INSERT INTO timer (person_id,project_id,started_at,note) VALUES ($1,$2,now() - interval '2 days',NULL) RETURNING id",
            )
            .bind(seed.attorney)
            .bind(seed.project_id)
            .fetch_one(pg.pool())
            .await
            .unwrap()
            .get(0);
            assert!(matches!(
                stop_timer(&pg, stale, seed.attorney, entry_input(seed.attorney, Utc::now().date_naive())).await,
                Err(TimerError::Invalid(
                    "timer ran longer than a single day; discard it or log the time by hand",
                ))
            ));
            // The stale timer stays put until its attorney decides.
            assert!(running_for_person(&pg, seed.attorney).await.unwrap().is_some());
        }
    }
}

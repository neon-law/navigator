//! An Attorney Calendar: a Google Calendar id recorded on a Person.
//!
//! The primary row is the calendar whose event details the signed-in Person
//! may read. Extra rows are opt-in free/busy shares. A Google OIDC subject
//! identifies the Person and is not a calendar id. Writes land in PostgreSQL.
//! A deployment without PostgreSQL records nothing and does not treat that
//! silence as free time.

use chrono::{DateTime, FixedOffset};
use model::projects::is_valid_calendar_id;
use sqlx::Row;
use thiserror::Error;
use uuid::Uuid;

use crate::postgres::{PostgresDb, PostgresError};

pub mod booking;

/// `primary` is the signed-in Person's own calendar. `freebusy` is an extra
/// calendar shared for busy time only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalendarKind {
    Primary,
    FreeBusy,
    Booking,
}

impl CalendarKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::FreeBusy => "freebusy",
            Self::Booking => "booking",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "primary" => Some(Self::Primary),
            "freebusy" => Some(Self::FreeBusy),
            "booking" => Some(Self::Booking),
            _ => None,
        }
    }
}

/// One recorded calendar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttorneyCalendar {
    pub person_id: Uuid,
    pub calendar_id: String,
    pub kind: CalendarKind,
}

#[derive(Debug, Error)]
pub enum AttorneyCalendarError {
    #[error("PostgreSQL is not configured")]
    NotConfigured,
    #[error("{}", model::projects::CALENDAR_ID_INVALID)]
    InvalidId,
    #[error("PostgreSQL is unavailable")]
    Unavailable,
    #[error("a different booking calendar is already registered")]
    BookingAlreadyRegistered,
}

impl From<PostgresError> for AttorneyCalendarError {
    fn from(error: PostgresError) -> Self {
        match error {
            PostgresError::Configuration => Self::NotConfigured,
            _ => Self::Unavailable,
        }
    }
}

/// Record the Workspace address as the primary calendar when the Person has
/// none. A Person row that PostgreSQL does not hold yet is left alone: sign-in
/// must not fail because the calendar table cannot name them.
///
/// # Errors
/// [`AttorneyCalendarError::Unavailable`] when PostgreSQL is configured and
/// cannot be reached, and [`AttorneyCalendarError::InvalidId`] when `email`
/// is not a calendar id.
pub async fn record_sign_in(person_id: Uuid, email: &str) -> Result<(), AttorneyCalendarError> {
    let pg = PostgresDb::application().await?;
    let email = email.trim();
    if !is_valid_calendar_id(email) {
        return Err(AttorneyCalendarError::InvalidId);
    }
    if crate::firms::is_consumer_gmail(email) {
        return Ok(());
    }
    let held: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM person WHERE id = $1)")
        .bind(person_id)
        .fetch_one(pg.pool())
        .await
        .map_err(|_| AttorneyCalendarError::Unavailable)?;
    if !held {
        return Ok(());
    }
    record_primary_if_absent(&pg, person_id, email).await?;
    Ok(())
}

/// The configured PostgreSQL store.
///
/// # Errors
/// [`AttorneyCalendarError::NotConfigured`] when no host is set, and
/// [`AttorneyCalendarError::Unavailable`] when the host cannot be reached.
pub async fn postgres() -> Result<PostgresDb, AttorneyCalendarError> {
    Ok(PostgresDb::application().await?)
}

/// Insert a primary calendar when the Person has none.
///
/// # Errors
/// [`AttorneyCalendarError::InvalidId`] or a PostgreSQL failure.
#[cfg(test)]
fn record_primary_if_absent_id(calendar_id: &str) -> Result<(), AttorneyCalendarError> {
    if is_valid_calendar_id(calendar_id.trim()) {
        Ok(())
    } else {
        Err(AttorneyCalendarError::InvalidId)
    }
}

pub async fn record_primary_if_absent(
    pg: &PostgresDb,
    person_id: Uuid,
    calendar_id: &str,
) -> Result<Option<AttorneyCalendar>, AttorneyCalendarError> {
    let calendar_id = calendar_id.trim();
    if !is_valid_calendar_id(calendar_id) {
        return Err(AttorneyCalendarError::InvalidId);
    }
    if listed(pg, &[person_id])
        .await?
        .into_iter()
        .any(|row| row.kind == CalendarKind::Primary)
    {
        return Ok(None);
    }
    register(pg, person_id, calendar_id, CalendarKind::Primary)
        .await
        .map(Some)
}

/// Record or replace the Person's calendar. A new primary demotes the previous
/// primary to free/busy so the partial unique index still holds one primary.
/// Registering free/busy never downgrades a primary row.
///
/// # Errors
/// [`AttorneyCalendarError::InvalidId`] or a PostgreSQL failure.
pub async fn register(
    pg: &PostgresDb,
    person_id: Uuid,
    calendar_id: &str,
    kind: CalendarKind,
) -> Result<AttorneyCalendar, AttorneyCalendarError> {
    let calendar_id = calendar_id.trim();
    if !is_valid_calendar_id(calendar_id) {
        return Err(AttorneyCalendarError::InvalidId);
    }
    if kind == CalendarKind::Booking {
        if !calendar_id.ends_with("@group.calendar.google.com") {
            return Err(AttorneyCalendarError::InvalidId);
        }
        let written = sqlx::query(
            "INSERT INTO person_calendar (person_id, calendar_id, kind)
             SELECT $1, $2, 'booking' WHERE NOT EXISTS (SELECT 1 FROM project WHERE calendar_id = $2)
             ON CONFLICT (person_id, calendar_id) DO UPDATE SET kind = 'booking'
             WHERE person_calendar.kind <> 'primary'",
        )
        .bind(person_id)
        .bind(calendar_id)
        .execute(pg.pool())
        .await
        .map_err(|error| {
            if error.as_database_error().is_some_and(sqlx::error::DatabaseError::is_unique_violation) {
                AttorneyCalendarError::BookingAlreadyRegistered
            } else {
                AttorneyCalendarError::Unavailable
            }
        })?;
        if written.rows_affected() != 1 {
            return Err(AttorneyCalendarError::InvalidId);
        }
        return Ok(AttorneyCalendar {
            person_id,
            calendar_id: calendar_id.into(),
            kind,
        });
    }
    let mut tx = pg
        .pool()
        .begin()
        .await
        .map_err(|_| AttorneyCalendarError::Unavailable)?;
    if kind == CalendarKind::Primary {
        sqlx::query(
            "UPDATE person_calendar SET kind = 'freebusy' \
             WHERE person_id = $1 AND kind = 'primary' AND lower(calendar_id) <> lower($2)",
        )
        .bind(person_id)
        .bind(calendar_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AttorneyCalendarError::Unavailable)?;
    }
    let written = sqlx::query(
        "INSERT INTO person_calendar (person_id, calendar_id, kind) VALUES ($1, $2, $3) \
         ON CONFLICT (person_id, calendar_id) DO UPDATE SET kind = \
         CASE WHEN person_calendar.kind = 'primary' THEN 'primary' ELSE EXCLUDED.kind END
         WHERE person_calendar.kind <> 'booking'",
    )
    .bind(person_id)
    .bind(calendar_id)
    .bind(kind.as_str())
    .execute(&mut *tx)
    .await
    .map_err(|_| AttorneyCalendarError::Unavailable)?;
    if written.rows_affected() != 1 {
        return Err(AttorneyCalendarError::InvalidId);
    }
    tx.commit()
        .await
        .map_err(|_| AttorneyCalendarError::Unavailable)?;
    let stored = listed(pg, &[person_id])
        .await?
        .into_iter()
        .find(|row| row.calendar_id.eq_ignore_ascii_case(calendar_id))
        .ok_or(AttorneyCalendarError::Unavailable)?;
    Ok(stored)
}

/// Every calendar recorded for these People, primary first.
///
/// # Errors
/// A PostgreSQL failure.
pub async fn listed(
    pg: &PostgresDb,
    person_ids: &[Uuid],
) -> Result<Vec<AttorneyCalendar>, AttorneyCalendarError> {
    if person_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query(
        "SELECT person_id, calendar_id, kind FROM person_calendar \
         WHERE person_id = ANY($1) AND kind <> 'booking' ORDER BY person_id, kind, calendar_id",
    )
    .bind(person_ids)
    .fetch_all(pg.pool())
    .await
    .map_err(|_| AttorneyCalendarError::Unavailable)?;
    rows.iter()
        .map(|row| {
            let kind: String = row
                .try_get("kind")
                .map_err(|_| AttorneyCalendarError::Unavailable)?;
            Ok(AttorneyCalendar {
                person_id: row
                    .try_get("person_id")
                    .map_err(|_| AttorneyCalendarError::Unavailable)?,
                calendar_id: row
                    .try_get("calendar_id")
                    .map_err(|_| AttorneyCalendarError::Unavailable)?,
                kind: CalendarKind::parse(&kind).ok_or(AttorneyCalendarError::Unavailable)?,
            })
        })
        .collect()
}

/// Opted-in attorney IDs. Booking calendars never feed Navigator's own free/busy checks.
pub async fn booking_candidates(pg: &PostgresDb) -> Result<Vec<Uuid>, AttorneyCalendarError> {
    sqlx::query_scalar(
        "SELECT person_id FROM person_calendar WHERE kind = 'booking' ORDER BY person_id",
    )
    .fetch_all(pg.pool())
    .await
    .map_err(|_| AttorneyCalendarError::Unavailable)
}

pub async fn booking_calendar(
    pg: &PostgresDb,
    person_id: Uuid,
) -> Result<Option<String>, AttorneyCalendarError> {
    sqlx::query_scalar(
        "SELECT calendar_id FROM person_calendar WHERE kind = 'booking' AND person_id = $1",
    )
    .bind(person_id)
    .fetch_optional(pg.pool())
    .await
    .map_err(|_| AttorneyCalendarError::Unavailable)
}

/// One busy interval attributed to a person, the Project Calendar, or a Deadline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBlock {
    pub source: String,
    pub role: String,
    pub calendar_id: Option<String>,
    pub start: DateTime<FixedOffset>,
    pub end: DateTime<FixedOffset>,
}

/// A calendar the check could not read. An empty busy list is not this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadableCalendar {
    pub email: String,
    pub role: String,
    pub calendar_id: Option<String>,
    pub missing: &'static str,
}

/// A repeated event title on the signed-in attorney's calendar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateTitle {
    pub summary: String,
    pub count: usize,
}

/// The scheduling answer: busy time, gaps, overlaps, and duplicate titles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleReport {
    pub busy: Vec<SourceBlock>,
    pub overlaps: Vec<SourceBlock>,
    pub unreadable: Vec<UnreadableCalendar>,
    pub duplicates: Vec<DuplicateTitle>,
    pub clear: bool,
}

/// Fold provider reads, Deadline instants, and the signed-in attorney's event
/// titles into one report. A gap or an overlap makes `clear` false.
#[must_use]
pub fn schedule(
    mut unreadable: Vec<UnreadableCalendar>,
    mut blocks: Vec<SourceBlock>,
    summaries: &[String],
    window_start: DateTime<FixedOffset>,
    window_end: DateTime<FixedOffset>,
) -> ScheduleReport {
    unreadable.sort_by(|left, right| {
        (&left.email, &left.calendar_id).cmp(&(&right.email, &right.calendar_id))
    });
    blocks.retain(|block| overlaps(block.start, block.end, window_start, window_end));
    blocks.sort_by(|left, right| {
        (left.start, &left.source, left.calendar_id.as_deref()).cmp(&(
            right.start,
            &right.source,
            right.calendar_id.as_deref(),
        ))
    });
    let overlaps = blocks.clone();
    let duplicates = duplicate_titles(summaries);
    let clear = unreadable.is_empty() && overlaps.is_empty() && duplicates.is_empty();
    ScheduleReport {
        busy: blocks,
        overlaps,
        unreadable,
        duplicates,
        clear,
    }
}

/// Titles that occur more than once, compared case-insensitively, blanks ignored.
#[must_use]
pub fn duplicate_titles(summaries: &[String]) -> Vec<DuplicateTitle> {
    let mut counts = std::collections::BTreeMap::<String, (String, usize)>::new();
    for summary in summaries {
        let trimmed = summary.trim();
        if trimmed.is_empty() {
            continue;
        }
        let key = trimmed.to_ascii_lowercase();
        counts
            .entry(key)
            .and_modify(|(_, count)| *count += 1)
            .or_insert_with(|| (trimmed.to_string(), 1));
    }
    counts
        .into_values()
        .filter(|(_, count)| *count > 1)
        .map(|(summary, count)| DuplicateTitle { summary, count })
        .collect()
}

#[must_use]
pub fn overlaps(
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
    window_start: DateTime<FixedOffset>,
    window_end: DateTime<FixedOffset>,
) -> bool {
    if end <= start {
        window_start <= start && start < window_end
    } else {
        start < window_end && window_start < end
    }
}

#[cfg(test)]
mod tests {
    use super::{duplicate_titles, overlaps, schedule, SourceBlock, UnreadableCalendar};
    use chrono::DateTime;

    fn at(value: &str) -> DateTime<chrono::FixedOffset> {
        DateTime::parse_from_rfc3339(value).unwrap()
    }

    #[test]
    fn a_gap_is_not_a_clear_window() {
        let start = at("2026-11-01T15:00:00Z");
        let end = at("2026-11-01T16:00:00Z");
        let report = schedule(
            vec![UnreadableCalendar {
                email: "ada@example.com".into(),
                role: "lawyer".into(),
                calendar_id: Some("ada@example.com".into()),
                missing: "share",
            }],
            vec![],
            &[],
            start,
            end,
        );
        assert!(!report.clear);
        assert_eq!(report.unreadable[0].missing, "share");
        assert!(report.overlaps.is_empty());
    }

    #[test]
    fn an_overlapping_deadline_and_a_repeated_title_block_the_window() {
        let start = at("2026-11-01T15:00:00Z");
        let end = at("2026-11-01T16:00:00Z");
        let report = schedule(
            vec![],
            vec![SourceBlock {
                source: "Reply brief".into(),
                role: "deadline".into(),
                calendar_id: None,
                start,
                end: start,
            }],
            &["Status conference".into(), "status conference".into()],
            start,
            end,
        );
        assert!(!report.clear);
        assert_eq!(report.overlaps.len(), 1);
        assert_eq!(report.duplicates.len(), 1);
        assert_eq!(report.duplicates[0].count, 2);
        assert!(overlaps(start, start, start, end));
        assert!(!overlaps(end, end, start, end));
    }

    #[test]
    fn a_blank_title_is_not_a_duplicate() {
        assert!(duplicate_titles(&["  ".into(), String::new()]).is_empty());
    }

    #[test]
    fn a_link_is_not_a_calendar_id() {
        assert!(
            super::record_primary_if_absent_id("https://calendar.google.com/calendar/r").is_err()
        );
    }

    #[tokio::test]
    async fn sign_in_records_one_primary_and_keeps_an_extra_calendar_freebusy() {
        let Some((options, database)) = crate::postgres::fixtures::fresh_schema().await else {
            return;
        };
        let pg = crate::postgres::PostgresDb::connect(options.database(&database))
            .await
            .unwrap();
        let person_id = uuid::Uuid::now_v7();
        sqlx::query(
            "INSERT INTO person (id, name, email, role) VALUES ($1, 'Synthetic Lawyer', 'ada@example.com', 'lawyer')",
        )
        .bind(person_id)
        .execute(pg.pool())
        .await
        .unwrap();
        super::record_primary_if_absent(&pg, person_id, "ada@example.com")
            .await
            .unwrap();
        assert!(
            super::record_primary_if_absent(&pg, person_id, "other@example.com")
                .await
                .unwrap()
                .is_none()
        );
        super::register(
            &pg,
            person_id,
            "personal@example.com",
            super::CalendarKind::FreeBusy,
        )
        .await
        .unwrap();
        super::register(
            &pg,
            person_id,
            "desk@example.com",
            super::CalendarKind::Primary,
        )
        .await
        .unwrap();
        let rows = super::listed(&pg, &[person_id]).await.unwrap();
        let primary: Vec<_> = rows
            .iter()
            .filter(|row| row.kind == super::CalendarKind::Primary)
            .collect();
        assert_eq!(primary.len(), 1);
        assert_eq!(primary[0].calendar_id, "desk@example.com");
        assert!(rows.iter().any(|row| {
            row.kind == super::CalendarKind::FreeBusy && row.calendar_id == "ada@example.com"
        }));
        assert!(rows.iter().any(|row| {
            row.kind == super::CalendarKind::FreeBusy && row.calendar_id == "personal@example.com"
        }));
    }
}

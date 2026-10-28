//! A Project's shared Google Calendar: create or adopt it, share it with the
//! firm-side participants, and make it read-only when the matter closes.
//!
//! The calendar is named for the Project code, the way the Drive folder and
//! the private Notion page are, so no client name reaches the Firm's
//! calendar list. Its id is recorded as `project.calendar_id` before any
//! sharing, so a run that shares and then fails cannot leave a calendar the
//! next run would create a second time.
//!
//! A recorded id is checked by id, never rediscovered by name: a calendar
//! someone renamed in Google is still the recorded one, and a deleted one is
//! reported rather than silently replaced.
//!
//! Sharing goes to firm-side participants only — every participation whose
//! Person is not a [`Role::Client`]. A client books through the Project's
//! `booking_url`, never through calendar access. Sharing only ever adds or
//! raises access while the matter is open; nobody is removed, so a person a
//! lawyer shared by hand in Google keeps their access.

use std::collections::{BTreeMap, BTreeSet};

use uuid::Uuid;

use cloud::calendar::{CalendarError, CalendarRole, CalendarService};

use crate::persons::{self, Role};
use crate::projects::{self, Project, ProjectCommandError};
use crate::surreal::SurrealDb;

/// What one calendar pass did, as a closed slug a door reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalendarOutcome {
    /// This pass created the calendar and recorded its id.
    Created,
    /// This pass found an unrecorded calendar named for the Project and
    /// recorded its id.
    Adopted,
    /// The recorded calendar exists; sharing was reconciled.
    Present,
    /// The matter is closed: writers were made readers and the booking link
    /// was cleared.
    ReadOnly,
    /// A calendar id is recorded but the calendar no longer exists. Nothing is
    /// created in its place; a person decides whether to clear the id.
    RecordedResourceMissing,
}

impl CalendarOutcome {
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Adopted => "adopted",
            Self::Present => "present",
            Self::ReadOnly => "read_only",
            Self::RecordedResourceMissing => "recorded_resource_missing",
        }
    }
}

/// A calendar pass's outcome and how many people it changed. Counts only:
/// addresses stay inside the pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalendarReport {
    pub outcome: CalendarOutcome,
    /// People newly shared as writers, or made readers on close.
    pub changed: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum ProjectCalendarError {
    #[error("no matter with that id")]
    NotFound,
    #[error("calendar: {0}")]
    Calendar(#[from] CalendarError),
    #[error("{0}")]
    Command(#[from] ProjectCommandError),
    #[error("database: {0}")]
    Db(String),
}

/// Create or adopt the Project's calendar and share it with its firm-side
/// participants. A closed or archived matter is made read-only instead, so a
/// re-run of setup after close converges rather than re-sharing.
///
/// # Errors
/// A store or provider failure. A provider failure after the id is recorded
/// leaves the id in place, so the next pass resumes with sharing.
pub async fn ensure(
    surreal: &SurrealDb,
    project_id: Uuid,
    calendar: &dyn CalendarService,
) -> Result<CalendarReport, ProjectCalendarError> {
    let project = find(surreal, project_id).await?;
    if project.status != "open" {
        return close(surreal, project_id, calendar).await;
    }
    let (calendar_id, outcome) = if let Some(recorded) = configured(&project) {
        if !calendar.calendar_exists(&recorded).await? {
            return Ok(CalendarReport {
                outcome: CalendarOutcome::RecordedResourceMissing,
                changed: 0,
            });
        }
        (recorded, CalendarOutcome::Present)
    } else {
        let (found, outcome) = match calendar.find_calendar(&project.code).await? {
            Some(found) => (found, CalendarOutcome::Adopted),
            None => (
                calendar.create_calendar(&project.code).await?,
                CalendarOutcome::Created,
            ),
        };
        projects::set_calendar_id(surreal, project_id, &found.id)
            .await?
            .ok_or(ProjectCalendarError::NotFound)?;
        (found.id, outcome)
    };
    let current: BTreeMap<String, CalendarRole> = calendar
        .members(&calendar_id)
        .await?
        .into_iter()
        .map(|member| (member.email.to_ascii_lowercase(), member.role))
        .collect();
    let mut changed = 0;
    for email in firm_side_emails(surreal, project_id).await? {
        if current
            .get(&email)
            .is_some_and(|role| *role >= CalendarRole::Writer)
        {
            continue;
        }
        calendar
            .set_member_role(&calendar_id, &email, CalendarRole::Writer)
            .await?;
        changed += 1;
    }
    Ok(CalendarReport { outcome, changed })
}

/// Make the Project's calendar read-only and clear its booking link. Every
/// writer becomes a reader; the owner is untouched. The calendar and its
/// events are kept: they are part of the matter's record.
///
/// A Project with no recorded calendar, or whose calendar is gone, still has
/// its booking link cleared.
///
/// # Errors
/// A store or provider failure.
pub async fn close(
    surreal: &SurrealDb,
    project_id: Uuid,
    calendar: &dyn CalendarService,
) -> Result<CalendarReport, ProjectCalendarError> {
    let project = find(surreal, project_id).await?;
    if project.booking_url.is_some() {
        projects::clear_booking_url(surreal, project_id).await?;
    }
    let Some(calendar_id) = configured(&project) else {
        return Ok(CalendarReport {
            outcome: CalendarOutcome::ReadOnly,
            changed: 0,
        });
    };
    if !calendar.calendar_exists(&calendar_id).await? {
        return Ok(CalendarReport {
            outcome: CalendarOutcome::RecordedResourceMissing,
            changed: 0,
        });
    }
    let mut changed = 0;
    for member in calendar.members(&calendar_id).await? {
        if member.role == CalendarRole::Writer {
            calendar
                .set_member_role(&calendar_id, &member.email, CalendarRole::Reader)
                .await?;
            changed += 1;
        }
    }
    Ok(CalendarReport {
        outcome: CalendarOutcome::ReadOnly,
        changed,
    })
}

/// The deployment's Workspace calendar, or `None` when none of its
/// configuration keys is set. A partial configuration is an error, never a
/// silent skip.
///
/// # Errors
/// [`CalendarError::MissingConfig`] naming each absent key, or an
/// authentication failure.
pub async fn calendar_from_env() -> Result<Option<cloud::GoogleCalendar>, CalendarError> {
    let configured = cloud::calendar::CALENDAR_CONFIGURATION_KEYS
        .iter()
        .any(|key| std::env::var(key).is_ok());
    if configured {
        cloud::GoogleCalendar::from_env().await.map(Some)
    } else {
        Ok(None)
    }
}

async fn find(surreal: &SurrealDb, project_id: Uuid) -> Result<Project, ProjectCalendarError> {
    projects::find_by_id(surreal, project_id)
        .await
        .map_err(|error| ProjectCalendarError::Db(error.to_string()))?
        .ok_or(ProjectCalendarError::NotFound)
}

fn configured(project: &Project) -> Option<String> {
    project
        .calendar_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

/// The lowercased email of every participant who is not a client.
async fn firm_side_emails(
    surreal: &SurrealDb,
    project_id: Uuid,
) -> Result<BTreeSet<String>, ProjectCalendarError> {
    let rows = projects::participations_for_project(surreal, project_id)
        .await
        .map_err(|error| ProjectCalendarError::Db(error.to_string()))?;
    let mut emails = BTreeSet::new();
    for row in rows {
        let Some(person) = persons::find_by_id(surreal, row.person_id)
            .await
            .map_err(|error| ProjectCalendarError::Db(error.to_string()))?
        else {
            continue;
        };
        let email = person.email.trim();
        if person.role == Role::Client || email.is_empty() {
            continue;
        }
        emails.insert(email.to_ascii_lowercase());
    }
    Ok(emails)
}

#[cfg(test)]
mod tests {
    use cloud::calendar::{CalendarRole, CalendarService, FakeCalendar, FAKE_CALENDAR_OWNER};

    use super::{close, ensure, CalendarOutcome};
    use crate::persons::{self, NewPerson, Role};
    use crate::projects::{self, OpenMatterCommand, UpdateProjectCommand};
    use crate::test_support::{mem_surreal, seed_entity, seed_firm_with_brand};

    async fn open_matter(surreal: &crate::surreal::SurrealDb) -> projects::Project {
        seed_firm_with_brand(surreal, "neon").await;
        let entity_id = seed_entity(surreal).await;
        let lawyer = persons::create(
            surreal,
            &NewPerson::with_role("Lawyer", "Lawyer@Example.com", Role::Lawyer),
        )
        .await
        .expect("lawyer");
        let client = persons::create(
            surreal,
            &NewPerson::with_role("Client", "client@example.com", Role::Client),
        )
        .await
        .expect("client");
        projects::open_matter(
            surreal,
            &OpenMatterCommand {
                name: "Synthetic matter".into(),
                code: "matter-one".into(),
                client_id: client.id,
                entity_id,
                jurisdiction_id: None,
                description: None,
                brand: "neon".to_string(),
                attestation: true,
                acting_person_id: lawyer.id,
                closed_at: None,
            },
        )
        .await
        .expect("open")
    }

    #[tokio::test]
    async fn setup_creates_records_and_shares_with_the_firm_side_only() {
        let surreal = mem_surreal().await;
        let project = open_matter(&surreal).await;
        let calendar = FakeCalendar::default();

        let report = ensure(&surreal, project.id, &calendar).await.unwrap();

        assert_eq!(report.outcome, CalendarOutcome::Created);
        assert_eq!(report.changed, 1);
        let created = calendar.calendars();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].summary, "matter-one");
        let stored = projects::find_by_id(&surreal, project.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.calendar_id.as_deref(), Some(created[0].id.as_str()));
        let sharing = calendar.sharing(&created[0].id);
        assert_eq!(
            sharing.get("lawyer@example.com"),
            Some(&CalendarRole::Writer)
        );
        assert!(
            !sharing.contains_key("client@example.com"),
            "a client is never shared the calendar: {sharing:?}"
        );
    }

    #[tokio::test]
    async fn setup_is_idempotent() {
        let surreal = mem_surreal().await;
        let project = open_matter(&surreal).await;
        let calendar = FakeCalendar::default();

        ensure(&surreal, project.id, &calendar).await.unwrap();
        let again = ensure(&surreal, project.id, &calendar).await.unwrap();

        assert_eq!(again.outcome, CalendarOutcome::Present);
        assert_eq!(again.changed, 0, "nobody is re-shared");
        assert_eq!(calendar.created(), 1, "no second calendar");
    }

    #[tokio::test]
    async fn an_unrecorded_calendar_named_for_the_project_is_adopted() {
        let surreal = mem_surreal().await;
        let project = open_matter(&surreal).await;
        let calendar = FakeCalendar::default();
        let existing = calendar.create_calendar("matter-one").await.unwrap();

        let report = ensure(&surreal, project.id, &calendar).await.unwrap();

        assert_eq!(report.outcome, CalendarOutcome::Adopted);
        assert_eq!(calendar.created(), 1);
        let stored = projects::find_by_id(&surreal, project.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.calendar_id.as_deref(), Some(existing.id.as_str()));
    }

    #[tokio::test]
    async fn a_deleted_recorded_calendar_is_reported_not_replaced() {
        let surreal = mem_surreal().await;
        let project = open_matter(&surreal).await;
        let calendar = FakeCalendar::default();
        ensure(&surreal, project.id, &calendar).await.unwrap();
        let id = calendar.calendars()[0].id.clone();
        calendar.delete(&id);

        let report = ensure(&surreal, project.id, &calendar).await.unwrap();

        assert_eq!(report.outcome, CalendarOutcome::RecordedResourceMissing);
        assert_eq!(calendar.created(), 1, "nothing is created in its place");
    }

    #[tokio::test]
    async fn close_makes_writers_readers_keeps_the_owner_and_clears_the_booking_link() {
        let surreal = mem_surreal().await;
        let project = open_matter(&surreal).await;
        let calendar = FakeCalendar::default();
        ensure(&surreal, project.id, &calendar).await.unwrap();
        projects::update_project(
            &surreal,
            project.id,
            &UpdateProjectCommand {
                booking_url: Some("https://calendar.app.google/synthetic".into()),
                ..UpdateProjectCommand::default()
            },
        )
        .await
        .unwrap();

        let report = close(&surreal, project.id, &calendar).await.unwrap();

        assert_eq!(report.outcome, CalendarOutcome::ReadOnly);
        assert_eq!(report.changed, 1);
        let id = calendar.calendars()[0].id.clone();
        let sharing = calendar.sharing(&id);
        assert_eq!(
            sharing.get("lawyer@example.com"),
            Some(&CalendarRole::Reader)
        );
        assert_eq!(sharing.get(FAKE_CALENDAR_OWNER), Some(&CalendarRole::Owner));
        let stored = projects::find_by_id(&surreal, project.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.booking_url, None);
        assert_eq!(stored.calendar_id.as_deref(), Some(id.as_str()));
    }

    #[tokio::test]
    async fn setup_on_a_closed_matter_stays_read_only() {
        let surreal = mem_surreal().await;
        let project = open_matter(&surreal).await;
        let calendar = FakeCalendar::default();
        ensure(&surreal, project.id, &calendar).await.unwrap();
        // Closed directly: the lifecycle's close needs an offboarding letter.
        surreal
            .query("UPDATE $id SET status = 'closed'")
            .bind(("id", crate::surreal::record_id("project", project.id)))
            .await
            .and_then(surrealdb::IndexedResults::check)
            .unwrap();

        let report = ensure(&surreal, project.id, &calendar).await.unwrap();

        assert_eq!(report.outcome, CalendarOutcome::ReadOnly);
        let id = calendar.calendars()[0].id.clone();
        assert_eq!(
            calendar.sharing(&id).get("lawyer@example.com"),
            Some(&CalendarRole::Reader)
        );
    }
}

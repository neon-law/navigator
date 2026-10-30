//! The Deadline mirror onto a Project's shared Google Calendar.
//!
//! Every Deadline row renders as one event on the matter's calendar, on the
//! same field basis as the ICS feed a subscriber already sees: the summary
//! fallback by calendar role and authority kind, the description's
//! authority and matter-page lines, the zero-length instant, the cancelled
//! status, and the internal lead days as the only reminder. What the feed
//! renders for a subscriber, the mirror puts on the calendar.
//!
//! The mirror is row-authoritative. A change made directly in Google — a
//! recolor, a moved time, a cancelled event — is the person's, and the
//! mirror leaves it alone unless the row itself changes. Reconciling a row
//! compares the content sha recorded in the event's private extended
//! properties against the sha the row now renders: equal means the event is
//! already in sync, different means the row changed since the write, and no
//! event at all means the row was never mirrored or its event was deleted
//! outside Navigator. Events whose Deadline row is gone are deleted; a
//! closed matter is skipped entirely, and a recorded-but-gone calendar is
//! reported rather than replaced with a second calendar.

use std::collections::{BTreeMap, BTreeSet};

use chrono::DateTime;
use cloud::calendar::{CalendarError, CalendarEvent, CalendarService, StoredEvent};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::project_calendar::{self, CalendarOutcome, ProjectCalendarError};
use crate::project_deadlines::{self, Deadline};
use crate::projects::{self, Project};
use crate::surreal::SurrealDb;

/// The private extended property naming the mirror, so one filtered read
/// finds every mirrored event and nothing else on the calendar.
pub const MARKER_KEY: &str = "navigator_mirror";
pub const MARKER_VALUE: &str = "deadline";
/// The private extended property carrying the Deadline's record id.
pub const DEADLINE_ID_KEY: &str = "deadline_id";
/// The private extended property carrying the content sha the event was
/// last written from.
pub const BODY_SHA_KEY: &str = "body_sha";

/// Whether a reconcile pass could act at all.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MirrorOutcome {
    /// The matter is open and the pass ran to its plan, applied or not.
    #[default]
    Mirrored,
    /// The matter is not open: nothing was written and nothing is planned.
    SkippedClosed,
    /// A calendar id is recorded but the calendar no longer exists. A person
    /// decides whether to clear it; the mirror refuses to invent a second
    /// calendar where one was recorded.
    RecordedResourceMissing,
}

impl MirrorOutcome {
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Mirrored => "mirrored",
            Self::SkippedClosed => "skipped_closed",
            Self::RecordedResourceMissing => "recorded_resource_missing",
        }
    }
}

/// What one reconcile pass did for a Project's mirror, as a closed report a
/// workflow journals and a door returns. It names deadline ids and event
/// ids, never client matter content.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorReport {
    pub outcome: MirrorOutcome,
    /// The pass reported a plan instead of writing it.
    pub dry_run: bool,
    /// The pass would create or adopt the Project's calendar before writing.
    pub creates_calendar: bool,
    /// Deadlines whose event was inserted, updated, or is planned to be.
    pub upserted: Vec<Uuid>,
    /// Calendar event ids deleted, or planned to be, because their Deadline
    /// row is gone.
    pub deleted: Vec<String>,
    /// Deadlines whose event already matched the row.
    pub in_sync: usize,
    /// One line per row that could not be mirrored, naming the deadline id
    /// and cause; the pass continues past a bad row.
    pub failures: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum MirrorError {
    #[error("no matter with that id")]
    NotFound,
    #[error("database: {0}")]
    Db(String),
    #[error("calendar: {0}")]
    Calendar(#[from] CalendarError),
    #[error("deadline {0} is not mirrorable: {1}")]
    InvalidDeadline(Uuid, String),
}

/// Reconcile the Project's mirrored events to its Deadline rows.
///
/// The applied pass ensures the Project's calendar exists and is shared
/// before mirroring, so a verb or schedule can call this alone. The dry run
/// writes nothing: with a recorded, existing calendar it plans the same
/// writes from a read-only diff; with no usable calendar it plans the
/// inserts a first pass would make and names the calendar it would create.
///
/// # Errors
/// A store failure, a calendar failure outside a single row, or a Project
/// that does not exist.
pub async fn reconcile(
    surreal: &SurrealDb,
    calendar: &dyn CalendarService,
    project_id: Uuid,
    base_url: Option<&str>,
    dry_run: bool,
) -> Result<MirrorReport, MirrorError> {
    let project = find_project(surreal, project_id).await?;
    if project.status != "open" {
        return Ok(MirrorReport {
            outcome: MirrorOutcome::SkippedClosed,
            ..MirrorReport::empty(MirrorOutcome::SkippedClosed, dry_run)
        });
    }
    let recorded = recorded_calendar_id(&project);
    if dry_run {
        return match recorded {
            Some(calendar_id) => {
                if calendar.calendar_exists(&calendar_id).await? {
                    reconcile_rows(surreal, calendar, &calendar_id, &project, base_url, dry_run)
                        .await
                } else {
                    Ok(MirrorReport::empty(
                        MirrorOutcome::RecordedResourceMissing,
                        dry_run,
                    ))
                }
            }
            None => {
                // No calendar yet: a first pass would create one and insert
                // every row. That plan needs no calendar to read.
                let mut report = MirrorReport::empty(MirrorOutcome::Mirrored, dry_run);
                report.creates_calendar = true;
                plan_rows(surreal, &project, base_url, &mut report).await?;
                Ok(report)
            }
        };
    }
    let ensured = ensure_calendar(surreal, project_id, calendar).await?;
    if ensured.outcome == CalendarOutcome::RecordedResourceMissing {
        return Ok(MirrorReport::empty(
            MirrorOutcome::RecordedResourceMissing,
            dry_run,
        ));
    }
    // The calendar pass recorded the id before sharing; read it back.
    let project = find_project(surreal, project_id).await?;
    let Some(calendar_id) = recorded_calendar_id(&project) else {
        return Err(MirrorError::Db(format!(
            "matter {project_id} has no calendar id after the calendar pass"
        )));
    };
    reconcile_rows(surreal, calendar, &calendar_id, &project, base_url, dry_run).await
}

/// The diff half of a pass, shared by the applied and dry-run paths: read
/// the rows and the mirrored events, then write (or plan) the difference.
async fn reconcile_rows(
    surreal: &SurrealDb,
    calendar: &dyn CalendarService,
    calendar_id: &str,
    project: &Project,
    base_url: Option<&str>,
    dry_run: bool,
) -> Result<MirrorReport, MirrorError> {
    let mut report = MirrorReport::empty(MirrorOutcome::Mirrored, dry_run);
    let rows = project_deadlines::list(surreal, project.id)
        .await
        .map_err(|error| MirrorError::Db(error.to_string()))?;
    let stored = calendar
        .mirrored_events(calendar_id, MARKER_KEY, MARKER_VALUE)
        .await?;
    let by_deadline: BTreeMap<String, StoredEvent> = stored
        .into_iter()
        .filter_map(|event| {
            event
                .private_properties
                .get(DEADLINE_ID_KEY)
                .cloned()
                .map(|id| (id, event))
        })
        .collect();
    let mut live: BTreeSet<String> = BTreeSet::new();
    for deadline in &rows {
        let sha = body_sha(deadline, &project.code, base_url);
        let id = deadline.id.to_string();
        live.insert(id.clone());
        let event = match deadline_event(deadline, &project.code, base_url) {
            Ok(event) => event,
            Err(error) => {
                report.failures.push(error.to_string());
                continue;
            }
        };
        match by_deadline.get(&id) {
            Some(stored) => {
                if stored
                    .private_properties
                    .get(BODY_SHA_KEY)
                    .is_some_and(|recorded| *recorded == sha)
                {
                    report.in_sync += 1;
                } else if dry_run {
                    report.upserted.push(deadline.id);
                } else if let Err(error) =
                    calendar.patch_event(calendar_id, &stored.id, &event).await
                {
                    report
                        .failures
                        .push(format!("deadline {}: {error}", deadline.id));
                } else {
                    report.upserted.push(deadline.id);
                }
            }
            None => {
                if dry_run {
                    report.upserted.push(deadline.id);
                } else if let Err(error) = calendar.insert_event(calendar_id, &event).await {
                    report
                        .failures
                        .push(format!("deadline {}: {error}", deadline.id));
                } else {
                    report.upserted.push(deadline.id);
                }
            }
        }
    }
    for (deadline_id, event) in &by_deadline {
        if live.contains(deadline_id) {
            continue;
        }
        if dry_run {
            report.deleted.push(event.id.clone());
        } else if let Err(error) = calendar.delete_event(calendar_id, &event.id).await {
            report.failures.push(format!("event {}: {error}", event.id));
        } else {
            report.deleted.push(event.id.clone());
        }
    }
    Ok(report)
}

/// The plan a first pass would make, before the Project has a calendar.
async fn plan_rows(
    surreal: &SurrealDb,
    project: &Project,
    base_url: Option<&str>,
    report: &mut MirrorReport,
) -> Result<(), MirrorError> {
    let rows = project_deadlines::list(surreal, project.id)
        .await
        .map_err(|error| MirrorError::Db(error.to_string()))?;
    for deadline in &rows {
        match deadline_event(deadline, &project.code, base_url) {
            Ok(_) => report.upserted.push(deadline.id),
            Err(error) => report.failures.push(error.to_string()),
        }
    }
    Ok(())
}

/// Ensure the Project's calendar exists and is shared, mapping the calendar
/// pass's errors onto the mirror's.
async fn ensure_calendar(
    surreal: &SurrealDb,
    project_id: Uuid,
    calendar: &dyn CalendarService,
) -> Result<project_calendar::CalendarReport, MirrorError> {
    project_calendar::ensure(surreal, project_id, calendar)
        .await
        .map_err(|error| match error {
            ProjectCalendarError::Calendar(calendar_error) => MirrorError::Calendar(calendar_error),
            ProjectCalendarError::NotFound => MirrorError::NotFound,
            ProjectCalendarError::Command(command) => MirrorError::Db(command.to_string()),
            ProjectCalendarError::Db(message) => MirrorError::Db(message),
        })
}

async fn find_project(surreal: &SurrealDb, project_id: Uuid) -> Result<Project, MirrorError> {
    projects::find_by_id(surreal, project_id)
        .await
        .map_err(|error| MirrorError::Db(error.to_string()))?
        .ok_or(MirrorError::NotFound)
}

fn recorded_calendar_id(project: &Project) -> Option<String> {
    project
        .calendar_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

impl MirrorReport {
    fn empty(outcome: MirrorOutcome, dry_run: bool) -> Self {
        Self {
            outcome,
            dry_run,
            creates_calendar: false,
            upserted: Vec::new(),
            deleted: Vec::new(),
            in_sync: 0,
            failures: Vec::new(),
        }
    }
}

/// The one line a door reports and the nightly workflow journals for one
/// matter's pass: the outcome and its counts, never matter content.
#[must_use]
pub fn report_line(report: &MirrorReport) -> String {
    let mut line = format!(
        "{} upserted={} in_sync={} deleted={}",
        report.outcome.slug(),
        report.upserted.len(),
        report.in_sync,
        report.deleted.len(),
    );
    if !report.failures.is_empty() {
        line.push_str(&format!(" failed={}", report.failures.len()));
    }
    if report.dry_run {
        line.push_str(" (dry run)");
    }
    line
}

/// The event a Deadline renders to, on the ICS feed's field basis.
///
/// # Errors
/// A row whose due instant or sequence cannot be carried by an event.
pub fn deadline_event(
    deadline: &Deadline,
    project_code: &str,
    base_url: Option<&str>,
) -> Result<CalendarEvent, MirrorError> {
    let start = DateTime::parse_from_rfc3339(deadline.due_at.trim()).map_err(|error| {
        MirrorError::InvalidDeadline(deadline.id, format!("unreadable due_at: {error}"))
    })?;
    Ok(CalendarEvent {
        summary: event_summary(deadline, project_code),
        description: Some(event_description(deadline, project_code, base_url)),
        location: None,
        start,
        sequence: event_sequence(deadline)?,
        cancelled: deadline.status == project_deadlines::STATUS_CANCELLED,
        reminder_minutes: reminder_minutes(deadline)?,
        private_properties: BTreeMap::from([
            (MARKER_KEY.to_string(), MARKER_VALUE.to_string()),
            (DEADLINE_ID_KEY.to_string(), deadline.id.to_string()),
            (
                BODY_SHA_KEY.to_string(),
                body_sha(deadline, project_code, base_url),
            ),
        ]),
    })
}

/// The summary the ICS feed renders: the title, or the fallback by calendar
/// role and authority kind, then the Project code.
#[must_use]
pub fn event_summary(deadline: &Deadline, project_code: &str) -> String {
    let title = deadline.title.as_deref().map(str::trim).unwrap_or("");
    let action = if !title.is_empty() {
        title.to_string()
    } else if deadline.calendar_role == "hearing" {
        "Hearing".to_string()
    } else {
        let kind = match deadline.authority_kind.as_str() {
            "statute" => "Statutory deadline",
            "court_rule" => "Court rule deadline",
            "court_order" => "Court order deadline",
            "contract" => "Contract deadline",
            _ => "Internal deadline",
        };
        if deadline.calendar_role == "opponent" {
            format!("{kind} (opponent)")
        } else {
            kind.to_string()
        }
    };
    format!("{action} - {project_code}")
}

/// The description the ICS feed renders: the row's description, then the
/// authority, source document, deadline id, and matter-page lines.
#[must_use]
pub fn event_description(
    deadline: &Deadline,
    project_code: &str,
    base_url: Option<&str>,
) -> String {
    let mut description = deadline.description.clone().unwrap_or_default();
    if !deadline.authority.is_empty() {
        push_line(
            &mut description,
            &format!("Authority: {}", deadline.authority),
        );
    }
    if let Some(document) = deadline.source_document_id {
        push_line(&mut description, &format!("Source document: {document}"));
    }
    push_line(&mut description, &format!("Deadline: {}", deadline.id));
    if let Some(base_url) = base_url.map(str::trim).filter(|url| !url.is_empty()) {
        push_line(
            &mut description,
            &format!(
                "Matter page: {}/app/projects/{}",
                base_url.trim_end_matches('/'),
                project_code
            ),
        );
    }
    description
}

fn push_line(description: &mut String, line: &str) {
    if !description.is_empty() {
        description.push('\n');
    }
    description.push_str(line);
}

/// The row's sequence, with the ICS's fallback for rows written before
/// sequences were recorded: the updated instant, else the first write.
fn event_sequence(deadline: &Deadline) -> Result<i32, MirrorError> {
    let fallback = || {
        DateTime::parse_from_rfc3339(&deadline.updated_at)
            .map(|updated| updated.timestamp().max(0))
            .unwrap_or(0)
    };
    let sequence = deadline.calendar_sequence.unwrap_or_else(fallback);
    i32::try_from(sequence).map_err(|_| {
        MirrorError::InvalidDeadline(
            deadline.id,
            format!("calendar_sequence {sequence} is outside the RFC 5545 integer range"),
        )
    })
}

/// The one reminder an open deadline carries: its internal lead days as a
/// popup. A completed or cancelled deadline stays silent — a dead deadline
/// is a missed reminder.
fn reminder_minutes(deadline: &Deadline) -> Result<Option<i64>, MirrorError> {
    if deadline.status != project_deadlines::STATUS_OPEN {
        return Ok(None);
    }
    deadline
        .internal_lead_days
        .map(|days| {
            days.checked_mul(24 * 60).ok_or_else(|| {
                MirrorError::InvalidDeadline(
                    deadline.id,
                    format!("internal_lead_days {days} is too large for a reminder"),
                )
            })
        })
        .transpose()
}

/// The sha256 over the row content an event renders, hex-encoded. The
/// reconcile compares it against the recorded sha to tell drift from a
/// stale cache.
#[must_use]
pub fn body_sha(deadline: &Deadline, project_code: &str, base_url: Option<&str>) -> String {
    let payload = serde_json::json!({
        "title": deadline.title,
        "description": deadline.description,
        "due_at": deadline.due_at,
        "authority_kind": deadline.authority_kind,
        "authority": deadline.authority,
        "calendar_role": deadline.calendar_role,
        "status": deadline.status,
        "source_document_id": deadline.source_document_id.map(|id| id.to_string()),
        "internal_lead_days": deadline.internal_lead_days,
        "project_code": project_code,
        "base_url": base_url.map(str::trim).filter(|url| !url.is_empty()),
    });
    let digest = Sha256::digest(payload.to_string().as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::{
        body_sha, deadline_event, event_sequence, event_summary, reconcile, report_line,
        BODY_SHA_KEY, DEADLINE_ID_KEY, MARKER_KEY, MARKER_VALUE,
    };
    use crate::project_deadlines::{self, NewDeadline};
    use crate::projects::{create as create_project, NewProject};
    use crate::surreal::SurrealDb;
    use crate::test_support::mem_surreal;
    use chrono::TimeZone;
    use cloud::calendar::{CalendarService, FakeCalendar};
    use uuid::Uuid;

    fn sample(project_id: Uuid) -> NewDeadline {
        NewDeadline {
            project_id,
            title: Some("Answering brief due".into()),
            description: None,
            due_at: "2026-11-02T17:00:00-08:00".into(),
            authority_kind: "court_order".into(),
            authority: "Order setting briefing".into(),
            source: "lawyer".into(),
            source_document_id: None,
            owner_person_id: None,
            calendar_role: "firm".into(),
            internal_lead_days: Some(7),
            client_lead_days: None,
            replay_key: None,
        }
    }

    async fn project(surreal: &SurrealDb) -> Uuid {
        create_project(
            surreal,
            &NewProject {
                code: format!("mirror-{}", Uuid::now_v7()),
                name: "Mirror matter".into(),
                status: "open".into(),
                entity_id: Uuid::now_v7(),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .id
    }

    async fn closed_project(surreal: &SurrealDb) -> Uuid {
        create_project(
            surreal,
            &NewProject {
                code: format!("mirror-{}", Uuid::now_v7()),
                name: "Closed matter".into(),
                status: "closed".into(),
                entity_id: Uuid::now_v7(),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .id
    }

    #[tokio::test]
    async fn a_fresh_project_inserts_one_event_per_deadline() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let first = project_deadlines::create(&surreal, &sample(project_id))
            .await
            .unwrap();
        let mut hearing = sample(project_id);
        hearing.title = None;
        hearing.calendar_role = "hearing".into();
        hearing.internal_lead_days = None;
        let second = project_deadlines::create(&surreal, &hearing).await.unwrap();
        let calendar = FakeCalendar::default();
        let report = reconcile(
            &surreal,
            &calendar,
            project_id,
            Some("https://law.test"),
            false,
        )
        .await
        .unwrap();
        assert_eq!(report.outcome.slug(), "mirrored");
        assert!(!report.dry_run);
        assert_eq!(report.upserted, vec![first.id, second.id]);
        assert!(report.deleted.is_empty());
        assert_eq!(report.in_sync, 0);
        assert!(report.failures.is_empty());
        let project = crate::projects::find_by_id(&surreal, project_id)
            .await
            .unwrap()
            .unwrap();
        let calendar_id = project.calendar_id.unwrap();
        let events = calendar.events(&calendar_id);
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(
            |event| event.private_properties.get(MARKER_KEY) == Some(&MARKER_VALUE.to_string())
        ));
    }

    #[tokio::test]
    async fn a_second_pass_finds_everything_in_sync() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        project_deadlines::create(&surreal, &sample(project_id))
            .await
            .unwrap();
        let calendar = FakeCalendar::default();
        let first = reconcile(&surreal, &calendar, project_id, None, false)
            .await
            .unwrap();
        assert_eq!(first.upserted.len(), 1);
        let second = reconcile(&surreal, &calendar, project_id, None, false)
            .await
            .unwrap();
        assert_eq!(second.outcome.slug(), "mirrored");
        assert!(second.upserted.is_empty());
        assert_eq!(second.in_sync, 1);
        assert!(second.deleted.is_empty());
    }

    #[tokio::test]
    async fn an_edited_row_updates_its_event() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let created = project_deadlines::create(&surreal, &sample(project_id))
            .await
            .unwrap();
        let calendar = FakeCalendar::default();
        reconcile(&surreal, &calendar, project_id, None, false)
            .await
            .unwrap();
        project_deadlines::update(
            &surreal,
            project_id,
            created.id,
            &project_deadlines::DeadlinePatch {
                due_at: Some("2026-12-01T09:00:00-08:00".into()),
                ..project_deadlines::DeadlinePatch::default()
            },
        )
        .await
        .unwrap();
        let report = reconcile(&surreal, &calendar, project_id, None, false)
            .await
            .unwrap();
        assert_eq!(report.upserted, vec![created.id]);
        assert_eq!(report.in_sync, 0);
    }

    #[tokio::test]
    async fn a_row_deleted_from_the_store_deletes_its_event() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let created = project_deadlines::create(&surreal, &sample(project_id))
            .await
            .unwrap();
        let calendar = FakeCalendar::default();
        reconcile(&surreal, &calendar, project_id, None, false)
            .await
            .unwrap();
        surreal
            .query(format!("DELETE FROM project_deadlines WHERE id = $id"))
            .bind((
                "id",
                crate::surreal::record_id("project_deadlines", created.id),
            ))
            .await
            .unwrap()
            .check()
            .unwrap();
        let report = reconcile(&surreal, &calendar, project_id, None, false)
            .await
            .unwrap();
        assert_eq!(report.deleted.len(), 1);
        let project = crate::projects::find_by_id(&surreal, project_id)
            .await
            .unwrap()
            .unwrap();
        assert!(calendar.events(&project.calendar_id.unwrap()).is_empty());
    }

    #[tokio::test]
    async fn a_dry_run_plans_writes_and_writes_nothing() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let created = project_deadlines::create(&surreal, &sample(project_id))
            .await
            .unwrap();
        let calendar = FakeCalendar::default();
        let report = reconcile(&surreal, &calendar, project_id, None, true)
            .await
            .unwrap();
        assert!(report.dry_run);
        assert!(report.creates_calendar);
        assert_eq!(report.upserted, vec![created.id]);
        assert!(
            calendar.events("").is_empty(),
            "no calendar was created, so no events exist anywhere"
        );
    }

    #[tokio::test]
    async fn a_dry_run_over_an_existing_mirror_plans_the_same_diff() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let created = project_deadlines::create(&surreal, &sample(project_id))
            .await
            .unwrap();
        let calendar = FakeCalendar::default();
        reconcile(&surreal, &calendar, project_id, None, false)
            .await
            .unwrap();
        project_deadlines::update(
            &surreal,
            project_id,
            created.id,
            &project_deadlines::DeadlinePatch {
                due_at: Some("2026-12-15T09:00:00-08:00".into()),
                ..project_deadlines::DeadlinePatch::default()
            },
        )
        .await
        .unwrap();
        let report = reconcile(&surreal, &calendar, project_id, None, true)
            .await
            .unwrap();
        assert!(report.dry_run);
        assert!(!report.creates_calendar);
        assert_eq!(report.upserted, vec![created.id]);
        let project = crate::projects::find_by_id(&surreal, project_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            calendar.events(&project.calendar_id.unwrap()).len(),
            1,
            "the dry run wrote nothing"
        );
    }

    #[tokio::test]
    async fn a_closed_matter_is_skipped_without_writing() {
        let surreal = mem_surreal().await;
        let project_id = closed_project(&surreal).await;
        let calendar = FakeCalendar::default();
        let report = reconcile(&surreal, &calendar, project_id, None, false)
            .await
            .unwrap();
        assert_eq!(report.outcome.slug(), "skipped_closed");
        let project = crate::projects::find_by_id(&surreal, project_id)
            .await
            .unwrap()
            .unwrap();
        assert!(project.calendar_id.is_none());
    }

    #[tokio::test]
    async fn a_recorded_but_gone_calendar_is_reported_not_replaced() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        project_deadlines::create(&surreal, &sample(project_id))
            .await
            .unwrap();
        let calendar = FakeCalendar::default();
        let created = calendar.create_calendar("mirror-dead").await.unwrap();
        crate::projects::set_calendar_id(&surreal, project_id, &created.id)
            .await
            .unwrap();
        calendar.delete(&created.id);
        let report = reconcile(&surreal, &calendar, project_id, None, false)
            .await
            .unwrap();
        assert_eq!(report.outcome.slug(), "recorded_resource_missing");
        assert!(report.upserted.is_empty());
    }

    #[tokio::test]
    async fn the_schema_refuses_sequence_values_beyond_the_i32_range() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let created = project_deadlines::create(&surreal, &sample(project_id))
            .await
            .unwrap();
        let rejected = surreal
            .query("UPDATE $id SET calendar_sequence = 3000000000")
            .bind((
                "id",
                crate::surreal::record_id("project_deadlines", created.id),
            ))
            .await
            .unwrap()
            .check();
        assert!(rejected.is_err(), "the schema must cap calendar_sequence");
    }

    #[test]
    fn event_sequence_refuses_values_outside_the_rfc5545_range() {
        let mut over = stored(sample(Uuid::nil()));
        over.calendar_sequence = Some(3_000_000_000);
        let refused = event_sequence(&over).unwrap_err().to_string();
        assert!(refused.contains("outside the RFC 5545 integer range"));
        let mut at_cap = stored(sample(Uuid::nil()));
        at_cap.calendar_sequence = Some(2_147_483_647);
        assert_eq!(event_sequence(&at_cap).unwrap(), 2_147_483_647);
    }

    #[tokio::test]
    async fn the_summary_fallback_matches_the_ics_feed() {
        let surreal = mem_surreal().await;
        let project = crate::projects::find_by_id(&surreal, project(&surreal).await)
            .await
            .unwrap()
            .unwrap();
        let titled = project_deadlines::create(&surreal, &sample(project.id))
            .await
            .unwrap();
        assert_eq!(
            event_summary(&titled, &project.code),
            format!("Answering brief due - {}", project.code)
        );
        let mut hearing = sample(project.id);
        hearing.title = None;
        hearing.calendar_role = "hearing".into();
        let hearing = project_deadlines::create(&surreal, &hearing).await.unwrap();
        assert_eq!(
            event_summary(&hearing, &project.code),
            format!("Hearing - {}", project.code)
        );
        let mut opponent = sample(project.id);
        opponent.title = None;
        opponent.calendar_role = "opponent".into();
        opponent.authority_kind = "statute".into();
        let opponent = project_deadlines::create(&surreal, &opponent)
            .await
            .unwrap();
        assert_eq!(
            event_summary(&opponent, &project.code),
            format!("Statutory deadline (opponent) - {}", project.code)
        );
    }

    #[tokio::test]
    async fn the_event_carries_the_ics_basis_and_its_identity() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let created = project_deadlines::create(&surreal, &sample(project_id))
            .await
            .unwrap();
        let event = deadline_event(&created, "mirror-code", Some("https://law.test/")).unwrap();
        assert_eq!(event.summary, "Answering brief due - mirror-code");
        assert_eq!(
            event.start,
            chrono::FixedOffset::east_opt(-8 * 3600)
                .unwrap()
                .with_ymd_and_hms(2026, 11, 2, 17, 0, 0)
                .unwrap()
        );
        assert_eq!(event.sequence, 0);
        assert!(!event.cancelled);
        assert_eq!(event.reminder_minutes, Some(7 * 24 * 60));
        assert_eq!(
            event.description.as_deref(),
            Some(format!(
                "Authority: Order setting briefing\nDeadline: {}\nMatter page: https://law.test/app/projects/mirror-code",
                created.id
            ))
            .as_deref()
        );
        assert_eq!(
            event.private_properties.get(MARKER_KEY),
            Some(&MARKER_VALUE.to_string())
        );
        assert_eq!(
            event.private_properties.get(DEADLINE_ID_KEY),
            Some(&created.id.to_string())
        );
        assert_eq!(
            event.private_properties.get(BODY_SHA_KEY),
            Some(&body_sha(
                &created,
                "mirror-code",
                Some("https://law.test/")
            ))
        );
    }

    #[test]
    fn a_report_line_names_the_outcome_and_its_counts() {
        use crate::deadline_calendar::{MirrorOutcome, MirrorReport};
        let mut report = MirrorReport::default();
        report.upserted = vec![Uuid::now_v7(), Uuid::now_v7()];
        report.in_sync = 3;
        assert_eq!(
            report_line(&report),
            "mirrored upserted=2 in_sync=3 deleted=0"
        );
        report.failures = vec!["deadline …".into()];
        assert_eq!(
            report_line(&report),
            "mirrored upserted=2 in_sync=3 deleted=0 failed=1"
        );
        report.dry_run = true;
        report.outcome = MirrorOutcome::SkippedClosed;
        assert_eq!(
            report_line(&report),
            "skipped_closed upserted=2 in_sync=3 deleted=0 failed=1 (dry run)"
        );
    }

    #[test]
    fn a_cancelled_row_renders_a_cancelled_event_without_a_reminder() {
        let mut deadline = stored(sample(Uuid::now_v7()));
        deadline.status = project_deadlines::STATUS_CANCELLED.into();
        deadline.internal_lead_days = Some(7);
        let event = deadline_event(&deadline, "mirror-code", None).unwrap();
        assert!(event.cancelled);
        assert_eq!(event.reminder_minutes, None);
    }

    #[test]
    fn a_completed_row_stays_on_the_calendar_as_confirmed() {
        let mut deadline = stored(sample(Uuid::now_v7()));
        deadline.status = project_deadlines::STATUS_COMPLETE.into();
        let event = deadline_event(&deadline, "mirror-code", None).unwrap();
        assert!(!event.cancelled);
        assert_eq!(event.reminder_minutes, None);
    }

    /// A stored-shaped row: the store's own normalization applied to a
    /// sample, for tests that need the row the store would read back.
    fn stored(deadline: NewDeadline) -> crate::project_deadlines::Deadline {
        crate::project_deadlines::Deadline {
            id: Uuid::now_v7(),
            project_id: deadline.project_id,
            title: deadline.title,
            description: deadline.description,
            due_at: deadline.due_at.clone(),
            authority_kind: deadline.authority_kind.clone(),
            authority: deadline.authority.clone(),
            source: deadline.source.clone(),
            source_document_id: deadline.source_document_id,
            owner_person_id: deadline.owner_person_id,
            status: project_deadlines::STATUS_OPEN.into(),
            calendar_role: deadline.calendar_role.clone(),
            internal_lead_days: deadline.internal_lead_days,
            client_lead_days: deadline.client_lead_days,
            replay_key: deadline.replay_key,
            inserted_at: "2026-10-30T00:00:00+00:00".into(),
            updated_at: "2026-10-30T00:00:00+00:00".into(),
            calendar_sequence: Some(0),
        }
    }
}

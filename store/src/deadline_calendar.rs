//! The Deadline mirror onto a Project's shared Google Calendar.
//!
//! Each open or completed Deadline renders as one event with its authority,
//! matter-page link, one-minute block, and configured reminder. Cancelling a
//! Deadline removes the managed event. The ICS feed retains its cancellation.
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
use std::fmt::Write as _;

use chrono::DateTime;
use cloud::calendar::{CalendarError, CalendarEvent, CalendarService, StoredEvent};
use model::project_deadlines::{is_appearance, SchedulingPolicy};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::project_calendar::{self, CalendarOutcome, ProjectCalendarError};
use crate::project_deadlines::{self, Deadline};
use crate::projects::{self, Project};

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
    /// The recorded id cannot be verified as this Project's calendar. A person
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
    /// Provider verification detail for a recorded calendar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar_problem: Option<String>,
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

/// IDs and codes of open Projects in stable directory order for the scheduled mirror.
pub async fn open_projects() -> Result<Vec<(Uuid, String)>, MirrorError> {
    let pg = crate::postgres::PostgresDb::application()
        .await
        .map_err(|error| MirrorError::Db(error.to_string()))?;
    sqlx::query_as("SELECT id, code FROM project WHERE status = 'open' ORDER BY name, id")
        .fetch_all(pg.pool())
        .await
        .map_err(|error| MirrorError::Db(error.to_string()))
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
    application_store: &ApplicationStore,
    calendar: &dyn CalendarService,
    project_id: Uuid,
    base_url: Option<&str>,
    dry_run: bool,
) -> Result<MirrorReport, MirrorError> {
    let project = find_project(application_store, project_id).await?;
    if project.status != "open" {
        return Ok(MirrorReport {
            outcome: MirrorOutcome::SkippedClosed,
            ..MirrorReport::empty(MirrorOutcome::SkippedClosed, dry_run)
        });
    }
    let recorded = recorded_calendar_id(&project);
    if dry_run {
        return if let Some(calendar_id) = recorded {
            if let Some(problem) = calendar
                .recorded_calendar_problem(&project.code, &calendar_id)
                .await?
            {
                Ok(MirrorReport {
                    calendar_problem: Some(problem.into()),
                    ..MirrorReport::empty(MirrorOutcome::RecordedResourceMissing, dry_run)
                })
            } else {
                reconcile_rows(
                    application_store,
                    calendar,
                    &calendar_id,
                    &project,
                    base_url,
                    dry_run,
                )
                .await
            }
        } else {
            // No calendar yet: a first pass would create one and insert
            // every row. That plan needs no calendar to read.
            let mut report = MirrorReport::empty(MirrorOutcome::Mirrored, dry_run);
            report.creates_calendar = true;
            plan_rows(application_store, &project, base_url, &mut report).await?;
            Ok(report)
        };
    }
    let ensured = ensure_calendar(application_store, project_id, calendar).await?;
    if ensured.outcome == CalendarOutcome::RecordedResourceMissing {
        return Ok(MirrorReport::empty(
            MirrorOutcome::RecordedResourceMissing,
            dry_run,
        ));
    }
    // The calendar pass recorded the id before sharing; read it back.
    let project = find_project(application_store, project_id).await?;
    let Some(calendar_id) = recorded_calendar_id(&project) else {
        return Err(MirrorError::Db(format!(
            "matter {project_id} has no calendar id after the calendar pass"
        )));
    };
    reconcile_rows(
        application_store,
        calendar,
        &calendar_id,
        &project,
        base_url,
        dry_run,
    )
    .await
}

/// The diff half of a pass, shared by the applied and dry-run paths: read
/// the rows and the mirrored events, then write (or plan) the difference.
async fn reconcile_rows(
    application_store: &ApplicationStore,
    calendar: &dyn CalendarService,
    calendar_id: &str,
    project: &Project,
    base_url: Option<&str>,
    dry_run: bool,
) -> Result<MirrorReport, MirrorError> {
    let mut report = MirrorReport::empty(MirrorOutcome::Mirrored, dry_run);
    let rows = project_deadlines::list(application_store, project.id)
        .await
        .map_err(|error| MirrorError::Db(error.to_string()))?;
    let stored = calendar
        .mirrored_events(calendar_id, MARKER_KEY, MARKER_VALUE)
        .await?;
    let policy = project_deadlines::scheduling_policy(project.firm_id)
        .await
        .map_err(|error| MirrorError::Db(error.to_string()))?;
    let by_deadline: BTreeMap<&String, &StoredEvent> = stored
        .iter()
        .filter_map(|event| {
            event
                .private_properties
                .get(DEADLINE_ID_KEY)
                .map(|id| (id, event))
        })
        .collect();
    let mut live: BTreeSet<String> = BTreeSet::new();
    for deadline in rows
        .iter()
        .filter(|row| row.status != project_deadlines::STATUS_CANCELLED)
    {
        let id = deadline.id.to_string();
        live.insert(id.clone());
        if deadline.calendar_review_pending {
            continue;
        }
        let sha = body_sha_with_policy(deadline, &project.code, base_url, policy);
        let event = match event_with_policy(deadline, &project.code, base_url, policy) {
            Ok(event) => event,
            Err(error) => {
                report.failures.push(error.to_string());
                continue;
            }
        };
        match by_deadline.get(&id) {
            Some(stored) => {
                let source_unchanged = stored
                    .private_properties
                    .get(BODY_SHA_KEY)
                    .is_some_and(|recorded| *recorded == sha);
                if source_unchanged && (stored.can_write() || !stored.has_baseline()) {
                    report.in_sync += 1;
                } else if !stored.can_write() {
                    report.failures.push(format!(
                        "event {}: {}",
                        stored.id,
                        CalendarError::UnverifiedEvent
                    ));
                } else if dry_run {
                    report.upserted.push(deadline.id);
                } else if let Err(error) = calendar.patch_event(calendar_id, stored, &event).await {
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
    for event in stored {
        let Some(deadline_id) = event.private_properties.get(DEADLINE_ID_KEY) else {
            continue;
        };
        if live.contains(deadline_id) {
            continue;
        }
        if !event.can_write() {
            report.failures.push(format!(
                "event {}: {}",
                event.id,
                CalendarError::UnverifiedEvent
            ));
        } else if dry_run {
            report.deleted.push(event.id.clone());
        } else if let Err(error) = calendar.delete_event(calendar_id, &event).await {
            report.failures.push(format!("event {}: {error}", event.id));
        } else {
            report.deleted.push(event.id.clone());
        }
    }
    Ok(report)
}

/// The plan a first pass would make, before the Project has a calendar.
async fn plan_rows(
    application_store: &ApplicationStore,
    project: &Project,
    base_url: Option<&str>,
    report: &mut MirrorReport,
) -> Result<(), MirrorError> {
    let rows = project_deadlines::list(application_store, project.id)
        .await
        .map_err(|error| MirrorError::Db(error.to_string()))?;
    let policy = project_deadlines::scheduling_policy(project.firm_id)
        .await
        .map_err(|error| MirrorError::Db(error.to_string()))?;
    for deadline in rows.iter().filter(|row| {
        !row.calendar_review_pending && row.status != project_deadlines::STATUS_CANCELLED
    }) {
        match event_with_policy(deadline, &project.code, base_url, policy) {
            Ok(_) => report.upserted.push(deadline.id),
            Err(error) => report.failures.push(error.to_string()),
        }
    }
    Ok(())
}

/// Ensure the Project's calendar exists before mirroring events.
async fn ensure_calendar(
    application_store: &ApplicationStore,
    project_id: Uuid,
    calendar: &dyn CalendarService,
) -> Result<project_calendar::CalendarReport, MirrorError> {
    project_calendar::ensure(application_store, project_id, calendar)
        .await
        .map_err(|error| match error {
            ProjectCalendarError::Calendar(calendar_error)
            | ProjectCalendarError::Participant {
                source: calendar_error,
                ..
            } => MirrorError::Calendar(calendar_error),
            ProjectCalendarError::NotFound => MirrorError::NotFound,
            ProjectCalendarError::Command(command) => MirrorError::Db(command.to_string()),
            ProjectCalendarError::Db(message) => MirrorError::Db(message),
        })
}

async fn find_project(
    application_store: &ApplicationStore,
    project_id: Uuid,
) -> Result<Project, MirrorError> {
    projects::find_by_id(application_store, project_id)
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
            calendar_problem: None,
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
    if let Some(problem) = &report.calendar_problem {
        let _ = write!(line, " {problem}");
    }
    if !report.failures.is_empty() {
        let _ = write!(line, " failed={}", report.failures.len());
    }
    if report.dry_run {
        line.push_str(" (dry run)");
    }
    line
}

/// The fixed category-to-Google-Calendar-color map. Every matter calendar
/// renders the same category in the same color; a row without a category
/// (or with an unknown one) leaves the calendar's default color.
#[must_use]
pub fn category_color(category: Option<&str>) -> Option<&'static str> {
    match category {
        Some("court") => Some("11"),                   // Tomato.
        Some("hearing" | "trial_day") => Some("3"),    // Grape.
        Some("discovery" | "deposition") => Some("5"), // Banana.
        Some("internal" | "tickler") => Some("8"),     // Graphite.
        _ => None,
    }
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
    event_with_policy(
        deadline,
        project_code,
        base_url,
        SchedulingPolicy::default(),
    )
}

fn event_with_policy(
    deadline: &Deadline,
    project_code: &str,
    base_url: Option<&str>,
    policy: SchedulingPolicy,
) -> Result<CalendarEvent, MirrorError> {
    let start = DateTime::parse_from_rfc3339(deadline.due_at.trim()).map_err(|error| {
        MirrorError::InvalidDeadline(deadline.id, format!("unreadable due_at: {error}"))
    })?;
    let appearance = is_appearance(
        &deadline.calendar_role,
        deadline
            .category
            .as_deref()
            .and_then(|category| category.parse().ok()),
    );
    let window = (appearance && deadline.status == project_deadlines::STATUS_OPEN)
        .then(|| {
            policy.appearance(
                start,
                deadline
                    .hearing
                    .as_ref()
                    .map_or(60, model::project_deadlines::Hearing::minutes),
            )
        })
        .flatten();
    let submit_by = (!appearance && deadline.status == project_deadlines::STATUS_OPEN)
        .then(|| policy.submit_by(start, deadline.due_on.as_ref()))
        .flatten();
    let mut description = event_description(deadline, project_code, base_url);
    if let Some((from, until)) = window {
        push_line(
            &mut description,
            &format!(
                "Starts {}. Busy from {} until {}.",
                start.to_rfc3339(),
                from.to_rfc3339(),
                until.to_rfc3339()
            ),
        );
    }
    let mut reminders = reminder_minutes(deadline)?;
    if let Some(target) = submit_by {
        push_line(
            &mut description,
            &format!("Submit by {}", target.to_rfc3339()),
        );
        if deadline.due_on.is_none() {
            reminders.push(policy.buffer_minutes());
        }
    }
    Ok(CalendarEvent {
        summary: event_summary(deadline, project_code),
        description: Some(description),
        location: deadline
            .hearing
            .as_ref()
            .and_then(|hearing| hearing.location.clone()),
        color_id: category_color(deadline.category.as_deref()).map(str::to_string),
        start: window.map_or(start, |(from, _)| from),
        timing: if let Some((from, until)) = window {
            cloud::calendar::EventTiming::Hearing(
                u32::try_from((until - from).num_minutes()).map_err(|error| {
                    MirrorError::InvalidDeadline(deadline.id, error.to_string())
                })?,
            )
        } else if let Some(day) = &deadline.due_on {
            cloud::calendar::EventTiming::AllDay(day.date)
        } else if deadline.status == project_deadlines::STATUS_OPEN {
            cloud::calendar::EventTiming::Due
        } else {
            cloud::calendar::EventTiming::Instant
        },
        sequence: event_sequence(deadline)?,
        cancelled: deadline.status == project_deadlines::STATUS_CANCELLED,
        reminder_minutes: reminders,
        private_properties: BTreeMap::from([
            (MARKER_KEY.to_string(), MARKER_VALUE.to_string()),
            (DEADLINE_ID_KEY.to_string(), deadline.id.to_string()),
            (
                BODY_SHA_KEY.to_string(),
                body_sha_with_policy(deadline, project_code, base_url, policy),
            ),
        ]),
    })
}

/// The summary the ICS feed renders: the title, or the fallback by calendar
/// role and authority kind, prefixed by the Project code.
#[must_use]
pub fn event_summary(deadline: &Deadline, project_code: &str) -> String {
    model::project_deadlines::calendar_summary(
        project_code,
        deadline.title.as_deref().unwrap_or_default(),
        &deadline.calendar_role,
        &deadline.authority_kind,
        deadline
            .category
            .as_deref()
            .and_then(|value| value.parse().ok()),
        &deadline.status,
    )
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
    if let Some(cutoff) = deadline
        .due_on
        .as_ref()
        .and_then(model::project_deadlines::DueOn::cutoff_description)
    {
        push_line(&mut description, &cutoff);
    }
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
    if let Some(origin) = calendar_origin(base_url) {
        push_line(
            &mut description,
            &format!("Matter page: {origin}/app/projects/{project_code}"),
        );
    }
    description
}

fn calendar_origin(base: Option<&str>) -> Option<String> {
    let base = base?.trim();
    let url = if base.contains("://") {
        url::Url::parse(base)
    } else {
        url::Url::parse(&format!("https://{base}"))
    }
    .ok()?;
    (matches!(url.scheme(), "https" | "http") && url.has_host())
        .then(|| url.origin().ascii_serialization())
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
            .map_or(0, |updated| updated.timestamp().max(0))
    };
    let sequence = deadline.calendar_sequence.unwrap_or_else(fallback);
    i32::try_from(sequence).map_err(|_| {
        MirrorError::InvalidDeadline(
            deadline.id,
            format!("calendar_sequence {sequence} is outside the RFC 5545 integer range"),
        )
    })
}

/// Court popup reminders and an open Deadline’s explicit internal lead.
fn reminder_minutes(deadline: &Deadline) -> Result<Vec<i64>, MirrorError> {
    model::project_deadlines::reminder_days(
        &deadline.status,
        deadline.category.as_deref().and_then(|s| s.parse().ok()),
        deadline.internal_lead_days,
    )
    .into_iter()
    .map(|days| {
        days.checked_mul(24 * 60).ok_or_else(|| {
            MirrorError::InvalidDeadline(
                deadline.id,
                format!("internal_lead_days {days} is too large for a reminder"),
            )
        })
    })
    .collect()
}

/// The sha256 over the row content an event renders, hex-encoded. The
/// reconcile compares it against the recorded sha to tell drift from a
/// stale cache.
#[must_use]
pub fn body_sha(deadline: &Deadline, project_code: &str, base_url: Option<&str>) -> String {
    body_sha_with_policy(
        deadline,
        project_code,
        base_url,
        SchedulingPolicy::default(),
    )
}

fn body_sha_with_policy(
    deadline: &Deadline,
    project_code: &str,
    base_url: Option<&str>,
    policy: SchedulingPolicy,
) -> String {
    let mut payload = serde_json::json!({
        "title": deadline.title,
        "description": deadline.description,
        "due_at": deadline.due_at,
        "authority_kind": deadline.authority_kind,
        "authority": deadline.authority,
        "calendar_role": deadline.calendar_role,
        "category": deadline.category,
        "status": deadline.status,
        "source_document_id": deadline.source_document_id.map(|id| id.to_string()),
        "internal_lead_days": deadline.internal_lead_days,
        "project_code": project_code,
        "base_url": calendar_origin(base_url),
    });
    // Reformatting a title alone must not trigger a full rewrite of existing Google events.
    if let Some(date) = &deadline.due_on {
        payload["due_on"] = serde_json::json!(date);
    }
    if let Some(hearing) = &deadline.hearing {
        payload["hearing"] = serde_json::json!(hearing);
    }
    // Open timed Deadlines hold Busy time; the key rewrites events mirrored while they showed as Free.
    if deadline.due_on.is_none() && deadline.status == project_deadlines::STATUS_OPEN {
        payload["busy"] = serde_json::json!(true);
    }
    if deadline.status == project_deadlines::STATUS_OPEN {
        payload["deadline_buffer_minutes"] = serde_json::json!(policy);
    }
    let digest = Sha256::digest(payload.to_string().as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in &digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::{
        body_sha, category_color, deadline_event, event_sequence, event_summary, reconcile,
        report_line, BODY_SHA_KEY, DEADLINE_ID_KEY, MARKER_KEY, MARKER_VALUE,
    };
    use crate::application_store::ApplicationStore;
    use crate::project_deadlines::{self, NewDeadline};
    use chrono::{DateTime, TimeZone};
    use cloud::calendar::{CalendarService, FakeCalendar};
    use uuid::Uuid;

    fn sample(project_id: Uuid) -> NewDeadline {
        NewDeadline {
            project_id,
            title: Some("Answering brief due".into()),
            description: None,
            hearing: None,
            effort_minutes: None,
            due_on: None,
            due_at: "2026-11-02T17:00:00-08:00".into(),
            authority_kind: "court_order".into(),
            authority: "Order setting briefing".into(),
            source: "lawyer".into(),
            source_document_id: None,
            owner_person_id: None,
            calendar_role: "firm".into(),
            category: Some("court".into()),
            internal_lead_days: Some(7),
            client_lead_days: None,
            replay_key: None,
        }
    }

    fn internal_task(project_id: Uuid) -> NewDeadline {
        NewDeadline {
            title: Some("Review the intake checklist".into()),
            authority_kind: "internal".into(),
            authority: "Matter work plan".into(),
            category: Some("internal".into()),
            ..sample(project_id)
        }
    }

    /// A court date published before the review gate was introduced.
    async fn previously_published(
        db: &ApplicationStore,
        input: &NewDeadline,
    ) -> project_deadlines::Deadline {
        let row = project_deadlines::create(db, input).await.unwrap();
        let pg = crate::postgres::PostgresDb::application().await.unwrap();
        sqlx::query("UPDATE project_deadlines SET calendar_review_pending=false WHERE id=$1")
            .bind(row.id)
            .execute(pg.pool())
            .await
            .unwrap();
        project_deadlines::find(db, row.project_id, row.id)
            .await
            .unwrap()
            .unwrap()
    }

    async fn with_project(test: impl AsyncFnOnce(&ApplicationStore, Uuid)) {
        crate::test_support::with_postgres(async {
            let db = ApplicationStore::uninitialized();
            let (project_id, _) = crate::test_support::seed_calendar_project().await;
            test(&db, project_id).await;
        })
        .await;
    }

    #[tokio::test]
    async fn mirror_roster_returns_only_open_projects_in_directory_order() {
        crate::test_support::with_postgres(async {
            let (seed, _) = crate::test_support::seed_calendar_project().await;
            let pg = crate::postgres::PostgresDb::application().await.unwrap();
            for (code, name, status) in [
                ("second-project", "Aardvark", "open"),
                ("closed-project", "Closed project", "closed"),
                ("archived-project", "Archived project", "archived"),
            ] {
                sqlx::query(
                    "INSERT INTO project (code, name, status, entity_id)
                     SELECT $1, $2, $3, entity_id FROM project WHERE id = $4",
                )
                .bind(code)
                .bind(name)
                .bind(status)
                .bind(seed)
                .execute(pg.pool())
                .await
                .unwrap();
            }
            let rows = super::open_projects().await.unwrap();
            let codes: Vec<_> = rows.iter().map(|(_, code)| code.as_str()).collect();
            assert_eq!(codes, ["second-project", "calendar-test"]);
            assert_eq!(rows[1].0, seed);
        })
        .await;
    }

    #[test]
    fn each_category_has_its_fixed_color_and_nothing_else_does() {
        assert_eq!(category_color(Some("court")), Some("11"));
        assert_eq!(category_color(Some("hearing")), Some("3"));
        assert_eq!(category_color(Some("discovery")), Some("5"));
        assert_eq!(category_color(Some("internal")), Some("8"));
        assert_eq!(category_color(None), None);
        assert_eq!(category_color(Some("filing")), None);
    }

    #[tokio::test]
    async fn a_mirrored_event_carries_its_category_color() {
        with_project(async |db, project_id| {
            let created = project_deadlines::create(db, &sample(project_id))
                .await
                .unwrap();
            let event = deadline_event(&created, "mirror-code", None).unwrap();
            assert_eq!(event.color_id.as_deref(), Some("11"));

            // A row whose category is cleared mirrors without a color, leaving
            // the calendar's default.
            let cleared = project_deadlines::update(
                db,
                project_id,
                created.id,
                &project_deadlines::DeadlinePatch {
                    category: Some(None),
                    ..project_deadlines::DeadlinePatch::default()
                },
            )
            .await
            .unwrap();
            let event = deadline_event(&cleared, "mirror-code", None).unwrap();
            assert_eq!(event.color_id, None);
        })
        .await;
    }

    #[tokio::test]
    async fn a_fresh_project_inserts_one_event_per_deadline() {
        with_project(async |db, project_id| {
            let first = previously_published(db, &sample(project_id)).await;
            let mut hearing = sample(project_id);
            hearing.title = None;
            hearing.calendar_role = "hearing".into();
            hearing.internal_lead_days = None;
            let second = previously_published(db, &hearing).await;
            let calendar = FakeCalendar::default();
            let report = reconcile(db, &calendar, project_id, Some("https://law.test"), false)
                .await
                .unwrap();
            assert_eq!(report.outcome.slug(), "mirrored");
            assert!(!report.dry_run);
            assert_eq!(report.upserted, vec![first.id, second.id]);
            assert!(report.deleted.is_empty());
            assert_eq!(report.in_sync, 0);
            assert!(report.failures.is_empty());
            let project = crate::projects::find_by_id(db, project_id)
                .await
                .unwrap()
                .unwrap();
            let calendar_id = project.calendar_id.unwrap();
            let events = calendar.events(&calendar_id);
            assert_eq!(events.len(), 2);
            assert!(events
                .iter()
                .all(|event| event.private_properties.get(MARKER_KEY)
                    == Some(&MARKER_VALUE.to_string())));
        })
        .await;
    }

    #[tokio::test]
    async fn a_pending_court_date_neither_creates_an_event_nor_deletes_a_previous_mirror() {
        with_project(async |db, project_id| {
            let row = project_deadlines::create(db, &sample(project_id))
                .await
                .unwrap();
            assert!(row.calendar_review_pending);
            let calendar = FakeCalendar::default();
            for dry_run in [true, false] {
                let report = reconcile(db, &calendar, project_id, None, dry_run)
                    .await
                    .unwrap();
                assert!(
                    report.upserted.is_empty()
                        && report.deleted.is_empty()
                        && report.failures.is_empty()
                );
            }
            let project = crate::projects::find_by_id(db, project_id)
                .await
                .unwrap()
                .unwrap();
            let calendar_id = project.calendar_id.unwrap();
            assert!(calendar.events(&calendar_id).is_empty());
            // Protect an already published provider copy during review even when its source changed.
            let mut prior = deadline_event(&row, &project.code, None).unwrap();
            prior.summary = "calendar-test: Previously published court date".into();
            prior
                .private_properties
                .insert(BODY_SHA_KEY.into(), "prior-source-digest".into());
            let previous = calendar.insert_event(&calendar_id, &prior).await.unwrap();
            for dry_run in [true, false] {
                let report = reconcile(db, &calendar, project_id, None, dry_run)
                    .await
                    .unwrap();
                assert!(
                    report.upserted.is_empty()
                        && report.deleted.is_empty()
                        && report.failures.is_empty()
                );
            }
            assert_eq!(calendar.events(&calendar_id), [previous]);
        })
        .await;
    }

    #[tokio::test]
    async fn a_second_pass_finds_everything_in_sync() {
        with_project(async |db, project_id| {
            previously_published(db, &sample(project_id)).await;
            let calendar = FakeCalendar::default();
            let first = reconcile(db, &calendar, project_id, None, false)
                .await
                .unwrap();
            assert_eq!(first.upserted.len(), 1);
            let second = reconcile(db, &calendar, project_id, None, false)
                .await
                .unwrap();
            assert_eq!(second.outcome.slug(), "mirrored");
            assert!(second.upserted.is_empty());
            assert_eq!(second.in_sync, 1);
            assert!(second.deleted.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn an_edited_row_updates_its_event() {
        with_project(async |db, project_id| {
            let created = project_deadlines::create(db, &internal_task(project_id))
                .await
                .unwrap();
            let calendar = FakeCalendar::default();
            reconcile(db, &calendar, project_id, None, false)
                .await
                .unwrap();
            project_deadlines::update(
                db,
                project_id,
                created.id,
                &project_deadlines::DeadlinePatch {
                    due_at: Some("2026-12-01T09:00:00-08:00".into()),
                    ..project_deadlines::DeadlinePatch::default()
                },
            )
            .await
            .unwrap();
            let report = reconcile(db, &calendar, project_id, None, false)
                .await
                .unwrap();
            assert_eq!(report.upserted, vec![created.id]);
            assert_eq!(report.in_sync, 0);
        })
        .await;
    }

    #[tokio::test]
    async fn direct_calendar_edits_and_manual_meetings_survive_source_updates_and_cancellation() {
        with_project(async |db, project_id| {
            let row = project_deadlines::create(db, &internal_task(project_id))
                .await
                .unwrap();
            let calendar = FakeCalendar::default();
            reconcile(db, &calendar, project_id, None, false)
                .await
                .unwrap();
            let project = crate::projects::find_by_id(db, project_id)
                .await
                .unwrap()
                .unwrap();
            let calendar_id = project.calendar_id.as_ref().unwrap();
            let stored = calendar.events(calendar_id).pop().unwrap();
            let mut direct = deadline_event(&row, &project.code, None).unwrap();
            direct.private_properties = stored.private_properties.clone();
            direct.description = Some("Recording and attorney notes".into());
            calendar.put_event(calendar_id, &stored.id, direct.clone());
            direct.private_properties.clear();
            direct.summary = "calendar-test: Client call".into();
            calendar.put_event(calendar_id, "manual-call", direct);
            project_deadlines::update(
                db,
                project_id,
                row.id,
                &project_deadlines::DeadlinePatch {
                    due_at: Some("2026-12-01T09:00:00-08:00".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            for cancelled in [false, true] {
                if cancelled {
                    project_deadlines::cancel(db, project_id, row.id)
                        .await
                        .unwrap();
                }
                for dry_run in [true, false] {
                    let report = reconcile(db, &calendar, project_id, None, dry_run)
                        .await
                        .unwrap();
                    assert!(report.upserted.is_empty() && report.deleted.is_empty());
                    assert_eq!(report.failures.len(), 1);
                    assert!(report.failures[0].contains(&stored.id));
                }
                assert_eq!(calendar.events(calendar_id).len(), 2);
                assert!(calendar
                    .events(calendar_id)
                    .iter()
                    .any(|event| event.id == "manual-call"));
            }
        })
        .await;
    }

    #[tokio::test]
    async fn legacy_events_wait_for_review_when_the_source_changes() {
        with_project(async |db, project_id| {
            let row = previously_published(db, &sample(project_id)).await;
            let calendar = FakeCalendar::default();
            reconcile(db, &calendar, project_id, None, false)
                .await
                .unwrap();
            let project = crate::projects::find_by_id(db, project_id)
                .await
                .unwrap()
                .unwrap();
            let calendar_id = project.calendar_id.as_ref().unwrap();
            let id = calendar.events(calendar_id).pop().unwrap().id;
            calendar.put_event(
                calendar_id,
                &id,
                deadline_event(&row, &project.code, None).unwrap(),
            );
            let no_change = reconcile(db, &calendar, project_id, None, false)
                .await
                .unwrap();
            assert!(no_change.failures.is_empty() && no_change.upserted.is_empty());
            project_deadlines::cancel(db, project_id, row.id)
                .await
                .unwrap();
            let changed = reconcile(db, &calendar, project_id, None, false)
                .await
                .unwrap();
            assert_eq!(changed.failures.len(), 1);
            assert!(changed.deleted.is_empty());
            assert_eq!(calendar.events(calendar_id)[0].id, id);
        })
        .await;
    }

    #[tokio::test]
    async fn cancelled_deadlines_converge_before_and_after_publication() {
        with_project(async |db, project_id| {
            let calendar = FakeCalendar::default();
            let open = previously_published(db, &sample(project_id)).await;
            let early = previously_published(db, &sample(project_id)).await;
            project_deadlines::cancel(db, project_id, early.id)
                .await
                .unwrap();
            for dry_run in [true, false] {
                let pass = reconcile(db, &calendar, project_id, None, dry_run)
                    .await
                    .unwrap();
                assert_eq!(pass.upserted, [open.id]);
            }
            project_deadlines::cancel(db, project_id, open.id)
                .await
                .unwrap();
            for dry_run in [true, false] {
                let pass = reconcile(db, &calendar, project_id, None, dry_run)
                    .await
                    .unwrap();
                assert!(pass.upserted.is_empty());
                assert_eq!(pass.deleted.len(), 1);
            }
            let pass = reconcile(db, &calendar, project_id, None, true)
                .await
                .unwrap();
            assert!(pass.upserted.is_empty() && pass.deleted.is_empty());
            let project = crate::projects::find_by_id(db, project_id)
                .await
                .unwrap()
                .unwrap();
            assert!(calendar.events(&project.calendar_id.unwrap()).is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn a_row_deleted_from_the_store_deletes_its_event() {
        with_project(async |db, project_id| {
            let created = previously_published(db, &sample(project_id)).await;
            let calendar = FakeCalendar::default();
            reconcile(db, &calendar, project_id, None, false)
                .await
                .unwrap();
            let pg = crate::postgres::PostgresDb::application().await.unwrap();
            sqlx::query("DELETE FROM project_deadlines WHERE id = $1")
                .bind(created.id)
                .execute(pg.pool())
                .await
                .unwrap();
            let report = reconcile(db, &calendar, project_id, None, false)
                .await
                .unwrap();
            assert_eq!(report.deleted.len(), 1);
            let project = crate::projects::find_by_id(db, project_id)
                .await
                .unwrap()
                .unwrap();
            assert!(calendar.events(&project.calendar_id.unwrap()).is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn a_dry_run_plans_writes_and_writes_nothing() {
        with_project(async |db, project_id| {
            let created = previously_published(db, &sample(project_id)).await;
            let calendar = FakeCalendar::default();
            let report = reconcile(db, &calendar, project_id, None, true)
                .await
                .unwrap();
            assert!(report.dry_run);
            assert!(report.creates_calendar);
            assert_eq!(report.upserted, vec![created.id]);
            assert!(
                calendar.events("").is_empty(),
                "no calendar was created, so no events exist anywhere"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_dry_run_over_an_existing_mirror_plans_the_same_diff() {
        with_project(async |db, project_id| {
            let created = project_deadlines::create(db, &internal_task(project_id))
                .await
                .unwrap();
            let calendar = FakeCalendar::default();
            reconcile(db, &calendar, project_id, None, false)
                .await
                .unwrap();
            project_deadlines::update(
                db,
                project_id,
                created.id,
                &project_deadlines::DeadlinePatch {
                    due_at: Some("2026-12-15T09:00:00-08:00".into()),
                    ..project_deadlines::DeadlinePatch::default()
                },
            )
            .await
            .unwrap();
            let report = reconcile(db, &calendar, project_id, None, true)
                .await
                .unwrap();
            assert!(report.dry_run);
            assert!(!report.creates_calendar);
            assert_eq!(report.upserted, vec![created.id]);
            let project = crate::projects::find_by_id(db, project_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                calendar.events(&project.calendar_id.unwrap()).len(),
                1,
                "the dry run wrote nothing"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_closed_matter_is_skipped_without_writing() {
        with_project(async |db, project_id| {
            let pg = crate::postgres::PostgresDb::application().await.unwrap();
            sqlx::query("UPDATE project SET status = 'closed' WHERE id = $1")
                .bind(project_id)
                .execute(pg.pool())
                .await
                .unwrap();
            let calendar = FakeCalendar::default();
            let report = reconcile(db, &calendar, project_id, None, false)
                .await
                .unwrap();
            assert_eq!(report.outcome.slug(), "skipped_closed");
            let project = crate::projects::find_by_id(db, project_id)
                .await
                .unwrap()
                .unwrap();
            assert!(project.calendar_id.is_none());
        })
        .await;
    }

    #[tokio::test]
    async fn a_recorded_but_gone_calendar_is_reported_not_replaced() {
        with_project(async |db, project_id| {
            project_deadlines::create(db, &sample(project_id))
                .await
                .unwrap();
            let calendar = FakeCalendar::default();
            let created = calendar
                .create_calendar("mirror-dead", "UTC")
                .await
                .unwrap();
            crate::projects::set_calendar_id(db, project_id, &created.id)
                .await
                .unwrap();
            calendar.delete(&created.id);
            let report = reconcile(db, &calendar, project_id, None, false)
                .await
                .unwrap();
            assert_eq!(report.outcome.slug(), "recorded_resource_missing");
            assert!(report.upserted.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn calendar_sequence_increments_on_rapid_updates_and_cancellation() {
        with_project(async |db, project_id| {
            let row = project_deadlines::create(db, &sample(project_id))
                .await
                .unwrap();
            assert_eq!(row.calendar_sequence, Some(0));
            for sequence in 1..=3 {
                let patch = project_deadlines::DeadlinePatch {
                    title: Some(Some(format!("Updated deadline {sequence}"))),
                    ..Default::default()
                };
                let updated = project_deadlines::update(db, project_id, row.id, &patch)
                    .await
                    .unwrap();
                assert_eq!(updated.id, row.id);
                assert_eq!(updated.calendar_sequence, Some(sequence));
                let replay = project_deadlines::update(db, project_id, row.id, &patch)
                    .await
                    .unwrap();
                assert_eq!(replay.calendar_sequence, Some(sequence));
            }
            let cancelled = project_deadlines::cancel(db, project_id, row.id)
                .await
                .unwrap();
            assert_eq!(cancelled.id, row.id);
            assert_eq!(cancelled.calendar_sequence, Some(4));
        })
        .await;
    }

    #[tokio::test]
    async fn historical_calendar_versions_never_regress_and_overflow_is_refused() {
        with_project(async |db, project_id| {
            let pg = crate::postgres::PostgresDb::application().await.unwrap();
            let row = project_deadlines::create(db, &sample(project_id)).await.unwrap();
            sqlx::query("UPDATE project_deadlines SET calendar_sequence = NULL, updated_at = '2026-10-26T00:00:00Z' WHERE id = $1")
                .bind(row.id).execute(pg.pool()).await.unwrap();
            let patch = project_deadlines::DeadlinePatch {
                title: Some(Some("Updated historical deadline".into())),
                ..Default::default()
            };
            let updated = project_deadlines::update(db, project_id, row.id, &patch).await.unwrap();
            assert_eq!(updated.calendar_sequence, Some(1_792_972_801));
            sqlx::query("UPDATE project_deadlines SET calendar_sequence = 2147483647 WHERE id = $1")
                .bind(row.id).execute(pg.pool()).await.unwrap();
            assert!(project_deadlines::cancel(db, project_id, row.id).await.is_err());
            assert_eq!(project_deadlines::find(db, project_id, row.id).await.unwrap().unwrap().calendar_sequence, Some(2_147_483_647));
        }).await;
    }

    #[tokio::test]
    async fn the_schema_refuses_sequence_values_beyond_the_i32_range() {
        with_project(async |db, project_id| {
            let created = project_deadlines::create(db, &sample(project_id))
                .await
                .unwrap();
            let pg = crate::postgres::PostgresDb::application().await.unwrap();
            let rejected = sqlx::query(
                "UPDATE project_deadlines SET calendar_sequence = 3000000000 WHERE id = $1",
            )
            .bind(created.id)
            .execute(pg.pool())
            .await;
            assert!(rejected.is_err(), "the schema must cap calendar_sequence");
        })
        .await;
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
        with_project(async |db, project_id| {
            let project = crate::projects::find_by_id(db, project_id)
                .await
                .unwrap()
                .unwrap();
            let titled = project_deadlines::create(db, &sample(project.id))
                .await
                .unwrap();
            assert_eq!(
                event_summary(&titled, &project.code),
                format!("{}: DEADLINE – Answering brief due", project.code)
            );
            let mut hearing = sample(project.id);
            hearing.title = None;
            hearing.calendar_role = "hearing".into();
            let hearing = project_deadlines::create(db, &hearing).await.unwrap();
            assert_eq!(
                event_summary(&hearing, &project.code),
                format!("{}: HEARING – Hearing", project.code)
            );
            let mut opponent = sample(project.id);
            opponent.title = None;
            opponent.calendar_role = "opponent".into();
            opponent.authority_kind = "statute".into();
            let opponent = project_deadlines::create(db, &opponent).await.unwrap();
            assert_eq!(
                event_summary(&opponent, &project.code),
                format!("{}: DEADLINE – Statutory deadline (opponent)", project.code)
            );
        })
        .await;
    }

    #[tokio::test]
    async fn the_event_carries_the_ics_basis_and_its_identity() {
        with_project(async |db, project_id| {
            let created = project_deadlines::create(db, &sample(project_id))
                .await
                .unwrap();
            let event = deadline_event(&created, "mirror-code", Some("https://law.test/")).unwrap();
            assert_eq!(event.summary, "mirror-code: DEADLINE – Answering brief due");
            assert_eq!(
                event.start,
                chrono::FixedOffset::east_opt(-8 * 3600)
                    .unwrap()
                    .with_ymd_and_hms(2026, 11, 2, 17, 0, 0)
                    .unwrap()
            );
            assert_eq!(event.sequence, 0);
            assert!(!event.cancelled);
            assert_eq!(event.reminder_minutes, vec![10080, 4320, 1440, 15]);
            let description = event.description.as_deref().unwrap();
            let (details, target) = description.rsplit_once("\nSubmit by ").unwrap();
            assert_eq!(
                details,
                format!(
                    "Authority: Order setting briefing\nDeadline: {}\nMatter page: https://law.test/app/projects/mirror-code",
                    created.id
                )
            );
            assert_eq!(
                DateTime::parse_from_rfc3339(target).unwrap(),
                DateTime::parse_from_rfc3339("2026-11-02T16:45:00-08:00").unwrap()
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
        }).await;
    }

    #[test]
    fn a_report_line_names_the_outcome_and_its_counts() {
        use crate::deadline_calendar::{MirrorOutcome, MirrorReport};
        let mut report = MirrorReport {
            upserted: vec![Uuid::now_v7(), Uuid::now_v7()],
            in_sync: 3,
            ..Default::default()
        };
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
        assert!(event.reminder_minutes.is_empty());
    }

    #[test]
    fn an_open_timed_deadline_holds_busy_time_and_a_completed_one_does_not() {
        let mut deadline = stored(sample(Uuid::now_v7()));
        let open = deadline_event(&deadline, "mirror-code", None).unwrap();
        assert_eq!(open.timing, cloud::calendar::EventTiming::Due);
        deadline.status = project_deadlines::STATUS_COMPLETE.into();
        let done = deadline_event(&deadline, "mirror-code", None).unwrap();
        assert_eq!(done.timing, cloud::calendar::EventTiming::Instant);
    }

    #[test]
    fn a_completed_row_stays_on_the_calendar_as_confirmed() {
        let mut deadline = stored(sample(Uuid::now_v7()));
        deadline.status = project_deadlines::STATUS_COMPLETE.into();
        let event = deadline_event(&deadline, "mirror-code", None).unwrap();
        assert!(!event.cancelled);
        assert!(event.summary.ends_with(" (complete)"));
        assert!(event.reminder_minutes.is_empty());
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
            due_on: deadline.due_on.clone(),
            hearing: deadline.hearing.clone(),
            effort_minutes: deadline.effort_minutes.unwrap_or_default(),
            authority_kind: deadline.authority_kind.clone(),
            authority: deadline.authority.clone(),
            source: deadline.source.clone(),
            source_document_id: deadline.source_document_id,
            owner_person_id: deadline.owner_person_id,
            status: project_deadlines::STATUS_OPEN.into(),
            calendar_role: deadline.calendar_role.clone(),
            category: deadline.category.clone(),
            internal_lead_days: deadline.internal_lead_days,
            client_lead_days: deadline.client_lead_days,
            replay_key: deadline.replay_key,
            inserted_at: "2026-10-30T00:00:00+00:00".into(),
            updated_at: "2026-10-30T00:00:00+00:00".into(),
            calendar_sequence: Some(0),
            calendar_review_pending: false,
        }
    }

    use super::MirrorReport;
    use super::{event_description, recorded_calendar_id, MirrorOutcome};

    /// The ICS description stacks the row's description, then the authority,
    /// the source document, the deadline id, and — only when a base URL is
    /// configured — the matter-page link, in that order, newline-joined.
    #[test]
    fn event_description_stacks_identity_lines_in_a_stable_order() {
        let mut row = stored(sample(Uuid::now_v7()));
        row.description = Some("Briefing schedule attached.".into());
        row.source_document_id = Some(Uuid::now_v7());
        let description = event_description(&row, "ACME", Some("https://www.example.com/"));
        assert_eq!(
            event_description(&row, "ACME", Some("www.example.com")),
            description
        );
        assert_eq!(
            body_sha(&row, "ACME", Some("www.example.com")),
            body_sha(&row, "ACME", Some("https://www.example.com"))
        );
        let lines: Vec<&str> = description.lines().collect();
        assert_eq!(lines[0], "Briefing schedule attached.");
        assert_eq!(lines[1], "Authority: Order setting briefing");
        assert_eq!(
            lines[2],
            format!("Source document: {}", row.source_document_id.unwrap())
        );
        assert_eq!(lines[3], format!("Deadline: {}", row.id));
        assert_eq!(
            lines[4],
            "Matter page: https://www.example.com/app/projects/ACME"
        );

        // A trailing slash on the base URL is trimmed once, not duplicated.
        assert!(
            event_description(&row, "ACME", Some("https://www.example.com//"))
                .lines()
                .last()
                .unwrap()
                .starts_with("Matter page: https://www.example.com/app/projects/")
        );
        // No base URL: no matter-page line at all.
        let without_base = event_description(&row, "ACME", None);
        assert!(!without_base.contains("Matter page:"));
        // A blank base URL is treated like an unset one.
        assert_eq!(event_description(&row, "ACME", Some("   ")), without_base);
        // A row with no description starts straight at its first line.
        let mut bare = stored(sample(Uuid::now_v7()));
        bare.description = None;
        bare.authority = String::new();
        assert!(event_description(&bare, "ACME", None).starts_with("Deadline: "));
    }

    /// A Project row with only its calendar id varied — the one field
    /// `recorded_calendar_id` reads.
    fn project_row(calendar_id: Option<&str>) -> crate::projects::Project {
        crate::projects::Project {
            id: Uuid::now_v7(),
            code: "ACME".into(),
            name: "Mirror matter".into(),
            status: "open".into(),
            brand: "neon".into(),
            entity_id: Uuid::now_v7(),
            firm_id: None,
            jurisdiction_id: None,
            onboarding_jurisdiction: None,
            client_dri_person_id: None,
            description: None,
            drive_folder_id: None,
            repository_url: None,
            git_initialized_at: None,
            forge_provisioned_at: None,
            closed_at: None,
            closure_reason: None,
            internal_slack_channel_url: None,
            external_slack_channel_url: None,
            internal_slack_channel_id: None,
            private_notion_page_url: None,
            shared_notion_page_url: None,
            calendar_id: calendar_id.map(str::to_string),
            calendar_timezone: None,
            booking_url: None,
            legal_plan_id: None,
            plan_member_id: None,
            plan_case_number: None,
            benefit_code: None,
            plan_matter_type: None,
            plan_state: None,
            plan_assigned_on: None,
            plan_accepted_on: None,
            plan_closed_on: None,
            kit_version: None,
            inserted_at: "2026-10-30T00:00:00+00:00".into(),
            updated_at: "2026-10-30T00:00:00+00:00".into(),
        }
    }

    /// A Project's recorded calendar id is read back only when it names a
    /// real calendar: unset stays unset and whitespace is not a calendar.
    #[test]
    fn recorded_calendar_id_refuses_blank_ids() {
        assert_eq!(
            recorded_calendar_id(&project_row(Some("  cal-123  "))).as_deref(),
            Some("cal-123")
        );
        assert_eq!(recorded_calendar_id(&project_row(Some("   "))), None);
        assert_eq!(recorded_calendar_id(&project_row(None)), None);
    }

    /// The door's one journal line reports counts, never matter content;
    /// failures and dry runs change the line's shape.
    #[test]
    fn report_line_reports_counts_and_flags_without_matter_content() {
        let mut report = MirrorReport::empty(MirrorOutcome::Mirrored, false);
        report.upserted.push(Uuid::now_v7());
        report.in_sync = 2;
        report.deleted.push("calendar-event-1".into());
        assert_eq!(
            report_line(&report),
            "mirrored upserted=1 in_sync=2 deleted=1"
        );

        report.failures.push("opaque-failure".into());
        assert!(report_line(&report).ends_with(" failed=1"));
        assert!(!report_line(&report).contains("opaque-failure"));

        let dry = MirrorReport::empty(MirrorOutcome::SkippedClosed, true);
        assert_eq!(
            report_line(&dry),
            "skipped_closed upserted=0 in_sync=0 deleted=0 (dry run)"
        );
    }

    /// The sequence an open deadline carries is its internal lead days as a
    /// popup; a closed or cancelled row stays silent, and an oversized lead
    /// fails the mirror rather than overflowing.
    #[test]
    fn reminder_minutes_follow_the_rows_status_and_limits() {
        let mut row = stored(sample(Uuid::now_v7()));
        row.internal_lead_days = Some(3);
        let event = super::deadline_event(&row, "ACME", None).unwrap();
        assert_eq!(event.reminder_minutes, vec![10080, 4320, 1440, 15]);

        row.status = project_deadlines::STATUS_COMPLETE.into();
        assert_eq!(
            super::deadline_event(&row, "ACME", None)
                .unwrap()
                .reminder_minutes,
            Vec::<i64>::new()
        );

        row.status = project_deadlines::STATUS_OPEN.into();
        row.internal_lead_days = Some(i64::MAX);
        assert!(super::deadline_event(&row, "ACME", None)
            .unwrap_err()
            .to_string()
            .contains("too large"));
    }

    /// A row written before sequences were recorded falls back to its updated
    /// instant, and a sequence outside the RFC 5545 integer range fails the
    /// mirror instead of wrapping.
    #[test]
    fn event_sequence_falls_back_to_the_updated_instant_and_rejects_overflow() {
        let mut row = stored(sample(Uuid::now_v7()));
        row.calendar_sequence = Some(7);
        assert_eq!(event_sequence(&row).unwrap(), 7);

        row.calendar_sequence = None;
        row.updated_at = "2026-10-30T00:00:30Z".into();
        let expected = chrono::DateTime::parse_from_rfc3339("2026-10-30T00:00:30Z")
            .unwrap()
            .timestamp();
        assert_eq!(
            event_sequence(&row).unwrap(),
            i32::try_from(expected).unwrap()
        );

        // An unparseable updated_at falls back to the ICS default of 0.
        row.updated_at = "not-a-timestamp".into();
        assert_eq!(event_sequence(&row).unwrap(), 0);

        row.calendar_sequence = Some(i64::MAX);
        assert!(event_sequence(&row)
            .unwrap_err()
            .to_string()
            .contains("outside the RFC 5545"));
    }

    /// The content digest covers exactly what the event renders — the same
    /// row renders the same sha, and any rendered field moves it.
    #[test]
    fn body_sha_changes_with_any_rendered_field_and_ignores_a_blank_base_url() {
        let row = stored(sample(Uuid::now_v7()));
        let base = body_sha(&row, "ACME", Some("https://www.example.com"));
        assert_eq!(
            base,
            body_sha(&row, "ACME", Some("https://www.example.com"))
        );
        // A real base URL is part of the digest; a blank one normalizes to
        // the unset case.
        assert_eq!(
            body_sha(&row, "ACME", Some("  ")),
            body_sha(&row, "ACME", None)
        );
        assert_ne!(base, body_sha(&row, "ACME", None));

        let mut retitled = stored(sample(Uuid::now_v7()));
        retitled.id = row.id;
        retitled.title = Some("Reply brief due".into());
        assert_ne!(
            body_sha(&retitled, "ACME", Some("https://www.example.com")),
            base
        );

        // A different matter code changes the digest.
        assert_ne!(
            body_sha(&row, "OTHER", Some("https://www.example.com")),
            base
        );
        // The digest is a 64-character lowercase hex string.
        assert_eq!(base.len(), 64);
        assert!(base
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
    }
}

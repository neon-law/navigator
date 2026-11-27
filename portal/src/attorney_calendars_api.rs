//! Lawyer-tier Attorney Calendar reads.
//!
//! The path is its own noun. Nesting it under `projects` would admit any
//! authenticated caller, including a client, and the participant list is
//! already visible to client participants. Calendar coordinates stay here.
//!
//! `ReadAttorneyCalendars` is the Firm boundary: a caller with no membership
//! on the Firm that owns the Workspace cannot spend its delegation. Free/busy
//! for other participants returns busy windows. Event details come from the
//! signed-in Person's primary calendar or the authorized Project Calendar.

use std::collections::{BTreeMap, BTreeSet};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, FixedOffset, NaiveDate, TimeDelta};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::{ApiError, ApiState, LawyerSession};
use crate::integrations::IntegrationError;
use store::attorney_calendars::{
    schedule, AttorneyCalendar, CalendarKind, ScheduleReport, SourceBlock, UnreadableCalendar,
};
use store::persons::Person;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WindowRequest {
    #[serde(default)]
    project_code: Option<String>,
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
    #[serde(default)]
    attorneys: Vec<String>,
    #[serde(default)]
    personal: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AccessRequest {
    #[serde(default)]
    project_code: Option<String>,
    #[serde(default)]
    personal: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegisterRequest {
    #[serde(default)]
    calendar_id: Option<String>,
    #[serde(default)]
    kind: Option<String>,
}

#[derive(Serialize)]
struct BusyBody {
    source: String,
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    calendar_id: Option<String>,
    start: String,
    end: String,
}

#[derive(Serialize)]
struct GapBody {
    email: String,
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    calendar_id: Option<String>,
    missing: &'static str,
}

#[derive(Serialize)]
struct DuplicateBody {
    summary: String,
    count: usize,
}

#[derive(Serialize)]
struct FreeBusyBody {
    busy: Vec<BusyBody>,
    unreadable: Vec<GapBody>,
}

#[derive(Serialize)]
struct CheckBody {
    clear: bool,
    overlaps: Vec<BusyBody>,
    duplicates: Vec<DuplicateBody>,
    unreadable: Vec<GapBody>,
}

#[derive(Serialize)]
struct AccessCalendarBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    calendar_id: Option<String>,
    kind: &'static str,
    readable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    missing: Option<&'static str>,
}

#[derive(Serialize)]
struct AccessAttorneyBody {
    person_id: Uuid,
    email: String,
    name: String,
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    missing: Option<&'static str>,
    calendars: Vec<AccessCalendarBody>,
}

#[derive(Serialize)]
struct AccessBody {
    attorneys: Vec<AccessAttorneyBody>,
    #[serde(skip_serializing_if = "Option::is_none")]
    project_calendar: Option<AccessCalendarBody>,
}

#[derive(Serialize)]
struct RegisteredBody {
    person_id: Uuid,
    calendar_id: String,
    kind: &'static str,
}

struct Attorney {
    person: Person,
    role: String,
}

fn window(start: DateTime<FixedOffset>, end: DateTime<FixedOffset>) -> Result<(), ApiError> {
    if start >= end || end - start > TimeDelta::days(31) {
        return Err(ApiError::MalformedBody(
            "provide a positive calendar window of at most 31 days".into(),
        ));
    }
    Ok(())
}

fn gap_word(gap: cloud::CalendarGap) -> &'static str {
    match gap {
        cloud::CalendarGap::Share => "share",
        cloud::CalendarGap::Scope => "scope",
        cloud::CalendarGap::InvalidResponse => "invalid_response",
    }
}

fn busy_body(block: &SourceBlock) -> BusyBody {
    BusyBody {
        source: block.source.clone(),
        role: block.role.clone(),
        calendar_id: block.calendar_id.clone(),
        start: block.start.to_rfc3339(),
        end: block.end.to_rfc3339(),
    }
}

fn gap_body(gap: &UnreadableCalendar) -> GapBody {
    GapBody {
        email: gap.email.clone(),
        role: gap.role.clone(),
        calendar_id: gap.calendar_id.clone(),
        missing: gap.missing,
    }
}

/// `POST /app/api/calendars/freebusy`
pub(crate) async fn freebusy_door(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Json(request): Json<WindowRequest>,
) -> Result<Response, ApiError> {
    window(request.start, request.end)?;
    let report = read_report(&state, &session, &request, false).await?;
    Ok(Json(FreeBusyBody {
        busy: report.busy.iter().map(busy_body).collect(),
        unreadable: report.unreadable.iter().map(gap_body).collect(),
    })
    .into_response())
}

/// `POST /app/api/calendars/check`
pub(crate) async fn check_door(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Json(request): Json<WindowRequest>,
) -> Result<Response, ApiError> {
    window(request.start, request.end)?;
    let report = read_report(&state, &session, &request, true).await?;
    Ok(Json(CheckBody {
        clear: report.clear,
        overlaps: report.overlaps.iter().map(busy_body).collect(),
        duplicates: report
            .duplicates
            .iter()
            .map(|duplicate| DuplicateBody {
                summary: duplicate.summary.clone(),
                count: duplicate.count,
            })
            .collect(),
        unreadable: report.unreadable.iter().map(gap_body).collect(),
    })
    .into_response())
}

/// `POST /app/api/calendars/events` — the signed-in attorney's primary calendar or a Project Calendar.
pub(crate) async fn events_door(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Json(request): Json<WindowRequest>,
) -> Result<Response, ApiError> {
    window(request.start, request.end)?;
    ensure_caller(&session).await;
    if let Some(code) = request.project_code.as_deref() {
        let project = authorized_project(&state, &session, code).await?;
        let body = match project_events(&state, &project, request.start, request.end).await? {
            Ok(events) => serde_json::json!(events),
            Err(missing) => serde_json::json!({
                "unreadable": [{"role": "project", "missing": missing}]
            }),
        };
        return Ok((
            [(axum::http::header::CACHE_CONTROL, "no-store")],
            Json(body),
        )
            .into_response());
    }
    let person_id = session.person_id.ok_or(ApiError::Forbidden)?;
    if !firm_member(&state, &session).await? {
        return Err(ApiError::NotFound);
    }
    let email = session
        .email
        .clone()
        .ok_or_else(|| ApiError::MalformedBody("the session has no email address".into()))?;
    let pg = store::attorney_calendars::postgres()
        .await
        .map_err(calendar_store)?;
    let primary = store::attorney_calendars::listed(&pg, &[person_id])
        .await
        .map_err(calendar_store)?
        .into_iter()
        .find(|row| row.kind == CalendarKind::Primary);
    let Some(primary) = primary else {
        return Ok(Json(serde_json::json!({
            "unreadable": [{
                "email": email,
                "role": session.role.as_str(),
                "missing": "unrecorded"
            }]
        }))
        .into_response());
    };
    let reader = state
        .integration_providers
        .attorney_calendars()
        .await
        .map_err(provider_status)?;
    match reader
        .events(&email, &primary.calendar_id, request.start, request.end)
        .await
    {
        Ok(cloud::EventRead::Events(events)) => Ok(Json(events).into_response()),
        Ok(cloud::EventRead::Gap(gap)) => Ok(Json(serde_json::json!({
            "unreadable": [{
                "email": email,
                "role": session.role.as_str(),
                "calendar_id": primary.calendar_id,
                "missing": gap_word(gap)
            }]
        }))
        .into_response()),
        Err(_) => Ok(unavailable()),
    }
}

/// `GET` is not used: the access report is a POST so a Project code stays out
/// of a URL. `POST /app/api/calendars/access`.
pub(crate) async fn access_door(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Json(request): Json<AccessRequest>,
) -> Result<Response, ApiError> {
    ensure_caller(&session).await;
    let (project, attorneys) = audience(
        &state,
        &session,
        request.project_code.as_deref(),
        &[],
        request.personal,
    )
    .await?;
    let calendars =
        SchedulingCalendars::load(&state, &session, project.as_ref(), &attorneys).await?;
    let now = chrono::Utc::now().fixed_offset();
    let probe_end = now + TimeDelta::minutes(5);
    let mut bodies = Vec::new();
    for attorney in &attorneys {
        let ids = calendars.ids(attorney.person.id);
        let windows = read_free_busy(&state, &attorney.person.email, &ids, now, probe_end).await?;
        let access = windows
            .into_iter()
            .map(|window| {
                let missing = window.gap.map(gap_word);
                let recorded = calendars.recorded.iter().find(|row| {
                    row.person_id == attorney.person.id && row.calendar_id == window.calendar_id
                });
                let visible = session.person_id == Some(attorney.person.id)
                    || project.as_ref().and_then(|p| p.calendar_id.as_ref())
                        == Some(&window.calendar_id)
                    || !calendars.is_shared(attorney.person.id, &window.calendar_id);
                AccessCalendarBody {
                    calendar_id: visible.then_some(window.calendar_id),
                    kind: recorded.map_or("freebusy", |row| row.kind.as_str()),
                    readable: missing.is_none(),
                    missing,
                }
            })
            .collect();
        bodies.push(AccessAttorneyBody {
            person_id: attorney.person.id,
            email: attorney.person.email.clone(),
            name: attorney.person.name.clone(),
            role: attorney.role.clone(),
            missing: calendars.missing(attorney.person.id),
            calendars: access,
        });
    }
    let project_calendar = if let Some(project) = project.as_ref() {
        if let Some(calendar_id) = project.calendar_id.clone() {
            let readable = project_events(&state, project, now, probe_end)
                .await?
                .is_ok();
            Some(AccessCalendarBody {
                calendar_id: Some(calendar_id),
                kind: "project",
                readable,
                missing: (!readable).then_some("scope"),
            })
        } else {
            Some(AccessCalendarBody {
                calendar_id: Some(String::new()),
                kind: "project",
                readable: false,
                missing: Some("unrecorded"),
            })
        }
    } else {
        None
    };
    Ok(Json(AccessBody {
        attorneys: bodies,
        project_calendar,
    })
    .into_response())
}

/// `POST /app/api/calendars/register`
pub(crate) async fn register_door(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Json(request): Json<RegisterRequest>,
) -> Result<Response, ApiError> {
    let person_id = session.person_id.ok_or(ApiError::Forbidden)?;
    if !firm_member(&state, &session).await? {
        return Err(ApiError::NotFound);
    }
    let email = session
        .email
        .clone()
        .ok_or_else(|| ApiError::MalformedBody("the session has no email address".into()))?;
    let kind = match request.kind.as_deref().unwrap_or("primary") {
        "primary" => CalendarKind::Primary,
        "freebusy" => CalendarKind::FreeBusy,
        "booking" => CalendarKind::Booking,
        _ => {
            return Err(ApiError::MalformedBody(
                "kind is primary, freebusy, or booking".into(),
            ))
        }
    };
    if kind == CalendarKind::Booking {
        let calendar_id = request.calendar_id.as_deref().ok_or_else(|| {
            ApiError::MalformedBody(
                "a booking calendar needs an explicit secondary Calendar ID".into(),
            )
        })?;
        let owner = cloud::calendar::GoogleCalendar::for_owner(&email)
            .await
            .map_err(|_| ApiError::Db("Google Calendar delegation is unavailable".into()))?;
        owner
            .verify_booking_calendar(calendar_id.trim())
            .await
            .map_err(|_| {
                ApiError::MalformedBody(
                    "use a private secondary calendar owned by your Workspace account".into(),
                )
            })?;
    }
    let calendar_id = request.calendar_id.unwrap_or(email);
    let pg = store::attorney_calendars::postgres()
        .await
        .map_err(calendar_store)?;
    let recorded = store::attorney_calendars::register(&pg, person_id, &calendar_id, kind)
        .await
        .map_err(calendar_store)?;
    Ok(Json(RegisteredBody {
        person_id,
        calendar_id: recorded.calendar_id,
        kind: recorded.kind.as_str(),
    })
    .into_response())
}

async fn read_report(
    state: &ApiState,
    session: &crate::session::SessionData,
    request: &WindowRequest,
    duplicates: bool,
) -> Result<ScheduleReport, ApiError> {
    ensure_caller(session).await;
    let (project, attorneys) = audience(
        state,
        session,
        request.project_code.as_deref(),
        &request.attorneys,
        request.personal,
    )
    .await?;
    let calendars = SchedulingCalendars::load(state, session, project.as_ref(), &attorneys).await?;
    let (mut unreadable, mut blocks) =
        attorney_busy(state, &attorneys, &calendars, request.start, request.end).await?;
    if let Some(project) = project.as_ref() {
        let (gaps, project_blocks) =
            project_busy(state, project, request.start, request.end).await?;
        unreadable.extend(gaps);
        blocks.extend(project_blocks);
    }
    let summaries = if duplicates {
        signed_in_titles(
            state,
            session,
            &calendars.recorded,
            request,
            &mut unreadable,
        )
        .await?
    } else {
        Vec::new()
    };
    Ok(schedule(
        unreadable,
        blocks,
        &summaries,
        request.start,
        request.end,
    ))
}

struct SchedulingCalendars {
    recorded: Vec<AttorneyCalendar>,
    shared: BTreeMap<Uuid, BTreeSet<String>>,
    unverified: BTreeSet<Uuid>,
}

impl SchedulingCalendars {
    async fn load(
        state: &ApiState,
        session: &crate::session::SessionData,
        project: Option<&store::projects::Project>,
        attorneys: &[Attorney],
    ) -> Result<Self, ApiError> {
        let pg = store::attorney_calendars::postgres()
            .await
            .map_err(calendar_store)?;
        let ids: Vec<_> = attorneys
            .iter()
            .map(|attorney| attorney.person.id)
            .collect();
        let firms = match project.and_then(|p| p.firm_id) {
            Some(firm) => vec![firm],
            None => google_firms(state, session).await?,
        };
        let candidates = store::project_calendar::shared_freebusy_calendars(&pg, &firms, &ids)
            .await
            .map_err(|error| ApiError::Db(error.to_string()))?;
        let recorded = store::attorney_calendars::listed(&pg, &ids)
            .await
            .map_err(calendar_store)?;
        let mut result = Self {
            recorded,
            shared: BTreeMap::new(),
            unverified: BTreeSet::new(),
        };
        for projects in candidates.values() {
            if let Some(first) = projects.first() {
                // One owner inventory per Firm also disambiguates equal calendar names.
                let owner = state
                    .integration_providers
                    .calendar(&state.application_store, first.project_id)
                    .await;
                let calendar_codes = if let Ok(owner) = owner {
                    owner.owned_calendar_codes().await.unwrap_or_default()
                } else {
                    BTreeMap::new()
                };
                for project in projects {
                    result.include_verified(project, &calendar_codes);
                }
            }
        }
        Ok(result)
    }

    fn include_verified(
        &mut self,
        project: &store::project_calendar::SharedCalendar,
        owned: &BTreeMap<String, String>,
    ) {
        let verified_id = project
            .calendar_id
            .as_ref()
            .filter(|id| owned.get(*id) == Some(&project.code));
        for person in &project.people {
            if let Some(id) = verified_id {
                self.shared.entry(*person).or_default().insert(id.clone());
            } else {
                self.unverified.insert(*person);
            }
        }
    }

    fn ids(&self, person: Uuid) -> Vec<String> {
        let mut ids = self.shared.get(&person).cloned().unwrap_or_default();
        ids.extend(
            self.recorded
                .iter()
                .filter(|row| row.person_id == person)
                .map(|row| row.calendar_id.clone()),
        );
        ids.into_iter().collect()
    }

    fn missing(&self, person: Uuid) -> Option<&'static str> {
        if self.unverified.contains(&person) {
            Some("unverified_project_calendar")
        } else {
            (!self.recorded.iter().any(|row| row.person_id == person)).then_some("unrecorded")
        }
    }

    fn is_shared(&self, person: Uuid, id: &str) -> bool {
        self.shared.get(&person).is_some_and(|ids| ids.contains(id))
    }
}

async fn attorney_busy(
    state: &ApiState,
    attorneys: &[Attorney],
    calendars: &SchedulingCalendars,
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
) -> Result<(Vec<UnreadableCalendar>, Vec<SourceBlock>), ApiError> {
    let mut unreadable = Vec::new();
    let mut blocks = Vec::new();
    for attorney in attorneys {
        if let Some(missing) = calendars.missing(attorney.person.id) {
            unreadable.push(UnreadableCalendar {
                email: attorney.person.email.clone(),
                role: attorney.role.clone(),
                calendar_id: None,
                missing,
            });
        }
        let calendar_ids = calendars.ids(attorney.person.id);
        let windows =
            read_free_busy(state, &attorney.person.email, &calendar_ids, start, end).await?;
        for window in windows {
            // Other Projects contribute busy windows only. Their IDs can identify a matter.
            let visible_id = (!calendars.is_shared(attorney.person.id, &window.calendar_id))
                .then_some(window.calendar_id);
            if let Some(gap) = window.gap {
                unreadable.push(UnreadableCalendar {
                    email: attorney.person.email.clone(),
                    role: attorney.role.clone(),
                    calendar_id: visible_id,
                    missing: gap_word(gap),
                });
                continue;
            }
            for busy in window.busy {
                blocks.push(SourceBlock {
                    source: attorney.person.email.clone(),
                    role: attorney.role.clone(),
                    calendar_id: visible_id.clone(),
                    start: busy.start,
                    end: busy.end,
                });
            }
        }
    }
    Ok((unreadable, blocks))
}

async fn project_busy(
    state: &ApiState,
    project: &store::projects::Project,
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
) -> Result<(Vec<UnreadableCalendar>, Vec<SourceBlock>), ApiError> {
    let mut unreadable = Vec::new();
    let mut blocks = Vec::new();
    match project_events(state, project, start, end).await? {
        Ok(events) => {
            for event in events {
                if let Some((event_start, event_end)) = event_bounds(&event) {
                    blocks.push(SourceBlock {
                        source: event.summary.unwrap_or_else(|| "project calendar".into()),
                        role: "project".into(),
                        calendar_id: project.calendar_id.clone(),
                        start: event_start,
                        end: event_end,
                    });
                }
            }
        }
        Err(missing) => unreadable.push(UnreadableCalendar {
            email: project.code.clone(),
            role: "project".into(),
            calendar_id: project.calendar_id.clone(),
            missing,
        }),
    }
    let policy = store::project_deadlines::scheduling_policy(project.firm_id)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?;
    for deadline in store::project_deadlines::list(&state.application_store, project.id)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?
    {
        if deadline.status != store::project_deadlines::STATUS_OPEN
            || deadline.calendar_review_pending
        {
            continue;
        }
        let bounds = deadline.buffered_window(policy).or_else(|| {
            if deadline
                .due_on
                .as_ref()
                .is_some_and(|day| day.cutoff.is_none())
            {
                return None;
            }
            let due = DateTime::parse_from_rfc3339(deadline.due_at.trim()).ok()?;
            Some((due, due))
        });
        if let Some((start, end)) = bounds {
            blocks.push(SourceBlock {
                source: deadline.title.unwrap_or_else(|| "deadline".into()),
                role: "deadline".into(),
                calendar_id: None,
                start,
                end,
            });
        }
    }
    Ok((unreadable, blocks))
}

async fn signed_in_titles(
    state: &ApiState,
    session: &crate::session::SessionData,
    recorded: &[AttorneyCalendar],
    request: &WindowRequest,
    unreadable: &mut Vec<UnreadableCalendar>,
) -> Result<Vec<String>, ApiError> {
    let Some(email) = session.email.as_deref() else {
        return Ok(Vec::new());
    };
    let Some(primary) = recorded
        .iter()
        .find(|row| session.person_id == Some(row.person_id) && row.kind == CalendarKind::Primary)
    else {
        return Ok(Vec::new());
    };
    match state
        .integration_providers
        .attorney_calendars()
        .await
        .map_err(provider_status)?
        .events(email, &primary.calendar_id, request.start, request.end)
        .await
    {
        Ok(cloud::EventRead::Events(events)) => Ok(events
            .into_iter()
            .filter_map(|event| event.summary)
            .collect()),
        Ok(cloud::EventRead::Gap(gap)) => {
            unreadable.push(UnreadableCalendar {
                email: email.to_string(),
                role: session.role.as_str().to_string(),
                calendar_id: Some(primary.calendar_id.clone()),
                missing: gap_word(gap),
            });
            Ok(Vec::new())
        }
        Err(_) => Err(provider_status(IntegrationError::Provider)),
    }
}

async fn read_free_busy(
    state: &ApiState,
    email: &str,
    calendar_ids: &[String],
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
) -> Result<Vec<cloud::CalendarWindow>, ApiError> {
    let reader = match state.integration_providers.attorney_calendars().await {
        Ok(reader) => reader,
        Err(IntegrationError::NotConfigured(_) | IntegrationError::CredentialUnusable) => {
            return Ok(calendar_ids
                .iter()
                .map(|calendar_id| cloud::CalendarWindow {
                    calendar_id: calendar_id.clone(),
                    busy: Vec::new(),
                    gap: Some(cloud::CalendarGap::Scope),
                })
                .collect());
        }
        Err(error) => return Err(provider_status(error)),
    };
    let mut windows = Vec::new();
    // Google accepts at most 50 calendar IDs per freeBusy request.
    for ids in calendar_ids.chunks(50) {
        windows.extend(
            reader
                .free_busy(email, ids, start, end)
                .await
                .map_err(|_| provider_status(IntegrationError::Provider))?,
        );
    }
    Ok(windows)
}

async fn project_events(
    state: &ApiState,
    project: &store::projects::Project,
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
) -> Result<Result<Vec<cloud::ProjectCalendarEvent>, &'static str>, ApiError> {
    let Some(calendar_id) = project.calendar_id.as_deref() else {
        return Ok(Err("unrecorded"));
    };
    let calendar = match state
        .integration_providers
        .calendar(&state.application_store, project.id)
        .await
    {
        Ok(calendar) => calendar,
        Err(IntegrationError::NotConfigured(_) | IntegrationError::CredentialUnusable) => {
            return Ok(Err("scope"))
        }
        Err(error) => return Err(provider_status(error)),
    };
    match calendar
        .project_events(&project.code, calendar_id, start, end)
        .await
    {
        Ok(Some(events)) => Ok(Ok(events)),
        Ok(None) | Err(_) => Ok(Err("scope")),
    }
}

pub(crate) fn event_bounds(
    event: &cloud::ProjectCalendarEvent,
) -> Option<(DateTime<FixedOffset>, DateTime<FixedOffset>)> {
    if event.transparency.as_deref() == Some("transparent") {
        return None;
    }
    let start = instant(&event.start)?;
    let end = instant(&event.end).unwrap_or(start);
    Some((start, end))
}

fn instant(time: &cloud::ProjectCalendarEventTime) -> Option<DateTime<FixedOffset>> {
    if let Some(value) = time.date_time.as_deref() {
        return DateTime::parse_from_rfc3339(value).ok();
    }
    let date = NaiveDate::parse_from_str(time.date.as_deref()?, "%Y-%m-%d").ok()?;
    Some(date.and_hms_opt(0, 0, 0)?.and_utc().fixed_offset())
}

async fn ensure_caller(session: &crate::session::SessionData) {
    let (Some(person_id), Some(email)) = (session.person_id, session.email.as_deref()) else {
        return;
    };
    if session.role.is_lawyer_tier() {
        let _ = store::attorney_calendars::record_sign_in(person_id, email).await;
    }
}

pub(crate) async fn firm_member(
    state: &ApiState,
    session: &crate::session::SessionData,
) -> Result<bool, ApiError> {
    Ok(!google_firms(state, session).await?.is_empty())
}

/// The caller's Firms that chose Google Workspace as the main email. A Firm
/// without that choice has no Attorney Calendar surface.
async fn google_firms(
    state: &ApiState,
    session: &crate::session::SessionData,
) -> Result<Vec<Uuid>, ApiError> {
    let firms = store::firm_capability::allowed_firm_ids(
        &state.application_store,
        session.role,
        session.person_id,
        store::firm_capability::FirmCapability::ReadAttorneyCalendars,
    )
    .await
    .map_err(|error| ApiError::Db(error.to_string()))?;
    store::firms::with_google_calendar(firms)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))
}

async fn authorized_project(
    state: &ApiState,
    session: &crate::session::SessionData,
    code: &str,
) -> Result<store::projects::Project, ApiError> {
    let visible =
        store::access::visible_projects(&state.application_store, session.person_id, session.role)
            .await
            .map_err(ApiError::Db)?;
    let mut matching = visible.into_iter().filter(|project| project.code == code);
    let project = matching.next().ok_or(ApiError::NotFound)?;
    if matching.next().is_some() {
        return Err(ApiError::NotFound);
    }
    let Some(firm_id) = project.firm_id else {
        return Err(ApiError::NotFound);
    };
    let allowed = store::firm_capability::resolve(
        &state.application_store,
        session.role,
        session.person_id,
        firm_id,
        store::firm_capability::FirmCapability::ReadAttorneyCalendars,
    )
    .await
    .map_err(|error| ApiError::Db(error.to_string()))?;
    if !allowed.is_allowed()
        || store::firms::calendar_owner(firm_id)
            .await
            .map_err(|error| ApiError::Db(error.to_string()))?
            .is_none()
    {
        return Err(ApiError::NotFound);
    }
    let participates = store::projects::can_access_as_lawyer(
        &state.application_store,
        session.person_id,
        session.role,
        project.id,
    )
    .await
    .map_err(|error| ApiError::Db(error.to_string()))?;
    if !participates {
        return Err(ApiError::NotFound);
    }
    Ok(project)
}

async fn project_attorneys(
    state: &ApiState,
    session: &crate::session::SessionData,
    code: &str,
) -> Result<(store::projects::Project, Vec<Attorney>), ApiError> {
    let project = authorized_project(state, session, code).await?;
    let rows = store::projects::participations_for_project(&state.application_store, project.id)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?;
    let mut attorneys = Vec::new();
    for row in rows {
        if store::projects::PARTICIPATION_CLIENT_SIDE.contains(&row.participation.as_str())
            || row.is_client_dri
        {
            continue;
        }
        let Some(person) = store::persons::find_by_id(&state.application_store, row.person_id)
            .await
            .map_err(ApiError::Person)?
        else {
            continue;
        };
        if !person.role.is_lawyer_tier() {
            continue;
        }
        attorneys.push(Attorney {
            role: row.participation,
            person,
        });
    }
    Ok((project, attorneys))
}

async fn firm_attorneys(
    state: &ApiState,
    session: &crate::session::SessionData,
) -> Result<Vec<Attorney>, ApiError> {
    let firms = google_firms(state, session).await?;
    if firms.is_empty() {
        return Err(ApiError::NotFound);
    }
    let mut by_person = BTreeMap::new();
    for firm_id in firms {
        let memberships = store::firms::memberships_for_firm(&state.application_store, firm_id)
            .await
            .map_err(|error| ApiError::Db(error.to_string()))?;
        for membership in memberships {
            if by_person.contains_key(&membership.person_id) {
                continue;
            }
            let Some(person) =
                store::persons::find_by_id(&state.application_store, membership.person_id)
                    .await
                    .map_err(ApiError::Person)?
            else {
                continue;
            };
            if !person.role.is_lawyer_tier() {
                continue;
            }
            by_person.insert(
                person.id,
                Attorney {
                    role: person.role.as_str().to_string(),
                    person,
                },
            );
        }
    }
    Ok(by_person.into_values().collect())
}

async fn audience(
    state: &ApiState,
    session: &crate::session::SessionData,
    project_code: Option<&str>,
    filter: &[String],
    personal: bool,
) -> Result<(Option<store::projects::Project>, Vec<Attorney>), ApiError> {
    if personal {
        if project_code.is_some() || !filter.is_empty() {
            return Err(ApiError::MalformedBody(
                "a personal calendar request cannot select a Project or other attorneys".into(),
            ));
        }
        let person_id = session.person_id.ok_or(ApiError::Forbidden)?;
        if !firm_member(state, session).await? {
            return Err(ApiError::NotFound);
        }
        let person = store::persons::find_by_id(&state.application_store, person_id)
            .await
            .map_err(ApiError::Person)?
            .ok_or(ApiError::NotFound)?;
        let role = person.role.as_str().to_string();
        return Ok((None, vec![Attorney { person, role }]));
    }
    let (project, mut attorneys) = match project_code {
        Some(code) => {
            let (project, attorneys) = project_attorneys(state, session, code).await?;
            (Some(project), attorneys)
        }
        None => (None, firm_attorneys(state, session).await?),
    };
    if !filter.is_empty() {
        let wanted: Vec<String> = filter
            .iter()
            .map(|email| email.to_ascii_lowercase())
            .collect();
        attorneys.retain(|attorney| {
            wanted
                .iter()
                .any(|email| attorney.person.email.eq_ignore_ascii_case(email))
        });
        if wanted.iter().any(|email| {
            !attorneys
                .iter()
                .any(|attorney| attorney.person.email.eq_ignore_ascii_case(email))
        }) {
            return Err(ApiError::NotFound);
        }
    }
    attorneys.sort_by(|left, right| left.person.email.cmp(&right.person.email));
    Ok((project, attorneys))
}

fn calendar_store(error: store::attorney_calendars::AttorneyCalendarError) -> ApiError {
    match error {
        store::attorney_calendars::AttorneyCalendarError::InvalidId => ApiError::MalformedBody(
            "A calendar id is the Calendar ID shown in the Google Calendar's settings, not a link."
                .into(),
        ),
        store::attorney_calendars::AttorneyCalendarError::NotConfigured => {
            ApiError::MalformedBody("PostgreSQL is not configured".into())
        }
        store::attorney_calendars::AttorneyCalendarError::BookingAlreadyRegistered => {
            ApiError::MalformedBody(error.to_string())
        }
        other @ store::attorney_calendars::AttorneyCalendarError::Unavailable => {
            ApiError::Db(other.to_string())
        }
    }
}

fn provider_status(error: IntegrationError) -> ApiError {
    match error {
        IntegrationError::NotConfigured(detail) => ApiError::MalformedBody(detail),
        _ => ApiError::Db("calendar provider unavailable".into()),
    }
}

fn unavailable() -> Response {
    (StatusCode::BAD_GATEWAY, "calendar provider unavailable").into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_busy_project_event_blocks_time_without_a_guest_and_free_events_do_not() {
        let mut event: cloud::ProjectCalendarEvent = serde_json::from_value(serde_json::json!({
            "id":"event", "start":{"dateTime":"2026-01-01T12:00:00Z"},
            "end":{"dateTime":"2026-01-01T13:00:00Z"}
        }))
        .unwrap();
        let (start, end) = event_bounds(&event).unwrap();
        assert_eq!(end - start, TimeDelta::hours(1));
        event.transparency = Some("transparent".into());
        assert!(event_bounds(&event).is_none());
    }

    #[test]
    fn scheduling_deduplicates_a_shared_calendar_and_preserves_personal_registration() {
        let person = Uuid::now_v7();
        let mut calendars = SchedulingCalendars {
            unverified: BTreeSet::new(),
            recorded: vec![
                AttorneyCalendar {
                    person_id: person,
                    calendar_id: "primary".into(),
                    kind: CalendarKind::Primary,
                },
                AttorneyCalendar {
                    person_id: person,
                    calendar_id: "shared".into(),
                    kind: CalendarKind::FreeBusy,
                },
            ],
            shared: [(
                person,
                ["shared".into(), "second".into()].into_iter().collect(),
            )]
            .into_iter()
            .collect(),
        };
        assert_eq!(calendars.ids(person), ["primary", "second", "shared"]);
        assert!(calendars.is_shared(person, "shared"));
        assert!(!calendars.is_shared(person, "primary"));
        assert_eq!(calendars.missing(person), None);
        assert!(calendars.ids(Uuid::nil()).is_empty());
        let project = store::project_calendar::SharedCalendar {
            project_id: Uuid::now_v7(),
            code: "sample".into(),
            calendar_id: Some("wrong".into()),
            people: vec![person],
        };
        calendars.include_verified(
            &project,
            &[("wrong".into(), "different".into())].into_iter().collect(),
        );
        assert!(!calendars.ids(person).contains(&"wrong".into()));
        assert_eq!(
            calendars.missing(person),
            Some("unverified_project_calendar")
        );
    }
}

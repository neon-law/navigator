//! Lawyer-tier Attorney Calendar reads.
//!
//! The path is its own noun. Nesting it under `projects` would admit any
//! authenticated caller, including a client, and the participant list is
//! already visible to client participants. Calendar coordinates stay here.
//!
//! `ReadAttorneyCalendars` is the Firm boundary: a caller with no membership
//! on the Firm that owns the Workspace cannot spend its delegation. Free/busy
//! for other participants returns no event details. Event details are the
//! signed-in Person's primary calendar only.

use std::collections::BTreeMap;

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
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AccessRequest {
    #[serde(default)]
    project_code: Option<String>,
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
    calendar_id: String,
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

/// `POST /app/api/calendars/events` — the signed-in attorney's primary calendar.
pub(crate) async fn events_door(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Json(request): Json<WindowRequest>,
) -> Result<Response, ApiError> {
    window(request.start, request.end)?;
    ensure_caller(&session).await;
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
    let (project, attorneys) =
        audience(&state, &session, request.project_code.as_deref(), &[]).await?;
    let pg = store::attorney_calendars::postgres()
        .await
        .map_err(calendar_store)?;
    let ids: Vec<Uuid> = attorneys
        .iter()
        .map(|attorney| attorney.person.id)
        .collect();
    let recorded = store::attorney_calendars::listed(&pg, &ids)
        .await
        .map_err(calendar_store)?;
    let now = chrono::Utc::now().fixed_offset();
    let probe_end = now + TimeDelta::minutes(5);
    let mut bodies = Vec::new();
    for attorney in &attorneys {
        let rows: Vec<&AttorneyCalendar> = recorded
            .iter()
            .filter(|row| row.person_id == attorney.person.id)
            .collect();
        if rows.is_empty() {
            bodies.push(AccessAttorneyBody {
                person_id: attorney.person.id,
                email: attorney.person.email.clone(),
                name: attorney.person.name.clone(),
                role: attorney.role.clone(),
                missing: Some("unrecorded"),
                calendars: Vec::new(),
            });
            continue;
        }
        let ids: Vec<String> = rows.iter().map(|row| row.calendar_id.clone()).collect();
        let windows = read_free_busy(&state, &attorney.person.email, &ids, now, probe_end).await?;
        let calendars = rows
            .iter()
            .map(|row| {
                let missing = windows.iter().find_map(|window| {
                    (window.calendar_id == row.calendar_id)
                        .then_some(window.gap)
                        .flatten()
                        .map(gap_word)
                });
                AccessCalendarBody {
                    calendar_id: row.calendar_id.clone(),
                    kind: row.kind.as_str(),
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
            missing: None,
            calendars,
        });
    }
    let project_calendar = if let Some(project) = project.as_ref() {
        if let Some(calendar_id) = project.calendar_id.clone() {
            let readable = project_events(&state, project, now, probe_end)
                .await?
                .is_ok();
            Some(AccessCalendarBody {
                calendar_id,
                kind: "project",
                readable,
                missing: (!readable).then_some("scope"),
            })
        } else {
            Some(AccessCalendarBody {
                calendar_id: String::new(),
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
        _ => {
            return Err(ApiError::MalformedBody(
                "kind is primary or freebusy".into(),
            ))
        }
    };
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
    )
    .await?;
    let pg = store::attorney_calendars::postgres()
        .await
        .map_err(calendar_store)?;
    let ids: Vec<Uuid> = attorneys
        .iter()
        .map(|attorney| attorney.person.id)
        .collect();
    let recorded = store::attorney_calendars::listed(&pg, &ids)
        .await
        .map_err(calendar_store)?;
    let (mut unreadable, mut blocks) =
        attorney_busy(state, &attorneys, &recorded, request.start, request.end).await?;
    if let Some(project) = project.as_ref() {
        let (gaps, project_blocks) =
            project_busy(state, project, request.start, request.end).await?;
        unreadable.extend(gaps);
        blocks.extend(project_blocks);
    }
    let summaries = if duplicates {
        signed_in_titles(state, session, &recorded, request, &mut unreadable).await?
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

async fn attorney_busy(
    state: &ApiState,
    attorneys: &[Attorney],
    recorded: &[AttorneyCalendar],
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
) -> Result<(Vec<UnreadableCalendar>, Vec<SourceBlock>), ApiError> {
    let mut unreadable = Vec::new();
    let mut blocks = Vec::new();
    for attorney in attorneys {
        let rows: Vec<&AttorneyCalendar> = recorded
            .iter()
            .filter(|row| row.person_id == attorney.person.id)
            .collect();
        if rows.is_empty() {
            unreadable.push(UnreadableCalendar {
                email: attorney.person.email.clone(),
                role: attorney.role.clone(),
                calendar_id: None,
                missing: "unrecorded",
            });
            continue;
        }
        let calendar_ids: Vec<String> = rows.iter().map(|row| row.calendar_id.clone()).collect();
        let windows =
            read_free_busy(state, &attorney.person.email, &calendar_ids, start, end).await?;
        for window in windows {
            if let Some(gap) = window.gap {
                unreadable.push(UnreadableCalendar {
                    email: attorney.person.email.clone(),
                    role: attorney.role.clone(),
                    calendar_id: Some(window.calendar_id),
                    missing: gap_word(gap),
                });
                continue;
            }
            for busy in window.busy {
                blocks.push(SourceBlock {
                    source: attorney.person.email.clone(),
                    role: attorney.role.clone(),
                    calendar_id: Some(window.calendar_id.clone()),
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
    for deadline in store::project_deadlines::list(&state.surreal, project.id)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?
    {
        if deadline.status != store::project_deadlines::STATUS_OPEN {
            continue;
        }
        let Ok(due) = DateTime::parse_from_rfc3339(deadline.due_at.trim()) else {
            continue;
        };
        blocks.push(SourceBlock {
            source: deadline.title.unwrap_or_else(|| "deadline".into()),
            role: "deadline".into(),
            calendar_id: None,
            start: due,
            end: due,
        });
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
    reader
        .free_busy(email, calendar_ids, start, end)
        .await
        .map_err(|_| provider_status(IntegrationError::Provider))
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
        .calendar(&state.surreal, project.id)
        .await
    {
        Ok(calendar) => calendar,
        Err(IntegrationError::NotConfigured(_) | IntegrationError::CredentialUnusable) => {
            return Ok(Err("scope"))
        }
        Err(error) => return Err(provider_status(error)),
    };
    match calendar.events_in_window(calendar_id, start, end).await {
        Ok(events) => Ok(Ok(events)),
        Err(_) => Ok(Err("scope")),
    }
}

fn event_bounds(
    event: &cloud::ProjectCalendarEvent,
) -> Option<(DateTime<FixedOffset>, DateTime<FixedOffset>)> {
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

async fn firm_member(
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
        &state.surreal,
        session.role,
        session.person_id,
        store::firm_capability::FirmCapability::ReadAttorneyCalendars,
    )
    .await
    .map_err(|error| ApiError::Db(error.to_string()))?;
    store::firms::with_google_calendar(&state.surreal, firms)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))
}

async fn project_attorneys(
    state: &ApiState,
    session: &crate::session::SessionData,
    code: &str,
) -> Result<(store::projects::Project, Vec<Attorney>), ApiError> {
    let visible = store::access::visible_projects(&state.surreal, session.person_id, session.role)
        .await
        .map_err(ApiError::Db)?;
    let project = visible
        .into_iter()
        .find(|project| project.code == code)
        .ok_or(ApiError::NotFound)?;
    let Some(firm_id) = project.firm_id else {
        return Err(ApiError::NotFound);
    };
    let allowed = store::firm_capability::resolve(
        &state.surreal,
        session.role,
        session.person_id,
        firm_id,
        store::firm_capability::FirmCapability::ReadAttorneyCalendars,
    )
    .await
    .map_err(|error| ApiError::Db(error.to_string()))?;
    if !allowed.is_allowed()
        || store::firms::with_google_calendar(&state.surreal, vec![firm_id])
            .await
            .map_err(|error| ApiError::Db(error.to_string()))?
            .is_empty()
    {
        return Err(ApiError::NotFound);
    }
    let participates = store::projects::can_access_as_lawyer_in_surreal(
        &state.surreal,
        session.person_id,
        session.role,
        project.id,
    )
    .await
    .map_err(|error| ApiError::Db(error.to_string()))?;
    if !participates {
        return Err(ApiError::NotFound);
    }
    let rows = store::projects::participations_for_project(&state.surreal, project.id)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?;
    let mut attorneys = Vec::new();
    for row in rows {
        if store::projects::PARTICIPATION_CLIENT_SIDE.contains(&row.participation.as_str())
            || row.is_client_dri
        {
            continue;
        }
        let Some(person) = store::persons::find_by_id(&state.surreal, row.person_id)
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
        let memberships = store::firms::memberships_for_firm(&state.surreal, firm_id)
            .await
            .map_err(|error| ApiError::Db(error.to_string()))?;
        for membership in memberships {
            if by_person.contains_key(&membership.person_id) {
                continue;
            }
            let Some(person) = store::persons::find_by_id(&state.surreal, membership.person_id)
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
) -> Result<(Option<store::projects::Project>, Vec<Attorney>), ApiError> {
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

//! Read the responsible attorney's free hours and project a Deadline work plan.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, NaiveTime, TimeDelta, TimeZone, Utc};
use chrono_tz::Tz;
use cloud::calendar::{CalendarEvent, CalendarService, EventTiming};
use model::deadline_schedule::{allocate, working_hours, Window, Work, WorkSchedule};
use store::application_store::ApplicationStore;
use uuid::Uuid;

use crate::integrations::IntegrationProviders;

const MARKER: &str = "navigator_deadline_work";
const VERSION: &str = "v1";
const KEY: &str = "deadline_work_interval";

#[derive(serde::Serialize)]
pub(crate) struct Plan {
    pub schedule: WorkSchedule,
    pub busy: Vec<Window>,
    pub working_hours: Vec<Window>,
    pub booked: bool,
    #[serde(skip)]
    urls: BTreeMap<Uuid, String>,
}

type PlanResult<T> = Result<T, String>;

pub(crate) async fn plan(
    db: &ApplicationStore,
    providers: &dyn IntegrationProviders,
    person: Uuid,
    zone: Tz,
    now: DateTime<Utc>,
) -> PlanResult<Plan> {
    let pg = store::attorney_calendars::postgres()
        .await
        .map_err(|e| e.to_string())?;
    let finish = now + TimeDelta::days(30);
    let start = zone
        .from_local_datetime(
            &now.with_timezone(&zone)
                .date_naive()
                .and_time(NaiveTime::MIN),
        )
        .earliest()
        .map_or(now, |time| time.to_utc());
    let horizon = Window { start, end: finish };
    let mut calendars_to_read = store::attorney_calendars::listed(&pg, &[person])
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|row| row.calendar_id)
        .collect::<BTreeSet<_>>();
    if calendars_to_read.is_empty() {
        return Err("record the attorney's calendar for Deadline scheduling".into());
    }
    let inputs = project_schedule_inputs(db, providers, &pg, person, now, finish).await?;
    calendars_to_read.extend(inputs.calendars);
    let person = store::persons::find_by_id(db, person)
        .await
        .map_err(|e| e.to_string())?
        .filter(|person| person.email_confirmed)
        .ok_or_else(|| "the attorney's mailbox is unverified".to_string())?;
    let ids: Vec<_> = calendars_to_read.into_iter().collect();
    let provider = providers
        .attorney_calendars()
        .await
        .map_err(|e| e.to_string())?;
    let mut windows = Vec::new();
    for chunk in ids.chunks(50) {
        let report = provider
            .free_busy(
                &person.email,
                chunk,
                start.fixed_offset(),
                finish.fixed_offset(),
            )
            .await
            .map_err(|e| e.to_string())?;
        let returned = report
            .iter()
            .map(|calendar| calendar.calendar_id.as_str())
            .collect::<BTreeSet<_>>();
        if report.len() != chunk.len()
            || chunk.iter().any(|id| !returned.contains(id.as_str()))
            || report.iter().any(|calendar| calendar.gap.is_some())
        {
            return Err("an attorney or Project Calendar could not report availability".into());
        }
        windows.extend(report);
    }
    let mut busy: Vec<_> = windows
        .into_iter()
        .flat_map(|calendar| calendar.busy)
        .map(|block| Window {
            start: block.start.to_utc(),
            end: block.end.to_utc(),
        })
        .collect();
    busy.extend(inputs.busy);
    let working_hours = working_hours(zone, horizon, inputs.close);
    let schedule = allocate(inputs.work, &working_hours, &busy);
    Ok(Plan {
        schedule,
        busy,
        working_hours,
        booked: false,
        urls: inputs.urls,
    })
}

struct ProjectInputs {
    calendars: BTreeSet<String>,
    busy: Vec<Window>,
    work: Vec<Work>,
    close: NaiveTime,
    urls: BTreeMap<Uuid, String>,
}

async fn project_schedule_inputs(
    db: &ApplicationStore,
    providers: &dyn IntegrationProviders,
    pg: &store::postgres::PostgresDb,
    person: Uuid,
    now: DateTime<Utc>,
    finish: DateTime<Utc>,
) -> PlanResult<ProjectInputs> {
    let memberships = store::firms::memberships_for_person(db, person)
        .await
        .map_err(|e| e.to_string())?;
    let firms: Vec<_> = memberships
        .iter()
        .map(|membership| membership.firm_id)
        .collect();
    let calendars = store::project_calendar::shared_freebusy_calendars(pg, &firms, &[person])
        .await
        .map_err(|e| e.to_string())?;
    let mut calendars_to_read = BTreeSet::new();
    let mut work = Vec::new();
    let mut project_busy = Vec::new();
    let mut close = None;
    let mut urls = BTreeMap::new();
    for (firm, projects) in calendars {
        let policy = store::project_deadlines::scheduling_policy(Some(firm))
            .await
            .map_err(|e| e.to_string())?;
        close = Some(
            close.map_or(policy.end_of_business(), |time: chrono::NaiveTime| {
                time.min(policy.end_of_business())
            }),
        );
        let Some(first) = projects.first() else {
            continue;
        };
        let reader = providers
            .calendar(db, first.project_id)
            .await
            .map_err(|e| e.to_string())?;
        let verified = reader
            .owned_calendar_codes()
            .await
            .map_err(|e| e.to_string())?;
        for project in &projects {
            let id = project
                .calendar_id
                .as_ref()
                .filter(|id| verified.get(*id) == Some(&project.code))
                .ok_or_else(|| "a Project Calendar is unavailable".to_string())?;
            calendars_to_read.insert(id.clone());
        }
        let ids: Vec<_> = projects.iter().map(|project| project.project_id).collect();
        for row in store::project_deadlines::open_for_projects(&ids)
            .await
            .map_err(|e| e.to_string())?
        {
            if let Some((start, end)) = row.buffered_window(policy) {
                project_busy.push(Window {
                    start: start.to_utc(),
                    end: end.to_utc(),
                });
            }
            if row.owner_person_id == Some(person) {
                if let Some(task) = row
                    .work(policy, now)
                    .filter(|task| task.window.end <= finish)
                {
                    work.push(task);
                    if let Some(project) = projects
                        .iter()
                        .find(|project| project.project_id == row.project_id)
                    {
                        urls.insert(
                            row.id,
                            format!(
                                "/app/projects/{}/deadlines#deadline-{}",
                                project.code, row.id
                            ),
                        );
                    }
                }
            }
        }
    }
    let close =
        close.ok_or_else(|| "enable Firm Google Calendar for Deadline scheduling".to_string())?;
    Ok(ProjectInputs {
        calendars: calendars_to_read,
        busy: project_busy,
        work,
        close,
        urls,
    })
}

/// Reconcile the private booking calendar against a complete availability read.
pub(crate) async fn sync(
    db: &ApplicationStore,
    providers: &dyn IntegrationProviders,
    person: Uuid,
    zone: Tz,
    base_url: Option<&str>,
) -> PlanResult<Plan> {
    let _guard = crate::integrations_api::calendar_lock(person)
        .await
        .map_err(|()| "the attorney's scheduling lock is unavailable".to_string())?;
    let pg = store::attorney_calendars::postgres()
        .await
        .map_err(|e| e.to_string())?;
    let mut plan = plan(db, providers, person, zone, Utc::now()).await?;
    let Some(target) = store::attorney_calendars::booking_calendar(&pg, person)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(plan);
    };
    let attorney = store::persons::find_by_id(db, person)
        .await
        .map_err(|e| e.to_string())?
        .filter(|person| person.email_confirmed)
        .ok_or_else(|| "the attorney's mailbox is unverified".to_string())?;
    let writer = cloud::calendar::GoogleCalendar::for_owner(&attorney.email)
        .await
        .map_err(|e| e.to_string())?;
    writer
        .verify_booking_calendar(&target)
        .await
        .map_err(|e| e.to_string())?;
    let stored = writer
        .mirrored_events(&target, MARKER, VERSION)
        .await
        .map_err(|e| e.to_string())?;
    if stored
        .iter()
        .any(|row| !row.can_write() || !row.private_properties.contains_key(KEY))
    {
        return Err("an existing Deadline work event could not be verified".into());
    }
    let mut keep = BTreeSet::new();
    for (id, blocks) in &plan.schedule.blocks {
        for block in blocks {
            let key = format!("{id}/{}/{}", block.start.timestamp(), block.end.timestamp());
            if let Some(row) = stored
                .iter()
                .find(|row| row.private_properties.get(KEY) == Some(&key))
            {
                keep.insert(row.id.as_str());
                continue;
            }
            writer
                .insert_event(
                    &target,
                    &CalendarEvent {
                        summary: "Deadline work".into(),
                        description: Some(match (base_url, plan.urls.get(id)) {
                            (Some(base), Some(path)) => {
                                format!("{}{path}", base.trim_end_matches('/'))
                            }
                            _ => format!("Deadline {id}"),
                        }),
                        location: None,
                        color_id: None,
                        start: block.start.fixed_offset(),
                        timing: EventTiming::BusyUntil(block.end.fixed_offset()),
                        sequence: 0,
                        cancelled: false,
                        reminder_minutes: Vec::new(),
                        private_properties: BTreeMap::from([
                            (MARKER.into(), VERSION.into()),
                            (KEY.into(), key),
                        ]),
                    },
                )
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    for row in &stored {
        if !keep.contains(row.id.as_str()) {
            writer
                .delete_event(&target, row)
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    plan.booked = true;
    Ok(plan)
}

/// Compare buffered appointments with readable Calendar events. The store checks
/// its own Deadline rows again while committing the schedule.
pub(crate) async fn check_appearance(
    state: &crate::api::ApiState,
    session: &crate::session::SessionData,
    project: Uuid,
    starts: &[DateTime<chrono::FixedOffset>],
    duration: u32,
    policy: model::project_deadlines::SchedulingPolicy,
    replacing: bool,
) -> Result<(), crate::api::ApiError> {
    use crate::api::ApiError;
    let Some(first) = starts.first() else {
        return Ok(());
    };
    let windows: Vec<_> = starts
        .iter()
        .filter_map(|start| policy.appearance(*start, duration))
        .collect();
    let start = windows
        .iter()
        .map(|(from, _)| *from)
        .min()
        .unwrap_or(*first);
    let end = windows
        .iter()
        .map(|(_, until)| *until)
        .max()
        .unwrap_or(*first);
    if end <= start {
        return Ok(());
    }
    let mut conflicts = BTreeSet::new();
    let mut consider = |event: cloud::ProjectCalendarEvent| {
        if crate::attorney_calendars_api::event_bounds(&event).is_some_and(|(from, until)| {
            windows
                .iter()
                .any(|(start, end)| from < *end && until > *start)
        }) {
            conflicts.insert(
                event
                    .summary
                    .unwrap_or_else(|| "Calendar appointment".into()),
            );
        }
    };
    if let Ok(Some(project)) = store::projects::find_by_id(&state.application_store, project).await
    {
        if let (Some(id), Ok(provider)) = (
            project.calendar_id.as_deref(),
            state
                .integration_providers
                .calendar(&state.application_store, project.id)
                .await,
        ) {
            let managed = if replacing {
                provider
                    .mirrored_events(
                        id,
                        store::deadline_calendar::MARKER_KEY,
                        store::deadline_calendar::MARKER_VALUE,
                    )
                    .await
                    .ok()
                    .map(|rows| rows.into_iter().map(|row| row.id).collect::<BTreeSet<_>>())
            } else {
                Some(BTreeSet::new())
            };
            if let (Some(managed), Ok(Some(events))) = (
                managed,
                provider.project_events(&project.code, id, start, end).await,
            ) {
                events
                    .into_iter()
                    .filter(|event| !managed.contains(&event.id))
                    .for_each(&mut consider);
            }
        }
    }
    if let Some(events) =
        crate::attorney_calendars_api::own_events(state, session, start, end).await
    {
        events.into_iter().for_each(consider);
    }
    if conflicts.is_empty() {
        Ok(())
    } else {
        Err(ApiError::DeadlineInvalid(format!(
            "appearance overlaps {}",
            conflicts.into_iter().collect::<Vec<_>>().join(", ")
        )))
    }
}

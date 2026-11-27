//! Read the responsible attorney's free hours and project a Deadline work plan.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, TimeDelta, Utc};
use chrono_tz::Tz;
use cloud::calendar::{CalendarEvent, CalendarService, EventTiming};
use model::deadline_schedule::{allocate, working_hours, Window, WorkSchedule};
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
    let memberships = store::firms::memberships_for_person(db, person)
        .await
        .map_err(|e| e.to_string())?;
    let firms: Vec<_> = memberships
        .iter()
        .map(|membership| membership.firm_id)
        .collect();
    let calendars = store::project_calendar::shared_freebusy_calendars(&pg, &firms, &[person])
        .await
        .map_err(|e| e.to_string())?;
    let finish = now + TimeDelta::days(30);
    let horizon = Window {
        start: now,
        end: finish,
    };
    let mut calendars_to_read = store::attorney_calendars::listed(&pg, &[person])
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|row| row.calendar_id)
        .collect::<BTreeSet<_>>();
    if calendars_to_read.is_empty() {
        return Err("record the attorney's calendar for Deadline scheduling".into());
    }
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
    let person = store::persons::find_by_id(db, person)
        .await
        .map_err(|e| e.to_string())?
        .filter(|person| person.email_confirmed)
        .ok_or_else(|| "the attorney's mailbox is unverified".to_string())?;
    if close.is_none() {
        return Err("record the attorney's calendar and enable Firm Google Calendar".into());
    }
    let ids: Vec<_> = calendars_to_read.into_iter().collect();
    let provider = providers
        .attorney_calendars()
        .await
        .map_err(|e| e.to_string())?;
    let windows = provider
        .free_busy(
            &person.email,
            &ids,
            now.fixed_offset(),
            finish.fixed_offset(),
        )
        .await
        .map_err(|e| e.to_string())?;
    if windows.len() != ids.len() || windows.iter().any(|calendar| calendar.gap.is_some()) {
        return Err("an attorney or Project Calendar could not report availability".into());
    }
    let mut busy: Vec<_> = windows
        .into_iter()
        .flat_map(|calendar| calendar.busy)
        .map(|block| Window {
            start: block.start.to_utc(),
            end: block.end.to_utc(),
        })
        .collect();
    busy.extend(project_busy);
    let close = close.ok_or_else(|| "the Firm's working hours are unavailable".to_string())?;
    let working_hours = working_hours(zone, horizon, close);
    let schedule = allocate(work, &working_hours, &busy);
    Ok(Plan {
        schedule,
        busy,
        working_hours,
        booked: false,
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

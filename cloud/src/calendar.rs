//! A Project's shared Google Calendar on the Firm's Workspace.
//!
//! One secondary calendar per Project, named for the Project code, holds the
//! matter's events. A [`CalendarService`] creates or finds that calendar,
//! manages who it is shared with, and writes the events Navigator mirrors
//! onto it. What an event means — a mirrored Deadline, a scheduled call — is
//! the caller's concern; the mirror's own identity travels in private
//! extended properties.
//!
//! The Calendar API has no resource for appointment schedules, so a client
//! booking page cannot be provisioned here. A lawyer makes one on the Project
//! calendar in the Google web UI and records its address on the Project.
//!
//! Authentication reuses the Workspace service account Drive uses
//! ([`crate::drive::DriveWorkspaceConfig`]): the same delegated user owns the
//! calendars, and the account's domain-wide delegation must also grant the
//! calendar scope.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const CALENDAR_SCOPE: &str = "https://www.googleapis.com/auth/calendar";
const CALENDAR_BASE_URL: &str = "https://www.googleapis.com/calendar/v3";

/// The environment variables a calendar provider needs. They are Drive's
/// Workspace coordinates, because the calendar is owned by the same
/// delegated user.
pub const CALENDAR_CONFIGURATION_KEYS: [&str; 2] = [
    "NAVIGATOR_DRIVE_NEON_LAW_DELEGATED_USER",
    "NAVIGATOR_DRIVE_NEON_LAW_SERVICE_ACCOUNT_JSON",
];

/// A Project's calendar, by its Calendar API id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectCalendar {
    pub id: String,
    /// The Project code the calendar is named for.
    pub summary: String,
}

/// A calendar access role. `Owner` is the delegated user and is never changed
/// here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd)]
pub enum CalendarRole {
    Reader,
    Writer,
    Owner,
}

impl CalendarRole {
    const fn api_value(self) -> &'static str {
        match self {
            Self::Reader => "reader",
            Self::Writer => "writer",
            Self::Owner => "owner",
        }
    }

    fn from_api(value: &str) -> Option<Self> {
        match value {
            "reader" | "freeBusyReader" => Some(Self::Reader),
            "writer" => Some(Self::Writer),
            "owner" => Some(Self::Owner),
            _ => None,
        }
    }
}

/// One person a calendar is shared with. The address is never attached to a
/// tracing span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarMember {
    pub email: String,
    pub role: CalendarRole,
}

/// One event on a Project calendar, as Navigator writes it.
///
/// An event written through this API is an instant: its end is its start, the
/// zero-duration reading an ICS event without a `DTEND` carries, so a
/// Deadline marks a moment instead of occupying a lawyer's morning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarEvent {
    pub summary: String,
    pub description: Option<String>,
    pub location: Option<String>,
    /// The instant the event marks, with its own offset.
    pub start: DateTime<FixedOffset>,
    /// RFC 5545 `SEQUENCE`, raised on every update.
    pub sequence: i32,
    /// A cancelled event keeps its place on the calendar with
    /// `status: cancelled`, the way the ICS feed renders a cancelled row.
    pub cancelled: bool,
    /// Display reminder minutes before `start`. `None` writes no reminder and
    /// suppresses the calendar's defaults, so an event without a reminder
    /// stays silent.
    pub reminder_minutes: Option<i64>,
    /// Private extended properties carrying the mirror's identity: the source
    /// row's record id and the body sha a reconcile compares against the row
    /// it now reads. Only Navigator reads these.
    pub private_properties: BTreeMap<String, String>,
}

/// An event as the calendar holds it: the Calendar API's own id and the
/// private extended properties it was last written with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEvent {
    pub id: String,
    pub private_properties: BTreeMap<String, String>,
}

#[derive(Debug, Error)]
pub enum CalendarError {
    #[error("missing required Calendar configuration: {0}")]
    MissingConfig(String),
    #[error("Calendar authentication failed")]
    Authentication,
    #[error("Calendar request failed while {action}")]
    Request {
        action: &'static str,
        #[source]
        source: reqwest::Error,
    },
    #[error("Calendar API returned {status} while {action}")]
    Api { action: &'static str, status: u16 },
    #[error("Calendar API returned an invalid response while {action}")]
    Response {
        action: &'static str,
        #[source]
        source: reqwest::Error,
    },
    #[error("Calendar API response while creating a calendar did not include an id")]
    MissingCalendarId,
    #[error("Calendar API response while writing an event did not include an id")]
    MissingEventId,
    #[error("Calendar found {count} calendars for one project code; refusing to guess")]
    AmbiguousSummary { count: usize },
}

/// Create, find, and share a Project calendar.
#[async_trait]
pub trait CalendarService: Send + Sync {
    /// The delegated user's own calendar named `summary`, if exactly one.
    async fn find_calendar(&self, summary: &str) -> Result<Option<ProjectCalendar>, CalendarError>;
    async fn create_calendar(&self, summary: &str) -> Result<ProjectCalendar, CalendarError>;
    /// Whether a calendar with this id still exists. Checked by id, never by
    /// name, so a renamed calendar is still the recorded one.
    async fn calendar_exists(&self, calendar_id: &str) -> Result<bool, CalendarError>;
    /// Every person the calendar is shared with, owner included.
    async fn members(&self, calendar_id: &str) -> Result<Vec<CalendarMember>, CalendarError>;
    /// Share the calendar with `email` at `role`, without a notification
    /// email. Re-sharing at another role changes the role.
    async fn set_member_role(
        &self,
        calendar_id: &str,
        email: &str,
        role: CalendarRole,
    ) -> Result<(), CalendarError>;

    /// Insert the event and read back its stored identity.
    async fn insert_event(
        &self,
        calendar_id: &str,
        event: &CalendarEvent,
    ) -> Result<StoredEvent, CalendarError>;

    /// Overwrite the event's Navigator-written fields, leaving everything a
    /// person set in Google — its color, its guests — alone.
    async fn patch_event(
        &self,
        calendar_id: &str,
        event_id: &str,
        event: &CalendarEvent,
    ) -> Result<StoredEvent, CalendarError>;

    /// Delete an event. Deleting one already gone succeeds: the desired end
    /// state is what the mirror reconciles to.
    async fn delete_event(&self, calendar_id: &str, event_id: &str) -> Result<(), CalendarError>;

    /// Every event on the calendar whose private extended property `key`
    /// carries `value`. The mirror's marker property finds its events in one
    /// paginated read.
    async fn mirrored_events(
        &self,
        calendar_id: &str,
        key: &str,
        value: &str,
    ) -> Result<Vec<StoredEvent>, CalendarError>;
}

/// Calendar v3 client authenticated as the Workspace service account with
/// domain-wide delegation.
pub struct GoogleCalendar {
    token_source: Arc<dyn google_cloud_token::TokenSource>,
    http: reqwest::Client,
    base_url: String,
}

impl GoogleCalendar {
    /// Build from the delegated user and the service-account key.
    ///
    /// # Errors
    /// [`CalendarError::Authentication`] when the key does not parse or no
    /// token source can be built.
    pub async fn new(
        delegated_user: &str,
        service_account_json: &str,
    ) -> Result<Self, CalendarError> {
        let credentials =
            google_cloud_auth::credentials::CredentialsFile::new_from_str(service_account_json)
                .await
                .map_err(|_| CalendarError::Authentication)?;
        let scopes = [CALENDAR_SCOPE];
        let auth_config = google_cloud_auth::project::Config::default()
            .with_scopes(&scopes)
            .with_sub(delegated_user);
        let provider = google_cloud_auth::token::DefaultTokenSourceProvider::new_with_credentials(
            auth_config,
            Box::new(credentials),
        )
        .await
        .map_err(|_| CalendarError::Authentication)?;
        Ok(Self::from_parts(
            google_cloud_token::TokenSourceProvider::token_source(&provider),
            CALENDAR_BASE_URL,
        ))
    }

    /// Build from [`CALENDAR_CONFIGURATION_KEYS`], naming every missing key.
    ///
    /// # Errors
    /// [`CalendarError::MissingConfig`] or an authentication failure.
    pub async fn from_env() -> Result<Self, CalendarError> {
        Self::from_lookup(|key| std::env::var(key).ok()).await
    }

    async fn from_lookup<F>(get: F) -> Result<Self, CalendarError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let [user_key, json_key] = CALENDAR_CONFIGURATION_KEYS;
        let value = |key: &str| get(key).filter(|value| !value.trim().is_empty());
        match (value(user_key), value(json_key)) {
            (Some(user), Some(json)) => Self::new(&user, &json).await,
            (user, json) => {
                let missing: Vec<&str> = [(user_key, user.is_none()), (json_key, json.is_none())]
                    .into_iter()
                    .filter_map(|(key, absent)| absent.then_some(key))
                    .collect();
                Err(CalendarError::MissingConfig(missing.join(", ")))
            }
        }
    }

    fn from_parts(token_source: Arc<dyn google_cloud_token::TokenSource>, base_url: &str) -> Self {
        Self {
            token_source,
            http: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    async fn token(&self) -> Result<String, CalendarError> {
        let raw = self
            .token_source
            .token()
            .await
            .map_err(|_| CalendarError::Authentication)?;
        Ok(raw.strip_prefix("Bearer ").unwrap_or(&raw).to_string())
    }

    /// `base/<segments...>`, each segment percent-encoded, so an id can never
    /// change the path it is placed in.
    fn url(&self, segments: &[&str]) -> String {
        let mut url = url::Url::parse(&self.base_url).expect("calendar base URL parses");
        url.path_segments_mut()
            .expect("calendar base URL has a path")
            .pop_if_empty()
            .extend(segments);
        url.to_string()
    }

    fn checked(
        response: Result<reqwest::Response, reqwest::Error>,
        action: &'static str,
    ) -> Result<reqwest::Response, CalendarError> {
        let response = response.map_err(|source| CalendarError::Request { action, source })?;
        if response.status().is_success() {
            Ok(response)
        } else {
            Err(CalendarError::Api {
                action,
                status: response.status().as_u16(),
            })
        }
    }
}

#[async_trait]
impl CalendarService for GoogleCalendar {
    async fn find_calendar(&self, summary: &str) -> Result<Option<ProjectCalendar>, CalendarError> {
        let action = "listing calendars";
        let mut found = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let token = self.token().await?;
            let mut query = vec![("minAccessRole", "owner".to_string())];
            if let Some(page) = &page_token {
                query.push(("pageToken", page.clone()));
            }
            let page = Self::checked(
                self.http
                    .get(self.url(&["users", "me", "calendarList"]))
                    .bearer_auth(token)
                    .query(&query)
                    .send()
                    .await,
                action,
            )?
            .json::<CalendarListPage>()
            .await
            .map_err(|source| CalendarError::Response { action, source })?;
            found.extend(
                page.items
                    .into_iter()
                    .filter(|entry| entry.summary.as_deref() == Some(summary))
                    .map(|entry| ProjectCalendar {
                        id: entry.id,
                        summary: summary.to_string(),
                    }),
            );
            page_token = page.next_page_token;
            if page_token.is_none() {
                break;
            }
        }
        match found.len() {
            0 => Ok(None),
            1 => Ok(found.pop()),
            count => Err(CalendarError::AmbiguousSummary { count }),
        }
    }

    async fn create_calendar(&self, summary: &str) -> Result<ProjectCalendar, CalendarError> {
        let action = "creating a calendar";
        let token = self.token().await?;
        let created = Self::checked(
            self.http
                .post(self.url(&["calendars"]))
                .bearer_auth(token)
                .json(&serde_json::json!({ "summary": summary }))
                .send()
                .await,
            action,
        )?
        .json::<CalendarResource>()
        .await
        .map_err(|source| CalendarError::Response { action, source })?;
        let id = created
            .id
            .filter(|id| !id.is_empty())
            .ok_or(CalendarError::MissingCalendarId)?;
        tracing::info!(
            project_code = summary,
            outcome = "created",
            "Project calendar provisioning completed"
        );
        Ok(ProjectCalendar {
            id,
            summary: summary.to_string(),
        })
    }

    async fn calendar_exists(&self, calendar_id: &str) -> Result<bool, CalendarError> {
        let action = "reading a calendar";
        let token = self.token().await?;
        let response = self
            .http
            .get(self.url(&["calendars", calendar_id]))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|source| CalendarError::Request { action, source })?;
        match response.status().as_u16() {
            status if (200..300).contains(&status) => Ok(true),
            404 | 410 => Ok(false),
            status => Err(CalendarError::Api { action, status }),
        }
    }

    async fn members(&self, calendar_id: &str) -> Result<Vec<CalendarMember>, CalendarError> {
        let action = "listing calendar sharing";
        let mut members = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let token = self.token().await?;
            let mut query = Vec::new();
            if let Some(page) = &page_token {
                query.push(("pageToken", page.clone()));
            }
            let page = Self::checked(
                self.http
                    .get(self.url(&["calendars", calendar_id, "acl"]))
                    .bearer_auth(token)
                    .query(&query)
                    .send()
                    .await,
                action,
            )?
            .json::<AclPage>()
            .await
            .map_err(|source| CalendarError::Response { action, source })?;
            members.extend(page.items.into_iter().filter_map(|rule| {
                if rule.scope.kind != "user" {
                    return None;
                }
                Some(CalendarMember {
                    email: rule.scope.value?,
                    role: CalendarRole::from_api(&rule.role)?,
                })
            }));
            page_token = page.next_page_token;
            if page_token.is_none() {
                break;
            }
        }
        Ok(members)
    }

    async fn set_member_role(
        &self,
        calendar_id: &str,
        email: &str,
        role: CalendarRole,
    ) -> Result<(), CalendarError> {
        let token = self.token().await?;
        // Inserting a rule for a scope that already has one updates its role,
        // so this one call both shares and re-shares.
        Self::checked(
            self.http
                .post(self.url(&["calendars", calendar_id, "acl"]))
                .bearer_auth(token)
                .query(&[("sendNotifications", "false")])
                .json(&serde_json::json!({
                    "role": role.api_value(),
                    "scope": { "type": "user", "value": email },
                }))
                .send()
                .await,
            "sharing a calendar",
        )?;
        tracing::info!(
            role = role.api_value(),
            outcome = "shared",
            "Project calendar sharing reconciliation completed"
        );
        Ok(())
    }

    async fn insert_event(
        &self,
        calendar_id: &str,
        event: &CalendarEvent,
    ) -> Result<StoredEvent, CalendarError> {
        let action = "inserting an event";
        let token = self.token().await?;
        let stored = Self::checked(
            self.http
                .post(self.url(&["calendars", calendar_id, "events"]))
                .bearer_auth(token)
                .json(&event_body(event))
                .send()
                .await,
            action,
        )?
        .json::<EventResource>()
        .await
        .map_err(|source| CalendarError::Response { action, source })?;
        stored_event(&stored)
    }

    async fn patch_event(
        &self,
        calendar_id: &str,
        event_id: &str,
        event: &CalendarEvent,
    ) -> Result<StoredEvent, CalendarError> {
        let action = "updating an event";
        let token = self.token().await?;
        let stored = Self::checked(
            self.http
                .patch(self.url(&["calendars", calendar_id, "events", event_id]))
                .bearer_auth(token)
                .json(&event_body(event))
                .send()
                .await,
            action,
        )?
        .json::<EventResource>()
        .await
        .map_err(|source| CalendarError::Response { action, source })?;
        stored_event(&stored)
    }

    async fn delete_event(&self, calendar_id: &str, event_id: &str) -> Result<(), CalendarError> {
        let action = "deleting an event";
        let token = self.token().await?;
        let response = self
            .http
            .delete(self.url(&["calendars", calendar_id, "events", event_id]))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|source| CalendarError::Request { action, source })?;
        match response.status().as_u16() {
            status if (200..300).contains(&status) => Ok(()),
            // The event a reconcile was about to delete someone else already
            // deleted; the end state is the same.
            404 | 410 => Ok(()),
            status => Err(CalendarError::Api { action, status }),
        }
    }

    async fn mirrored_events(
        &self,
        calendar_id: &str,
        key: &str,
        value: &str,
    ) -> Result<Vec<StoredEvent>, CalendarError> {
        let action = "listing mirrored events";
        let mut found = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let token = self.token().await?;
            let mut query = vec![
                ("privateExtendedProperty", format!("{key}:{value}")),
                ("maxResults", "250".to_string()),
            ];
            if let Some(page) = &page_token {
                query.push(("pageToken", page.clone()));
            }
            let page = Self::checked(
                self.http
                    .get(self.url(&["calendars", calendar_id, "events"]))
                    .bearer_auth(token)
                    .query(&query)
                    .send()
                    .await,
                action,
            )?
            .json::<EventPage>()
            .await
            .map_err(|source| CalendarError::Response { action, source })?;
            for event in &page.items {
                found.push(stored_event(event)?);
            }
            page_token = page.next_page_token;
            if page_token.is_none() {
                break;
            }
        }
        Ok(found)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CalendarListPage {
    #[serde(default)]
    items: Vec<CalendarListEntry>,
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct CalendarListEntry {
    id: String,
    summary: Option<String>,
}

#[derive(Deserialize)]
struct CalendarResource {
    id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AclPage {
    #[serde(default)]
    items: Vec<AclRule>,
    next_page_token: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct AclRule {
    role: String,
    scope: AclScope,
}

#[derive(Deserialize, Serialize)]
struct AclScope {
    #[serde(rename = "type")]
    kind: String,
    value: Option<String>,
}

/// One page of a filtered events read.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventPage {
    #[serde(default)]
    items: Vec<EventResource>,
    next_page_token: Option<String>,
}

/// One event as the Calendar API returns it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventResource {
    id: Option<String>,
    extended_properties: Option<EventExtendedProperties>,
}

/// The `extendedProperties` object; only the `private` map is ours.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct EventExtendedProperties {
    #[serde(default)]
    private: BTreeMap<String, String>,
}

/// The event body Navigator writes. `end` repeats `start`: an event written
/// here marks a moment, and the ICS feed it mirrors renders no `DTEND`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EventBody<'a> {
    summary: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    location: Option<&'a str>,
    start: EventTime,
    end: EventTime,
    status: &'static str,
    transparency: &'static str,
    sequence: i32,
    reminders: EventReminders,
    extended_properties: EventExtendedProperties,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EventTime {
    date_time: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EventReminders {
    use_default: bool,
    overrides: Vec<EventReminder>,
}

#[derive(Serialize)]
struct EventReminder {
    method: &'static str,
    minutes: i64,
}

fn event_body(event: &CalendarEvent) -> EventBody<'_> {
    let start = event.start.to_rfc3339();
    EventBody {
        summary: &event.summary,
        description: event.description.as_deref(),
        location: event.location.as_deref(),
        start: EventTime {
            date_time: start.clone(),
        },
        end: EventTime { date_time: start },
        status: if event.cancelled {
            "cancelled"
        } else {
            "confirmed"
        },
        transparency: "transparent",
        sequence: event.sequence,
        reminders: EventReminders {
            use_default: false,
            overrides: event
                .reminder_minutes
                .map(|minutes| {
                    vec![EventReminder {
                        method: "popup",
                        minutes,
                    }]
                })
                .unwrap_or_default(),
        },
        extended_properties: EventExtendedProperties {
            private: event.private_properties.clone(),
        },
    }
}

fn stored_event(resource: &EventResource) -> Result<StoredEvent, CalendarError> {
    let id = resource
        .id
        .clone()
        .filter(|id| !id.is_empty())
        .ok_or(CalendarError::MissingEventId)?;
    Ok(StoredEvent {
        id,
        private_properties: resource
            .extended_properties
            .as_ref()
            .map(|properties| properties.private.clone())
            .unwrap_or_default(),
    })
}

/// In-memory calendars for store and door tests. The delegated user is
/// `owner@calendar.test`, shared as `Owner` on every calendar it creates.
#[derive(Clone, Debug, Default)]
pub struct FakeCalendar {
    state: Arc<Mutex<FakeCalendarState>>,
}

#[derive(Debug, Default)]
struct FakeCalendarState {
    calendars: BTreeMap<String, FakeCalendarEntry>,
    events: BTreeMap<String, BTreeMap<String, CalendarEvent>>,
    next_id: usize,
    created: usize,
}

#[derive(Debug)]
struct FakeCalendarEntry {
    summary: String,
    members: BTreeMap<String, CalendarRole>,
}

/// The delegated user that owns every [`FakeCalendar`] calendar.
pub const FAKE_CALENDAR_OWNER: &str = "owner@calendar.test";

impl FakeCalendar {
    /// How many calendars [`CalendarService::create_calendar`] made.
    #[must_use]
    pub fn created(&self) -> usize {
        self.state
            .lock()
            .expect("fake calendar mutex poisoned")
            .created
    }

    /// Every calendar's id and summary, in id order.
    #[must_use]
    pub fn calendars(&self) -> Vec<ProjectCalendar> {
        let state = self.state.lock().expect("fake calendar mutex poisoned");
        state
            .calendars
            .iter()
            .map(|(id, entry)| ProjectCalendar {
                id: id.clone(),
                summary: entry.summary.clone(),
            })
            .collect()
    }

    /// Who a calendar is shared with, by email.
    #[must_use]
    pub fn sharing(&self, calendar_id: &str) -> BTreeMap<String, CalendarRole> {
        let state = self.state.lock().expect("fake calendar mutex poisoned");
        state
            .calendars
            .get(calendar_id)
            .map(|entry| entry.members.clone())
            .unwrap_or_default()
    }

    /// Delete a calendar outside Navigator, as a person could in Google.
    pub fn delete(&self, calendar_id: &str) {
        let mut state = self.state.lock().expect("fake calendar mutex poisoned");
        state.calendars.remove(calendar_id);
        state.events.remove(calendar_id);
    }

    /// Every event a calendar holds, in id order.
    #[must_use]
    pub fn events(&self, calendar_id: &str) -> Vec<StoredEvent> {
        let state = self.state.lock().expect("fake calendar mutex poisoned");
        state
            .events
            .get(calendar_id)
            .map(|events| {
                events
                    .iter()
                    .map(|(id, event)| StoredEvent {
                        id: id.clone(),
                        private_properties: event.private_properties.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Delete an event outside Navigator, as a person could in Google.
    pub fn remove_event(&self, calendar_id: &str, event_id: &str) {
        let mut state = self.state.lock().expect("fake calendar mutex poisoned");
        if let Some(events) = state.events.get_mut(calendar_id) {
            events.remove(event_id);
        }
    }

    /// Overwrite an event outside Navigator, as a person could in Google.
    pub fn put_event(&self, calendar_id: &str, event_id: &str, event: CalendarEvent) {
        let mut state = self.state.lock().expect("fake calendar mutex poisoned");
        if let Some(events) = state.events.get_mut(calendar_id) {
            events.insert(event_id.to_string(), event);
        }
    }
}

#[async_trait]
impl CalendarService for FakeCalendar {
    async fn find_calendar(&self, summary: &str) -> Result<Option<ProjectCalendar>, CalendarError> {
        let state = self.state.lock().expect("fake calendar mutex poisoned");
        let found: Vec<_> = state
            .calendars
            .iter()
            .filter(|(_, entry)| entry.summary == summary)
            .map(|(id, _)| ProjectCalendar {
                id: id.clone(),
                summary: summary.to_string(),
            })
            .collect();
        match found.len() {
            0 => Ok(None),
            1 => Ok(found.into_iter().next()),
            count => Err(CalendarError::AmbiguousSummary { count }),
        }
    }

    async fn create_calendar(&self, summary: &str) -> Result<ProjectCalendar, CalendarError> {
        let mut state = self.state.lock().expect("fake calendar mutex poisoned");
        state.next_id += 1;
        state.created += 1;
        let id = format!("c_{}@group.calendar.test", state.next_id);
        state.calendars.insert(
            id.clone(),
            FakeCalendarEntry {
                summary: summary.to_string(),
                members: BTreeMap::from([(FAKE_CALENDAR_OWNER.to_string(), CalendarRole::Owner)]),
            },
        );
        Ok(ProjectCalendar {
            id,
            summary: summary.to_string(),
        })
    }

    async fn calendar_exists(&self, calendar_id: &str) -> Result<bool, CalendarError> {
        let state = self.state.lock().expect("fake calendar mutex poisoned");
        Ok(state.calendars.contains_key(calendar_id))
    }

    async fn members(&self, calendar_id: &str) -> Result<Vec<CalendarMember>, CalendarError> {
        Ok(self
            .sharing(calendar_id)
            .into_iter()
            .map(|(email, role)| CalendarMember { email, role })
            .collect())
    }

    async fn set_member_role(
        &self,
        calendar_id: &str,
        email: &str,
        role: CalendarRole,
    ) -> Result<(), CalendarError> {
        let mut state = self.state.lock().expect("fake calendar mutex poisoned");
        match state.calendars.get_mut(calendar_id) {
            Some(entry) => {
                entry.members.insert(email.to_string(), role);
                Ok(())
            }
            None => Err(CalendarError::Api {
                action: "sharing a calendar",
                status: 404,
            }),
        }
    }

    async fn insert_event(
        &self,
        calendar_id: &str,
        event: &CalendarEvent,
    ) -> Result<StoredEvent, CalendarError> {
        let mut state = self.state.lock().expect("fake calendar mutex poisoned");
        if !state.calendars.contains_key(calendar_id) {
            return Err(CalendarError::Api {
                action: "inserting an event",
                status: 404,
            });
        }
        state.next_id += 1;
        let id = format!("e_{}", state.next_id);
        state
            .events
            .entry(calendar_id.to_string())
            .or_default()
            .insert(id.clone(), event.clone());
        Ok(StoredEvent {
            id,
            private_properties: event.private_properties.clone(),
        })
    }

    async fn patch_event(
        &self,
        calendar_id: &str,
        event_id: &str,
        event: &CalendarEvent,
    ) -> Result<StoredEvent, CalendarError> {
        let mut state = self.state.lock().expect("fake calendar mutex poisoned");
        match state
            .events
            .get_mut(calendar_id)
            .and_then(|events| events.get_mut(event_id))
        {
            Some(stored) => {
                *stored = event.clone();
                Ok(StoredEvent {
                    id: event_id.to_string(),
                    private_properties: event.private_properties.clone(),
                })
            }
            None => Err(CalendarError::Api {
                action: "updating an event",
                status: 404,
            }),
        }
    }

    async fn delete_event(&self, calendar_id: &str, event_id: &str) -> Result<(), CalendarError> {
        let mut state = self.state.lock().expect("fake calendar mutex poisoned");
        if let Some(events) = state.events.get_mut(calendar_id) {
            events.remove(event_id);
        }
        Ok(())
    }

    async fn mirrored_events(
        &self,
        calendar_id: &str,
        key: &str,
        value: &str,
    ) -> Result<Vec<StoredEvent>, CalendarError> {
        Ok(self
            .events(calendar_id)
            .into_iter()
            .filter(|stored| {
                stored
                    .private_properties
                    .get(key)
                    .is_some_and(|v| v == value)
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use async_trait::async_trait;
    use wiremock::matchers::{body_json, body_partial_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::{
        CalendarError, CalendarEvent, CalendarRole, CalendarService, FakeCalendar, GoogleCalendar,
        StoredEvent,
    };

    #[derive(Debug)]
    struct StaticToken;

    #[async_trait]
    impl google_cloud_token::TokenSource for StaticToken {
        async fn token(&self) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok("Bearer test-token".into())
        }
    }

    fn google(server: &MockServer) -> GoogleCalendar {
        GoogleCalendar::from_parts(Arc::new(StaticToken), &server.uri())
    }

    #[tokio::test]
    async fn missing_configuration_names_every_key() {
        let error = GoogleCalendar::from_lookup(|_| None)
            .await
            .err()
            .expect("no configuration is refused");
        let message = error.to_string();
        assert!(
            message.contains("NAVIGATOR_DRIVE_NEON_LAW_DELEGATED_USER"),
            "{message}"
        );
        assert!(
            message.contains("NAVIGATOR_DRIVE_NEON_LAW_SERVICE_ACCOUNT_JSON"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn creates_a_calendar_named_for_the_project_code() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/calendars"))
            .and(body_json(serde_json::json!({ "summary": "matter-42" })))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "id": "c_42@group.calendar.example.com" })),
            )
            .expect(1)
            .mount(&server)
            .await;
        let calendar = google(&server).create_calendar("matter-42").await.unwrap();
        assert_eq!(calendar.id, "c_42@group.calendar.example.com");
        assert_eq!(calendar.summary, "matter-42");
    }

    #[tokio::test]
    async fn finds_an_owned_calendar_by_name_across_pages() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/users/me/calendarList"))
            .and(query_param("pageToken", "next"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [{ "id": "c_42@group.calendar.example.com", "summary": "matter-42" }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/users/me/calendarList"))
            .and(query_param("minAccessRole", "owner"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [{ "id": "c_other@group.calendar.example.com", "summary": "matter-7" }],
                "nextPageToken": "next"
            })))
            .mount(&server)
            .await;
        let found = google(&server).find_calendar("matter-42").await.unwrap();
        assert_eq!(
            found.map(|calendar| calendar.id).as_deref(),
            Some("c_42@group.calendar.example.com")
        );
    }

    #[tokio::test]
    async fn a_missing_calendar_reads_as_absent() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/calendars/c_gone@group.calendar.example.com"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        assert!(!google(&server)
            .calendar_exists("c_gone@group.calendar.example.com")
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn shares_without_a_notification_email() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/calendars/c_42@group.calendar.example.com/acl"))
            .and(query_param("sendNotifications", "false"))
            .and(body_json(serde_json::json!({
                "role": "writer",
                "scope": { "type": "user", "value": "lawyer@example.com" }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;
        google(&server)
            .set_member_role(
                "c_42@group.calendar.example.com",
                "lawyer@example.com",
                CalendarRole::Writer,
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn lists_only_people_from_the_sharing_rules() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/calendars/c_42@group.calendar.example.com/acl"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [
                    { "role": "owner", "scope": { "type": "user", "value": "owner@example.com" } },
                    { "role": "writer", "scope": { "type": "user", "value": "lawyer@example.com" } },
                    { "role": "reader", "scope": { "type": "domain", "value": "example.com" } },
                    { "role": "freeBusyReader", "scope": { "type": "default" } }
                ]
            })))
            .mount(&server)
            .await;
        let members = google(&server)
            .members("c_42@group.calendar.example.com")
            .await
            .unwrap();
        let summary: Vec<_> = members
            .iter()
            .map(|member| (member.email.as_str(), member.role))
            .collect();
        assert_eq!(
            summary,
            [
                ("owner@example.com", CalendarRole::Owner),
                ("lawyer@example.com", CalendarRole::Writer),
            ]
        );
    }

    #[tokio::test]
    async fn an_api_refusal_names_the_action_and_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/calendars"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let error = google(&server)
            .create_calendar("matter-42")
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            CalendarError::Api {
                action: "creating a calendar",
                status: 403
            }
        ));
    }

    #[tokio::test]
    async fn the_fake_shares_and_refuses_an_unknown_calendar() {
        let calendar = FakeCalendar::default();
        let created = calendar.create_calendar("matter-42").await.unwrap();
        calendar
            .set_member_role(&created.id, "lawyer@example.com", CalendarRole::Writer)
            .await
            .unwrap();
        assert_eq!(
            calendar.sharing(&created.id).get("lawyer@example.com"),
            Some(&CalendarRole::Writer)
        );
        assert_eq!(
            calendar.find_calendar("matter-42").await.unwrap(),
            Some(created)
        );
        assert!(calendar
            .set_member_role("c_unknown", "lawyer@example.com", CalendarRole::Writer)
            .await
            .is_err());
    }

    fn event() -> CalendarEvent {
        CalendarEvent {
            summary: "Court order deadline - acme".into(),
            description: Some("Authority: Order setting briefing".into()),
            location: None,
            start: DateTime::parse_from_rfc3339("2026-11-02T17:00:00-08:00").unwrap(),
            sequence: 3,
            cancelled: false,
            reminder_minutes: Some(10_080),
            private_properties: BTreeMap::from([
                ("navigator_mirror".to_string(), "deadline".to_string()),
                (
                    "deadline_id".to_string(),
                    "0190a000-0000-7000-8000-000000000001".to_string(),
                ),
                ("body_sha".to_string(), "abc123".to_string()),
            ]),
        }
    }

    fn stored(id: &str, private_properties: &[(&str, &str)]) -> StoredEvent {
        StoredEvent {
            id: id.to_string(),
            private_properties: private_properties
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect(),
        }
    }

    #[tokio::test]
    async fn insert_event_posts_an_instant_event_with_its_identity() {
        let server = MockServer::start().await;
        let calendar_id = "c_42@group.calendar.example.com";
        Mock::given(method("POST"))
            .and(path(format!("/calendars/{calendar_id}/events")))
            .and(body_json(serde_json::json!({
                "summary": "Court order deadline - acme",
                "description": "Authority: Order setting briefing",
                "start": { "dateTime": "2026-11-02T17:00:00-08:00" },
                "end": { "dateTime": "2026-11-02T17:00:00-08:00" },
                "status": "confirmed",
                "transparency": "transparent",
                "sequence": 3,
                "reminders": {
                    "useDefault": false,
                    "overrides": [{ "method": "popup", "minutes": 10_080 }]
                },
                "extendedProperties": { "private": {
                    "navigator_mirror": "deadline",
                    "deadline_id": "0190a000-0000-7000-8000-000000000001",
                    "body_sha": "abc123"
                }}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "evt_1",
                "extendedProperties": { "private": {
                    "navigator_mirror": "deadline",
                    "deadline_id": "0190a000-0000-7000-8000-000000000001",
                    "body_sha": "abc123"
                }}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let stored_event = google(&server)
            .insert_event(calendar_id, &event())
            .await
            .unwrap();
        assert_eq!(
            stored_event,
            stored(
                "evt_1",
                &[
                    ("body_sha", "abc123"),
                    ("deadline_id", "0190a000-0000-7000-8000-000000000001"),
                    ("navigator_mirror", "deadline")
                ]
            )
        );
    }

    #[tokio::test]
    async fn an_event_without_a_reminder_suppresses_the_defaults() {
        let server = MockServer::start().await;
        let calendar_id = "c_42@group.calendar.example.com";
        let mut silent = event();
        silent.reminder_minutes = None;
        Mock::given(method("POST"))
            .and(path(format!("/calendars/{calendar_id}/events")))
            .and(body_partial_json(serde_json::json!({
                "reminders": { "useDefault": false, "overrides": [] }
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "id": "evt_2" })),
            )
            .expect(1)
            .mount(&server)
            .await;
        let stored_event = google(&server)
            .insert_event(calendar_id, &silent)
            .await
            .unwrap();
        assert_eq!(stored_event.id, "evt_2");
    }

    #[tokio::test]
    async fn a_patch_overwrites_the_navigator_fields_at_the_same_event() {
        let server = MockServer::start().await;
        let calendar_id = "c_42@group.calendar.example.com";
        let mut moved = event();
        moved.sequence = 4;
        moved.start = DateTime::parse_from_rfc3339("2026-11-09T17:00:00-08:00").unwrap();
        Mock::given(method("PATCH"))
            .and(path(format!("/calendars/{calendar_id}/events/evt_1")))
            .and(body_partial_json(serde_json::json!({
                "sequence": 4,
                "start": { "dateTime": "2026-11-09T17:00:00-08:00" }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "evt_1",
                "extendedProperties": { "private": { "body_sha": "def456" } }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let stored_event = google(&server)
            .patch_event(calendar_id, "evt_1", &moved)
            .await
            .unwrap();
        assert_eq!(
            stored_event.private_properties.get("body_sha"),
            Some(&"def456".to_string())
        );
    }

    #[tokio::test]
    async fn deleting_an_event_already_gone_still_succeeds() {
        let server = MockServer::start().await;
        let calendar_id = "c_42@group.calendar.example.com";
        Mock::given(method("DELETE"))
            .and(path(format!("/calendars/{calendar_id}/events/evt_1")))
            .respond_with(ResponseTemplate::new(204))
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path(format!("/calendars/{calendar_id}/events/evt_1")))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let calendar = google(&server);
        calendar.delete_event(calendar_id, "evt_1").await.unwrap();
        calendar.delete_event(calendar_id, "evt_1").await.unwrap();
    }

    #[tokio::test]
    async fn mirrored_events_paginate_and_filter_by_the_marker() {
        let server = MockServer::start().await;
        let calendar_id = "c_42@group.calendar.example.com";
        Mock::given(method("GET"))
            .and(path(format!("/calendars/{calendar_id}/events")))
            .and(query_param(
                "privateExtendedProperty",
                "navigator_mirror:deadline",
            ))
            .and(query_param("pageToken", "next"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [
                    { "id": "evt_2", "extendedProperties": { "private": { "deadline_id": "d2" } } }
                ]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/calendars/{calendar_id}/events")))
            .and(query_param(
                "privateExtendedProperty",
                "navigator_mirror:deadline",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [
                    { "id": "evt_1", "extendedProperties": { "private": { "deadline_id": "d1" } } }
                ],
                "nextPageToken": "next"
            })))
            .mount(&server)
            .await;
        let events = google(&server)
            .mirrored_events(calendar_id, "navigator_mirror", "deadline")
            .await
            .unwrap();
        let ids: Vec<_> = events.iter().map(|event| event.id.as_str()).collect();
        assert_eq!(ids, ["evt_1", "evt_2"]);
        assert_eq!(
            events[0].private_properties.get("deadline_id"),
            Some(&"d1".to_string())
        );
    }

    #[tokio::test]
    async fn an_event_refusal_names_its_action() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/calendars/c_42@group.calendar.example.com/events"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let error = google(&server)
            .insert_event("c_42@group.calendar.example.com", &event())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            CalendarError::Api {
                action: "inserting an event",
                status: 403
            }
        ));
    }

    #[tokio::test]
    async fn the_fake_finds_patches_and_deletes_mirrored_events() {
        let calendar = FakeCalendar::default();
        let created = calendar.create_calendar("matter-42").await.unwrap();
        let first = calendar.insert_event(&created.id, &event()).await.unwrap();
        let mut second = event();
        second
            .private_properties
            .insert("deadline_id".into(), "d2".into());
        let second_stored = calendar.insert_event(&created.id, &second).await.unwrap();
        assert_eq!(
            calendar
                .mirrored_events(&created.id, "navigator_mirror", "deadline")
                .await
                .unwrap()
                .len(),
            2
        );
        let mut edited = event();
        edited.sequence = 4;
        edited
            .private_properties
            .insert("body_sha".into(), "def456".into());
        calendar
            .patch_event(&created.id, &first.id, &edited)
            .await
            .unwrap();
        assert_eq!(
            calendar
                .mirrored_events(&created.id, "body_sha", "def456")
                .await
                .unwrap()[0]
                .id,
            first.id
        );
        calendar.delete_event(&created.id, &first.id).await.unwrap();
        calendar.delete_event(&created.id, &first.id).await.unwrap();
        assert_eq!(calendar.events(&created.id).len(), 1);
        calendar.remove_event(&created.id, &second_stored.id);
        assert!(calendar.events(&created.id).is_empty());
        assert!(calendar.insert_event("c_unknown", &event()).await.is_err());
    }

    #[tokio::test]
    async fn the_fake_patches_and_refuses_an_unknown_event() {
        let calendar = FakeCalendar::default();
        let created = calendar.create_calendar("matter-42").await.unwrap();
        let inserted = calendar.insert_event(&created.id, &event()).await.unwrap();
        let mut moved = event();
        moved.start = DateTime::parse_from_rfc3339("2026-11-09T17:00:00-08:00").unwrap();
        calendar
            .patch_event(&created.id, &inserted.id, &moved)
            .await
            .unwrap();
        assert!(calendar
            .patch_event(&created.id, "e_missing", &moved)
            .await
            .is_err());
        calendar.put_event(&created.id, &inserted.id, moved);
        assert_eq!(calendar.events(&created.id).len(), 1);
    }
}

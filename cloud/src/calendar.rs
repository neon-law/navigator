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
//! ([`crate::drive::DriveWorkspaceConfig`]): a dedicated delegated user owns the
//! calendars, and the account's domain-wide delegation must also grant the
//! calendar scope.

mod caldav;
pub use caldav::CalendarEventBackup;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use base64::Engine as _;
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

const CALENDAR_SCOPE: &str = "https://www.googleapis.com/auth/calendar";
/// Busy time for calendars the Firm may read. Event details stay out of this scope.
pub const CALENDAR_FREEBUSY_SCOPE: &str = "https://www.googleapis.com/auth/calendar.freebusy";
/// Event details for the signed-in attorney's own calendar.
pub const CALENDAR_EVENTS_SCOPE: &str = "https://www.googleapis.com/auth/calendar.readonly";
const CALENDAR_BASE_URL: &str = "https://www.googleapis.com/calendar/v3";

/// Workspace mailbox that owns Guieb & Shook's Project Calendars.
pub const CALENDAR_OWNER_ENV: &str = "NAVIGATOR_CALENDAR_NEON_LAW_OWNER";
/// Calendar uses its own Workspace subject and the deployment's keyless signing account.
pub const CALENDAR_CONFIGURATION_KEYS: [&str; 2] = [
    CALENDAR_OWNER_ENV,
    crate::workspace::NAVIGATOR_DRIVE_NEON_LAW_SERVICE_ACCOUNT,
];

/// A Project's calendar, by its Calendar API id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectCalendar {
    pub id: String,
    /// The Project code the calendar is named for.
    pub summary: String,
}

/// A calendar access role. Reconciliation preserves existing owner grants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd)]
pub enum CalendarRole {
    FreeBusyReader,
    Reader,
    Writer,
    Owner,
}

impl CalendarRole {
    const fn api_value(self) -> &'static str {
        match self {
            Self::FreeBusyReader => "freeBusyReader",
            Self::Reader => "reader",
            Self::Writer => "writer",
            Self::Owner => "owner",
        }
    }

    fn from_api(value: &str) -> Option<Self> {
        match value {
            "freeBusyReader" => Some(Self::FreeBusyReader),
            "reader" => Some(Self::Reader),
            "writer" | "writerWithoutPrivateAccess" => Some(Self::Writer),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventTiming {
    Instant,
    Hearing(u32),
    AllDay(chrono::NaiveDate),
    /// A private hold on an attorney's booking calendar.
    BusyUntil(DateTime<FixedOffset>),
}

/// One event on a Project calendar, as Navigator writes it.
///
/// Civil dates display as Free all-day events. A hearing holds Busy time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarEvent {
    pub summary: String,
    pub description: Option<String>,
    pub location: Option<String>,
    /// The Calendar API color id an event renders in, from Navigator's fixed
    /// category map. `None` leaves the calendar's default color.
    pub color_id: Option<String>,
    /// The instant the event marks, with its own offset.
    pub start: DateTime<FixedOffset>,
    pub timing: EventTiming,
    /// RFC 5545 `SEQUENCE`, raised on every update.
    pub sequence: i32,
    /// A cancelled event keeps its place on the calendar with
    /// `status: cancelled`, the way the ICS feed renders a cancelled row.
    pub cancelled: bool,
    /// Display reminder minutes before `start`. An empty list writes no reminder and
    /// suppresses the calendar's defaults, so an event without a reminder
    /// stays silent.
    pub reminder_minutes: Vec<i64>,
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
    /// Present only when the event still matches Navigator’s last confirmed write.
    writable_etag: Option<String>,
    pub private_properties: BTreeMap<String, String>,
}

impl StoredEvent {
    /// A source update or cancellation can be applied at this exact version.
    #[must_use]
    pub fn can_write(&self) -> bool {
        self.writable_etag.is_some()
    }

    /// Legacy events can still be left in place when their source is unchanged.
    #[must_use]
    pub fn has_baseline(&self) -> bool {
        self.private_properties.contains_key(BASELINE)
    }
}

/// An event returned by an authorized Project Calendar read.
#[derive(Debug, Deserialize, Serialize)]
pub struct ProjectCalendarEvent {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transparency: Option<String>,
    pub summary: Option<String>,
    pub start: ProjectCalendarEventTime,
    pub end: ProjectCalendarEventTime,
}

/// Google Calendar uses a civil date for all-day events and RFC 3339 for timed events.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCalendarEventTime {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_time: Option<String>,
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
    #[error("CalDAV returned an incomplete event backup")]
    InvalidEventBackup,
    #[error("Calendar API response while creating a calendar did not include an id")]
    MissingCalendarId,
    #[error("could not verify the managed Project Calendar metadata")]
    UnverifiedCalendarMetadata,
    #[error("the participant's calendar list did not confirm the recorded Project Calendar ID")]
    UnverifiedCalendarListEntry,
    #[error("Calendar API response while writing an event did not include an id")]
    MissingEventId,
    #[error("calendar event was edited directly or has no verified mirror baseline")]
    UnverifiedEvent,
    #[error("Calendar found {count} calendars for one project code; refusing to guess")]
    AmbiguousSummary { count: usize },
}

/// Create, find, and share a Project calendar.
#[async_trait]
pub trait CalendarService: Send + Sync {
    /// The delegated user's own calendar named `summary`, if exactly one.
    async fn find_calendar(&self, summary: &str) -> Result<Option<ProjectCalendar>, CalendarError>;
    async fn create_calendar(
        &self,
        summary: &str,
        timezone: &str,
    ) -> Result<ProjectCalendar, CalendarError>;
    /// Apply the Project zone to its recorded, deployment-managed calendar.
    async fn reconcile_calendar_timezone(
        &self,
        code: &str,
        id: &str,
        timezone: &str,
    ) -> Result<(), CalendarError>;
    /// Whether a calendar with this id still exists. Checked by id, never by
    /// name, so a renamed calendar is still the recorded one.
    async fn calendar_exists(&self, calendar_id: &str) -> Result<bool, CalendarError>;
    /// Verify a recorded calendar against the Project code and its owning account.
    async fn recorded_calendar_matches(&self, code: &str, id: &str) -> Result<bool, CalendarError> {
        Ok(self.recorded_calendar_problem(code, id).await?.is_none())
    }

    /// Read-only explanation when the recorded calendar cannot be verified.
    async fn recorded_calendar_problem(
        &self,
        code: &str,
        id: &str,
    ) -> Result<Option<&'static str>, CalendarError> {
        let calendar = self.find_calendar(code).await?;
        Ok(calendar
            .is_none_or(|calendar| calendar.id != id)
            .then_some("The provider could not verify the recorded calendar."))
    }

    /// Version of the calendar resource itself, or absence after a completed deletion.
    async fn calendar_etag(&self, calendar_id: &str) -> Result<Option<String>, CalendarError>;
    /// Remove a closed Project's secondary calendar at the verified resource version.
    async fn delete_calendar(&self, calendar_id: &str, etag: &str) -> Result<(), CalendarError>;

    /// Require the selected owner to be the only identity able to change events at retirement.
    async fn verify_retirement_acl(&self, id: &str, owner: &str) -> Result<(), CalendarError>;

    /// Owned secondary calendar IDs and Project codes that can be checked for availability.
    async fn owned_calendar_codes(&self) -> Result<BTreeMap<String, String>, CalendarError>;

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

    /// Insert a reader grant without a sharing email. A conflict
    /// is read-only and means a grant already exists.
    async fn grant_reader(&self, calendar_id: &str, email: &str) -> Result<bool, CalendarError>;

    /// Remove a direct reader grant without modifying elevated grants.
    async fn revoke_reader(&self, calendar_id: &str, email: &str) -> Result<(), CalendarError>;

    /// Reject calendars with public or domain access before automated sharing.
    async fn verify_private_acl(&self, calendar_id: &str) -> Result<(), CalendarError>;

    /// Verify that the Firm's delegation can act as this Workspace member.
    async fn verify_workspace_member(&self, email: &str) -> Result<(), CalendarError>;

    /// Verify event access as the participant and add a missing calendar to their list.
    async fn ensure_reader_calendar(
        &self,
        calendar_id: &str,
        email: &str,
    ) -> Result<bool, CalendarError>;

    /// Correct owned event titles, remove internal guests from upcoming open-Project events, and revoke expired grants.
    async fn reconcile_project_events(
        &self,
        calendar_id: &str,
        open_project_code: Option<&str>,
        members: &BTreeSet<&str>,
        internal_domains: &BTreeSet<&str>,
    ) -> Result<(), CalendarError>;

    /// Read the complete iCalendar resources on a verified private calendar.
    async fn event_backups(
        &self,
        calendar_id: &str,
    ) -> Result<Vec<CalendarEventBackup>, CalendarError>;

    /// Read a Project Calendar only when its recorded id agrees with the
    /// calendar the deployment owns under the Project code.
    async fn project_events(
        &self,
        project_code: &str,
        recorded_id: &str,
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
    ) -> Result<Option<Vec<ProjectCalendarEvent>>, CalendarError> {
        if !self
            .recorded_calendar_matches(project_code, recorded_id)
            .await?
        {
            return Ok(None);
        }
        self.events_in_window(recorded_id, start, end)
            .await
            .map(Some)
    }

    /// List the events overlapping the bounded half-open window.
    async fn events_in_window(
        &self,
        calendar_id: &str,
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
    ) -> Result<Vec<ProjectCalendarEvent>, CalendarError>;

    /// Insert the event and read back its stored identity.
    async fn insert_event(
        &self,
        calendar_id: &str,
        event: &CalendarEvent,
    ) -> Result<StoredEvent, CalendarError>;

    /// Update a verified mirror at its last observed version.
    async fn patch_event(
        &self,
        calendar_id: &str,
        previous: &StoredEvent,
        event: &CalendarEvent,
    ) -> Result<StoredEvent, CalendarError>;

    /// Delete an event. Deleting one already gone succeeds: the desired end
    /// state is what the mirror reconciles to.
    async fn delete_event(
        &self,
        calendar_id: &str,
        previous: &StoredEvent,
    ) -> Result<(), CalendarError>;

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
    namespace: Option<&'static str>,
    delegated_user: String,
}

impl GoogleCalendar {
    /// Build a client that acts as `delegation.delegated_user`.
    ///
    /// # Errors
    /// [`CalendarError::Authentication`] when no runtime credential exists.
    pub async fn new(
        delegation: &crate::delegation::WorkspaceDelegation,
    ) -> Result<Self, CalendarError> {
        let token_source = delegation
            .token_source(&[CALENDAR_SCOPE])
            .await
            .map_err(|_| CalendarError::Authentication)?;
        Ok(Self::from_parts(
            token_source,
            CALENDAR_BASE_URL,
            &delegation.delegated_user,
        ))
    }

    /// Build from [`CALENDAR_CONFIGURATION_KEYS`], naming every missing key.
    ///
    /// # Errors
    /// [`CalendarError::MissingConfig`] or an authentication failure.
    pub async fn from_env() -> Result<Self, CalendarError> {
        Self::from_lookup(|key| std::env::var(key).ok()).await
    }

    /// Use the Firm's mailbox with the deployment's keyless signing account and naming rules.
    pub async fn for_owner(owner: &str) -> Result<Self, CalendarError> {
        Self::from_lookup(|key| {
            if key == CALENDAR_OWNER_ENV {
                Some(owner.to_owned())
            } else {
                std::env::var(key).ok()
            }
        })
        .await
    }

    /// Verify an explicitly registered private secondary calendar under the attorney's account.
    pub async fn verify_booking_calendar(&self, id: &str) -> Result<(), CalendarError> {
        if !id.ends_with("@group.calendar.google.com") {
            return Err(CalendarError::UnverifiedCalendarMetadata);
        }
        let action = "verifying the attorney's booking calendar";
        let marker = format!(
            "Navigator Booking Availability ({})",
            self.namespace.unwrap_or("production")
        );
        let body = Self::checked(
            self.http
                .get(self.url(&["calendars", id]))
                .bearer_auth(self.token().await?)
                .query(&[("fields", "id,dataOwner,description")])
                .send()
                .await,
            action,
        )?
        .json::<CalendarResource>()
        .await
        .map_err(|source| CalendarError::Response { action, source })?;
        if body.id.as_deref() != Some(id)
            || body.description.as_deref() != Some(marker.as_str())
            || !body
                .data_owner
                .as_deref()
                .is_some_and(|owner| owner.eq_ignore_ascii_case(&self.delegated_user))
        {
            return Err(CalendarError::UnverifiedCalendarMetadata);
        }
        self.verify_private_acl(id).await
    }

    async fn from_lookup<F>(get: F) -> Result<Self, CalendarError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let [owner_key, account_key] = CALENDAR_CONFIGURATION_KEYS;
        let value = |key| get(key).filter(|value| !value.trim().is_empty());
        let owner = value(owner_key);
        let account = value(account_key);
        let (delegated_user, service_account) = match (owner, account) {
            (Some(owner), Some(account)) => (owner, account),
            (owner, account) => {
                let missing = [
                    (owner_key, owner.is_none()),
                    (account_key, account.is_none()),
                ]
                .into_iter()
                .filter_map(|(key, missing)| missing.then_some(key))
                .collect::<Vec<_>>()
                .join(" and ");
                return Err(CalendarError::MissingConfig(missing));
            }
        };
        let mut calendar = Self::new(&crate::delegation::WorkspaceDelegation {
            service_account,
            delegated_user,
        })
        .await?;
        calendar.namespace = Some(
            if get("NAVIGATOR_SIMULATED_MATTERS").as_deref() == Some("true") {
                "staging"
            } else {
                "production"
            },
        );
        Ok(calendar)
    }

    async fn is_listed(&self, id: &str, token: &str) -> Result<bool, CalendarError> {
        let action = "checking the participant's calendar list";
        let response = self
            .http
            .get(self.url(&["users", "me", "calendarList", id]))
            .bearer_auth(token)
            .query(&[("fields", "id")])
            .send()
            .await
            .map_err(|source| CalendarError::Request { action, source })?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }
        let entry = Self::checked(Ok(response), action)?
            .json::<serde_json::Value>()
            .await
            .map_err(|source| CalendarError::Response { action, source })?;
        if entry["id"].as_str() != Some(id) {
            return Err(CalendarError::UnverifiedCalendarListEntry);
        }
        Ok(true)
    }

    async fn ensure_listed(&self, id: &str) -> Result<(), CalendarError> {
        let token = self.token().await?;
        if self.is_listed(id, &token).await? {
            return Ok(());
        }
        let action = "adding the Project Calendar to the participant's list";
        let response = self
            .http
            .post(self.url(&["users", "me", "calendarList"]))
            .bearer_auth(&token)
            .json(&serde_json::json!({"id": id, "selected": true}))
            .send()
            .await
            .map_err(|source| CalendarError::Request { action, source })?;
        if response.status() != reqwest::StatusCode::CONFLICT {
            Self::checked(Ok(response), action)?;
        }
        if !self.is_listed(id, &token).await? {
            return Err(CalendarError::UnverifiedCalendarListEntry);
        }
        Ok(())
    }

    async fn owned_entries(
        &self,
        include_hidden: bool,
    ) -> Result<Vec<CalendarListEntry>, CalendarError> {
        let action = "listing calendars";
        let mut found = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let token = self.token().await?;
            let mut query = vec![("minAccessRole", "owner".to_string())];
            if include_hidden {
                query.push(("showHidden", "true".into()));
            }
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
            found.extend(page.items);
            page_token = page.next_page_token;
            if page_token.is_none() {
                break;
            }
        }
        Ok(found)
    }

    fn other_namespace(&self, entry: &CalendarListEntry) -> bool {
        entry.description.as_deref().is_some_and(|description| {
            description.starts_with("Navigator Project Calendar (")
                && Some(description) != self.ownership_marker().as_deref()
        })
    }

    fn project_summary<'a>(&self, code: &'a str) -> std::borrow::Cow<'a, str> {
        if self.namespace == Some("staging") {
            format!("staging-{code}").into()
        } else {
            code.into()
        }
    }

    fn ownership_marker(&self) -> Option<String> {
        self.namespace
            .map(|environment| format!("Navigator Project Calendar ({environment})"))
    }

    fn from_parts(
        token_source: Arc<dyn google_cloud_token::TokenSource>,
        base_url: &str,
        delegated_user: &str,
    ) -> Self {
        Self {
            token_source,
            http: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            namespace: None,
            delegated_user: delegated_user.to_owned(),
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

    /// Stamp the exact representation returned by our write. A concurrent edit
    /// invalidates this precondition and can never become the mirror baseline.
    async fn baseline_event(
        &self,
        calendar_id: &str,
        resource: serde_json::Value,
    ) -> Result<StoredEvent, CalendarError> {
        let action = "recording the event mirror baseline";
        let id = resource["id"]
            .as_str()
            .ok_or(CalendarError::MissingEventId)?;
        let etag = event_etag(&resource).ok_or(CalendarError::UnverifiedEvent)?;
        if resource["summary"].as_str().is_none()
            || resource["start"].is_null()
            || resource["end"].is_null()
            || contains_meeting_material(&resource)
        {
            return Err(CalendarError::UnverifiedEvent);
        }
        let sha = event_fingerprint(&resource);
        let patch = serde_json::json!({"extendedProperties":{"private":{(BASELINE):sha}}});
        let confirmed = Self::checked(
            self.http
                .patch(self.url(&["calendars", calendar_id, "events", id]))
                .bearer_auth(self.token().await?)
                .header(reqwest::header::IF_MATCH, etag)
                .query(&[
                    ("sendUpdates", "none"),
                    ("supportsAttachments", "true"),
                    ("conferenceDataVersion", "1"),
                ])
                .json(&patch)
                .send()
                .await,
            action,
        )?
        .json::<serde_json::Value>()
        .await
        .map_err(|source| CalendarError::Response { action, source })?;
        if confirmed["id"] != resource["id"]
            || confirmed["extendedProperties"]["private"][BASELINE] != sha
        {
            return Err(CalendarError::UnverifiedEvent);
        }
        // The baseline stamp changes the ETag. Verify Google's complete response too.
        let stored = stored_event(&confirmed)?;
        stored
            .writable_etag
            .as_ref()
            .ok_or(CalendarError::UnverifiedEvent)?;
        Ok(stored)
    }

    async fn list_events<T: serde::de::DeserializeOwned>(
        &self,
        calendar_id: &str,
        action: &'static str,
        options: &[(&str, &str)],
    ) -> Result<Vec<T>, CalendarError> {
        let mut events = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let token = self.token().await?;
            let mut query = options.to_vec();
            if let Some(page) = &page_token {
                query.push(("pageToken", page));
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
            .json::<EventPage<T>>()
            .await
            .map_err(|source| CalendarError::Response { action, source })?;
            events.extend(page.items);
            page_token = page.next_page_token;
            if page_token.is_none() {
                return Ok(events);
            }
        }
    }

    async fn acl_rules(&self, calendar_id: &str) -> Result<Vec<AclRule>, CalendarError> {
        let action = "listing calendar sharing";
        let mut rules = Vec::new();
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
            rules.extend(page.items);
            page_token = page.next_page_token;
            if page_token.is_none() {
                return Ok(rules);
            }
        }
    }
    async fn user_acl_rules(&self, calendar_id: &str) -> Result<Vec<AclRule>, CalendarError> {
        Ok(self
            .acl_rules(calendar_id)
            .await?
            .into_iter()
            .filter(|rule| rule.scope.kind == "user")
            .collect())
    }
}

#[async_trait]
impl CalendarService for GoogleCalendar {
    async fn reconcile_project_events(
        &self,
        calendar_id: &str,
        open_project_code: Option<&str>,
        members: &BTreeSet<&str>,
        internal_domains: &BTreeSet<&str>,
    ) -> Result<(), CalendarError> {
        let events = self
            .list_events::<MembershipEvent>(
                calendar_id,
                "reading Project Calendar events",
                &[(
                    "fields",
                    "nextPageToken,items(id,etag,summary,organizer(email,self),status,locked,attendees,attendeesOmitted,extendedProperties/private)",
                )],
            )
            .await?;
        let upcoming: BTreeSet<_> = if open_project_code.is_some()
            && !internal_domains.is_empty()
            && events.iter().any(|event| !event.attendees.is_empty())
        {
            let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            self.list_events::<ProviderEventId>(
                calendar_id,
                "reading upcoming Project Calendar events",
                &[("fields", "nextPageToken,items(id)"), ("timeMin", &now)],
            )
            .await?
            .into_iter()
            .map(|event| event.id)
            .collect()
        } else {
            BTreeSet::new()
        };
        for event in events {
            let guest_domains = upcoming.contains(&event.id).then_some(internal_domains);
            let Some(patch) = event.membership_patch(open_project_code, members, guest_domains)?
            else {
                continue;
            };
            Self::checked(
                self.http
                    .patch(self.url(&["calendars", calendar_id, "events", &event.id]))
                    .bearer_auth(self.token().await?)
                    .header(reqwest::header::IF_MATCH, event.etag)
                    .query(&[
                        ("sendUpdates", "none"),
                        ("supportsAttachments", "true"),
                        ("conferenceDataVersion", "1"),
                    ])
                    .json(&patch)
                    .send()
                    .await,
                "updating a Project Calendar event",
            )?;
        }
        Ok(())
    }

    async fn event_backups(
        &self,
        calendar_id: &str,
    ) -> Result<Vec<CalendarEventBackup>, CalendarError> {
        self.caldav_events(calendar_id).await
    }

    async fn events_in_window(
        &self,
        calendar_id: &str,
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
    ) -> Result<Vec<ProjectCalendarEvent>, CalendarError> {
        let start = start.to_rfc3339();
        let end = end.to_rfc3339();
        self.list_events(
            calendar_id,
            "listing project calendar events",
            &[
                ("timeMin", &start),
                ("timeMax", &end),
                ("singleEvents", "true"),
                ("orderBy", "startTime"),
                (
                    "fields",
                    "nextPageToken,items(id,summary,start,end,transparency)",
                ),
            ],
        )
        .await
    }
    async fn find_calendar(&self, summary: &str) -> Result<Option<ProjectCalendar>, CalendarError> {
        let summary = self.project_summary(summary);
        let marker = self.ownership_marker();
        let found: Vec<_> = self
            .owned_entries(false)
            .await?
            .into_iter()
            .filter(|entry| {
                entry.summary.as_deref() == Some(summary.as_ref())
                    && marker
                        .as_ref()
                        .is_none_or(|expected| entry.description.as_ref() == Some(expected))
            })
            .map(|entry| ProjectCalendar {
                id: entry.id,
                summary: summary.to_string(),
            })
            .collect();
        match found.len() {
            0 => Ok(None),
            1 => Ok(found.into_iter().next()),
            count => Err(CalendarError::AmbiguousSummary { count }),
        }
    }

    async fn create_calendar(
        &self,
        summary: &str,
        timezone: &str,
    ) -> Result<ProjectCalendar, CalendarError> {
        let summary = self.project_summary(summary);
        let mut body = serde_json::json!({ "summary": summary.as_ref(), "timeZone": timezone });
        if let Some(marker) = self.ownership_marker() {
            body["description"] = marker.into();
        }
        let action = "creating a calendar";
        let token = self.token().await?;
        let created = Self::checked(
            self.http
                .post(self.url(&["calendars"]))
                .bearer_auth(token)
                .json(&body)
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
            project_code = summary.as_ref(),
            outcome = "created",
            "Project calendar provisioning completed"
        );
        Ok(ProjectCalendar {
            id,
            summary: summary.to_string(),
        })
    }

    async fn reconcile_calendar_timezone(
        &self,
        code: &str,
        id: &str,
        timezone: &str,
    ) -> Result<(), CalendarError> {
        let Some(marker) = self.ownership_marker() else {
            return Ok(());
        };
        let action = "reading the managed Project Calendar time zone";
        let token = self.token().await?;
        let url = self.url(&["calendars", id]);
        let resource = Self::checked(
            self.http
                .get(&url)
                .bearer_auth(&token)
                .query(&[("fields", "id,etag,summary,description,dataOwner,timeZone")])
                .send()
                .await,
            action,
        )?
        .json::<CalendarResource>()
        .await
        .map_err(|source| CalendarError::Response { action, source })?;
        if resource.id.as_deref() != Some(id)
            || resource.summary.as_deref() != Some(self.project_summary(code).as_ref())
        {
            return Err(CalendarError::UnverifiedCalendarMetadata);
        }
        // A hand-built calendar can be explicitly linked and remains user-managed.
        if resource.description.as_deref() != Some(marker.as_str()) {
            return Ok(());
        }
        if !resource
            .data_owner
            .as_deref()
            .is_some_and(|owner| owner.eq_ignore_ascii_case(&self.delegated_user))
        {
            return Err(CalendarError::UnverifiedCalendarMetadata);
        }
        if resource.time_zone.as_deref() == Some(timezone) {
            return Ok(());
        }
        let version = resource
            .etag
            .filter(|etag| !etag.is_empty())
            .ok_or(CalendarError::UnverifiedCalendarMetadata)?;
        Self::checked(
            self.http
                .patch(url)
                .bearer_auth(token)
                .header(reqwest::header::IF_MATCH, version)
                .json(&serde_json::json!({"timeZone":timezone}))
                .send()
                .await,
            "setting the managed Project Calendar time zone",
        )?;
        Ok(())
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
            status if (200..300).contains(&status) => {
                let Some(marker) = self.ownership_marker() else {
                    return Ok(true);
                };
                let calendar: CalendarListEntry = response
                    .json()
                    .await
                    .map_err(|source| CalendarError::Response { action, source })?;
                Ok(calendar.description.as_deref() == Some(marker.as_str()))
            }
            404 | 410 => Ok(false),
            status => Err(CalendarError::Api { action, status }),
        }
    }

    async fn recorded_calendar_problem(
        &self,
        code: &str,
        id: &str,
    ) -> Result<Option<&'static str>, CalendarError> {
        let action = "verifying the recorded Project Calendar owner and name";
        let response = self
            .http
            .get(self.url(&["users", "me", "calendarList", id]))
            .bearer_auth(self.token().await?)
            .send()
            .await
            .map_err(|source| CalendarError::Request { action, source })?;
        if matches!(response.status().as_u16(), 404 | 410) {
            let resource = self
                .http
                .get(self.url(&["calendars", id]))
                .bearer_auth(self.token().await?)
                .send()
                .await
                .map_err(|source| CalendarError::Request { action, source })?;
            return Ok(Some(match resource.status().as_u16() {
                200..=299 => "The calendar exists but is absent from the delegated owner's calendar list.",
                403 => "The delegated account cannot access the recorded calendar.",
                410 => "The provider reports that the recorded calendar was deleted.",
                404 => "The calendar is missing or hidden from the delegated account. Confirm with its owner.",
                status => return Err(CalendarError::Api { action, status }),
            }));
        }
        let entry = Self::checked(Ok(response), action)?
            .json::<CalendarListEntry>()
            .await
            .map_err(|source| CalendarError::Response { action, source })?;
        // A manually linked calendar can lack a Navigator marker. An explicit
        // marker for another deployment can never be adopted.
        Ok(if entry.id != id {
            Some("The provider returned a different calendar ID.")
        } else if entry.primary {
            Some("A primary calendar cannot be a Project Calendar.")
        } else if self.other_namespace(&entry) {
            Some("The recorded calendar belongs to another deployment.")
        } else if entry.access_role.as_deref() != Some("owner") {
            Some("The delegated account does not own the recorded calendar.")
        } else if entry.summary.as_deref() != Some(self.project_summary(code).as_ref()) {
            Some("The recorded calendar name differs from the expected Project calendar name.")
        } else {
            None
        })
    }

    async fn calendar_etag(&self, id: &str) -> Result<Option<String>, CalendarError> {
        let action = "reading the calendar version";
        let response = self
            .http
            .get(self.url(&["calendars", id]))
            .bearer_auth(self.token().await?)
            .query(&[("fields", "id,etag")])
            .send()
            .await
            .map_err(|source| CalendarError::Request { action, source })?;
        if matches!(response.status().as_u16(), 404 | 410) {
            return Ok(None);
        }
        let resource = Self::checked(Ok(response), action)?
            .json::<CalendarResource>()
            .await
            .map_err(|source| CalendarError::Response { action, source })?;
        if resource.id.as_deref() != Some(id) {
            return Err(CalendarError::InvalidEventBackup);
        }
        resource
            .etag
            .filter(|etag| !etag.is_empty())
            .map(Some)
            .ok_or(CalendarError::InvalidEventBackup)
    }

    async fn delete_calendar(&self, id: &str, etag: &str) -> Result<(), CalendarError> {
        let action = "deleting the archived secondary calendar";
        Self::checked(
            self.http
                .delete(self.url(&["calendars", id]))
                .bearer_auth(self.token().await?)
                .header(reqwest::header::IF_MATCH, etag)
                .send()
                .await,
            action,
        )?;
        Ok(())
    }

    async fn verify_retirement_acl(&self, id: &str, owner: &str) -> Result<(), CalendarError> {
        let mut owner_found = false;
        for rule in self.acl_rules(id).await? {
            if rule.role == "owner"
                && rule.scope.kind == "user"
                && rule
                    .scope
                    .value
                    .as_deref()
                    .is_some_and(|value| value.eq_ignore_ascii_case(owner))
            {
                owner_found = true;
            } else if !matches!(rule.role.as_str(), "none" | "freeBusyReader" | "reader") {
                return Err(CalendarError::Api {
                    action: "verifying the closed calendar has a single writer",
                    status: 409,
                });
            }
        }
        if !owner_found {
            return Err(CalendarError::Api {
                action: "verifying the closed calendar owner",
                status: 403,
            });
        }
        Ok(())
    }

    async fn owned_calendar_codes(&self) -> Result<BTreeMap<String, String>, CalendarError> {
        let mut codes = BTreeMap::new();
        for entry in self.owned_entries(true).await? {
            if entry.primary
                || self.other_namespace(&entry)
                || entry.access_role.as_deref() != Some("owner")
            {
                continue;
            }
            if let Some(summary) = entry.summary {
                let code = if self.namespace == Some("staging") {
                    let Some(code) = summary.strip_prefix("staging-") else {
                        continue;
                    };
                    code.to_owned()
                } else {
                    summary
                };
                codes.insert(entry.id, code);
            }
        }
        Ok(codes)
    }

    async fn members(&self, calendar_id: &str) -> Result<Vec<CalendarMember>, CalendarError> {
        Ok(self
            .user_acl_rules(calendar_id)
            .await?
            .into_iter()
            .filter_map(|rule| {
                Some(CalendarMember {
                    email: rule.scope.value?,
                    role: CalendarRole::from_api(&rule.role)?,
                })
            })
            .collect())
    }

    async fn grant_reader(&self, calendar_id: &str, email: &str) -> Result<bool, CalendarError> {
        let token = self.token().await?;
        let action = "granting calendar access";
        let response = self
            .http
            .post(self.url(&["calendars", calendar_id, "acl"]))
            .bearer_auth(token)
            .query(&[("sendNotifications", "false")])
            .json(&serde_json::json!({
                "role": "reader",
                "scope": { "type": "user", "value": email },
            }))
            .send()
            .await
            .map_err(|source| CalendarError::Request { action, source })?;
        if response.status().as_u16() == 409 {
            return Ok(false);
        }
        Self::checked(Ok(response), action)?;
        Ok(true)
    }

    async fn revoke_reader(&self, calendar_id: &str, email: &str) -> Result<(), CalendarError> {
        let action = "revoking a calendar reader";
        let Some(rule) = self
            .user_acl_rules(calendar_id)
            .await?
            .into_iter()
            .find(|rule| {
                rule.role == "reader"
                    && rule
                        .scope
                        .value
                        .as_deref()
                        .is_some_and(|value| value.eq_ignore_ascii_case(email))
            })
        else {
            return Ok(());
        };
        let id = rule.id.ok_or(CalendarError::Api {
            action,
            status: 409,
        })?;
        let mut request = self
            .http
            .delete(self.url(&["calendars", calendar_id, "acl", &id]))
            .bearer_auth(self.token().await?);
        if let Some(etag) = rule.etag {
            request = request.header(reqwest::header::IF_MATCH, etag);
        }
        let response = request
            .send()
            .await
            .map_err(|source| CalendarError::Request { action, source })?;
        if !matches!(response.status().as_u16(), 404 | 410) {
            Self::checked(Ok(response), action)?;
        }
        if self.members(calendar_id).await?.iter().any(|member| {
            member.role == CalendarRole::Reader && member.email.eq_ignore_ascii_case(email)
        }) {
            return Err(CalendarError::Api {
                action,
                status: 409,
            });
        }
        Ok(())
    }

    async fn verify_private_acl(&self, calendar_id: &str) -> Result<(), CalendarError> {
        if self.acl_rules(calendar_id).await?.iter().any(|rule| {
            matches!(rule.scope.kind.as_str(), "default" | "domain") && rule.role != "none"
        }) {
            return Err(CalendarError::Api {
                action: "Project Calendar has public or domain access",
                status: 403,
            });
        }
        Ok(())
    }

    async fn verify_workspace_member(&self, email: &str) -> Result<(), CalendarError> {
        Self::for_owner(email).await?.token().await.map(|_| ())
    }

    async fn ensure_reader_calendar(
        &self,
        calendar_id: &str,
        email: &str,
    ) -> Result<bool, CalendarError> {
        let participant = Self::for_owner(email).await?;
        let action = "reading Project Calendar as the assigned participant";
        let response = participant
            .http
            .get(participant.url(&["calendars", calendar_id, "events"]))
            .bearer_auth(participant.token().await?)
            .query(&[("maxResults", "1"), ("fields", "accessRole")])
            .send()
            .await
            .map_err(|source| CalendarError::Request { action, source })?;
        if matches!(response.status().as_u16(), 403 | 404) {
            return Ok(false);
        }
        let list = Self::checked(Ok(response), action)?
            .json::<serde_json::Value>()
            .await
            .map_err(|source| CalendarError::Response { action, source })?;
        let readable = matches!(
            CalendarRole::from_api(list["accessRole"].as_str().unwrap_or_default()),
            Some(CalendarRole::Reader | CalendarRole::Writer | CalendarRole::Owner)
        );
        if readable {
            participant.ensure_listed(calendar_id).await?;
        }
        Ok(readable)
    }

    async fn set_member_role(
        &self,
        calendar_id: &str,
        email: &str,
        role: CalendarRole,
    ) -> Result<(), CalendarError> {
        let token = self.token().await?;
        let action = "sharing a calendar";
        let response = self
            .http
            .post(self.url(&["calendars", calendar_id, "acl"]))
            .bearer_auth(&token)
            .query(&[("sendNotifications", "false")])
            .json(&serde_json::json!({
                "role": role.api_value(),
                "scope": { "type": "user", "value": email },
            }))
            .send()
            .await
            .map_err(|source| CalendarError::Request { action, source })?;
        if response.status() == reqwest::StatusCode::CONFLICT {
            let existing = self
                .user_acl_rules(calendar_id)
                .await?
                .into_iter()
                .find(|rule| {
                    rule.scope
                        .value
                        .as_deref()
                        .is_some_and(|value| value.eq_ignore_ascii_case(email))
                })
                .ok_or(CalendarError::Api {
                    action,
                    status: 409,
                })?;
            if existing.role == "owner" {
                return Ok(());
            }
            let rule_id = existing.id.ok_or(CalendarError::Api {
                action,
                status: 409,
            })?;
            Self::checked(
                self.http
                    .patch(self.url(&["calendars", calendar_id, "acl", &rule_id]))
                    .bearer_auth(token)
                    .query(&[("sendNotifications", "false")])
                    .json(&serde_json::json!({ "role": role.api_value() }))
                    .send()
                    .await,
                action,
            )?;
        } else {
            Self::checked(Ok(response), action)?;
        }
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
                .query(&[("sendUpdates", "none")])
                .json(&event_body(event))
                .send()
                .await,
            action,
        )?
        .json::<serde_json::Value>()
        .await
        .map_err(|source| CalendarError::Response { action, source })?;
        self.baseline_event(calendar_id, stored).await
    }

    async fn patch_event(
        &self,
        calendar_id: &str,
        previous: &StoredEvent,
        event: &CalendarEvent,
    ) -> Result<StoredEvent, CalendarError> {
        let action = "updating an event";
        let etag = previous
            .writable_etag
            .as_deref()
            .ok_or(CalendarError::UnverifiedEvent)?;
        let token = self.token().await?;
        let stored = Self::checked(
            self.http
                .patch(self.url(&["calendars", calendar_id, "events", &previous.id]))
                .header(reqwest::header::IF_MATCH, etag)
                .query(&[
                    ("sendUpdates", "none"),
                    ("supportsAttachments", "true"),
                    ("conferenceDataVersion", "1"),
                ])
                .bearer_auth(token)
                .json(&event_body(event))
                .send()
                .await,
            action,
        )?
        .json::<serde_json::Value>()
        .await
        .map_err(|source| CalendarError::Response { action, source })?;
        if stored["id"].as_str() != Some(&previous.id) {
            return Err(CalendarError::UnverifiedEvent);
        }
        self.baseline_event(calendar_id, stored).await
    }

    async fn delete_event(
        &self,
        calendar_id: &str,
        previous: &StoredEvent,
    ) -> Result<(), CalendarError> {
        let action = "deleting an event";
        let etag = previous
            .writable_etag
            .as_deref()
            .ok_or(CalendarError::UnverifiedEvent)?;
        let token = self.token().await?;
        let response = self
            .http
            .delete(self.url(&["calendars", calendar_id, "events", &previous.id]))
            .header(reqwest::header::IF_MATCH, etag)
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
        let filter = format!("{key}={value}");
        self.list_events::<serde_json::Value>(
            calendar_id,
            "listing mirrored events",
            &[("privateExtendedProperty", &filter), ("maxResults", "250")],
        )
        .await?
        .iter()
        .map(stored_event)
        .collect()
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
#[serde(rename_all = "camelCase")]
struct CalendarListEntry {
    #[serde(default)]
    primary: bool,
    id: String,
    access_role: Option<String>,
    summary: Option<String>,
    description: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CalendarResource {
    summary: Option<String>,
    description: Option<String>,
    data_owner: Option<String>,
    time_zone: Option<String>,
    id: Option<String>,
    etag: Option<String>,
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
    etag: Option<String>,
    id: Option<String>,
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
#[serde(rename_all = "camelCase", bound(deserialize = "T: Deserialize<'de>"))]
struct EventPage<T> {
    #[serde(default)]
    items: Vec<T>,
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct EventOrganizer {
    #[serde(rename = "self", default)]
    owns_event: bool,
    email: Option<String>,
}

#[derive(Deserialize)]
struct ProviderEventId {
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MembershipEvent {
    id: String,
    etag: String,
    summary: Option<String>,
    organizer: Option<EventOrganizer>,
    status: Option<String>,
    #[serde(default)]
    locked: bool,
    #[serde(default)]
    attendees: Vec<serde_json::Value>,
    #[serde(default)]
    attendees_omitted: bool,
    #[serde(default)]
    extended_properties: EventExtendedProperties,
}

impl MembershipEvent {
    fn membership_patch(
        &self,
        code: Option<&str>,
        members: &BTreeSet<&str>,
        internal_domains: Option<&BTreeSet<&str>>,
    ) -> Result<Option<serde_json::Value>, CalendarError> {
        let Some(organizer) = self
            .organizer
            .as_ref()
            .filter(|organizer| organizer.owns_event)
        else {
            return Ok(None);
        };
        if self.status.as_deref() == Some("cancelled") {
            return Ok(None);
        }
        let mut patch = serde_json::Map::new();
        if let Some(code) = code.filter(|_| !self.locked) {
            let title = model::projects::calendar_event_title(
                code,
                self.summary.as_deref().unwrap_or_default(),
            );
            if self.summary.as_deref() != Some(&title) {
                patch.insert("summary".into(), title.into());
            }
        }
        let attendees_writable = !self.attendees_omitted && !self.locked;
        let person_address = |guest: &serde_json::Value| {
            if guest["organizer"].as_bool() == Some(true)
                || guest["resource"].as_bool() == Some(true)
            {
                return None;
            }
            let email = guest["email"].as_str()?.trim().to_ascii_lowercase();
            (!organizer
                .email
                .as_ref()
                .is_some_and(|owner| owner.eq_ignore_ascii_case(&email)))
            .then_some(email)
        };
        let mut removed = BTreeSet::new();
        if let Some(domains) = internal_domains.filter(|_| attendees_writable) {
            removed.extend(
                self.attendees
                    .iter()
                    .filter_map(person_address)
                    .filter(|email| {
                        matches!(
                            email.rsplit_once('@').and_then(|(_, domain)| url::Host::parse(domain).ok()),
                            Some(url::Host::Domain(domain)) if domains.contains(domain.as_str())
                        )
                    }),
            );
        }

        // A marked guest can retain event access after losing the calendar grant.
        let mut markers = serde_json::Map::new();
        for (key, email) in &self.extended_properties.private {
            let address = email.trim().to_ascii_lowercase();
            if key.starts_with("nv_invited_")
                && (!members.contains(address.as_str()) || removed.contains(&address))
            {
                removed.insert(address);
                markers.insert(key.clone(), serde_json::Value::Null);
            }
        }
        if !removed.is_empty() {
            if !attendees_writable {
                return Err(CalendarError::Api {
                    action: "revoking expired Project event access",
                    status: 409,
                });
            }
            let guests: Vec<_> = self
                .attendees
                .iter()
                .filter(|guest| {
                    !person_address(guest).is_some_and(|email| removed.contains(&email))
                })
                .collect();
            if guests.len() != self.attendees.len() {
                patch.insert("attendees".into(), serde_json::json!(guests));
            }
        }
        if !markers.is_empty() {
            patch.insert(
                "extendedProperties".into(),
                serde_json::json!({"private":markers}),
            );
        }
        Ok((!patch.is_empty()).then_some(serde_json::Value::Object(patch)))
    }
}

/// The `extendedProperties` object; only the `private` map is ours.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct EventExtendedProperties {
    #[serde(default)]
    private: BTreeMap<String, String>,
}

/// The event body Navigator writes. `end` is one minute after `start`: a
/// zero-length event vanishes from most calendar views, so every Deadline
/// holds at least a one-minute block.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EventBody<'a> {
    summary: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    visibility: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    location: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    color_id: Option<&'a str>,
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
enum EventTime {
    DateTime(String),
    Date(chrono::NaiveDate),
}

impl From<EventTime> for ProjectCalendarEventTime {
    fn from(time: EventTime) -> Self {
        match time {
            EventTime::DateTime(value) => Self {
                date_time: Some(value),
                date: None,
            },
            EventTime::Date(value) => Self {
                date: Some(value.to_string()),
                date_time: None,
            },
        }
    }
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
    let (start, end) = match event.timing {
        EventTiming::AllDay(day) => (
            EventTime::Date(day),
            EventTime::Date(day.succ_opt().unwrap_or(day)),
        ),
        EventTiming::BusyUntil(end) => (
            EventTime::DateTime(event.start.to_rfc3339()),
            EventTime::DateTime(end.to_rfc3339()),
        ),
        timing => {
            let minutes = if let EventTiming::Hearing(minutes) = timing {
                i64::from(minutes)
            } else {
                1
            };
            (
                EventTime::DateTime(event.start.to_rfc3339()),
                EventTime::DateTime(
                    (event.start + chrono::Duration::minutes(minutes)).to_rfc3339(),
                ),
            )
        }
    };
    EventBody {
        summary: &event.summary,
        visibility: matches!(event.timing, EventTiming::BusyUntil(_)).then_some("private"),
        description: event.description.as_deref(),
        location: event.location.as_deref(),
        color_id: event.color_id.as_deref(),
        start,
        end,
        status: if event.cancelled {
            "cancelled"
        } else {
            "confirmed"
        },
        transparency: if matches!(
            event.timing,
            EventTiming::Hearing(_) | EventTiming::BusyUntil(_)
        ) {
            "opaque"
        } else {
            "transparent"
        },
        sequence: event.sequence,
        reminders: EventReminders {
            use_default: false,
            overrides: event
                .reminder_minutes
                .iter()
                .copied()
                // Google accepts popup offsets of up to four weeks. Longer internal leads remain on the Deadline.
                .filter(|minutes| (0..=40_320).contains(minutes))
                .map(|minutes| EventReminder {
                    method: "popup",
                    minutes,
                })
                .collect(),
        },
        extended_properties: EventExtendedProperties {
            private: event.private_properties.clone(),
        },
    }
}

const BASELINE: &str = "navigator_event_baseline_v1";
pub const BOOKING_MARKER: &str = "navigator_booking_availability";
pub const BOOKING_VERSION: &str = "v1";
pub const BOOKING_INTERVAL: &str = "navigator_booking_interval";

fn event_etag(resource: &serde_json::Value) -> Option<&str> {
    resource["etag"]
        .as_str()
        .map(str::trim)
        .filter(|etag| !etag.is_empty() && *etag != "*")
}

/// Hash Google's own complete response, including the event ID, meeting data and unknown
/// fields. Only the baseline stamp and Google's write bookkeeping may change on our stamp.
fn event_fingerprint(resource: &serde_json::Value) -> String {
    let mut content = resource.clone();
    if let Some(object) = content.as_object_mut() {
        for field in ["etag", "updated", "sequence"] {
            object.remove(field);
        }
    }
    if let Some(props) = content
        .pointer_mut("/extendedProperties/private")
        .and_then(serde_json::Value::as_object_mut)
    {
        props.remove(BASELINE);
    }
    for key in ["start", "end"] {
        if let Some(instant) = content[key]["dateTime"]
            .as_str()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        {
            // Google can render the same instant in the query's or calendar's time zone.
            content[key]["dateTime"] = instant.to_utc().to_rfc3339().into();
        }
    }
    content.sort_all_objects();
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(content.to_string().as_bytes()))
}

/// A provider can add conferencing or a recording even to a Navigator-created event.
fn contains_meeting_material(resource: &serde_json::Value) -> bool {
    ["attendees", "attachments"].iter().any(|field| {
        resource[*field]
            .as_array()
            .is_some_and(|entries| !entries.is_empty())
    }) || ["conferenceData", "hangoutLink"]
        .iter()
        .any(|field| !resource[*field].is_null())
        || resource["attendeesOmitted"].as_bool() == Some(true)
}

// A Google insert can succeed before its ETag baseline is stamped. Recover
// only the canonical guest-free hold that this reconcile owns.
fn unstamped_booking_hold(
    resource: &serde_json::Value,
    properties: &BTreeMap<String, String>,
) -> bool {
    if properties.get(BOOKING_MARKER).map(String::as_str) != Some(BOOKING_VERSION)
        || properties.len() != 2
        || resource["summary"] != "Busy"
        || resource["visibility"] != "private"
        || resource["status"] != "confirmed"
        || resource["transparency"]
            .as_str()
            .is_some_and(|value| value != "opaque")
        || resource["reminders"]["useDefault"] != false
        || resource["reminders"]["overrides"]
            .as_array()
            .is_some_and(|values| !values.is_empty())
        || [
            "description",
            "location",
            "colorId",
            "recurrence",
            "recurringEventId",
        ]
        .iter()
        .any(|key| !resource[*key].is_null())
    {
        return false;
    }
    let time = |field: &str| {
        resource[field]["dateTime"]
            .as_str()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
    };
    let (Some(start), Some(end)) = (time("start"), time("end")) else {
        return false;
    };
    start < end
        && properties.get(BOOKING_INTERVAL)
            == Some(&format!(
                "{}/{}",
                start.to_utc().to_rfc3339(),
                end.to_utc().to_rfc3339()
            ))
}

fn stored_event(resource: &serde_json::Value) -> Result<StoredEvent, CalendarError> {
    let id = resource["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or(CalendarError::MissingEventId)?;
    let private_properties: BTreeMap<String, String> = resource["extendedProperties"]["private"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(key, value)| value.as_str().map(|v| (key.clone(), v.to_string())))
        .collect();
    let writable_etag = event_etag(resource)
        .filter(|_| !contains_meeting_material(resource))
        .filter(|_| {
            private_properties.get(BASELINE).map_or_else(
                || unstamped_booking_hold(resource, &private_properties),
                |saved| saved == &event_fingerprint(resource),
            )
        })
        .map(str::to_owned);
    Ok(StoredEvent {
        id: id.to_string(),
        private_properties,
        writable_etag,
    })
}

fn fake_event_resource(id: &str, event: &CalendarEvent) -> serde_json::Value {
    let mut resource = serde_json::json!(event_body(event));
    resource["id"] = id.into();
    resource["etag"] = event_fingerprint(&resource).into();
    resource
}

fn fake_mirror_write(id: &str, mut event: CalendarEvent) -> (CalendarEvent, StoredEvent) {
    let sha = event_fingerprint(&fake_event_resource(id, &event));
    event.private_properties.insert(BASELINE.into(), sha);
    let stored = stored_event(&fake_event_resource(id, &event)).expect("fixture event has ID");
    (event, stored)
}

fn verify_fake_version(
    previous: &StoredEvent,
    existing: &CalendarEvent,
) -> Result<(), CalendarError> {
    let etag = previous
        .writable_etag
        .as_deref()
        .ok_or(CalendarError::UnverifiedEvent)?;
    if fake_event_resource(&previous.id, existing)["etag"] != etag {
        return Err(CalendarError::Api {
            action: "checking event version",
            status: 412,
        });
    }
    Ok(())
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
    timezone: String,
    backups: Vec<CalendarEventBackup>,
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

    /// Supply the `CalDAV` resources returned by a simulated calendar.
    pub fn set_event_backups(&self, id: &str, backups: Vec<CalendarEventBackup>) {
        self.state
            .lock()
            .expect("fake calendar mutex poisoned")
            .calendars
            .get_mut(id)
            .expect("fixture calendar exists")
            .backups = backups;
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

    /// The zone recorded on a simulated calendar.
    #[must_use]
    pub fn timezone(&self, calendar_id: &str) -> Option<String> {
        self.state
            .lock()
            .expect("fake calendar mutex poisoned")
            .calendars
            .get(calendar_id)
            .map(|entry| entry.timezone.clone())
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
                    .map(|(id, event)| {
                        stored_event(&fake_event_resource(id, event)).expect("fixture event has ID")
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
    async fn reconcile_project_events(
        &self,
        calendar_id: &str,
        open_project_code: Option<&str>,
        _: &BTreeSet<&str>,
        _: &BTreeSet<&str>,
    ) -> Result<(), CalendarError> {
        let Some(code) = open_project_code else {
            return Ok(());
        };
        let mut state = self.state.lock().expect("fake calendar mutex poisoned");
        if let Some(events) = state.events.get_mut(calendar_id) {
            for event in events.values_mut().filter(|event| !event.cancelled) {
                event.summary = model::projects::calendar_event_title(code, &event.summary);
            }
        }
        Ok(())
    }

    async fn event_backups(
        &self,
        calendar_id: &str,
    ) -> Result<Vec<CalendarEventBackup>, CalendarError> {
        let state = self.state.lock().expect("fake calendar mutex poisoned");
        let calendar = state
            .calendars
            .get(calendar_id)
            .ok_or(CalendarError::InvalidEventBackup)?;
        Ok(calendar.backups.clone())
    }

    async fn events_in_window(
        &self,
        calendar_id: &str,
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
    ) -> Result<Vec<ProjectCalendarEvent>, CalendarError> {
        let state = self.state.lock().expect("fake calendar mutex poisoned");
        let mut events = Vec::new();
        for (id, event) in state.events.get(calendar_id).into_iter().flatten() {
            if event.cancelled {
                continue;
            }
            let body = event_body(event);
            let in_window = match event.timing {
                EventTiming::AllDay(day) => day >= start.date_naive() && day <= end.date_naive(),
                EventTiming::Hearing(minutes) => {
                    event.start < end
                        && event.start + chrono::Duration::minutes(i64::from(minutes)) > start
                }
                EventTiming::BusyUntil(ends_at) => event.start < end && ends_at > start,
                EventTiming::Instant => {
                    event.start < end && event.start + chrono::Duration::minutes(1) > start
                }
            };
            if in_window {
                events.push((
                    event.start,
                    ProjectCalendarEvent {
                        id: id.clone(),
                        transparency: Some(body.transparency.into()),
                        summary: Some(event.summary.clone()),
                        start: body.start.into(),
                        end: body.end.into(),
                    },
                ));
            }
        }
        events.sort_by(|(a, left), (b, right)| (a, &left.id).cmp(&(b, &right.id)));
        Ok(events.into_iter().map(|(_, event)| event).collect())
    }

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

    async fn create_calendar(
        &self,
        summary: &str,
        timezone: &str,
    ) -> Result<ProjectCalendar, CalendarError> {
        let mut state = self.state.lock().expect("fake calendar mutex poisoned");
        state.next_id += 1;
        state.created += 1;
        let id = format!("c_{}@group.calendar.test", state.next_id);
        state.calendars.insert(
            id.clone(),
            FakeCalendarEntry {
                timezone: timezone.to_owned(),
                backups: Vec::new(),
                summary: summary.to_string(),
                members: BTreeMap::from([(FAKE_CALENDAR_OWNER.to_string(), CalendarRole::Owner)]),
            },
        );
        Ok(ProjectCalendar {
            id,
            summary: summary.to_string(),
        })
    }

    async fn reconcile_calendar_timezone(
        &self,
        code: &str,
        id: &str,
        timezone: &str,
    ) -> Result<(), CalendarError> {
        let mut state = self.state.lock().expect("fake calendar mutex poisoned");
        let entry = state
            .calendars
            .get_mut(id)
            .ok_or(CalendarError::UnverifiedCalendarMetadata)?;
        if entry.summary != code
            || entry.members.get(FAKE_CALENDAR_OWNER) != Some(&CalendarRole::Owner)
        {
            return Err(CalendarError::UnverifiedCalendarMetadata);
        }
        timezone.clone_into(&mut entry.timezone);
        Ok(())
    }

    async fn calendar_exists(&self, calendar_id: &str) -> Result<bool, CalendarError> {
        let state = self.state.lock().expect("fake calendar mutex poisoned");
        Ok(state.calendars.contains_key(calendar_id))
    }

    async fn calendar_etag(&self, id: &str) -> Result<Option<String>, CalendarError> {
        Ok(self.calendar_exists(id).await?.then(|| id.to_owned()))
    }
    async fn delete_calendar(&self, id: &str, etag: &str) -> Result<(), CalendarError> {
        if self.calendar_etag(id).await?.as_deref() != Some(etag) {
            return Err(CalendarError::Api {
                action: "conditional calendar deletion",
                status: 412,
            });
        }
        self.delete(id);
        Ok(())
    }

    async fn verify_retirement_acl(&self, id: &str, owner: &str) -> Result<(), CalendarError> {
        let sharing = self.sharing(id);
        if sharing.get(owner) == Some(&CalendarRole::Owner)
            && !sharing.iter().any(|(email, role)| {
                email != owner && matches!(role, CalendarRole::Owner | CalendarRole::Writer)
            })
        {
            return Ok(());
        }
        Err(CalendarError::Api {
            action: "verifying retirement access",
            status: 409,
        })
    }

    async fn owned_calendar_codes(&self) -> Result<BTreeMap<String, String>, CalendarError> {
        Ok(self
            .calendars()
            .into_iter()
            .map(|c| (c.id, c.summary))
            .collect())
    }

    async fn members(&self, calendar_id: &str) -> Result<Vec<CalendarMember>, CalendarError> {
        Ok(self
            .sharing(calendar_id)
            .into_iter()
            .map(|(email, role)| CalendarMember { email, role })
            .collect())
    }

    async fn grant_reader(&self, calendar_id: &str, email: &str) -> Result<bool, CalendarError> {
        let mut state = self.state.lock().expect("fake calendar mutex poisoned");
        let entry = state
            .calendars
            .get_mut(calendar_id)
            .ok_or(CalendarError::Api {
                action: "granting calendar access",
                status: 404,
            })?;
        if entry.members.contains_key(email) {
            return Ok(false);
        }
        entry.members.insert(email.into(), CalendarRole::Reader);
        Ok(true)
    }

    async fn revoke_reader(&self, calendar_id: &str, email: &str) -> Result<(), CalendarError> {
        let mut state = self.state.lock().expect("fake calendar mutex poisoned");
        if let Some(entry) = state.calendars.get_mut(calendar_id) {
            if entry.members.get(email) == Some(&CalendarRole::Reader) {
                entry.members.remove(email);
            }
        }
        Ok(())
    }

    async fn verify_private_acl(&self, _calendar_id: &str) -> Result<(), CalendarError> {
        Ok(())
    }

    async fn verify_workspace_member(&self, _email: &str) -> Result<(), CalendarError> {
        Ok(())
    }

    async fn ensure_reader_calendar(
        &self,
        calendar_id: &str,
        email: &str,
    ) -> Result<bool, CalendarError> {
        Ok(self
            .sharing(calendar_id)
            .get(email)
            .is_some_and(|role| *role != CalendarRole::FreeBusyReader))
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
        let (event, stored) = fake_mirror_write(&id, event.clone());
        state
            .events
            .entry(calendar_id.to_string())
            .or_default()
            .insert(id, event);
        Ok(stored)
    }

    async fn patch_event(
        &self,
        calendar_id: &str,
        previous: &StoredEvent,
        event: &CalendarEvent,
    ) -> Result<StoredEvent, CalendarError> {
        let mut state = self.state.lock().expect("fake calendar mutex poisoned");
        let existing = state
            .events
            .get_mut(calendar_id)
            .and_then(|events| events.get_mut(&previous.id))
            .ok_or(CalendarError::Api {
                action: "updating an event",
                status: 404,
            })?;
        verify_fake_version(previous, existing)?;
        let (updated, stored) = fake_mirror_write(&previous.id, event.clone());
        *existing = updated;
        Ok(stored)
    }

    async fn delete_event(
        &self,
        calendar_id: &str,
        previous: &StoredEvent,
    ) -> Result<(), CalendarError> {
        let mut state = self.state.lock().expect("fake calendar mutex poisoned");
        if let Some(events) = state.events.get_mut(calendar_id) {
            if let Some(existing) = events.get(&previous.id) {
                verify_fake_version(previous, existing)?;
                events.remove(&previous.id);
            }
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

/// Why a calendar cannot be read. A gap is never an empty busy list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalendarGap {
    /// The calendar is outside the sharing arrangement.
    Share,
    /// The Workspace grant does not include the scope this read needs.
    Scope,
    /// The provider returned incomplete or invalid availability.
    InvalidResponse,
}

/// One busy interval on a calendar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreeBusyBlock {
    pub start: DateTime<FixedOffset>,
    pub end: DateTime<FixedOffset>,
}

/// Free/busy for one calendar in a window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarWindow {
    pub calendar_id: String,
    pub busy: Vec<FreeBusyBlock>,
    pub gap: Option<CalendarGap>,
}

/// The signed-in attorney's events, or the gap that blocked the read.
#[derive(Debug)]
pub enum EventRead {
    Events(Vec<ProjectCalendarEvent>),
    Gap(CalendarGap),
}

/// Read attorney calendars through Workspace delegation.
///
/// Free/busy uses [`CALENDAR_FREEBUSY_SCOPE`] and returns no event details.
/// Event details use [`CALENDAR_EVENTS_SCOPE`] and only for the attorney the
/// caller names, which the portal limits to the signed-in Person.
#[async_trait]
pub trait AttorneyCalendars: Send + Sync {
    async fn free_busy(
        &self,
        attorney_email: &str,
        calendar_ids: &[String],
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
    ) -> Result<Vec<CalendarWindow>, CalendarError>;

    async fn events(
        &self,
        attorney_email: &str,
        calendar_id: &str,
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
    ) -> Result<EventRead, CalendarError>;
}

/// Calendar v3 free/busy and event reads, acting as one Workspace user at a time.
pub struct GoogleAttorneyCalendars {
    service_account: String,
    base_url: String,
}

impl GoogleAttorneyCalendars {
    /// Build from the Workspace delegation keys.
    ///
    /// # Errors
    /// [`CalendarError::MissingConfig`] when a key is absent.
    pub fn from_env() -> Result<Self, CalendarError> {
        match crate::delegation::WorkspaceDelegation::from_lookup(|key| std::env::var(key).ok()) {
            Ok(delegation) => Ok(Self {
                service_account: delegation.service_account,
                base_url: CALENDAR_BASE_URL.to_string(),
            }),
            Err(crate::delegation::DelegationError::MissingConfig(missing)) => {
                Err(CalendarError::MissingConfig(missing))
            }
            Err(crate::delegation::DelegationError::Authentication) => {
                Err(CalendarError::Authentication)
            }
        }
    }

    async fn token(&self, attorney_email: &str, scope: &str) -> Result<String, CalendarError> {
        let delegation = crate::delegation::WorkspaceDelegation {
            service_account: self.service_account.clone(),
            delegated_user: attorney_email.to_string(),
        };
        let source = delegation
            .token_source(&[scope])
            .await
            .map_err(|_| CalendarError::Authentication)?;
        let raw = source
            .token()
            .await
            .map_err(|_| CalendarError::Authentication)?;
        Ok(raw.strip_prefix("Bearer ").unwrap_or(&raw).to_string())
    }
}

#[async_trait]
impl AttorneyCalendars for GoogleAttorneyCalendars {
    async fn free_busy(
        &self,
        attorney_email: &str,
        calendar_ids: &[String],
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
    ) -> Result<Vec<CalendarWindow>, CalendarError> {
        if calendar_ids.is_empty() {
            return Ok(Vec::new());
        }
        let token = match self.token(attorney_email, CALENDAR_FREEBUSY_SCOPE).await {
            Ok(token) => token,
            Err(CalendarError::Authentication) => {
                return Ok(scope_gaps(calendar_ids));
            }
            Err(error) => return Err(error),
        };
        let action = "reading free/busy";
        let response = reqwest::Client::new()
            .post(format!(
                "{}/freeBusy",
                self.base_url.trim_end_matches('/')
            ))
            .bearer_auth(token)
            .json(&serde_json::json!({
                "timeMin": start.to_rfc3339(),
                "timeMax": end.to_rfc3339(),
                "items": calendar_ids.iter().map(|id| serde_json::json!({"id": id})).collect::<Vec<_>>(),
            }))
            .send()
            .await
            .map_err(|source| CalendarError::Request { action, source })?;
        let status = response.status().as_u16();
        if matches!(status, 401 | 403) {
            return Ok(scope_gaps(calendar_ids));
        }
        if !(200..300).contains(&status) {
            return Err(CalendarError::Api { action, status });
        }
        let body = response
            .text()
            .await
            .map_err(|source| CalendarError::Response { action, source })?;
        parse_free_busy(&body, calendar_ids)
    }

    async fn events(
        &self,
        attorney_email: &str,
        calendar_id: &str,
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
    ) -> Result<EventRead, CalendarError> {
        let token = match self.token(attorney_email, CALENDAR_EVENTS_SCOPE).await {
            Ok(token) => token,
            Err(CalendarError::Authentication) => return Ok(EventRead::Gap(CalendarGap::Scope)),
            Err(error) => return Err(error),
        };
        let action = "listing attorney events";
        let start_text = start.to_rfc3339();
        let end_text = end.to_rfc3339();
        let url = format!(
            "{}/calendars/{}/events",
            self.base_url.trim_end_matches('/'),
            urlencoding_path(calendar_id)
        );
        let mut page_token = None;
        let mut items = Vec::new();
        loop {
            let mut query = vec![
                ("timeMin", start_text.as_str()),
                ("timeMax", end_text.as_str()),
                ("singleEvents", "true"),
                ("orderBy", "startTime"),
                ("fields", "nextPageToken,items(id,summary,start,end)"),
            ];
            if let Some(page) = page_token.as_deref() {
                query.push(("pageToken", page));
            }
            let response = reqwest::Client::new()
                .get(&url)
                .bearer_auth(&token)
                .query(&query)
                .send()
                .await
                .map_err(|source| CalendarError::Request { action, source })?;
            let status = response.status().as_u16();
            if matches!(status, 401 | 403) {
                return Ok(EventRead::Gap(CalendarGap::Scope));
            }
            if matches!(status, 404 | 410) {
                return Ok(EventRead::Gap(CalendarGap::Share));
            }
            if !(200..300).contains(&status) {
                return Err(CalendarError::Api { action, status });
            }
            let page = response
                .json::<EventPage<ProjectCalendarEvent>>()
                .await
                .map_err(|source| CalendarError::Response { action, source })?;
            items.extend(page.items);
            page_token = page.next_page_token;
            if page_token.is_none() {
                break;
            }
        }
        Ok(EventRead::Events(items))
    }
}

fn scope_gaps(calendar_ids: &[String]) -> Vec<CalendarWindow> {
    calendar_ids
        .iter()
        .map(|calendar_id| CalendarWindow {
            calendar_id: calendar_id.clone(),
            busy: Vec::new(),
            gap: Some(CalendarGap::Scope),
        })
        .collect()
}

fn urlencoding_path(segment: &str) -> String {
    let mut url = url::Url::parse("https://calendar.example/").expect("base parses");
    url.path_segments_mut()
        .expect("base has a path")
        .push(segment);
    url.path().trim_start_matches('/').to_string()
}

/// Parse a Calendar API freeBusy body. A calendar with an error is a gap.
///
/// # Errors
/// [`CalendarError::Response`] when the body is not the freeBusy document.
pub fn parse_free_busy(
    body: &str,
    requested: &[String],
) -> Result<Vec<CalendarWindow>, CalendarError> {
    let parsed: FreeBusyResponse = serde_json::from_str(body).map_err(|_| CalendarError::Api {
        action: "reading free/busy",
        status: 200,
    })?;
    Ok(requested
        .iter()
        .map(|calendar_id| {
            let Some(entry) = parsed.calendars.get(calendar_id) else {
                return CalendarWindow {
                    calendar_id: calendar_id.clone(),
                    busy: Vec::new(),
                    gap: Some(CalendarGap::Share),
                };
            };
            if !entry.errors.is_empty() {
                let scope = entry.errors.iter().any(|error| {
                    matches!(
                        error.reason.as_deref(),
                        Some("forbidden" | "insufficientPermissions" | "authError")
                    )
                });
                return CalendarWindow {
                    calendar_id: calendar_id.clone(),
                    busy: Vec::new(),
                    gap: Some(if scope {
                        CalendarGap::Scope
                    } else {
                        CalendarGap::Share
                    }),
                };
            }
            let busy = entry.busy.as_ref().and_then(|blocks| {
                blocks
                    .iter()
                    .map(FreeBusyInstant::interval)
                    .collect::<Option<Vec<_>>>()
            });
            CalendarWindow {
                calendar_id: calendar_id.clone(),
                gap: busy.is_none().then_some(CalendarGap::InvalidResponse),
                busy: busy.unwrap_or_default(),
            }
        })
        .collect())
}

#[derive(Deserialize)]
struct FreeBusyResponse {
    #[serde(default)]
    calendars: BTreeMap<String, FreeBusyEntry>,
}

#[derive(Deserialize)]
struct FreeBusyEntry {
    busy: Option<Vec<FreeBusyInstant>>,
    #[serde(default)]
    errors: Vec<FreeBusyReason>,
}

#[derive(Deserialize)]
struct FreeBusyInstant {
    start: String,
    end: String,
}

impl FreeBusyInstant {
    fn interval(&self) -> Option<FreeBusyBlock> {
        let start = DateTime::parse_from_rfc3339(&self.start).ok()?;
        let end = DateTime::parse_from_rfc3339(&self.end).ok()?;
        (start < end).then_some(FreeBusyBlock { start, end })
    }
}

#[derive(Deserialize)]
struct FreeBusyReason {
    reason: Option<String>,
}

#[derive(Debug, Clone)]
struct SyntheticBusy {
    calendar_id: String,
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
    summary: Option<String>,
}

/// In-memory attorney calendars for tests. A hidden calendar is a missing
/// share. An email on the scope-denied set fails every read for that person.
#[derive(Clone, Debug, Default)]
pub struct FakeAttorneyCalendars {
    rows: Arc<Mutex<Vec<SyntheticBusy>>>,
    hidden: Arc<Mutex<BTreeSet<String>>>,
    scope_denied: Arc<Mutex<BTreeSet<String>>>,
}

impl FakeAttorneyCalendars {
    /// Record a busy interval. `summary` is returned only by [`AttorneyCalendars::events`].
    pub fn record(
        &self,
        calendar_id: impl Into<String>,
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
        summary: Option<&str>,
    ) {
        self.rows
            .lock()
            .expect("fake attorney calendars poisoned")
            .push(SyntheticBusy {
                calendar_id: calendar_id.into(),
                start,
                end,
                summary: summary.map(str::to_string),
            });
    }

    /// The calendar is outside the sharing arrangement.
    pub fn hide(&self, calendar_id: impl Into<String>) {
        self.hidden
            .lock()
            .expect("fake attorney calendars poisoned")
            .insert(calendar_id.into());
    }

    /// Delegation for this attorney lacks the calendar scope.
    pub fn deny_scope(&self, email: impl Into<String>) {
        self.scope_denied
            .lock()
            .expect("fake attorney calendars poisoned")
            .insert(email.into().to_ascii_lowercase());
    }

    fn denied(&self, email: &str) -> bool {
        self.scope_denied
            .lock()
            .expect("fake attorney calendars poisoned")
            .contains(&email.to_ascii_lowercase())
    }

    fn hidden_id(&self, calendar_id: &str) -> bool {
        self.hidden
            .lock()
            .expect("fake attorney calendars poisoned")
            .contains(calendar_id)
    }
}

#[async_trait]
impl AttorneyCalendars for FakeAttorneyCalendars {
    async fn free_busy(
        &self,
        attorney_email: &str,
        calendar_ids: &[String],
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
    ) -> Result<Vec<CalendarWindow>, CalendarError> {
        let rows = self.rows.lock().expect("fake attorney calendars poisoned");
        Ok(calendar_ids
            .iter()
            .map(|calendar_id| {
                if self.denied(attorney_email) {
                    return CalendarWindow {
                        calendar_id: calendar_id.clone(),
                        busy: Vec::new(),
                        gap: Some(CalendarGap::Scope),
                    };
                }
                if self.hidden_id(calendar_id) {
                    return CalendarWindow {
                        calendar_id: calendar_id.clone(),
                        busy: Vec::new(),
                        gap: Some(CalendarGap::Share),
                    };
                }
                let busy = rows
                    .iter()
                    .filter(|row| {
                        row.calendar_id == *calendar_id && overlaps(row.start, row.end, start, end)
                    })
                    .map(|row| FreeBusyBlock {
                        start: row.start,
                        end: row.end,
                    })
                    .collect();
                CalendarWindow {
                    calendar_id: calendar_id.clone(),
                    busy,
                    gap: None,
                }
            })
            .collect())
    }

    async fn events(
        &self,
        attorney_email: &str,
        calendar_id: &str,
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
    ) -> Result<EventRead, CalendarError> {
        if self.denied(attorney_email) {
            return Ok(EventRead::Gap(CalendarGap::Scope));
        }
        if self.hidden_id(calendar_id) {
            return Ok(EventRead::Gap(CalendarGap::Share));
        }
        let rows = self.rows.lock().expect("fake attorney calendars poisoned");
        let events = rows
            .iter()
            .filter(|row| {
                row.calendar_id == calendar_id && overlaps(row.start, row.end, start, end)
            })
            .enumerate()
            .map(|(index, row)| ProjectCalendarEvent {
                id: format!("synthetic-{index}"),
                transparency: None,
                summary: row.summary.clone(),
                start: ProjectCalendarEventTime {
                    date: None,
                    date_time: Some(row.start.to_rfc3339()),
                },
                end: ProjectCalendarEventTime {
                    date: None,
                    date_time: Some(row.end.to_rfc3339()),
                },
            })
            .collect();
        Ok(EventRead::Events(events))
    }
}

fn overlaps(
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
    use chrono::DateTime;
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    use async_trait::async_trait;
    use wiremock::matchers::{body_json, body_partial_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::{
        event_body, AttorneyCalendars, CalendarError, CalendarEvent, CalendarRole, CalendarService,
        FakeCalendar, GoogleCalendar, StoredEvent,
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
        GoogleCalendar::from_parts(Arc::new(StaticToken), &server.uri(), "owner@calendar.test")
    }

    #[tokio::test]
    async fn missing_configuration_names_every_key() {
        let error = GoogleCalendar::from_lookup(|_| None)
            .await
            .err()
            .expect("no configuration is refused");
        let message = error.to_string();
        assert!(message.contains(super::CALENDAR_OWNER_ENV), "{message}");
        assert!(
            message.contains("NAVIGATOR_DRIVE_NEON_LAW_SERVICE_ACCOUNT"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn lists_new_project_calendars_and_preserves_existing_display_preferences() {
        let server = MockServer::start().await;
        for (id, response) in [
            (
                "existing",
                ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({"id":"existing","selected":false,"hidden":true}),
                ),
            ),
            ("new", ResponseTemplate::new(404)),
        ] {
            Mock::given(method("GET"))
                .and(path(format!("/users/me/calendarList/{id}")))
                .respond_with(response)
                .up_to_n_times(1)
                .expect(1)
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path("/users/me/calendarList/new"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"new"})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/users/me/calendarList"))
            .and(body_json(serde_json::json!({"id":"new","selected":true})))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let participant = google(&server);
        participant.ensure_listed("existing").await.unwrap();
        participant.ensure_listed("new").await.unwrap();
    }

    #[tokio::test]
    async fn subscription_requires_exact_readback_after_success_or_conflict() {
        let server = MockServer::start().await;
        let calendar = google(&server);
        for (id, insert_status, returned_id) in [
            ("raced", 409, Some("raced")),
            ("wrong", 409, Some("other")),
            ("missing", 200, None),
        ] {
            let entry = format!("/users/me/calendarList/{id}");
            Mock::given(method("GET"))
                .and(path(&entry))
                .respond_with(ResponseTemplate::new(404))
                .up_to_n_times(1)
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path(&entry))
                .respond_with(match returned_id {
                    Some(id) => {
                        ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": id}))
                    }
                    None => ResponseTemplate::new(404),
                })
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path("/users/me/calendarList"))
                .and(body_json(serde_json::json!({"id":id,"selected":true})))
                .respond_with(ResponseTemplate::new(insert_status))
                .expect(1)
                .mount(&server)
                .await;
            let result = calendar.ensure_listed(id).await;
            if returned_id == Some(id) {
                result.unwrap();
            } else {
                assert!(matches!(
                    result,
                    Err(CalendarError::UnverifiedCalendarListEntry)
                ));
            }
        }
    }

    #[tokio::test]
    async fn creates_a_calendar_named_for_the_project_code() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/calendars"))
            .and(body_json(
                serde_json::json!({ "summary": "matter-42", "timeZone":"America/Chicago" }),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "id": "c_42@group.calendar.example.com" })),
            )
            .expect(1)
            .mount(&server)
            .await;
        let calendar = google(&server)
            .create_calendar("matter-42", "America/Chicago")
            .await
            .unwrap();
        assert_eq!(calendar.id, "c_42@group.calendar.example.com");
        assert_eq!(calendar.summary, "matter-42");
    }

    #[tokio::test]
    async fn timezone_reconcile_changes_only_drifted_managed_calendar_metadata() {
        let server = MockServer::start().await;
        let mut calendar = google(&server);
        calendar.namespace = Some("production");
        let marker = "Navigator Project Calendar (production)";
        let zone = "America/Phoenix";
        for (id, before, description, owner) in [
            ("moved", "UTC", marker, "owner@calendar.test"),
            ("current", zone, marker, "owner@calendar.test"),
            ("manual", "UTC", "", "owner@calendar.test"),
            ("transferred", "UTC", marker, "other@calendar.test"),
        ] {
            Mock::given(method("GET"))
                .and(path(format!("/calendars/{id}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id":id,"summary":"sample","description":description,"timeZone":before,
                    "dataOwner":owner,"etag":"version-one"
                })))
                .expect(1)
                .mount(&server)
                .await;
        }
        Mock::given(method("PATCH"))
            .and(path("/calendars/moved"))
            .and(wiremock::matchers::header("if-match", "version-one"))
            .and(body_json(serde_json::json!({"timeZone": zone})))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        for id in ["moved", "current", "manual"] {
            calendar
                .reconcile_calendar_timezone("sample", id, zone)
                .await
                .unwrap();
        }
        assert!(matches!(
            calendar
                .reconcile_calendar_timezone("sample", "transferred", zone)
                .await,
            Err(CalendarError::UnverifiedCalendarMetadata)
        ));
    }

    #[tokio::test]
    async fn staging_creates_and_finds_only_its_own_project_calendars() {
        let server = MockServer::start().await;
        let mut calendar = google(&server);
        calendar.namespace = Some("staging");
        let marker = "Navigator Project Calendar (staging)";
        Mock::given(method("POST"))
            .and(path("/calendars"))
            .and(body_json(serde_json::json!({
                "summary": "staging-fixture", "description": marker, "timeZone":"America/New_York"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "staging-id"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/users/me/calendarList"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [
                    {"id": "staging-id", "summary": "staging-fixture", "description": marker},
                    {"id": "production-id", "summary": "staging-fixture", "description": "Navigator Project Calendar (production)"},
                    {"id": "legacy-id", "summary": "staging-fixture"}
                ]
            })))
            .expect(2)
            .mount(&server)
            .await;
        assert_eq!(
            calendar
                .create_calendar("fixture", "America/New_York")
                .await
                .unwrap()
                .summary,
            "staging-fixture"
        );
        assert_eq!(
            calendar.find_calendar("fixture").await.unwrap().unwrap().id,
            "staging-id"
        );
        Mock::given(method("GET"))
            .and(path("/calendars/legacy-id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "legacy-id", "summary": "staging-fixture"
            })))
            .expect(1)
            .mount(&server)
            .await;
        assert!(!calendar.calendar_exists("legacy-id").await.unwrap());
        calendar.namespace = Some("production");
        assert_eq!(
            calendar
                .find_calendar("staging-fixture")
                .await
                .unwrap()
                .unwrap()
                .id,
            "production-id"
        );
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
    async fn calendar_list_absence_is_checked_against_the_recorded_resource() {
        let server = MockServer::start().await;
        let calendar = google(&server);
        for (id, status, expected) in [
            (
                "unlisted",
                200,
                "absent from the delegated owner's calendar list",
            ),
            ("hidden", 404, "missing or hidden"),
            ("denied", 403, "cannot access"),
            ("deleted", 410, "was deleted"),
        ] {
            Mock::given(method("GET"))
                .and(path(format!("/users/me/calendarList/{id}")))
                .respond_with(ResponseTemplate::new(404))
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path(format!("/calendars/{id}")))
                .respond_with(ResponseTemplate::new(status))
                .expect(1)
                .mount(&server)
                .await;
            let detail = calendar
                .recorded_calendar_problem("synthetic", id)
                .await
                .unwrap()
                .unwrap();
            assert!(detail.contains(expected), "{detail}");
        }
    }

    #[tokio::test]
    async fn a_linked_calendar_must_have_the_projects_name_and_be_owned() {
        let server = MockServer::start().await;
        for (id, summary, role) in [
            ("owned", "synthetic", "owner"),
            ("other-project", "different", "owner"),
            ("other-firm", "synthetic", "reader"),
        ] {
            Mock::given(method("GET"))
                .and(path(format!("/users/me/calendarList/{id}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": id, "summary": summary, "accessRole": role
                })))
                .expect(1)
                .mount(&server)
                .await;
        }
        let calendar = google(&server);
        assert!(calendar
            .recorded_calendar_matches("synthetic", "owned")
            .await
            .unwrap());
        assert!(!calendar
            .recorded_calendar_matches("synthetic", "other-project")
            .await
            .unwrap());
        assert!(!calendar
            .recorded_calendar_matches("synthetic", "other-firm")
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn a_domain_grant_blocks_automatic_sharing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/calendars/shared/acl"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [{"role": "reader", "scope": {"type": "domain", "value": "example.com"}}]
            })))
            .expect(1)
            .mount(&server)
            .await;
        assert!(matches!(
            google(&server).verify_private_acl("shared").await,
            Err(CalendarError::Api { status: 403, .. })
        ));
    }

    #[tokio::test]
    async fn title_repair_only_patches_the_summary_of_an_owned_event() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/calendars/shared/events"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"items": [
                {"id":"series","etag":"one","organizer":{"self":true},"summary":"Hearing - acme"},
                {"id":"correct","etag":"two","organizer":{"self":true},"summary":"acme: Call"},
                {"id":"external","etag":"three","organizer":{"self":false},"summary":"Meeting"},
                {"id":"locked","etag":"four","organizer":{"self":true},"locked":true}
            ]}))).mount(&server).await;
        Mock::given(method("PATCH"))
            .and(path("/calendars/shared/events/series"))
            .and(query_param("sendUpdates", "none"))
            .and(wiremock::matchers::header("if-match", "one"))
            .and(body_json(serde_json::json!({"summary":"acme: Hearing"})))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        google(&server)
            .reconcile_project_events("shared", Some("acme"), &BTreeSet::new(), &BTreeSet::new())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn upcoming_owned_events_drop_only_firm_and_brand_guests() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/calendars/shared/events"))
            .and(wiremock::matchers::query_param_is_missing("timeMin"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"items": [
                {"id":"upcoming","etag":"one","summary":"sample: Meeting",
                 "organizer":{"self":true,"email":"owner@firm.example"},
                 "attendees":[{"email":"owner@firm.example","organizer":true},
                              {"email":"staff@FIRM.EXAMPLE","optional":true},
                              {"email":"team@brand.example"},
                              {"email":"room@firm.example","resource":true},
                              {"email":"client@otherfirm.example","responseStatus":"accepted"}],
                 "extendedProperties":{"private":{"nv_invited_current":"team@brand.example"}}},
                {"id":"past","etag":"two","summary":"sample: Past","organizer":{"self":true},
                 "attendees":[{"email":"staff@firm.example"}]},
                {"id":"external","etag":"three","organizer":{"self":false},
                 "attendees":[{"email":"staff@firm.example"}]},
                {"id":"incomplete","etag":"four","summary":"sample: Hidden","organizer":{"self":true},
                 "attendeesOmitted":true,"attendees":[{"email":"staff@firm.example"}]}
            ]})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/calendars/shared/events"))
            .and(query_param("fields", "nextPageToken,items(id)"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"items": [
                    {"id":"upcoming"},{"id":"external"},{"id":"incomplete"}
                ]})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/calendars/shared/events/upcoming"))
            .and(query_param("sendUpdates", "none"))
            .and(wiremock::matchers::header("if-match", "one"))
            .and(body_json(serde_json::json!({
                "attendees":[{"email":"owner@firm.example","organizer":true},
                             {"email":"room@firm.example","resource":true},
                             {"email":"client@otherfirm.example","responseStatus":"accepted"}],
                "extendedProperties":{"private":{"nv_invited_current":null}}
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        google(&server)
            .reconcile_project_events(
                "shared",
                Some("sample"),
                &["team@brand.example"].into_iter().collect(),
                &["firm.example", "brand.example"].into_iter().collect(),
            )
            .await
            .unwrap();
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
    }

    #[test]
    fn offboarding_revokes_only_marked_guests_who_lost_calendar_access() {
        let event: super::MembershipEvent = serde_json::from_value(serde_json::json!({
            "id":"series", "etag":"one", "organizer":{"self":true},
            "extendedProperties":{"private":{"nv_invited_old":"old@example.com","nv_invited_current":"current@example.com"}},
            "attendees":[{"email":"old@example.com"},{"email":"manual@example.com"},{"email":"current@example.com","responseStatus":"accepted"}]
        })).unwrap();
        let members = ["current@example.com"].into_iter().collect();
        assert_eq!(
            event.membership_patch(None, &members, None).unwrap(),
            Some(serde_json::json!({
                "attendees":[{"email":"manual@example.com"},{"email":"current@example.com","responseStatus":"accepted"}],
                "extendedProperties":{"private":{"nv_invited_old":null}}
            }))
        );
    }

    #[tokio::test]
    async fn quiet_reader_grant_preserves_an_existing_role() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/calendars/shared/acl"))
            .and(query_param("sendNotifications", "false"))
            .respond_with(ResponseTemplate::new(409))
            .expect(1)
            .mount(&server)
            .await;
        assert!(!google(&server)
            .grant_reader("shared", "clerk@example.com")
            .await
            .unwrap());
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
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
    async fn updates_an_existing_writer_grant_without_notifying() {
        let server = MockServer::start().await;
        let acl = "/calendars/c_42@group.calendar.example.com/acl";
        Mock::given(method("POST"))
            .and(path(acl))
            .and(query_param("sendNotifications", "false"))
            .respond_with(ResponseTemplate::new(409))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(acl))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [{
                    "id": "user:Clerk@example.com",
                    "role": "writer",
                    "scope": { "type": "user", "value": "Clerk@example.com" }
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path(format!("{acl}/user:Clerk@example.com")))
            .and(query_param("sendNotifications", "false"))
            .and(body_json(serde_json::json!({ "role": "reader" })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        google(&server)
            .set_member_role(
                "c_42@group.calendar.example.com",
                "clerk@example.com",
                CalendarRole::Reader,
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
                    { "role": "freeBusyReader", "scope": { "type": "user", "value": "clerk@example.com" } },
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
                ("clerk@example.com", CalendarRole::FreeBusyReader),
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
            .create_calendar("matter-42", "America/Chicago")
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
        let created = calendar
            .create_calendar("matter-42", "America/Chicago")
            .await
            .unwrap();
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

    #[test]
    fn civil_date_writes_an_exclusive_date_end_and_hearing_blocks_an_hour() {
        let mut event = event();
        event.timing = super::EventTiming::AllDay("2028-12-31".parse().unwrap());
        let json = serde_json::to_value(super::event_body(&event)).unwrap();
        assert_eq!(json["start"], serde_json::json!({"date":"2028-12-31"}));
        assert_eq!(json["end"], serde_json::json!({"date":"2029-01-01"}));
        assert_eq!(json["transparency"], "transparent");
        event.timing = super::EventTiming::Hearing(60);
        let json = serde_json::to_value(super::event_body(&event)).unwrap();
        assert_eq!(json["transparency"], "opaque");
        assert_eq!(
            json["end"]["dateTime"],
            (event.start + chrono::Duration::hours(1)).to_rfc3339()
        );
    }

    #[test]
    fn google_omits_an_unsupported_lead_and_preserves_all_three_court_popups() {
        let mut event = event();
        event.reminder_minutes = vec![43_200, 10_080, 4_320, 1_440];
        let body = serde_json::to_value(super::event_body(&event)).unwrap();
        assert_eq!(
            body["reminders"],
            serde_json::json!({
                "useDefault":false, "overrides":[
                    {"method":"popup","minutes":10_080}, {"method":"popup","minutes":4_320},
                    {"method":"popup","minutes":1_440}
                ]
            })
        );
    }

    fn event() -> CalendarEvent {
        CalendarEvent {
            summary: "acme: Court order deadline".into(),
            description: Some("Authority: Order setting briefing".into()),
            location: None,
            color_id: Some("11".into()),
            start: DateTime::parse_from_rfc3339("2026-11-02T17:00:00-08:00").unwrap(),
            sequence: 3,
            cancelled: false,
            timing: super::EventTiming::Instant,
            reminder_minutes: vec![10_080],
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

    #[test]
    fn a_booking_hold_has_no_guests_or_matter_details() {
        let hold = CalendarEvent {
            summary: "Busy".into(),
            description: None,
            location: None,
            color_id: None,
            start: "2026-11-26T12:30:00Z".parse().unwrap(),
            timing: super::EventTiming::BusyUntil("2026-11-26T12:50:15Z".parse().unwrap()),
            sequence: 0,
            cancelled: false,
            reminder_minutes: Vec::new(),
            private_properties: BTreeMap::new(),
        };
        let body = serde_json::to_value(event_body(&hold)).unwrap();
        assert_eq!(body["visibility"], "private");
        assert_eq!(body["transparency"], "opaque");
        assert_eq!(body["end"]["dateTime"], "2026-11-26T12:50:15+00:00");
        for key in ["attendees", "description", "location"] {
            assert!(body.get(key).is_none());
        }
        assert_eq!(body["reminders"]["useDefault"], false);
    }

    #[test]
    fn a_booking_hold_is_recoverable_if_google_created_it_before_the_baseline_stamp() {
        let mut record = serde_json::json!({
            "id": "hold",
            "etag": "one",
            "summary": "Busy",
            "status": "confirmed",
            "visibility": "private",
            "start": {"dateTime": "2026-11-26T07:30:00-05:00"},
            "end": {"dateTime": "2026-11-26T07:45:00-05:00"},
            "reminders": {"useDefault": false},
            "extendedProperties": {"private": {
                (super::BOOKING_MARKER): super::BOOKING_VERSION,
                (super::BOOKING_INTERVAL): "2026-11-26T12:30:00+00:00/2026-11-26T12:45:00+00:00"
            }}
        });
        assert!(super::stored_event(&record).unwrap().can_write());
        record["description"] = "A person's own edit".into();
        assert!(!super::stored_event(&record).unwrap().can_write());
    }

    async fn mock_mirror_write(
        server: &MockServer,
        calendar_id: &str,
        id: &str,
        event: &CalendarEvent,
    ) -> ResponseTemplate {
        let mut resource = serde_json::json!(event_body(event));
        resource["id"] = id.into();
        resource["etag"] = "written".into();
        let written = ResponseTemplate::new(200).set_body_json(&resource);
        let hash = super::event_fingerprint(&resource);
        resource["extendedProperties"]["private"][super::BASELINE] = hash.clone().into();
        resource["etag"] = "baselined".into();
        Mock::given(method("PATCH"))
            .and(path(format!("/calendars/{calendar_id}/events/{id}")))
            .and(wiremock::matchers::header("if-match", "written"))
            .and(body_json(
                serde_json::json!({"extendedProperties":{"private":{(super::BASELINE):hash}}}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(resource))
            .expect(1)
            .mount(server)
            .await;
        written
    }

    fn previous() -> StoredEvent {
        StoredEvent {
            id: "evt_1".into(),
            private_properties: BTreeMap::new(),
            writable_etag: Some("original".into()),
        }
    }

    #[test]
    fn a_colored_event_writes_colorid_and_an_uncolored_one_omits_it() {
        let colored = serde_json::to_value(event_body(&event())).unwrap();
        assert_eq!(colored["colorId"], "11");
        let mut plain = event();
        plain.color_id = None;
        let plain = serde_json::to_value(event_body(&plain)).unwrap();
        assert!(plain.get("colorId").is_none());
    }

    #[tokio::test]
    async fn insert_event_posts_a_one_minute_event_with_its_identity() {
        let server = MockServer::start().await;
        let calendar_id = "c_42@group.calendar.example.com";
        Mock::given(method("POST"))
            .and(path(format!("/calendars/{calendar_id}/events")))
            .and(body_json(serde_json::json!({
                "summary": "acme: Court order deadline",
                "description": "Authority: Order setting briefing",
                "colorId": "11",
                "start": { "dateTime": "2026-11-02T17:00:00-08:00" },
                "end": { "dateTime": "2026-11-02T17:01:00-08:00" },
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
            .respond_with(mock_mirror_write(&server, calendar_id, "evt_1", &event()).await)
            .expect(1)
            .mount(&server)
            .await;
        let stored_event = google(&server)
            .insert_event(calendar_id, &event())
            .await
            .unwrap();
        assert_eq!(stored_event.id, "evt_1");
        assert_eq!(stored_event.writable_etag.as_deref(), Some("baselined"));
        assert_eq!(stored_event.private_properties["body_sha"], "abc123");
    }

    #[tokio::test]
    async fn an_event_without_a_reminder_suppresses_the_defaults() {
        let server = MockServer::start().await;
        let calendar_id = "c_42@group.calendar.example.com";
        let mut silent = event();
        silent.reminder_minutes.clear();
        Mock::given(method("POST"))
            .and(path(format!("/calendars/{calendar_id}/events")))
            .and(body_partial_json(serde_json::json!({
                "reminders": { "useDefault": false, "overrides": [] }
            })))
            .respond_with(mock_mirror_write(&server, calendar_id, "evt_2", &silent).await)
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
        moved
            .private_properties
            .insert("body_sha".into(), "def456".into());
        moved.start = DateTime::parse_from_rfc3339("2026-11-09T17:00:00-08:00").unwrap();
        Mock::given(method("PATCH"))
            .and(wiremock::matchers::header("if-match", "original"))
            .and(path(format!("/calendars/{calendar_id}/events/evt_1")))
            .and(body_partial_json(serde_json::json!({
                "sequence": 4,
                "start": { "dateTime": "2026-11-09T17:00:00-08:00" }
            })))
            .respond_with(mock_mirror_write(&server, calendar_id, "evt_1", &moved).await)
            .expect(1)
            .mount(&server)
            .await;
        let stored_event = google(&server)
            .patch_event(calendar_id, &previous(), &moved)
            .await
            .unwrap();
        assert_eq!(
            stored_event.private_properties.get("body_sha"),
            Some(&"def456".to_string())
        );
    }

    #[tokio::test]
    async fn a_racing_edit_blocks_the_patch_and_delete() {
        let server = MockServer::start().await;
        for verb in ["PATCH", "DELETE"] {
            Mock::given(method(verb))
                .and(path("/calendars/cal/events/evt_1"))
                .and(wiremock::matchers::header("if-match", "original"))
                .respond_with(ResponseTemplate::new(412))
                .expect(1)
                .mount(&server)
                .await;
        }
        let calendar = google(&server);
        assert!(matches!(
            calendar.patch_event("cal", &previous(), &event()).await,
            Err(CalendarError::Api { status: 412, .. })
        ));
        assert!(matches!(
            calendar.delete_event("cal", &previous()).await,
            Err(CalendarError::Api { status: 412, .. })
        ));
    }

    #[tokio::test]
    async fn a_racing_edit_cannot_be_adopted_as_the_initial_baseline() {
        let server = MockServer::start().await;
        let mut inserted = serde_json::json!(event_body(&event()));
        inserted["id"] = "evt_1".into();
        inserted["etag"] = "insert-version".into();
        Mock::given(method("POST"))
            .and(path("/calendars/cal/events"))
            .respond_with(ResponseTemplate::new(200).set_body_json(inserted))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/calendars/cal/events/evt_1"))
            .and(wiremock::matchers::header("if-match", "insert-version"))
            .respond_with(ResponseTemplate::new(412))
            .expect(1)
            .mount(&server)
            .await;
        assert!(matches!(
            google(&server).insert_event("cal", &event()).await,
            Err(CalendarError::Api {
                action: "recording the event mirror baseline",
                status: 412
            })
        ));
    }

    #[test]
    fn edited_meeting_content_and_legacy_events_have_no_writable_version() {
        let mut resource = super::fake_event_resource("evt", &event());
        assert!(super::stored_event(&resource)
            .unwrap()
            .writable_etag
            .is_none());
        resource["extendedProperties"]["private"][super::BASELINE] =
            super::event_fingerprint(&resource).into();
        assert!(super::stored_event(&resource)
            .unwrap()
            .writable_etag
            .is_some());
        for (key, value) in [
            ("summary", serde_json::json!("New hearing name")),
            ("description", serde_json::json!("Call notes")),
            (
                "attendees",
                serde_json::json!([{"email":"guest@example.test"}]),
            ),
            ("attachments", serde_json::json!([{"fileId":"recording"}])),
            (
                "conferenceData",
                serde_json::json!({"conferenceId":"test-meet"}),
            ),
            ("id", serde_json::json!("manually-copied-event")),
        ] {
            let mut changed = resource.clone();
            changed[key] = value;
            assert!(
                super::stored_event(&changed)
                    .unwrap()
                    .writable_etag
                    .is_none(),
                "{key}"
            );
        }
        resource["start"]["dateTime"] = "2026-11-03T01:00:00Z".into();
        assert!(super::stored_event(&resource)
            .unwrap()
            .writable_etag
            .is_some());
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
        calendar
            .delete_event(calendar_id, &previous())
            .await
            .unwrap();
        calendar
            .delete_event(calendar_id, &previous())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn availability_accepts_only_owned_secondary_calendars_in_the_deployment() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/users/me/calendarList"))
            .and(query_param("minAccessRole", "owner")).and(query_param("showHidden", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"items":[
                {"id":"matching","summary":"staging-sample","accessRole":"owner"},
                {"id":"foreign","summary":"staging-sample","accessRole":"owner","description":"Navigator Project Calendar (production)"},
                {"id":"primary","summary":"staging-sample","accessRole":"owner","primary":true},
                {"id":"other","summary":"sample","accessRole":"owner"},
                {"id":"reader","summary":"staging-sample","accessRole":"reader"}
            ]}))).expect(1).mount(&server).await;
        let mut calendar = google(&server);
        calendar.namespace = Some("staging");
        assert_eq!(
            calendar.owned_calendar_codes().await.unwrap(),
            [("matching".into(), "sample".into())].into_iter().collect()
        );
    }

    #[tokio::test]
    async fn project_events_only_read_the_verified_owned_calendar() {
        let server = MockServer::start().await;
        for (id, summary) in [
            ("owned", "sample-litigation"),
            ("different", "another-project"),
        ] {
            Mock::given(method("GET"))
                .and(path(format!("/users/me/calendarList/{id}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": id, "summary": summary, "accessRole": "owner"
                })))
                .expect(1)
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path("/calendars/different/events"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/calendars/owned/events"))
            .and(query_param("timeMin", "2026-11-01T00:00:00+00:00"))
            .and(query_param("timeMax", "2026-11-02T00:00:00+00:00"))
            .and(query_param("singleEvents", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [{
                    "id": "evt_1", "summary": "Synthetic call",
                    "start": {"dateTime": "2026-11-01T13:00:00Z"},
                    "end": {"dateTime": "2026-11-01T14:00:00Z"}
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let calendar = google(&server);
        let start = DateTime::parse_from_rfc3339("2026-11-01T00:00:00Z").unwrap();
        let end = DateTime::parse_from_rfc3339("2026-11-02T00:00:00Z").unwrap();
        assert!(calendar
            .project_events("sample-litigation", "different", start, end)
            .await
            .unwrap()
            .is_none());
        let events = calendar
            .project_events("sample-litigation", "owned", start, end)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(events[0].summary.as_deref(), Some("Synthetic call"));
    }

    #[tokio::test]
    async fn mirrored_events_paginate_and_filter_by_the_marker() {
        let server = MockServer::start().await;
        let calendar_id = "c_42@group.calendar.example.com";
        Mock::given(method("GET"))
            .and(path(format!("/calendars/{calendar_id}/events")))
            .and(query_param(
                "privateExtendedProperty",
                "navigator_mirror=deadline",
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
                "navigator_mirror=deadline",
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
        let created = calendar
            .create_calendar("matter-42", "America/Chicago")
            .await
            .unwrap();
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
        let updated = calendar
            .patch_event(&created.id, &first, &edited)
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
        calendar.delete_event(&created.id, &updated).await.unwrap();
        calendar.delete_event(&created.id, &updated).await.unwrap();
        assert_eq!(calendar.events(&created.id).len(), 1);
        calendar.remove_event(&created.id, &second_stored.id);
        assert!(calendar.events(&created.id).is_empty());
        assert!(calendar.insert_event("c_unknown", &event()).await.is_err());
    }

    #[tokio::test]
    async fn the_fake_patches_and_refuses_an_unknown_event() {
        let calendar = FakeCalendar::default();
        let created = calendar
            .create_calendar("matter-42", "America/Chicago")
            .await
            .unwrap();
        let inserted = calendar.insert_event(&created.id, &event()).await.unwrap();
        let mut moved = event();
        moved.start = DateTime::parse_from_rfc3339("2026-11-09T17:00:00-08:00").unwrap();
        calendar
            .patch_event(&created.id, &inserted, &moved)
            .await
            .unwrap();
        assert!(calendar
            .patch_event(&created.id, &previous(), &moved)
            .await
            .is_err());
        calendar.put_event(&created.id, &inserted.id, moved);
        assert_eq!(calendar.events(&created.id).len(), 1);
    }

    #[test]
    fn freebusy_requires_a_complete_list_of_valid_intervals() {
        let start = "2026-11-01T15:00:00Z";
        let end = "2026-11-01T16:00:00Z";
        for entry in [
            serde_json::json!({}),
            serde_json::json!({"busy": [{"start": start, "end": "invalid"}]}),
            serde_json::json!({"busy": [{"start": end, "end": start}]}),
            serde_json::json!({"busy": [{"start": start, "end": start}]}),
        ] {
            let body = serde_json::json!({"calendars": {"first": entry, "second": {"busy": []}}});
            let windows =
                super::parse_free_busy(&body.to_string(), &["first".into(), "second".into()])
                    .unwrap();
            assert!(windows[0].gap.is_some(), "{body}");
            assert!(windows[1].gap.is_none());
            assert!(windows[1].busy.is_empty());
        }
    }

    #[test]
    fn freebusy_errors_are_gaps_and_never_an_empty_busy_list() {
        let body = r#"{
            "calendars": {
                "ada@example.com": {"busy": [{"start": "2026-11-01T15:00:00Z", "end": "2026-11-01T16:00:00Z"}]},
                "personal@example.com": {"errors": [{"reason": "notFound"}]},
                "closed@example.com": {"errors": [{"reason": "forbidden"}]}
            }
        }"#;
        let windows = super::parse_free_busy(
            body,
            &[
                "ada@example.com".into(),
                "personal@example.com".into(),
                "closed@example.com".into(),
                "missing@example.com".into(),
            ],
        )
        .unwrap();
        assert!(windows[0].gap.is_none());
        assert_eq!(windows[0].busy.len(), 1);
        assert_eq!(windows[1].gap, Some(super::CalendarGap::Share));
        assert!(windows[1].busy.is_empty());
        assert_eq!(windows[2].gap, Some(super::CalendarGap::Scope));
        assert_eq!(windows[3].gap, Some(super::CalendarGap::Share));
    }

    #[tokio::test]
    async fn a_hidden_calendar_is_a_missing_share() {
        let calendars = super::FakeAttorneyCalendars::default();
        let start = DateTime::parse_from_rfc3339("2026-11-01T15:00:00Z").unwrap();
        let end = DateTime::parse_from_rfc3339("2026-11-01T16:00:00Z").unwrap();
        calendars.record("ada@example.com", start, end, Some("Call"));
        calendars.hide("personal@example.com");
        calendars.deny_scope("blocked@example.com");
        let window_end = DateTime::parse_from_rfc3339("2026-11-01T18:00:00Z").unwrap();
        let windows = calendars
            .free_busy(
                "ada@example.com",
                &["ada@example.com".into(), "personal@example.com".into()],
                start,
                window_end,
            )
            .await
            .unwrap();
        assert!(windows[0].gap.is_none());
        assert_eq!(windows[0].busy.len(), 1);
        assert_eq!(windows[1].gap, Some(super::CalendarGap::Share));
        let denied = calendars
            .free_busy(
                "blocked@example.com",
                &["blocked@example.com".into()],
                start,
                window_end,
            )
            .await
            .unwrap();
        assert_eq!(denied[0].gap, Some(super::CalendarGap::Scope));
        assert!(denied[0].busy.is_empty());
    }
}

//! Project-scoped Deadline rows (`project_deadlines`).
//!
//! A write stores the due instant the caller named. It never counts court
//! days. Complete, cancel, and reopen change `status`; they keep the row.

use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::project_deadlines::{Normalized, StoredDeadline};
pub use model::project_deadlines::{
    optional_category, AuthorityKind, CalendarRole, Category, Due, DueOn, DueOnInput, Hearing,
    SchedulingPolicy, AUTHORITY_KINDS, CALENDAR_ROLES, CATEGORIES,
};

pub const STATUS_OPEN: &str = "open";
pub const STATUS_COMPLETE: &str = "complete";
pub const STATUS_CANCELLED: &str = "cancelled";

pub const MAX_TITLE_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deadline {
    pub id: Uuid,
    pub project_id: Uuid,
    /// A short name for what is due; rows written before titles existed have none.
    pub title: Option<String>,
    pub description: Option<String>,
    pub due_at: String,
    pub due_on: Option<DueOn>,
    pub hearing: Option<Hearing>,
    #[serde(default)]
    pub effort_minutes: i32,
    pub authority_kind: String,
    pub authority: String,
    pub source: String,
    pub source_document_id: Option<Uuid>,
    pub owner_person_id: Option<Uuid>,
    pub status: String,
    pub calendar_role: String,
    /// What kind of date this is: `court`, `hearing`, `discovery`, or
    /// `internal`. Rows written before categories existed have none.
    pub category: Option<String>,
    pub internal_lead_days: Option<i64>,
    pub client_lead_days: Option<i64>,
    pub replay_key: Option<String>,
    pub inserted_at: String,
    pub updated_at: String,
    /// Monotonic RFC 5545 revision; absent on historical rows.
    pub calendar_sequence: Option<i64>,
    #[serde(default)]
    pub calendar_review_pending: bool,
}

/// One page of upcoming open Deadlines and whether another page exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadlinePage {
    pub rows: Vec<Deadline>,
    pub has_next: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum DeadlineError {
    #[error(transparent)]
    Project(#[from] crate::projects::ProjectStoreError),
    #[error(transparent)]
    Postgres(#[from] crate::postgres::matter::MatterAuthorityError),
    #[error("deadline changed concurrently; retry the operation")]
    Conflict,
    #[error("{0}")]
    Invalid(String),
    #[error("no such deadline")]
    NotFound,
    #[error("writing a deadline returned no usable row")]
    WriteReturnedNothing,
}

#[derive(Debug, Clone)]
pub struct NewDeadline {
    pub project_id: Uuid,
    pub title: Option<String>,
    pub description: Option<String>,
    pub due_at: String,
    pub due_on: Option<DueOn>,
    pub hearing: Option<Hearing>,
    pub effort_minutes: Option<i32>,
    pub authority_kind: String,
    pub authority: String,
    pub source: String,
    pub source_document_id: Option<Uuid>,
    pub owner_person_id: Option<Uuid>,
    pub calendar_role: String,
    /// What kind of date this is. When absent, creation settles it from the
    /// calendar role and authority kind and refuses when neither does.
    pub category: Option<String>,
    pub internal_lead_days: Option<i64>,
    pub client_lead_days: Option<i64>,
    pub replay_key: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct DeadlinePatch {
    /// `Some(None)` clears the title.
    pub title: Option<Option<String>>,
    pub description: Option<Option<String>>,
    pub due_at: Option<String>,
    pub due_on: Option<DueOn>,
    pub hearing: Option<Hearing>,
    pub effort_minutes: Option<i32>,
    pub authority_kind: Option<String>,
    pub authority: Option<String>,
    pub source: Option<String>,
    pub source_document_id: Option<Option<Uuid>>,
    pub owner_person_id: Option<Option<Uuid>>,
    pub calendar_role: Option<String>,
    /// `Some(None)` clears the category.
    pub category: Option<Option<String>>,
    pub internal_lead_days: Option<Option<i64>>,
    pub client_lead_days: Option<Option<i64>>,
}

#[derive(Clone, Copy)]
struct DeadlineInput<'a> {
    title: Option<&'a str>,
    description: Option<&'a str>,
    due_at: &'a str,
    due_on: Option<&'a DueOn>,
    hearing: Option<&'a Hearing>,
    effort_minutes: Option<i32>,
    authority_kind: &'a str,
    authority: &'a str,
    source: &'a str,
    calendar_role: &'a str,
    category: Option<&'a str>,
    internal_lead_days: Option<i64>,
    client_lead_days: Option<i64>,
    replay_key: Option<&'a str>,
}

fn normalized_time(
    due_at: &str,
    due_on: Option<&DueOn>,
    hearing: Option<&Hearing>,
    calendar_role: &str,
    category: Option<&str>,
) -> Result<DateTime<FixedOffset>, DeadlineError> {
    let due_at = DateTime::<FixedOffset>::parse_from_rfc3339(due_at.trim()).map_err(|_| {
        DeadlineError::Invalid(
            "due_at must be an RFC 3339 timestamp with an explicit offset".into(),
        )
    })?;
    let is_hearing = model::project_deadlines::is_appearance(
        calendar_role,
        category.and_then(|value| value.trim().parse().ok()),
    );
    if hearing.is_some() && !is_hearing {
        return Err(DeadlineError::Invalid(
            "appearance details require an appearance role or category".into(),
        ));
    }
    if matches!(category.map(str::trim), Some("trial_day" | "deposition")) && hearing.is_none() {
        return Err(DeadlineError::Invalid(
            "trial and deposition details are required".into(),
        ));
    }
    if let Some(details) = hearing {
        details
            .validate_appearance(category.and_then(|value| value.trim().parse().ok()), due_at)
            .map_err(|error| DeadlineError::Invalid(error.into()))?;
        if details.minutes() > 1440 {
            return Err(DeadlineError::Invalid(
                "an appearance may last at most 1440 minutes".into(),
            ));
        }
        let duration = chrono::Duration::minutes(i64::from(details.minutes()));
        if due_at.checked_add_signed(duration).is_none() {
            return Err(DeadlineError::Invalid(
                "hearing end is outside the supported calendar range".into(),
            ));
        }
    }
    if let Some(date) = due_on {
        let civil = date
            .instant()
            .map_err(|error| DeadlineError::Invalid(error.into()))?;
        if civil != due_at {
            return Err(DeadlineError::Invalid("due_at and due_on disagree".into()));
        }
        if is_hearing {
            return Err(DeadlineError::Invalid(
                "an appearance requires a scheduled start time".into(),
            ));
        }
    }
    Ok(due_at)
}

fn normalize(input: &DeadlineInput<'_>) -> Result<Normalized, DeadlineError> {
    let DeadlineInput {
        title,
        description,
        due_at,
        due_on,
        hearing,
        effort_minutes,
        authority_kind,
        authority,
        source,
        calendar_role,
        category,
        internal_lead_days,
        client_lead_days,
        replay_key,
    } = *input;
    let due_at = normalized_time(due_at, due_on, hearing, calendar_role, category)?;
    if !AUTHORITY_KINDS.contains(&authority_kind) {
        return Err(DeadlineError::Invalid(format!(
            "authority_kind must be one of {}",
            AUTHORITY_KINDS.join(", ")
        )));
    }
    let authority = authority.trim().to_string();
    if matches!(authority_kind, "statute" | "court_rule" | "case_law") && authority.is_empty() {
        return Err(DeadlineError::Invalid(
            "a statute or court_rule deadline requires an authority citation".into(),
        ));
    }
    if !CALENDAR_ROLES.contains(&calendar_role) {
        return Err(DeadlineError::Invalid(format!(
            "calendar_role must be one of {}",
            CALENDAR_ROLES.join(", ")
        )));
    }
    let category = category.map(str::trim).filter(|value| !value.is_empty());
    if category.is_some_and(|value| !CATEGORIES.contains(&value)) {
        return Err(DeadlineError::Invalid(format!(
            "category must be one of {}",
            CATEGORIES.join(", ")
        )));
    }
    for (name, days) in [
        ("internal_lead_days", internal_lead_days),
        ("client_lead_days", client_lead_days),
    ] {
        if days.is_some_and(|days| days < 0) {
            return Err(DeadlineError::Invalid(format!(
                "{name} must be zero or more days"
            )));
        }
    }
    if client_lead_days.is_some() && internal_lead_days.is_none() {
        return Err(DeadlineError::Invalid(
            "a client lead time requires an internal lead time".into(),
        ));
    }
    if let (Some(internal), Some(client)) = (internal_lead_days, client_lead_days) {
        if internal < client {
            return Err(DeadlineError::Invalid(
                "the internal lead time must be at least the client lead time".into(),
            ));
        }
    }
    let source = {
        let trimmed = source.trim();
        if trimmed.is_empty() {
            "lawyer".to_string()
        } else {
            trimmed.to_string()
        }
    };
    let title = title.map(str::trim).filter(|title| !title.is_empty());
    if title.is_some_and(|title| title.chars().count() > MAX_TITLE_CHARS) {
        return Err(DeadlineError::Invalid(format!(
            "title must be at most {MAX_TITLE_CHARS} characters"
        )));
    }
    let replay_key = replay_key.map(str::trim).filter(|key| !key.is_empty());
    Ok(Normalized {
        title: title.map(str::to_string),
        description: description
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        due_at: due_at.to_rfc3339(),
        due_on: due_on.cloned(),
        hearing: hearing.cloned(),
        effort_minutes,
        authority_kind: authority_kind.to_string(),
        authority,
        source,
        calendar_role: calendar_role.to_string(),
        category: category.map(str::to_string),
        internal_lead_days,
        client_lead_days,
        replay_key: replay_key.map(str::to_string),
    })
}

/// The category creation settles on when the caller names none: a hearing
/// role is a hearing date, an internal authority is an internal target, and
/// everything else is the caller's to choose — `court` and `discovery` are
/// never guessed.
fn settle_category(
    calendar_role: &str,
    authority_kind: &str,
) -> Result<Option<String>, DeadlineError> {
    if calendar_role == "hearing" {
        return Ok(Some("hearing".to_string()));
    }
    if authority_kind == "internal" {
        return Ok(Some("internal".to_string()));
    }
    Err(DeadlineError::Invalid(format!(
        "a deadline with this calendar role and authority needs a category: pass one of {}",
        CATEGORIES.join(", ")
    )))
}

async fn require_links(
    application_store: &ApplicationStore,
    project_id: Uuid,
    source_document_id: Option<Uuid>,
    owner_person_id: Option<Uuid>,
) -> Result<(), DeadlineError> {
    if crate::projects::find_by_id(application_store, project_id)
        .await
        .map_err(|error| DeadlineError::Invalid(error.to_string()))?
        .is_none()
    {
        return Err(DeadlineError::Invalid("no such Project".into()));
    }
    if let Some(document_id) = source_document_id {
        let asset = crate::assets::find_by_id(application_store, document_id)
            .await
            .map_err(|error| DeadlineError::Invalid(error.to_string()))?;
        let Some(asset) = asset else {
            return Err(DeadlineError::Invalid("no such source document".into()));
        };
        if asset.project_id != Some(project_id) {
            return Err(DeadlineError::Invalid(
                "source document is not on this Project".into(),
            ));
        }
    }
    if let Some(person_id) = owner_person_id {
        if crate::persons::find_by_id(application_store, person_id)
            .await
            .map_err(|error| DeadlineError::Invalid(error.to_string()))?
            .is_none()
        {
            return Err(DeadlineError::Invalid("no such owner".into()));
        }
    }
    Ok(())
}

/// Read the Firm's internal calendar policy independently of calendar integration status.
pub async fn scheduling_policy(
    firm_id: Option<Uuid>,
) -> Result<model::project_deadlines::SchedulingPolicy, DeadlineError> {
    use crate::postgres::matter::MatterAuthorityError as PgError;
    use model::project_deadlines::SchedulingPolicy;
    let Some(firm_id) = firm_id else {
        return Ok(SchedulingPolicy::default());
    };
    let pg = crate::postgres::matter::authority_for("project_deadlines").await?;
    let settings = sqlx::query_as::<_, (i32, String)>(
        "SELECT deadline_buffer_minutes, deadline_end_of_business FROM firm_calendar_settings WHERE firm_id = $1",
    )
    .bind(firm_id)
    .fetch_optional(pg.pool())
    .await
    .map_err(PgError::from)?;
    settings.map_or_else(
        || Ok(SchedulingPolicy::default()),
        |(buffer, end)| {
            let end = end.parse().map_err(|_| {
                DeadlineError::Invalid("invalid stored end-of-business time".into())
            })?;
            SchedulingPolicy::new(i64::from(buffer), end)
                .map_err(|error| DeadlineError::Invalid(error.into()))
        },
    )
}

impl Deadline {
    #[must_use]
    pub fn work(
        &self,
        policy: model::project_deadlines::SchedulingPolicy,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Option<model::deadline_schedule::Work> {
        if self.status != STATUS_OPEN
            || self.calendar_review_pending
            || self.calendar_role == "opponent"
            || self.effort_minutes == 0
        {
            return None;
        }
        let due = self.due_at.parse().ok()?;
        let target = if model::project_deadlines::is_appearance(
            &self.calendar_role,
            self.category
                .as_deref()
                .and_then(|value| value.parse().ok()),
        ) {
            self.buffered_window(policy)?.0
        } else {
            policy.submit_by(due, self.due_on.as_ref())?
        };
        Some(model::deadline_schedule::Work::new(
            self.id,
            target.to_utc(),
            now,
            self.internal_lead_days,
            self.effort_minutes,
            policy.buffer_minutes(),
        ))
    }

    #[must_use]
    pub fn buffered_window(
        &self,
        policy: model::project_deadlines::SchedulingPolicy,
    ) -> Option<(DateTime<FixedOffset>, DateTime<FixedOffset>)> {
        if self.status != STATUS_OPEN
            || self.calendar_review_pending
            || !model::project_deadlines::is_appearance(
                &self.calendar_role,
                self.category
                    .as_deref()
                    .and_then(|category| category.parse().ok()),
            )
        {
            return None;
        }
        let start = self.due_at.parse().ok()?;
        policy.appearance(start, self.hearing.as_ref().map_or(60, Hearing::minutes))
    }
}

/// Deadlines on one Project, earliest due instant first.
pub async fn list(
    _application_store: &ApplicationStore,
    project_id: Uuid,
) -> Result<Vec<Deadline>, DeadlineError> {
    let pg = crate::postgres::matter::authority_for("project_deadlines").await?;
    Ok(crate::postgres::matter::select(
        &pg,
        "project_deadlines",
        serde_json::json!({"project_id": project_id}),
        &["due_at", "id"],
    )
    .await?)
}

/// Open Deadlines across `project_ids`, earliest due instant first: one
/// statement for the whole workbench rather than one per Project.
pub async fn open_for_projects(project_ids: &[Uuid]) -> Result<Vec<Deadline>, DeadlineError> {
    if project_ids.is_empty() {
        return Ok(Vec::new());
    }
    let pg = crate::postgres::matter::authority_for("project_deadlines").await?;
    Ok(crate::postgres::matter::select_any(
        &pg,
        "project_deadlines",
        serde_json::json!({"status": STATUS_OPEN}),
        ("project_id", project_ids),
        &["due_at", "id"],
    )
    .await?)
}

/// All recorded Deadlines across the authorized Project ids, including
/// completed and cancelled rows used by the cross-Project status filter.
pub async fn list_for_projects(project_ids: &[Uuid]) -> Result<Vec<Deadline>, DeadlineError> {
    if project_ids.is_empty() {
        return Ok(Vec::new());
    }
    let pg = crate::postgres::matter::authority_for("project_deadlines").await?;
    Ok(crate::postgres::matter::select_any(
        &pg,
        "project_deadlines",
        serde_json::json!({}),
        ("project_id", project_ids),
        &["due_at", "id"],
    )
    .await?)
}

/// Page upcoming open Deadlines across visible Projects. One lookahead row
/// exposes a next page without counting every matching Deadline.
pub async fn upcoming_page_for_projects(
    project_ids: &[Uuid],
    today: chrono::NaiveDate,
    limit: i64,
    offset: i64,
) -> Result<DeadlinePage, DeadlineError> {
    if project_ids.is_empty() {
        return Ok(DeadlinePage {
            rows: Vec::new(),
            has_next: false,
        });
    }
    if !(1..=100).contains(&limit) || offset < 0 {
        return Err(DeadlineError::Invalid("deadline page".into()));
    }
    let pg = crate::postgres::matter::authority_for("project_deadlines").await?;
    let result: Option<serde_json::Value> = sqlx::query_scalar(
        "WITH page AS (
             SELECT to_jsonb(d) AS row, due_at, id
             FROM project_deadlines d
             WHERE status = 'open' AND project_id = ANY($1::uuid[])
               AND due_at >= ($2::date::timestamp AT TIME ZONE 'UTC')
             ORDER BY due_at, id
             LIMIT $3 OFFSET $4
         )
         SELECT jsonb_agg(row ORDER BY due_at, id) FROM page",
    )
    .bind(project_ids)
    .bind(today)
    // One lookahead row enables Next without counting the entire workload.
    .bind(limit + 1)
    .bind(offset)
    .fetch_one(pg.pool())
    .await
    .map_err(crate::postgres::matter::MatterAuthorityError::from)?;
    let mut rows: Vec<Deadline> = result
        .map(serde_json::from_value)
        .transpose()
        .map_err(crate::postgres::matter::MatterAuthorityError::from)?
        .unwrap_or_default();
    let has_next = rows.len() > usize::try_from(limit).unwrap_or(usize::MAX);
    rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    Ok(DeadlinePage { rows, has_next })
}

/// Earliest upcoming open Deadline per Project for the dashboard's compact
/// Project list. The lateral lookup stops at the first indexed row per Project.
pub async fn next_upcoming_date_by_project(
    project_ids: &[Uuid],
    today: chrono::NaiveDate,
) -> Result<std::collections::HashMap<Uuid, String>, DeadlineError> {
    if project_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let pg = crate::postgres::matter::authority_for("project_deadlines").await?;
    let rows: Vec<(Uuid, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT visible.id, next_due.due_at
         FROM unnest($1::uuid[]) AS visible(id)
         CROSS JOIN LATERAL (
             SELECT due_at
             FROM project_deadlines
             WHERE project_id = visible.id AND status = 'open'
               AND due_at >= ($2::date::timestamp AT TIME ZONE 'UTC')
             ORDER BY due_at, id
             LIMIT 1
         ) AS next_due",
    )
    .bind(project_ids)
    .bind(today)
    .fetch_all(pg.pool())
    .await
    .map_err(crate::postgres::matter::MatterAuthorityError::from)?;
    Ok(rows
        .into_iter()
        .map(|(project_id, due_at)| (project_id, due_at.date_naive().to_string()))
        .collect())
}

pub async fn find(
    _application_store: &ApplicationStore,
    project_id: Uuid,
    id: Uuid,
) -> Result<Option<Deadline>, DeadlineError> {
    let pg = crate::postgres::matter::authority_for("project_deadlines").await?;
    Ok(crate::postgres::matter::find(
        &pg,
        "project_deadlines",
        serde_json::json!({"id": id, "project_id": project_id}),
    )
    .await?)
}

async fn find_replay(
    _application_store: &ApplicationStore,
    project_id: Uuid,
    replay_key: &str,
) -> Result<Option<Deadline>, DeadlineError> {
    let pg = crate::postgres::matter::authority_for("project_deadlines").await?;
    Ok(crate::postgres::matter::find(
        &pg,
        "project_deadlines",
        serde_json::json!({"project_id": project_id, "replay_key": replay_key}),
    )
    .await?)
}

async fn persist(
    _application_store: &ApplicationStore,
    id: Uuid,
    project_id: Uuid,
    stored: &StoredDeadline<'_>,
) -> Result<Deadline, DeadlineError> {
    let pg = crate::postgres::matter::authority_for("project_deadlines").await?;
    crate::postgres::project_deadlines::persist(&pg, id, project_id, stored).await
}

/// Docket a configured claim's civil filing date once and complete it when filing is recorded.
pub(crate) async fn record_claim_filing(
    connection: &mut sqlx::PgConnection,
    project: Uuid,
    claim: Uuid,
    due_on: Option<DueOn>,
    filed: bool,
) -> Result<(), DeadlineError> {
    use crate::postgres::{matter::MatterAuthorityError as PgError, project_deadlines::persist_on};

    if due_on.is_none() && !filed {
        return Ok(());
    }
    // Match the lock order used by ordinary Deadline writes before reading its row.
    sqlx::query("SELECT pg_advisory_xact_lock(1312902739, hashtext($1::text))")
        .bind(project)
        .execute(&mut *connection)
        .await
        .map_err(PgError::from)?;
    let replay_key = format!("legal-plan-claim:{claim}");
    let existing = sqlx::query_scalar::<_, sqlx::types::Json<Deadline>>(
        "SELECT to_jsonb(d) FROM project_deadlines d
         WHERE project_id=$1 AND replay_key=$2 FOR UPDATE",
    )
    .bind(project)
    .bind(&replay_key)
    .fetch_optional(&mut *connection)
    .await
    .map_err(PgError::from)?;
    if let Some(existing) = existing {
        let deadline = existing.0;
        if filed && deadline.status == STATUS_OPEN {
            let normalized = deadline_schedule(&deadline)?;
            persist_on(
                connection,
                deadline.id,
                project,
                &StoredDeadline {
                    normalized: &normalized,
                    source_document_id: deadline.source_document_id,
                    owner_person_id: deadline.owner_person_id,
                    status: STATUS_COMPLETE,
                    expected_status: Some(STATUS_OPEN),
                    expected_updated_at: Some(&deadline.updated_at),
                    inserted_at: &deadline.inserted_at,
                },
                false,
            )
            .await?;
        }
        return Ok(());
    }
    let Some(due_on) = due_on else {
        return Ok(());
    };
    let due_at = due_on
        .instant()
        .map_err(|error| DeadlineError::Invalid(error.into()))?
        .to_rfc3339();
    let normalized = normalize(&DeadlineInput {
        title: Some("Legal Plan Claim filing"),
        description: None,
        due_at: &due_at,
        due_on: Some(&due_on),
        hearing: None,
        effort_minutes: None,
        authority_kind: "contract",
        authority: "Verified Legal Plan filing rule",
        source: "legal_plan_claim",
        calendar_role: "firm",
        category: Some("internal"),
        internal_lead_days: None,
        client_lead_days: None,
        replay_key: Some(&replay_key),
    })?;
    persist_on(
        connection,
        Uuid::now_v7(),
        project,
        &StoredDeadline {
            normalized: &normalized,
            source_document_id: None,
            owner_person_id: None,
            status: if filed { STATUS_COMPLETE } else { STATUS_OPEN },
            expected_status: None,
            expected_updated_at: None,
            inserted_at: &chrono::Utc::now().to_rfc3339(),
        },
        false,
    )
    .await?;
    Ok(())
}

/// An existing Deadline whose calendar zone survives a retry or reschedule.
#[derive(Clone, Copy, Debug)]
pub enum DeadlineIdentity<'a> {
    Id(Uuid),
    Replay(Option<&'a str>),
}

/// Freeze the Project default into a civil Deadline at the time of a scheduling write.
pub async fn resolve_due(
    application_store: &ApplicationStore,
    project_id: Uuid,
    identity: DeadlineIdentity<'_>,
    at: Option<&str>,
    on: Option<DueOnInput>,
) -> Result<(String, Option<DueOn>), DeadlineError> {
    let date = match on {
        Some(input) if at.is_none() => {
            let timezone = if let Some(zone) = input.timezone {
                zone
            } else {
                let existing = match identity {
                    DeadlineIdentity::Id(id) => find(application_store, project_id, id).await?,
                    DeadlineIdentity::Replay(Some(key)) => {
                        find_replay(application_store, project_id, key.trim()).await?
                    }
                    DeadlineIdentity::Replay(None) => None,
                };
                if let Some(saved) = existing.and_then(|row| row.due_on) {
                    saved.timezone
                } else {
                    let project = crate::projects::find_by_id(application_store, project_id)
                        .await?
                        .ok_or(DeadlineError::NotFound)?;
                    crate::projects::calendar_timezone(application_store, &project).await?
                }
            };
            Some(DueOn {
                date: input.date,
                timezone,
                cutoff: input.cutoff,
            })
        }
        Some(_) => {
            return Err(DeadlineError::Invalid(
                "supply exactly one of due_at and due_on".into(),
            ))
        }
        None => None,
    };
    Due::from_fields(at, date)
        .and_then(Due::into_fields)
        .map_err(|error| DeadlineError::Invalid(error.into()))
}

/// Create a Deadline and return the stored row. A `replay_key` that already
/// names a row on this Project updates that row instead of inserting another.
/// Cancelled rows refuse replay until explicitly reopened. Complete rows keep
/// their status while their fields update.
pub async fn create(
    application_store: &ApplicationStore,
    new: &NewDeadline,
) -> Result<Deadline, DeadlineError> {
    let mut normalized = normalize(&DeadlineInput {
        title: new.title.as_deref(),
        description: new.description.as_deref(),
        due_at: &new.due_at,
        due_on: new.due_on.as_ref(),
        hearing: new.hearing.as_ref(),
        effort_minutes: new.effort_minutes,
        authority_kind: &new.authority_kind,
        authority: &new.authority,
        source: &new.source,
        calendar_role: &new.calendar_role,
        category: new.category.as_deref(),
        internal_lead_days: new.internal_lead_days,
        client_lead_days: new.client_lead_days,
        replay_key: new.replay_key.as_deref(),
    })?;
    if normalized.category.is_none() {
        normalized.category =
            settle_category(&normalized.calendar_role, &normalized.authority_kind)?;
    }
    if normalized.category.as_deref() == Some("deposition")
        && normalized
            .hearing
            .as_ref()
            .is_none_or(|details| details.prep.is_none())
    {
        return Err(DeadlineError::Invalid(
            "review the deposition and explicitly accept or drop its preparation Deadlines".into(),
        ));
    }
    require_links(
        application_store,
        new.project_id,
        new.source_document_id,
        new.owner_person_id,
    )
    .await?;
    if let Some(key) = &normalized.replay_key {
        if let Some(existing) = find_replay(application_store, new.project_id, key).await? {
            if existing.status == STATUS_CANCELLED {
                return Err(DeadlineError::Invalid(format!(
                    "deadline {} is cancelled; use navigator project deadline reopen {} before replaying",
                    existing.id, existing.id
                )));
            }
            return persist(
                application_store,
                existing.id,
                new.project_id,
                &StoredDeadline {
                    normalized: &normalized,
                    source_document_id: new.source_document_id,
                    owner_person_id: new.owner_person_id,
                    expected_status: Some(&existing.status),
                    expected_updated_at: Some(&existing.updated_at),
                    status: &existing.status,
                    inserted_at: &existing.inserted_at,
                },
            )
            .await;
        }
    }
    // Two first attempts may both miss the replay lookup. Address the same
    // row even on a compatibility store without the PostgreSQL unique index.
    let id = normalized
        .replay_key
        .as_deref()
        .map_or_else(Uuid::now_v7, |key| {
            use sha2::{Digest, Sha256};
            let hash = Sha256::new()
                .chain_update(b"navigator/deadline-replay/v1")
                .chain_update(new.project_id.as_bytes())
                .chain_update(key.as_bytes())
                .finalize();
            let mut bytes = [0; 16];
            bytes.copy_from_slice(&hash[..16]);
            uuid::Builder::from_custom_bytes(bytes).into_uuid()
        });
    let inserted_at = chrono::Utc::now().to_rfc3339();
    persist(
        application_store,
        id,
        new.project_id,
        &StoredDeadline {
            normalized: &normalized,
            source_document_id: new.source_document_id,
            owner_person_id: new.owner_person_id,
            expected_status: None,
            expected_updated_at: None,
            status: STATUS_OPEN,
            inserted_at: &inserted_at,
        },
    )
    .await
}

/// Persist the reviewed revision using the same normalization, identity, lifecycle and
/// calendar sequence rules as an ordinary Deadline command. Its caller commits the receipt.
pub(crate) async fn publish_reviewed(
    conn: &mut sqlx::PgConnection,
    proposal: &model::deadline_proposals::Proposal,
    citations: &[String],
    reviewer: Option<Uuid>,
) -> Result<Deadline, DeadlineError> {
    use crate::postgres::matter::MatterAuthorityError as PgError;
    use model::deadline_proposals::{Kind, Role};
    let draft = &proposal.draft;
    let (due_at, due_on) = draft
        .reviewed_due()
        .and_then(Due::into_fields)
        .map_err(|e| DeadlineError::Invalid(e.into()))?;
    let previous: Option<Deadline> = if let Some(target) = proposal.deadline_id {
        let json = sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT to_jsonb(d) FROM project_deadlines d WHERE id=$1 AND project_id=$2 FOR UPDATE",
        )
        .bind(target)
        .bind(proposal.project_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(PgError::from)?
        .ok_or(DeadlineError::NotFound)?;
        Some(serde_json::from_value(json).map_err(PgError::from)?)
    } else {
        None
    };
    let prior = previous.as_ref();
    if prior.is_some_and(|d| d.status != STATUS_OPEN) {
        return Err(DeadlineError::Invalid(
            "only an open Deadline can be amended".into(),
        ));
    }
    let category =
        draft
            .category
            .unwrap_or(if draft.calendar_role == Some(CalendarRole::Hearing) {
                Category::Hearing
            } else {
                Category::Court
            });
    let category_text = category.to_string();
    let role = draft
        .calendar_role
        .unwrap_or_else(|| {
            if category.is_appearance() {
                CalendarRole::Hearing
            } else if prior.is_some_and(|row| row.calendar_role == "opponent") {
                CalendarRole::Opponent
            } else {
                CalendarRole::Firm
            }
        })
        .to_string();
    let kind = draft
        .calculation
        .as_ref()
        .map_or(AuthorityKind::CourtOrder, |r| r.authority_kind)
        .to_string();
    let authority = citations.join("; ");
    let source_role = if draft.kind == Kind::SignedOrder {
        Role::SignedOrder
    } else {
        Role::Trigger
    };
    let source_document_id = draft
        .evidence
        .iter()
        .find(|e| e.role == source_role)
        .map(|e| e.id);
    let normalized = normalize(&DeadlineInput {
        due_at: &due_at,
        due_on: due_on.as_ref(),
        hearing: draft.hearing.as_ref(),
        effort_minutes: prior.map(|row| row.effort_minutes),
        title: Some(&draft.title),
        description: prior.and_then(|d| d.description.as_deref()),
        source: prior.map_or("lawyer", |d| d.source.as_str()),
        authority_kind: &kind,
        authority: &authority,
        calendar_role: &role,
        category: Some(&category_text),
        client_lead_days: prior.and_then(|d| d.client_lead_days),
        internal_lead_days: prior.and_then(|d| d.internal_lead_days),
        replay_key: prior.and_then(|d| d.replay_key.as_deref()),
    })?;
    publish_reviewed_schedule(
        conn,
        proposal.project_id,
        proposal.deadline_id,
        normalized,
        prior,
        source_document_id,
        reviewer,
    )
    .await
}

#[allow(clippy::too_many_lines)]
async fn publish_reviewed_schedule(
    conn: &mut sqlx::PgConnection,
    project: Uuid,
    target: Option<Uuid>,
    mut normalized: Normalized,
    prior: Option<&Deadline>,
    source_document_id: Option<Uuid>,
    reviewer: Option<Uuid>,
) -> Result<Deadline, DeadlineError> {
    let now = chrono::Utc::now().to_rfc3339();
    let parent_id = target.unwrap_or_else(Uuid::now_v7);
    let old_trial = prior
        .and_then(|row| row.hearing.as_ref())
        .and_then(|details| match details.appearance {
            Some(model::project_deadlines::Appearance::TrialDay { day, .. }) => day,
            _ => None,
        });
    let mut starts = vec![normalized.due_at.clone()];
    let mut cancelled = false;
    if let Some(details) = &mut normalized.hearing {
        if let Some(model::project_deadlines::Appearance::TrialDay {
            additional_starts,
            day,
            cancel,
        }) = &mut details.appearance
        {
            if day.is_some()
                || (prior.is_some() && !additional_starts.is_empty())
                || (*cancel && old_trial.is_none())
            {
                return Err(DeadlineError::Invalid(
                    "a trial revision may amend one existing day".into(),
                ));
            }
            cancelled = *cancel;
            *cancel = false;
            starts.extend(additional_starts.drain(..).map(|start| start.to_rfc3339()));
            *day = old_trial.or(Some(model::project_deadlines::TrialDay {
                series_id: parent_id,
                number: 1,
                total: u16::try_from(starts.len())
                    .map_err(|_| DeadlineError::Invalid("too many trial days".into()))?,
            }));
        } else if old_trial.is_some() {
            return Err(DeadlineError::Invalid(
                "a trial day must retain its trial category".into(),
            ));
        }
    } else if old_trial.is_some() {
        return Err(DeadlineError::Invalid(
            "a trial day must retain its trial details".into(),
        ));
    }
    let prep = normalized
        .hearing
        .as_mut()
        .and_then(|details| details.prep.take())
        .unwrap_or_default();
    let owner = prior.and_then(|d| d.owner_person_id).or(reviewer);
    let parent = crate::postgres::project_deadlines::persist_on(
        conn,
        parent_id,
        project,
        &StoredDeadline {
            normalized: &normalized,
            source_document_id,
            owner_person_id: owner,
            status: if cancelled {
                STATUS_CANCELLED
            } else {
                STATUS_OPEN
            },
            expected_status: prior.map(|d| d.status.as_str()),
            expected_updated_at: prior.map(|d| d.updated_at.as_str()),
            inserted_at: &now,
        },
        true,
    )
    .await?;
    for (index, start) in starts.into_iter().enumerate().skip(1) {
        normalized.due_at = start;
        if let Some(model::project_deadlines::Appearance::TrialDay { day: Some(day), .. }) =
            normalized
                .hearing
                .as_mut()
                .and_then(|details| details.appearance.as_mut())
        {
            day.number = u16::try_from(index + 1)
                .map_err(|_| DeadlineError::Invalid("too many trial days".into()))?;
        }
        crate::postgres::project_deadlines::persist_on(
            conn,
            Uuid::now_v7(),
            project,
            &StoredDeadline {
                normalized: &normalized,
                source_document_id,
                owner_person_id: owner,
                status: STATUS_OPEN,
                expected_status: None,
                expected_updated_at: None,
                inserted_at: &now,
            },
            true,
        )
        .await?;
    }
    if prior.is_none() {
        crate::postgres::project_deadlines::persist_preparation(conn, &parent, &prep, &now).await?;
    }
    Ok(parent)
}

/// Replace the named fields and return the stored row. Status stays put.
pub async fn update(
    application_store: &ApplicationStore,
    project_id: Uuid,
    id: Uuid,
    patch: &DeadlinePatch,
) -> Result<Deadline, DeadlineError> {
    let existing = find(application_store, project_id, id)
        .await?
        .ok_or(DeadlineError::NotFound)?;
    let title = patch.title.clone().unwrap_or(existing.title);
    let description = patch.description.clone().unwrap_or(existing.description);
    let due_on = if patch.due_at.is_some() {
        patch.due_on.clone()
    } else {
        patch.due_on.clone().or(existing.due_on)
    };
    let due_at = patch.due_at.clone().unwrap_or(existing.due_at);
    let authority_kind = patch
        .authority_kind
        .clone()
        .unwrap_or(existing.authority_kind);
    let authority = patch.authority.clone().unwrap_or(existing.authority);
    let source = patch.source.clone().unwrap_or(existing.source);
    let calendar_role = patch
        .calendar_role
        .clone()
        .unwrap_or(existing.calendar_role);
    let category = patch.category.clone().unwrap_or(existing.category);
    let hearing = match (&patch.hearing, existing.hearing) {
        (Some(changes), Some(current)) => Some(Hearing {
            duration_minutes: changes.duration_minutes.or(current.duration_minutes),
            location: changes.location.clone().or(current.location),
            appearance: changes.appearance.clone().or(current.appearance),
            prep: current.prep,
        }),
        (Some(changes), None) => Some(changes.clone()),
        (None, current)
            if model::project_deadlines::is_appearance(
                &calendar_role,
                category.as_deref().and_then(|value| value.parse().ok()),
            ) =>
        {
            current
        }
        (None, _) => None,
    };
    let internal_lead_days = patch
        .internal_lead_days
        .unwrap_or(existing.internal_lead_days);
    let client_lead_days = patch.client_lead_days.unwrap_or(existing.client_lead_days);
    let source_document_id = patch
        .source_document_id
        .unwrap_or(existing.source_document_id);
    let owner_person_id = patch.owner_person_id.unwrap_or(existing.owner_person_id);
    let normalized = normalize(&DeadlineInput {
        title: title.as_deref(),
        description: description.as_deref(),
        due_at: &due_at,
        due_on: due_on.as_ref(),
        hearing: hearing.as_ref(),
        effort_minutes: patch.effort_minutes.or(Some(existing.effort_minutes)),
        authority_kind: &authority_kind,
        authority: &authority,
        source: &source,
        calendar_role: &calendar_role,
        category: category.as_deref(),
        internal_lead_days,
        client_lead_days,
        replay_key: existing.replay_key.as_deref(),
    })?;
    require_links(
        application_store,
        project_id,
        source_document_id,
        owner_person_id,
    )
    .await?;
    persist(
        application_store,
        existing.id,
        project_id,
        &StoredDeadline {
            normalized: &normalized,
            source_document_id,
            owner_person_id,
            expected_status: Some(&existing.status),
            expected_updated_at: Some(&existing.updated_at),
            status: &existing.status,
            inserted_at: &existing.inserted_at,
        },
    )
    .await
}

fn deadline_schedule(existing: &Deadline) -> Result<Normalized, DeadlineError> {
    normalize(&DeadlineInput {
        title: existing.title.as_deref(),
        description: existing.description.as_deref(),
        due_at: &existing.due_at,
        due_on: existing.due_on.as_ref(),
        hearing: existing.hearing.as_ref(),
        effort_minutes: Some(existing.effort_minutes),
        authority_kind: &existing.authority_kind,
        authority: &existing.authority,
        source: &existing.source,
        calendar_role: &existing.calendar_role,
        category: existing.category.as_deref(),
        internal_lead_days: existing.internal_lead_days,
        client_lead_days: existing.client_lead_days,
        replay_key: existing.replay_key.as_deref(),
    })
}

async fn transition(
    application_store: &ApplicationStore,
    project_id: Uuid,
    id: Uuid,
    next: &str,
) -> Result<Deadline, DeadlineError> {
    let existing = find(application_store, project_id, id)
        .await?
        .ok_or(DeadlineError::NotFound)?;
    if existing.category.as_deref() == Some("trial_day")
        && existing.status != next
        && matches!(next, STATUS_CANCELLED | STATUS_OPEN)
    {
        return Err(DeadlineError::Invalid(
            "amend the trial day through a Deadline Proposal with the source order or stipulation"
                .into(),
        ));
    }
    if existing.category.as_deref() == Some("trial_day")
        && next == STATUS_COMPLETE
        && existing.status == STATUS_OPEN
        && existing
            .due_at
            .parse::<DateTime<FixedOffset>>()
            .is_ok_and(|date| date > chrono::Utc::now())
    {
        return Err(DeadlineError::Invalid(
            "a future trial day requires reviewed evidence to remove".into(),
        ));
    }
    let allowed = existing.status == next
        || (existing.status == STATUS_OPEN && next != STATUS_OPEN)
        || (existing.status == STATUS_CANCELLED && next == STATUS_OPEN);
    if !allowed {
        return Err(DeadlineError::Invalid(format!(
            "cannot move a {} deadline to {next}",
            existing.status
        )));
    }
    if existing.status == next {
        return Ok(existing);
    }
    let normalized = deadline_schedule(&existing)?;
    persist(
        application_store,
        existing.id,
        project_id,
        &StoredDeadline {
            normalized: &normalized,
            source_document_id: existing.source_document_id,
            owner_person_id: existing.owner_person_id,
            expected_status: Some(&existing.status),
            expected_updated_at: Some(&existing.updated_at),
            status: next,
            inserted_at: &existing.inserted_at,
        },
    )
    .await
}

pub async fn complete(
    application_store: &ApplicationStore,
    project_id: Uuid,
    id: Uuid,
) -> Result<Deadline, DeadlineError> {
    transition(application_store, project_id, id, STATUS_COMPLETE).await
}

pub async fn cancel(
    application_store: &ApplicationStore,
    project_id: Uuid,
    id: Uuid,
) -> Result<Deadline, DeadlineError> {
    transition(application_store, project_id, id, STATUS_CANCELLED).await
}

/// Restore the same cancelled row to open. Open rows are unchanged; complete
/// rows cannot be reopened.
pub async fn reopen(
    application_store: &ApplicationStore,
    project_id: Uuid,
    id: Uuid,
) -> Result<Deadline, DeadlineError> {
    transition(application_store, project_id, id, STATUS_OPEN).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::{create as create_project, NewProject};
    use crate::test_support::{self};

    fn sample(project_id: Uuid) -> NewDeadline {
        NewDeadline {
            project_id,
            title: None,
            description: None,
            hearing: None,
            effort_minutes: None,
            due_on: None,
            due_at: "2026-11-02T17:00:00-08:00".into(),
            authority_kind: "statute".into(),
            authority: "15 U.S.C. § 1681i(a)(1)".into(),
            source: "lawyer".into(),
            source_document_id: None,
            owner_person_id: None,
            calendar_role: "firm".into(),
            category: Some("court".into()),
            internal_lead_days: Some(14),
            client_lead_days: Some(7),
            replay_key: None,
        }
    }

    async fn pg_project(application_store: &ApplicationStore) -> Uuid {
        test_support::seed_compiled_brands_postgres(application_store).await;
        let entity_id = test_support::seed_entity(application_store).await;
        create_project(
            application_store,
            &NewProject {
                code: format!("deadline-{}", Uuid::now_v7()),
                name: "Deadline matter".into(),
                status: "open".into(),
                entity_id,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .id
    }

    // Select the embedded Source for the same application commands used by callers.

    async fn pg_create(
        db: &ApplicationStore,
        input: &NewDeadline,
    ) -> Result<Deadline, DeadlineError> {
        super::create(db, input).await
    }

    async fn pg_update(
        db: &ApplicationStore,
        project: Uuid,
        id: Uuid,
        patch: &DeadlinePatch,
    ) -> Result<Deadline, DeadlineError> {
        super::update(db, project, id, patch).await
    }

    async fn pg_find(
        db: &ApplicationStore,
        project: Uuid,
        id: Uuid,
    ) -> Result<Option<Deadline>, DeadlineError> {
        super::find(db, project, id).await
    }

    async fn pg_list(db: &ApplicationStore, project: Uuid) -> Result<Vec<Deadline>, DeadlineError> {
        super::list(db, project).await
    }

    #[tokio::test]
    async fn effort_round_trips_and_overlapping_appearance_buffers_are_refused() {
        test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let project = pg_project(&db).await;
            let mut filing = sample(project);
            filing.category = Some("court".into());
            let filing = pg_create(&db, &filing).await.unwrap();
            assert_eq!(filing.effort_minutes, 360);
            let no_work = pg_update(
                &db,
                project,
                filing.id,
                &DeadlinePatch {
                    effort_minutes: Some(0),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            assert_eq!(no_work.effort_minutes, 0);

            let mut call = sample(project);
            call.authority_kind = "internal".into();
            call.category = Some("call".into());
            call.title = Some("Synthetic first call".into());
            call.due_at = "2028-07-12T10:00:00-07:00".into();
            pg_create(&db, &call).await.unwrap();
            call.title = Some("Synthetic second call".into());
            call.due_at = "2028-07-12T11:15:00-07:00".into();
            let error = pg_create(&db, &call).await.unwrap_err();
            assert!(error.to_string().contains("Synthetic first call"));
            call.due_at = "2028-07-12T11:30:00-07:00".into();
            pg_create(&db, &call).await.unwrap();
        })
        .await;
    }

    #[tokio::test]
    async fn create_list_update_and_complete_round_trip() {
        test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let project_id = pg_project(&application_store).await;
            let created = pg_create(&application_store, &sample(project_id))
                .await
                .unwrap();
            assert_eq!(created.status, STATUS_OPEN);
            assert_eq!(
                DateTime::parse_from_rfc3339(&created.due_at).unwrap(),
                DateTime::parse_from_rfc3339("2026-11-02T17:00:00-08:00").unwrap()
            );
            assert_eq!(created.calendar_role, "firm");
            let listed = pg_list(&application_store, project_id).await.unwrap();
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0].id, created.id);
            let updated = pg_update(
                &application_store,
                project_id,
                created.id,
                &DeadlinePatch {
                    calendar_role: Some("opponent".into()),
                    due_at: Some("2026-12-01T00:00:00Z".into()),
                    ..DeadlinePatch::default()
                },
            )
            .await
            .unwrap();
            assert_eq!(updated.calendar_role, "opponent");
            assert_eq!(updated.status, STATUS_OPEN);
            assert_eq!(updated.id, created.id);
            let completed = complete(&application_store, project_id, created.id)
                .await
                .unwrap();
            assert_eq!(completed.status, STATUS_COMPLETE);
            assert_eq!(
                pg_list(&application_store, project_id).await.unwrap()[0].status,
                STATUS_COMPLETE
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_create_without_a_settleable_category_is_refused() {
        test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let project_id = pg_project(&application_store).await;
            let mut input = sample(project_id);
            input.category = None;
            let error = pg_create(&application_store, &input).await.unwrap_err();
            assert!(matches!(error, DeadlineError::Invalid(_)), "{error}");
            assert!(error.to_string().contains("category"), "{error}");
            assert!(pg_list(&application_store, project_id)
                .await
                .unwrap()
                .is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn an_unknown_category_is_refused() {
        test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let project_id = pg_project(&application_store).await;
            let mut input = sample(project_id);
            input.category = Some("filing".into());
            let error = pg_create(&application_store, &input).await.unwrap_err();
            assert!(matches!(error, DeadlineError::Invalid(_)), "{error}");
            assert!(
                error
                    .to_string()
                    .contains("court, hearing, discovery, internal"),
                "{error}"
            );
            // Whitespace is trimmed before the vocabulary check, and an empty
            // value means "unsettled", so the default rule answers it.
            let mut trimmed = sample(project_id);
            trimmed.calendar_role = "hearing".into();
            trimmed.category = Some("  hearing  ".into());
            assert_eq!(
                pg_create(&application_store, &trimmed)
                    .await
                    .unwrap()
                    .category,
                Some("hearing".into())
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_hearing_role_settles_a_hearing_category() {
        test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let project_id = pg_project(&application_store).await;
            let mut input = sample(project_id);
            input.calendar_role = "hearing".into();
            input.category = None;
            assert_eq!(
                pg_create(&application_store, &input)
                    .await
                    .unwrap()
                    .category,
                Some("hearing".into())
            );
        })
        .await;
    }

    #[tokio::test]
    async fn an_internal_authority_settles_an_internal_category() {
        test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let project_id = pg_project(&application_store).await;
            let mut input = sample(project_id);
            input.authority_kind = "internal".into();
            input.authority = "Firm preparation target".into();
            input.category = None;
            let internal = pg_create(&application_store, &input).await.unwrap();
            assert_eq!(internal.category.as_deref(), Some("internal"));
            let promotion = DeadlinePatch {
                category: Some(Some("court".into())),
                ..DeadlinePatch::default()
            };
            assert!(matches!(
                pg_update(&application_store, project_id, internal.id, &promotion).await,
                Err(DeadlineError::Invalid(_))
            ));
            assert_eq!(
                pg_find(&application_store, project_id, internal.id)
                    .await
                    .unwrap(),
                Some(internal)
            );
        })
        .await;
    }

    #[tokio::test]
    async fn update_preserves_the_category_until_it_is_cleared_or_changed() {
        test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let project_id = pg_project(&application_store).await;
            let created = pg_create(&application_store, &sample(project_id))
                .await
                .unwrap();
            // A patch that names no category leaves the stored one alone.
            let untouched = pg_update(
                &application_store,
                project_id,
                created.id,
                &DeadlinePatch::default(),
            )
            .await
            .unwrap();
            assert_eq!(untouched.category, Some("court".into()));
            let renamed = pg_update(
                &application_store,
                project_id,
                created.id,
                &DeadlinePatch {
                    category: Some(Some("discovery".into())),
                    ..DeadlinePatch::default()
                },
            )
            .await
            .unwrap();
            assert_eq!(renamed.category, Some("discovery".into()));
            let cleared = pg_update(
                &application_store,
                project_id,
                created.id,
                &DeadlinePatch {
                    category: Some(None),
                    ..DeadlinePatch::default()
                },
            )
            .await
            .unwrap();
            assert_eq!(cleared.category, None);
        })
        .await;
    }

    #[tokio::test]
    async fn a_historical_row_without_a_category_reads_and_updates() {
        test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let project_id = pg_project(&application_store).await;
            let created = pg_create(&application_store, &sample(project_id)).await.unwrap();
            let pg = crate::postgres::PostgresDb::application().await.unwrap();
            sqlx::query(
                "UPDATE project_deadlines SET category=NULL, calendar_review_pending=false WHERE id=$1",
            )
                .bind(created.id)
                .execute(pg.pool())
                .await
                .unwrap();
            let row = pg_find(&application_store, project_id, created.id)
                .await
                .unwrap()
                .expect("row still reads");
            assert_eq!(row.category, None);
            // A historical row still updates: an absent patch preserves its lack
            // of a category rather than inventing one.
            let untouched = pg_update(&application_store, project_id, created.id, &DeadlinePatch::default())
                .await
                .unwrap();
            assert_eq!(untouched.category, None);
            let reschedule = DeadlinePatch {
                due_at: Some("2027-02-01T12:00:00Z".into()),
                ..DeadlinePatch::default()
            };
            assert!(matches!(
                pg_update(&application_store, project_id, created.id, &reschedule).await,
                Err(DeadlineError::Invalid(_))
            ));
            assert_eq!(
                pg_find(&application_store, project_id, created.id).await.unwrap(),
                Some(untouched)
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_statute_without_a_citation_is_refused() {
        test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let project_id = pg_project(&application_store).await;
            let mut input = sample(project_id);
            input.authority = "  ".into();
            let error = pg_create(&application_store, &input).await.unwrap_err();
            assert!(matches!(error, DeadlineError::Invalid(_)), "{error}");
            assert!(pg_list(&application_store, project_id)
                .await
                .unwrap()
                .is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn a_shorter_internal_lead_is_refused() {
        test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let project_id = pg_project(&application_store).await;
            let mut input = sample(project_id);
            input.internal_lead_days = Some(1);
            input.client_lead_days = Some(5);
            assert!(matches!(
                pg_create(&application_store, &input).await.unwrap_err(),
                DeadlineError::Invalid(_)
            ));
        })
        .await;
    }

    #[tokio::test]
    async fn a_date_without_an_offset_is_refused() {
        test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let project_id = pg_project(&application_store).await;
            let mut input = sample(project_id);
            input.due_at = "2026-11-02".into();
            assert!(matches!(
                pg_create(&application_store, &input).await.unwrap_err(),
                DeadlineError::Invalid(_)
            ));
        })
        .await;
    }

    #[tokio::test]
    async fn replay_key_updates_the_same_row() {
        test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let project_id = pg_project(&application_store).await;
            let mut input = sample(project_id);
            input.replay_key = Some("notice-1".into());
            let (first, second) = tokio::join!(
                pg_create(&application_store, &input),
                pg_create(&application_store, &input)
            );
            let rows = pg_list(&application_store, project_id).await.unwrap();
            assert_eq!(rows.len(), 1);
            // A transaction conflict can require one more read-back.
            assert!(first.is_ok() || second.is_ok());
            for row in [first, second].into_iter().flatten() {
                assert_eq!(row.id, rows[0].id);
            }
            input.due_at = "2026-12-15T12:00:00+00:00".into();
            let updated = pg_create(&application_store, &input).await.unwrap();
            assert_eq!(updated.id, rows[0].id);
            assert_eq!(updated.due_at, input.due_at);
            assert_eq!(
                pg_list(&application_store, project_id).await.unwrap(),
                vec![updated]
            );
        })
        .await;
    }

    #[tokio::test]
    async fn cancelled_replay_is_refused_without_changing_the_row() {
        test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let project_id = pg_project(&application_store).await;
            let mut input = sample(project_id);
            input.replay_key = Some("notice-1".into());
            let created = pg_create(&application_store, &input).await.unwrap();
            let cancelled = cancel(&application_store, project_id, created.id)
                .await
                .unwrap();
            input.due_at = "2026-12-15T12:00:00Z".into();
            let error = pg_create(&application_store, &input).await.unwrap_err();
            assert!(error.to_string().contains("cancelled"), "{error}");
            assert!(error.to_string().contains("reopen"), "{error}");
            assert_eq!(
                pg_find(&application_store, project_id, created.id)
                    .await
                    .unwrap(),
                Some(cancelled)
            );
            assert_eq!(
                pg_list(&application_store, project_id).await.unwrap().len(),
                1
            );
        })
        .await;
    }

    #[tokio::test]
    async fn reopen_preserves_identity_and_fields_and_is_idempotent() {
        test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let project_id = pg_project(&application_store).await;
            let other = pg_project(&application_store).await;
            let mut input = sample(project_id);
            input.replay_key = Some("notice-1".into());
            let created = pg_create(&application_store, &input).await.unwrap();
            let cancelled = cancel(&application_store, project_id, created.id)
                .await
                .unwrap();
            assert!(matches!(
                reopen(&application_store, other, created.id).await,
                Err(DeadlineError::NotFound)
            ));
            assert_eq!(
                pg_find(&application_store, project_id, created.id)
                    .await
                    .unwrap(),
                Some(cancelled)
            );
            let reopened = reopen(&application_store, project_id, created.id)
                .await
                .unwrap();
            let mut expected = created.clone();
            expected.updated_at = reopened.updated_at.clone();
            expected.calendar_sequence = Some(2);
            assert_eq!(reopened, expected);
            assert_eq!(
                reopen(&application_store, project_id, created.id)
                    .await
                    .unwrap(),
                reopened
            );
            input.due_at = "2026-12-15T12:00:00Z".into();
            let replayed = pg_create(&application_store, &input).await.unwrap();
            assert_eq!(replayed.id, created.id);
            assert_eq!(replayed.status, STATUS_OPEN);
            assert_eq!(replayed.due_at, input.due_at.replace('Z', "+00:00"));
            input.project_id = other;
            let foreign = pg_create(&application_store, &input).await.unwrap();
            assert_ne!(foreign.id, created.id);
            assert_eq!(
                pg_list(&application_store, project_id).await.unwrap().len(),
                1
            );
        })
        .await;
    }

    #[tokio::test]
    async fn completed_replay_keeps_status_and_cannot_reopen() {
        test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let project_id = pg_project(&application_store).await;
            let mut input = sample(project_id);
            input.replay_key = Some("notice-1".into());
            let created = pg_create(&application_store, &input).await.unwrap();
            complete(&application_store, project_id, created.id)
                .await
                .unwrap();
            input.due_at = "2026-12-15T12:00:00Z".into();
            let replayed = pg_create(&application_store, &input).await.unwrap();
            assert_eq!(replayed.id, created.id);
            assert_eq!(replayed.status, STATUS_COMPLETE);
            assert_eq!(replayed.inserted_at, created.inserted_at);
            assert_eq!(replayed.due_at, input.due_at.replace('Z', "+00:00"));
            assert!(matches!(
                reopen(&application_store, project_id, created.id).await,
                Err(DeadlineError::Invalid(_))
            ));
            assert_eq!(
                pg_find(&application_store, project_id, created.id)
                    .await
                    .unwrap(),
                Some(replayed)
            );
        })
        .await;
    }

    #[tokio::test]
    async fn cancel_keeps_the_row() {
        test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let project_id = pg_project(&application_store).await;
            let created = pg_create(&application_store, &sample(project_id))
                .await
                .unwrap();
            let cancelled = cancel(&application_store, project_id, created.id)
                .await
                .unwrap();
            assert_eq!(cancelled.status, STATUS_CANCELLED);
            assert_eq!(
                pg_list(&application_store, project_id).await.unwrap().len(),
                1
            );
            assert!(matches!(
                complete(&application_store, project_id, created.id)
                    .await
                    .unwrap_err(),
                DeadlineError::Invalid(_)
            ));
        })
        .await;
    }

    #[tokio::test]
    async fn a_title_round_trips_updates_and_clears() {
        test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let project_id = pg_project(&application_store).await;
            let mut input = sample(project_id);
            input.title = Some("  Answer to the complaint ".into());
            let created = pg_create(&application_store, &input).await.unwrap();
            assert_eq!(created.title.as_deref(), Some("Answer to the complaint"));
            let renamed = pg_update(
                &application_store,
                project_id,
                created.id,
                &DeadlinePatch {
                    title: Some(Some("Reply brief".into())),
                    ..DeadlinePatch::default()
                },
            )
            .await
            .unwrap();
            assert_eq!(renamed.title.as_deref(), Some("Reply brief"));
            let kept = complete(&application_store, project_id, created.id)
                .await
                .unwrap();
            assert_eq!(kept.title.as_deref(), Some("Reply brief"));
            let cleared = pg_update(
                &application_store,
                project_id,
                created.id,
                &DeadlinePatch {
                    title: Some(None),
                    ..DeadlinePatch::default()
                },
            )
            .await
            .unwrap();
            assert_eq!(cleared.title, None);
        })
        .await;
    }

    #[tokio::test]
    async fn descriptions_round_trip_and_clear_without_changing_visibility() {
        test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let project_id = pg_project(&application_store).await;
            let mut input = sample(project_id);
            input.description = Some("  Synthetic preparation instructions  ".into());
            let created = pg_create(&application_store, &input).await.unwrap();
            assert_eq!(
                created.description.as_deref(),
                Some("Synthetic preparation instructions")
            );
            let changed = pg_update(
                &application_store,
                project_id,
                created.id,
                &DeadlinePatch {
                    description: Some(Some("Revised instructions".into())),
                    ..DeadlinePatch::default()
                },
            )
            .await
            .unwrap();
            assert_eq!(changed.description.as_deref(), Some("Revised instructions"));
            assert_eq!(changed.client_lead_days, input.client_lead_days);
            let cleared = pg_update(
                &application_store,
                project_id,
                created.id,
                &DeadlinePatch {
                    description: Some(Some(String::new())),
                    ..DeadlinePatch::default()
                },
            )
            .await
            .unwrap();
            assert_eq!(cleared.description, None);
        })
        .await;
    }

    #[tokio::test]
    async fn an_overlong_title_is_refused() {
        test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let project_id = pg_project(&application_store).await;
            let mut input = sample(project_id);
            input.title = Some("x".repeat(MAX_TITLE_CHARS + 1));
            assert!(matches!(
                pg_create(&application_store, &input).await.unwrap_err(),
                DeadlineError::Invalid(_)
            ));
        })
        .await;
    }

    #[tokio::test]
    async fn a_row_written_without_a_title_still_reads() {
        test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let project_id = pg_project(&application_store).await;
            let id = Uuid::now_v7();
            let pg = crate::postgres::PostgresDb::application().await.unwrap();
            sqlx::query(
                "INSERT INTO project_deadlines \
                 (id, project_id, due_at, authority_kind, authority, source, status, calendar_role, inserted_at, updated_at) \
                 VALUES ($1, $2, $3::timestamptz, 'internal', '', 'lawyer', 'open', 'firm', $4::timestamptz, $4::timestamptz)",
            )
                .bind(id)
                .bind(project_id)
                .bind("2027-04-05T12:00:00+00:00")
                .bind("2026-10-15T12:00:00Z")
                .execute(pg.pool())
                .await
                .unwrap();
            let row = pg_find(&application_store, project_id, id).await.unwrap().unwrap();
            assert_eq!(row.title, None);
            assert_eq!(row.description, None);
            assert_eq!(row.calendar_sequence, None);
            assert_eq!(pg_list(&application_store, project_id).await.unwrap().len(), 1);

        }).await;
    }
}

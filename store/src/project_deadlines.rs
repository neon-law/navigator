//! Project-scoped Deadline rows (`project_deadlines`).
//!
//! A write stores the due instant the caller named. It never counts court
//! days. Complete, cancel, and reopen change `status`; they keep the row.

use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::postgres::project_deadlines::{Normalized, StoredDeadline};
use crate::surreal::{record_id, record_uuid, SurrealDb};
pub use model::project_deadlines::{
    optional_category, AuthorityKind, CalendarRole, Category, Due, DueOn, DueOnInput, Hearing,
    AUTHORITY_KINDS, CALENDAR_ROLES, CATEGORIES,
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

#[derive(SurrealValue)]
struct DeadlineRow {
    id: surrealdb::types::RecordId,
    project_id: surrealdb::types::RecordId,
    title: Option<String>,
    description: Option<String>,
    due_at: String,
    due_on: Option<serde_json::Value>,
    hearing: Option<serde_json::Value>,
    authority_kind: String,
    authority: String,
    source: String,
    source_document_id: Option<surrealdb::types::RecordId>,
    owner_person_id: Option<surrealdb::types::RecordId>,
    status: String,
    calendar_role: String,
    category: Option<String>,
    internal_lead_days: Option<i64>,
    client_lead_days: Option<i64>,
    replay_key: Option<String>,
    inserted_at: String,
    updated_at: String,
    calendar_sequence: Option<i64>,
    calendar_review_pending: Option<bool>,
}

impl DeadlineRow {
    fn into_deadline(self) -> Result<Deadline, DeadlineError> {
        Ok(Deadline {
            id: record_uuid(&self.id).ok_or(DeadlineError::WriteReturnedNothing)?,
            project_id: record_uuid(&self.project_id).ok_or(DeadlineError::WriteReturnedNothing)?,
            title: self.title,
            description: self.description,
            due_at: self.due_at,
            due_on: self
                .due_on
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| DeadlineError::Invalid(format!("unreadable due_on: {error}")))?,
            hearing: self
                .hearing
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| DeadlineError::Invalid(format!("unreadable hearing: {error}")))?,
            authority_kind: self.authority_kind,
            authority: self.authority,
            source: self.source,
            source_document_id: self.source_document_id.as_ref().and_then(record_uuid),
            owner_person_id: self.owner_person_id.as_ref().and_then(record_uuid),
            status: self.status,
            calendar_role: self.calendar_role,
            category: self.category,
            internal_lead_days: self.internal_lead_days,
            client_lead_days: self.client_lead_days,
            replay_key: self.replay_key,
            inserted_at: self.inserted_at,
            updated_at: self.updated_at,
            calendar_sequence: self.calendar_sequence,
            calendar_review_pending: self.calendar_review_pending.unwrap_or(false),
        })
    }
}

const SELECT: &str =
    "id, project_id, title, description, due_at, due_on, hearing, authority_kind, authority, source, \
     source_document_id, owner_person_id, status, calendar_role, category, \
     internal_lead_days, client_lead_days, replay_key, inserted_at, updated_at, calendar_sequence, calendar_review_pending";

#[derive(Debug, thiserror::Error)]
pub enum DeadlineError {
    #[error(transparent)]
    Project(#[from] crate::projects::ProjectStoreError),
    #[error(transparent)]
    Postgres(#[from] crate::postgres::matter::MatterAuthorityError),
    #[error("deadline changed concurrently; retry the operation")]
    Conflict,
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
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
    let is_hearing = calendar_role == "hearing" || category.map(str::trim) == Some("hearing");
    if hearing.is_some() && !is_hearing {
        return Err(DeadlineError::Invalid(
            "hearing details require a hearing role or category".into(),
        ));
    }
    if let Some(details) = hearing {
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
                "a hearing requires a scheduled start time".into(),
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
    surreal: &SurrealDb,
    project_id: Uuid,
    source_document_id: Option<Uuid>,
    owner_person_id: Option<Uuid>,
) -> Result<(), DeadlineError> {
    if crate::projects::find_by_id(surreal, project_id)
        .await
        .map_err(|error| DeadlineError::Invalid(error.to_string()))?
        .is_none()
    {
        return Err(DeadlineError::Invalid("no such Project".into()));
    }
    if let Some(document_id) = source_document_id {
        let asset = crate::assets::find_by_id(surreal, document_id)
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
        if crate::persons::find_by_id(surreal, person_id)
            .await
            .map_err(|error| DeadlineError::Invalid(error.to_string()))?
            .is_none()
        {
            return Err(DeadlineError::Invalid("no such owner".into()));
        }
    }
    Ok(())
}

/// Deadlines on one Project, earliest due instant first.
pub async fn list(surreal: &SurrealDb, project_id: Uuid) -> Result<Vec<Deadline>, DeadlineError> {
    if let Some(pg) = crate::postgres::matter::authority_for("project_deadlines").await? {
        return Ok(crate::postgres::matter::select(
            &pg,
            "project_deadlines",
            serde_json::json!({"project_id": project_id}),
            &["due_at", "id"],
        )
        .await?);
    }

    let mut response = surreal
        .query(format!(
            "SELECT {SELECT} FROM project_deadlines WHERE project_id = $project_id \
             ORDER BY due_at, id"
        ))
        .bind(("project_id", record_id("project", project_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<DeadlineRow> = response.take(0)?;
    rows.into_iter().map(DeadlineRow::into_deadline).collect()
}

/// Open Deadlines across `project_ids`, earliest due instant first: one
/// statement for the whole workbench rather than one per Project.
pub async fn open_for_projects(project_ids: &[Uuid]) -> Result<Vec<Deadline>, DeadlineError> {
    if project_ids.is_empty() {
        return Ok(Vec::new());
    }
    let pg = crate::postgres::matter::authority_for("project_deadlines")
        .await?
        .ok_or(crate::postgres::matter::MatterAuthorityError::Unavailable)?;
    Ok(crate::postgres::matter::select_any(
        &pg,
        "project_deadlines",
        serde_json::json!({"status": STATUS_OPEN}),
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
    let pg = crate::postgres::matter::authority_for("project_deadlines")
        .await?
        .ok_or(crate::postgres::matter::MatterAuthorityError::Unavailable)?;
    let result: Option<serde_json::Value> = sqlx::query_scalar(
        "WITH page AS (
             SELECT to_jsonb(d) AS row,
                    COALESCE(
                        (due_on->>'date')::date,
                        (due_at AT TIME ZONE 'UTC')::date
                    ) AS sort_date,
                    due_at, id
             FROM project_deadlines d
             WHERE status = 'open' AND project_id = ANY($1::uuid[])
               AND COALESCE(
                   (due_on->>'date')::date,
                   (due_at AT TIME ZONE 'UTC')::date
               ) >= $2
             ORDER BY sort_date, due_at, id
             LIMIT $3 OFFSET $4
         )
         SELECT jsonb_agg(row ORDER BY sort_date, due_at, id) FROM page",
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
    let pg = crate::postgres::matter::authority_for("project_deadlines")
        .await?
        .ok_or(crate::postgres::matter::MatterAuthorityError::Unavailable)?;
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT visible.id, next_due.due_date
         FROM unnest($1::uuid[]) AS visible(id)
         CROSS JOIN LATERAL (
             SELECT COALESCE(
                 due_on->>'date',
                 ((due_at AT TIME ZONE 'UTC')::date)::text
             ) AS due_date
             FROM project_deadlines
             WHERE project_id = visible.id AND status = 'open'
               AND COALESCE(
                   (due_on->>'date')::date,
                   (due_at AT TIME ZONE 'UTC')::date
               ) >= $2
             ORDER BY COALESCE(
                 (due_on->>'date')::date,
                 (due_at AT TIME ZONE 'UTC')::date
             ), due_at, id
             LIMIT 1
         ) AS next_due",
    )
    .bind(project_ids)
    .bind(today)
    .fetch_all(pg.pool())
    .await
    .map_err(crate::postgres::matter::MatterAuthorityError::from)?;
    Ok(rows.into_iter().collect())
}

pub async fn find(
    surreal: &SurrealDb,
    project_id: Uuid,
    id: Uuid,
) -> Result<Option<Deadline>, DeadlineError> {
    if let Some(pg) = crate::postgres::matter::authority_for("project_deadlines").await? {
        return Ok(crate::postgres::matter::find(
            &pg,
            "project_deadlines",
            serde_json::json!({"id": id, "project_id": project_id}),
        )
        .await?);
    }

    let mut response = surreal
        .query(format!(
            "SELECT {SELECT} FROM ONLY project_deadlines \
             WHERE id = $id AND project_id = $project_id LIMIT 1"
        ))
        .bind(("id", record_id("project_deadlines", id)))
        .bind(("project_id", record_id("project", project_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let row: Option<DeadlineRow> = response.take(0)?;
    row.map(DeadlineRow::into_deadline).transpose()
}

async fn find_replay(
    surreal: &SurrealDb,
    project_id: Uuid,
    replay_key: &str,
) -> Result<Option<Deadline>, DeadlineError> {
    if let Some(pg) = crate::postgres::matter::authority_for("project_deadlines").await? {
        return Ok(crate::postgres::matter::find(
            &pg,
            "project_deadlines",
            serde_json::json!({"project_id": project_id, "replay_key": replay_key}),
        )
        .await?);
    }

    let mut response = surreal
        .query(format!(
            "SELECT {SELECT} FROM ONLY project_deadlines \
             WHERE project_id = $project_id AND replay_key = $replay_key LIMIT 1"
        ))
        .bind(("project_id", record_id("project", project_id)))
        .bind(("replay_key", replay_key.to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let row: Option<DeadlineRow> = response.take(0)?;
    row.map(DeadlineRow::into_deadline).transpose()
}

async fn persist(
    surreal: &SurrealDb,
    id: Uuid,
    project_id: Uuid,
    stored: &StoredDeadline<'_>,
) -> Result<Deadline, DeadlineError> {
    if let Some(pg) = crate::postgres::matter::authority_for("project_deadlines").await? {
        return crate::postgres::project_deadlines::persist(&pg, id, project_id, stored).await;
    }

    let StoredDeadline {
        normalized,
        source_document_id,
        owner_person_id,
        status,
        expected_status: _,
        expected_updated_at: _,
        inserted_at,
    } = *stored;
    let old = find(surreal, project_id, id).await?;
    stored.check_revision(old.as_ref())?;
    if old.is_some() && stored.expected_updated_at.is_none() {
        return Err(DeadlineError::Conflict);
    }
    let same_due = old.as_ref().is_some_and(|row| {
        DateTime::parse_from_rfc3339(&row.due_at).ok()
            == DateTime::parse_from_rfc3339(&normalized.due_at).ok()
    });
    let pending =
        crate::postgres::project_deadlines::review_pending(old.as_ref(), stored, same_due, false)?;
    // UPDATE cannot resurrect a deleted row. A first writer only inserts if the
    // deterministic replay id is still unused.
    let operation = if old.is_some() { "UPDATE" } else { "UPSERT" };
    let now = chrono::Utc::now().to_rfc3339();
    let mut response = surreal
        .query(format!(
            "{operation} $id SET project_id = $project_id, title = $title, description = $description, due_at = $due_at, due_on = $due_on, hearing = $hearing, \
             authority_kind = $authority_kind, authority = $authority, source = $source, \
             source_document_id = $source_document_id, owner_person_id = $owner_person_id, \
             status = $status, calendar_role = $calendar_role, category = $category, \
             internal_lead_days = $internal_lead_days, client_lead_days = $client_lead_days, \
             replay_key = $replay_key, calendar_review_pending = $pending, calendar_sequence = IF updated_at IS NONE THEN 0 ELSE \
             (calendar_sequence ?? math::max([0, time::unix(<datetime>updated_at)])) + 1 END, \
             inserted_at = $inserted_at, updated_at = $now WHERE updated_at = $observed \
             RETURN {SELECT}"
        ))
        .bind(("id", record_id("project_deadlines", id)))
        .bind(("project_id", record_id("project", project_id)))
        .bind(("pending", pending))
        .bind(("observed", old.map(|row| row.updated_at)))
        .bind(("title", normalized.title.clone()))
        .bind(("description", normalized.description.clone()))
        .bind(("due_at", normalized.due_at.clone()))
        .bind(("due_on", normalized.due_on.as_ref().map(|date| serde_json::json!(date))))
        .bind(("hearing", normalized.hearing.as_ref().map(|details| serde_json::json!(details))))
        .bind(("authority_kind", normalized.authority_kind.clone()))
        .bind(("authority", normalized.authority.clone()))
        .bind(("source", normalized.source.clone()))
        .bind((
            "source_document_id",
            source_document_id.map(|id| record_id("asset", id)),
        ))
        .bind((
            "owner_person_id",
            owner_person_id.map(|id| record_id("person", id)),
        ))
        .bind(("status", status.to_string()))
        .bind(("calendar_role", normalized.calendar_role.clone()))
        .bind(("category", normalized.category.clone()))
        .bind(("internal_lead_days", normalized.internal_lead_days))
        .bind(("client_lead_days", normalized.client_lead_days))
        .bind(("replay_key", normalized.replay_key.clone()))
        .bind(("inserted_at", inserted_at.to_string()))
        .bind(("now", now))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let row: Option<DeadlineRow> = response.take(0)?;
    let deadline = row
        .map(DeadlineRow::into_deadline)
        .transpose()?
        .ok_or(DeadlineError::Conflict)?;
    crate::postgres_mirror::after_write(
        surreal,
        crate::postgres_mirror::MirrorKey::ProjectDeadline(deadline.id),
    )
    .await;
    Ok(deadline)
}

/// An existing Deadline whose calendar zone survives a retry or reschedule.
#[derive(Clone, Copy, Debug)]
pub enum DeadlineIdentity<'a> {
    Id(Uuid),
    Replay(Option<&'a str>),
}

/// Freeze the Project default into a civil Deadline at the time of a scheduling write.
pub async fn resolve_due(
    surreal: &SurrealDb,
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
                    DeadlineIdentity::Id(id) => find(surreal, project_id, id).await?,
                    DeadlineIdentity::Replay(Some(key)) => {
                        find_replay(surreal, project_id, key.trim()).await?
                    }
                    DeadlineIdentity::Replay(None) => None,
                };
                if let Some(saved) = existing.and_then(|row| row.due_on) {
                    saved.timezone
                } else {
                    let project = crate::projects::find_by_id(surreal, project_id)
                        .await?
                        .ok_or(DeadlineError::NotFound)?;
                    crate::projects::calendar_timezone(surreal, &project).await?
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
pub async fn create(surreal: &SurrealDb, new: &NewDeadline) -> Result<Deadline, DeadlineError> {
    let mut normalized = normalize(&DeadlineInput {
        title: new.title.as_deref(),
        description: new.description.as_deref(),
        due_at: &new.due_at,
        due_on: new.due_on.as_ref(),
        hearing: new.hearing.as_ref(),
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
    require_links(
        surreal,
        new.project_id,
        new.source_document_id,
        new.owner_person_id,
    )
    .await?;
    if let Some(key) = &normalized.replay_key {
        if let Some(existing) = find_replay(surreal, new.project_id, key).await? {
            if existing.status == STATUS_CANCELLED {
                return Err(DeadlineError::Invalid(format!(
                    "deadline {} is cancelled; use navigator project deadline reopen {} before replaying",
                    existing.id, existing.id
                )));
            }
            return persist(
                surreal,
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
        surreal,
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
            if category == Category::Hearing {
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
    let now = chrono::Utc::now().to_rfc3339();
    crate::postgres::project_deadlines::persist_on(
        conn,
        proposal.deadline_id.unwrap_or_else(Uuid::now_v7),
        proposal.project_id,
        &StoredDeadline {
            normalized: &normalized,
            source_document_id,
            owner_person_id: prior.and_then(|d| d.owner_person_id),
            status: STATUS_OPEN,
            expected_status: prior.map(|d| d.status.as_str()),
            expected_updated_at: prior.map(|d| d.updated_at.as_str()),
            inserted_at: &now,
        },
        true,
    )
    .await
}

/// Replace the named fields and return the stored row. Status stays put.
pub async fn update(
    surreal: &SurrealDb,
    project_id: Uuid,
    id: Uuid,
    patch: &DeadlinePatch,
) -> Result<Deadline, DeadlineError> {
    let existing = find(surreal, project_id, id)
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
        }),
        (Some(changes), None) => Some(changes.clone()),
        (None, current) if calendar_role == "hearing" || category.as_deref() == Some("hearing") => {
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
        authority_kind: &authority_kind,
        authority: &authority,
        source: &source,
        calendar_role: &calendar_role,
        category: category.as_deref(),
        internal_lead_days,
        client_lead_days,
        replay_key: existing.replay_key.as_deref(),
    })?;
    require_links(surreal, project_id, source_document_id, owner_person_id).await?;
    persist(
        surreal,
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

async fn transition(
    surreal: &SurrealDb,
    project_id: Uuid,
    id: Uuid,
    next: &str,
) -> Result<Deadline, DeadlineError> {
    let existing = find(surreal, project_id, id)
        .await?
        .ok_or(DeadlineError::NotFound)?;
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
    let normalized = normalize(&DeadlineInput {
        title: existing.title.as_deref(),
        description: existing.description.as_deref(),
        due_at: &existing.due_at,
        due_on: existing.due_on.as_ref(),
        hearing: existing.hearing.as_ref(),
        authority_kind: &existing.authority_kind,
        authority: &existing.authority,
        source: &existing.source,
        calendar_role: &existing.calendar_role,
        category: existing.category.as_deref(),
        internal_lead_days: existing.internal_lead_days,
        client_lead_days: existing.client_lead_days,
        replay_key: existing.replay_key.as_deref(),
    })?;
    persist(
        surreal,
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
    surreal: &SurrealDb,
    project_id: Uuid,
    id: Uuid,
) -> Result<Deadline, DeadlineError> {
    transition(surreal, project_id, id, STATUS_COMPLETE).await
}

pub async fn cancel(
    surreal: &SurrealDb,
    project_id: Uuid,
    id: Uuid,
) -> Result<Deadline, DeadlineError> {
    transition(surreal, project_id, id, STATUS_CANCELLED).await
}

/// Restore the same cancelled row to open. Open rows are unchanged; complete
/// rows cannot be reopened.
pub async fn reopen(
    surreal: &SurrealDb,
    project_id: Uuid,
    id: Uuid,
) -> Result<Deadline, DeadlineError> {
    transition(surreal, project_id, id, STATUS_OPEN).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::postgres_mirror::from_source;
    use crate::projects::{create as create_project, NewProject};
    use crate::test_support::mem_surreal;

    fn sample(project_id: Uuid) -> NewDeadline {
        NewDeadline {
            project_id,
            title: None,
            description: None,
            hearing: None,
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

    async fn project(surreal: &SurrealDb) -> Uuid {
        from_source(create_project(
            surreal,
            &NewProject {
                code: format!("deadline-{}", Uuid::now_v7()),
                name: "Deadline matter".into(),
                status: "open".into(),
                entity_id: Uuid::now_v7(),
                ..Default::default()
            },
        ))
        .await
        .unwrap()
        .id
    }

    // Select the embedded Source for the same application commands used by callers.
    async fn create(db: &SurrealDb, input: &NewDeadline) -> Result<Deadline, DeadlineError> {
        from_source(super::create(db, input)).await
    }

    async fn update(
        db: &SurrealDb,
        project: Uuid,
        id: Uuid,
        patch: &DeadlinePatch,
    ) -> Result<Deadline, DeadlineError> {
        from_source(super::update(db, project, id, patch)).await
    }

    async fn find(
        db: &SurrealDb,
        project: Uuid,
        id: Uuid,
    ) -> Result<Option<Deadline>, DeadlineError> {
        from_source(super::find(db, project, id)).await
    }

    async fn list(db: &SurrealDb, project: Uuid) -> Result<Vec<Deadline>, DeadlineError> {
        from_source(super::list(db, project)).await
    }

    #[tokio::test]
    async fn create_list_update_and_complete_round_trip() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let created = create(&surreal, &sample(project_id)).await.unwrap();
        assert_eq!(created.status, STATUS_OPEN);
        assert_eq!(created.due_at, "2026-11-02T17:00:00-08:00");
        assert_eq!(created.calendar_role, "firm");
        let listed = list(&surreal, project_id).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, created.id);
        let updated = update(
            &surreal,
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
        let completed = from_source(complete(&surreal, project_id, created.id))
            .await
            .unwrap();
        assert_eq!(completed.status, STATUS_COMPLETE);
        assert_eq!(
            list(&surreal, project_id).await.unwrap()[0].status,
            STATUS_COMPLETE
        );
    }

    #[tokio::test]
    async fn a_create_without_a_settleable_category_is_refused() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let mut input = sample(project_id);
        input.category = None;
        let error = create(&surreal, &input).await.unwrap_err();
        assert!(matches!(error, DeadlineError::Invalid(_)), "{error}");
        assert!(error.to_string().contains("category"), "{error}");
        assert!(list(&surreal, project_id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_unknown_category_is_refused() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let mut input = sample(project_id);
        input.category = Some("filing".into());
        let error = create(&surreal, &input).await.unwrap_err();
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
            create(&surreal, &trimmed).await.unwrap().category,
            Some("hearing".into())
        );
    }

    #[tokio::test]
    async fn a_hearing_role_settles_a_hearing_category() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let mut input = sample(project_id);
        input.calendar_role = "hearing".into();
        input.category = None;
        assert_eq!(
            create(&surreal, &input).await.unwrap().category,
            Some("hearing".into())
        );
    }

    #[tokio::test]
    async fn an_internal_authority_settles_an_internal_category() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let mut input = sample(project_id);
        input.authority_kind = "internal".into();
        input.authority = "Firm preparation target".into();
        input.category = None;
        let internal = create(&surreal, &input).await.unwrap();
        assert_eq!(internal.category.as_deref(), Some("internal"));
        let promotion = DeadlinePatch {
            category: Some(Some("court".into())),
            ..DeadlinePatch::default()
        };
        assert!(matches!(
            update(&surreal, project_id, internal.id, &promotion).await,
            Err(DeadlineError::Invalid(_))
        ));
        assert_eq!(
            find(&surreal, project_id, internal.id).await.unwrap(),
            Some(internal)
        );
    }

    #[tokio::test]
    async fn update_preserves_the_category_until_it_is_cleared_or_changed() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let created = create(&surreal, &sample(project_id)).await.unwrap();
        // A patch that names no category leaves the stored one alone.
        let untouched = update(&surreal, project_id, created.id, &DeadlinePatch::default())
            .await
            .unwrap();
        assert_eq!(untouched.category, Some("court".into()));
        let renamed = update(
            &surreal,
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
        let cleared = update(
            &surreal,
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
    }

    #[tokio::test]
    async fn a_null_category_is_refused_without_changing_the_deadline() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let before = create(&surreal, &sample(project_id)).await.unwrap();
        let attempted = surreal
            .query("UPDATE $id SET category = NULL")
            .bind((
                "id",
                crate::surreal::record_id("project_deadlines", before.id),
            ))
            .await
            .and_then(surrealdb::IndexedResults::check);
        assert!(
            attempted.is_err(),
            "a missing optional category is NONE, not NULL"
        );
        assert_eq!(
            find(&surreal, project_id, before.id).await.unwrap(),
            Some(before)
        );
    }

    #[tokio::test]
    async fn applying_preserves_a_lawyers_cleared_category() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let mut ids = Vec::new();
        for role in ["hearing", "firm"] {
            let mut input = sample(project_id);
            input.calendar_role = role.into();
            input.authority_kind = "internal".into();
            input.category = None;
            ids.push(create(&surreal, &input).await.unwrap().id);
        }
        surreal
            .query("UPSERT schema_version:current SET version = 111")
            .await
            .unwrap()
            .check()
            .unwrap();
        crate::schema::apply(&surreal).await.unwrap();
        for id in ids {
            let cleared = update(
                &surreal,
                project_id,
                id,
                &DeadlinePatch {
                    category: Some(None),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            assert_eq!(cleared.category, None);
            crate::schema::apply(&surreal).await.unwrap();
            assert_eq!(find(&surreal, project_id, id).await.unwrap(), Some(cleared));
        }
    }

    #[tokio::test]
    async fn a_repaired_category_retries_until_the_schema_upgrade_is_recorded() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let mut input = sample(project_id);
        input.authority_kind = "internal".into();
        input.category = None;
        let created = create(&surreal, &input).await.unwrap();
        surreal
            .query("UPDATE $id UNSET category; UPSERT schema_version:current SET version = 111")
            .bind((
                "id",
                crate::surreal::record_id("project_deadlines", created.id),
            ))
            .await
            .unwrap()
            .check()
            .unwrap();
        surreal
            .query(
                "DEFINE EVENT interrupt_category_upgrade ON TABLE schema_version \
            WHEN $after.version > 111 THEN { THROW 'synthetic version write interruption'; };",
            )
            .await
            .unwrap()
            .check()
            .unwrap();
        assert!(matches!(
            crate::schema::apply(&surreal).await,
            Err(crate::schema::SchemaError::RecordVersion(_))
        ));
        assert_eq!(
            crate::schema::installed_version(&surreal).await.unwrap(),
            Some(111)
        );
        let repaired = find(&surreal, project_id, created.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(repaired.category.as_deref(), Some("internal"));
        surreal
            .query("REMOVE EVENT interrupt_category_upgrade ON TABLE schema_version")
            .await
            .unwrap()
            .check()
            .unwrap();
        crate::schema::apply(&surreal).await.unwrap();
        assert_eq!(
            crate::schema::installed_version(&surreal).await.unwrap(),
            Some(crate::schema::SCHEMA_VERSION)
        );
        assert_eq!(
            find(&surreal, project_id, created.id).await.unwrap(),
            Some(repaired)
        );
    }

    #[tokio::test]
    async fn applying_recovers_only_unambiguous_missing_categories() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let mut ids = Vec::new();
        for (role, authority, category) in [
            ("hearing", "internal", Some("hearing")),
            ("firm", "internal", Some("internal")),
            ("opponent", "court_rule", None),
            ("hearing", "internal", Some("discovery")),
        ] {
            let mut input = sample(project_id);
            input.calendar_role = role.into();
            input.authority_kind = authority.into();
            input.category = Some(category.unwrap_or("court").into());
            let row = create(&surreal, &input).await.unwrap();
            if category != Some("discovery") {
                surreal
                    .query("UPDATE $id UNSET category")
                    .bind(("id", crate::surreal::record_id("project_deadlines", row.id)))
                    .await
                    .unwrap()
                    .check()
                    .unwrap();
            }
            let historical = find(&surreal, project_id, row.id).await.unwrap().unwrap();
            ids.push((historical, category));
        }
        surreal
            .query("UPSERT schema_version:current SET version = 111")
            .await
            .unwrap()
            .check()
            .unwrap();
        for _ in 0..2 {
            crate::schema::apply(&surreal).await.unwrap();
            for (before, expected) in &ids {
                let after = find(&surreal, project_id, before.id)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(after.category.as_deref(), *expected);
                assert_eq!(after.due_at, before.due_at);
                assert_eq!(after.authority, before.authority);
                assert_eq!(after.status, before.status);
                assert_eq!(after.owner_person_id, before.owner_person_id);
                assert_eq!(after.replay_key, before.replay_key);
                assert_eq!(after.inserted_at, before.inserted_at);
                assert_eq!(after.updated_at, before.updated_at);
                assert_eq!(after.calendar_sequence, before.calendar_sequence);
                if before.category.is_none() && after.category.is_some() {
                    assert_ne!(
                        crate::deadline_calendar::body_sha(before, "synthetic", None),
                        crate::deadline_calendar::body_sha(&after, "synthetic", None),
                        "the existing mirror detects the repaired color without a timestamp change"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn a_historical_row_without_a_category_reads_and_updates() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let created = create(&surreal, &sample(project_id)).await.unwrap();
        surreal
            .query("UPDATE $id UNSET category, calendar_review_pending")
            .bind((
                "id",
                crate::surreal::record_id("project_deadlines", created.id),
            ))
            .await
            .unwrap()
            .check()
            .unwrap();
        let row = find(&surreal, project_id, created.id)
            .await
            .unwrap()
            .expect("row still reads");
        assert_eq!(row.category, None);
        // A historical row still updates: an absent patch preserves its lack
        // of a category rather than inventing one.
        let untouched = update(&surreal, project_id, created.id, &DeadlinePatch::default())
            .await
            .unwrap();
        assert_eq!(untouched.category, None);
        let reschedule = DeadlinePatch {
            due_at: Some("2027-02-01T12:00:00Z".into()),
            ..DeadlinePatch::default()
        };
        assert!(matches!(
            update(&surreal, project_id, created.id, &reschedule).await,
            Err(DeadlineError::Invalid(_))
        ));
        assert_eq!(
            find(&surreal, project_id, created.id).await.unwrap(),
            Some(untouched)
        );
    }

    #[tokio::test]
    async fn a_statute_without_a_citation_is_refused() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let mut input = sample(project_id);
        input.authority = "  ".into();
        let error = create(&surreal, &input).await.unwrap_err();
        assert!(matches!(error, DeadlineError::Invalid(_)), "{error}");
        assert!(list(&surreal, project_id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_shorter_internal_lead_is_refused() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let mut input = sample(project_id);
        input.internal_lead_days = Some(1);
        input.client_lead_days = Some(5);
        assert!(matches!(
            create(&surreal, &input).await.unwrap_err(),
            DeadlineError::Invalid(_)
        ));
    }

    #[tokio::test]
    async fn a_date_without_an_offset_is_refused() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let mut input = sample(project_id);
        input.due_at = "2026-11-02".into();
        assert!(matches!(
            create(&surreal, &input).await.unwrap_err(),
            DeadlineError::Invalid(_)
        ));
    }

    #[tokio::test]
    async fn replay_key_updates_the_same_row() {
        crate::postgres_mirror::from_source(async {
            let surreal = mem_surreal().await;
            let project_id = project(&surreal).await;
            let mut input = sample(project_id);
            input.replay_key = Some("notice-1".into());
            let (first, second) = tokio::join!(create(&surreal, &input), create(&surreal, &input));
            let rows = list(&surreal, project_id).await.unwrap();
            assert_eq!(rows.len(), 1);
            // A transaction conflict can require one more read-back.
            assert!(first.is_ok() || second.is_ok());
            for row in [first, second].into_iter().flatten() {
                assert_eq!(row.id, rows[0].id);
            }
            input.due_at = "2026-12-15T12:00:00+00:00".into();
            let updated = create(&surreal, &input).await.unwrap();
            assert_eq!(updated.id, rows[0].id);
            assert_eq!(updated.due_at, input.due_at);
            assert_eq!(list(&surreal, project_id).await.unwrap(), vec![updated]);
        })
        .await;
    }

    #[tokio::test]
    async fn cancelled_replay_is_refused_without_changing_the_row() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let mut input = sample(project_id);
        input.replay_key = Some("notice-1".into());
        let created = create(&surreal, &input).await.unwrap();
        let cancelled = from_source(cancel(&surreal, project_id, created.id))
            .await
            .unwrap();
        input.due_at = "2026-12-15T12:00:00Z".into();
        let error = create(&surreal, &input).await.unwrap_err();
        assert!(error.to_string().contains("cancelled"), "{error}");
        assert!(error.to_string().contains("reopen"), "{error}");
        assert_eq!(
            find(&surreal, project_id, created.id).await.unwrap(),
            Some(cancelled)
        );
        assert_eq!(list(&surreal, project_id).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn reopen_preserves_identity_and_fields_and_is_idempotent() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let other = project(&surreal).await;
        let mut input = sample(project_id);
        input.replay_key = Some("notice-1".into());
        let created = create(&surreal, &input).await.unwrap();
        let cancelled = from_source(cancel(&surreal, project_id, created.id))
            .await
            .unwrap();
        assert!(matches!(
            from_source(reopen(&surreal, other, created.id)).await,
            Err(DeadlineError::NotFound)
        ));
        assert_eq!(
            find(&surreal, project_id, created.id).await.unwrap(),
            Some(cancelled)
        );
        let reopened = from_source(reopen(&surreal, project_id, created.id))
            .await
            .unwrap();
        let mut expected = created.clone();
        expected.updated_at = reopened.updated_at.clone();
        expected.calendar_sequence = Some(2);
        assert_eq!(reopened, expected);
        assert_eq!(
            from_source(reopen(&surreal, project_id, created.id))
                .await
                .unwrap(),
            reopened
        );
        input.due_at = "2026-12-15T12:00:00Z".into();
        let replayed = create(&surreal, &input).await.unwrap();
        assert_eq!(replayed.id, created.id);
        assert_eq!(replayed.status, STATUS_OPEN);
        assert_eq!(replayed.due_at, input.due_at.replace('Z', "+00:00"));
        input.project_id = other;
        let foreign = create(&surreal, &input).await.unwrap();
        assert_ne!(foreign.id, created.id);
        assert_eq!(list(&surreal, project_id).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn completed_replay_keeps_status_and_cannot_reopen() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let mut input = sample(project_id);
        input.replay_key = Some("notice-1".into());
        let created = create(&surreal, &input).await.unwrap();
        from_source(complete(&surreal, project_id, created.id))
            .await
            .unwrap();
        input.due_at = "2026-12-15T12:00:00Z".into();
        let replayed = create(&surreal, &input).await.unwrap();
        assert_eq!(replayed.id, created.id);
        assert_eq!(replayed.status, STATUS_COMPLETE);
        assert_eq!(replayed.inserted_at, created.inserted_at);
        assert_eq!(replayed.due_at, input.due_at.replace('Z', "+00:00"));
        assert!(matches!(
            from_source(reopen(&surreal, project_id, created.id)).await,
            Err(DeadlineError::Invalid(_))
        ));
        assert_eq!(
            find(&surreal, project_id, created.id).await.unwrap(),
            Some(replayed)
        );
    }

    #[tokio::test]
    async fn cancel_keeps_the_row() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let created = create(&surreal, &sample(project_id)).await.unwrap();
        let cancelled = from_source(cancel(&surreal, project_id, created.id))
            .await
            .unwrap();
        assert_eq!(cancelled.status, STATUS_CANCELLED);
        assert_eq!(list(&surreal, project_id).await.unwrap().len(), 1);
        assert!(matches!(
            from_source(complete(&surreal, project_id, created.id))
                .await
                .unwrap_err(),
            DeadlineError::Invalid(_)
        ));
    }

    #[tokio::test]
    async fn a_title_round_trips_updates_and_clears() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let mut input = sample(project_id);
        input.title = Some("  Answer to the complaint ".into());
        let created = create(&surreal, &input).await.unwrap();
        assert_eq!(created.title.as_deref(), Some("Answer to the complaint"));
        let renamed = update(
            &surreal,
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
        let kept = from_source(complete(&surreal, project_id, created.id))
            .await
            .unwrap();
        assert_eq!(kept.title.as_deref(), Some("Reply brief"));
        let cleared = update(
            &surreal,
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
    }

    #[tokio::test]
    async fn descriptions_round_trip_and_clear_without_changing_visibility() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let mut input = sample(project_id);
        input.description = Some("  Synthetic preparation instructions  ".into());
        let created = create(&surreal, &input).await.unwrap();
        assert_eq!(
            created.description.as_deref(),
            Some("Synthetic preparation instructions")
        );
        let changed = update(
            &surreal,
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
        let cleared = update(
            &surreal,
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
    }

    #[tokio::test]
    async fn an_overlong_title_is_refused() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let mut input = sample(project_id);
        input.title = Some("x".repeat(MAX_TITLE_CHARS + 1));
        assert!(matches!(
            create(&surreal, &input).await.unwrap_err(),
            DeadlineError::Invalid(_)
        ));
    }

    #[tokio::test]
    async fn a_row_written_without_a_title_still_reads() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let id = Uuid::now_v7();
        surreal
            .query(
                "CREATE $id SET project_id = $project, due_at = '2027-04-05T12:00:00+00:00', \
                 authority_kind = 'internal', authority = '', source = 'lawyer', status = 'open', \
                 calendar_role = 'firm', inserted_at = '2026-10-15T12:00:00Z', \
                 updated_at = '2026-10-15T12:00:00Z'",
            )
            .bind(("id", record_id("project_deadlines", id)))
            .bind(("project", record_id("project", project_id)))
            .await
            .unwrap()
            .check()
            .unwrap();
        let row = find(&surreal, project_id, id).await.unwrap().unwrap();
        assert_eq!(row.title, None);
        assert_eq!(row.description, None);
        assert_eq!(row.calendar_sequence, None);
        assert_eq!(list(&surreal, project_id).await.unwrap().len(), 1);
    }
}

//! Project-scoped Deadline rows (`project_deadlines`).
//!
//! A write stores the due instant the caller named. It never counts court
//! days. Complete, cancel, and reopen change `status`; they keep the row.

use chrono::{DateTime, FixedOffset};
use serde::Serialize;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::surreal::{record_id, record_uuid, SurrealDb};

pub const STATUS_OPEN: &str = "open";
pub const STATUS_COMPLETE: &str = "complete";
pub const STATUS_CANCELLED: &str = "cancelled";

const AUTHORITY_KINDS: &[&str] = &[
    "statute",
    "court_rule",
    "court_order",
    "contract",
    "internal",
];
pub const MAX_TITLE_CHARS: usize = 200;
const CALENDAR_ROLES: &[&str] = &["firm", "opponent", "hearing"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Deadline {
    pub id: Uuid,
    pub project_id: Uuid,
    /// A short name for what is due; rows written before titles existed have none.
    pub title: Option<String>,
    pub due_at: String,
    pub authority_kind: String,
    pub authority: String,
    pub source: String,
    pub source_document_id: Option<Uuid>,
    pub owner_person_id: Option<Uuid>,
    pub status: String,
    pub calendar_role: String,
    pub internal_lead_days: Option<i64>,
    pub client_lead_days: Option<i64>,
    pub replay_key: Option<String>,
    pub inserted_at: String,
    pub updated_at: String,
    /// Monotonic RFC 5545 revision; absent on historical rows.
    pub calendar_sequence: Option<i64>,
}

#[derive(SurrealValue)]
struct DeadlineRow {
    id: surrealdb::types::RecordId,
    project_id: surrealdb::types::RecordId,
    title: Option<String>,
    due_at: String,
    authority_kind: String,
    authority: String,
    source: String,
    source_document_id: Option<surrealdb::types::RecordId>,
    owner_person_id: Option<surrealdb::types::RecordId>,
    status: String,
    calendar_role: String,
    internal_lead_days: Option<i64>,
    client_lead_days: Option<i64>,
    replay_key: Option<String>,
    inserted_at: String,
    updated_at: String,
    calendar_sequence: Option<i64>,
}

impl DeadlineRow {
    fn into_deadline(self) -> Option<Deadline> {
        Some(Deadline {
            id: record_uuid(&self.id)?,
            project_id: record_uuid(&self.project_id)?,
            title: self.title,
            due_at: self.due_at,
            authority_kind: self.authority_kind,
            authority: self.authority,
            source: self.source,
            source_document_id: self.source_document_id.as_ref().and_then(record_uuid),
            owner_person_id: self.owner_person_id.as_ref().and_then(record_uuid),
            status: self.status,
            calendar_role: self.calendar_role,
            internal_lead_days: self.internal_lead_days,
            client_lead_days: self.client_lead_days,
            replay_key: self.replay_key,
            inserted_at: self.inserted_at,
            updated_at: self.updated_at,
            calendar_sequence: self.calendar_sequence,
        })
    }
}

const SELECT: &str = "id, project_id, title, due_at, authority_kind, authority, source, \
     source_document_id, owner_person_id, status, calendar_role, internal_lead_days, \
     client_lead_days, replay_key, inserted_at, updated_at, calendar_sequence";

#[derive(Debug, thiserror::Error)]
pub enum DeadlineError {
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
    pub due_at: String,
    pub authority_kind: String,
    pub authority: String,
    pub source: String,
    pub source_document_id: Option<Uuid>,
    pub owner_person_id: Option<Uuid>,
    pub calendar_role: String,
    pub internal_lead_days: Option<i64>,
    pub client_lead_days: Option<i64>,
    pub replay_key: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct DeadlinePatch {
    /// `Some(None)` clears the title.
    pub title: Option<Option<String>>,
    pub due_at: Option<String>,
    pub authority_kind: Option<String>,
    pub authority: Option<String>,
    pub source: Option<String>,
    pub source_document_id: Option<Option<Uuid>>,
    pub owner_person_id: Option<Option<Uuid>>,
    pub calendar_role: Option<String>,
    pub internal_lead_days: Option<Option<i64>>,
    pub client_lead_days: Option<Option<i64>>,
}

struct Normalized {
    title: Option<String>,
    due_at: String,
    authority_kind: String,
    authority: String,
    source: String,
    calendar_role: String,
    internal_lead_days: Option<i64>,
    client_lead_days: Option<i64>,
    replay_key: Option<String>,
}

#[derive(Clone, Copy)]
struct DeadlineInput<'a> {
    title: Option<&'a str>,
    due_at: &'a str,
    authority_kind: &'a str,
    authority: &'a str,
    source: &'a str,
    calendar_role: &'a str,
    internal_lead_days: Option<i64>,
    client_lead_days: Option<i64>,
    replay_key: Option<&'a str>,
}

fn normalize(input: &DeadlineInput<'_>) -> Result<Normalized, DeadlineError> {
    let DeadlineInput {
        title,
        due_at,
        authority_kind,
        authority,
        source,
        calendar_role,
        internal_lead_days,
        client_lead_days,
        replay_key,
    } = *input;
    let due_at = DateTime::<FixedOffset>::parse_from_rfc3339(due_at.trim())
        .map(|instant| instant.to_rfc3339())
        .map_err(|_| {
            DeadlineError::Invalid(
                "due_at must be an RFC 3339 timestamp with an explicit offset".into(),
            )
        })?;
    if !AUTHORITY_KINDS.contains(&authority_kind) {
        return Err(DeadlineError::Invalid(format!(
            "authority_kind must be one of {}",
            AUTHORITY_KINDS.join(", ")
        )));
    }
    let authority = authority.trim().to_string();
    if matches!(authority_kind, "statute" | "court_rule") && authority.is_empty() {
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
        due_at,
        authority_kind: authority_kind.to_string(),
        authority,
        source,
        calendar_role: calendar_role.to_string(),
        internal_lead_days,
        client_lead_days,
        replay_key: replay_key.map(str::to_string),
    })
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
    let mut response = surreal
        .query(format!(
            "SELECT {SELECT} FROM project_deadlines WHERE project_id = $project_id \
             ORDER BY due_at, id"
        ))
        .bind(("project_id", record_id("project", project_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<DeadlineRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(DeadlineRow::into_deadline)
        .collect())
}

pub async fn find(
    surreal: &SurrealDb,
    project_id: Uuid,
    id: Uuid,
) -> Result<Option<Deadline>, DeadlineError> {
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
    Ok(row.and_then(DeadlineRow::into_deadline))
}

async fn find_replay(
    surreal: &SurrealDb,
    project_id: Uuid,
    replay_key: &str,
) -> Result<Option<Deadline>, DeadlineError> {
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
    Ok(row.and_then(DeadlineRow::into_deadline))
}

#[derive(Clone, Copy)]
struct StoredDeadline<'a> {
    normalized: &'a Normalized,
    source_document_id: Option<Uuid>,
    owner_person_id: Option<Uuid>,
    status: &'a str,
    inserted_at: &'a str,
}

async fn persist(
    surreal: &SurrealDb,
    id: Uuid,
    project_id: Uuid,
    stored: &StoredDeadline<'_>,
) -> Result<Deadline, DeadlineError> {
    let StoredDeadline {
        normalized,
        source_document_id,
        owner_person_id,
        status,
        inserted_at,
    } = *stored;
    let now = chrono::Utc::now().to_rfc3339();
    let mut response = surreal
        .query(format!(
            "UPSERT $id SET project_id = $project_id, title = $title, due_at = $due_at, \
             authority_kind = $authority_kind, authority = $authority, source = $source, \
             source_document_id = $source_document_id, owner_person_id = $owner_person_id, \
             status = $status, calendar_role = $calendar_role, \
             internal_lead_days = $internal_lead_days, client_lead_days = $client_lead_days, \
             replay_key = $replay_key, calendar_sequence = IF updated_at IS NONE THEN 0 ELSE \
             (calendar_sequence ?? math::max([0, time::unix(<datetime>updated_at)])) + 1 END, \
             inserted_at = $inserted_at, updated_at = $now \
             RETURN {SELECT}"
        ))
        .bind(("id", record_id("project_deadlines", id)))
        .bind(("project_id", record_id("project", project_id)))
        .bind(("title", normalized.title.clone()))
        .bind(("due_at", normalized.due_at.clone()))
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
        .bind(("internal_lead_days", normalized.internal_lead_days))
        .bind(("client_lead_days", normalized.client_lead_days))
        .bind(("replay_key", normalized.replay_key.clone()))
        .bind(("inserted_at", inserted_at.to_string()))
        .bind(("now", now))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let row: Option<DeadlineRow> = response.take(0)?;
    let deadline = row
        .and_then(DeadlineRow::into_deadline)
        .ok_or(DeadlineError::WriteReturnedNothing)?;
    crate::postgres_mirror::after_write(
        surreal,
        crate::postgres_mirror::MirrorKey::ProjectDeadline(deadline.id),
    )
    .await;
    Ok(deadline)
}

/// Create a Deadline and return the stored row. A `replay_key` that already
/// names a row on this Project updates that row instead of inserting another.
/// Cancelled rows refuse replay until explicitly reopened. Complete rows keep
/// their status while their fields update.
pub async fn create(surreal: &SurrealDb, new: &NewDeadline) -> Result<Deadline, DeadlineError> {
    let normalized = normalize(&DeadlineInput {
        title: new.title.as_deref(),
        due_at: &new.due_at,
        authority_kind: &new.authority_kind,
        authority: &new.authority,
        source: &new.source,
        calendar_role: &new.calendar_role,
        internal_lead_days: new.internal_lead_days,
        client_lead_days: new.client_lead_days,
        replay_key: new.replay_key.as_deref(),
    })?;
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
                    status: &existing.status,
                    inserted_at: &existing.inserted_at,
                },
            )
            .await;
        }
    }
    let inserted_at = chrono::Utc::now().to_rfc3339();
    persist(
        surreal,
        Uuid::now_v7(),
        new.project_id,
        &StoredDeadline {
            normalized: &normalized,
            source_document_id: new.source_document_id,
            owner_person_id: new.owner_person_id,
            status: STATUS_OPEN,
            inserted_at: &inserted_at,
        },
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
        due_at: &due_at,
        authority_kind: &authority_kind,
        authority: &authority,
        source: &source,
        calendar_role: &calendar_role,
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
        due_at: &existing.due_at,
        authority_kind: &existing.authority_kind,
        authority: &existing.authority,
        source: &existing.source,
        calendar_role: &existing.calendar_role,
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
    use crate::projects::{create as create_project, NewProject};
    use crate::test_support::mem_surreal;

    fn sample(project_id: Uuid) -> NewDeadline {
        NewDeadline {
            project_id,
            title: None,
            due_at: "2026-11-02T17:00:00-08:00".into(),
            authority_kind: "statute".into(),
            authority: "15 U.S.C. § 1681i(a)(1)".into(),
            source: "lawyer".into(),
            source_document_id: None,
            owner_person_id: None,
            calendar_role: "firm".into(),
            internal_lead_days: Some(14),
            client_lead_days: Some(7),
            replay_key: None,
        }
    }

    async fn project(surreal: &SurrealDb) -> Uuid {
        create_project(
            surreal,
            &NewProject {
                code: format!("deadline-{}", Uuid::now_v7()),
                name: "Deadline matter".into(),
                status: "open".into(),
                entity_id: Uuid::now_v7(),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .id
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
        let completed = complete(&surreal, project_id, created.id).await.unwrap();
        assert_eq!(completed.status, STATUS_COMPLETE);
        assert_eq!(
            list(&surreal, project_id).await.unwrap()[0].status,
            STATUS_COMPLETE
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
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let mut input = sample(project_id);
        input.replay_key = Some("notice-1".into());
        let first = create(&surreal, &input).await.unwrap();
        input.due_at = "2026-12-15T12:00:00Z".into();
        let second = create(&surreal, &input).await.unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(list(&surreal, project_id).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn cancelled_replay_is_refused_without_changing_the_row() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let mut input = sample(project_id);
        input.replay_key = Some("notice-1".into());
        let created = create(&surreal, &input).await.unwrap();
        let cancelled = cancel(&surreal, project_id, created.id).await.unwrap();
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
        let cancelled = cancel(&surreal, project_id, created.id).await.unwrap();
        assert!(matches!(
            reopen(&surreal, other, created.id).await,
            Err(DeadlineError::NotFound)
        ));
        assert_eq!(
            find(&surreal, project_id, created.id).await.unwrap(),
            Some(cancelled)
        );
        let reopened = reopen(&surreal, project_id, created.id).await.unwrap();
        let mut expected = created.clone();
        expected.updated_at = reopened.updated_at.clone();
        expected.calendar_sequence = Some(2);
        assert_eq!(reopened, expected);
        assert_eq!(
            reopen(&surreal, project_id, created.id).await.unwrap(),
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
        complete(&surreal, project_id, created.id).await.unwrap();
        input.due_at = "2026-12-15T12:00:00Z".into();
        let replayed = create(&surreal, &input).await.unwrap();
        assert_eq!(replayed.id, created.id);
        assert_eq!(replayed.status, STATUS_COMPLETE);
        assert_eq!(replayed.inserted_at, created.inserted_at);
        assert_eq!(replayed.due_at, input.due_at.replace('Z', "+00:00"));
        assert!(matches!(
            reopen(&surreal, project_id, created.id).await,
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
        let cancelled = cancel(&surreal, project_id, created.id).await.unwrap();
        assert_eq!(cancelled.status, STATUS_CANCELLED);
        assert_eq!(list(&surreal, project_id).await.unwrap().len(), 1);
        assert!(matches!(
            complete(&surreal, project_id, created.id)
                .await
                .unwrap_err(),
            DeadlineError::Invalid(_)
        ));
    }

    #[tokio::test]
    async fn calendar_sequence_increments_on_rapid_updates_and_cancellation() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let row = create(&surreal, &sample(project_id)).await.unwrap();
        assert_eq!(row.calendar_sequence, Some(0));
        for sequence in 1..=3 {
            let updated = update(&surreal, project_id, row.id, &DeadlinePatch::default())
                .await
                .unwrap();
            assert_eq!(updated.id, row.id);
            assert_eq!(updated.calendar_sequence, Some(sequence));
        }
        let cancelled = cancel(&surreal, project_id, row.id).await.unwrap();
        assert_eq!(cancelled.id, row.id);
        assert_eq!(cancelled.calendar_sequence, Some(4));
    }

    #[tokio::test]
    async fn historical_calendar_versions_never_regress_and_overflow_is_refused() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let row = create(&surreal, &sample(project_id)).await.unwrap();
        surreal
            .query("UPDATE $id SET calendar_sequence = NONE, updated_at = '2026-10-26T00:00:00Z'")
            .bind(("id", record_id("project_deadlines", row.id)))
            .await
            .unwrap()
            .check()
            .unwrap();
        let updated = update(&surreal, project_id, row.id, &DeadlinePatch::default())
            .await
            .unwrap();
        assert_eq!(updated.calendar_sequence, Some(1_792_972_801));
        surreal
            .query("UPDATE $id SET calendar_sequence = 2147483647")
            .bind(("id", record_id("project_deadlines", row.id)))
            .await
            .unwrap()
            .check()
            .unwrap();
        assert!(
            update(&surreal, project_id, row.id, &DeadlinePatch::default())
                .await
                .is_err()
        );
        assert_eq!(
            find(&surreal, project_id, row.id)
                .await
                .unwrap()
                .unwrap()
                .calendar_sequence,
            Some(2_147_483_647)
        );
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
        let kept = complete(&surreal, project_id, created.id).await.unwrap();
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
        assert_eq!(row.calendar_sequence, None);
        assert_eq!(list(&surreal, project_id).await.unwrap().len(), 1);
    }
}

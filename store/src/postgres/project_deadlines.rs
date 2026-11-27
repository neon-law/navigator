//! Transactional Deadline replay identity and calendar revisions.

use crate::project_deadlines::{Deadline, DeadlineError, STATUS_CANCELLED};
use chrono::DateTime;
use uuid::Uuid;

pub(crate) struct Normalized {
    pub(crate) title: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) due_at: String,
    pub(crate) due_on: Option<crate::project_deadlines::DueOn>,
    pub(crate) hearing: Option<crate::project_deadlines::Hearing>,
    pub(crate) effort_minutes: Option<i32>,
    pub(crate) authority_kind: String,
    pub(crate) authority: String,
    pub(crate) source: String,
    pub(crate) calendar_role: String,
    pub(crate) category: Option<String>,
    pub(crate) internal_lead_days: Option<i64>,
    pub(crate) client_lead_days: Option<i64>,
    pub(crate) replay_key: Option<String>,
}

#[derive(Clone, Copy)]
pub(crate) struct StoredDeadline<'a> {
    pub(crate) normalized: &'a Normalized,
    pub(crate) source_document_id: Option<Uuid>,
    pub(crate) owner_person_id: Option<Uuid>,
    pub(crate) status: &'a str,
    pub(crate) expected_status: Option<&'a str>,
    pub(crate) expected_updated_at: Option<&'a str>,
    pub(crate) inserted_at: &'a str,
}

fn same_schedule(row: &Deadline, stored: &StoredDeadline<'_>, same_due: bool) -> bool {
    let n = stored.normalized;
    same_due
        && row.due_on == n.due_on
        && row.hearing == n.hearing
        && row.authority_kind == n.authority_kind
        && row.authority == n.authority
        && row.source_document_id == stored.source_document_id
        && row.category == n.category
        && row.calendar_role == n.calendar_role
}

async fn default_effort(
    conn: &mut sqlx::PgConnection,
    project: Uuid,
    category: Option<&str>,
) -> Result<i32, DeadlineError> {
    use crate::postgres::matter::MatterAuthorityError as PgError;
    let Some(text) = category else {
        return Ok(0);
    };
    let configured = sqlx::query_scalar::<_, Option<i32>>(
        "SELECT (f.deadline_effort_defaults ->> $2)::integer FROM project p
         LEFT JOIN firm_calendar_settings f ON f.firm_id=p.firm_id WHERE p.id=$1",
    )
    .bind(project)
    .bind(text)
    .fetch_one(conn)
    .await
    .map_err(PgError::from)?;
    Ok(configured.unwrap_or_else(|| {
        text.parse::<model::project_deadlines::Category>().map_or(
            0,
            model::project_deadlines::Category::default_effort_minutes,
        )
    }))
}

fn same_payload(
    row: &Deadline,
    stored: &StoredDeadline<'_>,
    status: &str,
    same_due: bool,
    effort: i32,
) -> bool {
    let n = stored.normalized;
    same_schedule(row, stored, same_due)
        && row.effort_minutes == effort
        && row.title == n.title
        && row.description == n.description
        && row.source == n.source
        && row.internal_lead_days == n.internal_lead_days
        && row.client_lead_days == n.client_lead_days
        && row.replay_key == n.replay_key
        && row.owner_person_id == stored.owner_person_id
        && row.status == status
}

pub(crate) fn review_pending(
    old: Option<&Deadline>,
    stored: &StoredDeadline<'_>,
    same_due: bool,
    reviewed: bool,
) -> Result<bool, DeadlineError> {
    use model::project_deadlines::requires_court_review;
    if reviewed {
        return Ok(false);
    }
    let n = stored.normalized;
    let court = requires_court_review(&n.calendar_role, &n.authority_kind, n.category.as_deref());
    let Some(old) = old else {
        return Ok(court);
    };
    if !old.calendar_review_pending
        && !same_schedule(old, stored, same_due)
        && (court
            || requires_court_review(
                &old.calendar_role,
                &old.authority_kind,
                old.category.as_deref(),
            ))
    {
        return Err(DeadlineError::Invalid(
            "amend the existing court date through a Deadline Proposal".into(),
        ));
    }
    Ok(old.calendar_review_pending)
}

async fn check_appearance_conflicts(
    conn: &mut sqlx::PgConnection,
    project: Uuid,
    id: Uuid,
    stored: &StoredDeadline<'_>,
) -> Result<(), DeadlineError> {
    use crate::postgres::matter::MatterAuthorityError as PgError;
    use model::project_deadlines::{is_appearance, Hearing, SchedulingPolicy};
    let n = stored.normalized;
    if !is_appearance(
        &n.calendar_role,
        n.category.as_deref().and_then(|value| value.parse().ok()),
    ) {
        return Ok(());
    }
    if let Some(owner) = stored.owner_person_id {
        sqlx::query("SELECT pg_advisory_xact_lock(1312902740, hashtext($1::text))")
            .bind(owner)
            .execute(&mut *conn)
            .await
            .map_err(PgError::from)?;
    }
    let buffer = sqlx::query_scalar::<_, i32>(
        "SELECT COALESCE(f.deadline_buffer_minutes, 15) FROM project p
         LEFT JOIN firm_calendar_settings f ON f.firm_id = p.firm_id WHERE p.id = $1",
    )
    .bind(project)
    .fetch_one(&mut *conn)
    .await
    .map_err(PgError::from)?;
    let policy = SchedulingPolicy::new(
        i64::from(buffer),
        SchedulingPolicy::default().end_of_business(),
    )
    .map_err(|error| DeadlineError::Invalid(error.into()))?;
    let start = n
        .due_at
        .parse()
        .map_err(|_| DeadlineError::Invalid("invalid appearance start".into()))?;
    let (start, end) = policy
        .appearance(start, n.hearing.as_ref().map_or(60, Hearing::minutes))
        .ok_or_else(|| {
            DeadlineError::Invalid("appearance is outside the supported calendar range".into())
        })?;
    let conflicts = sqlx::query_scalar::<_, String>(
        "SELECT CASE WHEN d.project_id = $1 THEN COALESCE(d.title, d.id::text)
                     ELSE 'another Project appointment' END
         FROM project_deadlines d JOIN project p ON p.id=d.project_id
         LEFT JOIN firm_calendar_settings f ON f.firm_id=p.firm_id
         WHERE (d.project_id = $1 OR d.owner_person_id = $3) AND d.id <> $2
           AND d.status = 'open' AND NOT d.calendar_review_pending
           AND (d.calendar_role = 'hearing' OR d.category IN
              ('hearing','trial_day','deposition','conference','consult','call'))
           AND d.due_at - make_interval(mins => COALESCE(f.deadline_buffer_minutes,15)) < $5
           AND d.due_at + make_interval(mins => COALESCE((d.hearing->>'duration_minutes')::integer,60)
              + COALESCE(f.deadline_buffer_minutes,15)) > $4
         ORDER BY d.due_at, d.id",
    ).bind(project).bind(id).bind(stored.owner_person_id)
     .bind(start).bind(end).fetch_all(conn).await.map_err(PgError::from)?;
    if conflicts.is_empty() {
        Ok(())
    } else {
        Err(DeadlineError::Invalid(format!(
            "appearance overlaps {}",
            conflicts.join(", ")
        )))
    }
}

impl StoredDeadline<'_> {
    pub(crate) fn check_revision(&self, old: Option<&Deadline>) -> Result<(), DeadlineError> {
        if self
            .expected_updated_at
            .is_some_and(|expected| old.is_none_or(|row| row.updated_at != expected))
            || self
                .expected_status
                .is_some_and(|expected| old.is_none_or(|row| row.status != expected))
        {
            return Err(DeadlineError::Conflict);
        }
        Ok(())
    }
}

/// A Project lock serializes replay identity, lifecycle checks, and calendar revision.
pub(crate) async fn persist(
    pg: &crate::postgres::PostgresDb,
    requested_id: Uuid,
    project: Uuid,
    stored: &StoredDeadline<'_>,
) -> Result<Deadline, DeadlineError> {
    use crate::postgres::matter::MatterAuthorityError as PgError;
    let mut tx = pg.pool().begin().await.map_err(PgError::from)?;
    let result = persist_on(&mut tx, requested_id, project, stored, false).await?;
    if result.category.as_deref() == Some("deposition") {
        if let Some(prep) = stored
            .normalized
            .hearing
            .as_ref()
            .and_then(|hearing| hearing.prep.as_deref())
        {
            persist_preparation(&mut tx, &result, prep, stored.inserted_at).await?;
        }
    }
    tx.commit().await.map_err(PgError::from)?;
    Ok(result)
}
/// Preparation is published atomically with its appearance. Retries retain the original child identity.
pub(crate) async fn persist_preparation(
    conn: &mut sqlx::PgConnection,
    parent: &Deadline,
    prep: &[model::project_deadlines::PreparationDeadline],
    now: &str,
) -> Result<(), DeadlineError> {
    use crate::postgres::matter::MatterAuthorityError as PgError;
    for (index, task) in prep.iter().enumerate() {
        let key = format!("preparation:{}:{index}", parent.id);
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM project_deadlines WHERE project_id=$1 AND replay_key=$2)",
        )
        .bind(parent.project_id)
        .bind(&key)
        .fetch_one(&mut *conn)
        .await
        .map_err(PgError::from)?;
        if exists {
            continue;
        }
        let normalized = Normalized {
            title: Some(task.title.trim().into()),
            description: None,
            due_at: task.due_at.to_rfc3339(),
            due_on: None,
            hearing: None,
            effort_minutes: Some(task.effort_minutes),
            authority_kind: "internal".into(),
            authority: "Appearance preparation".into(),
            source: "appearance_preparation".into(),
            calendar_role: "firm".into(),
            category: Some("internal".into()),
            internal_lead_days: None,
            client_lead_days: None,
            replay_key: Some(key),
        };
        persist_on(
            conn,
            Uuid::now_v7(),
            parent.project_id,
            &StoredDeadline {
                normalized: &normalized,
                source_document_id: None,
                owner_person_id: parent.owner_person_id,
                status: "open",
                expected_status: None,
                expected_updated_at: None,
                inserted_at: now,
            },
            false,
        )
        .await?;
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
pub(crate) async fn persist_on(
    conn: &mut sqlx::PgConnection,
    requested_id: Uuid,
    project: Uuid,
    stored: &StoredDeadline<'_>,
    reviewed: bool,
) -> Result<Deadline, DeadlineError> {
    use crate::postgres::matter::{self, MatterAuthorityError as PgError};
    let n = stored.normalized;
    sqlx::query("SELECT pg_advisory_xact_lock(1312902739, hashtext($1::text))")
        .bind(project)
        .execute(&mut *conn)
        .await
        .map_err(PgError::from)?;
    let old: Option<(serde_json::Value, bool)> = sqlx::query_as("SELECT to_jsonb(d), d.due_at IS NOT DISTINCT FROM $4::timestamptz FROM project_deadlines d WHERE project_id=$1 AND (id=$2 OR ($3::text IS NOT NULL AND replay_key=$3)) ORDER BY id LIMIT 1 FOR UPDATE")
        .bind(project).bind(requested_id).bind(&n.replay_key).bind(&n.due_at)
        .fetch_optional(&mut *conn).await.map_err(PgError::from)?;
    let (old, same_due) = old.map_or((None, false), |(row, same_due)| (Some(row), same_due));
    let old: Option<Deadline> = old
        .map(serde_json::from_value)
        .transpose()
        .map_err(PgError::from)?;
    let pending = review_pending(old.as_ref(), stored, same_due, reviewed)?;
    stored.check_revision(old.as_ref())?;
    if stored.expected_status.is_none()
        && old
            .as_ref()
            .is_some_and(|row| row.status == STATUS_CANCELLED)
    {
        return Err(DeadlineError::Invalid(
            "cancelled deadline must be reopened before replay".into(),
        ));
    }
    let status = if stored.expected_status.is_none() {
        old.as_ref()
            .map_or(stored.status, |row| row.status.as_str())
    } else {
        stored.status
    };
    let effort = match n
        .effort_minutes
        .or_else(|| old.as_ref().map(|row| row.effort_minutes))
    {
        Some(minutes) => minutes,
        None => default_effort(conn, project, n.category.as_deref()).await?,
    };
    if effort < 0 {
        return Err(DeadlineError::Invalid(
            "effort minutes must be nonnegative".into(),
        ));
    }
    // Identical retries do not create another calendar notification revision.
    if let Some(existing) = old.as_ref().filter(|row| {
        row.calendar_review_pending == pending
            && same_payload(row, stored, status, same_due, effort)
    }) {
        return Ok(existing.clone());
    }
    if !pending
        && status == "open"
        && old.as_ref().is_none_or(|row| {
            row.calendar_review_pending
                || row.status != "open"
                || !same_due
                || row.hearing != n.hearing
                || row.category != n.category
                || row.calendar_role != n.calendar_role
                || row.owner_person_id != stored.owner_person_id
        })
    {
        let id = old.as_ref().map_or(requested_id, |row| row.id);
        check_appearance_conflicts(conn, project, id, stored).await?;
    }
    let sequence = old
        .as_ref()
        .map(|row| {
            row.calendar_sequence
                .unwrap_or_else(|| {
                    DateTime::parse_from_rfc3339(&row.updated_at)
                        .map_or(0, |time| time.timestamp().max(0))
                })
                .checked_add(1)
                .filter(|sequence| *sequence <= i64::from(i32::MAX))
                .ok_or_else(|| DeadlineError::Invalid("calendar revision is exhausted".into()))
        })
        .transpose()?
        .unwrap_or(0);
    let id = old.as_ref().map_or(requested_id, |row| row.id);
    let inserted = old
        .as_ref()
        .map_or(stored.inserted_at, |row| row.inserted_at.as_str());
    let mut row = serde_json::json!({
        "id": id, "calendar_review_pending":pending, "project_id": project, "title": n.title, "description": n.description,
        "due_at": n.due_at,  "authority_kind": n.authority_kind, "authority": n.authority,
        "source": n.source, "source_document_id": stored.source_document_id,
        "owner_person_id": stored.owner_person_id, "status": status, "calendar_role": n.calendar_role,
        "category": n.category, "effort_minutes": effort, "internal_lead_days": n.internal_lead_days,
        "client_lead_days": n.client_lead_days, "replay_key": n.replay_key,
        "calendar_sequence": sequence, "inserted_at": inserted, "updated_at": chrono::Utc::now(),
    });
    if let Some(date) = &n.due_on {
        row["due_on"] = serde_json::json!(date);
    }
    if let Some(hearing) = &n.hearing {
        row["hearing"] = serde_json::json!(hearing);
    }
    let result = matter::upsert_on(&mut *conn, "project_deadlines", row).await?;
    Ok(result)
}

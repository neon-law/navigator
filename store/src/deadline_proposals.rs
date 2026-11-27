//! PostgreSQL holds immutable court-date drafts. A lawyer's decision queues the approved revision.
mod labels;
mod receipts;
use crate::postgres::PostgresDb;
use crate::project_deadlines::DeadlineError;
pub use labels::{available_evidence, source_labels};
pub use model::deadline_proposals::{Decision, Draft, Kind, Proposal, Role, Status};
pub use receipts::{evidence_receipt, SourceReceipt, VerifiedForgejo};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(&'static str),
    #[error("court-date proposal not found")]
    NotFound,
    #[error("the draft or decision has changed")]
    Conflict,
    #[error("confirmation requires a participating lawyer")]
    Forbidden,
    #[error("court-date storage unavailable")]
    Database(#[from] sqlx::Error),
    #[error("stored proposal is unreadable")]
    Record(#[from] serde_json::Error),
    #[error(transparent)]
    Deadline(#[from] DeadlineError),
}

fn decode(row: &sqlx::postgres::PgRow) -> Result<Proposal, Error> {
    let value: serde_json::Value = row.try_get("draft")?;
    let status = serde_json::from_value(serde_json::Value::String(row.try_get("status")?))?;
    Ok(Proposal {
        id: row.try_get("id")?,
        project_id: row.try_get("project_id")?,
        revision: row.try_get("revision")?,
        draft: serde_json::from_value(value)?,
        status,
        deadline_id: row.try_get("deadline_id")?,
        reason: row.try_get("reason")?,
    })
}

async fn latest(
    conn: &mut PgConnection,
    project: Uuid,
    id: Uuid,
) -> Result<Option<Proposal>, Error> {
    sqlx::query(
        "SELECT *
         FROM deadline_proposal
         WHERE project_id=$1
         AND id=$2
         ORDER BY revision DESC
         LIMIT 1",
    )
    .bind(project)
    .bind(id)
    .fetch_optional(conn)
    .await?
    .as_ref()
    .map(decode)
    .transpose()
}

// Serialize competing edits, publication, and the Deadline commands on this Project.
async fn lock(conn: &mut PgConnection, project: Uuid) -> Result<(), Error> {
    sqlx::query("SELECT pg_advisory_xact_lock(1312902739,hashtext($1::text))")
        .bind(project)
        .execute(conn)
        .await?;
    Ok(())
}

/// An omitted zone is frozen to the original revision on replay and to the reviewed draft on correction.
pub async fn resolve_draft(
    pg: &PostgresDb,
    db: &crate::application_store::ApplicationStore,
    project: Uuid,
    id: Uuid,
    revision: Option<i64>,
    input: model::deadline_proposals::DraftInput,
) -> Result<Draft, Error> {
    let timezone = if let Some(zone) = input.timezone {
        zone
    } else {
        let saved = sqlx::query_scalar::<_, String>(
            "SELECT draft->>'timezone'
         FROM deadline_proposal
         WHERE project_id=$1
         AND id=$2
         AND revision=$3",
        )
        .bind(project)
        .bind(id)
        .bind(revision.unwrap_or(1))
        .fetch_optional(pg.pool())
        .await?;
        match saved {
            Some(zone) => zone
                .parse()
                .map_err(|_| Error::Invalid("invalid saved proposal timezone"))?,
            None if revision.is_some() => return Err(Error::Conflict),
            None => {
                let row = crate::projects::find_by_id(db, project)
                    .await
                    .map_err(DeadlineError::from)?
                    .ok_or(Error::NotFound)?;
                crate::projects::calendar_timezone(db, &row)
                    .await
                    .map_err(DeadlineError::from)?
            }
        }
    };
    let draft = input.with_timezone(timezone);
    draft.validate().map_err(Error::Invalid)?;
    Ok(draft)
}
pub async fn get(pg: &PostgresDb, project: Uuid, id: Uuid) -> Result<Option<Proposal>, Error> {
    latest(&mut *pg.pool().acquire().await?, project, id).await
}
pub async fn get_revision(
    pg: &PostgresDb,
    project: Uuid,
    id: Uuid,
    revision: i64,
) -> Result<Option<Proposal>, Error> {
    sqlx::query("SELECT * FROM deadline_proposal WHERE project_id=$1 AND id=$2 AND revision=$3")
        .bind(project)
        .bind(id)
        .bind(revision)
        .fetch_optional(pg.pool())
        .await?
        .as_ref()
        .map(decode)
        .transpose()
}
pub async fn list(pg: &PostgresDb, project: Uuid) -> Result<Vec<Proposal>, Error> {
    sqlx::query(
        "SELECT DISTINCT ON (id) *
         FROM deadline_proposal
         WHERE project_id=$1
         ORDER BY id,revision DESC",
    )
    .bind(project)
    .fetch_all(pg.pool())
    .await?
    .iter()
    .map(decode)
    .collect()
}
struct Originals {
    citations: Vec<String>,
    assets: Vec<Uuid>,
}

// Lock the exact originals while a revision is staged, confirmed, or published.
async fn sources(
    conn: &mut PgConnection,
    project: Uuid,
    draft: &Draft,
) -> Result<Originals, Error> {
    draft.validate().map_err(Error::Invalid)?;
    let mut citations = Vec::new();
    let mut assets = Vec::new();
    for source in &draft.evidence {
        if source.role == Role::Authority {
            let (citation, class, asset) = sqlx::query_as::<_, (String, String, Uuid)>(
                "SELECT a.citation, a.class, s.id
                 FROM authority a JOIN asset s ON s.id = a.archived_asset_id
                 WHERE a.id = $1 AND s.sha256_hex = $2 AND s.project_id IS NULL
                   AND s.content_type IN ('application/pdf','image/png','image/jpeg','application/octet-stream')
                   AND s.deleted_at IS NULL AND s.purged_at IS NULL AND s.byte_size > 0
                   AND s.source IS DISTINCT FROM 'generated' AND s.derived_from IS NULL
                   AND s.kind IS DISTINCT FROM 'transcript'
                 FOR SHARE OF a, s",
            )
            .bind(source.id)
            .bind(&source.sha256_hex)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or(Error::Invalid("filed Authority archive version is unavailable"))?;
            if citation.trim().is_empty()
                || draft
                    .calculation
                    .as_ref()
                    .is_some_and(|rule| rule.authority_class() != Some(class.as_str()))
            {
                return Err(Error::Invalid(
                    "filed Authority does not support the stated authority kind",
                ));
            }
            if !citations.contains(&citation) {
                citations.push(citation);
            }
            assets.push(asset);
        } else {
            let document = sqlx::query_scalar::<_, Uuid>(
                "SELECT s.id FROM asset s
                 WHERE s.project_id = $1 AND s.id = $2 AND s.sha256_hex = $3
                   AND s.content_type IN ('application/pdf','image/png','image/jpeg','application/octet-stream')
                   AND s.byte_size > 0 AND s.deleted_at IS NULL AND s.purged_at IS NULL
                   AND s.source IS DISTINCT FROM 'generated' AND s.derived_from IS NULL
                   AND s.kind IS DISTINCT FROM 'transcript'
                   AND NOT EXISTS (
                     SELECT FROM asset newer WHERE newer.project_id = $1 AND newer.slug = s.slug
                       AND newer.deleted_at IS NULL AND newer.id > s.id)
                   AND (NOT $4 OR (s.kind = 'order' AND NOT EXISTS (
                     SELECT FROM efiling_transactions e WHERE e.project_id = $1 AND e.needs_entered_order
                       AND s.id IN (e.submitted_document_id,e.conformed_document_id))))
                 FOR SHARE OF s",
            )
            .bind(project)
            .bind(source.id)
            .bind(&source.sha256_hex)
            .bind(source.role == Role::SignedOrder)
            .fetch_optional(&mut *conn)
            .await?;
            if document.is_none() {
                return Err(Error::Invalid(
                    "evidence source version is unavailable in this Project",
                ));
            }
            assets.push(source.id);
        }
    }
    Ok(Originals { citations, assets })
}

async fn insert(
    conn: &mut PgConnection,
    project: Uuid,
    id: Uuid,
    rev: i64,
    target: Option<Uuid>,
    draft: &Draft,
    verified: &[VerifiedForgejo],
) -> Result<Proposal, Error> {
    let originals = sources(conn, project, draft).await?;
    let receipts = receipts::capture(conn, project, draft, &originals.assets, verified).await?;
    let row = sqlx::query(
        "INSERT INTO deadline_proposal(project_id,id,revision,deadline_id,draft,target_deadline_id,source_receipts)
         VALUES($1,$2,$3,$4,$5,CASE WHEN $3=1 THEN $4 ELSE NULL END,$6)
         RETURNING *",
    )
    .bind(project)
    .bind(id)
    .bind(rev)
    .bind(target)
    .bind(serde_json::to_value(draft)?)
    .bind(sqlx::types::Json(receipts))
    .fetch_one(conn)
    .await?;
    decode(&row)
}
pub async fn stage(
    pg: &PostgresDb,
    project: Uuid,
    id: Uuid,
    target: Option<Uuid>,
    draft: &Draft,
) -> Result<Proposal, Error> {
    stage_verified(pg, project, id, target, draft, &[]).await
}
pub async fn stage_verified(
    pg: &PostgresDb,
    project: Uuid,
    id: Uuid,
    target: Option<Uuid>,
    draft: &Draft,
    verified: &[VerifiedForgejo],
) -> Result<Proposal, Error> {
    let mut tx = pg.pool().begin().await?;
    lock(&mut tx, project).await?;
    if let Some(current) = latest(&mut tx, project, id).await? {
        let first = sqlx::query(
            "SELECT * FROM deadline_proposal WHERE project_id=$1 AND id=$2 AND revision=1",
        )
        .bind(project)
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        let original_target: Option<Uuid> = first.try_get("target_deadline_id")?;
        let first = decode(&first)?;
        if first.draft != *draft || original_target != target {
            return Err(Error::Conflict);
        }
        return Ok(current);
    }
    if let Some(target) = target {
        if sqlx::query_scalar::<_, Uuid>(
            "SELECT id
         FROM project_deadlines
         WHERE project_id=$1
         AND id=$2
         AND status='open'
         FOR UPDATE",
        )
        .bind(project)
        .bind(target)
        .fetch_optional(&mut *tx)
        .await?
        .is_none()
        {
            return Err(Error::Invalid(
                "target must be an open Deadline in this Project",
            ));
        }
        if sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT FROM deadline_proposal WHERE deadline_id=$1)",
        )
        .bind(target)
        .fetch_one(&mut *tx)
        .await?
        {
            return Err(Error::Conflict);
        }
    }
    let result = insert(&mut tx, project, id, 1, target, draft, verified).await?;
    tx.commit().await?;
    Ok(result)
}

pub async fn correct(
    pg: &PostgresDb,
    project: Uuid,
    id: Uuid,
    revision: i64,
    draft: &Draft,
) -> Result<Proposal, Error> {
    correct_verified(pg, project, id, revision, draft, &[]).await
}
pub async fn correct_verified(
    pg: &PostgresDb,
    project: Uuid,
    id: Uuid,
    revision: i64,
    draft: &Draft,
    verified: &[VerifiedForgejo],
) -> Result<Proposal, Error> {
    let mut tx = pg.pool().begin().await?;
    lock(&mut tx, project).await?;
    let current = latest(&mut tx, project, id).await?.ok_or(Error::NotFound)?;
    if current.revision != revision || current.status == Status::Approved {
        return Err(Error::Conflict);
    }
    sqlx::query(
        "UPDATE deadline_proposal
         SET next_attempt_at=now()
         WHERE project_id=$1
         AND id=$2
         AND revision=$3",
    )
    .bind(project)
    .bind(id)
    .bind(revision)
    .execute(&mut *tx)
    .await?;
    let result = insert(
        &mut tx,
        project,
        id,
        revision.checked_add(1).ok_or(Error::Conflict)?,
        current.deadline_id,
        draft,
        verified,
    )
    .await?;
    tx.commit().await?;
    Ok(result)
}

pub async fn decide(
    pg: &PostgresDb,
    project: Uuid,
    id: Uuid,
    actor: Uuid,
    revision: i64,
    decision: Decision,
) -> Result<Proposal, Error> {
    let mut tx = pg.pool().begin().await?;
    lock(&mut tx, project).await?;
    let authorized = sqlx::query_scalar::<_, Uuid>(
        "SELECT p.id
         FROM person p
         JOIN person_project_role r ON r.person_id=p.id
         WHERE r.project_id=$1
         AND p.id=$2
         AND p.role IN ('lawyer','admin','owner')
         AND NOT r.is_client_dri
         AND NOT (r.participation=ANY($3))
         FOR SHARE OF p,r",
    )
    .bind(project)
    .bind(actor)
    .bind(crate::projects::PARTICIPATION_CLIENT_SIDE)
    .fetch_optional(&mut *tx)
    .await?;
    if authorized.is_none() {
        return Err(Error::Forbidden);
    }
    let current = latest(&mut tx, project, id).await?.ok_or(Error::NotFound)?;
    if current.revision != revision {
        return Err(Error::Conflict);
    }
    let (state, reason) = match decision {
        Decision::Confirm if matches!(current.status, Status::Approved | Status::Published) => {
            return Ok(current)
        }
        Decision::Confirm if current.status == Status::Pending => {
            current.draft.reviewed_due().map_err(Error::Invalid)?;
            reviewed_sources(&mut tx, &current).await?;
            ("approved", None)
        }
        Decision::Return { reason } if reason.trim().is_empty() || reason.len() > 2000 => {
            return Err(Error::Invalid(
                "a return reason is required (up to 2000 bytes)",
            ))
        }
        Decision::Return { reason }
            if current.status == Status::Returned
                && current.reason.as_deref() == Some(reason.trim()) =>
        {
            return Ok(current)
        }
        Decision::Return { reason } if current.status == Status::Pending => {
            ("returned", Some(reason.trim().to_owned()))
        }
        _ => return Err(Error::Conflict),
    };
    let row = sqlx::query(
        "UPDATE deadline_proposal
         SET status=$4,reviewer_id=$5,reviewed_at=now(),next_attempt_at=now(),reason=$6
         WHERE project_id=$1
         AND id=$2
         AND revision=$3
         RETURNING *",
    )
    .bind(project)
    .bind(id)
    .bind(revision)
    .bind(state)
    .bind(actor)
    .bind(reason)
    .fetch_one(&mut *tx)
    .await?;
    let result = decode(&row)?;
    tx.commit().await?;
    Ok(result)
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct Job {
    pub project_id: Uuid,
    pub id: Uuid,
    pub revision: i64,
    pub attempt: i32,
}

#[cfg(test)]
pub(crate) async fn approved(pg: &PostgresDb) -> Result<Vec<Job>, Error> {
    let rows = sqlx::query(
        "SELECT project_id,id,revision,attempt
         FROM deadline_proposal
         WHERE status='approved'
         AND next_attempt_at<=now()
         ORDER BY next_attempt_at,id
         LIMIT 50",
    )
    .fetch_all(pg.pool())
    .await?;
    rows.iter().map(job_from_row).collect()
}
/// Only revision state, never the draft or cited material, enters a DBOS checkpoint.
pub async fn phase(pg: &PostgresDb, job: Job) -> Result<Status, Error> {
    let (revision, status): (i64, String) = sqlx::query_as(
        "SELECT revision, status FROM deadline_proposal
         WHERE project_id=$1 AND id=$2 ORDER BY revision DESC LIMIT 1",
    )
    .bind(job.project_id)
    .bind(job.id)
    .fetch_optional(pg.pool())
    .await?
    .ok_or(Error::NotFound)?;
    if revision != job.revision {
        return Ok(Status::Returned);
    }
    Ok(serde_json::from_value(status.into())?)
}

/// A stable outbox entry starts each revision, including a draft waiting on its lawyer.
/// Revisiting active entries also detects a stopped worker and repairs a lost wakeup.
pub async fn work(pg: &PostgresDb) -> Result<Vec<Job>, Error> {
    let rows = sqlx::query(
        "SELECT project_id,id,revision,attempt
         FROM deadline_proposal
         WHERE NOT worker_settled
         AND next_attempt_at<=now()
         ORDER BY next_attempt_at,id
         LIMIT 50",
    )
    .fetch_all(pg.pool())
    .await?;
    rows.iter().map(job_from_row).collect()
}
fn job_from_row(r: &sqlx::postgres::PgRow) -> Result<Job, Error> {
    Ok(Job {
        project_id: r.try_get("project_id")?,
        id: r.try_get("id")?,
        revision: r.try_get("revision")?,
        attempt: r.try_get("attempt")?,
    })
}

pub async fn observe(pg: &PostgresDb, job: Job, terminal: bool) -> Result<(), Error> {
    sqlx::query(
        "UPDATE deadline_proposal AS d
         SET worker_settled=$5
         AND (status IN ('returned','published') OR EXISTS(SELECT
         FROM deadline_proposal n
         WHERE (n.project_id,n.id)=(d.project_id,d.id)
         AND n.revision>d.revision)), attempt=attempt+CASE WHEN $5 THEN 1 ELSE 0 END, next_attempt_at=now()+CASE WHEN $5 THEN interval '1 minute'*least(60,attempt+1) ELSE interval '45 seconds' END
         WHERE project_id=$1
         AND id=$2
         AND revision=$3
         AND attempt=$4",
    )
        .bind(job.project_id)
        .bind(job.id)
        .bind(job.revision)
        .bind(job.attempt)
        .bind(terminal)
        .execute(pg.pool()).await?;
    Ok(())
}

pub async fn publish(pg: &PostgresDb, job: Job) -> Result<(), Error> {
    let mut tx = pg.pool().begin().await?;
    lock(&mut tx, job.project_id).await?;
    let row = latest(&mut tx, job.project_id, job.id)
        .await?
        .ok_or(Error::NotFound)?;
    if row.revision != job.revision || row.status != Status::Approved {
        return Ok(());
    }
    let result = match reviewed_sources(&mut tx, &row).await {
        Ok(citations) => crate::project_deadlines::publish_reviewed(&mut tx, &row, &citations)
            .await
            .map_err(Error::from),
        Err(error) => Err(error),
    };
    let deadline = match result {
        Ok(deadline) => deadline,
        Err(
            Error::Invalid(_)
            | Error::Deadline(DeadlineError::Invalid(_) | DeadlineError::NotFound),
        ) => {
            sqlx::query(
        "UPDATE deadline_proposal
         SET status='returned',reason='The approved evidence or target Deadline changed. Review the latest revision.'
         WHERE project_id=$1
         AND id=$2
         AND revision=$3",
    )
        .bind(job.project_id)
        .bind(job.id)
        .bind(job.revision)
        .execute(&mut *tx).await?;
            tx.commit().await?;
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    sqlx::query(
        "UPDATE deadline_proposal
         SET status='published',deadline_id=$4
         WHERE project_id=$1
         AND id=$2
         AND revision=$3
         AND status='approved'",
    )
    .bind(job.project_id)
    .bind(job.id)
    .bind(job.revision)
    .bind(deadline.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn reviewed_sources(conn: &mut PgConnection, row: &Proposal) -> Result<Vec<String>, Error> {
    let originals = sources(conn, row.project_id, &row.draft).await?;
    receipts::check(
        conn,
        row.project_id,
        row.id,
        row.revision,
        &row.draft,
        &originals.assets,
    )
    .await?;
    Ok(originals.citations)
}

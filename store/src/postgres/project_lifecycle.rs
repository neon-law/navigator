//! PostgreSQL Project lifecycle ledger and its current-state projection.

use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::PostgresDb;
use crate::projects::Project;

#[derive(Debug, serde::Serialize)]
pub struct StatusTransition {
    pub from_status: Option<String>,
    pub to_status: String,
    pub effective_at: Option<DateTime<Utc>>,
    pub recorded_at: Option<DateTime<Utc>>,
    pub reason: Option<String>,
    pub attesting_person_id: Option<Uuid>,
}

pub(crate) async fn lock(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<Option<Project>, super::projects::ProjectError> {
    sqlx::query("SELECT * FROM project WHERE id = $1 FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .as_ref()
        .map(super::projects::row)
        .transpose()
}

pub(crate) async fn append(
    tx: &mut Transaction<'_, Postgres>,
    project: &Project,
    event: &StatusTransition,
    closed_at: Option<&str>,
    closure_reason: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO project_status_transition
         (project_id, from_status, to_status, effective_at, recorded_at, reason,
          attesting_person_id, closed_at, closure_reason)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
    )
    .bind(project.id)
    .bind(&event.from_status)
    .bind(&event.to_status)
    .bind(event.effective_at)
    .bind(event.recorded_at)
    .bind(&event.reason)
    .bind(event.attesting_person_id)
    .bind(closed_at)
    .bind(closure_reason)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub(crate) async fn has_history(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM project_status_transition WHERE project_id=$1)",
    )
    .bind(id)
    .fetch_one(&mut **tx)
    .await
}

pub(crate) async fn history(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<Vec<StatusTransition>, sqlx::Error> {
    sqlx::query_as::<
        _,
        (
            Option<String>,
            String,
            Option<DateTime<Utc>>,
            Option<DateTime<Utc>>,
            Option<String>,
            Option<Uuid>,
        ),
    >(
        "SELECT from_status,to_status,effective_at,recorded_at,reason,attesting_person_id
         FROM project_status_transition WHERE project_id=$1 ORDER BY sequence",
    )
    .bind(id)
    .fetch_all(&mut **tx)
    .await
    .map(|rows| {
        rows.into_iter()
            .map(
                |(
                    from_status,
                    to_status,
                    effective_at,
                    recorded_at,
                    reason,
                    attesting_person_id,
                )| {
                    StatusTransition {
                        from_status,
                        to_status,
                        effective_at,
                        recorded_at,
                        reason,
                        attesting_person_id,
                    }
                },
            )
            .collect()
    })
}

/// Record only the lifecycle facts available on a Project that predates the ledger.
pub(crate) fn baseline(project: &Project) -> StatusTransition {
    let parse = |value: &str| value.parse::<DateTime<Utc>>().ok();
    let opened = (project.status == "open")
        .then(|| parse(&project.inserted_at))
        .flatten();
    StatusTransition {
        from_status: None,
        to_status: project.status.clone(),
        effective_at: if project.status == "closed" {
            project.closed_at.as_deref().and_then(parse)
        } else {
            opened
        },
        recorded_at: opened,
        reason: project.closure_reason.clone(),
        attesting_person_id: None,
    }
}

pub(crate) async fn overlay(pg: &PostgresDb, projects: &mut [Project]) -> Result<(), sqlx::Error> {
    let ids: Vec<_> = projects.iter().map(|project| project.id).collect();
    let rows = sqlx::query_as::<_, (Uuid, String, Option<String>, Option<String>)>(
        "SELECT id,status,closed_at,closure_reason FROM project
         WHERE id=ANY($1) AND EXISTS (
             SELECT 1 FROM project_status_transition WHERE project_id=project.id
         )",
    )
    .bind(ids)
    .fetch_all(pg.pool())
    .await?;
    let mut by_id: std::collections::HashMap<_, _> = rows
        .into_iter()
        .map(|(id, status, closed_at, reason)| (id, (status, closed_at, reason)))
        .collect();
    for project in projects {
        if let Some((status, closed_at, reason)) = by_id.remove(&project.id) {
            project.status = status;
            project.closed_at = closed_at;
            project.closure_reason = reason;
        }
    }
    Ok(())
}

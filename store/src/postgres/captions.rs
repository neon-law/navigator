//! PostgreSQL authority for the Project repository's synced Matter Caption.

use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;
use word::matter::Matter;

use super::{matter::MatterAuthorityError, PostgresDb};
use crate::matter_captions::MatterCaption;

fn caption(row: &sqlx::postgres::PgRow) -> Result<MatterCaption, MatterAuthorityError> {
    let mut value: Value = row.try_get("row")?;
    let id = serde_json::from_value(value["id"].take())?;
    let project_id = serde_json::from_value(value["project_id"].take())?;
    let synced_at = serde_json::from_value(value["synced_at"].take())?;
    if let Some(fields) = value.as_object_mut() {
        for key in ["id", "project_id", "firm_id", "synced_at"] {
            fields.remove(key);
        }
    }
    Ok(MatterCaption {
        id,
        project_id,
        synced_at,
        matter: serde_json::from_value(value)?,
    })
}

pub(crate) async fn find(
    pg: &PostgresDb,
    project: Uuid,
) -> Result<Option<MatterCaption>, MatterAuthorityError> {
    sqlx::query("SELECT to_jsonb(c) AS row FROM matter_caption c WHERE project_id = $1")
        .bind(project)
        .fetch_optional(pg.pool())
        .await?
        .as_ref()
        .map(caption)
        .transpose()
}

pub(crate) async fn replace(
    pg: &PostgresDb,
    project: Uuid,
    matter: &Matter,
) -> Result<MatterCaption, MatterAuthorityError> {
    let mut tx = pg.pool().begin().await?;
    // The Project lock serializes whole-caption replacement with ownership changes.
    let firm: Uuid = sqlx::query_scalar("SELECT firm_id FROM project WHERE id = $1 FOR UPDATE")
        .bind(project)
        .fetch_one(&mut *tx)
        .await?;
    let row = sqlx::query("INSERT INTO matter_caption AS c (id, project_id, firm_id, court, case_number, judicial_officer, department, filed_on, trial_on, signed_on, parties, counsel, synced_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, now()) ON CONFLICT (project_id) DO UPDATE SET id = EXCLUDED.id, firm_id = EXCLUDED.firm_id, court = EXCLUDED.court, case_number = EXCLUDED.case_number, judicial_officer = EXCLUDED.judicial_officer, department = EXCLUDED.department, filed_on = EXCLUDED.filed_on, trial_on = EXCLUDED.trial_on, signed_on = EXCLUDED.signed_on, parties = EXCLUDED.parties, counsel = EXCLUDED.counsel, synced_at = EXCLUDED.synced_at RETURNING to_jsonb(c) AS row")
        .bind(Uuid::now_v7()).bind(project).bind(firm).bind(&matter.court).bind(&matter.case_number)
        .bind(&matter.judicial_officer).bind(&matter.department).bind(&matter.filed_on)
        .bind(&matter.trial_on).bind(&matter.signed_on).bind(sqlx::types::Json(&matter.parties))
        .bind(sqlx::types::Json(&matter.counsel)).fetch_one(&mut *tx).await?;
    let result = caption(&row)?;
    tx.commit().await?;
    Ok(result)
}

pub(crate) async fn delete(pg: &PostgresDb, project: Uuid) -> Result<(), MatterAuthorityError> {
    sqlx::query("DELETE FROM matter_caption WHERE project_id = $1")
        .bind(project)
        .execute(pg.pool())
        .await?;
    Ok(())
}

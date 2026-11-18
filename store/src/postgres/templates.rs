//! PostgreSQL Template reads and atomic immutable version append.
use serde_json::Value;
use uuid::Uuid;

use crate::postgres::{matter::MatterAuthorityError, PostgresDb};
use crate::templates::{Saved, Scope, Template, TemplateError, Version};

fn decode(value: Value) -> Result<Template, TemplateError> {
    serde_json::from_value(value)
        .map_err(MatterAuthorityError::from)
        .map_err(Into::into)
}

fn decode_all(rows: Vec<Value>) -> Result<Vec<Template>, TemplateError> {
    rows.into_iter().map(decode).collect()
}

pub(crate) async fn find_by_id(
    pg: &PostgresDb,
    id: Uuid,
) -> Result<Option<Template>, TemplateError> {
    sqlx::query_scalar("SELECT to_jsonb(t) FROM template t WHERE id=$1")
        .bind(id)
        .fetch_optional(pg.pool())
        .await?
        .map(decode)
        .transpose()
}

pub(crate) async fn find_by_ids(
    pg: &PostgresDb,
    ids: &[Uuid],
) -> Result<Vec<Template>, TemplateError> {
    decode_all(
        sqlx::query_scalar("SELECT to_jsonb(t) FROM template t WHERE id=ANY($1) ORDER BY code ASC")
            .bind(ids)
            .fetch_all(pg.pool())
            .await?,
    )
}

pub(crate) async fn resolve(
    pg: &PostgresDb,
    scope: Scope,
    code: &str,
) -> Result<Option<Template>, TemplateError> {
    sqlx::query_scalar("SELECT to_jsonb(t) FROM template t WHERE current_key=$1 AND is_current")
        .bind(crate::templates::current_key_in(scope, code))
        .fetch_optional(pg.pool())
        .await?
        .map(decode)
        .transpose()
}

pub(crate) async fn owning_firm(
    pg: &PostgresDb,
    project: Uuid,
) -> Result<Option<Uuid>, TemplateError> {
    Ok(
        sqlx::query_scalar("SELECT firm_id FROM project WHERE id=$1")
            .bind(project)
            .fetch_optional(pg.pool())
            .await?
            .flatten(),
    )
}

pub(crate) async fn versions(
    pg: &PostgresDb,
    project: Option<Uuid>,
    code: &str,
) -> Result<Vec<Template>, TemplateError> {
    decode_all(sqlx::query_scalar("SELECT to_jsonb(t) FROM template t WHERE code=$1 AND project_id IS NOT DISTINCT FROM $2 AND firm_id IS NULL ORDER BY id DESC")
        .bind(code).bind(project).fetch_all(pg.pool()).await?)
}

pub(crate) async fn list(
    pg: &PostgresDb,
    scope: Option<Scope>,
) -> Result<Vec<Template>, TemplateError> {
    let (kind, id) = match scope {
        None => (None, None),
        Some(Scope::Shared) => (Some("shared"), None),
        Some(Scope::Firm(id)) => (Some("firm"), Some(id)),
        Some(Scope::Project(id)) => (Some("project"), Some(id)),
    };
    decode_all(sqlx::query_scalar("SELECT to_jsonb(t) FROM template t WHERE is_current AND (($1::text IS NULL AND firm_id IS NULL) OR ($1='shared' AND firm_id IS NULL AND project_id IS NULL) OR ($1='firm' AND firm_id=$2) OR ($1='project' AND project_id=$2)) ORDER BY code ASC")
        .bind(kind).bind(id).fetch_all(pg.pool()).await?)
}

pub(crate) async fn exists_for_project(
    pg: &PostgresDb,
    project: Uuid,
) -> Result<bool, TemplateError> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM template WHERE project_id=$1)")
            .bind(project)
            .fetch_one(pg.pool())
            .await?,
    )
}

fn classify(error: sqlx::Error) -> TemplateError {
    if error.as_database_error().is_some_and(|database| {
        database.is_unique_violation() && database.constraint() == Some("template_current_key_key")
    }) {
        TemplateError::CurrentVersionTaken
    } else {
        error.into()
    }
}

/// Serialize even an empty version line, then retire and append in one transaction.
pub(crate) async fn save_version(
    pg: &PostgresDb,
    scope: Scope,
    code: &str,
    version: &Version,
    body_sha256: Option<&str>,
) -> Result<Saved, TemplateError> {
    let key = crate::templates::current_key_in(scope, code);
    let mut tx = pg.pool().begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("template-version:{key}"))
        .execute(&mut *tx)
        .await?;
    let current: Option<Value> = sqlx::query_scalar(
        "SELECT to_jsonb(t) FROM template t WHERE current_key=$1 AND is_current FOR UPDATE",
    )
    .bind(&key)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(current) = current.map(decode).transpose()? {
        if crate::templates::matches_spec(&current, version) {
            tx.commit().await?;
            return Ok(Saved::Unchanged(current));
        }
        let retired = sqlx::query("UPDATE template SET is_current=false,current_key=NULL,updated_at=now() WHERE id=$1 AND current_key=$2 AND is_current")
            .bind(current.id).bind(&key).execute(&mut *tx).await?;
        if retired.rows_affected() != 1 {
            return Err(TemplateError::CurrentVersionTaken);
        }
    }
    let row = sqlx::query_scalar("INSERT INTO template AS t (id,code,title,respondent_type,project_id,firm_id,is_current,asset_id,form_code,kind,source_commit_sha,body_sha256,current_key,inserted_at,updated_at) VALUES ($1,$2,$3,$4,$5,$6,true,$7,$8,$9,$10,$11,$12,now(),now()) RETURNING to_jsonb(t)")
        .bind(Uuid::now_v7()).bind(code).bind(&version.title).bind(&version.respondent_type)
        .bind(scope.project_id()).bind(scope.firm_id()).bind(version.asset_id).bind(&version.form_code)
        .bind(&version.kind).bind(&version.source_commit_sha).bind(body_sha256).bind(key)
        .fetch_one(&mut *tx).await.map_err(classify)?;
    let row = decode(row)?;
    tx.commit().await?;
    Ok(Saved::Written(row))
}

#[cfg(test)]
#[path = "templates_tests.rs"]
mod tests;

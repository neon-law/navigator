//! Atomic manifest replacement with a Project-derived Firm.
use super::{matter::MatterAuthorityError, PostgresDb};
use crate::matter_pages::{MatterPage, MatterPageError};
use rules::{dashboard::Lens, pages::PageEntry};
use uuid::Uuid;

const SELECT:&str="SELECT to_jsonb(r)||jsonb_build_object('entry',to_jsonb(r)||jsonb_build_object('media',coalesce(r.media,'[]'::jsonb),'lenses',jsonb_strip_nulls(jsonb_build_object('client',r.client_sections,'lawyer',r.lawyer_sections,'clerk',r.clerk_sections)))) FROM matter_page r WHERE project_id=$1 ORDER BY position,id";

pub(crate) async fn list(
    pg: &PostgresDb,
    project: Uuid,
) -> Result<Vec<MatterPage>, MatterPageError> {
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(SELECT)
        .bind(project)
        .fetch_all(pg.pool())
        .await?;
    rows.into_iter()
        .map(|value| {
            serde_json::from_value(value)
                .map_err(MatterAuthorityError::from)
                .map_err(Into::into)
        })
        .collect()
}
async fn owner(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    project: Uuid,
) -> Result<Uuid, MatterPageError> {
    let firm: Option<Uuid> =
        sqlx::query_scalar("SELECT firm_id FROM project WHERE id=$1 FOR UPDATE")
            .bind(project)
            .fetch_one(&mut **tx)
            .await?;
    firm.ok_or_else(|| MatterAuthorityError::Unavailable.into())
}
pub(crate) async fn replace(
    pg: &PostgresDb,
    project: Uuid,
    pages: &[PageEntry],
) -> Result<Vec<MatterPage>, MatterPageError> {
    let mut tx = pg.pool().begin().await?;
    let firm = owner(&mut tx, project).await?;
    sqlx::query("DELETE FROM matter_page WHERE project_id=$1")
        .bind(project)
        .execute(&mut *tx)
        .await?;
    let now = chrono::Utc::now();
    for (position, page) in pages.iter().enumerate() {
        let position = i64::try_from(position)
            .map_err(|_| MatterPageError::Invalid(vec!["Too many Matter Pages".into()]))?;
        let lens = |lens: Lens| page.lenses.get(lens.as_str()).cloned();
        sqlx::query("INSERT INTO matter_page(id,project_id,firm_id,slug,kind,title,draft,document_revision,media,position,client_sections,lawyer_sections,clerk_sections,synced_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)")
            .bind(Uuid::now_v7()).bind(project).bind(firm).bind(&page.slug).bind(&page.kind).bind(&page.title).bind(&page.draft).bind(&page.document_revision).bind(sqlx::types::Json(&page.media)).bind(position).bind(lens(Lens::Client)).bind(lens(Lens::Lawyer)).bind(lens(Lens::Clerk)).bind(now).execute(&mut *tx).await?;
    }
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(SELECT)
        .bind(project)
        .fetch_all(&mut *tx)
        .await?;
    let rows = rows
        .into_iter()
        .map(|value| serde_json::from_value(value).map_err(MatterAuthorityError::from))
        .collect::<Result<Vec<_>, _>>()?;
    tx.commit().await?;
    Ok(rows)
}
pub(crate) async fn delete(pg: &PostgresDb, project: Uuid) -> Result<(), MatterPageError> {
    let mut tx = pg.pool().begin().await?;
    // The matter-delete cascade may already have removed its Project.
    sqlx::query("SELECT id FROM project WHERE id=$1 FOR UPDATE")
        .bind(project)
        .fetch_optional(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM matter_page WHERE project_id=$1")
        .bind(project)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}
#[cfg(test)]
#[path = "matter_pages_tests.rs"]
mod tests;

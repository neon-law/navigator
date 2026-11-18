//! Opt-in PostgreSQL mirror of a Project's synced
//! [Matter Pages](../../../docs/glossary/matter-page.md) and
//! [Matter Caption](../../../docs/glossary/matter-caption.md).
//!
//! Captured Page imports preserve target-only identities and report them as drift.
//! Native manifest replacement owns removal. Matter Captions are PostgreSQL-authoritative:
//! their first backfill inserts missing captions, and later source reads cannot
//! overwrite or delete them. [`MirrorKey::MatterPages`] identifies the page set;
//! [`MirrorKey::MatterCaption`] identifies the Project's caption.
//!
//! Each row carries its Project's Firm under a compound Project/Firm foreign
//! key, so PostgreSQL refuses a row whose Firm is not its Project's. A Project
//! with no Firm is refused before any write and stays in the parity report.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use sqlx::{postgres::PgRow, types::Json, Row};
use surrealdb::types::{RecordId, SurrealValue};
use uuid::Uuid;
use word::matter::{Counsel, Party};

use super::{
    changed, identity, micros, tenancy, Columns, MirrorError, MirrorKey, Mirrored, ParityReport,
};
use crate::matter_captions::{CounselRow, PartyRow};
use crate::postgres::PostgresDb;
use crate::postgres_backfill::family::{self, ImportMode};
use crate::surreal::{record_id, record_uuid, SurrealDb};

/// One Matter Page as both stores compare it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PageImage {
    id: Uuid,
    project_id: Uuid,
    /// The Project's Firm; `None` only on a source Project that has none.
    firm_id: Option<Uuid>,
    slug: String,
    kind: String,
    title: String,
    draft: Option<String>,
    document_revision: Option<String>,
    media: Option<serde_json::Value>,
    position: i64,
    client_sections: Option<Vec<String>>,
    lawyer_sections: Option<Vec<String>>,
    clerk_sections: Option<Vec<String>>,
    synced_at: DateTime<Utc>,
}

/// One Matter Caption as both stores compare it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CaptionImage {
    id: Uuid,
    project_id: Uuid,
    /// The Project's Firm; `None` only on a source Project that has none.
    firm_id: Option<Uuid>,
    court: Vec<String>,
    case_number: String,
    judicial_officer: String,
    department: String,
    filed_on: String,
    trial_on: Option<String>,
    signed_on: Option<String>,
    parties: Vec<Party>,
    counsel: Counsel,
    synced_at: DateTime<Utc>,
}

#[derive(SurrealValue)]
struct PageSource {
    id: RecordId,
    project_id: RecordId,
    firm_id: Option<RecordId>,
    slug: String,
    kind: String,
    title: String,
    draft: Option<String>,
    document_revision: Option<String>,
    media: Option<serde_json::Value>,
    position: i64,
    client_sections: Option<Vec<String>>,
    lawyer_sections: Option<Vec<String>>,
    clerk_sections: Option<Vec<String>>,
    synced_at: String,
}

#[derive(SurrealValue)]
struct CaptionSource {
    id: RecordId,
    project_id: RecordId,
    firm_id: Option<RecordId>,
    court: Vec<String>,
    case_number: String,
    judicial_officer: String,
    department: String,
    filed_on: String,
    trial_on: Option<String>,
    signed_on: Option<String>,
    parties: Vec<PartyRow>,
    counsel: CounselRow,
    synced_at: String,
}

fn link(id: &RecordId) -> Result<Uuid, MirrorError> {
    record_uuid(id).ok_or(MirrorError::Source)
}

/// `synced_at` is RFC 3339 text in SurrealDB, compared at microseconds.
fn synced(at: &str) -> Result<DateTime<Utc>, MirrorError> {
    DateTime::parse_from_rfc3339(at)
        .map(|at| micros(at.with_timezone(&Utc)))
        .map_err(|_| MirrorError::SourceTimestamp)
}

impl TryFrom<PageSource> for PageImage {
    type Error = MirrorError;
    fn try_from(row: PageSource) -> Result<Self, Self::Error> {
        Ok(Self {
            id: link(&row.id)?,
            project_id: link(&row.project_id)?,
            firm_id: row.firm_id.as_ref().map(link).transpose()?,
            slug: row.slug,
            kind: row.kind,
            title: row.title,
            draft: row.draft,
            document_revision: row.document_revision,
            media: row.media,
            position: row.position,
            client_sections: row.client_sections,
            lawyer_sections: row.lawyer_sections,
            clerk_sections: row.clerk_sections,
            synced_at: synced(&row.synced_at)?,
        })
    }
}

impl TryFrom<CaptionSource> for CaptionImage {
    type Error = MirrorError;
    fn try_from(row: CaptionSource) -> Result<Self, Self::Error> {
        Ok(Self {
            id: link(&row.id)?,
            project_id: link(&row.project_id)?,
            firm_id: row.firm_id.as_ref().map(link).transpose()?,
            court: row.court,
            case_number: row.case_number,
            judicial_officer: row.judicial_officer,
            department: row.department,
            filed_on: row.filed_on,
            trial_on: row.trial_on,
            signed_on: row.signed_on,
            parties: row.parties.into_iter().map(Party::from).collect(),
            counsel: row.counsel.into(),
            synced_at: synced(&row.synced_at)?,
        })
    }
}

const PAGE_SOURCE: &str = "SELECT id, project_id, project_id.firm_id AS firm_id, slug, kind, \
     title, draft, document_revision, media, position, client_sections, lawyer_sections, clerk_sections, synced_at \
     FROM matter_page";
const CAPTION_SOURCE: &str = "SELECT id, project_id, project_id.firm_id AS firm_id, court, \
     case_number, judicial_officer, department, filed_on, trial_on, signed_on, parties, counsel, synced_at \
     FROM matter_caption";

/// Run one source `SELECT`, every row or one Project's.
async fn source<T: SurrealValue>(
    surreal: &SurrealDb,
    select: &'static str,
    project_id: Option<Uuid>,
) -> Result<Vec<T>, MirrorError> {
    let request = match project_id {
        Some(id) => surreal
            .query(format!("{select} WHERE project_id = $project"))
            .bind(("project", record_id(crate::projects::PROJECT_TABLE, id))),
        None => surreal.query(select),
    };
    request
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?
        .take(0)
        .map_err(|_| MirrorError::Source)
}

/// Pages grouped by Project, each set in declared order.
fn page_sets(pages: Vec<PageImage>) -> BTreeMap<Uuid, Vec<PageImage>> {
    let mut sets: BTreeMap<Uuid, Vec<PageImage>> = BTreeMap::new();
    for page in pages {
        sets.entry(page.project_id).or_default().push(page);
    }
    for set in sets.values_mut() {
        set.sort_by_key(|page| (page.position, page.id));
    }
    sets
}

async fn source_pages(
    surreal: &SurrealDb,
    project_id: Option<Uuid>,
) -> Result<BTreeMap<Uuid, Vec<PageImage>>, MirrorError> {
    let rows: Vec<PageSource> = source(surreal, PAGE_SOURCE, project_id).await?;
    Ok(page_sets(
        rows.into_iter()
            .map(PageImage::try_from)
            .collect::<Result<_, _>>()?,
    ))
}

async fn source_captions(
    surreal: &SurrealDb,
    project_id: Option<Uuid>,
) -> Result<BTreeMap<Uuid, CaptionImage>, MirrorError> {
    let rows: Vec<CaptionSource> = source(surreal, CAPTION_SOURCE, project_id).await?;
    rows.into_iter()
        .map(|row| CaptionImage::try_from(row).map(|image| (image.project_id, image)))
        .collect()
}

fn page_from_row(row: &PgRow) -> Result<PageImage, sqlx::Error> {
    Ok(PageImage {
        id: row.try_get("id")?,
        project_id: row.try_get("project_id")?,
        firm_id: Some(row.try_get("firm_id")?),
        slug: row.try_get("slug")?,
        kind: row.try_get("kind")?,
        title: row.try_get("title")?,
        draft: row.try_get("draft")?,
        document_revision: row.try_get("document_revision")?,
        media: row.try_get("media")?,
        position: row.try_get("position")?,
        client_sections: row.try_get("client_sections")?,
        lawyer_sections: row.try_get("lawyer_sections")?,
        clerk_sections: row.try_get("clerk_sections")?,
        synced_at: row.try_get("synced_at")?,
    })
}

fn caption_from_row(row: &PgRow) -> Result<CaptionImage, sqlx::Error> {
    let parties: Json<Vec<Party>> = row.try_get("parties")?;
    let counsel: Json<Counsel> = row.try_get("counsel")?;
    Ok(CaptionImage {
        id: row.try_get("id")?,
        project_id: row.try_get("project_id")?,
        firm_id: Some(row.try_get("firm_id")?),
        court: row.try_get("court")?,
        case_number: row.try_get("case_number")?,
        judicial_officer: row.try_get("judicial_officer")?,
        department: row.try_get("department")?,
        filed_on: row.try_get("filed_on")?,
        trial_on: row.try_get("trial_on")?,
        signed_on: row.try_get("signed_on")?,
        parties: parties.0,
        counsel: counsel.0,
        synced_at: row.try_get("synced_at")?,
    })
}

async fn target_pages(
    pg: &PostgresDb,
    project_id: Option<Uuid>,
) -> Result<BTreeMap<Uuid, Vec<PageImage>>, MirrorError> {
    let rows = sqlx::query(
        "SELECT id, project_id, firm_id, slug, kind, title, draft, document_revision, media, position, client_sections, \
         lawyer_sections, clerk_sections, synced_at FROM matter_page \
         WHERE $1::uuid IS NULL OR project_id = $1",
    )
    .bind(project_id)
    .fetch_all(&pg.0)
    .await?;
    Ok(page_sets(
        rows.iter()
            .map(page_from_row)
            .collect::<Result<_, sqlx::Error>>()?,
    ))
}

async fn target_captions(pg: &PostgresDb) -> Result<BTreeMap<Uuid, CaptionImage>, MirrorError> {
    let rows = sqlx::query(
        "SELECT id, project_id, firm_id, court, case_number, judicial_officer, department, \
         filed_on, trial_on, signed_on, parties, counsel, synced_at FROM matter_caption",
    )
    .fetch_all(&pg.0)
    .await?;
    rows.iter()
        .map(|row| {
            let image = caption_from_row(row)?;
            Ok((image.project_id, image))
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(Into::into)
}

/// The Firm a Project's rows belong to, with that Firm and the Project
/// itself present in PostgreSQL. The Project is re-mirrored so a Firm move
/// lands before its rows do. A Project with no Firm is refused.
async fn ensure_owner(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    project_id: Uuid,
    firm_id: Option<Uuid>,
) -> Result<Uuid, MirrorError> {
    let firm_id = firm_id.ok_or(MirrorError::Source)?;
    let firm_key = MirrorKey::Firm(firm_id);
    if !super::held_by_target(pg, &firm_key).await? {
        tenancy::mirror(surreal, pg, &firm_key).await?;
    }
    let project = crate::projects::find_by_id(surreal, project_id)
        .await
        .map_err(|_| MirrorError::Source)?
        .ok_or(MirrorError::Source)?;
    if let Some(jurisdiction_id) = project.jurisdiction_id {
        identity::ensure(surreal, pg, MirrorKey::Jurisdiction(jurisdiction_id)).await?;
    }
    tenancy::mirror(surreal, pg, &MirrorKey::Project(project_id)).await?;
    Ok(firm_id)
}

const PAGE_UPSERT: &str = "INSERT INTO matter_page (id, project_id, firm_id, slug, kind, title, \
     draft, position, client_sections, lawyer_sections, clerk_sections, synced_at, document_revision, media) \
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) \
     ON CONFLICT (id) DO UPDATE SET project_id = EXCLUDED.project_id, \
     firm_id = EXCLUDED.firm_id, slug = EXCLUDED.slug, kind = EXCLUDED.kind, \
     title = EXCLUDED.title, draft = EXCLUDED.draft, position = EXCLUDED.position, \
     document_revision = EXCLUDED.document_revision, media = EXCLUDED.media, \
     client_sections = EXCLUDED.client_sections, lawyer_sections = EXCLUDED.lawyer_sections, \
     clerk_sections = EXCLUDED.clerk_sections, synced_at = EXCLUDED.synced_at \
     WHERE (matter_page.project_id, matter_page.firm_id, matter_page.slug, matter_page.kind, \
     matter_page.title, matter_page.draft, matter_page.position, matter_page.client_sections, \
     matter_page.lawyer_sections, matter_page.clerk_sections, matter_page.synced_at, matter_page.document_revision, matter_page.media) \
     IS DISTINCT FROM (EXCLUDED.project_id, EXCLUDED.firm_id, EXCLUDED.slug, EXCLUDED.kind, \
     EXCLUDED.title, EXCLUDED.draft, EXCLUDED.position, EXCLUDED.client_sections, \
     EXCLUDED.lawyer_sections, EXCLUDED.clerk_sections, EXCLUDED.synced_at, EXCLUDED.document_revision, EXCLUDED.media)";

const CAPTION_UPSERT: &str = "INSERT INTO matter_caption (id, project_id, firm_id, court, case_number, judicial_officer, department, filed_on, trial_on, parties, counsel, synced_at, signed_on) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) ON CONFLICT (project_id) DO UPDATE SET id=EXCLUDED.id, firm_id=EXCLUDED.firm_id, court=EXCLUDED.court, case_number=EXCLUDED.case_number, judicial_officer=EXCLUDED.judicial_officer, department=EXCLUDED.department, filed_on=EXCLUDED.filed_on, trial_on=EXCLUDED.trial_on, parties=EXCLUDED.parties, counsel=EXCLUDED.counsel, synced_at=EXCLUDED.synced_at, signed_on=EXCLUDED.signed_on";

/// Copy captured identities without treating Source absence as deletion evidence.
async fn write_pages(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    project_id: Uuid,
    pages: &[PageImage],
    mode: ImportMode,
) -> Result<Mirrored, MirrorError> {
    let source_project = crate::projects::find_by_id_in_surreal(surreal, project_id)
        .await
        .map_err(|_| MirrorError::Source)?
        .ok_or(MirrorError::Source)?;
    let firm = source_project.firm_id.ok_or(MirrorError::Source)?;
    if pages
        .iter()
        .any(|page| page.project_id != project_id || page.firm_id != Some(firm))
    {
        return Err(MirrorError::Source);
    }
    let project_key = MirrorKey::Project(project_id);
    if !super::held_by_target(pg, &project_key).await? {
        if mode != ImportMode::InsertMissing {
            return Err(MirrorError::Source);
        }
        for key in [MirrorKey::Firm(firm), project_key] {
            if !super::held_by_target(pg, &key).await? {
                Box::pin(super::mirror(surreal, pg, &key)).await?;
            }
        }
    }
    let mut tx = pg.pool().begin().await?;
    let target_firm: Option<Uuid> =
        sqlx::query_scalar("SELECT firm_id FROM project WHERE id=$1 FOR UPDATE")
            .bind(project_id)
            .fetch_one(&mut *tx)
            .await?;
    if target_firm != Some(firm) {
        return Err(MirrorError::Source);
    }
    let statement = if mode == ImportMode::InsertMissing {
        let (insert, _) = PAGE_UPSERT
            .split_once(" ON CONFLICT")
            .ok_or(MirrorError::Source)?;
        format!("{insert} ON CONFLICT(id) DO NOTHING")
    } else {
        PAGE_UPSERT.to_owned()
    };
    let mut written = 0;
    for page in pages {
        let wrong_owner: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM matter_page WHERE id=$1 AND project_id<>$2)",
        )
        .bind(page.id)
        .bind(project_id)
        .fetch_one(&mut *tx)
        .await?;
        if wrong_owner {
            return Err(MirrorError::Source);
        }
        written += sqlx::query(sqlx::AssertSqlSafe(statement.as_str()))
            .bind(page.id)
            .bind(project_id)
            .bind(firm)
            .bind(&page.slug)
            .bind(&page.kind)
            .bind(&page.title)
            .bind(&page.draft)
            .bind(page.position)
            .bind(&page.client_sections)
            .bind(&page.lawyer_sections)
            .bind(&page.clerk_sections)
            .bind(page.synced_at)
            .bind(&page.document_revision)
            .bind(&page.media)
            .execute(&mut *tx)
            .await?
            .rows_affected();
    }
    tx.commit().await?;
    Ok(changed(written, Mirrored::Written))
}

async fn write_caption(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    project_id: Uuid,
    caption: Option<&CaptionImage>,
) -> Result<Mirrored, MirrorError> {
    let mode = family::import_mode(pg, "matter_caption").await?;
    if mode == ImportMode::Refuse {
        return Err(MirrorError::MatterAuthority);
    }
    let caption = caption.ok_or(MirrorError::Source)?;
    let firm_id = if mode == ImportMode::InsertMissing {
        ensure_owner(surreal, pg, project_id, caption.firm_id).await?
    } else {
        caption.firm_id.ok_or(MirrorError::Source)?
    };
    let statement = if mode == ImportMode::InsertMissing {
        let (insert, _) = CAPTION_UPSERT
            .split_once(" ON CONFLICT")
            .ok_or(MirrorError::Source)?;
        format!("{insert} ON CONFLICT (project_id) DO NOTHING")
    } else {
        CAPTION_UPSERT.to_owned()
    };
    let mut tx = pg.0.begin().await?;
    let written = sqlx::query(sqlx::AssertSqlSafe(statement))
        .bind(caption.id)
        .bind(caption.project_id)
        .bind(firm_id)
        .bind(&caption.court)
        .bind(&caption.case_number)
        .bind(&caption.judicial_officer)
        .bind(&caption.department)
        .bind(&caption.filed_on)
        .bind(&caption.trial_on)
        .bind(Json(&caption.parties))
        .bind(Json(&caption.counsel))
        .bind(caption.synced_at)
        .bind(&caption.signed_on)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    Ok(changed(written, Mirrored::Written))
}

/// Import one Project's captured page set or caption under its receipt.
pub(super) async fn mirror(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    key: &MirrorKey,
) -> Result<Mirrored, MirrorError> {
    match key {
        MirrorKey::MatterPages(project_id) => {
            let mode = family::import_mode(pg, "matter_page").await?;
            if mode == ImportMode::Refuse {
                return Err(MirrorError::MatterAuthority);
            }
            super::from_source(async {
                let pages = source_pages(surreal, Some(*project_id))
                    .await?
                    .remove(project_id)
                    .unwrap_or_default();
                write_pages(surreal, pg, *project_id, &pages, mode).await
            })
            .await
        }
        MirrorKey::MatterCaption(project_id) => {
            if family::import_mode(pg, "matter_caption").await? == ImportMode::Refuse {
                return Err(MirrorError::MatterAuthority);
            }
            let caption = source_captions(surreal, Some(*project_id))
                .await?
                .remove(project_id);
            write_caption(surreal, pg, *project_id, caption.as_ref()).await
        }
        _ => Err(MirrorError::Source),
    }
}

fn keyed_pages(sets: BTreeMap<Uuid, Vec<PageImage>>) -> BTreeMap<MirrorKey, Columns> {
    sets.into_iter()
        .map(|(project_id, set)| {
            (
                MirrorKey::MatterPages(project_id),
                Columns::MatterPages(set),
            )
        })
        .collect()
}

fn keyed_captions(captions: BTreeMap<Uuid, CaptionImage>) -> BTreeMap<MirrorKey, Columns> {
    captions
        .into_iter()
        .map(|(project_id, caption)| {
            (
                MirrorKey::MatterCaption(project_id),
                Columns::MatterCaption(Box::new(caption)),
            )
        })
        .collect()
}

/// Compare every Project's page set and caption in both stores. A page set
/// diverges as a whole: a missing, extra, reordered, or changed page names
/// its Project. Reports name keys only.
pub async fn parity(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let mut tables = vec![scoped_page_parity(surreal, pg).await?];
    if !family::active_for_table(pg, "matter_caption").await? {
        if family::importing("matter_caption") {
            tables.push(caption_parity(surreal, pg).await?);
        } else if super::compares_authoritative(pg).await? {
            tables.push(super::only_missing(pg, caption_parity(surreal, pg).await?).await?);
        }
    }
    Ok(ParityReport { tables })
}

/// Compare full Page images and count Page rows rather than Project groups.
pub(crate) async fn scoped_page_parity(
    surreal: &SurrealDb,
    pg: &PostgresDb,
) -> Result<super::TableParity, MirrorError> {
    let source = source_pages(surreal, None).await?;
    let target = target_pages(pg, None).await?;
    let source_rows = source.values().map(Vec::len).sum();
    let target_rows = target.values().map(Vec::len).sum();
    let mut report = super::compare("matter_page", &keyed_pages(source), &keyed_pages(target));
    report.source_rows = source_rows;
    report.target_rows = target_rows;
    Ok(report)
}

async fn source_parent_ids(
    surreal: &SurrealDb,
    table: &'static str,
) -> Result<BTreeSet<Uuid>, MirrorError> {
    super::select_table(surreal, table, format!("SELECT VALUE id FROM `{table}`"))
        .await?
        .into_iter()
        .map(|value| {
            let surrealdb::types::Value::RecordId(id) = value else {
                return Err(MirrorError::Source);
            };
            record_uuid(&id).ok_or(MirrorError::Source)
        })
        .collect()
}

/// Count each Page's Project link and derived `project_id.firm_id` owner proof.
/// `firm_id` names the canonical PostgreSQL foreign key, never a Source Page field.
pub(crate) async fn scoped_page_references(
    surreal: &SurrealDb,
    pg: &PostgresDb,
) -> Result<Vec<super::records::UnresolvedReference>, MirrorError> {
    let source_projects = source_parent_ids(surreal, "project").await?;
    let source_firms = source_parent_ids(surreal, "firm").await?;
    let projects: Vec<(Uuid, Option<Uuid>)> = sqlx::query_as("SELECT id,firm_id FROM project")
        .fetch_all(pg.pool())
        .await?;
    let projects: BTreeMap<_, _> = projects.into_iter().collect();
    let firms: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM firm")
        .fetch_all(pg.pool())
        .await?;
    let firms: BTreeSet<_> = firms.into_iter().collect();
    let mut counts = ["project_id", "firm_id"].map(|field| super::records::UnresolvedReference {
        table: "matter_page",
        field,
        source_orphans: 0,
        postgres_missing: 0,
        newly_target_missing: 0,
    });
    for page in source_pages(surreal, None).await?.into_values().flatten() {
        let source_project = source_projects.contains(&page.project_id);
        let target_project = projects.contains_key(&page.project_id);
        if !source_project || !target_project {
            counts[0].observe(source_project, target_project);
        }
        let source_owner = source_project
            && page
                .firm_id
                .is_some_and(|firm| source_firms.contains(&firm));
        let target_owner = page.firm_id.is_some_and(|firm| {
            projects.get(&page.project_id) == Some(&Some(firm)) && firms.contains(&firm)
        });
        if !source_owner || !target_owner {
            counts[1].observe(source_owner, target_owner);
        }
    }
    Ok(counts
        .into_iter()
        .filter(|count| count.source_orphans != 0 || count.postgres_missing != 0)
        .collect())
}

/// Compare the enrolled caption without reading unrelated Matter Pages.
pub(crate) async fn caption_parity(
    surreal: &SurrealDb,
    pg: &PostgresDb,
) -> Result<super::TableParity, MirrorError> {
    Ok(super::compare(
        "matter_caption",
        &keyed_captions(source_captions(surreal, None).await?),
        &keyed_captions(target_captions(pg).await?),
    ))
}

/// Re-mirror every divergent Project, then compare again. A Project that
/// still fails, such as one with no Firm, stays in the report.
pub async fn reconcile(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let before = parity(surreal, pg).await?;
    for key in before.divergent() {
        if let Err(error) = mirror(surreal, pg, key).await {
            tracing::warn!(key = %key, error = %error, "PostgreSQL reconcile could not mirror the Project's matter rows");
        }
    }
    parity(surreal, pg).await
}

#[cfg(test)]
#[path = "matter_pages_captions_tests.rs"]
mod tests;

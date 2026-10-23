//! Firm, Brand, Project, and membership rows in the PostgreSQL mirror.
//!
//! The same contract as the Post mirror: SurrealDB is read back after its
//! write, and PostgreSQL's copy is upserted or deleted to match. Each row
//! keeps its Surreal record UUID as its PostgreSQL `id`. A natural key held in
//! PostgreSQL by a different id (a Firm slug, a Brand key, a Project code, a
//! membership pair) is a row SurrealDB no longer has under that identity, so
//! SurrealDB's row replaces it.
//!
//! Person and Entity rows are identity storage ([`super::identity`]). Before
//! a Firm, Project, or membership is upserted, each Person or Entity it names
//! is mirrored first when PostgreSQL does not yet hold it. A reference
//! SurrealDB cannot supply (a dangling link) is still refused with SQLSTATE
//! 23503 and reported by [`parity`], the same as any other failed mirror.
//!
//! Surreal timestamps are RFC 3339 text. One that does not parse is refused
//! as [`MirrorError::SourceTimestamp`]; a time is never substituted.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use chrono::{DateTime, SubsecRound, Utc};
use sqlx::Row;
use uuid::Uuid;

use super::{changed, identity, MirrorError, MirrorKey, Mirrored, ParityReport, TableParity};
use crate::postgres::PostgresDb;
use crate::postgres_shadow::{compare_row, Comparison, ShadowKey};
use crate::surreal::SurrealDb;

const FIRM_COLUMNS: &[&str] = &[
    "id",
    "name",
    "slug",
    "status",
    "entity_id",
    "inserted_at",
    "updated_at",
];
const BRAND_COLUMNS: &[&str] = &[
    "id",
    "name",
    "brand_key",
    "firm_id",
    "primary_color",
    "accent_color",
    "typeface",
    "is_law_firm",
    "legal_entity",
    "logo_object_key",
    "logo_content_type",
    "font_family",
    "font_object_key",
    "font_licence",
    "website_url",
    "inserted_at",
    "updated_at",
];
const FIRM_BRAND_COLUMNS: &[&str] = &["id", "firm_id", "brand_key", "inserted_at", "updated_at"];
const PROJECT_COLUMNS: &[&str] = &[
    "id",
    "code",
    "name",
    "status",
    "brand",
    "entity_id",
    "firm_id",
    "jurisdiction_id",
    "description",
    "drive_folder_id",
    "repository_url",
    "git_initialized_at",
    "forge_provisioned_at",
    "closed_at",
    "closure_reason",
    "internal_slack_channel_url",
    "external_slack_channel_url",
    "internal_slack_channel_id",
    "private_notion_page_url",
    "shared_notion_page_url",
    "inserted_at",
    "updated_at",
];
const MEMBERSHIP_COLUMNS: &[&str] = &[
    "id",
    "person_id",
    "firm_id",
    "membership",
    "is_dri",
    "inserted_at",
    "updated_at",
];
const PARTICIPATION_COLUMNS: &[&str] = &[
    "id",
    "person_id",
    "project_id",
    "participation",
    "is_lawyer_dri",
    "is_client_dri",
    "inserted_at",
    "updated_at",
];

/// An upsert keyed on `id` that writes only when a column differs.
pub(super) fn upsert_sql(table: &str, columns: &[&str]) -> String {
    let placeholders = (1..=columns.len())
        .map(|index| format!("${index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let rest = &columns[1..];
    let set = rest
        .iter()
        .map(|column| format!("{column} = EXCLUDED.{column}"))
        .collect::<Vec<_>>()
        .join(", ");
    let current = rest
        .iter()
        .map(|column| format!("{table}.{column}"))
        .collect::<Vec<_>>()
        .join(", ");
    let proposed = rest
        .iter()
        .map(|column| format!("EXCLUDED.{column}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "INSERT INTO {table} ({}) VALUES ({placeholders}) ON CONFLICT (id) DO UPDATE SET {set} \
         WHERE ({current}) IS DISTINCT FROM ({proposed})",
        columns.join(", ")
    )
}

pub(super) fn select_sql(table: &str, columns: &[&str]) -> String {
    format!("SELECT {} FROM {table}", columns.join(", "))
}

macro_rules! statement {
    ($name:ident, $build:expr) => {
        static $name: LazyLock<String> = LazyLock::new(|| $build);
    };
}

statement!(FIRM_UPSERT, upsert_sql("firm", FIRM_COLUMNS));
statement!(BRAND_UPSERT, upsert_sql("brand", BRAND_COLUMNS));
statement!(
    FIRM_BRAND_UPSERT,
    upsert_sql("firm_brand", FIRM_BRAND_COLUMNS)
);
statement!(PROJECT_UPSERT, upsert_sql("project", PROJECT_COLUMNS));
statement!(
    MEMBERSHIP_UPSERT,
    upsert_sql("person_firm_role", MEMBERSHIP_COLUMNS)
);
statement!(
    PARTICIPATION_UPSERT,
    upsert_sql("person_project_role", PARTICIPATION_COLUMNS)
);
statement!(FIRM_SELECT, select_sql("firm", FIRM_COLUMNS));
statement!(BRAND_SELECT, select_sql("brand", BRAND_COLUMNS));
statement!(
    FIRM_BRAND_SELECT,
    select_sql("firm_brand", FIRM_BRAND_COLUMNS)
);
statement!(PROJECT_SELECT, select_sql("project", PROJECT_COLUMNS));
statement!(
    MEMBERSHIP_SELECT,
    select_sql("person_firm_role", MEMBERSHIP_COLUMNS)
);
statement!(
    PARTICIPATION_SELECT,
    select_sql("person_project_role", PARTICIPATION_COLUMNS)
);

statement!(
    FIRM_BY_ID,
    format!("{} WHERE id = $1", select_sql("firm", FIRM_COLUMNS))
);
statement!(
    PROJECT_BY_ID,
    format!("{} WHERE id = $1", select_sql("project", PROJECT_COLUMNS))
);
statement!(
    PROJECT_BY_CODE,
    format!("{} WHERE code = $1", select_sql("project", PROJECT_COLUMNS))
);

/// Compare one Firm read by id with PostgreSQL's row, through the mirror's
/// Firm image.
pub(crate) async fn shadow_firm(
    pg: &PostgresDb,
    id: Uuid,
    source: Option<&crate::firms::Firm>,
) -> Result<Comparison, MirrorError> {
    let source = source.map(FirmImage::from_source).transpose()?;
    compare_row(
        pg,
        FIRM_BY_ID.as_str(),
        ShadowKey::Id(id),
        source,
        FirmImage::from_row,
        |image| image.id,
    )
    .await
}

/// Compare one Project read, by id or by code, with PostgreSQL's row for
/// the same lookup, through the mirror's Project image.
pub(crate) async fn shadow_project(
    pg: &PostgresDb,
    key: ShadowKey,
    source: Option<&crate::projects::Project>,
) -> Result<Comparison, MirrorError> {
    let source = source.map(ProjectImage::from_source).transpose()?;
    let sql = match key {
        ShadowKey::Id(_) => PROJECT_BY_ID.as_str(),
        ShadowKey::Text(_) => PROJECT_BY_CODE.as_str(),
    };
    compare_row(pg, sql, key, source, ProjectImage::from_row, |image| {
        image.id
    })
    .await
}

/// A Surreal RFC 3339 timestamp at PostgreSQL's microsecond precision.
pub(super) fn timestamp(text: &str) -> Result<DateTime<Utc>, MirrorError> {
    DateTime::parse_from_rfc3339(text)
        .map(|at| at.with_timezone(&Utc).trunc_subsecs(6))
        .map_err(|_| MirrorError::SourceTimestamp)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FirmImage {
    id: Uuid,
    name: String,
    slug: String,
    status: String,
    entity_id: Option<Uuid>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl FirmImage {
    fn from_source(firm: &crate::firms::Firm) -> Result<Self, MirrorError> {
        Ok(Self {
            id: firm.id,
            name: firm.name.clone(),
            slug: firm.slug.clone(),
            status: firm.status.clone(),
            entity_id: firm.entity_id,
            inserted_at: timestamp(&firm.inserted_at)?,
            updated_at: timestamp(&firm.updated_at)?,
        })
    }

    fn from_row(row: &sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            slug: row.try_get("slug")?,
            status: row.try_get("status")?,
            entity_id: row.try_get("entity_id")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BrandImage {
    id: Uuid,
    name: String,
    brand_key: String,
    firm_id: Option<Uuid>,
    primary_color: Option<String>,
    accent_color: Option<String>,
    typeface: Option<String>,
    is_law_firm: bool,
    legal_entity: Option<String>,
    logo_object_key: Option<String>,
    logo_content_type: Option<String>,
    font_family: Option<String>,
    font_object_key: Option<String>,
    font_licence: Option<String>,
    website_url: Option<String>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl BrandImage {
    fn from_source(brand: &crate::brands::Brand) -> Result<Self, MirrorError> {
        Ok(Self {
            id: brand.id,
            name: brand.name.clone(),
            brand_key: brand.key.clone(),
            firm_id: brand.firm_id,
            primary_color: brand.primary_color.clone(),
            accent_color: brand.accent_color.clone(),
            typeface: brand.typeface.clone(),
            is_law_firm: brand.is_law_firm,
            legal_entity: brand.legal_entity.clone(),
            logo_object_key: brand.logo_object_key.clone(),
            logo_content_type: brand.logo_content_type.clone(),
            font_family: brand.font_family.clone(),
            font_object_key: brand.font_object_key.clone(),
            font_licence: brand.font_licence.clone(),
            website_url: brand.website_url.clone(),
            inserted_at: timestamp(&brand.inserted_at)?,
            updated_at: timestamp(&brand.updated_at)?,
        })
    }

    fn from_row(row: &sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            brand_key: row.try_get("brand_key")?,
            firm_id: row.try_get("firm_id")?,
            primary_color: row.try_get("primary_color")?,
            accent_color: row.try_get("accent_color")?,
            typeface: row.try_get("typeface")?,
            is_law_firm: row.try_get("is_law_firm")?,
            legal_entity: row.try_get("legal_entity")?,
            logo_object_key: row.try_get("logo_object_key")?,
            logo_content_type: row.try_get("logo_content_type")?,
            font_family: row.try_get("font_family")?,
            font_object_key: row.try_get("font_object_key")?,
            font_licence: row.try_get("font_licence")?,
            website_url: row.try_get("website_url")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FirmBrandImage {
    id: Uuid,
    firm_id: Uuid,
    brand_key: String,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl FirmBrandImage {
    fn from_source(row: &crate::firms::FirmBrand) -> Result<Self, MirrorError> {
        Ok(Self {
            id: row.id,
            firm_id: row.firm_id,
            brand_key: row.brand_key.clone(),
            inserted_at: timestamp(&row.inserted_at)?,
            updated_at: timestamp(&row.updated_at)?,
        })
    }

    fn from_row(row: &sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            firm_id: row.try_get("firm_id")?,
            brand_key: row.try_get("brand_key")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectImage {
    id: Uuid,
    code: String,
    name: String,
    status: String,
    brand: String,
    entity_id: Uuid,
    firm_id: Option<Uuid>,
    jurisdiction_id: Option<Uuid>,
    description: Option<String>,
    drive_folder_id: Option<String>,
    repository_url: Option<String>,
    git_initialized_at: Option<String>,
    forge_provisioned_at: Option<String>,
    closed_at: Option<String>,
    closure_reason: Option<String>,
    internal_slack_channel_url: Option<String>,
    external_slack_channel_url: Option<String>,
    internal_slack_channel_id: Option<String>,
    private_notion_page_url: Option<String>,
    shared_notion_page_url: Option<String>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl ProjectImage {
    fn from_source(project: &crate::projects::Project) -> Result<Self, MirrorError> {
        Ok(Self {
            id: project.id,
            code: project.code.clone(),
            name: project.name.clone(),
            status: project.status.clone(),
            brand: project.brand.clone(),
            entity_id: project.entity_id,
            firm_id: project.firm_id,
            jurisdiction_id: project.jurisdiction_id,
            description: project.description.clone(),
            drive_folder_id: project.drive_folder_id.clone(),
            repository_url: project.repository_url.clone(),
            git_initialized_at: project.git_initialized_at.clone(),
            forge_provisioned_at: project.forge_provisioned_at.clone(),
            closed_at: project.closed_at.clone(),
            closure_reason: project.closure_reason.clone(),
            internal_slack_channel_url: project.internal_slack_channel_url.clone(),
            external_slack_channel_url: project.external_slack_channel_url.clone(),
            internal_slack_channel_id: project.internal_slack_channel_id.clone(),
            private_notion_page_url: project.private_notion_page_url.clone(),
            shared_notion_page_url: project.shared_notion_page_url.clone(),
            inserted_at: timestamp(&project.inserted_at)?,
            updated_at: timestamp(&project.updated_at)?,
        })
    }

    fn from_row(row: &sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        // PostgreSQL keeps `brand` nullable; SurrealDB requires it, so an
        // absent value compares as an empty key and reads divergent.
        let brand: Option<String> = row.try_get("brand")?;
        Ok(Self {
            id: row.try_get("id")?,
            code: row.try_get("code")?,
            name: row.try_get("name")?,
            status: row.try_get("status")?,
            brand: brand.unwrap_or_default(),
            entity_id: row.try_get("entity_id")?,
            firm_id: row.try_get("firm_id")?,
            jurisdiction_id: row.try_get("jurisdiction_id")?,
            description: row.try_get("description")?,
            drive_folder_id: row.try_get("drive_folder_id")?,
            repository_url: row.try_get("repository_url")?,
            git_initialized_at: row.try_get("git_initialized_at")?,
            forge_provisioned_at: row.try_get("forge_provisioned_at")?,
            closed_at: row.try_get("closed_at")?,
            closure_reason: row.try_get("closure_reason")?,
            internal_slack_channel_url: row.try_get("internal_slack_channel_url")?,
            external_slack_channel_url: row.try_get("external_slack_channel_url")?,
            internal_slack_channel_id: row.try_get("internal_slack_channel_id")?,
            private_notion_page_url: row.try_get("private_notion_page_url")?,
            shared_notion_page_url: row.try_get("shared_notion_page_url")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MembershipImage {
    id: Uuid,
    person_id: Uuid,
    firm_id: Uuid,
    membership: String,
    is_dri: bool,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl MembershipImage {
    fn from_source(role: &crate::firms::PersonFirmRole) -> Result<Self, MirrorError> {
        Ok(Self {
            id: role.id,
            person_id: role.person_id,
            firm_id: role.firm_id,
            membership: role.membership.as_str().to_string(),
            is_dri: role.is_dri,
            inserted_at: timestamp(&role.inserted_at)?,
            updated_at: timestamp(&role.updated_at)?,
        })
    }

    fn from_row(row: &sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            person_id: row.try_get("person_id")?,
            firm_id: row.try_get("firm_id")?,
            membership: row.try_get("membership")?,
            is_dri: row.try_get("is_dri")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }

    fn key(&self) -> MirrorKey {
        MirrorKey::FirmMembership {
            person_id: self.person_id,
            firm_id: self.firm_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParticipationImage {
    id: Uuid,
    person_id: Uuid,
    project_id: Uuid,
    participation: String,
    is_lawyer_dri: bool,
    is_client_dri: bool,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl ParticipationImage {
    fn from_source(role: &crate::projects::PersonProjectRole) -> Result<Self, MirrorError> {
        Ok(Self {
            id: role.id,
            person_id: role.person_id,
            project_id: role.project_id,
            participation: role.participation.clone(),
            is_lawyer_dri: role.is_lawyer_dri,
            is_client_dri: role.is_client_dri,
            inserted_at: timestamp(&role.inserted_at)?,
            updated_at: timestamp(&role.updated_at)?,
        })
    }

    fn from_row(row: &sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            person_id: row.try_get("person_id")?,
            project_id: row.try_get("project_id")?,
            participation: row.try_get("participation")?,
            is_lawyer_dri: row.try_get("is_lawyer_dri")?,
            is_client_dri: row.try_get("is_client_dri")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

/// Make PostgreSQL's copy of one tenancy row match SurrealDB's.
pub(super) async fn mirror(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    key: &MirrorKey,
) -> Result<Mirrored, MirrorError> {
    match key {
        MirrorKey::Firm(id) => {
            let source = crate::firms::find_by_id(surreal, *id)
                .await
                .map_err(|_| MirrorError::Source)?;
            match source {
                Some(firm) => {
                    let image = FirmImage::from_source(&firm)?;
                    if let Some(entity) = image.entity_id {
                        identity::ensure(surreal, pg, MirrorKey::Entity(entity)).await?;
                    }
                    upsert_firm(pg, &image).await
                }
                None => delete(pg, "DELETE FROM firm WHERE id = $1", *id).await,
            }
        }
        MirrorKey::Brand(id) => {
            let source = crate::brands::find_by_id(surreal, *id)
                .await
                .map_err(|_| MirrorError::Source)?;
            match source {
                Some(brand) => upsert_brand(pg, &BrandImage::from_source(&brand)?).await,
                None => delete(pg, "DELETE FROM brand WHERE id = $1", *id).await,
            }
        }
        MirrorKey::FirmBrand { brand_key } => {
            let source = crate::firms::firm_brand_for_key(surreal, brand_key)
                .await
                .map_err(|_| MirrorError::Source)?;
            if let Some(row) = source {
                return upsert_firm_brand(pg, &FirmBrandImage::from_source(&row)?).await;
            }
            let deleted = sqlx::query("DELETE FROM firm_brand WHERE brand_key = $1")
                .bind(brand_key)
                .execute(&pg.0)
                .await?;
            Ok(changed(deleted.rows_affected(), Mirrored::Deleted))
        }
        MirrorKey::Project(id) => {
            let source = crate::projects::find_by_id(surreal, *id)
                .await
                .map_err(|_| MirrorError::Source)?;
            match source {
                Some(project) => {
                    let image = ProjectImage::from_source(&project)?;
                    identity::ensure(surreal, pg, MirrorKey::Entity(image.entity_id)).await?;
                    upsert_project(pg, &image).await
                }
                None => delete_project(pg, *id).await,
            }
        }
        MirrorKey::FirmMembership { person_id, firm_id } => {
            let source = crate::firms::membership_for_person(surreal, *person_id, *firm_id)
                .await
                .map_err(|_| MirrorError::Source)?;
            if let Some(role) = source {
                let image = MembershipImage::from_source(&role)?;
                identity::ensure(surreal, pg, MirrorKey::Person(image.person_id)).await?;
                return upsert_membership(pg, &image).await;
            }
            let deleted =
                sqlx::query("DELETE FROM person_firm_role WHERE person_id = $1 AND firm_id = $2")
                    .bind(person_id)
                    .bind(firm_id)
                    .execute(&pg.0)
                    .await?;
            Ok(changed(deleted.rows_affected(), Mirrored::Deleted))
        }
        MirrorKey::ProjectParticipation(id) => {
            let source = crate::projects::participation_by_id(surreal, *id)
                .await
                .map_err(|_| MirrorError::Source)?;
            match source {
                Some(role) => {
                    let image = ParticipationImage::from_source(&role)?;
                    identity::ensure(surreal, pg, MirrorKey::Person(image.person_id)).await?;
                    upsert_participation(pg, &image).await
                }
                None => delete(pg, "DELETE FROM person_project_role WHERE id = $1", *id).await,
            }
        }
        // `super::mirror` routes only tenancy keys here.
        _ => Err(MirrorError::Source),
    }
}

async fn delete(pg: &PostgresDb, sql: &'static str, id: Uuid) -> Result<Mirrored, MirrorError> {
    let deleted = sqlx::query(sql).bind(id).execute(&pg.0).await?;
    Ok(changed(deleted.rows_affected(), Mirrored::Deleted))
}

async fn upsert_firm(pg: &PostgresDb, firm: &FirmImage) -> Result<Mirrored, MirrorError> {
    let mut tx = pg.0.begin().await?;
    let stale = sqlx::query("DELETE FROM firm WHERE slug = $1 AND id <> $2")
        .bind(&firm.slug)
        .bind(firm.id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    let written = sqlx::query(FIRM_UPSERT.as_str())
        .bind(firm.id)
        .bind(&firm.name)
        .bind(&firm.slug)
        .bind(&firm.status)
        .bind(firm.entity_id)
        .bind(firm.inserted_at)
        .bind(firm.updated_at)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(changed(stale + written.rows_affected(), Mirrored::Written))
}

async fn upsert_brand(pg: &PostgresDb, brand: &BrandImage) -> Result<Mirrored, MirrorError> {
    let mut tx = pg.0.begin().await?;
    let stale = sqlx::query("DELETE FROM brand WHERE brand_key = $1 AND id <> $2")
        .bind(&brand.brand_key)
        .bind(brand.id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    let written = sqlx::query(BRAND_UPSERT.as_str())
        .bind(brand.id)
        .bind(&brand.name)
        .bind(&brand.brand_key)
        .bind(brand.firm_id)
        .bind(&brand.primary_color)
        .bind(&brand.accent_color)
        .bind(&brand.typeface)
        .bind(brand.is_law_firm)
        .bind(&brand.legal_entity)
        .bind(&brand.logo_object_key)
        .bind(&brand.logo_content_type)
        .bind(&brand.font_family)
        .bind(&brand.font_object_key)
        .bind(&brand.font_licence)
        .bind(&brand.website_url)
        .bind(brand.inserted_at)
        .bind(brand.updated_at)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    Ok(changed(stale + written, Mirrored::Written))
}

async fn upsert_firm_brand(pg: &PostgresDb, row: &FirmBrandImage) -> Result<Mirrored, MirrorError> {
    let mut tx = pg.0.begin().await?;
    let stale = sqlx::query("DELETE FROM firm_brand WHERE brand_key = $1 AND id <> $2")
        .bind(&row.brand_key)
        .bind(row.id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    let written = sqlx::query(FIRM_BRAND_UPSERT.as_str())
        .bind(row.id)
        .bind(row.firm_id)
        .bind(&row.brand_key)
        .bind(row.inserted_at)
        .bind(row.updated_at)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    Ok(changed(stale + written, Mirrored::Written))
}

async fn upsert_project(pg: &PostgresDb, project: &ProjectImage) -> Result<Mirrored, MirrorError> {
    let mut tx = pg.0.begin().await?;
    let stale = sqlx::query(
        "DELETE FROM person_project_role WHERE project_id IN \
         (SELECT id FROM project WHERE code = $1 AND id <> $2)",
    )
    .bind(&project.code)
    .bind(project.id)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        + sqlx::query("DELETE FROM project WHERE code = $1 AND id <> $2")
            .bind(&project.code)
            .bind(project.id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
    let written = sqlx::query(PROJECT_UPSERT.as_str())
        .bind(project.id)
        .bind(&project.code)
        .bind(&project.name)
        .bind(&project.status)
        .bind(&project.brand)
        .bind(project.entity_id)
        .bind(project.firm_id)
        .bind(project.jurisdiction_id)
        .bind(&project.description)
        .bind(&project.drive_folder_id)
        .bind(&project.repository_url)
        .bind(&project.git_initialized_at)
        .bind(&project.forge_provisioned_at)
        .bind(&project.closed_at)
        .bind(&project.closure_reason)
        .bind(&project.internal_slack_channel_url)
        .bind(&project.external_slack_channel_url)
        .bind(&project.internal_slack_channel_id)
        .bind(&project.private_notion_page_url)
        .bind(&project.shared_notion_page_url)
        .bind(project.inserted_at)
        .bind(project.updated_at)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    Ok(changed(stale + written, Mirrored::Written))
}

async fn delete_project(pg: &PostgresDb, id: Uuid) -> Result<Mirrored, MirrorError> {
    let mut tx = pg.0.begin().await?;
    let participations = sqlx::query("DELETE FROM person_project_role WHERE project_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    let projects = sqlx::query("DELETE FROM project WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    Ok(changed(participations + projects, Mirrored::Deleted))
}

async fn upsert_membership(
    pg: &PostgresDb,
    role: &MembershipImage,
) -> Result<Mirrored, MirrorError> {
    let mut tx = pg.0.begin().await?;
    let stale = sqlx::query(
        "DELETE FROM person_firm_role WHERE person_id = $1 AND firm_id = $2 AND id <> $3",
    )
    .bind(role.person_id)
    .bind(role.firm_id)
    .bind(role.id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let written = sqlx::query(MEMBERSHIP_UPSERT.as_str())
        .bind(role.id)
        .bind(role.person_id)
        .bind(role.firm_id)
        .bind(&role.membership)
        .bind(role.is_dri)
        .bind(role.inserted_at)
        .bind(role.updated_at)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    Ok(changed(stale + written, Mirrored::Written))
}

async fn upsert_participation(
    pg: &PostgresDb,
    role: &ParticipationImage,
) -> Result<Mirrored, MirrorError> {
    let mut tx = pg.0.begin().await?;
    let stale = sqlx::query(
        "DELETE FROM person_project_role WHERE person_id = $1 AND project_id = $2 AND id <> $3",
    )
    .bind(role.person_id)
    .bind(role.project_id)
    .bind(role.id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let written = sqlx::query(PARTICIPATION_UPSERT.as_str())
        .bind(role.id)
        .bind(role.person_id)
        .bind(role.project_id)
        .bind(&role.participation)
        .bind(role.is_lawyer_dri)
        .bind(role.is_client_dri)
        .bind(role.inserted_at)
        .bind(role.updated_at)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    Ok(changed(stale + written, Mirrored::Written))
}

/// One table's comparison. A source row whose timestamp cannot be read is
/// divergent: it can never be mirrored as it stands.
pub(super) fn compare<T: PartialEq>(
    table: &'static str,
    source: &BTreeMap<MirrorKey, Option<T>>,
    target: &BTreeMap<MirrorKey, T>,
) -> TableParity {
    let mut divergent: Vec<MirrorKey> = source
        .iter()
        .filter(|(key, image)| match image {
            Some(image) => target.get(*key) != Some(image),
            None => true,
        })
        .map(|(key, _)| key.clone())
        .collect();
    divergent.extend(
        target
            .keys()
            .filter(|key| !source.contains_key(*key))
            .cloned(),
    );
    divergent.sort();
    TableParity {
        table,
        source_rows: source.len(),
        target_rows: target.len(),
        divergent,
    }
}

/// One table's source rows keyed for comparison. A row whose image cannot
/// be built keeps its key with no image.
pub(super) fn images<S, I>(
    rows: &[S],
    key: impl Fn(&S) -> MirrorKey,
    image: impl Fn(&S) -> Result<I, MirrorError>,
) -> BTreeMap<MirrorKey, Option<I>> {
    rows.iter().map(|row| (key(row), image(row).ok())).collect()
}

pub(super) async fn target<T>(
    pg: &PostgresDb,
    sql: &'static str,
    read: impl Fn(&sqlx::postgres::PgRow) -> Result<T, sqlx::Error>,
    key: impl Fn(&T) -> MirrorKey,
) -> Result<BTreeMap<MirrorKey, T>, MirrorError> {
    let rows = sqlx::query(sql).fetch_all(&pg.0).await?;
    rows.iter()
        .map(|row| read(row).map(|image| (key(&image), image)))
        .collect::<Result<_, sqlx::Error>>()
        .map_err(Into::into)
}

pub(super) fn source<E>(_: E) -> MirrorError {
    MirrorError::Source
}

fn firm_key(firm: &crate::firms::Firm) -> MirrorKey {
    MirrorKey::Firm(firm.id)
}

fn brand_key(brand: &crate::brands::Brand) -> MirrorKey {
    MirrorKey::Brand(brand.id)
}

fn firm_brand_key(brand_key: &str) -> MirrorKey {
    MirrorKey::FirmBrand {
        brand_key: brand_key.to_string(),
    }
}

/// Compare every Firm, Brand, `firm_brand`, Project, and membership row in
/// both stores, parents before children.
///
/// # Errors
///
/// Either store could not be read.
#[expect(
    clippy::too_many_lines,
    reason = "six tables compared in one parent-first list"
)]
pub async fn parity(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let firms = crate::firms::all(surreal).await.map_err(source)?;
    let mut brands = crate::brands::system_wide(surreal).await.map_err(source)?;
    brands.extend(
        crate::brands::all_firm_scoped(surreal)
            .await
            .map_err(source)?,
    );
    let firm_brands = crate::firms::all_firm_brands(surreal)
        .await
        .map_err(source)?;
    let projects = crate::projects::all(surreal).await.map_err(source)?;
    let memberships = crate::firms::all_memberships(surreal)
        .await
        .map_err(source)?;
    let participations = crate::projects::all_participations(surreal)
        .await
        .map_err(source)?;

    let tables = vec![
        compare(
            "firm",
            &images(&firms, firm_key, FirmImage::from_source),
            &target(pg, FIRM_SELECT.as_str(), FirmImage::from_row, |firm| {
                MirrorKey::Firm(firm.id)
            })
            .await?,
        ),
        compare(
            "brand",
            &images(&brands, brand_key, BrandImage::from_source),
            &target(pg, BRAND_SELECT.as_str(), BrandImage::from_row, |brand| {
                MirrorKey::Brand(brand.id)
            })
            .await?,
        ),
        compare(
            "firm_brand",
            &images(
                &firm_brands,
                |row| firm_brand_key(&row.brand_key),
                FirmBrandImage::from_source,
            ),
            &target(
                pg,
                FIRM_BRAND_SELECT.as_str(),
                FirmBrandImage::from_row,
                |row| firm_brand_key(&row.brand_key),
            )
            .await?,
        ),
        compare(
            "project",
            &images(
                &projects,
                |project| MirrorKey::Project(project.id),
                ProjectImage::from_source,
            ),
            &target(
                pg,
                PROJECT_SELECT.as_str(),
                ProjectImage::from_row,
                |project| MirrorKey::Project(project.id),
            )
            .await?,
        ),
        compare(
            "person_firm_role",
            &images(
                &memberships,
                |role| MirrorKey::FirmMembership {
                    person_id: role.person_id,
                    firm_id: role.firm_id,
                },
                MembershipImage::from_source,
            ),
            &target(
                pg,
                MEMBERSHIP_SELECT.as_str(),
                MembershipImage::from_row,
                MembershipImage::key,
            )
            .await?,
        ),
        compare(
            "person_project_role",
            &images(
                &participations,
                |role| MirrorKey::ProjectParticipation(role.id),
                ParticipationImage::from_source,
            ),
            &target(
                pg,
                PARTICIPATION_SELECT.as_str(),
                ParticipationImage::from_row,
                |role| MirrorKey::ProjectParticipation(role.id),
            )
            .await?,
        ),
    ];
    Ok(ParityReport { tables })
}

/// Re-mirror every divergent tenancy row from SurrealDB, then compare again.
///
/// See [`remirror`] for the order. A row that fails both passes stays in
/// the returned report.
///
/// # Errors
///
/// Either store could not be read for the comparison.
pub async fn reconcile(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let before = parity(surreal, pg).await?;
    remirror(surreal, pg, &before).await;
    parity(surreal, pg).await
}

/// Re-mirror every divergent key in `report`. Upserts run parents first. A
/// row that still fails is retried once in reverse order, so a stale
/// PostgreSQL parent is deleted after its stale children.
pub(super) async fn remirror(surreal: &SurrealDb, pg: &PostgresDb, report: &ParityReport) {
    let mut failed = Vec::new();
    for key in report.divergent() {
        if super::mirror(surreal, pg, key).await.is_err() {
            failed.push(key.clone());
        }
    }
    for key in failed.iter().rev() {
        if let Err(error) = super::mirror(surreal, pg, key).await {
            tracing::warn!(key = %key, error = %error, "PostgreSQL reconcile could not mirror the row");
        }
    }
}

#[cfg(test)]
#[path = "tenancy_tests.rs"]
mod tests;

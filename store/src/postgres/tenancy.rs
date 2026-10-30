//! PostgreSQL authority for Firms, Brands, Firm Brands and Firm memberships.
//!
//! These are canonical tenancy rows, not mirror payloads. The authority
//! switch is opt-in during the migration; once enabled, configuration and
//! database errors are returned to callers and never fall back to SurrealDB.

use crate::{brands, firms};
use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

const AUTHORITY_ENV: &str = "NAVIGATOR_POSTGRES_TENANCY_AUTHORITY";
// Deliberately false in the preparatory tenancy PR. The follow-on authority
// slice flips this only when Project and access consumers have also moved.
const CUTOVER_READY: bool = false;

#[cfg(test)]
tokio::task_local! {
    static TEST_TENANCY_DB: crate::postgres::PostgresDb;
}

#[derive(Debug, thiserror::Error)]
pub enum TenancyError {
    #[error("PostgreSQL tenancy authority configuration is invalid")]
    Configuration,
    #[error("PostgreSQL tenancy authority requires the complete Project and access cutover")]
    CutoverIncomplete,
    #[error("PostgreSQL is unavailable for tenancy authority")]
    Postgres(#[from] crate::postgres::PostgresError),
    #[error("PostgreSQL tenancy operation failed")]
    Query(#[from] sqlx::Error),
    #[error("PostgreSQL returned an invalid tenancy row")]
    InvalidRow,
}

/// Resolve the migration switch strictly: unset/off means legacy mode, on is
/// accepted only after the complete cutover, and unknown values fail closed.
pub(crate) fn enabled() -> Result<bool, TenancyError> {
    let value = match std::env::var(AUTHORITY_ENV) {
        Err(std::env::VarError::NotPresent) => return Ok(false),
        Err(std::env::VarError::NotUnicode(_)) => return Err(TenancyError::Configuration),
        Ok(value) => value,
    };
    enabled_value(Some(&value), CUTOVER_READY)
}

fn enabled_value(value: Option<&str>, cutover_ready: bool) -> Result<bool, TenancyError> {
    let configured = match value {
        None | Some("0" | "false") => false,
        Some("1" | "true") => true,
        Some(_) => return Err(TenancyError::Configuration),
    };
    if configured && !cutover_ready {
        Err(TenancyError::CutoverIncomplete)
    } else {
        Ok(configured)
    }
}

async fn db() -> Result<crate::postgres::PostgresDb, TenancyError> {
    #[cfg(test)]
    if let Ok(pg) = TEST_TENANCY_DB.try_with(Clone::clone) {
        return Ok(pg);
    }
    Ok(crate::postgres::PostgresDb::from_env().await?)
}

#[cfg(test)]
pub(crate) async fn with_test_db<T>(
    pg: crate::postgres::PostgresDb,
    future: impl std::future::Future<Output = T>,
) -> T {
    TEST_TENANCY_DB.scope(pg, future).await
}

fn time(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

fn firm(row: &sqlx::postgres::PgRow) -> Result<firms::Firm, TenancyError> {
    Ok(firms::Firm {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        slug: row.try_get("slug")?,
        status: row.try_get("status")?,
        entity_id: row.try_get("entity_id")?,
        inserted_at: time(row.try_get("inserted_at")?),
        updated_at: time(row.try_get("updated_at")?),
    })
}

fn brand(row: &sqlx::postgres::PgRow) -> Result<brands::Brand, TenancyError> {
    Ok(brands::Brand {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        key: row.try_get("brand_key")?,
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
        inserted_at: time(row.try_get("inserted_at")?),
        updated_at: time(row.try_get("updated_at")?),
    })
}

/// Read a Firm by id from the authoritative store.
pub async fn find_firm(id: Uuid) -> Result<Option<firms::Firm>, TenancyError> {
    let pg = db().await?;
    find_firm_on(&pg, id).await
}

pub(crate) async fn find_firm_on(
    pg: &crate::postgres::PostgresDb,
    id: Uuid,
) -> Result<Option<firms::Firm>, TenancyError> {
    let row = sqlx::query(
        "SELECT id,name,slug,status,entity_id,inserted_at,updated_at FROM firm WHERE id=$1",
    )
    .bind(id)
    .fetch_optional(pg.pool())
    .await?;
    row.as_ref().map(firm).transpose()
}

pub async fn create_firm(input: &firms::NewFirm) -> Result<firms::Firm, firms::FirmError> {
    let pg = db().await?;
    create_firm_on(&pg, input).await
}

pub(crate) async fn create_firm_on(
    pg: &crate::postgres::PostgresDb,
    input: &firms::NewFirm,
) -> Result<firms::Firm, firms::FirmError> {
    let entity_exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM entity WHERE id=$1)")
        .bind(input.entity_id)
        .fetch_one(pg.pool())
        .await?;
    if !entity_exists {
        return Err(firms::FirmError::NoSuchEntity(input.entity_id));
    }
    let role: Option<String> = sqlx::query_scalar("SELECT role FROM person WHERE id=$1")
        .bind(input.admin_dri_person_id)
        .fetch_optional(pg.pool())
        .await?;
    let Some(role) = role else {
        return Err(firms::FirmError::NoSuchPerson(input.admin_dri_person_id));
    };
    if !matches!(role.as_str(), "admin" | "owner") {
        return Err(firms::FirmError::IneligibleAdminDriTier(
            input.admin_dri_person_id,
        ));
    }
    let slug = firms::slug::derive(&input.name);
    if !firms::slug::is_valid(&slug) {
        return Err(firms::FirmError::InvalidSlug(slug));
    }
    let id = Uuid::new_v4();
    let membership_id = Uuid::new_v4();
    let mut tx = pg.pool().begin().await?;
    let row = sqlx::query("INSERT INTO firm (id,name,slug,status,entity_id) VALUES ($1,$2,$3,$4,$5) RETURNING id,name,slug,status,entity_id,inserted_at,updated_at")
        .bind(id).bind(&input.name).bind(&slug).bind(&input.status).bind(input.entity_id)
        .fetch_one(&mut *tx).await.map_err(|error| {
            match error.as_database_error().and_then(sqlx::error::DatabaseError::constraint) {
                Some("firm_slug") => firms::FirmError::DuplicateSlug(slug.clone()),
                Some("firm_entity") => firms::FirmError::DuplicateEntity,
                _ => firms::FirmError::Postgres(TenancyError::Query(error)),
            }
        })?;
    sqlx::query("INSERT INTO person_firm_role (id,person_id,firm_id,membership,is_dri) VALUES ($1,$2,$3,'admin',true)")
        .bind(membership_id).bind(input.admin_dri_person_id).bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    firm(&row).map_err(firms::FirmError::Postgres)
}

pub async fn update_firm(
    actor_role: crate::persons::Role,
    actor_person_id: Option<Uuid>,
    firm_id: Uuid,
    input: &firms::FirmEdit,
) -> Result<firms::Firm, firms::FirmError> {
    let pg = db().await?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM firm WHERE id=$1)")
        .bind(firm_id)
        .fetch_one(pg.pool())
        .await?;
    if !exists {
        return Err(firms::FirmError::NoSuchFirm(firm_id));
    }
    authorize_firm_management(pg.pool(), actor_role, actor_person_id, firm_id).await?;
    if let Some(entity_id) = input.entity_id {
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM entity WHERE id=$1)")
            .bind(entity_id)
            .fetch_one(pg.pool())
            .await?;
        if !exists {
            return Err(firms::FirmError::NoSuchEntity(entity_id));
        }
    }
    let row = sqlx::query("UPDATE firm SET name=CASE WHEN $2 THEN $3 ELSE name END, status=CASE WHEN $4 THEN $5 ELSE status END, entity_id=CASE WHEN $6 THEN $7 ELSE entity_id END, updated_at=now() WHERE id=$1 RETURNING id,name,slug,status,entity_id,inserted_at,updated_at")
        .bind(firm_id)
        .bind(input.name.is_some()).bind(input.name.as_deref())
        .bind(input.status.is_some()).bind(input.status.as_deref())
        .bind(input.entity_id.is_some()).bind(input.entity_id)
        .fetch_one(pg.pool()).await.map_err(|error| {
            if error.as_database_error().and_then(sqlx::error::DatabaseError::constraint).is_some_and(|name| name == "firm_entity") {
                firms::FirmError::DuplicateEntity
            } else { firms::FirmError::Postgres(TenancyError::Query(error)) }
        })?;
    firm(&row).map_err(firms::FirmError::Postgres)
}

pub async fn delete_firm(
    actor_role: crate::persons::Role,
    actor_person_id: Option<Uuid>,
    firm_id: Uuid,
) -> Result<(), firms::FirmError> {
    let pg = db().await?;
    let mut tx = pg.pool().begin().await?;
    let firm_row = sqlx::query("SELECT id FROM firm WHERE id=$1 FOR UPDATE")
        .bind(firm_id)
        .fetch_optional(&mut *tx)
        .await?;
    if firm_row.is_none() {
        return Err(firms::FirmError::NoSuchFirm(firm_id));
    }
    authorize_firm_management(pg.pool(), actor_role, actor_person_id, firm_id).await?;
    let owns_projects: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM project WHERE firm_id=$1)")
            .bind(firm_id)
            .fetch_one(&mut *tx)
            .await?;
    if owns_projects {
        return Err(firms::FirmError::FirmOwnsProjects(firm_id));
    }
    let owns_brands: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM brand WHERE firm_id=$1)")
            .bind(firm_id)
            .fetch_one(&mut *tx)
            .await?;
    if owns_brands {
        return Err(firms::FirmError::FirmOwnsBrands(firm_id));
    }
    sqlx::query("DELETE FROM person_firm_role WHERE firm_id=$1")
        .bind(firm_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM firm_brand WHERE firm_id=$1")
        .bind(firm_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM firm WHERE id=$1")
        .bind(firm_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub(crate) async fn authorize_firm_management(
    pool: &sqlx::PgPool,
    actor_role: crate::persons::Role,
    actor_person_id: Option<Uuid>,
    firm_id: Uuid,
) -> Result<(), firms::FirmError> {
    if actor_role == crate::persons::Role::Owner {
        return Ok(());
    }
    if actor_role != crate::persons::Role::Admin {
        return Err(firms::FirmError::NotAuthorized);
    }
    let Some(person_id) = actor_person_id else {
        return Err(firms::FirmError::NotAuthorized);
    };
    let membership: Option<String> = sqlx::query_scalar(
        "SELECT membership FROM person_firm_role WHERE person_id=$1 AND firm_id=$2",
    )
    .bind(person_id)
    .bind(firm_id)
    .fetch_optional(pool)
    .await?;
    match membership.as_deref() {
        Some("admin") => Ok(()),
        _ => Err(firms::FirmError::NotAuthorized),
    }
}

/// Read all Firms in stable id order.
pub async fn all_firms() -> Result<Vec<firms::Firm>, TenancyError> {
    let pg = db().await?;
    sqlx::query(
        "SELECT id,name,slug,status,entity_id,inserted_at,updated_at FROM firm ORDER BY name,id",
    )
    .fetch_all(pg.pool())
    .await?
    .iter()
    .map(firm)
    .collect()
}

pub async fn firm_by_entity(entity_id: Uuid) -> Result<Option<firms::Firm>, TenancyError> {
    let pg = db().await?;
    let row = sqlx::query("SELECT id,name,slug,status,entity_id,inserted_at,updated_at FROM firm WHERE entity_id=$1 LIMIT 1")
        .bind(entity_id).fetch_optional(pg.pool()).await?;
    row.as_ref().map(firm).transpose()
}

pub async fn memberships_for_person(
    person_id: Uuid,
) -> Result<Vec<firms::PersonFirmRole>, TenancyError> {
    membership_query("SELECT id,person_id,firm_id,membership,is_dri,inserted_at,updated_at FROM person_firm_role WHERE person_id=$1 ORDER BY inserted_at,id", person_id).await
}

pub async fn memberships_for_firm(
    firm_id: Uuid,
) -> Result<Vec<firms::PersonFirmRole>, TenancyError> {
    membership_query("SELECT id,person_id,firm_id,membership,is_dri,inserted_at,updated_at FROM person_firm_role WHERE firm_id=$1 ORDER BY inserted_at,id", firm_id).await
}

pub async fn all_memberships() -> Result<Vec<firms::PersonFirmRole>, TenancyError> {
    let pg = db().await?;
    sqlx::query("SELECT id,person_id,firm_id,membership,is_dri,inserted_at,updated_at FROM person_firm_role ORDER BY firm_id,person_id")
        .fetch_all(pg.pool()).await?.iter().map(membership_row).collect()
}

pub async fn membership(
    person_id: Uuid,
    firm_id: Uuid,
) -> Result<Option<firms::PersonFirmRole>, TenancyError> {
    let pg = db().await?;
    let row = sqlx::query("SELECT id,person_id,firm_id,membership,is_dri,inserted_at,updated_at FROM person_firm_role WHERE person_id=$1 AND firm_id=$2 LIMIT 1")
        .bind(person_id).bind(firm_id).fetch_optional(pg.pool()).await?;
    row.as_ref().map(membership_row).transpose()
}

pub async fn add_membership(
    input: &firms::NewPersonFirmRole,
) -> Result<firms::PersonFirmRole, firms::FirmError> {
    let pg = db().await?;
    add_membership_on(&pg, input).await
}

pub(crate) async fn add_membership_on(
    pg: &crate::postgres::PostgresDb,
    input: &firms::NewPersonFirmRole,
) -> Result<firms::PersonFirmRole, firms::FirmError> {
    let role: Option<String> = sqlx::query_scalar("SELECT role FROM person WHERE id=$1")
        .bind(input.person_id)
        .fetch_optional(pg.pool())
        .await?;
    match role.as_deref() {
        None => return Err(firms::FirmError::NoSuchPerson(input.person_id)),
        Some("client") => return Err(firms::FirmError::ClientCannotJoinFirm(input.person_id)),
        Some(_) => {}
    }
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM firm WHERE id=$1)")
        .bind(input.firm_id)
        .fetch_one(pg.pool())
        .await?;
    if !exists {
        return Err(firms::FirmError::NoSuchFirm(input.firm_id));
    }
    let row = sqlx::query("INSERT INTO person_firm_role (id,person_id,firm_id,membership,is_dri) VALUES ($1,$2,$3,$4,$5) RETURNING id,person_id,firm_id,membership,is_dri,inserted_at,updated_at")
        .bind(Uuid::new_v4())
        .bind(input.person_id)
        .bind(input.firm_id)
        .bind(input.membership.as_str())
        .bind(input.is_dri)
        .fetch_one(pg.pool())
        .await
        .map_err(|error| {
            if error.as_database_error().and_then(sqlx::error::DatabaseError::constraint).is_some_and(|name| name == "person_firm_role_person_id_firm_id_key") {
                firms::FirmError::DuplicateMembership
            } else { firms::FirmError::Postgres(TenancyError::Query(error)) }
        })?;
    membership_row(&row).map_err(firms::FirmError::Postgres)
}

pub async fn update_membership(
    actor_role: crate::persons::Role,
    actor_person_id: Option<Uuid>,
    person_id: Uuid,
    firm_id: Uuid,
    membership: firms::FirmMembership,
) -> Result<firms::PersonFirmRole, firms::FirmError> {
    let pg = db().await?;
    authorize_firm_management(pg.pool(), actor_role, actor_person_id, firm_id).await?;
    let mut tx = pg.pool().begin().await?;
    let status: Option<String> =
        sqlx::query_scalar("SELECT status FROM firm WHERE id=$1 FOR UPDATE")
            .bind(firm_id)
            .fetch_optional(&mut *tx)
            .await?;
    if status.is_none() {
        return Err(firms::FirmError::NoSuchFirm(firm_id));
    }
    let row = sqlx::query("SELECT id,person_id,firm_id,membership,is_dri,inserted_at,updated_at FROM person_firm_role WHERE person_id=$1 AND firm_id=$2 FOR UPDATE")
        .bind(person_id).bind(firm_id).fetch_optional(&mut *tx).await?
        .ok_or(firms::FirmError::NotAMember(person_id, firm_id))?;
    let current = membership_row(&row).map_err(firms::FirmError::Postgres)?;
    if current.is_dri
        && status.as_deref() == Some("active")
        && membership != firms::FirmMembership::Admin
    {
        return Err(firms::FirmError::WouldLeaveFirmWithoutAdminDri(firm_id));
    }
    let updated = sqlx::query("UPDATE person_firm_role SET membership=$3,updated_at=now() WHERE person_id=$1 AND firm_id=$2 RETURNING id,person_id,firm_id,membership,is_dri,inserted_at,updated_at")
        .bind(person_id).bind(firm_id).bind(membership.as_str())
        .fetch_one(&mut *tx).await?;
    tx.commit().await?;
    membership_row(&updated).map_err(firms::FirmError::Postgres)
}

pub async fn remove_membership(
    actor_role: crate::persons::Role,
    actor_person_id: Option<Uuid>,
    person_id: Uuid,
    firm_id: Uuid,
) -> Result<(), firms::FirmError> {
    let pg = db().await?;
    authorize_firm_management(pg.pool(), actor_role, actor_person_id, firm_id).await?;
    let mut tx = pg.pool().begin().await?;
    let status: Option<String> =
        sqlx::query_scalar("SELECT status FROM firm WHERE id=$1 FOR UPDATE")
            .bind(firm_id)
            .fetch_optional(&mut *tx)
            .await?;
    if status.is_none() {
        return Err(firms::FirmError::NoSuchFirm(firm_id));
    }
    let row = sqlx::query(
        "SELECT is_dri FROM person_firm_role WHERE person_id=$1 AND firm_id=$2 FOR UPDATE",
    )
    .bind(person_id)
    .bind(firm_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(firms::FirmError::NotAMember(person_id, firm_id))?;
    let is_dri: bool = row.try_get("is_dri")?;
    if is_dri && status.as_deref() == Some("active") {
        return Err(firms::FirmError::WouldLeaveFirmWithoutAdminDri(firm_id));
    }
    sqlx::query("DELETE FROM person_firm_role WHERE person_id=$1 AND firm_id=$2")
        .bind(person_id)
        .bind(firm_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn appoint_admin_dri(
    actor_role: crate::persons::Role,
    firm_id: Uuid,
    person_id: Uuid,
) -> Result<firms::PersonFirmRole, firms::FirmError> {
    if actor_role != crate::persons::Role::Owner {
        return Err(firms::FirmError::NotAuthorized);
    }
    let pg = db().await?;
    appoint_admin_dri_on(&pg, firm_id, person_id).await
}

pub(crate) async fn appoint_admin_dri_on(
    pg: &crate::postgres::PostgresDb,
    firm_id: Uuid,
    person_id: Uuid,
) -> Result<firms::PersonFirmRole, firms::FirmError> {
    let mut tx = pg.pool().begin().await?;
    let firm_row = sqlx::query("SELECT id FROM firm WHERE id=$1 FOR UPDATE")
        .bind(firm_id)
        .fetch_optional(&mut *tx)
        .await?;
    if firm_row.is_none() {
        return Err(firms::FirmError::NoSuchFirm(firm_id));
    }
    let person_role: Option<String> = sqlx::query_scalar("SELECT role FROM person WHERE id=$1")
        .bind(person_id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(person_role) = person_role else {
        return Err(firms::FirmError::NoSuchPerson(person_id));
    };
    if !matches!(person_role.as_str(), "owner" | "admin") {
        return Err(firms::FirmError::IneligibleAdminDriTier(person_id));
    }
    let membership = sqlx::query(
        "SELECT membership FROM person_firm_role WHERE person_id=$1 AND firm_id=$2 FOR UPDATE",
    )
    .bind(person_id)
    .bind(firm_id)
    .fetch_optional(&mut *tx)
    .await?;
    match membership {
        None if person_role == "owner" => {
            sqlx::query("UPDATE person_firm_role SET is_dri=false,updated_at=now() WHERE firm_id=$1 AND is_dri=true")
                .bind(firm_id).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO person_firm_role (id,person_id,firm_id,membership,is_dri) VALUES ($1,$2,$3,'admin',true)")
                .bind(Uuid::new_v4()).bind(person_id).bind(firm_id).execute(&mut *tx).await?;
        }
        None => return Err(firms::FirmError::AdminDriCrossFirm(person_id, firm_id)),
        Some(row) => {
            let membership: String = row.try_get("membership")?;
            if membership != "admin" {
                return Err(firms::FirmError::WrongAdminDriMembership(
                    person_id, firm_id,
                ));
            }
            sqlx::query("UPDATE person_firm_role SET is_dri=false,updated_at=now() WHERE firm_id=$1 AND is_dri=true")
                .bind(firm_id).execute(&mut *tx).await?;
            sqlx::query("UPDATE person_firm_role SET is_dri=true,updated_at=now() WHERE person_id=$1 AND firm_id=$2")
                .bind(person_id).bind(firm_id).execute(&mut *tx).await?;
        }
    }
    tx.commit().await?;
    let row = sqlx::query("SELECT id,person_id,firm_id,membership,is_dri,inserted_at,updated_at FROM person_firm_role WHERE person_id=$1 AND firm_id=$2")
        .bind(person_id).bind(firm_id).fetch_optional(pg.pool()).await?
        .ok_or(firms::FirmError::WriteReturnedNothing)?;
    membership_row(&row).map_err(firms::FirmError::Postgres)
}

pub async fn attach_brand(firm_id: Uuid, brand_key: &str) -> Result<(), firms::FirmError> {
    let pg = db().await?;
    if find_firm_on(&pg, firm_id).await?.is_none() {
        return Err(firms::FirmError::NoSuchFirm(firm_id));
    }
    let brand_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM brand WHERE brand_key=$1)")
            .bind(brand_key)
            .fetch_one(pg.pool())
            .await?;
    if !brand_exists {
        return Err(firms::FirmError::UnknownBrand(brand_key.to_string()));
    }
    let result = sqlx::query("INSERT INTO firm_brand (id,firm_id,brand_key) VALUES ($1,$2,$3)")
        .bind(Uuid::new_v4())
        .bind(firm_id)
        .bind(brand_key)
        .execute(pg.pool())
        .await;
    result.map(|_| ()).map_err(|error| {
        if error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::constraint)
            .is_some_and(|name| {
                matches!(
                    name,
                    "firm_brand_brand_key_key" | "firm_brand_firm_id_brand_key_key"
                )
            })
        {
            firms::FirmError::DuplicateBrand
        } else {
            firms::FirmError::Postgres(TenancyError::Query(error))
        }
    })
}

pub async fn firm_brand_keys(firm_id: Uuid) -> Result<Vec<String>, TenancyError> {
    let pg = db().await?;
    sqlx::query_scalar("SELECT brand_key FROM firm_brand WHERE firm_id=$1 ORDER BY brand_key")
        .bind(firm_id)
        .fetch_all(pg.pool())
        .await
        .map_err(Into::into)
}

pub async fn firm_for_brand_key(brand_key: &str) -> Result<Option<Uuid>, TenancyError> {
    let pg = db().await?;
    sqlx::query_scalar("SELECT firm_id FROM firm_brand WHERE brand_key=$1")
        .bind(brand_key)
        .fetch_optional(pg.pool())
        .await
        .map_err(Into::into)
}

pub async fn all_firm_brand_rows() -> Result<Vec<firms::FirmBrand>, TenancyError> {
    let pg = db().await?;
    sqlx::query(
        "SELECT id,firm_id,brand_key,inserted_at,updated_at FROM firm_brand ORDER BY brand_key",
    )
    .fetch_all(pg.pool())
    .await?
    .iter()
    .map(firm_brand_row)
    .collect()
}

pub async fn firm_brand_row_for_key(key: &str) -> Result<Option<firms::FirmBrand>, TenancyError> {
    let pg = db().await?;
    let row = sqlx::query(
        "SELECT id,firm_id,brand_key,inserted_at,updated_at FROM firm_brand WHERE brand_key=$1",
    )
    .bind(key)
    .fetch_optional(pg.pool())
    .await?;
    row.as_ref().map(firm_brand_row).transpose()
}

fn firm_brand_row(row: &sqlx::postgres::PgRow) -> Result<firms::FirmBrand, TenancyError> {
    Ok(firms::FirmBrand {
        id: row.try_get("id")?,
        firm_id: row.try_get("firm_id")?,
        brand_key: row.try_get("brand_key")?,
        inserted_at: time(row.try_get("inserted_at")?),
        updated_at: time(row.try_get("updated_at")?),
    })
}

pub async fn detach_brand(firm_id: Uuid, brand_key: &str) -> Result<(), firms::FirmError> {
    let pg = db().await?;
    sqlx::query("DELETE FROM firm_brand WHERE firm_id=$1 AND brand_key=$2")
        .bind(firm_id)
        .bind(brand_key)
        .execute(pg.pool())
        .await?;
    Ok(())
}

/// Move one Brand to another Firm in one transaction: the old `firm_brand`
/// row is removed, the `brand` row's owning Firm is re-pointed, and the
/// target Firm's row is created together, so a partial failure leaves the
/// old association intact. A move to the Firm that already wears the brand,
/// with the `brand` row already naming it, is a no-op. Authorization
/// happens in [`crate::firms::move_brand`] before this write runs.
pub async fn move_brand(brand_key: &str, target_firm_id: Uuid) -> Result<(), firms::FirmError> {
    let pg = db().await?;
    let mut tx = pg.pool().begin().await?;
    let brand_id: Option<Uuid> = sqlx::query_scalar("SELECT id FROM brand WHERE brand_key=$1")
        .bind(brand_key)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(brand_id) = brand_id else {
        return Err(firms::FirmError::UnknownBrand(brand_key.to_string()));
    };
    let target_exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM firm WHERE id=$1)")
        .bind(target_firm_id)
        .fetch_one(&mut *tx)
        .await?;
    if !target_exists {
        return Err(firms::FirmError::NoSuchFirm(target_firm_id));
    }
    let worn_by: Option<Uuid> =
        sqlx::query_scalar("SELECT firm_id FROM firm_brand WHERE brand_key=$1")
            .bind(brand_key)
            .fetch_optional(&mut *tx)
            .await?;
    let owned_by: Option<Uuid> = sqlx::query_scalar("SELECT firm_id FROM brand WHERE id=$1")
        .bind(brand_id)
        .fetch_one(&mut *tx)
        .await?;
    if worn_by == Some(target_firm_id) && owned_by == Some(target_firm_id) {
        return Ok(());
    }
    sqlx::query("DELETE FROM firm_brand WHERE brand_key=$1")
        .bind(brand_key)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE brand SET firm_id=$2, updated_at=now() WHERE id=$1")
        .bind(brand_id)
        .bind(target_firm_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO firm_brand (id,firm_id,brand_key) VALUES ($1,$2,$3)")
        .bind(Uuid::new_v4())
        .bind(target_firm_id)
        .bind(brand_key)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            if error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::constraint)
                .is_some_and(|name| {
                    matches!(
                        name,
                        "firm_brand_brand_key_key" | "firm_brand_firm_id_brand_key_key"
                    )
                })
            {
                firms::FirmError::DuplicateBrand
            } else {
                firms::FirmError::Postgres(TenancyError::Query(error))
            }
        })?;
    tx.commit().await?;
    Ok(())
}

async fn membership_query(
    sql: &'static str,
    id: Uuid,
) -> Result<Vec<firms::PersonFirmRole>, TenancyError> {
    let pg = db().await?;
    sqlx::query(sql)
        .bind(id)
        .fetch_all(pg.pool())
        .await?
        .iter()
        .map(membership_row)
        .collect()
}

fn membership_row(row: &sqlx::postgres::PgRow) -> Result<firms::PersonFirmRole, TenancyError> {
    let membership = firms::FirmMembership::parse(&row.try_get::<String, _>("membership")?)
        .ok_or(TenancyError::InvalidRow)?;
    Ok(firms::PersonFirmRole {
        id: row.try_get("id")?,
        person_id: row.try_get("person_id")?,
        firm_id: row.try_get("firm_id")?,
        membership,
        is_dri: row.try_get("is_dri")?,
        inserted_at: time(row.try_get("inserted_at")?),
        updated_at: time(row.try_get("updated_at")?),
    })
}

/// Read a Brand by id from the authoritative store.
pub async fn find_brand(id: Uuid) -> Result<Option<brands::Brand>, TenancyError> {
    let pg = db().await?;
    let row = sqlx::query("SELECT id,name,brand_key,firm_id,primary_color,accent_color,typeface,is_law_firm,legal_entity,logo_object_key,logo_content_type,font_family,font_object_key,font_licence,website_url,inserted_at,updated_at FROM brand WHERE id=$1")
        .bind(id).fetch_optional(pg.pool()).await?;
    row.as_ref().map(brand).transpose()
}

pub async fn brand_by_key(key: &str) -> Result<Option<brands::Brand>, TenancyError> {
    let pg = db().await?;
    let row = sqlx::query("SELECT id,name,brand_key,firm_id,primary_color,accent_color,typeface,is_law_firm,legal_entity,logo_object_key,logo_content_type,font_family,font_object_key,font_licence,website_url,inserted_at,updated_at FROM brand WHERE brand_key=$1 LIMIT 1")
        .bind(key).fetch_optional(pg.pool()).await?;
    row.as_ref().map(brand).transpose()
}

pub async fn brands_for_firm(firm_id: Uuid) -> Result<Vec<brands::Brand>, TenancyError> {
    brand_list("firm_id=$1", Some(firm_id)).await
}

pub async fn create_brand(
    actor_role: crate::persons::Role,
    actor_person_id: Option<Uuid>,
    input: &brands::NewBrand,
) -> Result<brands::Brand, brands::BrandError> {
    let person_id = actor_person_id.ok_or(brands::BrandError::NotAuthorized)?;
    let pg = db().await?;
    create_brand_on(&pg, actor_role, person_id, input).await
}

pub(crate) async fn create_brand_on(
    pg: &crate::postgres::PostgresDb,
    actor_role: crate::persons::Role,
    person_id: Uuid,
    input: &brands::NewBrand,
) -> Result<brands::Brand, brands::BrandError> {
    let firm_id = input.firm_id.ok_or(brands::BrandError::FirmRequired)?;
    if actor_role == crate::persons::Role::Client {
        return Err(brands::BrandError::NotAuthorized);
    }
    let firm_row = sqlx::query("SELECT entity_id FROM firm WHERE id=$1")
        .bind(firm_id)
        .fetch_optional(pg.pool())
        .await?
        .ok_or(brands::BrandError::NoSuchFirm(firm_id))?;
    let entity_id: Option<Uuid> = firm_row.try_get("entity_id")?;
    let legal_entity = if let Some(id) = entity_id {
        sqlx::query_scalar::<_, String>("SELECT name FROM entity WHERE id=$1")
            .bind(id)
            .fetch_optional(pg.pool())
            .await?
    } else {
        None
    };
    let authorized: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM person_firm_role WHERE person_id=$1 AND firm_id=$2 AND membership='admin' AND is_dri)")
        .bind(person_id).bind(firm_id).fetch_one(pg.pool()).await?;
    if !authorized {
        return Err(brands::BrandError::NotAuthorized);
    }
    let row = sqlx::query("INSERT INTO brand (id,name,brand_key,firm_id,primary_color,accent_color,typeface,is_law_firm,legal_entity) VALUES ($1,$2,$3,$4,$5,$6,$7,true,$8) RETURNING id,name,brand_key,firm_id,primary_color,accent_color,typeface,is_law_firm,legal_entity,logo_object_key,logo_content_type,font_family,font_object_key,font_licence,website_url,inserted_at,updated_at")
        .bind(Uuid::new_v4()).bind(&input.name).bind(&input.key).bind(firm_id)
        .bind(&input.primary_color).bind(&input.accent_color).bind(&input.typeface).bind(legal_entity)
        .fetch_one(pg.pool()).await.map_err(|error| {
            match error.as_database_error().and_then(sqlx::error::DatabaseError::constraint) {
                Some("brand_name") => brands::BrandError::DuplicateName,
                Some("brand_brand_key_key") => brands::BrandError::DuplicateKey,
                _ => brands::BrandError::Postgres(TenancyError::Query(error)),
            }
        })?;
    brand(&row).map_err(brands::BrandError::Postgres)
}

pub async fn seed_brand(
    input: &brands::NewBrand,
    website_url: Option<&str>,
) -> Result<brands::Brand, brands::BrandError> {
    let firm_id = input.firm_id.ok_or(brands::BrandError::FirmRequired)?;
    let pg = db().await?;
    let firm_row = sqlx::query("SELECT entity_id FROM firm WHERE id=$1")
        .bind(firm_id)
        .fetch_optional(pg.pool())
        .await?
        .ok_or(brands::BrandError::NoSuchFirm(firm_id))?;
    let entity_id: Option<Uuid> = firm_row.try_get("entity_id")?;
    let legal_entity = if let Some(id) = entity_id {
        sqlx::query_scalar::<_, String>("SELECT name FROM entity WHERE id=$1")
            .bind(id)
            .fetch_optional(pg.pool())
            .await?
    } else {
        None
    };
    if let Some(existing) = brand_by_key(&input.key).await? {
        let row = sqlx::query("UPDATE brand SET firm_id=$2,primary_color=$3,accent_color=$4,typeface=$5,updated_at=now() WHERE id=$1 RETURNING id,name,brand_key,firm_id,primary_color,accent_color,typeface,is_law_firm,legal_entity,logo_object_key,logo_content_type,font_family,font_object_key,font_licence,website_url,inserted_at,updated_at")
            .bind(existing.id).bind(firm_id).bind(&input.primary_color).bind(&input.accent_color).bind(&input.typeface)
            .fetch_one(pg.pool()).await.map_err(map_brand_write)?;
        return brand(&row).map_err(brands::BrandError::Postgres);
    }
    let website_url = website_url
        .map(crate::brand_domains::canonical_website_url)
        .transpose()?;
    let row = sqlx::query("INSERT INTO brand (id,name,brand_key,firm_id,primary_color,accent_color,typeface,is_law_firm,legal_entity,website_url) VALUES ($1,$2,$3,$4,$5,$6,$7,true,$8,$9) RETURNING id,name,brand_key,firm_id,primary_color,accent_color,typeface,is_law_firm,legal_entity,logo_object_key,logo_content_type,font_family,font_object_key,font_licence,website_url,inserted_at,updated_at")
        .bind(Uuid::new_v4()).bind(&input.name).bind(&input.key).bind(firm_id)
        .bind(&input.primary_color).bind(&input.accent_color).bind(&input.typeface).bind(legal_entity).bind(website_url)
        .fetch_one(pg.pool()).await.map_err(map_brand_write)?;
    brand(&row).map_err(brands::BrandError::Postgres)
}

fn map_brand_write(error: sqlx::Error) -> brands::BrandError {
    match error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::constraint)
    {
        Some("brand_name") => brands::BrandError::DuplicateName,
        Some("brand_brand_key_key") => brands::BrandError::DuplicateKey,
        _ => brands::BrandError::Postgres(TenancyError::Query(error)),
    }
}

pub async fn update_brand(
    actor_role: crate::persons::Role,
    actor_person_id: Option<Uuid>,
    brand_id: Uuid,
    input: &brands::BrandEdit,
) -> Result<brands::Brand, brands::BrandError> {
    let pg = db().await?;
    update_brand_on(&pg, actor_role, actor_person_id, brand_id, input).await
}

pub(crate) async fn update_brand_on(
    pg: &crate::postgres::PostgresDb,
    actor_role: crate::persons::Role,
    actor_person_id: Option<Uuid>,
    brand_id: Uuid,
    input: &brands::BrandEdit,
) -> Result<brands::Brand, brands::BrandError> {
    let existing = find_brand_on(pg, brand_id)
        .await?
        .ok_or(brands::BrandError::NoSuchBrand(brand_id))?;
    authorize_brand_edit(pg.pool(), actor_role, actor_person_id, existing.firm_id).await?;
    if input.name.is_none()
        && input.primary_color.is_none()
        && input.accent_color.is_none()
        && input.typeface.is_none()
        && input.font_family.is_none()
    {
        return Ok(existing);
    }
    let row = sqlx::query("UPDATE brand SET name=CASE WHEN $2 THEN $3 ELSE name END, primary_color=CASE WHEN $4 THEN $5 ELSE primary_color END, accent_color=CASE WHEN $6 THEN $7 ELSE accent_color END, typeface=CASE WHEN $8 THEN $9 ELSE typeface END, font_family=CASE WHEN $10 THEN $11 ELSE font_family END, updated_at=now() WHERE id=$1 RETURNING id,name,brand_key,firm_id,primary_color,accent_color,typeface,is_law_firm,legal_entity,logo_object_key,logo_content_type,font_family,font_object_key,font_licence,website_url,inserted_at,updated_at")
        .bind(brand_id)
        .bind(input.name.is_some()).bind(input.name.as_deref())
        .bind(input.primary_color.is_some()).bind(input.primary_color.as_ref().and_then(Option::as_deref))
        .bind(input.accent_color.is_some()).bind(input.accent_color.as_ref().and_then(Option::as_deref))
        .bind(input.typeface.is_some()).bind(input.typeface.as_ref().and_then(Option::as_deref))
        .bind(input.font_family.is_some()).bind(input.font_family.as_ref().and_then(Option::as_deref))
        .fetch_one(pg.pool()).await.map_err(|error| {
            if error.as_database_error().and_then(sqlx::error::DatabaseError::constraint) == Some("brand_name") {
                brands::BrandError::DuplicateName
            } else { brands::BrandError::Postgres(TenancyError::Query(error)) }
        })?;
    brand(&row).map_err(brands::BrandError::Postgres)
}

pub async fn set_brand_logo(
    actor_role: crate::persons::Role,
    actor_person_id: Option<Uuid>,
    brand_id: Uuid,
    object_key: &str,
    content_type: &str,
) -> Result<brands::Brand, brands::BrandError> {
    let pg = db().await?;
    let existing = find_brand_on(&pg, brand_id)
        .await?
        .ok_or(brands::BrandError::NoSuchBrand(brand_id))?;
    authorize_brand_edit(pg.pool(), actor_role, actor_person_id, existing.firm_id).await?;
    let row = sqlx::query("UPDATE brand SET logo_object_key=$2,logo_content_type=$3,updated_at=now() WHERE id=$1 RETURNING id,name,brand_key,firm_id,primary_color,accent_color,typeface,is_law_firm,legal_entity,logo_object_key,logo_content_type,font_family,font_object_key,font_licence,website_url,inserted_at,updated_at")
        .bind(brand_id).bind(object_key).bind(content_type).fetch_one(pg.pool()).await?;
    brand(&row).map_err(brands::BrandError::Postgres)
}

pub async fn set_brand_font(
    actor_role: crate::persons::Role,
    actor_person_id: Option<Uuid>,
    brand_id: Uuid,
    family: &str,
    object_key: &str,
    licence: &str,
) -> Result<brands::Brand, brands::BrandError> {
    let pg = db().await?;
    let existing = find_brand_on(&pg, brand_id)
        .await?
        .ok_or(brands::BrandError::NoSuchBrand(brand_id))?;
    authorize_brand_edit(pg.pool(), actor_role, actor_person_id, existing.firm_id).await?;
    let row = sqlx::query("UPDATE brand SET font_family=$2,font_object_key=$3,font_licence=$4,updated_at=now() WHERE id=$1 RETURNING id,name,brand_key,firm_id,primary_color,accent_color,typeface,is_law_firm,legal_entity,logo_object_key,logo_content_type,font_family,font_object_key,font_licence,website_url,inserted_at,updated_at")
        .bind(brand_id).bind(family).bind(object_key).bind(licence).fetch_one(pg.pool()).await?;
    brand(&row).map_err(brands::BrandError::Postgres)
}

pub async fn set_brand_website(
    brand_id: Uuid,
    website_url: Option<&str>,
) -> Result<brands::Brand, brands::BrandError> {
    let pg = db().await?;
    let website_url = website_url
        .map(crate::brand_domains::canonical_website_url)
        .transpose()?;
    let row = sqlx::query("UPDATE brand SET website_url=$2,updated_at=now() WHERE id=$1 RETURNING id,name,brand_key,firm_id,primary_color,accent_color,typeface,is_law_firm,legal_entity,logo_object_key,logo_content_type,font_family,font_object_key,font_licence,website_url,inserted_at,updated_at")
        .bind(brand_id).bind(website_url).fetch_optional(pg.pool()).await?
        .ok_or(brands::BrandError::NoSuchBrand(brand_id))?;
    brand(&row).map_err(brands::BrandError::Postgres)
}

pub async fn delete_brand(
    actor_role: crate::persons::Role,
    actor_person_id: Option<Uuid>,
    brand_id: Uuid,
) -> Result<(), brands::BrandError> {
    let pg = db().await?;
    delete_brand_on(&pg, actor_role, actor_person_id, brand_id).await
}

pub(crate) async fn delete_brand_on(
    pg: &crate::postgres::PostgresDb,
    actor_role: crate::persons::Role,
    actor_person_id: Option<Uuid>,
    brand_id: Uuid,
) -> Result<(), brands::BrandError> {
    let existing = find_brand_on(pg, brand_id)
        .await?
        .ok_or(brands::BrandError::NoSuchBrand(brand_id))?;
    authorize_brand_edit(pg.pool(), actor_role, actor_person_id, existing.firm_id).await?;
    let referenced: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM firm_brand WHERE brand_key=$1) OR EXISTS(SELECT 1 FROM project WHERE brand=$1)")
        .bind(&existing.key).fetch_one(pg.pool()).await?;
    if referenced {
        return Err(brands::BrandError::StillReferenced);
    }
    sqlx::query("DELETE FROM brand WHERE id=$1")
        .bind(brand_id)
        .execute(pg.pool())
        .await?;
    Ok(())
}

pub(crate) async fn find_brand_on(
    pg: &crate::postgres::PostgresDb,
    id: Uuid,
) -> Result<Option<brands::Brand>, TenancyError> {
    let row = sqlx::query("SELECT id,name,brand_key,firm_id,primary_color,accent_color,typeface,is_law_firm,legal_entity,logo_object_key,logo_content_type,font_family,font_object_key,font_licence,website_url,inserted_at,updated_at FROM brand WHERE id=$1")
        .bind(id).fetch_optional(pg.pool()).await?;
    row.as_ref().map(brand).transpose()
}

async fn authorize_brand_edit(
    pool: &sqlx::PgPool,
    actor_role: crate::persons::Role,
    actor_person_id: Option<Uuid>,
    firm_id: Option<Uuid>,
) -> Result<(), brands::BrandError> {
    if firm_id.is_none() {
        return if actor_role == crate::persons::Role::Owner {
            Ok(())
        } else {
            Err(brands::BrandError::NotAuthorized)
        };
    }
    if actor_role == crate::persons::Role::Owner {
        return Ok(());
    }
    if actor_role == crate::persons::Role::Client {
        return Err(brands::BrandError::NotAuthorized);
    }
    let person_id = actor_person_id.ok_or(brands::BrandError::NotAuthorized)?;
    let firm_id = firm_id.unwrap();
    let authorized: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM person_firm_role WHERE person_id=$1 AND firm_id=$2 AND membership='admin' AND is_dri)")
        .bind(person_id).bind(firm_id).fetch_one(pool).await?;
    if authorized {
        Ok(())
    } else {
        Err(brands::BrandError::NotAuthorized)
    }
}

pub async fn system_brands() -> Result<Vec<brands::Brand>, TenancyError> {
    brand_list("firm_id IS NULL", None).await
}

pub async fn all_firm_brands() -> Result<Vec<brands::Brand>, TenancyError> {
    brand_list("firm_id IS NOT NULL", None).await
}

pub async fn brand_for_actor(
    actor_role: crate::persons::Role,
    actor_person_id: Option<Uuid>,
    key: &str,
) -> Result<brands::Brand, brands::BrandError> {
    let brand = brand_by_key(key)
        .await?
        .ok_or(brands::BrandError::NoSuchBrand(Uuid::nil()))?;
    let pg = db().await?;
    authorize_brand_edit(pg.pool(), actor_role, actor_person_id, brand.firm_id).await?;
    Ok(brand)
}

pub async fn visible_brands_for_actor(
    actor_role: crate::persons::Role,
    actor_person_id: Option<Uuid>,
) -> Result<Vec<brands::Brand>, TenancyError> {
    let mut brands = match actor_role {
        crate::persons::Role::Owner => {
            let mut rows = system_brands().await?;
            rows.extend(all_firm_brands().await?);
            rows
        }
        crate::persons::Role::Admin => {
            let mut rows = system_brands().await?;
            if let Some(person_id) = actor_person_id {
                let pg = db().await?;
                let ids: Vec<Uuid> = sqlx::query_scalar("SELECT firm_id FROM person_firm_role WHERE person_id=$1 AND membership='admin' AND is_dri ORDER BY firm_id")
                    .bind(person_id).fetch_all(pg.pool()).await?;
                for id in ids {
                    rows.extend(brands_for_firm(id).await?);
                }
            }
            rows
        }
        _ => Vec::new(),
    };
    brands.sort_by(|left, right| left.name.cmp(&right.name).then(left.id.cmp(&right.id)));
    Ok(brands)
}

async fn brand_list(
    filter: &str,
    firm_id: Option<Uuid>,
) -> Result<Vec<brands::Brand>, TenancyError> {
    let pg = db().await?;
    let rows = match (filter, firm_id) {
        ("firm_id=$1", Some(id)) => sqlx::query("SELECT id,name,brand_key,firm_id,primary_color,accent_color,typeface,is_law_firm,legal_entity,logo_object_key,logo_content_type,font_family,font_object_key,font_licence,website_url,inserted_at,updated_at FROM brand WHERE firm_id=$1 ORDER BY name,id").bind(id).fetch_all(pg.pool()).await?,
        ("firm_id IS NULL", None) => sqlx::query("SELECT id,name,brand_key,firm_id,primary_color,accent_color,typeface,is_law_firm,legal_entity,logo_object_key,logo_content_type,font_family,font_object_key,font_licence,website_url,inserted_at,updated_at FROM brand WHERE firm_id IS NULL ORDER BY name,id").fetch_all(pg.pool()).await?,
        ("firm_id IS NOT NULL", None) => sqlx::query("SELECT id,name,brand_key,firm_id,primary_color,accent_color,typeface,is_law_firm,legal_entity,logo_object_key,logo_content_type,font_family,font_object_key,font_licence,website_url,inserted_at,updated_at FROM brand WHERE firm_id IS NOT NULL ORDER BY name,id").fetch_all(pg.pool()).await?,
        _ => return Err(TenancyError::Configuration),
    };
    rows.iter().map(brand).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenancy_authority_fails_closed_until_project_cutover_is_ready() {
        assert!(!enabled_value(None, false).unwrap());
        assert!(!enabled_value(Some("0"), false).unwrap());
        assert!(!enabled_value(Some("false"), false).unwrap());
        assert!(matches!(
            enabled_value(Some("1"), false),
            Err(TenancyError::CutoverIncomplete)
        ));
        assert!(matches!(
            enabled_value(Some("true"), false),
            Err(TenancyError::CutoverIncomplete)
        ));
        assert!(enabled_value(Some("1"), true).unwrap());
        assert!(matches!(
            enabled_value(Some("yes"), true),
            Err(TenancyError::Configuration)
        ));
    }
}

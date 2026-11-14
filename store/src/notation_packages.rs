//! The catalog of Notation packages and their ordered template assemblies.
//!
//! A package is the client-facing deliverable: an estate package or employee
//! onboarding can contain several template instances, while each instance is
//! still a separate [`crate::notations::Notation`] with its own signing and
//! audit trail. The package holds a starting price, never an invoice or a
//! promise to perform unquoted work. Xero remains the invoicing record.
//!
//! PostgreSQL holds the catalog wherever it is configured: every read and
//! write here goes to PostgreSQL
//! ([`crate::postgres::catalog`]), where a package template must name a
//! Template version PostgreSQL already holds.

use chrono::{DateTime, Utc};
use serde::Serialize;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::postgres::catalog;
use crate::surreal::{record_id, record_uuid, SurrealDb};

const PACKAGE_TABLE: &str = "notation_package";
const MEMBER_TABLE: &str = "notation_package_template";
const TEMPLATE_TABLE: &str = "template";

/// One client-facing bundle of one or more notation templates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NotationPackage {
    pub id: Uuid,
    pub code: String,
    pub title: String,
    pub description: Option<String>,
    /// The pricing/assembly complexity selected by the firm.
    pub complexity: String,
    /// Quoted minimum in minor currency units; the actual matter scope may
    /// require a higher agreed fee.
    pub starting_price_cents: i64,
    pub currency: String,
    pub active: bool,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(SurrealValue)]
struct PackageRow {
    id: surrealdb::types::RecordId,
    code: String,
    title: String,
    description: Option<String>,
    complexity: String,
    starting_price_cents: i64,
    currency: String,
    active: bool,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

impl PackageRow {
    fn into_model(self) -> Option<NotationPackage> {
        Some(NotationPackage {
            id: record_uuid(&self.id)?,
            code: self.code,
            title: self.title,
            description: self.description,
            complexity: self.complexity,
            starting_price_cents: self.starting_price_cents,
            currency: self.currency,
            active: self.active,
            inserted_at: self.inserted_at.into(),
            updated_at: self.updated_at.into(),
        })
    }
}

/// One template in a package, in assembly order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackageTemplate {
    pub package_id: Uuid,
    pub template_id: Uuid,
    pub position: i32,
    pub required: bool,
}

#[derive(SurrealValue)]
struct PackageTemplateRow {
    package_id: surrealdb::types::RecordId,
    template_id: surrealdb::types::RecordId,
    position: i32,
    required: bool,
}

impl PackageTemplateRow {
    fn into_model(self) -> Option<PackageTemplate> {
        Some(PackageTemplate {
            package_id: record_uuid(&self.package_id)?,
            template_id: record_uuid(&self.template_id)?,
            position: self.position,
            required: self.required,
        })
    }
}

const PACKAGE_SELECT: &str = "id, code, title, description, complexity, starting_price_cents, \
                              currency, active, inserted_at, updated_at";
const TEMPLATE_SELECT: &str = "package_id, template_id, position, required";

/// Input to create one package in the catalog.
#[derive(Debug, Clone)]
pub struct NewNotationPackage {
    pub code: String,
    pub title: String,
    pub description: Option<String>,
    pub complexity: String,
    pub starting_price_cents: i64,
    pub currency: String,
}

impl NewNotationPackage {
    #[must_use]
    pub fn new(
        code: impl Into<String>,
        title: impl Into<String>,
        complexity: impl Into<String>,
        starting_price_cents: i64,
    ) -> Self {
        Self {
            code: code.into(),
            title: title.into(),
            description: None,
            complexity: complexity.into(),
            starting_price_cents,
            currency: "USD".to_string(),
        }
    }
}

/// Errors from the package catalog.
#[derive(Debug, thiserror::Error)]
pub enum NotationPackageError {
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    #[error("writing a notation package returned no usable row")]
    WriteReturnedNothing,
    /// The PostgreSQL catalog authority could not serve the call.
    #[error(transparent)]
    Postgres(#[from] catalog::CatalogError),
}

impl From<sqlx::Error> for NotationPackageError {
    fn from(error: sqlx::Error) -> Self {
        Self::Postgres(error.into())
    }
}

const PG_INSERT: &str = "INSERT INTO notation_package \
     (id, code, title, description, complexity, starting_price_cents, currency) \
     VALUES ($1, $2, $3, $4, $5, $6, $7) \
     RETURNING id, code, title, description, complexity, starting_price_cents, currency, active, \
     inserted_at, updated_at";
const PG_BY_CODE: &str = "SELECT id, code, title, description, complexity, starting_price_cents, \
     currency, active, inserted_at, updated_at FROM notation_package WHERE code = $1";
const PG_ADD_TEMPLATE: &str = "INSERT INTO notation_package_template \
     (id, package_id, template_id, position, required) VALUES ($1, $2, $3, $4, $5)";
const PG_TEMPLATES: &str = "SELECT package_id, template_id, position, required \
     FROM notation_package_template WHERE package_id = $1 ORDER BY position ASC";

fn package_from_pg(row: &sqlx::postgres::PgRow) -> Result<NotationPackage, sqlx::Error> {
    use sqlx::Row;
    Ok(NotationPackage {
        id: row.try_get("id")?,
        code: row.try_get("code")?,
        title: row.try_get("title")?,
        description: row.try_get("description")?,
        complexity: row.try_get("complexity")?,
        starting_price_cents: row.try_get("starting_price_cents")?,
        currency: row.try_get("currency")?,
        active: row.try_get("active")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn template_from_pg(row: &sqlx::postgres::PgRow) -> Result<PackageTemplate, sqlx::Error> {
    use sqlx::Row;
    Ok(PackageTemplate {
        package_id: row.try_get("package_id")?,
        template_id: row.try_get("template_id")?,
        position: row.try_get("position")?,
        required: row.try_get("required")?,
    })
}

fn one(
    mut response: surrealdb::IndexedResults,
) -> Result<Option<NotationPackage>, NotationPackageError> {
    let row: Option<PackageRow> = response.take(0)?;
    Ok(row.and_then(PackageRow::into_model))
}

/// Insert a package catalog entry. Package templates are attached separately
/// with [`add_template`] so their order is explicit.
pub async fn create(
    db: &SurrealDb,
    new: &NewNotationPackage,
) -> Result<NotationPackage, NotationPackageError> {
    let id = Uuid::now_v7();
    if let Some(pg) = catalog::authority().await? {
        let row = catalog::row(
            sqlx::query(PG_INSERT)
                .bind(id)
                .bind(new.code.clone())
                .bind(new.title.clone())
                .bind(new.description.clone())
                .bind(new.complexity.clone())
                .bind(new.starting_price_cents)
                .bind(new.currency.clone()),
            &pg,
        )
        .await?;
        return Ok(package_from_pg(&row)?);
    }
    let mut response = db
        .query(format!(
            "CREATE $id SET code = $code, title = $title, description = $description, \
             complexity = $complexity, starting_price_cents = $starting_price_cents, \
             currency = $currency RETURN {PACKAGE_SELECT}"
        ))
        .bind(("id", record_id(PACKAGE_TABLE, id)))
        .bind(("code", new.code.clone()))
        .bind(("title", new.title.clone()))
        .bind(("description", new.description.clone()))
        .bind(("complexity", new.complexity.clone()))
        .bind(("starting_price_cents", new.starting_price_cents))
        .bind(("currency", new.currency.clone()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let row: Option<PackageRow> = response.take(0)?;
    let package = row
        .and_then(PackageRow::into_model)
        .ok_or(NotationPackageError::WriteReturnedNothing)?;
    crate::postgres_mirror::after_write(
        db,
        crate::postgres_mirror::MirrorKey::NotationPackage(package.id),
    )
    .await;
    Ok(package)
}

/// Add a template to a package at its explicit assembly position.
///
/// The schema rejects duplicate template membership and duplicate positions
/// within a package.
pub async fn add_template(
    db: &SurrealDb,
    package_id: Uuid,
    template_id: Uuid,
    position: i32,
    required: bool,
) -> Result<(), NotationPackageError> {
    let id = Uuid::now_v7();
    if let Some(pg) = catalog::authority().await? {
        catalog::execute(
            sqlx::query(PG_ADD_TEMPLATE)
                .bind(id)
                .bind(package_id)
                .bind(template_id)
                .bind(position)
                .bind(required),
            &pg,
        )
        .await?;
        return Ok(());
    }
    db.query(
        "CREATE $id SET package_id = $package_id, \
         template_id = $template_id, position = $position, required = $required",
    )
    .bind(("id", record_id(MEMBER_TABLE, id)))
    .bind(("package_id", record_id(PACKAGE_TABLE, package_id)))
    .bind(("template_id", record_id(TEMPLATE_TABLE, template_id)))
    .bind(("position", position))
    .bind(("required", required))
    .await
    .and_then(surrealdb::IndexedResults::check)?;
    crate::postgres_mirror::after_write(
        db,
        crate::postgres_mirror::MirrorKey::NotationPackageTemplate(id),
    )
    .await;
    Ok(())
}

/// Find a package by its stable catalog code.
pub async fn find_by_code(
    db: &SurrealDb,
    code: &str,
) -> Result<Option<NotationPackage>, NotationPackageError> {
    if let Some(pg) = catalog::authority().await? {
        return Ok(catalog::one(&pg, PG_BY_CODE, code.to_string(), package_from_pg).await?);
    }
    let response = db
        .query(format!(
            "SELECT {PACKAGE_SELECT} FROM {PACKAGE_TABLE} WHERE code = $code LIMIT 1"
        ))
        .bind(("code", code.to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    one(response)
}

/// The templates a package assembles, in its declared order.
pub async fn templates(
    db: &SurrealDb,
    package_id: Uuid,
) -> Result<Vec<PackageTemplate>, NotationPackageError> {
    if let Some(pg) = catalog::authority().await? {
        return Ok(catalog::all(&pg, PG_TEMPLATES, Some(package_id), template_from_pg).await?);
    }
    let mut response = db
        .query(format!(
            "SELECT {TEMPLATE_SELECT} FROM {MEMBER_TABLE} WHERE package_id = $package_id \
             ORDER BY position ASC"
        ))
        .bind(("package_id", record_id(PACKAGE_TABLE, package_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<PackageTemplateRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(PackageTemplateRow::into_model)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{add_template, create, find_by_code, templates, NewNotationPackage};
    use crate::surreal::test_support::mem;
    use uuid::Uuid;

    #[tokio::test]
    async fn a_package_keeps_its_starting_price_and_template_assembly_order() {
        let db = mem().await;
        let package = create(
            &db,
            &NewNotationPackage::new("estate-package", "Estate package", "complex", 10_000),
        )
        .await
        .unwrap();
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        add_template(&db, package.id, second, 1, false)
            .await
            .unwrap();
        add_template(&db, package.id, first, 0, true).await.unwrap();

        let found = find_by_code(&db, "estate-package").await.unwrap().unwrap();
        assert_eq!(found.starting_price_cents, 10_000);
        assert_eq!(found.currency, "USD");
        assert_eq!(
            templates(&db, package.id)
                .await
                .unwrap()
                .into_iter()
                .map(|member| (member.template_id, member.required))
                .collect::<Vec<_>>(),
            vec![(first, true), (second, false)]
        );
    }
}

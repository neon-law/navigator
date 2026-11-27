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
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::catalog;

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

/// One template in a package, in assembly order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackageTemplate {
    pub package_id: Uuid,
    pub template_id: Uuid,
    pub position: i32,
    pub required: bool,
}

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
    #[error("notation package writes require PostgreSQL authority")]
    AuthorityUnavailable,
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

/// Insert a package catalog entry. Package templates are attached separately
/// with [`add_template`] so their order is explicit.
pub async fn create(
    _db: &ApplicationStore,
    new: &NewNotationPackage,
) -> Result<NotationPackage, NotationPackageError> {
    let pg = catalog::authority().await?;
    let row = catalog::row(
        sqlx::query(PG_INSERT)
            .bind(Uuid::now_v7())
            .bind(new.code.clone())
            .bind(new.title.clone())
            .bind(new.description.clone())
            .bind(new.complexity.clone())
            .bind(new.starting_price_cents)
            .bind(new.currency.clone()),
        &pg,
    )
    .await?;
    Ok(package_from_pg(&row)?)
}

/// Add a template to a package at its explicit assembly position.
///
/// The schema rejects duplicate template membership and duplicate positions
/// within a package.
pub async fn add_template(
    _db: &ApplicationStore,
    package_id: Uuid,
    template_id: Uuid,
    position: i32,
    required: bool,
) -> Result<(), NotationPackageError> {
    let pg = catalog::authority().await?;
    catalog::execute(
        sqlx::query(PG_ADD_TEMPLATE)
            .bind(Uuid::now_v7())
            .bind(package_id)
            .bind(template_id)
            .bind(position)
            .bind(required),
        &pg,
    )
    .await?;
    Ok(())
}

/// Find a package by its stable catalog code.
pub async fn find_by_code(
    _db: &ApplicationStore,
    code: &str,
) -> Result<Option<NotationPackage>, NotationPackageError> {
    let pg = catalog::authority().await?;
    Ok(catalog::one(&pg, PG_BY_CODE, code.to_string(), package_from_pg).await?)
}

/// The templates a package assembles, in its declared order.
pub async fn templates(
    _db: &ApplicationStore,
    package_id: Uuid,
) -> Result<Vec<PackageTemplate>, NotationPackageError> {
    let pg = catalog::authority().await?;
    Ok(catalog::all(&pg, PG_TEMPLATES, Some(package_id), template_from_pg).await?)
}

#[cfg(test)]
mod tests {
    use super::{add_template, create, find_by_code, templates, NewNotationPackage};
    use crate::application_store::ApplicationStore;
    use crate::test_support;
    use uuid::Uuid;

    #[tokio::test]
    async fn a_package_keeps_its_starting_price_and_template_assembly_order() {
        test_support::with_required_postgres(async {
        let db = ApplicationStore::uninitialized();
        let package = create(
            &db,
            &NewNotationPackage::new("estate-package", "Estate package", "complex", 10_000),
        )
        .await
        .unwrap();
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        let pg = crate::postgres::PostgresDb::application().await.unwrap();
        for (id, code) in [(first, "synthetic-first"), (second, "synthetic-second")] {
            sqlx::query("INSERT INTO template (id, code, title, respondent_type, is_current, inserted_at, updated_at) VALUES ($1, $2, 'Synthetic', 'person', false, now(), now())")
                .bind(id).bind(code).execute(pg.pool()).await.unwrap();
        }
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
            }).await;
    }
}

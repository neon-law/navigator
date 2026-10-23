//! Authority, Glossary Term, Question, Notation package, package template,
//! and Notation rate rows in the PostgreSQL mirror.
//!
//! These are shared catalogs: owned by no Firm or Project. The contract is
//! the identity mirror's. SurrealDB is read back after its write, and
//! PostgreSQL's copy is upserted or deleted to match under the same record
//! UUID. Columns carry the Surreal field names, so one column list reads
//! the source and writes the target.
//!
//! A package template names its package and exact Template version. Mirroring
//! one first mirrors either parent when PostgreSQL does not hold it. An Authority's archived Asset
//! is not mirrored here: an Authority naming an Asset PostgreSQL lacks is
//! refused with SQLSTATE 23503 and reported by [`parity`] until that Asset
//! exists there.
//!
//! A natural key (a citation, slug, or code) is immutable in SurrealDB. One
//! that PostgreSQL holds under another id is a row SurrealDB deleted and
//! re-created, so the upsert is refused with SQLSTATE 23505 and [`reconcile`]
//! converges it once the stale row is deleted.
//!
//! Notation rates have no Surreal writer. They reach PostgreSQL only through
//! [`reconcile`].
//!
//! Reports name rows by key only.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use sqlx::Row;
use surrealdb::types::{RecordId, SurrealValue};
use uuid::Uuid;

use super::tenancy::{compare, remirror, select_sql, source, upsert_sql};
use super::{changed, micros, MirrorError, MirrorKey, Mirrored, ParityReport, TableParity};
use crate::postgres::PostgresDb;
use crate::surreal::{record_id, record_uuid, SurrealDb};

type PgQuery<'q> = sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>;
type PgRow = sqlx::postgres::PgRow;

/// One catalog table: its columns, named the same in both stores, and how a
/// row moves between them.
trait Catalog: Sized + PartialEq {
    const TABLE: &'static str;
    /// `id` first, as [`upsert_sql`] requires.
    const COLUMNS: &'static [&'static str];
    type Source: SurrealValue;

    fn key(id: Uuid) -> MirrorKey;
    fn source_id(row: &Self::Source) -> &RecordId;
    /// The PostgreSQL image of a source row, at microsecond precision.
    fn from_source(row: Self::Source) -> Result<Self, MirrorError>;
    fn from_row(row: &PgRow) -> Result<Self, sqlx::Error>;
    fn id(&self) -> Uuid;
    /// Bind every column after `id`, in [`Self::COLUMNS`] order.
    fn bind<'q>(&self, query: PgQuery<'q>) -> PgQuery<'q>;
    fn upsert_statement() -> &'static str;
    fn select_statement() -> &'static str;
    fn delete_statement() -> &'static str;
}

/// The table's statements, built once from its constant column list. A
/// nested `static` cannot name `Self`, so the macro takes the type.
macro_rules! statements {
    ($image:ident) => {
        fn upsert_statement() -> &'static str {
            static STATEMENT: LazyLock<String> =
                LazyLock::new(|| upsert_sql($image::TABLE, $image::COLUMNS));
            STATEMENT.as_str()
        }

        fn select_statement() -> &'static str {
            static STATEMENT: LazyLock<String> =
                LazyLock::new(|| select_sql($image::TABLE, $image::COLUMNS));
            STATEMENT.as_str()
        }

        fn delete_statement() -> &'static str {
            static STATEMENT: LazyLock<String> =
                LazyLock::new(|| format!("DELETE FROM {} WHERE id = $1", $image::TABLE));
            STATEMENT.as_str()
        }
    };
}

fn uuid_of(record: &RecordId) -> Result<Uuid, MirrorError> {
    record_uuid(record).ok_or(MirrorError::Source)
}

#[derive(SurrealValue)]
struct AuthoritySource {
    id: RecordId,
    class: String,
    citation: String,
    short_cite: Option<String>,
    title: String,
    publisher: Option<String>,
    issued_on: Option<String>,
    canonical_url: Option<String>,
    checked_on: Option<String>,
    archived_asset_id: Option<RecordId>,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AuthorityImage {
    id: Uuid,
    class: String,
    citation: String,
    short_cite: Option<String>,
    title: String,
    publisher: Option<String>,
    issued_on: Option<String>,
    canonical_url: Option<String>,
    checked_on: Option<String>,
    archived_asset_id: Option<Uuid>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl Catalog for AuthorityImage {
    const TABLE: &'static str = "authority";
    const COLUMNS: &'static [&'static str] = &[
        "id",
        "class",
        "citation",
        "short_cite",
        "title",
        "publisher",
        "issued_on",
        "canonical_url",
        "checked_on",
        "archived_asset_id",
        "inserted_at",
        "updated_at",
    ];
    type Source = AuthoritySource;

    fn key(id: Uuid) -> MirrorKey {
        MirrorKey::Authority(id)
    }

    fn source_id(row: &AuthoritySource) -> &RecordId {
        &row.id
    }

    fn from_source(row: AuthoritySource) -> Result<Self, MirrorError> {
        Ok(Self {
            id: uuid_of(&row.id)?,
            class: row.class,
            citation: row.citation,
            short_cite: row.short_cite,
            title: row.title,
            publisher: row.publisher,
            issued_on: row.issued_on,
            canonical_url: row.canonical_url,
            checked_on: row.checked_on,
            archived_asset_id: row.archived_asset_id.as_ref().map(uuid_of).transpose()?,
            inserted_at: micros(row.inserted_at.into()),
            updated_at: micros(row.updated_at.into()),
        })
    }

    fn from_row(row: &PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            class: row.try_get("class")?,
            citation: row.try_get("citation")?,
            short_cite: row.try_get("short_cite")?,
            title: row.try_get("title")?,
            publisher: row.try_get("publisher")?,
            issued_on: row.try_get("issued_on")?,
            canonical_url: row.try_get("canonical_url")?,
            checked_on: row.try_get("checked_on")?,
            archived_asset_id: row.try_get("archived_asset_id")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }

    fn id(&self) -> Uuid {
        self.id
    }

    fn bind<'q>(&self, query: PgQuery<'q>) -> PgQuery<'q> {
        query
            .bind(self.class.clone())
            .bind(self.citation.clone())
            .bind(self.short_cite.clone())
            .bind(self.title.clone())
            .bind(self.publisher.clone())
            .bind(self.issued_on.clone())
            .bind(self.canonical_url.clone())
            .bind(self.checked_on.clone())
            .bind(self.archived_asset_id)
            .bind(self.inserted_at)
            .bind(self.updated_at)
    }

    statements!(AuthorityImage);
}

#[derive(SurrealValue)]
struct GlossaryTermSource {
    id: RecordId,
    slug: String,
    title: String,
    body: String,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GlossaryTermImage {
    id: Uuid,
    slug: String,
    title: String,
    body: String,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl Catalog for GlossaryTermImage {
    const TABLE: &'static str = "glossary_term";
    const COLUMNS: &'static [&'static str] =
        &["id", "slug", "title", "body", "inserted_at", "updated_at"];
    type Source = GlossaryTermSource;

    fn key(id: Uuid) -> MirrorKey {
        MirrorKey::GlossaryTerm(id)
    }

    fn source_id(row: &GlossaryTermSource) -> &RecordId {
        &row.id
    }

    fn from_source(row: GlossaryTermSource) -> Result<Self, MirrorError> {
        Ok(Self {
            id: uuid_of(&row.id)?,
            slug: row.slug,
            title: row.title,
            body: row.body,
            inserted_at: micros(row.inserted_at.into()),
            updated_at: micros(row.updated_at.into()),
        })
    }

    fn from_row(row: &PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            slug: row.try_get("slug")?,
            title: row.try_get("title")?,
            body: row.try_get("body")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }

    fn id(&self) -> Uuid {
        self.id
    }

    fn bind<'q>(&self, query: PgQuery<'q>) -> PgQuery<'q> {
        query
            .bind(self.slug.clone())
            .bind(self.title.clone())
            .bind(self.body.clone())
            .bind(self.inserted_at)
            .bind(self.updated_at)
    }

    statements!(GlossaryTermImage);
}

#[derive(SurrealValue)]
struct QuestionSource {
    id: RecordId,
    code: String,
    prompt: String,
    answer_type: String,
    audience: String,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct QuestionImage {
    id: Uuid,
    code: String,
    prompt: String,
    answer_type: String,
    audience: String,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl Catalog for QuestionImage {
    const TABLE: &'static str = "question";
    const COLUMNS: &'static [&'static str] = &[
        "id",
        "code",
        "prompt",
        "answer_type",
        "audience",
        "inserted_at",
        "updated_at",
    ];
    type Source = QuestionSource;

    fn key(id: Uuid) -> MirrorKey {
        MirrorKey::Question(id)
    }

    fn source_id(row: &QuestionSource) -> &RecordId {
        &row.id
    }

    fn from_source(row: QuestionSource) -> Result<Self, MirrorError> {
        Ok(Self {
            id: uuid_of(&row.id)?,
            code: row.code,
            prompt: row.prompt,
            answer_type: row.answer_type,
            audience: row.audience,
            inserted_at: micros(row.inserted_at.into()),
            updated_at: micros(row.updated_at.into()),
        })
    }

    fn from_row(row: &PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            code: row.try_get("code")?,
            prompt: row.try_get("prompt")?,
            answer_type: row.try_get("answer_type")?,
            audience: row.try_get("audience")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }

    fn id(&self) -> Uuid {
        self.id
    }

    fn bind<'q>(&self, query: PgQuery<'q>) -> PgQuery<'q> {
        query
            .bind(self.code.clone())
            .bind(self.prompt.clone())
            .bind(self.answer_type.clone())
            .bind(self.audience.clone())
            .bind(self.inserted_at)
            .bind(self.updated_at)
    }

    statements!(QuestionImage);
}

#[derive(SurrealValue)]
struct NotationPackageSource {
    id: RecordId,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct NotationPackageImage {
    id: Uuid,
    code: String,
    title: String,
    description: Option<String>,
    complexity: String,
    starting_price_cents: i64,
    currency: String,
    active: bool,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl Catalog for NotationPackageImage {
    const TABLE: &'static str = "notation_package";
    const COLUMNS: &'static [&'static str] = &[
        "id",
        "code",
        "title",
        "description",
        "complexity",
        "starting_price_cents",
        "currency",
        "active",
        "inserted_at",
        "updated_at",
    ];
    type Source = NotationPackageSource;

    fn key(id: Uuid) -> MirrorKey {
        MirrorKey::NotationPackage(id)
    }

    fn source_id(row: &NotationPackageSource) -> &RecordId {
        &row.id
    }

    fn from_source(row: NotationPackageSource) -> Result<Self, MirrorError> {
        Ok(Self {
            id: uuid_of(&row.id)?,
            code: row.code,
            title: row.title,
            description: row.description,
            complexity: row.complexity,
            starting_price_cents: row.starting_price_cents,
            currency: row.currency,
            active: row.active,
            inserted_at: micros(row.inserted_at.into()),
            updated_at: micros(row.updated_at.into()),
        })
    }

    fn from_row(row: &PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
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

    fn id(&self) -> Uuid {
        self.id
    }

    fn bind<'q>(&self, query: PgQuery<'q>) -> PgQuery<'q> {
        query
            .bind(self.code.clone())
            .bind(self.title.clone())
            .bind(self.description.clone())
            .bind(self.complexity.clone())
            .bind(self.starting_price_cents)
            .bind(self.currency.clone())
            .bind(self.active)
            .bind(self.inserted_at)
            .bind(self.updated_at)
    }

    statements!(NotationPackageImage);
}

#[derive(SurrealValue)]
struct PackageTemplateSource {
    id: RecordId,
    package_id: RecordId,
    template_id: RecordId,
    position: i64,
    required: bool,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PackageTemplateImage {
    id: Uuid,
    package_id: Uuid,
    template_id: Uuid,
    position: i32,
    required: bool,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl Catalog for PackageTemplateImage {
    const TABLE: &'static str = "notation_package_template";
    const COLUMNS: &'static [&'static str] = &[
        "id",
        "package_id",
        "template_id",
        "position",
        "required",
        "inserted_at",
        "updated_at",
    ];
    type Source = PackageTemplateSource;

    fn key(id: Uuid) -> MirrorKey {
        MirrorKey::NotationPackageTemplate(id)
    }

    fn source_id(row: &PackageTemplateSource) -> &RecordId {
        &row.id
    }

    fn from_source(row: PackageTemplateSource) -> Result<Self, MirrorError> {
        Ok(Self {
            id: uuid_of(&row.id)?,
            package_id: uuid_of(&row.package_id)?,
            template_id: uuid_of(&row.template_id)?,
            position: i32::try_from(row.position).map_err(source)?,
            required: row.required,
            inserted_at: micros(row.inserted_at.into()),
            updated_at: micros(row.updated_at.into()),
        })
    }

    fn from_row(row: &PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            package_id: row.try_get("package_id")?,
            template_id: row.try_get("template_id")?,
            position: row.try_get("position")?,
            required: row.try_get("required")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }

    fn id(&self) -> Uuid {
        self.id
    }

    fn bind<'q>(&self, query: PgQuery<'q>) -> PgQuery<'q> {
        query
            .bind(self.package_id)
            .bind(self.template_id)
            .bind(self.position)
            .bind(self.required)
            .bind(self.inserted_at)
            .bind(self.updated_at)
    }

    statements!(PackageTemplateImage);
}

#[derive(SurrealValue)]
struct NotationRateSource {
    id: RecordId,
    code: String,
    label: String,
    unit: String,
    unit_amount_cents: i64,
    currency: String,
    active: bool,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NotationRateImage {
    id: Uuid,
    code: String,
    label: String,
    unit: String,
    unit_amount_cents: i64,
    currency: String,
    active: bool,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl Catalog for NotationRateImage {
    const TABLE: &'static str = "notation_rate";
    const COLUMNS: &'static [&'static str] = &[
        "id",
        "code",
        "label",
        "unit",
        "unit_amount_cents",
        "currency",
        "active",
        "inserted_at",
        "updated_at",
    ];
    type Source = NotationRateSource;

    fn key(id: Uuid) -> MirrorKey {
        MirrorKey::NotationRate(id)
    }

    fn source_id(row: &NotationRateSource) -> &RecordId {
        &row.id
    }

    fn from_source(row: NotationRateSource) -> Result<Self, MirrorError> {
        Ok(Self {
            id: uuid_of(&row.id)?,
            code: row.code,
            label: row.label,
            unit: row.unit,
            unit_amount_cents: row.unit_amount_cents,
            currency: row.currency,
            active: row.active,
            inserted_at: micros(row.inserted_at.into()),
            updated_at: micros(row.updated_at.into()),
        })
    }

    fn from_row(row: &PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            code: row.try_get("code")?,
            label: row.try_get("label")?,
            unit: row.try_get("unit")?,
            unit_amount_cents: row.try_get("unit_amount_cents")?,
            currency: row.try_get("currency")?,
            active: row.try_get("active")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }

    fn id(&self) -> Uuid {
        self.id
    }

    fn bind<'q>(&self, query: PgQuery<'q>) -> PgQuery<'q> {
        query
            .bind(self.code.clone())
            .bind(self.label.clone())
            .bind(self.unit.clone())
            .bind(self.unit_amount_cents)
            .bind(self.currency.clone())
            .bind(self.active)
            .bind(self.inserted_at)
            .bind(self.updated_at)
    }

    statements!(NotationRateImage);
}

/// One source row by id. `Ok(None)` when SurrealDB has no such row; an
/// error when it has one PostgreSQL cannot hold as it stands.
async fn source_one<C: Catalog>(surreal: &SurrealDb, id: Uuid) -> Result<Option<C>, MirrorError> {
    let mut response = surreal
        .query(format!(
            "SELECT {} FROM ONLY $id LIMIT 1",
            C::COLUMNS.join(", ")
        ))
        .bind(("id", record_id(C::TABLE, id)))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(source)?;
    let row: Option<C::Source> = response.take(0).map_err(source)?;
    row.map(C::from_source).transpose()
}

/// Every source row keyed for comparison. A row whose image cannot be built
/// keeps its key with no image, so it is reported as divergent; a row whose
/// own key is not a UUID has no PostgreSQL identity and is left out.
async fn source_all<C: Catalog>(
    surreal: &SurrealDb,
) -> Result<BTreeMap<MirrorKey, Option<C>>, MirrorError> {
    let mut response = surreal
        .query(format!(
            "SELECT {} FROM {}",
            C::COLUMNS.join(", "),
            C::TABLE
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(source)?;
    let rows: Vec<C::Source> = response.take(0).map_err(source)?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let id = record_uuid(C::source_id(&row))?;
            Some((C::key(id), C::from_source(row).ok()))
        })
        .collect())
}

async fn target_all<C: Catalog>(pg: &PostgresDb) -> Result<BTreeMap<MirrorKey, C>, MirrorError> {
    let rows = sqlx::query(C::select_statement()).fetch_all(&pg.0).await?;
    rows.iter()
        .map(|row| C::from_row(row).map(|image| (C::key(image.id()), image)))
        .collect::<Result<_, sqlx::Error>>()
        .map_err(Into::into)
}

async fn upsert<C: Catalog>(pg: &PostgresDb, image: &C) -> Result<Mirrored, MirrorError> {
    let query = sqlx::query(C::upsert_statement()).bind(image.id());
    let written = image.bind(query).execute(&pg.0).await?;
    Ok(changed(written.rows_affected(), Mirrored::Written))
}

async fn delete<C: Catalog>(pg: &PostgresDb, id: Uuid) -> Result<Mirrored, MirrorError> {
    let deleted = sqlx::query(C::delete_statement())
        .bind(id)
        .execute(&pg.0)
        .await?;
    Ok(changed(deleted.rows_affected(), Mirrored::Deleted))
}

/// Make PostgreSQL's copy of one row match SurrealDB's.
async fn mirror_row<C: Catalog>(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    id: Uuid,
) -> Result<Mirrored, MirrorError> {
    match source_one::<C>(surreal, id).await? {
        Some(image) => upsert(pg, &image).await,
        None => delete::<C>(pg, id).await,
    }
}

/// Make PostgreSQL's copy of one catalog row match SurrealDB's.
pub(super) async fn mirror(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    key: &MirrorKey,
) -> Result<Mirrored, MirrorError> {
    match key {
        MirrorKey::Authority(id) => mirror_row::<AuthorityImage>(surreal, pg, *id).await,
        MirrorKey::GlossaryTerm(id) => mirror_row::<GlossaryTermImage>(surreal, pg, *id).await,
        MirrorKey::Question(id) => mirror_row::<QuestionImage>(surreal, pg, *id).await,
        MirrorKey::NotationPackage(id) => {
            mirror_row::<NotationPackageImage>(surreal, pg, *id).await
        }
        MirrorKey::NotationPackageTemplate(id) => {
            let Some(member) = source_one::<PackageTemplateImage>(surreal, *id).await? else {
                return delete::<PackageTemplateImage>(pg, *id).await;
            };
            let held: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM notation_package WHERE id = $1)")
                    .bind(member.package_id)
                    .fetch_one(&pg.0)
                    .await?;
            if !held {
                mirror_row::<NotationPackageImage>(surreal, pg, member.package_id).await?;
            }
            let template_held: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM template WHERE id = $1)")
                    .bind(member.template_id)
                    .fetch_one(&pg.0)
                    .await?;
            if !template_held {
                super::templates::mirror(surreal, pg, member.template_id).await?;
            }
            upsert(pg, &member).await
        }
        MirrorKey::NotationRate(id) => mirror_row::<NotationRateImage>(surreal, pg, *id).await,
        // `super::mirror` routes only catalog keys here.
        _ => Err(MirrorError::Source),
    }
}

async fn table<C: Catalog>(
    surreal: &SurrealDb,
    pg: &PostgresDb,
) -> Result<TableParity, MirrorError> {
    Ok(compare(
        C::TABLE,
        &source_all::<C>(surreal).await?,
        &target_all::<C>(pg).await?,
    ))
}

/// Compare every catalog row in both stores, a package before its
/// templates.
///
/// # Errors
///
/// Either store could not be read.
pub async fn parity(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    Ok(ParityReport {
        tables: vec![
            table::<AuthorityImage>(surreal, pg).await?,
            table::<GlossaryTermImage>(surreal, pg).await?,
            table::<QuestionImage>(surreal, pg).await?,
            table::<NotationPackageImage>(surreal, pg).await?,
            table::<PackageTemplateImage>(surreal, pg).await?,
            table::<NotationRateImage>(surreal, pg).await?,
        ],
    })
}

/// Re-mirror every divergent catalog row from SurrealDB, then compare
/// again. A row that fails both passes stays in the returned report.
///
/// # Errors
///
/// Either store could not be read for the comparison.
pub async fn reconcile(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let before = parity(surreal, pg).await?;
    remirror(surreal, pg, &before).await;
    parity(surreal, pg).await
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;

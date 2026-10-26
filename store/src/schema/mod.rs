//! The SurrealDB schema, applied rather than migrated.
//!
//! # Schema, not history
//!
//! The schema is a *statement of the present*
//! ([`navigator.surql`](https://github.com/neon-law/navigator/blob/main/store/src/schema/navigator.surql)):
//! one idempotent file describing the tables and fields that should
//! exist, applied whole by [`apply`] on every boot and by every test
//! that opens an embedded engine. The current shape is what the file
//! says, not what a chain of steps left behind.
//!
//! A describable present costs replayable history, which is why
//! [`SCHEMA_VERSION`] exists. Applying the file converges a database's
//! *definitions*. `apply` may also run narrowly-scoped, idempotent,
//! guarded backfills when convergence must materialize a write-time default
//! on rows that predate the field. `DEFINE TABLE IF NOT EXISTS` deliberately
//! leaves an existing table's rows alone. The version record is what lets a
//! process notice it is looking at a database some other version of the
//! code prepared, rather than discovering it one confusing query at a time.
//!
//! Bump [`SCHEMA_VERSION`] in the same change that edits the `.surql`
//! file.

use std::collections::BTreeMap;

use surrealdb::types::{ErrorDetails, NotFoundError, RecordId, SurrealValue};
use surrealdb::Error as SurrealQueryError;
use thiserror::Error;

use crate::surreal::SurrealDb;

/// The version this build of Navigator applies. Bump it whenever
/// `navigator.surql` changes so a database prepared by another build
/// reports as drifted instead of silently disagreeing.
pub const SCHEMA_VERSION: u32 = 96;

/// The table holding the applied version.
const VERSION_TABLE: &str = "schema_version";

/// The record holding the applied version — exactly one, always.
const VERSION_RECORD: &str = "schema_version:current";

/// The schema itself. Embedded rather than read from disk so a
/// deployed binary carries its own schema and cannot be pointed at a
/// stale copy on a volume.
const DEFINITIONS: &str = include_str!("navigator.surql");

/// The backfill may be deleted after every environment has applied a build
/// containing it, including the production rollout of release `26.9.20`.
pub const PERSON_DEFAULTS_BACKFILL_RETIRE_AFTER: &str = "26.9.20";

const PERSON_DEFAULTS_BACKFILL: &str = "\
    UPDATE person SET \
        email_confirmed = IF email_confirmed IS NONE THEN false ELSE email_confirmed END, \
        is_admitted = IF is_admitted IS NONE THEN true ELSE is_admitted END \
    WHERE (email_confirmed IS NONE OR is_admitted IS NONE);";
const PROJECT_BRAND_BACKFILL: &str = "\
    UPDATE project SET brand = 'neon' WHERE brand IS NONE;\
    DEFINE FIELD OVERWRITE brand ON project TYPE string;";
/// A Post written before `vertical_video` and `social_links` existed
/// declared no portrait video and carries no links, so it reads `false` and
/// `[]`. Surreal validates every field on any later
/// update, so the column must hold a value before the worker next touches the
/// row. A second apply matches no rows.
const BLOG_POST_VERTICAL_VIDEO_BACKFILL: &str =
    "UPDATE post SET vertical_video = false WHERE vertical_video IS NONE; \
     UPDATE post SET social_links = [] WHERE social_links IS NONE;";

/// Draft pins for rows written before `template.body_sha256` and the
/// Notation's copies existed.
///
/// A template version takes the digest its body asset row recorded at
/// ingest; one whose asset is missing, or whose recorded digest is not
/// lowercase SHA-256 hex, keeps NONE and is still checked against the asset
/// row on every read. A Notation then copies its pinned version's digest
/// and commit, which are what it was opened against because a version row
/// is never rewritten. No commit sha is invented: a version synced without
/// one keeps NONE, and so does every Notation opened from it. Guarded on
/// NONE, so a second apply matches no rows.
const DRAFT_PIN_BACKFILL: &str = "\
    UPDATE template SET body_sha256 = asset_id.sha256_hex \
    WHERE body_sha256 IS NONE AND asset_id IS NOT NONE \
        AND type::is_string(asset_id.sha256_hex) \
        AND string::len(asset_id.sha256_hex) = 64 \
        AND string::is_hexadecimal(asset_id.sha256_hex) \
        AND asset_id.sha256_hex = string::lowercase(asset_id.sha256_hex); \
    UPDATE notation SET template_body_sha256 = template_id.body_sha256 \
    WHERE template_body_sha256 IS NONE AND template_id.body_sha256 IS NOT NONE; \
    UPDATE notation SET template_source_commit_sha = template_id.source_commit_sha \
    WHERE template_source_commit_sha IS NONE AND template_id.source_commit_sha IS NOT NONE;";

const SUMMARY_APPROVAL_PROJECT_BACKFILL: &str = "\
    UPDATE summary_sender_approval SET status = 'revoked', updated_at = time::now() \
    WHERE project_id IS NONE AND status = 'approved';";

#[derive(surrealdb::types::SurrealValue)]
struct LegacyInboxMessage {
    id: surrealdb::types::RecordId,
    conversation_id: surrealdb::types::RecordId,
    direction: String,
    body: String,
    author_person_id: Option<surrealdb::types::RecordId>,
    provider_message_id: Option<String>,
    provider_ref: Option<String>,
    delivery_state: String,
    purpose: String,
    attachment_asset_id: Option<surrealdb::types::RecordId>,
    communication_id: Option<surrealdb::types::RecordId>,
    occurred_at: String,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

/// Every table declared in the shipped Surreal schema, in stable order.
///
/// Consumers that must cover the whole operational store (rather than a
/// hand-maintained subset) use this list. The schema remains the sole source
/// of truth: adding a `DEFINE TABLE` makes it visible to those consumers in
/// the same build.
///
/// It includes the derived tables ([`derived_tables`]); a consumer that
/// copies *data* rather than counting or verifying rows wants
/// [`base_table_names`], which leaves each derived table to be rebuilt from
/// its source.
#[must_use]
pub fn table_names() -> Vec<String> {
    table_names_in(DEFINITIONS)
}

/// The tables `definitions` declares, in stable order. The name is the
/// first token after `DEFINE TABLE` once the optional `IF NOT EXISTS` or
/// `OVERWRITE` clause is skipped — never the clause itself.
fn table_names_in(definitions: &str) -> Vec<String> {
    let mut tables: Vec<String> = definitions
        .lines()
        .filter_map(|line| line.trim().strip_prefix("DEFINE TABLE "))
        .filter_map(|line| {
            let mut words = line.split_whitespace().peekable();
            match words.peek().copied() {
                Some("IF") => words.nth(3),
                Some("OVERWRITE") => words.nth(1),
                _ => words.next(),
            }
        })
        .map(ToOwned::to_owned)
        .collect();
    tables.sort_unstable();
    tables.dedup();
    tables
}

/// A table the schema computes from another table with `AS SELECT`.
///
/// The engine fills it from `source`, keeps it current as `source` changes,
/// and refuses every write to it. That makes it *derived*: its rows are not
/// an independent entity to snapshot, and an export must not carry them (see
/// [`strip_derived_tables`]), because import writes rows rather than
/// computing them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedTable {
    /// The view's own name, e.g. `draft`.
    pub name: String,
    /// The table it selects from, e.g. `template`.
    pub source: String,
    /// The `SELECT …` the view is defined as, whitespace-normalised — what
    /// the live definition must contain for the view to be converged.
    select: String,
    /// The full `DEFINE TABLE …` statement, run to (re)create the view.
    statement: String,
}

/// Every derived table in the shipped schema, in stable order.
#[must_use]
pub fn derived_tables() -> Vec<DerivedTable> {
    derived_tables_in(DEFINITIONS)
}

fn normalise_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A derived table is one `DEFINE TABLE … AS SELECT … FROM <source> …;`
/// statement on a single line — the shape the schema file keeps them in, so
/// a line scan finds them without parsing SurrealQL.
fn derived_tables_in(definitions: &str) -> Vec<DerivedTable> {
    let mut views: Vec<DerivedTable> = definitions
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("DEFINE TABLE ") && line.contains(" AS SELECT "))
        .filter_map(|line| {
            let statement = line.strip_suffix(';').unwrap_or(line);
            let name = table_names_in(line).into_iter().next()?;
            let (_, after_as) = statement.split_once(" AS ")?;
            let select = after_as
                .split_once(" PERMISSIONS ")
                .map_or(after_as, |(select, _)| select);
            let source = select
                .split_once(" FROM ")?
                .1
                .split_whitespace()
                .next()?
                .to_string();
            Some(DerivedTable {
                name,
                source,
                select: normalise_whitespace(select),
                statement: statement.to_string(),
            })
        })
        .collect();
    views.sort_by(|a, b| a.name.cmp(&b.name));
    views
}

/// Every table that holds its own rows: [`table_names`] without the derived
/// ones. What an analytical snapshot copies — a derived table's rows are the
/// same records as its source's, so exporting both would count one entity
/// twice.
#[must_use]
pub fn base_table_names() -> Vec<String> {
    let derived: Vec<String> = derived_tables().into_iter().map(|view| view.name).collect();
    table_names()
        .into_iter()
        .filter(|name| !derived.contains(name))
        .collect()
}

/// An operational `SurrealQL` export with every derived table's definition
/// and rows removed.
///
/// Why: `export` lists tables alphabetically, so a view appears *before* the
/// table it selects from, and importing that file defines the view first,
/// fails ("the table does not exist"), and then writes the view's rows into
/// a plain, writable table of the same name. A derived table is computed, so
/// the export leaves it out and [`apply`] rebuilds it from the restored
/// source. Dropping an export's sections is safe to repeat: an export
/// already stripped passes through unchanged.
#[must_use]
pub fn strip_derived_tables(export: &str) -> String {
    let derived: Vec<String> = derived_tables().into_iter().map(|view| view.name).collect();
    let mut kept = String::with_capacity(export.len());
    let mut skipping = false;
    for line in export.split_inclusive('\n') {
        // A section is introduced by a banner: a row of dashes, the header,
        // and another row of dashes. The dashes carry no name, so they leave
        // the current section as it was.
        let header = line
            .trim_end()
            .strip_prefix("-- ")
            .filter(|header| !header.starts_with('-'));
        if let Some(header) = header {
            // `-- TABLE: draft` and `-- TABLE DATA: draft` open a derived
            // table's sections; any other section header ends them.
            skipping = header
                .strip_prefix("TABLE DATA: ")
                .or_else(|| header.strip_prefix("TABLE: "))
                .is_some_and(|name| derived.iter().any(|view| view == name));
        }
        if !skipping {
            kept.push_str(line);
        }
    }
    kept
}

/// Every field a table declares in the shipped schema, as
/// `(name, Surreal type)` pairs.
///
/// Ordered the implicit `id` primary key first — every Surreal record
/// has one, and it never appears in a `DEFINE FIELD` — then the
/// declared fields alphabetically. Reading [`DEFINITIONS`] rather than introspecting a
/// live engine keeps this pure, so a renderer or a drift test can call
/// it without a database.
///
/// An unknown table yields an empty vector rather than an error: the
/// caller asking "is this a table?" is [`table_names`].
#[must_use]
pub fn table_columns(table: &str) -> Vec<(String, String)> {
    let mut fields: Vec<(String, String)> = Vec::new();
    // Comment prose can contain semicolons. Remove complete `--` comment
    // lines before splitting statements so documentation punctuation cannot
    // hide a real field declaration from schema inventory consumers.
    let definitions = DEFINITIONS
        .lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");
    for statement in definitions.split(';') {
        let Some((name, on, ty)) = parse_define_field(statement) else {
            continue;
        };
        if on == table {
            fields.push((name, ty));
        }
    }
    if fields.is_empty() {
        return Vec::new();
    }
    // Surreal relation endpoints are implicit columns rather than DEFINE
    // FIELD statements. `relationship` is mirrored by endpoint name and
    // stores each typed record id as JSONB so its table tag survives.
    if table == "relationship" {
        fields.push(("in".to_string(), "record<person|entity>".to_string()));
        fields.push(("out".to_string(), "record<person|entity>".to_string()));
    }
    fields.sort_by(|a, b| a.0.cmp(&b.0));
    fields.dedup_by(|a, b| a.0 == b.0);
    let mut columns = vec![("id".to_string(), "record".to_string())];
    columns.append(&mut fields);
    columns
}

/// Split one `DEFINE FIELD` statement into `(field, table, type)`.
///
/// The statement may wrap across lines and may carry `OVERWRITE` or
/// `IF NOT EXISTS` before the field name, so this works on whitespace
/// tokens rather than on the line. The type is the single token after
/// `TYPE` — Surreal spells a composite type without spaces
/// (`option<record<person>>`), so the clauses that may follow
/// (`DEFAULT`, `ASSERT`, `READONLY`, …) are simply the tokens after it.
fn parse_define_field(statement: &str) -> Option<(String, String, String)> {
    let mut words = statement.split_whitespace();
    if words.next()? != "DEFINE" || words.next()? != "FIELD" {
        return None;
    }
    let mut name = words.next()?;
    if name == "OVERWRITE" {
        name = words.next()?;
    } else if name == "IF" {
        // `IF NOT EXISTS <name>`
        words.next()?;
        words.next()?;
        name = words.next()?;
    }
    if words.next()? != "ON" {
        return None;
    }
    let mut on = words.next()?;
    if on == "TABLE" {
        on = words.next()?;
    }
    if words.next()? != "TYPE" {
        return None;
    }
    Some((name.to_string(), on.to_string(), words.next()?.to_string()))
}

/// How a database's applied schema compares to this build's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaState {
    /// No schema has been applied — a fresh database.
    Absent,
    /// The applied version is this build's.
    InSync,
    /// Some other build prepared this database. `installed` may be
    /// older (this build is ahead) or newer (this build is behind);
    /// both are the same fact to the caller — the code and the
    /// database disagree.
    Drifted { installed: u32, expected: u32 },
}

/// One table as the engine describes it: its `DEFINE TABLE` statement
/// and its fields' `DEFINE FIELD` statements, keyed by field name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableDefinition {
    pub definition: String,
    pub fields: BTreeMap<String, String>,
}

/// Every table in the database, keyed by name. Ordered, so a consumer
/// that renders it is deterministic by construction.
pub type Introspection = BTreeMap<String, TableDefinition>;

#[derive(Debug, Error)]
pub enum SchemaError {
    #[error("apply the Surreal schema definitions")]
    Apply(#[source] SurrealQueryError),
    #[error("legacy inbox message id is not a native UUID record id")]
    LegacyInboxMessageId,
    #[error("legacy blog_post id is not a native UUID record id")]
    LegacyBlogPostId,
    /// The Xero invoice historical re-key cannot safely choose between two
    /// rows that claim the same Xero invoice id. The schema's preflight
    /// detects it before changing either row.
    #[error("xero invoice re-key collision: {0}")]
    XeroInvoiceRekeyCollision(String),
    #[error("project brand backfill missing default brand: {0}")]
    ProjectBrandBackfillMissingDefault(String),
    #[error("project brand backfill dangling brand: {0}")]
    ProjectBrandBackfillDangling(String),
    /// [`backfill_brand_firm_id`] found a `brand` row with `firm_id IS NONE`
    /// that no `firm_brand` row wears and no `project.brand` names — this
    /// should be a compiled house brand's row with a `firm_brand` match, or
    /// an orphan the backfill deletes on its own, so this only ever fires
    /// when the anchor Firm itself cannot be resolved to receive a
    /// project-referenced orphan.
    #[error("brand firm_id backfill: {0}")]
    BrandFirmIdBackfillFirmLookup(String),
    /// [`backfill_unowned_project_firms`] could not read the anchor Firm or
    /// write the guarded Project update.
    #[error("project firm_id backfill: {0}")]
    ProjectFirmIdBackfill(String),
    /// A matter recorded under a prior schema version already holds a code
    /// [`cloud::workspace::RESERVED_PROJECT_CODES`] has since reserved. The
    /// `ASSERT` on `project.code` only guards future writes — SurrealDB does
    /// not retroactively validate stored data — so a historical row like this
    /// would otherwise keep an immutable code that now collides with a
    /// literal Navigator route, with nothing to say so. There is no
    /// automatic remediation: a Project code is immutable and meaningful, so
    /// renaming it is a deliberate, one-off human decision, not a retry.
    #[error(
        "Project code {0:?} is reserved but already recorded on an existing matter — apply \
         refuses to converge the schema until that matter's code is deliberately resolved"
    )]
    ReservedProjectCode(String),
    /// [`backfill_firm_slugs`] found a Firm whose name derives an invalid or
    /// reserved slug, or two Firms that would share one. There is no
    /// automatic remediation: a slug is immutable once written, so choosing
    /// it is a deliberate, one-off decision (rename the Firm, then boot), not
    /// a suffix the backfill invents.
    #[error("firm slug backfill refused: {0}; apply refuses to converge the schema until it is resolved")]
    FirmSlugBackfill(#[from] crate::firms::slug::BackfillRefusal),
    #[error("record the applied schema version")]
    RecordVersion(#[source] SurrealQueryError),
    #[error("read the applied schema version")]
    ReadVersion(#[source] SurrealQueryError),
    #[error("the applied schema version is {0}, which is not a version number")]
    UnreadableVersion(i64),
    #[error("read the applied schema back from the engine")]
    Introspect(#[source] SurrealQueryError),
}

const XERO_INVOICE_REKEY_COLLISION_PREFIX: &str =
    "An error occurred: xero invoice re-key collision: ";

fn classify_apply(error: SurrealQueryError) -> SchemaError {
    let message = error.to_string();
    if let Some(invoice_id) = message.strip_prefix(XERO_INVOICE_REKEY_COLLISION_PREFIX) {
        SchemaError::XeroInvoiceRekeyCollision(invoice_id.to_string())
    } else {
        SchemaError::Apply(error)
    }
}

async fn live_brand_key(db: &SurrealDb, brand_key: &str) -> Result<bool, SchemaError> {
    let mut response = db
        .query("SELECT VALUE id FROM brand WHERE brand_key = $brand_key LIMIT 1")
        .bind(("brand_key", brand_key.to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(SchemaError::Apply)?;
    let rows: Vec<RecordId> = response.take(0).map_err(SchemaError::Apply)?;
    Ok(!rows.is_empty())
}

/// Refuse to converge the schema while an existing matter holds a code that
/// [`cloud::workspace::RESERVED_PROJECT_CODES`] reserves. A preflight, not a
/// backfill: unlike the brand backfill below, there is nothing safe to write
/// on the matter's behalf, so a collision is a hard failure rather than a
/// row this apply corrects.
async fn guard_no_reserved_project_codes(db: &SurrealDb) -> Result<(), SchemaError> {
    let mut response = db
        .query("SELECT VALUE code FROM project")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(SchemaError::Apply)?;
    let codes: Vec<String> = response.take(0).map_err(SchemaError::Apply)?;
    for code in codes {
        if cloud::workspace::RESERVED_PROJECT_CODES.contains(&code.as_str()) {
            return Err(SchemaError::ReservedProjectCode(code));
        }
    }
    Ok(())
}

/// Give every Firm without a slug the one its name derives, then make the
/// field required and immutable.
///
/// Refuses the whole batch, before writing anything, when a derived slug is
/// invalid or reserved, or when two Firms (one possibly already slugged)
/// would share it. Guarded on `slug IS NONE`, so a Firm that already holds a
/// slug keeps it and a second boot is a no-op.
async fn backfill_firm_slugs(db: &SurrealDb) -> Result<(), SchemaError> {
    #[derive(SurrealValue)]
    struct FirmSlugRow {
        id: RecordId,
        name: String,
        slug: Option<String>,
    }
    let mut response = db
        .query("SELECT id, name, slug FROM firm")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(SchemaError::Apply)?;
    let rows: Vec<FirmSlugRow> = response.take(0).map_err(SchemaError::Apply)?;
    let rows = rows
        .into_iter()
        .filter_map(|row| {
            Some(crate::firms::slug::BackfillRow {
                id: crate::surreal::record_uuid(&row.id)?,
                name: row.name,
                slug: row.slug,
            })
        })
        .collect::<Vec<_>>();
    for (id, slug) in crate::firms::slug::plan_backfill(&rows)? {
        db.query("UPDATE $id SET slug = $slug WHERE slug IS NONE")
            .bind(("id", crate::surreal::record_id(crate::firms::TABLE, id)))
            .bind(("slug", slug))
            .await
            .and_then(surrealdb::IndexedResults::check)
            .map_err(SchemaError::Apply)?;
    }
    db.query("DEFINE FIELD OVERWRITE slug ON firm TYPE string READONLY;")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(SchemaError::Apply)?;
    Ok(())
}

async fn backfill_project_brand(db: &SurrealDb) -> Result<(), SchemaError> {
    let mut response = db
        .query("SELECT VALUE id FROM project WHERE brand IS NONE")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(SchemaError::Apply)?;
    let missing: Vec<RecordId> = response.take(0).map_err(SchemaError::Apply)?;

    let mut response = db
        .query("SELECT VALUE brand FROM project WHERE brand IS NOT NONE")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(SchemaError::Apply)?;
    let existing: Vec<String> = response.take(0).map_err(SchemaError::Apply)?;
    for brand_key in existing {
        if !live_brand_key(db, &brand_key).await? {
            return Err(SchemaError::ProjectBrandBackfillDangling(brand_key));
        }
    }
    if !missing.is_empty() && !live_brand_key(db, "neon").await? {
        return Err(SchemaError::ProjectBrandBackfillMissingDefault(
            "neon".to_string(),
        ));
    }
    db.query(PROJECT_BRAND_BACKFILL)
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(SchemaError::Apply)?;
    Ok(())
}

/// Materialize the historical defaults for the two required person flags.
///
/// Typed reads already tolerate a missing value: `PersonRow.email_confirmed`
/// is `Option<bool>` and collapses to `false`, and `is_admitted` reads
/// `Option<bool>` then `.unwrap_or(true)`. A full-record `UPDATE` does not.
/// Surreal validates every field of the row on any update, including fields
/// the query never mentions, so `link_oidc_subject` fails coercion on a
/// historical row until both flags exist. `is_admitted` is in this statement
/// for that validation, not because its reader was broken. Existing values
/// are preserved, and a second apply is a no-op.
async fn backfill_person_defaults(db: &SurrealDb) -> Result<(), SchemaError> {
    let missing_rows = person_defaults_missing_rows(db).await?;
    if missing_rows == 0 {
        tracing::debug!(missing_rows, "person defaults backfill already converged");
        return Ok(());
    }

    db.query(PERSON_DEFAULTS_BACKFILL)
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(SchemaError::Apply)?;
    Ok(())
}

/// A `brand` row's id and key, when [`backfill_brand_firm_id`] needs to
/// resolve a stray `firm_id IS NONE` row onto its owning Firm.
#[derive(SurrealValue)]
struct OrphanBrand {
    id: RecordId,
    brand_key: String,
}

/// Backfill every historical `brand.firm_id IS NONE` row onto the Firm that
/// wears its key through `firm_brand` (ENG-659) — at most one Firm, since
/// `firm_brand_key` is itself a UNIQUE index over `brand_key` alone, so two
/// Firms can never both wear one key and there is no collision left to
/// resolve with a synthetic key suffix. A row no Firm wears is deleted, the
/// same "delete an unworn system-wide row" the migration is allowed to take
/// — unless a `project.brand` still names it, in which case it is assigned
/// to the anchor Firm instead of deleted (mirroring ENG-677's own precedent
/// for an orphaned brand reference), so a live matter never points at a
/// deleted row.
///
/// Tightens `brand.firm_id` to a required `record<firm>` only once every row
/// is guaranteed to carry one. `navigator.surql` itself keeps the looser
/// `option<record<firm>>` definition, so a fresh apply never races the
/// tightening against this backfill — the same split
/// [`backfill_project_brand`] already established for `project.brand`.
async fn backfill_brand_firm_id(db: &SurrealDb) -> Result<(), SchemaError> {
    let mut response = db
        .query("SELECT id, brand_key FROM brand WHERE firm_id IS NONE")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(SchemaError::Apply)?;
    let orphans: Vec<OrphanBrand> = response.take(0).map_err(SchemaError::Apply)?;

    for orphan in orphans {
        let mut response = db
            .query("SELECT VALUE firm_id FROM firm_brand WHERE brand_key = $key LIMIT 1")
            .bind(("key", orphan.brand_key.clone()))
            .await
            .and_then(surrealdb::IndexedResults::check)
            .map_err(SchemaError::Apply)?;
        let wearer: Vec<RecordId> = response.take(0).map_err(SchemaError::Apply)?;

        if let Some(firm_id) = wearer.into_iter().next() {
            db.query("UPDATE $id SET firm_id = $firm_id WHERE firm_id IS NONE")
                .bind(("id", orphan.id.clone()))
                .bind(("firm_id", firm_id))
                .await
                .and_then(surrealdb::IndexedResults::check)
                .map_err(SchemaError::Apply)?;
            continue;
        }

        let mut response = db
            .query("SELECT VALUE id FROM project WHERE brand = $key LIMIT 1")
            .bind(("key", orphan.brand_key.clone()))
            .await
            .and_then(surrealdb::IndexedResults::check)
            .map_err(SchemaError::Apply)?;
        let referenced: Vec<RecordId> = response.take(0).map_err(SchemaError::Apply)?;

        if referenced.is_empty() {
            db.query("DELETE $id")
                .bind(("id", orphan.id.clone()))
                .await
                .and_then(surrealdb::IndexedResults::check)
                .map_err(SchemaError::Apply)?;
            continue;
        }

        let anchor = crate::firms::anchor_firm(db)
            .await
            .map_err(|error| SchemaError::BrandFirmIdBackfillFirmLookup(error.to_string()))?
            .ok_or_else(|| {
                SchemaError::BrandFirmIdBackfillFirmLookup(format!(
                    "no anchor Firm exists to receive orphaned brand {}",
                    orphan.brand_key
                ))
            })?;
        db.query("UPDATE $id SET firm_id = $firm_id WHERE firm_id IS NONE")
            .bind(("id", orphan.id.clone()))
            .bind(("firm_id", crate::surreal::record_id("firm", anchor.id)))
            .await
            .and_then(surrealdb::IndexedResults::check)
            .map_err(SchemaError::Apply)?;
    }

    db.query("DEFINE FIELD OVERWRITE firm_id ON brand TYPE record<firm>;")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(SchemaError::Apply)?;
    Ok(())
}

/// The schema version that added `brand.website_url`.
const BRAND_WEBSITE_URL_VERSION: u32 = 88;

/// Give each existing compiled-brand row the website URL its compiled
/// registry entry names (`store::seed::compiled_brand_website_url`).
///
/// Runs once, on the apply that upgrades a database from before
/// [`BRAND_WEBSITE_URL_VERSION`]. A later boot never runs it, so a URL the
/// Owner clears stays cleared and the approved domains never grow back on
/// their own. Runtime-created Brands have no verified source and keep none.
/// Guarded on `website_url IS NONE`, so it never overwrites a value.
async fn backfill_compiled_brand_website_urls(db: &SurrealDb) -> Result<(), SchemaError> {
    for key in crate::firms::CLOSED_BRAND_KEYS {
        let Some(url) = crate::seed::compiled_brand_website_url(key) else {
            continue;
        };
        db.query(
            "UPDATE brand SET website_url = $url WHERE brand_key = $key AND website_url IS NONE",
        )
        .bind(("key", (*key).to_string()))
        .bind(("url", url.to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(SchemaError::Apply)?;
    }
    Ok(())
}

/// Point every Project that still has no owner at the anchor Firm.
///
/// Guarded on `firm_id IS NONE`, so a Project that already names a Firm is
/// left alone and a second boot is a no-op. When the deployment has no
/// anchor Firm yet, the column stays empty: this never creates a Firm.
async fn backfill_unowned_project_firms(db: &SurrealDb) -> Result<(), SchemaError> {
    let anchor = crate::firms::anchor_firm(db)
        .await
        .map_err(|error| SchemaError::ProjectFirmIdBackfill(error.to_string()))?;
    crate::firms::backfill_unowned_projects(db, anchor.map(|firm| firm.id))
        .await
        .map_err(|error| SchemaError::ProjectFirmIdBackfill(error.to_string()))?;
    Ok(())
}

async fn person_defaults_missing_rows(db: &SurrealDb) -> Result<usize, SchemaError> {
    let mut response = db
        .query("SELECT VALUE id FROM person WHERE (email_confirmed IS NONE OR is_admitted IS NONE)")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(SchemaError::Apply)?;
    let missing: Vec<RecordId> = response.take(0).map_err(SchemaError::Apply)?;
    Ok(missing.len())
}

/// Make every derived table in the shipped schema the view the schema
/// declares.
///
/// `DEFINE TABLE IF NOT EXISTS` is what makes re-applying a populated view
/// safe (`OVERWRITE` fails on one), but it also skips a table that exists
/// under *any* definition. Two ways that leaves a derived table wrong:
///
/// - the shipped `SELECT` changed, and a database prepared by an older build
///   keeps computing the old one;
/// - the table is a plain, writable table — what importing an export that
///   still carried the view leaves behind (see [`strip_derived_tables`]).
///
/// So the live definition is compared with the shipped one, and a table that
/// differs is removed and defined again. That is safe because nothing writes
/// a derived table; its rows are recomputed from the source on definition.
async fn converge_derived_tables(db: &SurrealDb) -> Result<(), SchemaError> {
    let views = derived_tables();
    if views.is_empty() {
        return Ok(());
    }
    let mut response = db
        .query("SELECT VALUE tables FROM (INFO FOR DB)")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(SchemaError::Apply)?;
    let live: Option<surrealdb::types::Value> = response.take(0).map_err(SchemaError::Apply)?;
    let live = live.map(surrealdb::types::Value::into_json_value);
    for view in views {
        let definition = live
            .as_ref()
            .and_then(|tables| tables.get(&view.name))
            .and_then(serde_json::Value::as_str)
            .map(normalise_whitespace);
        let converged = definition
            .as_deref()
            .is_some_and(|definition| definition.contains(&view.select));
        if converged {
            continue;
        }
        db.query(format!(
            "REMOVE TABLE IF EXISTS {}; {};",
            view.name, view.statement
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(SchemaError::Apply)?;
    }
    Ok(())
}

/// Apply the schema, run the guarded row backfills, and record [`SCHEMA_VERSION`].
///
/// Idempotent: running it against an already-prepared database converges
/// every definition. The backfills fill only a missing value, so a second
/// boot leaves an already-owned Project alone. Callers run it at boot
/// without checking [`state`] first.
pub async fn apply(db: &SurrealDb) -> Result<(), SchemaError> {
    let installed = installed_version(db).await?;
    migrate_inbox_message_table(db).await?;
    move_blog_post_rows_to_post(db).await?;
    db.query(DEFINITIONS)
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(classify_apply)?;

    converge_derived_tables(db).await?;
    finish_blog_post_move(db).await?;

    // Checked right after `DEFINE TABLE IF NOT EXISTS project` has run (so a
    // never-touched table reads as empty rather than "does not exist") and
    // before anything treats this apply as having converged: a reserved-code
    // collision is a fact about existing data the new `ASSERT` cannot see
    // retroactively, so it must fail the boot rather than pass silently.
    guard_no_reserved_project_codes(db).await?;

    // Before anything reads a Firm: `store::firms` reads `slug` as required.
    backfill_firm_slugs(db).await?;
    backfill_project_brand(db).await?;
    backfill_person_defaults(db).await?;
    backfill_brand_firm_id(db).await?;
    backfill_unowned_project_firms(db).await?;
    if installed.is_some_and(|version| version < BRAND_WEBSITE_URL_VERSION) {
        backfill_compiled_brand_website_urls(db).await?;
    }
    db.query(BLOG_POST_VERTICAL_VIDEO_BACKFILL)
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(SchemaError::Apply)?;
    db.query(SUMMARY_APPROVAL_PROJECT_BACKFILL)
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(SchemaError::Apply)?;
    db.query(DRAFT_PIN_BACKFILL)
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(SchemaError::Apply)?;

    db.query(format!(
        "UPSERT {VERSION_RECORD} SET version = $version, applied_at = time::now()"
    ))
    .bind(("version", i64::from(SCHEMA_VERSION)))
    .await
    .and_then(surrealdb::IndexedResults::check)
    .map_err(SchemaError::RecordVersion)?;

    Ok(())
}

/// The table was `blog_post` before it took its glossary name, `post`. A
/// database that still holds it has its rows copied, keeping each record key,
/// and its `social_publication` rows pointed at the new records, before the
/// definitions run: the unique `(post_id, provider)` index is built over rows
/// that already carry a `post_id`. A database without `blog_post` is untouched.
///
/// Idempotent and resumable: the copy is an upsert, and [`finish_blog_post_move`]
/// removes the old table only once every publication has moved.
async fn move_blog_post_rows_to_post(db: &SurrealDb) -> Result<(), SchemaError> {
    if !table_exists(db, "blog_post").await? {
        return Ok(());
    }
    let old_ids: Vec<RecordId> = db
        .query("SELECT VALUE id FROM blog_post")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(SchemaError::Apply)?
        .take(0)
        .map_err(SchemaError::Apply)?;
    db.query("REMOVE INDEX IF EXISTS social_publication_post_provider ON social_publication")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(SchemaError::Apply)?;
    db.query("DEFINE FIELD OVERWRITE post_id ON social_publication TYPE option<record<post>>")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(SchemaError::Apply)?;
    for old in old_ids {
        let key = crate::surreal::record_uuid(&old).ok_or(SchemaError::LegacyBlogPostId)?;
        db.query(
            "UPSERT $new CONTENT (SELECT * OMIT id FROM ONLY $old); \
             UPDATE social_publication SET post_id = $new WHERE blog_post_id = $old;",
        )
        .bind(("old", old))
        .bind(("new", crate::surreal::record_id("post", key)))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(SchemaError::Apply)?;
    }
    Ok(())
}

/// The second half of the move off `blog_post`: once the definitions have
/// replaced the publication field and index, the old field and table go.
async fn finish_blog_post_move(db: &SurrealDb) -> Result<(), SchemaError> {
    if !table_exists(db, "blog_post").await? {
        return Ok(());
    }
    db.query(
        "REMOVE FIELD IF EXISTS blog_post_id ON social_publication; \
         UPDATE social_publication UNSET blog_post_id; \
         REMOVE TABLE blog_post;",
    )
    .await
    .and_then(surrealdb::IndexedResults::check)
    .map_err(SchemaError::Apply)?;
    Ok(())
}

async fn table_exists(db: &SurrealDb, table: &str) -> Result<bool, SchemaError> {
    let tables: Option<BTreeMap<String, String>> = db
        .query("RETURN (INFO FOR DB).tables")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(SchemaError::Apply)?
        .take(0)
        .map_err(SchemaError::Apply)?;
    Ok(tables.is_some_and(|tables| tables.contains_key(table)))
}

async fn migrate_inbox_message_table(db: &SurrealDb) -> Result<(), SchemaError> {
    let tables: Option<BTreeMap<String, String>> = db
        .query("RETURN (INFO FOR DB).tables")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(SchemaError::Apply)?
        .take(0)
        .map_err(SchemaError::Apply)?;
    if !tables.is_some_and(|tables| tables.contains_key("inbox_message")) {
        return Ok(());
    }
    let mut response = db
        .query("SELECT * FROM inbox_message")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(SchemaError::Apply)?;
    let legacy: Vec<LegacyInboxMessage> = response.take(0).map_err(SchemaError::Apply)?;
    for row in legacy {
        db.query("UPSERT $id SET conversation_id = $conversation, direction = $direction, \
                  body = $body, author_person_id = $author, provider_message_id = $provider_message, \
                  provider_ref = $provider_ref, delivery_state = $delivery, purpose = $purpose, \
                  attachment_asset_id = $attachment, communication_id = $communication, \
                  occurred_at = $occurred, inserted_at = $inserted, updated_at = $updated")
            .bind((
                "id",
                crate::surreal::record_id(
                    "message",
                    crate::surreal::record_uuid(&row.id)
                        .ok_or(SchemaError::LegacyInboxMessageId)?,
                ),
            ))
            .bind(("conversation", row.conversation_id))
            .bind(("direction", row.direction))
            .bind(("body", row.body))
            .bind(("author", row.author_person_id))
            .bind(("provider_message", row.provider_message_id))
            .bind(("provider_ref", row.provider_ref))
            .bind(("delivery", row.delivery_state))
            .bind(("purpose", row.purpose))
            .bind(("attachment", row.attachment_asset_id))
            .bind(("communication", row.communication_id))
            .bind(("occurred", row.occurred_at))
            .bind(("inserted", row.inserted_at))
            .bind(("updated", row.updated_at))
            .await
            .and_then(surrealdb::IndexedResults::check)
            .map_err(classify_apply)?;
    }
    db.query("DEFINE FIELD legacy_message_id ON inbox_ingress TYPE option<record<inbox_message>>; \
              DEFINE FIELD legacy_message_id ON inbox_delivery_attempt TYPE option<record<inbox_message>>; \
              UPDATE inbox_ingress SET legacy_message_id = message_id; \
              UPDATE inbox_delivery_attempt SET legacy_message_id = message_id; \
              REMOVE FIELD message_id ON inbox_ingress; \
              REMOVE FIELD message_id ON inbox_delivery_attempt; \
              DEFINE FIELD message_id ON inbox_ingress TYPE record<message>; \
              DEFINE FIELD message_id ON inbox_delivery_attempt TYPE record<message>; \
              UPDATE inbox_ingress SET message_id = type::record('message', meta::id(legacy_message_id)); \
              UPDATE inbox_delivery_attempt SET message_id = type::record('message', meta::id(legacy_message_id)); \
              REMOVE FIELD legacy_message_id ON inbox_ingress; \
              REMOVE FIELD legacy_message_id ON inbox_delivery_attempt; \
              REMOVE TABLE inbox_message")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(classify_apply)?;
    Ok(())
}

/// The version recorded in `db`, or `None` when no schema has been
/// applied.
pub async fn installed_version(db: &SurrealDb) -> Result<Option<u32>, SchemaError> {
    let response = db
        .query(format!("SELECT VALUE version FROM {VERSION_RECORD}"))
        .await
        .and_then(surrealdb::IndexedResults::check);

    let recorded: Option<i64> = match response {
        Ok(mut response) => response.take(0).map_err(SchemaError::ReadVersion)?,
        // A never-applied database has no `schema_version` table at
        // all, and selecting from an undefined table is an error here
        // rather than an empty result. That error IS the absent
        // answer, so it is matched structurally — on the engine's typed
        // `NotFound(Table)` detail, never on message text — and any
        // other failure still surfaces.
        Err(err) if is_undefined_table(&err, VERSION_TABLE) => return Ok(None),
        Err(err) => return Err(SchemaError::ReadVersion(err)),
    };

    recorded
        .map(|version| u32::try_from(version).map_err(|_| SchemaError::UnreadableVersion(version)))
        .transpose()
}

/// Whether `err` is the engine reporting that `table` has never been
/// defined.
fn is_undefined_table(err: &SurrealQueryError, table: &str) -> bool {
    matches!(
        err.details(),
        ErrorDetails::NotFound(Some(NotFoundError::Table { name })) if name == table
    )
}

/// Read the applied schema back out of the engine.
///
/// The engine is the authority here, not this crate's `.surql` file:
/// a diagram or a drift report has to describe the database that
/// actually exists. Surreal keeps a field's reference in its *type*
/// (`record<entity>`), so these definitions carry both the columns and
/// the relationships in one read.
pub async fn introspect(db: &SurrealDb) -> Result<Introspection, SchemaError> {
    // `take` deserializes one statement's result; the projection above
    // yields a single object, hence `Option<..>`.
    let tables: Option<BTreeMap<String, String>> = db
        .query("RETURN (INFO FOR DB).tables")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(SchemaError::Introspect)?
        .take(0)
        .map_err(SchemaError::Introspect)?;

    let mut introspection = Introspection::new();
    for (name, definition) in tables.unwrap_or_default() {
        let fields: Option<BTreeMap<String, String>> = db
            .query(format!("RETURN (INFO FOR TABLE {name}).fields"))
            .await
            .and_then(surrealdb::IndexedResults::check)
            .map_err(SchemaError::Introspect)?
            .take(0)
            .map_err(SchemaError::Introspect)?;
        introspection.insert(
            name,
            TableDefinition {
                definition,
                fields: fields.unwrap_or_default(),
            },
        );
    }
    Ok(introspection)
}

/// Compare `db`'s applied schema to this build's.
pub async fn state(db: &SurrealDb) -> Result<SchemaState, SchemaError> {
    Ok(match installed_version(db).await? {
        None => SchemaState::Absent,
        Some(installed) if installed == SCHEMA_VERSION => SchemaState::InSync,
        Some(installed) => SchemaState::Drifted {
            installed,
            expected: SCHEMA_VERSION,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::{
        apply, backfill_person_defaults, base_table_names, derived_tables, derived_tables_in,
        installed_version, introspect, person_defaults_missing_rows, state, strip_derived_tables,
        table_names, table_names_in, SchemaError, SchemaState, DEFINITIONS, SCHEMA_VERSION,
        VERSION_RECORD,
    };
    use crate::surreal::test_support::{mem, unmigrated};
    use surrealdb::types::SurrealValue;
    use uuid::Uuid;

    #[derive(surrealdb::types::SurrealValue)]
    struct PreservedMessage {
        provider_message_id: String,
        provider_ref: String,
        delivery_state: String,
        communication_id: surrealdb::types::RecordId,
        attachment_asset_id: surrealdb::types::RecordId,
    }

    /// A historical `brand` row from before ENG-659, written with a raw
    /// query exactly as a pre-migration build's `store::brands::create`
    /// once would — `firm_id IS NONE`, no live path left to reproduce it.
    ///
    /// The very first `apply` against a fresh database already finds zero
    /// `firm_id IS NONE` rows and tightens the field to a required
    /// `record<firm>` immediately (there is nothing historical to
    /// backfill yet) — the same shape [`PROJECT_BRAND_BACKFILL`]'s own
    /// tightening `DEFINE FIELD` already takes for `project.brand`. So a
    /// test that wants to simulate a *pre-tightening* row loosens the field
    /// back open first, exactly as
    /// `designating_a_dri_on_a_project_written_before_brand_was_defined`
    /// does for `project.brand`.
    async fn historical_orphan_brand(db: &crate::surreal::SurrealDb, key: &str) -> Uuid {
        db.query("DEFINE FIELD OVERWRITE firm_id ON brand TYPE option<record<firm>>")
            .await
            .unwrap()
            .check()
            .unwrap();
        let id = Uuid::now_v7();
        let now = chrono::Utc::now().to_rfc3339();
        db.query(
            "CREATE $id SET name = $name, brand_key = $key, \
             inserted_at = $now, updated_at = $now",
        )
        .bind(("id", crate::surreal::record_id("brand", id)))
        .bind(("name", key.to_string()))
        .bind(("key", key.to_string()))
        .bind(("now", now))
        .await
        .unwrap()
        .check()
        .unwrap();
        id
    }

    /// ENG-659: a historical `firm_id IS NONE` brand row that some Firm
    /// already wears through `firm_brand` is backfilled onto exactly that
    /// Firm — the primary case the migration exists for, since every
    /// compiled house brand reaches this state.
    #[tokio::test]
    async fn applying_backfills_a_worn_historical_brand_onto_its_wearing_firm() {
        let db = unmigrated().await;
        apply(&db).await.unwrap();

        let entity_id = crate::test_support::seed_entity(&db).await;
        let admin = crate::persons::create(
            &db,
            &crate::persons::NewPerson::with_role(
                "Backfill Admin DRI",
                "backfill-admin-dri@example.com",
                crate::persons::Role::Admin,
            ),
        )
        .await
        .unwrap();
        let firm = crate::firms::create(
            &db,
            &crate::firms::NewFirm {
                name: "Backfill Wearing Practice".to_string(),
                status: "active".to_string(),
                entity_id,
                admin_dri_person_id: admin.id,
            },
        )
        .await
        .unwrap();

        let brand_id = historical_orphan_brand(&db, "worn-orphan").await;
        crate::firms::attach_brand(&db, firm.id, "worn-orphan")
            .await
            .unwrap();

        // Re-apply, exactly as a boot would against an already-seeded
        // database: this is what runs the backfill now that the historical
        // row and its `firm_brand` wearer both exist.
        apply(&db).await.unwrap();

        let backfilled = crate::brands::find_by_id(&db, brand_id)
            .await
            .unwrap()
            .expect("the historical row survives the backfill");
        assert_eq!(backfilled.firm_id, Some(firm.id));
    }

    /// ENG-659: a historical `firm_id IS NONE` row no Firm wears and no
    /// Project names is deleted — the migration's own "delete an unworn
    /// system-wide row" guidance.
    #[tokio::test]
    async fn applying_deletes_an_unworn_unreferenced_historical_brand() {
        let db = unmigrated().await;
        apply(&db).await.unwrap();

        let brand_id = historical_orphan_brand(&db, "truly-unworn-orphan").await;
        apply(&db).await.unwrap();

        assert!(
            crate::brands::find_by_id(&db, brand_id)
                .await
                .unwrap()
                .is_none(),
            "an unworn, unreferenced historical brand must be deleted, not left dangling"
        );
    }

    /// ENG-659: a historical `firm_id IS NONE` row no Firm wears, but a
    /// live Project still names, is not deleted — it is assigned to the
    /// anchor Firm instead, mirroring ENG-677's own precedent for an
    /// orphaned brand reference, so a live matter never points at a
    /// deleted row.
    #[tokio::test]
    async fn applying_assigns_a_project_referenced_unworn_historical_brand_to_the_anchor_firm() {
        let db = unmigrated().await;
        apply(&db).await.unwrap();

        // The anchor Firm: an Entity claiming the configured anchor key,
        // and a Firm on it — the same resolution `store::firms::anchor_firm`
        // performs at read time.
        let configured = std::env::var(crate::seed::BOOTSTRAP_COMPANY_ENV)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| crate::seed::FIRM_ENTITY_NAME.to_string());
        let anchor_entity = crate::entities::create(
            &db,
            &crate::entities::NewEntity {
                name: configured.clone(),
                entity_type_id: crate::test_support::SEED_ENTITY_TYPE_ID,
                jurisdiction_id: crate::test_support::SEED_ENTITY_JURISDICTION_ID,
                phone: None,
                url: None,
                xero_id: None,
                firm_anchor_key: Some(configured.to_lowercase()),
            },
        )
        .await
        .unwrap();
        let admin = crate::persons::create(
            &db,
            &crate::persons::NewPerson::with_role(
                "Anchor Admin DRI",
                "anchor-admin-dri@example.com",
                crate::persons::Role::Admin,
            ),
        )
        .await
        .unwrap();
        let anchor_firm = crate::firms::create(
            &db,
            &crate::firms::NewFirm {
                name: configured,
                status: "active".to_string(),
                entity_id: anchor_entity.id,
                admin_dri_person_id: admin.id,
            },
        )
        .await
        .unwrap();

        let brand_id = historical_orphan_brand(&db, "project-referenced-orphan").await;
        let project_entity_id = crate::test_support::seed_entity(&db).await;
        crate::projects::create(
            &db,
            &crate::projects::NewProject {
                code: "orphan-brand-project".to_string(),
                name: "Orphan Brand Project".to_string(),
                status: "open".to_string(),
                brand: "project-referenced-orphan".to_string(),
                entity_id: project_entity_id,
                ..Default::default()
            },
        )
        .await
        .unwrap();

        apply(&db).await.unwrap();

        let backfilled = crate::brands::find_by_id(&db, brand_id)
            .await
            .unwrap()
            .expect("a project-referenced historical row must not be deleted");
        assert_eq!(backfilled.firm_id, Some(anchor_firm.id));
    }

    /// The Firm `store::firms::anchor_firm` resolves: an Entity claiming the
    /// configured anchor key, and a Firm on that Entity.
    async fn seed_anchor_firm(db: &crate::surreal::SurrealDb) -> Uuid {
        let configured = std::env::var(crate::seed::BOOTSTRAP_COMPANY_ENV)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| crate::seed::FIRM_ENTITY_NAME.to_string());
        let anchor_entity = crate::entities::create(
            db,
            &crate::entities::NewEntity {
                name: configured.clone(),
                entity_type_id: crate::test_support::SEED_ENTITY_TYPE_ID,
                jurisdiction_id: crate::test_support::SEED_ENTITY_JURISDICTION_ID,
                phone: None,
                url: None,
                xero_id: None,
                firm_anchor_key: Some(configured.to_lowercase()),
            },
        )
        .await
        .unwrap();
        let admin = crate::persons::create(
            db,
            &crate::persons::NewPerson::with_role(
                "Anchor Admin DRI",
                "project-firm-anchor-admin@example.com",
                crate::persons::Role::Admin,
            ),
        )
        .await
        .unwrap();
        crate::firms::create(
            db,
            &crate::firms::NewFirm {
                name: configured,
                status: "active".to_string(),
                entity_id: anchor_entity.id,
                admin_dri_person_id: admin.id,
            },
        )
        .await
        .unwrap()
        .id
    }

    async fn seed_practice_firm(
        db: &crate::surreal::SurrealDb,
        name: &str,
        admin_name: &str,
        admin_email: &str,
    ) -> Uuid {
        let entity_id = crate::test_support::seed_entity(db).await;
        let admin = crate::persons::create(
            db,
            &crate::persons::NewPerson::with_role(
                admin_name,
                admin_email,
                crate::persons::Role::Admin,
            ),
        )
        .await
        .unwrap();
        crate::firms::create(
            db,
            &crate::firms::NewFirm {
                name: name.to_string(),
                status: "active".to_string(),
                entity_id,
                admin_dri_person_id: admin.id,
            },
        )
        .await
        .unwrap()
        .id
    }

    /// Boot assigns a Project to the Firm wearing its brand before using the
    /// anchor fallback. A Project that already names a Firm keeps it.
    #[tokio::test]
    async fn applying_points_an_unowned_project_at_the_anchor_firm() {
        let db = unmigrated().await;
        apply(&db).await.unwrap();
        let anchor_firm_id = seed_anchor_firm(&db).await;
        let other_firm_id = seed_practice_firm(
            &db,
            "Other Practice",
            "Other Admin DRI",
            "project-firm-other-admin@example.com",
        )
        .await;

        let brand_key = "project-firm-backfill";
        historical_orphan_brand(&db, brand_key).await;
        crate::firms::attach_brand(&db, other_firm_id, brand_key)
            .await
            .unwrap();
        let fallback_brand = "project-firm-backfill-fallback";
        historical_orphan_brand(&db, fallback_brand).await;
        let unowned = crate::projects::create(
            &db,
            &crate::projects::NewProject {
                code: "unowned-on-boot".to_string(),
                name: "Unowned On Boot".to_string(),
                status: "open".to_string(),
                brand: brand_key.to_string(),
                entity_id: crate::test_support::seed_entity(&db).await,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let owned = crate::projects::create(
            &db,
            &crate::projects::NewProject {
                code: "already-owned".to_string(),
                name: "Already Owned".to_string(),
                status: "open".to_string(),
                brand: brand_key.to_string(),
                entity_id: crate::test_support::seed_entity(&db).await,
                firm_id: Some(other_firm_id),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let fallback = crate::projects::create(
            &db,
            &crate::projects::NewProject {
                code: "fallback-on-boot".to_string(),
                name: "Fallback On Boot".to_string(),
                status: "open".to_string(),
                brand: fallback_brand.to_string(),
                entity_id: crate::test_support::seed_entity(&db).await,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(unowned.firm_id, None);

        apply(&db).await.unwrap();

        let unowned = crate::projects::find_by_id(&db, unowned.id)
            .await
            .unwrap()
            .expect("unowned project");
        let owned = crate::projects::find_by_id(&db, owned.id)
            .await
            .unwrap()
            .expect("owned project");
        assert_eq!(unowned.firm_id, Some(other_firm_id));
        assert_eq!(
            crate::projects::find_by_id(&db, fallback.id)
                .await
                .unwrap()
                .unwrap()
                .firm_id,
            Some(anchor_firm_id)
        );
        assert_eq!(owned.firm_id, Some(other_firm_id));

        apply(&db).await.unwrap();
        let again = crate::projects::find_by_id(&db, unowned.id)
            .await
            .unwrap()
            .expect("unowned project");
        assert_eq!(again.firm_id, Some(other_firm_id));
    }

    /// #1145: Navigator's authorization stays above the database, so
    /// every table lands `PERMISSIONS NONE`. That is also the engine's
    /// default, which is exactly the risk — an omitted clause and a
    /// deliberate one would be indistinguishable. This asserts the
    /// clause is written, not which clause it is, so the day a table
    /// wants `PERMISSIONS FOR select ...` it passes unchanged and only
    /// a silent default fails.
    #[test]
    fn every_defined_table_states_its_permissions() {
        for (file, definitions) in [("navigator.surql", DEFINITIONS)] {
            let tables: Vec<&str> = definitions
                .lines()
                .map(str::trim)
                .filter(|line| line.starts_with("DEFINE TABLE"))
                .collect();
            assert!(!tables.is_empty(), "no DEFINE TABLE statements in {file}");

            for statement in tables {
                assert!(
                    statement.contains(" PERMISSIONS "),
                    "no PERMISSIONS clause in {file}: {statement}"
                );
            }
        }
    }

    #[tokio::test]
    async fn answer_notation_id_is_a_nullable_notation_link() {
        let db = unmigrated().await;
        apply(&db).await.unwrap();

        let fields = introspect(&db).await.unwrap();
        let notation_id = &fields["answer"].fields["notation_id"];
        assert!(notation_id.contains("record<notation>"), "{notation_id}");
        assert!(!notation_id.contains("uuid"), "{notation_id}");
    }

    #[tokio::test]
    async fn notation_package_id_is_a_nullable_package_link() {
        let db = unmigrated().await;
        apply(&db).await.unwrap();

        let fields = introspect(&db).await.unwrap();
        let package_id = &fields["notation"].fields["package_id"];
        assert!(
            package_id.contains("record<notation_package>"),
            "{package_id}"
        );
        assert!(!package_id.contains("uuid"), "{package_id}");
    }

    #[tokio::test]
    async fn relationship_source_id_is_a_nullable_union_link() {
        let db = unmigrated().await;
        apply(&db).await.unwrap();

        let fields = introspect(&db).await.unwrap();
        let source_id = &fields["relationship"].fields["source_id"];
        assert!(
            source_id.contains("record<relationship_log | disclosure>"),
            "{source_id}"
        );
        assert!(!source_id.contains("uuid"), "{source_id}");
    }

    /// The name is the table, never the clause before it: `OVERWRITE` and
    /// `IF NOT EXISTS` are skipped, and a bare `DEFINE TABLE` still works.
    #[test]
    fn discovery_names_the_table_not_the_clause() {
        let definitions = "\
            DEFINE TABLE IF NOT EXISTS alpha SCHEMAFULL PERMISSIONS NONE;\n\
            DEFINE TABLE OVERWRITE beta TYPE NORMAL AS SELECT * FROM alpha;\n\
            DEFINE TABLE gamma SCHEMALESS;\n\
            -- DEFINE TABLE commented SCHEMAFULL;\n";
        assert_eq!(table_names_in(definitions), ["alpha", "beta", "gamma"]);
        assert!(!table_names().contains(&"OVERWRITE".to_string()));
        assert!(!table_names().contains(&"IF".to_string()));
    }

    /// The shipped schema's one derived table is `draft`, computed from
    /// `template`: a table, but not an independent entity.
    #[test]
    fn the_draft_projection_is_the_one_derived_table() {
        let derived = derived_tables();
        assert_eq!(derived.len(), 1, "{derived:?}");
        assert_eq!(derived[0].name, "draft");
        assert_eq!(derived[0].source, "template");
        assert!(table_names().contains(&"draft".to_string()));
        assert!(!base_table_names().contains(&"draft".to_string()));
        assert!(base_table_names().contains(&"template".to_string()));
        assert_eq!(
            base_table_names().len() + 1,
            table_names().len(),
            "every other table keeps its own rows"
        );
    }

    #[test]
    fn a_derived_table_is_found_by_its_select_and_source() {
        let views = derived_tables_in(
            "DEFINE TABLE IF NOT EXISTS v TYPE NORMAL AS SELECT *, id AS s FROM src PERMISSIONS NONE;\n\
             DEFINE TABLE IF NOT EXISTS plain SCHEMAFULL PERMISSIONS NONE;\n",
        );
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].name, "v");
        assert_eq!(views[0].source, "src");
        assert_eq!(views[0].select, "SELECT *, id AS s FROM src");
    }

    /// An export section for a derived table — its definition and its rows —
    /// is dropped whole; every other section, and the banners around them,
    /// survive byte for byte.
    #[test]
    fn stripping_drops_only_the_derived_tables_sections() {
        let banner = |name: &str| {
            format!("-- ------------------------------\n-- {name}\n-- ------------------------------\n\n")
        };
        let export = format!(
            "OPTION IMPORT;\n\n{a}DEFINE TABLE alpha TYPE NORMAL;\n\n{d}DEFINE TABLE draft TYPE NORMAL SCHEMALESS AS SELECT * FROM template;\n\n\
             {dd}INSERT [ {{ id: draft:1 }} ];\n\n{t}DEFINE TABLE template TYPE NORMAL;\n\n{td}INSERT [ {{ id: template:1 }} ];\n",
            a = banner("TABLE: alpha"),
            d = banner("TABLE: draft"),
            dd = banner("TABLE DATA: draft"),
            t = banner("TABLE: template"),
            td = banner("TABLE DATA: template"),
        );
        let stripped = strip_derived_tables(&export);
        assert!(
            stripped.contains("DEFINE TABLE alpha") && stripped.contains("DEFINE TABLE template")
        );
        assert!(stripped.contains("INSERT [ { id: template:1 } ]"));
        assert!(!stripped.contains("draft"), "{stripped}");
        assert!(stripped.starts_with("OPTION IMPORT;"));
        assert_eq!(strip_derived_tables(&stripped), stripped);
        // A table whose name merely starts with a derived one is kept.
        let similar = format!(
            "{}DEFINE TABLE notation_draft TYPE NORMAL;\n",
            banner("TABLE: notation_draft")
        );
        assert_eq!(strip_derived_tables(&similar), similar);
    }

    #[test]
    fn table_names_cover_the_shipped_schema_in_stable_order() {
        let tables = table_names();
        assert!(tables.contains(&"schema_version".to_string()));
        assert!(tables.contains(&"person".to_string()));
        let mut sorted = tables.clone();
        sorted.sort_unstable();
        assert_eq!(tables, sorted);
    }

    #[test]
    fn table_columns_keep_fields_after_semicolons_in_comments() {
        let brand_fields: Vec<String> = super::table_columns("brand")
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert!(brand_fields.contains(&"primary_color".to_string()));
        assert!(brand_fields.contains(&"typeface".to_string()));
        assert!(brand_fields.contains(&"logo_object_key".to_string()));
    }

    #[tokio::test]
    async fn an_empty_database_reports_absent_and_applying_makes_it_in_sync() {
        let db = unmigrated().await;

        assert_eq!(state(&db).await.unwrap(), SchemaState::Absent);
        assert_eq!(installed_version(&db).await.unwrap(), None);

        apply(&db).await.unwrap();

        assert_eq!(state(&db).await.unwrap(), SchemaState::InSync);
        assert_eq!(installed_version(&db).await.unwrap(), Some(SCHEMA_VERSION));
    }

    /// Every boot re-applies, so the second apply must be a no-op that
    /// neither errors nor multiplies the version record.
    #[tokio::test]
    async fn applying_twice_succeeds_and_leaves_one_version_record() {
        let db = unmigrated().await;

        apply(&db).await.unwrap();
        apply(&db).await.unwrap();

        let records: Vec<i64> = db
            .query("SELECT VALUE version FROM schema_version")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(records, vec![i64::from(SCHEMA_VERSION)]);
        assert_eq!(state(&db).await.unwrap(), SchemaState::InSync);
    }

    #[tokio::test]
    async fn renaming_blog_post_to_post_keeps_the_rows_and_their_publications() {
        let db = unmigrated().await;
        let post = Uuid::now_v7();
        let key = crate::social_publications::publication_key(post, "x");
        db.query(
            "DEFINE TABLE blog_post SCHEMAFULL PERMISSIONS NONE; \
             DEFINE FIELD brand_key ON blog_post TYPE string; \
             DEFINE FIELD slug ON blog_post TYPE string; \
             DEFINE FIELD title ON blog_post TYPE string; \
             DEFINE FIELD description ON blog_post TYPE string; \
             DEFINE FIELD body_html ON blog_post TYPE string; \
             DEFINE FIELD status ON blog_post TYPE string; \
             DEFINE FIELD publish_at ON blog_post TYPE option<datetime>; \
             DEFINE FIELD inserted_at ON blog_post TYPE datetime; \
             DEFINE FIELD updated_at ON blog_post TYPE datetime; \
             DEFINE FIELD selected_social_accounts ON blog_post TYPE array<string> DEFAULT []; \
             DEFINE FIELD social_links ON blog_post TYPE array<object> DEFAULT []; \
             DEFINE FIELD social_links.*.provider ON blog_post TYPE string; \
             DEFINE FIELD social_links.*.account ON blog_post TYPE string; \
             DEFINE FIELD social_links.*.post_id ON blog_post TYPE string; \
             DEFINE FIELD social_links.*.permalink ON blog_post TYPE string; \
             DEFINE TABLE social_publication SCHEMAFULL PERMISSIONS NONE; \
             DEFINE FIELD blog_post_id ON social_publication TYPE record<blog_post>; \
             DEFINE FIELD provider ON social_publication TYPE string; \
             DEFINE FIELD state ON social_publication TYPE string; \
             DEFINE FIELD attempt_count ON social_publication TYPE int; \
             DEFINE FIELD provider_post_id ON social_publication TYPE option<string>; \
             DEFINE FIELD inserted_at ON social_publication TYPE datetime; \
             DEFINE FIELD updated_at ON social_publication TYPE datetime; \
             DEFINE INDEX social_publication_post_provider \
                 ON social_publication FIELDS blog_post_id, provider UNIQUE; \
             CREATE $post SET brand_key = 'neon', slug = 'old', title = 'Old', description = 'd', \
                 body_html = '<p>kept</p>', status = 'published', \
                 publish_at = d'2026-06-19T00:00:00Z', inserted_at = time::now(), \
                 updated_at = time::now(), \
                 social_links = [{ provider: 'facebook', account: 'page', post_id: '1', \
                                   permalink: 'https://www.facebook.com/page/posts/1' }]; \
             CREATE type::record('social_publication', $key) SET blog_post_id = $post, provider = 'x', state = 'confirmed', \
                 attempt_count = 1, provider_post_id = '99', \
                 inserted_at = time::now(), updated_at = time::now();",
        )
        .bind(("post", crate::surreal::record_id("blog_post", post)))
        .bind(("key", key))
        .await
        .unwrap()
        .check()
        .unwrap();

        apply(&db).await.unwrap();
        apply(&db).await.unwrap();

        let moved = crate::posts::find_by_id(&db, post)
            .await
            .unwrap()
            .expect("the Post moved");
        assert_eq!((moved.slug.as_str(), moved.title.as_str()), ("old", "Old"));
        assert_eq!(moved.body_html, "<p>kept</p>");
        assert_eq!(moved.social_links.len(), 1);
        assert_eq!(moved.social_links[0].post_id.as_deref(), Some("1"));
        let publications = crate::social_publications::for_posts(&db, &[post])
            .await
            .unwrap();
        assert_eq!(publications.len(), 1, "the publication follows its Post");
        assert_eq!(publications[0].provider_post_id.as_deref(), Some("99"));
        assert!(!super::table_exists(&db, "blog_post").await.unwrap());
        // The new definitions took over: the one-publication-per-Post-and-provider
        // index still refuses a duplicate.
        let duplicate = db
            .query(
                "CREATE type::record('social_publication', 'dup') SET post_id = $post, \
                 provider = 'x', state = 'sending', attempt_count = 1",
            )
            .bind(("post", crate::surreal::record_id("post", post)))
            .await
            .and_then(surrealdb::IndexedResults::check);
        assert!(duplicate.is_err(), "the unique index survives the move");
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn renaming_inbox_message_preserves_transcript_and_relationships() {
        let db = unmigrated().await;
        db.query(
            "DEFINE TABLE inbox_message SCHEMAFULL PERMISSIONS NONE; \
                  DEFINE FIELD conversation_id ON inbox_message TYPE record<inbox_conversation>; \
                  DEFINE FIELD direction ON inbox_message TYPE string; \
                  DEFINE FIELD body ON inbox_message TYPE string; \
                  DEFINE FIELD author_person_id ON inbox_message TYPE option<record<person>>; \
                  DEFINE FIELD provider_message_id ON inbox_message TYPE option<string>; \
                  DEFINE FIELD provider_ref ON inbox_message TYPE option<string>; \
                  DEFINE FIELD delivery_state ON inbox_message TYPE string; \
                  DEFINE FIELD purpose ON inbox_message TYPE string; \
                  DEFINE FIELD attachment_asset_id ON inbox_message TYPE option<record<asset>>; \
                  DEFINE FIELD communication_id ON inbox_message TYPE option<record<communication>>; \
                  DEFINE FIELD occurred_at ON inbox_message TYPE string; \
                  DEFINE FIELD inserted_at ON inbox_message TYPE datetime; \
                  DEFINE FIELD updated_at ON inbox_message TYPE datetime; \
                  DEFINE TABLE inbox_conversation SCHEMAFULL PERMISSIONS NONE; \
                  DEFINE TABLE inbox_ingress SCHEMAFULL PERMISSIONS NONE; \
                  DEFINE FIELD firm_id ON inbox_ingress TYPE record<firm>; \
                  DEFINE FIELD channel ON inbox_ingress TYPE string; \
                  DEFINE FIELD provider_account ON inbox_ingress TYPE string; \
                  DEFINE FIELD provider_message_id ON inbox_ingress TYPE string; \
                  DEFINE FIELD message_id ON inbox_ingress TYPE record<inbox_message>; \
                  DEFINE FIELD inserted_at ON inbox_ingress TYPE datetime; \
                  DEFINE INDEX inbox_ingress_key ON inbox_ingress FIELDS firm_id, channel, provider_account, provider_message_id UNIQUE; \
                  DEFINE TABLE inbox_delivery_attempt SCHEMAFULL PERMISSIONS NONE; \
                  DEFINE FIELD message_id ON inbox_delivery_attempt TYPE record<inbox_message>; \
                  DEFINE FIELD state ON inbox_delivery_attempt TYPE string; \
                  DEFINE FIELD provider_ref ON inbox_delivery_attempt TYPE option<string>; \
                  DEFINE FIELD detail ON inbox_delivery_attempt TYPE option<string>; \
                  DEFINE FIELD inserted_at ON inbox_delivery_attempt TYPE datetime;",
        )
        .await
        .unwrap()
        .check()
        .unwrap();
        let conversation = Uuid::now_v7();
        let message = Uuid::now_v7();
        db.query("CREATE $conversation SET id = $conversation; \
                  CREATE $message SET \
                    conversation_id = $conversation, direction = 'inbound', \
                    body = 'preserved', author_person_id = $author, provider_message_id = 'social-comment-1', provider_ref = 'provider-receipt-1', \
                    delivery_state = 'unknown', purpose = 'matter', attachment_asset_id = $attachment, communication_id = $communication, \
                    occurred_at = 'now', inserted_at = time::now(), updated_at = time::now(); \
                  CREATE inbox_ingress:ingress SET firm_id = $firm, channel = 'instagram', provider_account = 'account-1', provider_message_id = 'social-comment-1', message_id = type::record('inbox_message', $message), inserted_at = time::now(); \
                  CREATE inbox_delivery_attempt:attempt SET message_id = type::record('inbox_message', $message), state = 'unknown', provider_ref = 'provider-receipt-1', detail = 'timeout', inserted_at = time::now();")
            .bind(("conversation", crate::surreal::record_id("inbox_conversation", conversation)))
            .bind(("message", crate::surreal::record_id("inbox_message", message)))
            .bind(("author", crate::surreal::record_id("person", Uuid::now_v7())))
            .bind(("attachment", crate::surreal::record_id("asset", Uuid::now_v7())))
            .bind(("communication", crate::surreal::record_id("communication", Uuid::now_v7())))
            .bind(("firm", crate::surreal::record_id("firm", Uuid::now_v7())))
            .await
            .unwrap()
            .check()
            .unwrap();

        apply(&db).await.unwrap();
        apply(&db).await.unwrap();

        let body: Option<String> = db
            .query("SELECT VALUE body FROM ONLY $message")
            .bind(("message", crate::surreal::record_id("message", message)))
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(body.as_deref(), Some("preserved"));
        let preserved: Option<PreservedMessage> = db
            .query("SELECT provider_message_id, provider_ref, delivery_state, communication_id, attachment_asset_id FROM ONLY $message")
            .bind(("message", crate::surreal::record_id("message", message)))
            .await
            .unwrap()
            .take(0)
            .unwrap();
        let preserved = preserved.expect("the migrated Message retains all old fields");
        assert_eq!(preserved.provider_message_id, "social-comment-1");
        assert_eq!(preserved.provider_ref, "provider-receipt-1");
        assert_eq!(preserved.delivery_state, "unknown");
        assert_eq!(
            preserved.communication_id.table.to_string(),
            "communication"
        );
        assert_eq!(preserved.attachment_asset_id.table.to_string(), "asset");
        let ingress: Option<surrealdb::types::RecordId> = db
            .query("SELECT VALUE message_id FROM ONLY inbox_ingress:ingress")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert!(ingress.is_some_and(|id| id.table.to_string() == "message"));
        let attempts: Option<surrealdb::types::RecordId> = db
            .query("SELECT VALUE message_id FROM ONLY inbox_delivery_attempt:attempt")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert!(attempts.is_some_and(|id| id.table.to_string() == "message"));
        let replay_key_count: Vec<i64> = db
            .query("SELECT VALUE count() FROM inbox_ingress WHERE channel = 'instagram' AND provider_account = 'account-1' AND provider_message_id = 'social-comment-1' GROUP ALL")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(
            replay_key_count,
            vec![1],
            "applying the migration twice must not duplicate the ingress key"
        );
    }

    #[tokio::test]
    async fn project_scoped_summary_approval_migration_revokes_unscoped_approval() {
        let db = unmigrated().await;
        apply(&db).await.unwrap();
        let entity_id = crate::test_support::seed_entity(&db).await;
        let admin = crate::persons::create(
            &db,
            &crate::persons::NewPerson::with_role(
                "Summary approval admin",
                format!("summary-admin-{}@example.com", Uuid::now_v7()),
                crate::persons::Role::Admin,
            ),
        )
        .await
        .unwrap();
        let firm_id = crate::firms::create(
            &db,
            &crate::firms::NewFirm {
                name: format!("Summary Firm {}", Uuid::now_v7()),
                status: "active".into(),
                entity_id,
                admin_dri_person_id: admin.id,
            },
        )
        .await
        .unwrap()
        .id;
        let sender = crate::persons::create(
            &db,
            &crate::persons::NewPerson::new(
                "Summary sender",
                format!("summary-sender-{}@example.com", Uuid::now_v7()),
            ),
        )
        .await
        .unwrap();
        let approval_id = Uuid::now_v7();
        db.query("CREATE $id SET firm_id = $firm, provider_account = 'sendgrid', person_id = $person, sender_email = $email, status = 'approved', updated_at = time::now()")
            .bind(("id", crate::surreal::record_id("summary_sender_approval", approval_id)))
            .bind(("firm", crate::surreal::record_id(crate::firms::TABLE, firm_id)))
            .bind(("person", crate::surreal::record_id("person", sender.id)))
            .bind(("email", sender.email))
            .await
            .unwrap()
            .check()
            .unwrap();

        apply(&db).await.unwrap();
        let status: Option<String> = db
            .query("SELECT VALUE status FROM ONLY $id")
            .bind((
                "id",
                crate::surreal::record_id("summary_sender_approval", approval_id),
            ))
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(status.as_deref(), Some("revoked"));
    }

    /// Rows written before the Draft pins existed: a version takes its
    /// asset's digest, a dangling one keeps NONE, a Notation copies its
    /// version's pins, and no commit sha is invented.
    #[tokio::test]
    async fn applying_backfills_draft_pins_from_the_stored_asset() {
        let db = mem().await;
        let project = crate::test_support::seed_project_surreal(&db, "legacy pins").await;
        let bytes = b"# Legacy lease";
        let asset = crate::test_support::seed_body_asset(&db, bytes).await;
        let (synced, unsynced, dangling) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        for (id, project_id, asset_id, commit) in [
            (synced, Some(project), asset, Some("abc1234")),
            (unsynced, Some(project), asset, None),
            (dangling, None, Uuid::now_v7(), None),
        ] {
            db.query(
                "CREATE $id SET code = 'lease', title = 'Lease', respondent_type = 'person', \
                 project_id = $project, is_current = false, asset_id = $asset, \
                 source_commit_sha = $commit",
            )
            .bind(("id", crate::surreal::record_id("template", id)))
            .bind((
                "project",
                project_id.map(|p| crate::surreal::record_id(crate::projects::PROJECT_TABLE, p)),
            ))
            .bind(("asset", crate::surreal::record_id("asset", asset_id)))
            .bind(("commit", commit))
            .await
            .unwrap()
            .check()
            .unwrap();
        }
        let person = crate::test_support::dri_person(&db).await;
        let mut opened = Vec::new();
        for template in [synced, unsynced, dangling] {
            let notation = crate::notations::create(
                &db,
                &crate::notations::NewNotation::new(template, person, project, "BEGIN"),
            )
            .await
            .unwrap();
            assert_eq!(notation.template_body_sha256, None, "nothing to copy yet");
            opened.push(notation.id);
        }

        for _ in 0..2 {
            apply(&db).await.unwrap();
        }

        let digest = crate::assets::sha256_hex(bytes);
        let read = |id| crate::templates::find_by_id(&db, id);
        let synced_row = read(synced).await.unwrap().unwrap();
        assert_eq!(synced_row.body_sha256.as_deref(), Some(digest.as_str()));
        assert_eq!(synced_row.source_commit_sha.as_deref(), Some("abc1234"));
        let unsynced_row = read(unsynced).await.unwrap().unwrap();
        assert_eq!(unsynced_row.body_sha256.as_deref(), Some(digest.as_str()));
        assert_eq!(unsynced_row.source_commit_sha, None, "never invented");
        assert_eq!(read(dangling).await.unwrap().unwrap().body_sha256, None);

        let pins = |id| crate::notations::find_by_id(&db, id);
        let n = pins(opened[0]).await.unwrap().unwrap();
        assert_eq!(n.template_body_sha256.as_deref(), Some(digest.as_str()));
        assert_eq!(n.template_source_commit_sha.as_deref(), Some("abc1234"));
        let n = pins(opened[1]).await.unwrap().unwrap();
        assert_eq!(n.template_body_sha256.as_deref(), Some(digest.as_str()));
        assert_eq!(n.template_source_commit_sha, None);
        let n = pins(opened[2]).await.unwrap().unwrap();
        assert_eq!(n.template_body_sha256, None);
    }

    /// Typed reads and the raw default after `apply`. The sign-in write
    /// that fails without this backfill is covered by
    /// `schema_apply_backfills_historical_person_writes`.
    #[tokio::test]
    async fn applying_backfills_historical_person_defaults_for_typed_sign_in_reads() {
        let db = unmigrated().await;
        let historical_id = uuid::Uuid::now_v7();
        db.query(
            "CREATE $id SET name = 'Historical Person', \
             email = 'historical@example.com', oidc_subject = 'historical-subject', \
             role = 'client', \
             inserted_at = type::datetime('2020-01-01T00:00:00Z'), \
             updated_at = type::datetime('2020-01-01T00:00:00Z')",
        )
        .bind(("id", crate::surreal::record_id("person", historical_id)))
        .await
        .unwrap()
        .check()
        .unwrap();

        apply(&db).await.unwrap();

        let confirmed: Vec<bool> = db
            .query("SELECT VALUE email_confirmed FROM ONLY $id")
            .bind(("id", crate::surreal::record_id("person", historical_id)))
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(confirmed, vec![false]);

        let person = crate::persons::find_by_id(&db, historical_id)
            .await
            .unwrap()
            .expect("the historical row remains readable through PersonRow");
        assert!(!person.email_confirmed);

        let resolved = crate::persons::find_by_oidc_subject(&db, "historical-subject")
            .await
            .unwrap()
            .expect("the sign-in subject lookup resolves the historical row");
        assert_eq!(resolved.id, historical_id);
        assert!(crate::persons::is_admitted(&db, historical_id)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn person_defaults_preflight_reports_no_rows_after_convergence() {
        let db = unmigrated().await;
        apply(&db).await.unwrap();
        db.query(
            "CREATE person:converged SET name = 'Converged Person', \
             email = 'converged@example.com', role = 'client', \
             email_confirmed = true, is_admitted = false",
        )
        .await
        .unwrap()
        .check()
        .unwrap();

        assert_eq!(person_defaults_missing_rows(&db).await.unwrap(), 0);
        backfill_person_defaults(&db).await.unwrap();
    }

    /// `OVERWRITE` converges a field *definition*; it does not touch the
    /// values already stored under that field. Every `DEFINE FIELD` in
    /// `navigator.surql` relies on that, because `apply` runs the whole file
    /// on every boot — so a re-apply must be a no-op for data. The three
    /// provider subjects are the case worth pinning: they are the columns a
    /// person's sign-in identity lives in, and losing one silently locks that
    /// person out of the provider it belonged to.
    #[tokio::test]
    async fn reapplying_the_schema_preserves_every_stored_provider_subject() {
        let db = unmigrated().await;
        apply(&db)
            .await
            .expect("the first apply defines the fields");

        let id = Uuid::now_v7();
        db.query(
            "CREATE $id SET name = 'Linked Person', \
             email = 'linked@example.com', role = 'client', is_admitted = true, \
             email_confirmed = true, \
             oidc_subject = 'primary-subject', \
             microsoft_subject = 'microsoft-subject', \
             apple_subject = 'apple-subject'",
        )
        .bind(("id", crate::surreal::record_id("person", id)))
        .await
        .unwrap()
        .check()
        .unwrap();

        // A second and third boot against the same database.
        apply(&db).await.expect("a re-apply converges");
        apply(&db)
            .await
            .expect("a third apply is still a no-op for data");

        let read = |field: &'static str| {
            let db = db.clone();
            async move {
                db.query(format!("SELECT VALUE {field} FROM ONLY $id"))
                    .bind(("id", crate::surreal::record_id("person", id)))
                    .await
                    .unwrap()
                    .take::<Option<String>>(0)
                    .unwrap()
            }
        };
        assert_eq!(
            read("oidc_subject").await.as_deref(),
            Some("primary-subject")
        );
        assert_eq!(
            read("microsoft_subject").await.as_deref(),
            Some("microsoft-subject")
        );
        assert_eq!(
            read("apple_subject").await.as_deref(),
            Some("apple-subject")
        );

        // The row itself is intact, not just the three columns.
        let name: Option<String> = db
            .query("SELECT VALUE name FROM ONLY $id")
            .bind(("id", crate::surreal::record_id("person", id)))
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(name.as_deref(), Some("Linked Person"));

        // And the unique indexes survived the re-apply as enforcing indexes.
        let collision = db
            .query(
                "CREATE person:collider SET name = 'Collider', \
                 email = 'collider@example.com', role = 'client', is_admitted = true, \
                 email_confirmed = true, apple_subject = 'apple-subject'",
            )
            .await
            .unwrap()
            .check();
        assert!(
            collision.is_err(),
            "person_apple_subject must still refuse a duplicate after a re-apply",
        );
    }

    #[tokio::test]
    async fn applying_preserves_a_historical_primary_subject_and_adds_provider_fields() {
        let db = unmigrated().await;
        let historical_id = Uuid::now_v7();
        db.query(
            "CREATE $id SET name = 'Historical Person', \
             email = 'historical@example.com', role = 'client', is_admitted = true, \
             oidc_subject = 'primary-subject', \
             inserted_at = type::datetime('2020-01-01T00:00:00Z'), \
             updated_at = type::datetime('2020-01-01T00:00:00Z')",
        )
        .bind(("id", crate::surreal::record_id("person", historical_id)))
        .await
        .unwrap()
        .check()
        .unwrap();

        apply(&db)
            .await
            .expect("the provider fields converge over a historical row");

        let primary: Option<String> = db
            .query("SELECT VALUE oidc_subject FROM ONLY $id")
            .bind(("id", crate::surreal::record_id("person", historical_id)))
            .await
            .unwrap()
            .take(0)
            .unwrap();
        let microsoft: Option<String> = db
            .query("SELECT VALUE microsoft_subject FROM ONLY $id")
            .bind(("id", crate::surreal::record_id("person", historical_id)))
            .await
            .unwrap()
            .take(0)
            .unwrap();
        let apple: Option<String> = db
            .query("SELECT VALUE apple_subject FROM ONLY $id")
            .bind(("id", crate::surreal::record_id("person", historical_id)))
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(primary.as_deref(), Some("primary-subject"));
        assert_eq!(microsoft, None);
        assert_eq!(apple, None);
    }

    /// A row coded `closed` written before that code was reserved (simulated
    /// here by writing it into a database with no schema applied at all, so
    /// nothing validates the write) must not be silently outlived by the new
    /// `ASSERT` on `project.code` — SurrealDB does not retroactively enforce
    /// a `DEFINE FIELD` against rows already on disk. `apply` must refuse
    /// rather than converge and leave that matter's own detail page shadowed
    /// by the new `/app/projects/closed` route with no warning.
    #[tokio::test]
    async fn apply_refuses_a_historical_row_already_holding_a_reserved_code() {
        let db = unmigrated().await;
        db.query(
            "CREATE project SET code = 'closed', name = 'Historical', status = 'open', \
             entity_id = rand::uuid(), inserted_at = '2020-01-01T00:00:00Z', \
             updated_at = '2020-01-01T00:00:00Z'",
        )
        .await
        .and_then(surrealdb::IndexedResults::check)
        .expect("write the historical row before any schema exists");

        let error = apply(&db)
            .await
            .expect_err("a reserved historical code must refuse to apply");
        assert!(
            matches!(error, SchemaError::ReservedProjectCode(ref code) if code == "closed"),
            "{error}"
        );

        // The guard runs before `DEFINE` installs the new schema, so a
        // refused apply must not have left the database half-migrated.
        assert_eq!(installed_version(&db).await.unwrap(), None);
    }

    /// ENG-119: a participation is one person's current scope on one
    /// Project. The natural-key uniqueness moves with the table so a second
    /// assignment cannot create competing authorization answers.
    #[tokio::test]
    async fn projects_cluster_schema_rejects_a_duplicate_participation() {
        let db = unmigrated().await;
        apply(&db).await.unwrap();

        db.query(
            "CREATE person:lawyer SET name = 'Lawyer', email = 'lawyer@example.com', \
             role = 'lawyer'",
        )
        .await
        .unwrap()
        .check()
        .unwrap();
        let entity_id = uuid::Uuid::parse_str("0198a36a-55cc-7fd0-8af7-4f30e72b761c").unwrap();
        db.query(
            "CREATE project:matter SET code = 'matter', name = 'Matter', status = 'open', \
             entity_id = $entity_id, brand = 'neon', \
             inserted_at = '2026-08-04T00:00:00Z', updated_at = '2026-08-04T00:00:00Z'",
        )
        .bind(("entity_id", crate::surreal::record_id("entity", entity_id)))
        .await
        .unwrap()
        .check()
        .unwrap();
        db.query(
            "CREATE person_project_role:one SET person_id = person:lawyer, \
             project_id = project:matter, participation = 'lawyer', \
             inserted_at = '2026-08-04T00:00:00Z', updated_at = '2026-08-04T00:00:00Z'",
        )
        .await
        .unwrap()
        .check()
        .unwrap();

        let duplicate = db
            .query(
                "CREATE person_project_role:two SET person_id = person:lawyer, \
                 project_id = project:matter, participation = 'client', \
                 inserted_at = '2026-08-04T00:00:00Z', updated_at = '2026-08-04T00:00:00Z'",
            )
            .await
            .unwrap()
            .check();
        assert!(duplicate.is_err(), "duplicate participation was accepted");
    }

    /// ENG-478: the schema closes `participation` to the same five
    /// role-derived words `person.role` admits. Every one of them must still
    /// write — this is defense in depth, not a narrower vocabulary than the
    /// one `store::projects::participation_for_role` already derives from.
    #[tokio::test]
    async fn participation_schema_accepts_every_role_derived_value() {
        let db = unmigrated().await;
        apply(&db).await.unwrap();
        let entity_id = uuid::Uuid::parse_str("0198a36a-55cc-7fd0-8af7-4f30e72b761c").unwrap();
        db.query(
            "CREATE project:role_matter SET code = 'role-matter', name = 'Role Matter', \
             status = 'open', entity_id = $entity_id, brand = 'neon', \
             inserted_at = '2026-08-04T00:00:00Z', updated_at = '2026-08-04T00:00:00Z'",
        )
        .bind(("entity_id", crate::surreal::record_id("entity", entity_id)))
        .await
        .unwrap()
        .check()
        .unwrap();

        // A distinct Person per value, keyed by the value itself: the unique
        // `person_project_role_pair` index refuses a second row for the same
        // (person, project), which is the invariant
        // `projects_cluster_schema_rejects_a_duplicate_participation` already
        // covers — this test's own row per value must not collide with it.
        for value in ["owner", "admin", "lawyer", "clerk", "client"] {
            db.query(format!(
                "CREATE person:{value}_role_person SET name = '{value}', \
                 email = '{value}-role-person@example.com', role = 'lawyer'"
            ))
            .await
            .unwrap()
            .check()
            .unwrap();
            db.query(format!(
                "CREATE person_project_role SET person_id = person:{value}_role_person, \
                 project_id = project:role_matter, participation = $participation, \
                 inserted_at = '2026-08-04T00:00:00Z', updated_at = '2026-08-04T00:00:00Z'"
            ))
            .bind(("participation", value.to_string()))
            .await
            .unwrap()
            .check()
            .unwrap_or_else(|error| panic!("{value} must be an accepted participation: {error}"));
        }
    }

    /// The other half of the same boundary: a word outside the five is
    /// refused, whatever wrote it — a privileged direct write included, since
    /// this is exactly the write path the typed `store::participation`
    /// commands never take.
    #[tokio::test]
    async fn participation_schema_rejects_an_unsupported_value() {
        let db = unmigrated().await;
        apply(&db).await.unwrap();
        db.query(
            "CREATE person:role_person SET name = 'Person', \
             email = 'role-person@example.com', role = 'lawyer'",
        )
        .await
        .unwrap()
        .check()
        .unwrap();
        let entity_id = uuid::Uuid::parse_str("0198a36a-55cc-7fd0-8af7-4f30e72b761c").unwrap();
        db.query(
            "CREATE project:unsupported_matter SET code = 'unsupported-matter', \
             name = 'Unsupported Matter', status = 'open', entity_id = $entity_id, \
             brand = 'neon', inserted_at = '2026-08-04T00:00:00Z', updated_at = '2026-08-04T00:00:00Z'",
        )
        .bind(("entity_id", crate::surreal::record_id("entity", entity_id)))
        .await
        .unwrap()
        .check()
        .unwrap();

        for value in ["counterparty", "attorney", "paralegal", "co_counsel", ""] {
            let rejected = db
                .query(
                    "CREATE person_project_role SET person_id = person:role_person, \
                     project_id = project:unsupported_matter, participation = $participation, \
                     inserted_at = '2026-08-04T00:00:00Z', updated_at = '2026-08-04T00:00:00Z'",
                )
                .bind(("participation", value.to_string()))
                .await
                .unwrap()
                .check();
            assert!(
                rejected.is_err(),
                "{value:?} must not be an accepted participation"
            );
        }
    }

    /// Re-applying must converge definitions without touching rows —
    /// the property `DEFINE TABLE IF NOT EXISTS` is chosen for.
    #[tokio::test]
    async fn re_applying_preserves_the_rows_already_there() {
        let db = unmigrated().await;
        apply(&db).await.unwrap();
        db.query("CREATE person:alice SET name = 'Alice', email = 'alice@example.com'")
            .await
            .unwrap()
            .check()
            .unwrap();

        apply(&db).await.unwrap();

        let name: Option<String> = db
            .query("SELECT VALUE name FROM person:alice")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(name.as_deref(), Some("Alice"));
    }

    #[tokio::test]
    async fn a_database_another_build_prepared_reports_drifted_in_both_directions() {
        for installed in [SCHEMA_VERSION - 1, SCHEMA_VERSION + 1] {
            let db = unmigrated().await;
            apply(&db).await.unwrap();
            db.query(format!("UPSERT {VERSION_RECORD} SET version = $version"))
                .bind(("version", i64::from(installed)))
                .await
                .unwrap()
                .check()
                .unwrap();

            assert_eq!(
                state(&db).await.unwrap(),
                SchemaState::Drifted {
                    installed,
                    expected: SCHEMA_VERSION,
                },
                "installed version {installed}"
            );
        }
    }

    /// Seed one person and one entity as the *deployment* schema
    /// defines them — full rows, not the name-only nodes the retired
    /// projection used. Both reference links point at rows that were
    /// never written, which the engine accepts: a `record<T>` link is a
    /// type constraint, not a foreign key, and nothing here needs the
    /// entity type or jurisdiction resolved.
    async fn graph_nodes(db: &crate::surreal::SurrealDb) {
        db.query(
            "CREATE person:alice SET name = 'Alice', email = 'alice@example.com';
             CREATE entity:acme SET name = 'Acme LLC',
                 entity_type_id = entity_type:llc, jurisdiction_id = jurisdiction:nv;",
        )
        .await
        .unwrap()
        .check()
        .unwrap();
    }

    /// The conflict graph is the deployment schema now (ENG-120), not a
    /// projection of it: `store::conflicts` traverses these two edge
    /// tables on the configured connection. So the relate-and-traverse
    /// that used to prove the *projection* works has to prove it here,
    /// against the rows the application actually writes.
    #[tokio::test]
    async fn the_conflicts_graph_relates_and_traverses() {
        let db = unmigrated().await;
        apply(&db).await.unwrap();
        graph_nodes(&db).await;

        db.query("RELATE person:alice->entity_role->entity:acme SET role = 'owner'")
            .await
            .unwrap()
            .check()
            .unwrap();
        db.query(
            "RELATE person:alice->relationship->entity:acme \
             SET kind = 'adverse', confidence_pct = 90, \
                 source_kind = 'manual', detail = 'disclosed'",
        )
        .await
        .unwrap()
        .check()
        .unwrap();

        let owned: Option<Vec<String>> = db
            .query("SELECT VALUE ->entity_role->entity.name FROM person:alice")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(owned, Some(vec!["Acme LLC".to_string()]));
    }

    /// An ASSERT bounds `relationship_edges.confidence_pct`, so a bad
    /// confidence is rejected by the engine rather than by whichever
    /// caller remembers to look.
    #[tokio::test]
    async fn an_out_of_range_confidence_is_rejected_by_the_schema() {
        let db = unmigrated().await;
        apply(&db).await.unwrap();
        graph_nodes(&db).await;

        let rejected = db
            .query(
                "RELATE person:alice->relationship->entity:acme \
                 SET kind = 'adverse', confidence_pct = 101, source_kind = 'manual'",
            )
            .await
            .unwrap()
            .check();

        assert!(rejected.is_err(), "the engine accepted confidence_pct 101");
    }

    /// The firm anchor's protection is a schema guarantee. SurrealDB has
    /// no advisory lock, so the
    /// invariant is carried by a UNIQUE index over a key only anchor
    /// rows populate. This pins both halves: the fork is refused, and
    /// ordinary namesakes — which leave the key NONE — still coexist,
    /// because multiple NONEs do not collide on a Surreal unique index.
    #[tokio::test]
    async fn the_firm_anchor_key_refuses_a_fork_but_admits_namesakes() {
        let db = unmigrated().await;
        apply(&db).await.unwrap();

        let anchor = |id: &str, key: &str| {
            format!(
                "CREATE entity:{id} SET name = 'Neon Law', \
                 entity_type_id = entity_type:llc, jurisdiction_id = jurisdiction:nv, \
                 firm_anchor_key = '{key}'"
            )
        };
        db.query(anchor("firm", "shook law pc"))
            .await
            .unwrap()
            .check()
            .unwrap();

        let forked = db
            .query(anchor("fork", "shook law pc"))
            .await
            .unwrap()
            .check();
        assert!(
            forked.is_err(),
            "a second row under the anchor's key must be refused by `entity_firm_anchor`"
        );

        // Two unrelated Betas both land: entity names are deliberately
        // non-unique, and only the anchor takes the key.
        db.query(
            "CREATE entity:beta_one SET name = 'Beta LLC',
                 entity_type_id = entity_type:llc, jurisdiction_id = jurisdiction:nv;
             CREATE entity:beta_two SET name = 'Beta LLC',
                 entity_type_id = entity_type:llc, jurisdiction_id = jurisdiction:nv;",
        )
        .await
        .unwrap()
        .check()
        .unwrap();
    }
}

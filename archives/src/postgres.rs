//! PostgreSQL source for the [Data Export](../../docs/glossary/data-export.md).
//!
//! Typed table reads use a `REPEATABLE READ, READ ONLY` transaction. Empty
//! tables produce valid zero-row Parquet files. Schema-qualified catalog
//! reads include AGE types and PostgreSQL large-object values.
//!
//! Configured runs require the paired coverage contract in [`crate::coverage`].
//! The bounded [`export_generation`] helper publishes a single Navigator
//! source; its result also decodes journaled single-source workflow output.

use std::fmt::Write as _;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use arrow::array::{
    ArrayRef, BinaryBuilder, BooleanBuilder, Date32Builder, FixedSizeBinaryBuilder, Float32Builder,
    Float64Builder, Int32Builder, Int64Builder, ListBuilder, RecordBatch, StringBuilder,
    TimestampMicrosecondBuilder,
};
use arrow::datatypes::{DataType, Field, Schema, SchemaRef, TimeUnit};
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use cloud::StorageService;
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Postgres, Row, Transaction};

/// Source identity of the bounded single-database Navigator export.
/// Its tables live under `postgres/navigator/<table>/`, apart from the
/// `SurrealDB` `application/` lane, so a table name both stores share never
/// mixes two sources in one Iceberg table.
pub const POSTGRES_LANE: &str = "postgres/navigator";

/// Every table in the canonical PostgreSQL schema: Firm journal first, then
/// Post and Blog Video, then core tenancy, its memberships, and a Project's
/// synced Matter Pages and Matter Caption, then documents and the Notation
/// workflow, then the Clause library and the other shared catalogs, then what
/// a Person or Entity holds beside its own row, followed by inbound email
/// receipts and their delivery attempts, social accounts and saved account
/// selections, and public Leads with their consent history. Then a Project's
/// recovery generations, source imports, retention archives and purges, and
/// handoffs; whole-row Project records and playbooks; a Notation's sources
/// and a Project's Notation drafts; Git repositories and access tokens; and
/// durable DBOS ingress request identities.
/// A test pins this
/// list to the schema, so a new table cannot be left out of the export unnoticed.
pub const POSTGRES_TABLES: &[&str] = &[
    "journal_entry",
    "journal_posting",
    "journal_source_reference",
    "journal_entry_source",
    "journal_source_snapshot",
    "journal_reconciliation",
    "journal_reconciliation_signal",
    "post",
    "blog_video",
    "social_handle",
    "social_publication",
    "social_account",
    "social_account_selection",
    "jurisdiction",
    "entity_type",
    "entity",
    "firm",
    "brand",
    "firm_brand",
    "firm_integration_secret",
    "dbos_workflow_request",
    "project",
    "project_costs",
    "project_cost_events",
    "project_deadlines",
    "project_service",
    "person",
    "person_firm_role",
    "person_project_role",
    "project_reference",
    "matter_page",
    "matter_caption",
    "asset",
    "notation",
    "notation_event",
    "notation_machine_state",
    "notation_clause",
    "attestation",
    "answer",
    "template",
    "relationship_log",
    "clause",
    "authority",
    "glossary_term",
    "question",
    "notation_package",
    "notation_package_template",
    "notation_rate",
    "address",
    "mailroom",
    "letter",
    "lead",
    "lead_consent",
    "credential",
    "email_token",
    "entity_role",
    "person_delegate",
    "person_external_identity",
    "person_mailbox",
    "summary_sender_approval",
    "summary_sender_approval_event",
    "communication",
    "email_conversation",
    "email_conversation_message",
    "inbox_conversation",
    "message",
    "inbox_ingress",
    "inbox_assignment",
    "inbox_delivery_attempt",
    "sent_email",
    "email_receipt",
    "email_delivery",
    "email_delivery_attempt",
    "project_archive_generation",
    "project_archive_item",
    "project_source_import",
    "document_chain_archive",
    "document_chain_archive_item",
    "retention_purge",
    "archived_object_purge",
    "project_handoff",
    "project_handoff_acknowledgment",
    "purchase",
    "mirror_backfill_run",
    "mirror_backfill_checkpoint",
    "mirror_backfill_failure",
    "project_module",
    "statutory_deadline",
    "testimonial",
    "playbook",
    "notation_source",
    "notation_draft",
    "git_repository",
    "git_access_token",
    "authority_use",
    "cap_table_holding",
    "case",
    "case_docket_entry",
    "citation",
    "contract_review",
    "deployment_secret_change",
    "disclosure",
    "discovery_item",
    "discovery_request",
    "document_comment",
    "efiling_transactions",
    "estate_asset",
    "expunge_record",
    "expunge_request",
    "fiduciary_role",
    "filing",
    "finance_aggregate",
    "firm_anchor",
    "iolta_account",
    "iolta_allocation",
    "iolta_withdrawal",
    "notarization",
    "notation_document",
    "notation_document_comment",
    "notation_document_version",
    "notation_meter_entry",
    "relationship",
    "review_document",
    "signature",
    "social_oauth_attempt",
    "verification",
    "visitor_route_count",
    "xero_invoice",
];

/// Rows read per cursor fetch and written per Parquet row group.
const FETCH_ROWS: usize = 8192;
/// Upper bound for any one statement in the export transaction.
const STATEMENT_TIMEOUT: &str = "300s";
/// Arrow field metadata naming the source column's PostgreSQL type.
pub const POSTGRES_TYPE_KEY: &str = "navigator.postgres_type";
pub const POSTGRES_RELATION_ID_KEY: &str = "navigator.postgres.relation_identity";
pub const POSTGRES_ATTRIBUTE_NUMBER_KEY: &str = "navigator.postgres.attribute_number";
const PARQUET_FIELD_ID_KEY: &str = "PARQUET:field_id";
const EXTENSION_NAME_KEY: &str = "ARROW:extension:name";

/// One table of a published generation. Serializable because it is part of
/// the journaled snapshot-step output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostgresTableEntry {
    pub table: String,
    pub rows: usize,
    pub bytes: usize,
    /// Content-addressed Parquet object path, so a retry or a changed export
    /// in the same scheduled slot cannot replace a file referenced by history.
    pub key: String,
    /// Lower-case hex SHA-256 of the Parquet object.
    pub sha256: String,
}

/// A published PostgreSQL generation: every table read at one cutoff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostgresGeneration {
    pub source: String,
    /// `clock_timestamp()` when the shared snapshot was taken.
    pub cutoff_at: DateTime<Utc>,
    /// `pg_current_snapshot()` of the export transaction: the MVCC watermark
    /// every table was read at.
    pub snapshot: String,
    pub server_version: String,
    pub manifest_key: String,
    pub tables: Vec<PostgresTableEntry>,
}

/// The PostgreSQL outcome of one scheduled run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PostgresExport {
    Published(PostgresGeneration),
    Paired(crate::coverage::PairedManifest),
    Failed { error: String },
}

/// Called after each table is read, still inside the export transaction.
/// Production passes [`NoPause`]; tests use it to commit concurrent writes.
#[async_trait::async_trait]
pub trait AfterTable: Send + Sync {
    async fn after(&self, table: &str);
}

/// The production [`AfterTable`]: does nothing.
pub struct NoPause;

#[async_trait::async_trait]
impl AfterTable for NoPause {
    async fn after(&self, _table: &str) {}
}

/// Export configured PostgreSQL sources, skipping unconfigured sources.
/// Connection and coverage errors become [`PostgresExport::Failed`].
/// The workflow retains completed
/// source snapshots for notification, then fails the coverage invocation.
pub async fn export_configured<F: Fn(&str) -> Option<String>>(
    get: F,
    storage: &dyn StorageService,
    _scheduled_for: Option<DateTime<Utc>>,
) -> Option<PostgresExport> {
    let host = get("NAVIGATOR_POSTGRES_HOST").filter(|host| !host.trim().is_empty());
    if host.is_none() {
        return if [
            "NAVIGATOR_ARCHIVE_COVERAGE",
            "NAVIGATOR_ARCHIVE_FORGEJO_USER",
        ]
        .iter()
        .any(|key| get(key).is_some_and(|value| !value.trim().is_empty()))
        {
            Some(PostgresExport::Failed {
                error: "NAVIGATOR_POSTGRES_HOST is required for paired coverage".into(),
            })
        } else {
            None
        };
    }
    let outcome = async {
        let required = |key: &str| {
            get(key)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| anyhow!("required archive setting {key} is missing"))
        };
        let coverage: crate::coverage::ConfiguredCoverage =
            serde_json::from_str(&required("NAVIGATOR_ARCHIVE_COVERAGE")?)
                .context("decode reviewed database coverage")?;
        let db = store::postgres::PostgresDb::from_env().await?;
        let forge_options = db
            .pool()
            .connect_options()
            .as_ref()
            .clone()
            .database("forgejo")
            .username(&required("NAVIGATOR_ARCHIVE_FORGEJO_USER")?)
            .password(&required("NAVIGATOR_ARCHIVE_FORGEJO_PASSWORD")?);
        let forgejo = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(30))
            .connect_with(forge_options)
            .await?;
        let base = format!("gs://{}", required("NAVIGATOR_ARCHIVES_BUCKET")?);
        crate::coverage::export_pair(
            crate::coverage::DatabaseSource {
                name: "navigator",
                pool: db.pool(),
                coverage: &coverage.navigator,
            },
            crate::coverage::DatabaseSource {
                name: "forgejo",
                pool: &forgejo,
                coverage: &coverage.forgejo,
            },
            storage,
            &base,
        )
        .await
    }
    .await;
    Some(match outcome {
        Ok(generation) => PostgresExport::Paired(generation),
        Err(error) => {
            tracing::error!(error = ?error, "PostgreSQL generation not published");
            PostgresExport::Failed {
                error: format!("{error:#}"),
            }
        }
    })
}

/// Read `tables` at one cutoff, upload one Parquet object per table, then
/// write the generation manifest. Nothing is uploaded unless every table is
/// read; the manifest is written only after every upload succeeds.
pub async fn export_generation(
    pool: &PgPool,
    storage: &dyn StorageService,
    tables: &[&str],
    scheduled_for: Option<DateTime<Utc>>,
) -> Result<PostgresGeneration> {
    export_generation_with(pool, storage, tables, scheduled_for, &NoPause).await
}

/// [`export_generation`] with a hook between table reads.
pub async fn export_generation_with(
    pool: &PgPool,
    storage: &dyn StorageService,
    tables: &[&str],
    scheduled_for: Option<DateTime<Utc>>,
    after_table: &dyn AfterTable,
) -> Result<PostgresGeneration> {
    let mut prior = std::collections::HashMap::new();
    for table in tables {
        if let Some((bytes, _)) =
            crate::iceberg::read_prior_metadata(storage, &format!("{POSTGRES_LANE}/{table}"))
                .await?
        {
            prior.insert(*table, bytes);
        }
    }
    let extracted = extract(pool, tables, after_table, &prior).await?;
    let run_at = scheduled_for.unwrap_or(extracted.cutoff_at);
    let run = if scheduled_for.is_some() {
        run_at.format("%H%M").to_string()
    } else {
        run_at.format("%H%M%S").to_string()
    };
    let generation_prefix = format!("dt={}/run={run}", run_at.date_naive());

    let mut entries = Vec::with_capacity(extracted.tables.len());
    for (table, rows, bytes) in &extracted.tables {
        let sha256 = sha256_hex(bytes);
        let key =
            format!("{POSTGRES_LANE}/{table}/data/{generation_prefix}/snapshot-{sha256}.parquet");
        storage
            .put(&key, bytes, "application/vnd.apache.parquet")
            .await
            .with_context(|| format!("upload {key}"))?;
        entries.push(PostgresTableEntry {
            table: table.clone(),
            rows: *rows,
            bytes: bytes.len(),
            key,
            sha256,
        });
    }

    let generation = PostgresGeneration {
        source: POSTGRES_LANE.to_string(),
        cutoff_at: extracted.cutoff_at,
        snapshot: extracted.snapshot,
        server_version: extracted.server_version,
        manifest_key: format!("{POSTGRES_LANE}/_generations/{generation_prefix}/generation.json"),
        tables: entries,
    };
    let manifest = serde_json::to_vec_pretty(&generation).context("serialize generation")?;
    storage
        .put(&generation.manifest_key, &manifest, "application/json")
        .await
        .with_context(|| format!("publish {}", generation.manifest_key))?;
    tracing::info!(
        tables = generation.tables.len(),
        cutoff_at = %generation.cutoff_at,
        manifest = %generation.manifest_key,
        "PostgreSQL generation published"
    );
    Ok(generation)
}

struct Extracted {
    cutoff_at: DateTime<Utc>,
    snapshot: String,
    server_version: String,
    tables: Vec<(String, usize, Vec<u8>)>,
}

async fn extract(
    pool: &PgPool,
    tables: &[&str],
    after_table: &dyn AfterTable,
    prior: &std::collections::HashMap<&str, Vec<u8>>,
) -> Result<Extracted> {
    let mut tx = pool.begin().await.context("begin export transaction")?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await
        .context("set export isolation")?;
    sqlx::query("SELECT set_config('statement_timeout', $1, true)")
        .bind(STATEMENT_TIMEOUT)
        .execute(&mut *tx)
        .await
        .context("bound export statements")?;
    // The first snapshot-taking statement fixes the transaction's snapshot;
    // every later read sees exactly this cutoff.
    let (cutoff_at, snapshot, server_version): (DateTime<Utc>, String, String) = sqlx::query_as(
        "SELECT clock_timestamp(), pg_current_snapshot()::text, current_setting('server_version')",
    )
    .fetch_one(&mut *tx)
    .await
    .context("take export snapshot")?;

    let mut out = Vec::with_capacity(tables.len());
    for table in tables {
        let (rows, bytes) =
            export_table_with_prior(&mut tx, table, prior.get(table).map(Vec::as_slice))
                .await
                .with_context(|| format!("export PostgreSQL table `{table}`"))?;
        out.push(((*table).to_string(), rows, bytes));
        after_table.after(table).await;
    }
    tx.rollback().await.context("close export transaction")?;
    Ok(Extracted {
        cutoff_at,
        snapshot,
        server_version,
        tables: out,
    })
}

/// How one PostgreSQL column is selected, decoded and stored in Arrow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Uuid,
    Text,
    Json,
    /// Exact decimal text: PostgreSQL `numeric` exceeds Arrow/Iceberg's
    /// 38-digit decimal, so the canonical text is the lossless form.
    Numeric,
    Bool,
    Int32,
    Int64,
    Float32,
    Float64,
    Bytes,
    Date,
    Timestamptz,
    Timestamp,
    TextArray,
}

impl Kind {
    fn from_pg(type_name: &str) -> Option<Self> {
        Some(match type_name {
            "uuid" => Self::Uuid,
            "text" | "varchar" | "bpchar" | "name" | "citext" | "agtype" | "graphid" | "oid"
            | "regnamespace" | "regclass" | "char" | "_aclitem" | "label_id" | "label_kind" => {
                Self::Text
            }
            "json" | "jsonb" => Self::Json,
            "numeric" => Self::Numeric,
            "bool" => Self::Bool,
            "int2" | "int4" => Self::Int32,
            "int8" => Self::Int64,
            "float4" => Self::Float32,
            "float8" => Self::Float64,
            "bytea" => Self::Bytes,
            "date" => Self::Date,
            "timestamptz" => Self::Timestamptz,
            "timestamp" => Self::Timestamp,
            "_text" | "_varchar" | "_bpchar" | "_citext" => Self::TextArray,
            _ => return None,
        })
    }

    /// The select expression; text-like and integer columns are cast to the
    /// one Rust type each kind decodes.
    fn select(self, quoted: &str) -> String {
        match self {
            Self::Text | Self::Json | Self::Numeric => format!("{quoted}::text"),
            Self::Int32 => format!("{quoted}::int4"),
            Self::TextArray => format!("{quoted}::text[]"),
            _ => quoted.to_string(),
        }
    }

    fn field(self, name: &str, pg_type: &str) -> Field {
        let data_type = match self {
            Self::Uuid => DataType::FixedSizeBinary(16),
            Self::Text | Self::Json | Self::Numeric => DataType::Utf8,
            Self::Bool => DataType::Boolean,
            Self::Int32 => DataType::Int32,
            Self::Int64 => DataType::Int64,
            Self::Float32 => DataType::Float32,
            Self::Float64 => DataType::Float64,
            Self::Bytes => DataType::Binary,
            Self::Date => DataType::Date32,
            Self::Timestamptz => DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            Self::Timestamp => DataType::Timestamp(TimeUnit::Microsecond, None),
            Self::TextArray => {
                DataType::List(Arc::new(Field::new_list_field(DataType::Utf8, true)))
            }
        };
        let mut metadata =
            std::collections::HashMap::from([(POSTGRES_TYPE_KEY.to_string(), pg_type.to_string())]);
        match self {
            Self::Uuid => {
                metadata.insert(EXTENSION_NAME_KEY.into(), "arrow.uuid".into());
            }
            Self::Json => {
                metadata.insert(EXTENSION_NAME_KEY.into(), "arrow.json".into());
            }
            _ => {}
        }
        Field::new(name, data_type, true).with_metadata(metadata)
    }
}

struct Column {
    name: String,
    kind: Kind,
}

type CatalogRow = (String, String, String, String, i16, String, String, String);

async fn columns(
    tx: &mut Transaction<'_, Postgres>,
    schema: &str,
    table: &str,
) -> Result<Vec<(Column, Field)>> {
    let rows: Vec<CatalogRow> = sqlx::query_as(
        "SELECT current_database()::text, d.oid::text, a.attrelid::text, c.relfilenode::text, a.attnum::int2, a.attname::text, t.typname::text, format_type(a.atttypid, a.atttypmod) \
         FROM pg_attribute a JOIN pg_type t ON t.oid = a.atttypid JOIN pg_class c ON c.oid = a.attrelid CROSS JOIN pg_database d \
         WHERE d.datname = current_database() AND a.attrelid = to_regclass(quote_ident($1) || '.' || quote_ident($2)) AND a.attnum > 0 AND NOT a.attisdropped \
         ORDER BY a.attnum",
    )
    .bind(schema)
    .bind(table)
    .fetch_all(&mut **tx)
    .await
    .context("read column catalog")?;
    if rows.is_empty() {
        bail!("table is missing from the database");
    }
    rows.into_iter()
        .map(
            |(
                database,
                database_oid,
                relation_oid,
                relfilenode,
                attnum,
                name,
                type_name,
                pg_type,
            )| {
                let kind = Kind::from_pg(&type_name).ok_or_else(|| {
                    anyhow!("column `{name}` has unsupported PostgreSQL type `{pg_type}`")
                })?;
                let relation_identity =
                    serde_json::to_string(&(database, database_oid, relation_oid, relfilenode))?;
                let mut field = kind.field(&name, &pg_type);
                let mut metadata = field.metadata().clone();
                metadata.insert(POSTGRES_RELATION_ID_KEY, relation_identity);
                metadata.insert(POSTGRES_ATTRIBUTE_NUMBER_KEY, attnum.to_string());
                field = field.with_metadata(metadata);
                Ok((Column { name, kind }, field))
            },
        )
        .collect()
}

async fn primary_key(
    tx: &mut Transaction<'_, Postgres>,
    schema: &str,
    table: &str,
) -> Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT a.attname::text FROM pg_index i \
         JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey) \
         WHERE i.indrelid = to_regclass(quote_ident($1) || '.' || quote_ident($2)) AND i.indisprimary \
         ORDER BY array_position(i.indkey::int2[], a.attnum)",
    )
    .bind(schema)
    .bind(table)
    .fetch_all(&mut **tx)
    .await
    .context("read primary key catalog")
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Read one table through a server-side cursor, `FETCH_ROWS` at a time, into
/// one Parquet file. An empty table yields a valid zero-row file, so the
/// generation records it rather than omitting it.
pub(super) async fn export_table_in_schema(
    tx: &mut Transaction<'_, Postgres>,
    schema: &str,
    table: &str,
) -> Result<(usize, Vec<u8>)> {
    export_table_in_schema_with_prior(tx, schema, table, None).await
}

async fn export_table_with_prior(
    tx: &mut Transaction<'_, Postgres>,
    table: &str,
    prior: Option<&[u8]>,
) -> Result<(usize, Vec<u8>)> {
    export_table_in_schema_with_prior(tx, "public", table, prior).await
}

pub(super) async fn export_table_in_schema_with_prior(
    tx: &mut Transaction<'_, Postgres>,
    schema: &str,
    table: &str,
    prior: Option<&[u8]>,
) -> Result<(usize, Vec<u8>)> {
    let columns = columns(tx, schema, table).await?;
    let arrow_schema = arrow_schema_with_field_ids(&columns, prior)?;
    let select = columns
        .iter()
        .map(|(column, _)| column.kind.select(&quote_ident(&column.name)))
        .collect::<Vec<_>>()
        .join(", ");
    let order = primary_key(tx, schema, table).await?;
    let order = if order.is_empty() {
        String::new()
    } else {
        format!(
            " ORDER BY {}",
            order
                .iter()
                .map(|c| quote_ident(c))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let declare = format!(
        "DECLARE archive_export NO SCROLL CURSOR FOR SELECT {select} FROM ONLY {}.{}{order}",
        quote_ident(schema),
        quote_ident(table)
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(declare))
        .execute(&mut **tx)
        .await
        .context("open export cursor")?;

    let mut buffer = Vec::new();
    let properties = WriterProperties::builder()
        .set_compression(Compression::SNAPPY)
        .build();
    let mut writer = ArrowWriter::try_new(&mut buffer, arrow_schema.clone(), Some(properties))
        .context("create parquet writer")?;
    let mut total = 0;
    loop {
        // Not a cached statement: each table's cursor has its own row shape.
        let rows: Vec<PgRow> = sqlx::query(sqlx::AssertSqlSafe(format!(
            "FETCH FORWARD {FETCH_ROWS} FROM archive_export"
        )))
        .persistent(false)
        .fetch_all(&mut **tx)
        .await
        .context("fetch export rows")?;
        if rows.is_empty() {
            break;
        }
        total += rows.len();
        let arrays = columns
            .iter()
            .enumerate()
            .map(|(index, (column, _))| {
                let array = build_array(column, index, &rows)?;
                let target = arrow_schema.field(index).data_type();
                if array.data_type() == target {
                    Ok(array)
                } else {
                    arrow::compute::cast(&array, target)
                        .context("apply Parquet field identity metadata to array")
                }
            })
            .collect::<Result<Vec<_>>>()?;
        let batch = RecordBatch::try_new(arrow_schema.clone(), arrays).context("assemble batch")?;
        writer.write(&batch).context("write parquet batch")?;
        writer.flush().context("flush parquet row group")?;
    }
    sqlx::raw_sql("CLOSE archive_export")
        .execute(&mut **tx)
        .await
        .context("close export cursor")?;
    writer.close().context("close parquet writer")?;
    Ok((total, buffer))
}

fn arrow_schema_with_field_ids(
    columns: &[(Column, Field)],
    prior: Option<&[u8]>,
) -> Result<SchemaRef> {
    let mut field_ids = std::collections::BTreeMap::<String, i32>::new();
    let mut next_id = if let Some(bytes) = prior {
        let metadata: iceberg::spec::TableMetadata =
            serde_json::from_slice(bytes).context("decode prior Iceberg metadata")?;
        let source = metadata
            .properties()
            .get("navigator.postgres.relation-identity");
        let this_relation = columns[0]
            .1
            .metadata()
            .get(POSTGRES_RELATION_ID_KEY)
            .context("PostgreSQL relation identity missing")?;
        crate::iceberg::validate_postgres_relation_identity(this_relation)?;
        if source != Some(this_relation) {
            bail!("PostgreSQL relation identity changed or prior identity missing; start a new archive lineage");
        }
        crate::iceberg::validate_postgres_metadata_mapping(&metadata, this_relation)?;
        field_ids = crate::iceberg::parse_postgres_field_mapping(
            this_relation,
            metadata
                .properties()
                .get("navigator.postgres.field-ids")
                .context("prior PostgreSQL field mapping missing")?,
        )?;
        metadata
            .last_column_id()
            .checked_add(1)
            .context("Iceberg field id overflow")?
    } else {
        1
    };
    let relation_identity = columns[0]
        .1
        .metadata()
        .get(POSTGRES_RELATION_ID_KEY)
        .cloned()
        .context("PostgreSQL relation identity missing")?;
    let mut source_fields = Vec::with_capacity(columns.len());
    for (_, field) in columns {
        let attnum = field
            .metadata()
            .get(POSTGRES_ATTRIBUTE_NUMBER_KEY)
            .context("attribute identity missing")?;
        let key = format!("{relation_identity}/{attnum}");
        let id = if let Some(id) = field_ids.get(&key) {
            *id
        } else {
            let id = next_id;
            next_id = next_id
                .checked_add(1)
                .context("Iceberg field id overflow")?;
            field_ids.insert(key.clone(), id);
            id
        };
        source_fields.push((key, id));
    }
    for ((_, field), (key, _)) in columns.iter().zip(&source_fields) {
        if let DataType::List(_) = field.data_type() {
            let element_key = format!("{key}/element");
            if let std::collections::btree_map::Entry::Vacant(entry) = field_ids.entry(element_key)
            {
                let id = next_id;
                next_id = next_id
                    .checked_add(1)
                    .context("Iceberg field id overflow")?;
                entry.insert(id);
            }
        }
    }
    let fields = columns
        .iter()
        .zip(source_fields)
        .map(|((_, field), (key, id))| {
            let mut output = field.clone().with_metadata({
                let mut metadata = field.metadata().clone();
                metadata.insert(PARQUET_FIELD_ID_KEY, id.to_string());
                metadata
            });
            if let DataType::List(element) = field.data_type() {
                let element_key = format!("{key}/element");
                let element_id = field_ids[&element_key];
                let mut element_metadata = element.metadata().clone();
                element_metadata.insert(PARQUET_FIELD_ID_KEY, element_id.to_string());
                output = output.with_data_type(DataType::List(Arc::new(
                    element.as_ref().clone().with_metadata(element_metadata),
                )));
            }
            Ok(output)
        })
        .collect::<Result<Vec<_>>>()?;
    let mut schema_metadata = std::collections::HashMap::new();
    schema_metadata.insert(
        "navigator.postgres.relation-identity".into(),
        relation_identity,
    );
    schema_metadata.insert(
        "navigator.postgres.field-ids".into(),
        serde_json::to_string(&field_ids)?,
    );
    Ok(Arc::new(Schema::new_with_metadata(fields, schema_metadata)))
}

fn get<'r, T>(row: &'r PgRow, index: usize, column: &Column) -> Result<Option<T>>
where
    T: sqlx::Decode<'r, Postgres> + sqlx::Type<Postgres>,
{
    row.try_get::<Option<T>, _>(index)
        .with_context(|| format!("decode column `{}`", column.name))
}

fn build_array(column: &Column, index: usize, rows: &[PgRow]) -> Result<ArrayRef> {
    macro_rules! primitive {
        ($builder:expr, $ty:ty) => {{
            let mut builder = $builder;
            for row in rows {
                builder.append_option(get::<$ty>(row, index, column)?);
            }
            Arc::new(builder.finish()) as ArrayRef
        }};
    }
    Ok(match column.kind {
        Kind::Text | Kind::Json | Kind::Numeric => primitive!(StringBuilder::new(), String),
        Kind::Bool => primitive!(BooleanBuilder::new(), bool),
        Kind::Int32 => primitive!(Int32Builder::new(), i32),
        Kind::Int64 => primitive!(Int64Builder::new(), i64),
        Kind::Float32 => primitive!(Float32Builder::new(), f32),
        Kind::Float64 => primitive!(Float64Builder::new(), f64),
        Kind::Bytes => primitive!(BinaryBuilder::new(), Vec<u8>),
        Kind::Uuid => {
            let mut builder = FixedSizeBinaryBuilder::new(16);
            for row in rows {
                match get::<uuid::Uuid>(row, index, column)? {
                    Some(value) => builder.append_value(value.as_bytes())?,
                    None => builder.append_null(),
                }
            }
            Arc::new(builder.finish())
        }
        Kind::Date => {
            let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("valid epoch");
            let mut builder = Date32Builder::new();
            for row in rows {
                let days = get::<NaiveDate>(row, index, column)?
                    .map(|date| i32::try_from((date - epoch).num_days()))
                    .transpose()
                    .context("date out of range")?;
                builder.append_option(days);
            }
            Arc::new(builder.finish())
        }
        Kind::Timestamptz => {
            let mut builder = TimestampMicrosecondBuilder::new().with_timezone("UTC");
            for row in rows {
                builder.append_option(
                    get::<DateTime<Utc>>(row, index, column)?.map(|at| at.timestamp_micros()),
                );
            }
            Arc::new(builder.finish())
        }
        Kind::Timestamp => {
            let mut builder = TimestampMicrosecondBuilder::new();
            for row in rows {
                builder.append_option(
                    get::<NaiveDateTime>(row, index, column)?
                        .map(|at| at.and_utc().timestamp_micros()),
                );
            }
            Arc::new(builder.finish())
        }
        Kind::TextArray => {
            let mut builder = ListBuilder::new(StringBuilder::new());
            for row in rows {
                match get::<Vec<Option<String>>>(row, index, column)? {
                    Some(values) => {
                        for value in values {
                            builder.values().append_option(value);
                        }
                        builder.append(true);
                    }
                    None => builder.append(false),
                }
            }
            Arc::new(builder.finish())
        }
    })
}

pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest.as_slice() {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{
        Array, AsArray, BinaryArray, FixedSizeBinaryArray, Int32Array, Int64Array, StringArray,
        TimestampMicrosecondArray,
    };
    use cloud::{FsStorage, ObjectListing, StorageError, StoredObject};
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use sqlx::postgres::{PgConnectOptions, PgSslMode};

    /// A disposable database holding the canonical schema, or `None` when no
    /// PostgreSQL test port is configured (and the lane does not require one).
    async fn postgres() -> Option<PgPool> {
        let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
            assert_ne!(
                std::env::var("NAV_REQUIRE_POSTGRES").as_deref(),
                Ok("1"),
                "PostgreSQL archive tests require NAVIGATOR_TEST_POSTGRES_PORT"
            );
            return None;
        };
        let options = PgConnectOptions::new()
            .host("127.0.0.1")
            .port(port.parse().unwrap())
            .username("postgres")
            .password("synthetic")
            .database(
                &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE")
                    .unwrap_or_else(|_| "smoke".into()),
            )
            .ssl_mode(PgSslMode::Disable);
        let admin = PgPool::connect_with(options.clone()).await.unwrap();
        let database = format!("archive_test_{}", uuid::Uuid::now_v7().simple());
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
        let pool = PgPool::connect_with(options.database(&database))
            .await
            .unwrap();
        sqlx::raw_sql(store::postgres_schema::canonical_schema())
            .execute(&pool)
            .await
            .unwrap();
        Some(pool)
    }

    async fn seed_brand(pool: &PgPool, key: &str) {
        sqlx::query("INSERT INTO brand (name, brand_key) VALUES ($1, $1)")
            .bind(key)
            .execute(pool)
            .await
            .unwrap();
    }

    fn parquet(bytes: Vec<u8>) -> RecordBatch {
        let reader = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(bytes))
            .unwrap()
            .build()
            .unwrap();
        let schema = arrow::array::RecordBatchReader::schema(&reader);
        let batches: Vec<RecordBatch> = reader.map(Result::unwrap).collect();
        arrow::compute::concat_batches(&schema, &batches).unwrap()
    }

    async fn object(storage: &dyn StorageService, key: &str) -> Vec<u8> {
        storage.get(key).await.unwrap().bytes
    }

    #[test]
    fn every_canonical_table_is_exported() {
        let declared =
            store::postgres_schema::parse_schema(store::postgres_schema::canonical_schema())
                .expect("canonical schema parses")
                .into_keys()
                .collect::<Vec<_>>();
        let mut exported = POSTGRES_TABLES
            .iter()
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>();
        exported.sort_unstable();
        assert_eq!(
            exported.len(),
            declared.len(),
            "no duplicate export entries"
        );
        assert_eq!(exported, declared);
    }

    #[test]
    fn every_supported_type_maps_and_unknown_types_fail() {
        assert_eq!(Kind::from_pg("uuid"), Some(Kind::Uuid));
        assert_eq!(Kind::from_pg("jsonb"), Some(Kind::Json));
        assert_eq!(Kind::from_pg("_text"), Some(Kind::TextArray));
        assert_eq!(Kind::from_pg("agtype"), Some(Kind::Text));
        assert_eq!(Kind::from_pg("graphid"), Some(Kind::Text));
        assert_eq!(Kind::from_pg("label_id"), Some(Kind::Text));
        assert_eq!(Kind::from_pg("label_kind"), Some(Kind::Text));
        assert_eq!(Kind::from_pg("vector"), None);
    }

    #[tokio::test]
    async fn an_unset_host_skips_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let storage = FsStorage::new(dir.path()).await.unwrap();
        for get in [
            &(|_: &str| None) as &dyn Fn(&str) -> Option<String>,
            &|key: &str| (key == "NAVIGATOR_POSTGRES_HOST").then(|| " ".to_string()),
        ] {
            assert!(export_configured(get, &storage, None).await.is_none());
        }
        assert!(storage.list("").await.unwrap().is_empty());
    }

    struct InsertBetween {
        pool: PgPool,
    }

    #[async_trait::async_trait]
    impl AfterTable for InsertBetween {
        async fn after(&self, table: &str) {
            if table == "brand" {
                // Committed after `brand` was read but before `post` is read.
                seed_brand(&self.pool, "late-brand").await;
                sqlx::query(
                    "INSERT INTO post (brand_key, slug, title, description, status) \
                     VALUES ('late-brand', 'late', 'Late', 'Synthetic', 'draft')",
                )
                .execute(&self.pool)
                .await
                .unwrap();
                sqlx::query("DELETE FROM post WHERE slug = 'early'")
                    .execute(&self.pool)
                    .await
                    .unwrap();
            }
        }
    }

    #[tokio::test]
    async fn every_table_shares_one_cutoff_despite_concurrent_writes() {
        let Some(pool) = postgres().await else { return };
        seed_brand(&pool, "sample-brand").await;
        sqlx::query(
            "INSERT INTO post (brand_key, slug, title, description, status) \
             VALUES ('sample-brand', 'early', 'Early', 'Synthetic', 'draft')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let storage = FsStorage::new(dir.path()).await.unwrap();
        let scheduled: DateTime<Utc> = "2026-10-12T05:55:00Z".parse().unwrap();
        let hook = InsertBetween { pool: pool.clone() };
        let generation =
            export_generation_with(&pool, &storage, &["brand", "post"], Some(scheduled), &hook)
                .await
                .unwrap();

        let rows: Vec<(&str, usize)> = generation
            .tables
            .iter()
            .map(|entry| (entry.table.as_str(), entry.rows))
            .collect();
        assert_eq!(rows, [("brand", 1), ("post", 1)], "the pre-cutoff state");
        let post = parquet(object(&storage, &generation.tables[1].key).await);
        let slug = post
            .column_by_name("slug")
            .unwrap()
            .as_any()
            .downcast_ref::<arrow::array::StringArray>()
            .unwrap();
        assert_eq!(slug.value(0), "early", "the later delete is not visible");

        let live: i64 = sqlx::query_scalar("SELECT count(*) FROM post")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(live, 1, "the concurrent insert and delete did commit");
        let late: i64 = sqlx::query_scalar("SELECT count(*) FROM post WHERE slug = 'late'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(late, 1);

        assert_eq!(
            generation.tables[0].key,
            format!(
                "postgres/navigator/brand/data/dt=2026-10-12/run=0555/snapshot-{}.parquet",
                generation.tables[0].sha256
            )
        );
        let manifest: PostgresGeneration = serde_json::from_slice(
            &object(
                &storage,
                "postgres/navigator/_generations/dt=2026-10-12/run=0555/generation.json",
            )
            .await,
        )
        .unwrap();
        assert_eq!(manifest, generation);
        for entry in &generation.tables {
            assert_eq!(
                sha256_hex(&object(&storage, &entry.key).await),
                entry.sha256
            );
        }
    }

    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "one synthetic row checks every supported type through Parquet and Iceberg"
    )]
    async fn postgres_types_round_trip_through_parquet_and_iceberg() {
        use futures::TryStreamExt;

        let Some(pool) = postgres().await else { return };
        sqlx::raw_sql(
            "CREATE TABLE archive_type_probe (
                id UUID PRIMARY KEY,
                at TIMESTAMPTZ,
                naive TIMESTAMP,
                day DATE,
                doc JSONB,
                tags TEXT[],
                amount NUMERIC,
                raw BYTEA,
                small SMALLINT,
                big BIGINT,
                ratio DOUBLE PRECISION,
                flag BOOLEAN,
                note VARCHAR(20)
            );
            INSERT INTO archive_type_probe VALUES (
                '7f0c1a2b-3c4d-4e5f-8a9b-0c1d2e3f4a5b',
                '2026-10-12 01:02:03.456789-07',
                '2026-10-12 01:02:03',
                '2026-10-12',
                '{\"b\": [1, 2], \"a\": null}',
                ARRAY['one', NULL, 'three'],
                170141183460469231731687303715884105727,
                '\\x00ff',
                -7,
                9007199254740993,
                0.25,
                true,
                'synthetic'
            ), ('00000000-0000-4000-8000-000000000000', NULL, NULL, NULL, NULL, NULL,
                NULL, NULL, NULL, NULL, NULL, NULL, NULL);",
        )
        .execute(&pool)
        .await
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let storage = FsStorage::new(dir.path()).await.unwrap();
        let reader_dir = tempfile::tempdir().unwrap();
        let reader_location = format!("file://{}", reader_dir.path().display());
        let first_slot = Utc::now()
            .date_naive()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc();
        let generation =
            export_generation(&pool, &storage, &["archive_type_probe"], Some(first_slot))
                .await
                .unwrap();
        let entry = &generation.tables[0];
        assert_eq!(entry.rows, 2);
        let original_bytes = object(&storage, &entry.key).await;
        let batch = parquet(original_bytes.clone());
        let schema = batch.schema();
        let field = |name: &str| schema.field_with_name(name).unwrap().clone();
        assert_eq!(field("id").data_type(), &DataType::FixedSizeBinary(16));
        assert_eq!(
            field("id")
                .metadata()
                .get(EXTENSION_NAME_KEY)
                .map(String::as_str),
            Some("arrow.uuid")
        );
        assert_eq!(
            field("at").data_type(),
            &DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into()))
        );
        assert_eq!(
            field("amount")
                .metadata()
                .get(POSTGRES_TYPE_KEY)
                .map(String::as_str),
            Some("numeric")
        );
        assert!(matches!(field("tags").data_type(), DataType::List(_)));

        // Ordered by primary key: the all-null row sorts first.
        let column = |name: &str| batch.column_by_name(name).unwrap().clone();
        let ids = column("id");
        let ids = ids.as_any().downcast_ref::<FixedSizeBinaryArray>().unwrap();
        assert_eq!(
            uuid::Uuid::from_slice(ids.value(1)).unwrap().to_string(),
            "7f0c1a2b-3c4d-4e5f-8a9b-0c1d2e3f4a5b"
        );
        let at = column("at");
        let at = at
            .as_any()
            .downcast_ref::<TimestampMicrosecondArray>()
            .unwrap();
        assert!(at.is_null(0));
        let expected: DateTime<Utc> = "2026-10-12T08:02:03.456789Z".parse().unwrap();
        assert_eq!(at.value(1), expected.timestamp_micros(), "UTC instant kept");
        let doc = column("doc");
        let doc: &StringArray = doc.as_string();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(doc.value(1)).unwrap(),
            serde_json::json!({"a": null, "b": [1, 2]})
        );
        let tags = column("tags");
        let tags = tags.as_list::<i32>().value(1);
        let tags: &StringArray = tags.as_string();
        assert_eq!(tags.len(), 3);
        assert!(tags.is_null(1));
        assert_eq!(tags.value(2), "three");
        let amount = column("amount");
        let amount: &StringArray = amount.as_string();
        assert_eq!(amount.value(1), "170141183460469231731687303715884105727");
        let raw = column("raw");
        let raw = raw.as_any().downcast_ref::<BinaryArray>().unwrap();
        assert_eq!(raw.value(1), [0x00, 0xff]);
        let small = column("small");
        assert_eq!(
            small
                .as_any()
                .downcast_ref::<Int32Array>()
                .unwrap()
                .value(1),
            -7
        );
        let big = column("big");
        assert_eq!(
            big.as_any().downcast_ref::<Int64Array>().unwrap().value(1),
            9_007_199_254_740_993
        );

        // The Iceberg schema carries the same types.
        let promoted = crate::author_iceberg_for_prefix(
            &storage,
            crate::PromotionRequest {
                lane: POSTGRES_LANE,
                table: "archive_type_probe",
                location_base: &reader_location,
                run_date: generation.cutoff_at.date_naive(),
                data_key: Some(&entry.key),
                snapshot_id: 1,
                timestamp_ms: generation.cutoff_at.timestamp_millis(),
                expire_before_ms: None,
            },
        )
        .await
        .unwrap()
        .unwrap();
        let metadata: iceberg::spec::TableMetadata = serde_json::from_slice(
            &object(
                &storage,
                &format!(
                    "{POSTGRES_LANE}/archive_type_probe/metadata/v{}.metadata.json",
                    promoted.version
                ),
            )
            .await,
        )
        .unwrap();
        let iceberg_schema = metadata.current_schema();
        let iceberg_type = |name: &str| {
            iceberg_schema
                .field_by_name(name)
                .unwrap()
                .field_type
                .to_string()
        };
        assert_eq!(iceberg_type("id"), "uuid");
        assert_eq!(iceberg_type("at"), "timestamptz");
        assert_eq!(iceberg_type("naive"), "timestamp");
        assert_eq!(iceberg_type("day"), "date");
        assert_eq!(iceberg_type("raw"), "binary");
        assert_eq!(iceberg_type("big"), "long");
        assert_eq!(iceberg_type("doc"), "string");
        let tags = iceberg_schema.field_by_name("tags").unwrap();
        let iceberg::spec::Type::List(list) = tags.field_type.as_ref() else {
            panic!("text[] must be an Iceberg list, got {}", tags.field_type);
        };
        assert_eq!(list.element_field.field_type.to_string(), "string");
        assert!(!list.element_field.required, "array elements may be null");

        let old_ids: std::collections::BTreeMap<_, _> = metadata
            .current_schema()
            .as_struct()
            .fields()
            .iter()
            .map(|field| (field.name.as_str(), field.id))
            .collect();
        let old_last_column_id = metadata.last_column_id();
        let old_tag_element_id = match metadata
            .current_schema()
            .field_by_name("tags")
            .unwrap()
            .field_type
            .as_ref()
        {
            iceberg::spec::Type::List(list) => list.element_field.id,
            other => panic!("tags must remain a list, got {other}"),
        };
        sqlx::raw_sql(
            "ALTER TABLE archive_type_probe RENAME COLUMN note TO caption;
             ALTER TABLE archive_type_probe DROP COLUMN small;
             ALTER TABLE archive_type_probe ADD COLUMN status text DEFAULT 'open';",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE archive_type_probe SET caption = 'renamed', status = 'closed'")
            .execute(&pool)
            .await
            .unwrap();
        let next = export_generation(&pool, &storage, &["archive_type_probe"], Some(first_slot))
            .await
            .unwrap();
        let next_entry = &next.tables[0];
        assert_ne!(
            entry.key, next_entry.key,
            "changed exports in the same scheduled slot need immutable data files"
        );
        assert_eq!(object(&storage, &entry.key).await, original_bytes);
        let next_batch = parquet(object(&storage, &next_entry.key).await);
        for field in next_batch.schema().fields() {
            assert!(
                field.metadata().get(PARQUET_FIELD_ID_KEY).is_some(),
                "physical ID missing for {}",
                field.name()
            );
        }
        let next_promoted = crate::author_iceberg_for_prefix(
            &storage,
            crate::PromotionRequest {
                lane: POSTGRES_LANE,
                table: "archive_type_probe",
                location_base: &reader_location,
                run_date: next.cutoff_at.date_naive(),
                data_key: Some(&next_entry.key),
                snapshot_id: 2,
                timestamp_ms: next.cutoff_at.timestamp_millis(),
                expire_before_ms: None,
            },
        )
        .await
        .unwrap()
        .unwrap();
        let latest_bytes = object(
            &storage,
            &format!(
                "{POSTGRES_LANE}/archive_type_probe/metadata/v{}.metadata.json",
                next_promoted.version
            ),
        )
        .await;
        let latest: iceberg::spec::TableMetadata = serde_json::from_slice(&latest_bytes).unwrap();
        let evolved = latest.current_schema();
        for name in [
            "id", "at", "naive", "day", "doc", "tags", "amount", "raw", "big", "ratio", "flag",
        ] {
            assert_eq!(
                evolved.field_by_name(name).unwrap().id,
                old_ids[name],
                "ID changed for {name}"
            );
        }
        assert_eq!(
            evolved.field_by_name("caption").unwrap().id,
            old_ids["note"]
        );
        let new_status_id = evolved.field_by_name("status").unwrap().id;
        assert!(new_status_id > old_last_column_id);
        let new_tag_element_id = match evolved.field_by_name("tags").unwrap().field_type.as_ref() {
            iceberg::spec::Type::List(list) => list.element_field.id,
            other => panic!("tags must remain a list, got {other}"),
        };
        assert_eq!(new_tag_element_id, old_tag_element_id);
        assert_eq!(
            next_batch
                .schema()
                .field_with_name("status")
                .unwrap()
                .metadata()
                .get(PARQUET_FIELD_ID_KEY)
                .unwrap()
                .parse::<i32>()
                .unwrap(),
            new_status_id
        );
        let arrow_tag_element_id = match next_batch
            .schema()
            .field_with_name("tags")
            .unwrap()
            .data_type()
        {
            DataType::List(element) => element
                .metadata()
                .get(PARQUET_FIELD_ID_KEY)
                .unwrap()
                .parse::<i32>()
                .unwrap(),
            other => panic!("tags must remain a list, got {other}"),
        };
        assert_eq!(arrow_tag_element_id, old_tag_element_id);
        let status_id = evolved.field_by_name("status").unwrap().id;
        assert!(status_id > old_ids.values().copied().max().unwrap());
        assert!(evolved.field_by_name("small").is_none());
        assert_eq!(
            latest.snapshots().count(),
            2,
            "the prior snapshot remains readable"
        );

        let indices: Vec<_> = (0..next_batch.num_columns()).rev().collect();
        let projected = next_batch.project(&indices).unwrap();
        let mut reordered_metadata = next_batch.schema().metadata().clone();
        reordered_metadata.insert(
            "navigator.postgres.relation-identity",
            latest.properties()["navigator.postgres.relation-identity"].clone(),
        );
        reordered_metadata.insert(
            "navigator.postgres.field-ids",
            latest.properties()["navigator.postgres.field-ids"].clone(),
        );
        let reordered_schema = Arc::new(Schema::new_with_metadata(
            projected
                .schema()
                .fields()
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            reordered_metadata,
        ));
        let reordered =
            RecordBatch::try_new(reordered_schema, projected.columns().to_vec()).unwrap();
        assert_eq!(reordered.schema().field(0).name(), "status");
        let mut reordered_bytes = Vec::new();
        let mut reordered_writer =
            ArrowWriter::try_new(&mut reordered_bytes, reordered.schema(), None).unwrap();
        reordered_writer.write(&reordered).unwrap();
        reordered_writer.close().unwrap();
        let reordered_key = format!(
            "{POSTGRES_LANE}/archive_type_probe/data/dt={}/run=1200/snapshot-{}.parquet",
            next.cutoff_at.date_naive(),
            sha256_hex(&reordered_bytes)
        );
        storage
            .put(
                &reordered_key,
                &reordered_bytes,
                "application/vnd.apache.parquet",
            )
            .await
            .unwrap();
        let reordered_promotion = crate::author_iceberg_for_prefix(
            &storage,
            crate::PromotionRequest {
                lane: POSTGRES_LANE,
                table: "archive_type_probe",
                location_base: &reader_location,
                run_date: next.cutoff_at.date_naive(),
                data_key: Some(&reordered_key),
                snapshot_id: 3,
                timestamp_ms: next.cutoff_at.timestamp_millis() + 1,
                expire_before_ms: None,
            },
        )
        .await
        .unwrap()
        .unwrap();

        // Exercise a reader independent of the authoring code against both
        // the old, current, and reordered snapshots, resolving Parquet IDs.
        for listed in storage.list("").await.unwrap() {
            let bytes = object(&storage, &listed.key).await;
            let path = reader_dir.path().join(&listed.key);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        let table = iceberg::table::StaticTable::from_metadata_file(
            &format!(
                "{reader_location}/{POSTGRES_LANE}/archive_type_probe/metadata/v{}.metadata.json",
                reordered_promotion.version
            ),
            iceberg::TableIdent::from_strs(["archive", "type_probe"]).unwrap(),
            iceberg::io::FileIO::new_with_fs(),
        )
        .await
        .unwrap();
        for snapshot_id in [1, 2, 3] {
            let tasks: Vec<_> = table
                .scan()
                .snapshot_id(snapshot_id)
                .build()
                .unwrap()
                .plan_files()
                .await
                .unwrap()
                .try_collect()
                .await
                .unwrap();
            assert_eq!(tasks.len(), 1);
            let task = &tasks[0];
            let path = task
                .data_file_path()
                .strip_prefix(&format!("{reader_location}/"))
                .unwrap();
            let actual_bytes = object(&storage, path).await;
            assert_eq!(task.file_size_in_bytes, actual_bytes.len() as u64);
            let batches: Vec<_> = table
                .reader_builder()
                .build()
                .read(Box::pin(futures::stream::iter(
                    tasks.into_iter().map(Ok::<_, iceberg::Error>),
                )))
                .unwrap()
                .stream()
                .try_collect()
                .await
                .unwrap_or_else(|error| panic!("read snapshot {snapshot_id}: {error:#}"));
            assert_eq!(
                batches
                    .iter()
                    .map(arrow_array58::RecordBatch::num_rows)
                    .sum::<usize>(),
                2
            );
            let schema = batches.first().unwrap().schema();
            if snapshot_id == 1 {
                assert!(schema.field_with_name("note").is_ok());
                assert!(schema.field_with_name("caption").is_err());
                let note = batches[0].column_by_name("note").unwrap();
                let note = note
                    .as_any()
                    .downcast_ref::<arrow_array58::StringArray>()
                    .unwrap();
                assert!(note.iter().flatten().any(|value| value == "synthetic"));
            } else {
                assert!(schema.field_with_name("caption").is_ok());
                assert!(schema.field_with_name("small").is_err());
                let caption = batches[0].column_by_name("caption").unwrap();
                let caption = caption
                    .as_any()
                    .downcast_ref::<arrow_array58::StringArray>()
                    .unwrap();
                assert!(caption.iter().flatten().any(|value| value == "renamed"));
                if snapshot_id == 3 {
                    let status = batches[0].column_by_name("status").unwrap();
                    let status = status
                        .as_any()
                        .downcast_ref::<arrow_array58::StringArray>()
                        .unwrap();
                    assert!(status.iter().flatten().any(|value| value == "closed"));
                }
            }
        }
    }

    #[tokio::test]
    async fn empty_tables_are_recorded_as_zero_row_files() {
        let Some(pool) = postgres().await else { return };
        let dir = tempfile::tempdir().unwrap();
        let storage = FsStorage::new(dir.path()).await.unwrap();
        let generation = export_generation(&pool, &storage, POSTGRES_TABLES, None)
            .await
            .unwrap();
        assert_eq!(generation.tables.len(), POSTGRES_TABLES.len());
        let exported = generation
            .tables
            .iter()
            .map(|entry| entry.table.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            exported.len(),
            POSTGRES_TABLES.len(),
            "one file per canonical table"
        );
        assert_eq!(exported, POSTGRES_TABLES.iter().copied().collect());
        for entry in &generation.tables {
            let batch = parquet(object(&storage, &entry.key).await);
            assert_eq!(
                batch.num_rows(),
                entry.rows,
                "row count for {}",
                entry.table
            );
        }
        for table in ["project_costs", "project_cost_events"] {
            let entry = generation
                .tables
                .iter()
                .find(|entry| entry.table == table)
                .expect("every canonical Project cost table is exported");
            assert_eq!(entry.rows, 0, "synthetic empty table {table}");
            assert_eq!(parquet(object(&storage, &entry.key).await).num_rows(), 0);
        }
        let journal = &generation.tables[0];
        assert_eq!((journal.table.as_str(), journal.rows), ("journal_entry", 0));
        let batch = parquet(object(&storage, &journal.key).await);
        assert_eq!(batch.num_rows(), 0);
        assert!(batch.schema().field_with_name("payload").is_ok());
    }

    #[tokio::test]
    async fn a_failed_table_publishes_nothing() {
        let Some(pool) = postgres().await else { return };
        seed_brand(&pool, "sample-brand").await;
        let dir = tempfile::tempdir().unwrap();
        let storage = FsStorage::new(dir.path()).await.unwrap();
        let error = export_generation(&pool, &storage, &["brand", "missing_table"], None)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("missing_table"), "{error:#}");
        assert!(
            storage.list("").await.unwrap().is_empty(),
            "no object written"
        );
    }

    /// Storage that refuses one key, standing in for an upload outage partway
    /// through publication.
    struct RefuseKey {
        inner: FsStorage,
        refused: &'static str,
    }

    #[async_trait::async_trait]
    impl StorageService for RefuseKey {
        async fn put(
            &self,
            key: &str,
            bytes: &[u8],
            content_type: &str,
        ) -> Result<(), StorageError> {
            if key.contains(self.refused) {
                return Err(StorageError::Unsupported("synthetic outage"));
            }
            self.inner.put(key, bytes, content_type).await
        }
        async fn get(&self, key: &str) -> Result<StoredObject, StorageError> {
            self.inner.get(key).await
        }
        async fn delete(&self, key: &str) -> Result<(), StorageError> {
            self.inner.delete(key).await
        }
        async fn list(&self, prefix: &str) -> Result<Vec<ObjectListing>, StorageError> {
            self.inner.list(prefix).await
        }
        async fn signed_url(
            &self,
            key: &str,
            ttl: std::time::Duration,
        ) -> Result<String, StorageError> {
            self.inner.signed_url(key, ttl).await
        }
    }

    #[tokio::test]
    async fn a_failed_upload_leaves_no_generation_manifest() {
        let Some(pool) = postgres().await else { return };
        seed_brand(&pool, "sample-brand").await;
        let dir = tempfile::tempdir().unwrap();
        let storage = RefuseKey {
            inner: FsStorage::new(dir.path()).await.unwrap(),
            refused: "/post/",
        };
        assert!(export_generation(&pool, &storage, &["brand", "post"], None)
            .await
            .is_err());
        assert!(storage
            .list("postgres/navigator/_generations/")
            .await
            .unwrap()
            .is_empty());
    }
}

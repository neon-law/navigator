//! Iceberg metadata writer — promotes the nightly Parquet snapshots into an
//! Apache Iceberg v2 table (metadata JSON + manifest list + manifest) so the
//! `<lane>/<table>/` prefix becomes a time-travelable, BigLake-readable table
//! rather than loose Parquet a reader has to glob. `lane` is `application`
//! for a `SurrealDB` entity table or `telemetry` for an `OTel` table — see
//! [`docs/iceberg-archive.md`](../../docs/iceberg-archive.md).
//!
//! ## Why this shape (the evaluation outcome, recorded)
//!
//! We author with the `iceberg` crate (0.10) for format correctness — a
//! subtly-wrong hand-rolled Avro manifest is a silently unreadable archive, and
//! this lake holds binding legal records. But two frictions shape the design:
//!
//! - **`iceberg` 0.10 targets arrow 58; the workspace is on arrow 60.** We never
//!   hand it an arrow `RecordBatch`; we derive the Iceberg [`Schema`] from the
//!   arrow-60 schema's fields ourselves ([`arrow_schema_to_iceberg`]), so the
//!   two arrow majors never meet at a type boundary.
//! - **Bytes must go through `cloud::StorageService`, never a GCS SDK**
//!   (AGENTS.md). `iceberg`'s manifest writers only write to their own
//!   `FileIO`, so we point them at an **in-memory** `FileIO`, read the bytes
//!   back, and hand them to the caller to persist. The final `gs://` object
//!   paths are passed as the in-memory output locations, so the paths embedded
//!   in the manifest list and snapshot are the real ones a reader resolves —
//!   the memory backend treats the path as an opaque key.
//!
//! ## v1 scope
//!
//! The table is **unpartitioned** in v1. The design's `dt=<date>` directory is
//! retained for the data files, but the Iceberg partition spec is unpartitioned
//! — point-in-time queries come from the **snapshot log** (each run appends a
//! snapshot), which the design names as the point-in-time mechanism anyway.
//! Identity-partitioning on `dt` is a deferred refinement; it needs partition
//! `Struct`/`Literal` values this writer omits today. Column bounds in the
//! manifest are likewise omitted (an optional read optimization, not required
//! for correctness).

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use arrow::datatypes::{DataType, Schema as ArrowSchema};
use chrono::NaiveDate;
use iceberg::io::FileIO;
use iceberg::spec::{
    DataContentType, DataFileBuilder, DataFileFormat, FormatVersion, ListType, ManifestListWriter,
    ManifestWriterBuilder, NestedField, Operation, PartitionSpec, PrimitiveType, Schema, SchemaRef,
    Snapshot, SnapshotReference, SnapshotRetention, SortOrder, Struct, Summary, TableMetadata,
    TableMetadataBuilder, Type, MAIN_BRANCH,
};
use uuid::Uuid;

const POSTGRES_RELATION_PROPERTY: &str = "navigator.postgres.relation-identity";
const POSTGRES_FIELD_IDS_PROPERTY: &str = "navigator.postgres.field-ids";
const PARQUET_FIELD_ID_KEY: &str = "PARQUET:field_id";

pub(crate) fn validate_postgres_relation_identity(relation: &str) -> Result<()> {
    let (database, database_oid, relation_oid, relfilenode): (String, String, String, String) =
        serde_json::from_str(relation).context("decode PostgreSQL relation identity")?;
    let positive_oid = |value: &str| value.parse::<u32>().is_ok_and(|oid| oid > 0);
    if database.trim().is_empty()
        || !positive_oid(&database_oid)
        || !positive_oid(&relation_oid)
        || relfilenode.parse::<u32>().is_err()
    {
        bail!("malformed PostgreSQL relation identity");
    }
    Ok(())
}

pub(crate) fn parse_postgres_field_mapping(
    relation: &str,
    mapping: &str,
) -> Result<std::collections::BTreeMap<String, i32>> {
    validate_postgres_relation_identity(relation)?;
    let field_ids: std::collections::BTreeMap<String, i32> =
        serde_json::from_str(mapping).context("decode PostgreSQL source field mapping")?;
    let mut ids = std::collections::HashSet::new();
    if field_ids.iter().any(|(key, id)| {
        let Some(attribute) = key.strip_prefix(&format!("{relation}/")) else {
            return true;
        };
        let (attnum, child) = attribute
            .split_once('/')
            .map_or((attribute, None), |(attnum, child)| (attnum, Some(child)));
        let valid_attnum = attnum
            .parse::<i16>()
            .is_ok_and(|number| number > 0 && number.to_string() == attnum);
        let valid_child = child.is_none_or(|child| child == "element");
        !valid_attnum || !valid_child || *id <= 0 || !ids.insert(*id)
    }) {
        bail!("malformed PostgreSQL source field mapping");
    }
    Ok(field_ids)
}

pub(crate) fn validate_postgres_metadata_mapping(
    metadata: &TableMetadata,
    relation: &str,
) -> Result<()> {
    let field_ids = parse_postgres_field_mapping(
        relation,
        metadata
            .properties()
            .get(POSTGRES_FIELD_IDS_PROPERTY)
            .context("prior PostgreSQL field mapping missing")?,
    )?;
    let mapped_ids: std::collections::HashSet<i32> = field_ids.values().copied().collect();
    let mut schema_ids = std::collections::HashSet::new();
    for schema in metadata.schemas_iter() {
        for field in schema.as_struct().fields() {
            schema_ids.insert(field.id);
            if let Type::List(list) = field.field_type.as_ref() {
                schema_ids.insert(list.element_field.id);
            }
        }
    }
    if field_ids.values().any(|id| *id > metadata.last_column_id())
        || !schema_ids.is_subset(&mapped_ids)
    {
        bail!("malformed or incomplete prior PostgreSQL field identity mapping");
    }
    Ok(())
}

/// One Parquet data file written this run, to be appended to the table as a
/// new snapshot. `path` is the final absolute object URI (e.g.
/// `gs://<bucket>/application/<table>/data/dt=2026-06-14/part-<uuid>.parquet`)
/// that a reader resolves — it is recorded verbatim in the manifest.
#[derive(Debug, Clone)]
pub struct DataFileSpec {
    pub path: String,
    pub record_count: u64,
    pub file_size_in_bytes: u64,
}

/// The prior run's table metadata, so the new snapshot chains onto the existing
/// snapshot log (append-only history) rather than starting fresh.
#[derive(Debug, Clone)]
pub struct PriorMetadata {
    /// The previous `v<N>.metadata.json` bytes.
    pub metadata_json: Vec<u8>,
    /// The absolute URI the previous metadata was stored at (recorded in the
    /// metadata log).
    pub location: String,
}

/// Inputs to author one nightly snapshot of one table. Timestamps / ids are
/// passed in (not read from the clock) so the workflow stays deterministic and
/// the unit tests are stable.
#[derive(Debug, Clone)]
pub struct SnapshotInput<'a> {
    /// SQL table name (the `iceberg/<table>/` prefix stem).
    pub table: &'a str,
    /// Arrow-59 schema of the snapshot's Parquet — mapped to the Iceberg schema.
    pub arrow_schema: &'a ArrowSchema,
    /// Absolute table location, e.g. `gs://<bucket>/iceberg/<table>` (no
    /// trailing slash). Metadata/manifest object URIs are built under it.
    pub table_location: &'a str,
    /// The data file(s) written this run.
    pub data_files: &'a [DataFileSpec],
    /// New snapshot id (unique per run).
    pub snapshot_id: i64,
    /// Iceberg sequence number for this snapshot (monotonic; `prior.seq + 1`).
    pub sequence_number: i64,
    /// Snapshot/metadata timestamp in epoch millis.
    pub timestamp_ms: i64,
    /// Metadata version `N` for this run (`v<N>.metadata.json`).
    pub version: u64,
    /// The previous run's metadata, to extend the snapshot log. `None` on the
    /// first run.
    pub prior: Option<PriorMetadata>,
    /// Snapshot-expiry cutoff (epoch millis): prior snapshots older than this
    /// are dropped from the log so it doesn't reference data files a GCS
    /// lifecycle rule has deleted. `Some` for the 30-day `otel_traces` /
    /// `otel_metrics` tables; `None` for the 10-year `otel_logs` table and the
    /// entity tables, which keep their full snapshot log (a deliberate
    /// divergence — see docs/iceberg-archive.md retention).
    pub expire_before_ms: Option<i64>,
}

/// The metadata objects to persist for one run, each as
/// `(path_relative_to_table_location, bytes)` — the caller prepends the table
/// prefix and writes them through `cloud::StorageService`.
#[derive(Debug)]
pub struct AuthoredMetadata {
    pub files: Vec<(String, Vec<u8>)>,
    /// The metadata version written (`N`), for `version-hint.text` and the next
    /// run's chaining.
    pub version: u64,
    /// Absolute URI of the `v<N>.metadata.json` written, to pass back as
    /// [`PriorMetadata::location`] next run.
    pub metadata_location: String,
}

/// Map a supported arrow-60 primitive without inventing a string type for
/// data whose physical Parquet representation an Iceberg reader cannot read.
fn arrow_primitive(dt: &DataType) -> Result<PrimitiveType> {
    Ok(match dt {
        DataType::Boolean => PrimitiveType::Boolean,
        DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::UInt8 | DataType::UInt16 => {
            PrimitiveType::Int
        }
        DataType::Int64 | DataType::UInt32 | DataType::UInt64 => PrimitiveType::Long,
        DataType::Float16 | DataType::Float32 => PrimitiveType::Float,
        DataType::Float64 => PrimitiveType::Double,
        DataType::Date32 | DataType::Date64 => PrimitiveType::Date,
        DataType::Timestamp(_, Some(_)) => PrimitiveType::Timestamptz,
        DataType::Timestamp(_, None) => PrimitiveType::Timestamp,
        DataType::Binary | DataType::LargeBinary | DataType::FixedSizeBinary(_) => {
            PrimitiveType::Binary
        }
        DataType::Decimal128(p, s) => PrimitiveType::Decimal {
            precision: u32::from(*p),
            scale: u32::from((*s).max(0).unsigned_abs()),
        },
        DataType::Utf8 | DataType::LargeUtf8 => PrimitiveType::String,
        _ => bail!("unsupported Arrow type for Iceberg metadata"),
    })
}

/// Map one arrow-60 field to an Iceberg [`Type`]. A list's element takes the
/// next free id from `next_id`; a 16-byte fixed binary tagged with the
/// `arrow.uuid` extension is an Iceberg `uuid`.
fn arrow_field_type(field: &arrow::datatypes::Field, next_id: &mut i32) -> Result<Type> {
    Ok(match field.data_type() {
        DataType::List(element) | DataType::LargeList(element) => {
            let id = element
                .metadata()
                .get(PARQUET_FIELD_ID_KEY)
                .and_then(|value| value.parse::<i32>().ok())
                .unwrap_or_else(|| {
                    let id = *next_id;
                    *next_id += 1;
                    id
                });
            Type::List(ListType::new(Arc::new(NestedField::list_element(
                id,
                arrow_field_type(element, next_id)?,
                !element.is_nullable(),
            ))))
        }
        DataType::FixedSizeBinary(16)
            if field
                .metadata()
                .get("ARROW:extension:name")
                .map(String::as_str)
                == Some("arrow.uuid") =>
        {
            Type::Primitive(PrimitiveType::Uuid)
        }
        data_type => Type::Primitive(arrow_primitive(data_type)?),
    })
}

/// Build an Iceberg [`Schema`] from an arrow-60 schema. PostgreSQL fields carry
/// explicit physical IDs; other sources receive top-level IDs by order with
/// nested list elements after them. Every field is `optional` because a full
/// snapshot can carry nulls. A new table's metadata builder reassigns IDs, so
/// PostgreSQL's initial mapping follows that builder's level-first allocation.
pub fn arrow_schema_to_iceberg(arrow: &ArrowSchema) -> Result<Schema> {
    let mut next_id = i32::try_from(arrow.fields().len() + 1)
        .context("schema has fewer than i32::MAX columns")?;
    let fields = arrow
        .fields()
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let default_id =
                i32::try_from(i + 1).context("schema has fewer than i32::MAX columns")?;
            let id = f
                .metadata()
                .get("PARQUET:field_id")
                .and_then(|v| v.parse::<i32>().ok())
                .unwrap_or(default_id);
            Ok(Arc::new(NestedField::optional(
                id,
                f.name(),
                arrow_field_type(f, &mut next_id)?,
            )))
        })
        .collect::<Result<Vec<_>>>()?;
    Schema::builder()
        .with_fields(fields)
        .with_schema_id(0)
        .build()
        .context("build iceberg schema from arrow schema")
}

/// Author the Iceberg metadata (manifest, manifest list, table metadata JSON,
/// version hint) for one nightly snapshot. All Avro is produced via an
/// in-memory `FileIO` and read back as bytes; nothing touches a real object
/// store here. The caller persists [`AuthoredMetadata::files`] through
/// `cloud::StorageService`.
#[expect(
    clippy::too_many_lines,
    reason = "one snapshot append must coordinate its manifest, snapshot, and metadata pointer"
)]
pub async fn author_snapshot(input: &SnapshotInput<'_>) -> Result<AuthoredMetadata> {
    let schema = arrow_schema_to_iceberg(input.arrow_schema)?;
    let pg_relation = input
        .arrow_schema
        .metadata()
        .get(POSTGRES_RELATION_PROPERTY)
        .cloned();
    let pg_mapping = input
        .arrow_schema
        .metadata()
        .get(POSTGRES_FIELD_IDS_PROPERTY)
        .cloned();
    let has_pg_field_identity = input.arrow_schema.fields().iter().any(|field| {
        field
            .metadata()
            .contains_key(crate::postgres::POSTGRES_RELATION_ID_KEY)
            || field
                .metadata()
                .contains_key(crate::postgres::POSTGRES_ATTRIBUTE_NUMBER_KEY)
    });
    let mut properties = HashMap::new();
    let pg_source = match (pg_relation, pg_mapping) {
        (Some(relation), Some(mapping)) => Some((relation, mapping)),
        (None, None) if !has_pg_field_identity => None,
        _ => bail!("incomplete PostgreSQL source identity metadata"),
    };
    if let Some((relation, mapping)) = pg_source {
        let field_ids = parse_postgres_field_mapping(&relation, &mapping)?;
        for field in input.arrow_schema.fields() {
            let field_relation = field
                .metadata()
                .get(crate::postgres::POSTGRES_RELATION_ID_KEY)
                .context("PostgreSQL field relation identity missing")?;
            let attnum = field
                .metadata()
                .get(crate::postgres::POSTGRES_ATTRIBUTE_NUMBER_KEY)
                .context("PostgreSQL field attribute identity missing")?;
            if field_relation != &relation {
                bail!("PostgreSQL schema mixes relation identities");
            }
            validate_postgres_relation_identity(field_relation)?;
            let attribute_number = attnum
                .parse::<i16>()
                .context("PostgreSQL field attribute identity is malformed")?;
            if attribute_number <= 0 || attribute_number.to_string() != *attnum {
                bail!("PostgreSQL field attribute identity is malformed");
            }
            let key = format!("{relation}/{attnum}");
            let expected = field_ids
                .get(&key)
                .context("PostgreSQL field ID mapping incomplete")?;
            let actual = field
                .metadata()
                .get(PARQUET_FIELD_ID_KEY)
                .and_then(|id| id.parse::<i32>().ok());
            if actual != Some(*expected) {
                bail!("Parquet field ID does not match PostgreSQL field mapping");
            }
            if let DataType::List(element) | DataType::LargeList(element) = field.data_type() {
                let expected = field_ids
                    .get(&format!("{key}/element"))
                    .context("PostgreSQL list element ID mapping incomplete")?;
                let actual = element
                    .metadata()
                    .get(PARQUET_FIELD_ID_KEY)
                    .and_then(|id| id.parse::<i32>().ok());
                if actual != Some(*expected) {
                    bail!("Parquet list element ID does not match PostgreSQL field mapping");
                }
            }
        }
        properties.insert(POSTGRES_RELATION_PROPERTY.to_string(), relation);
        properties.insert(POSTGRES_FIELD_IDS_PROPERTY.to_string(), mapping);
    }
    let mut schema_changed = false;
    if let Some(prior) = &input.prior {
        let previous: TableMetadata =
            serde_json::from_slice(&prior.metadata_json).context("decode prior table metadata")?;
        let prior_has_postgres_identity = previous
            .properties()
            .contains_key(POSTGRES_RELATION_PROPERTY)
            || previous
                .properties()
                .contains_key(POSTGRES_FIELD_IDS_PROPERTY);
        if prior_has_postgres_identity && !properties.contains_key(POSTGRES_RELATION_PROPERTY) {
            bail!("PostgreSQL source identity missing from continuing archive lineage");
        }
        if properties.contains_key(POSTGRES_RELATION_PROPERTY) {
            let relation = properties
                .get(POSTGRES_RELATION_PROPERTY)
                .context("PostgreSQL relation identity missing")?;
            let prior_relation = previous
                .properties()
                .get(POSTGRES_RELATION_PROPERTY)
                .context("prior PostgreSQL relation identity missing")?;
            validate_postgres_relation_identity(prior_relation)?;
            validate_postgres_metadata_mapping(&previous, prior_relation)?;
            if previous.properties().get(POSTGRES_RELATION_PROPERTY) != Some(relation) {
                bail!("PostgreSQL relation identity changed; start a new archive lineage");
            }
            for field in schema.as_struct().fields() {
                if let Some(old) = previous
                    .current_schema()
                    .as_struct()
                    .fields()
                    .iter()
                    .find(|old| old.id == field.id)
                {
                    if old.field_type.to_string() != field.field_type.to_string() {
                        bail!("schema evolution cannot change an existing PostgreSQL field type");
                    }
                }
            }
            schema_changed = previous.current_schema().as_struct() != schema.as_struct();
        } else if previous.current_schema().as_struct() != schema.as_struct() {
            bail!("schema evolution requires an explicit Iceberg field identity mapping");
        }
    }
    let schema_ref: SchemaRef = Arc::new(schema.clone());
    let spec = PartitionSpec::unpartition_spec();

    let io = FileIO::new_with_memory();

    // 1. Manifest — lists this run's data files. Written to a memory output at
    //    its FINAL absolute URI so the path the manifest list records is real.
    let manifest_name = format!("{}-m0.avro", Uuid::new_v4());
    let manifest_uri = format!("{}/metadata/{}", input.table_location, manifest_name);
    let manifest_out = io
        .new_output(&manifest_uri)
        .context("memory output for manifest")?;
    let mut mw = ManifestWriterBuilder::new(
        manifest_out,
        Some(input.snapshot_id),
        schema_ref.clone(),
        spec.clone(),
    )
    .build_v2_data();
    for df in input.data_files {
        let data_file = DataFileBuilder::default()
            .content(DataContentType::Data)
            .file_path(df.path.clone())
            .file_format(DataFileFormat::Parquet)
            .partition(Struct::empty())
            .record_count(df.record_count)
            .file_size_in_bytes(df.file_size_in_bytes)
            .build()
            .context("build iceberg DataFile")?;
        mw.add_file(data_file, input.sequence_number)
            .context("add data file to manifest")?;
    }
    let manifest_file = mw
        .write_manifest_file()
        .await
        .context("write manifest avro")?;
    let manifest_bytes = io
        .new_input(&manifest_uri)?
        .read()
        .await
        .context("read back manifest avro")?
        .to_vec();

    // 2. Manifest list — one entry, this run's manifest.
    let parent = input.prior.as_ref().and_then(|_| prior_snapshot_id(input));
    let list_name = format!("snap-{}-{}.avro", input.snapshot_id, Uuid::new_v4());
    let list_uri = format!("{}/metadata/{}", input.table_location, list_name);
    let list_out = io.new_output(&list_uri).context("memory output for list")?;
    let list_writer = list_out
        .writer()
        .await
        .context("open memory writer for manifest list")?;
    let mut mlw = ManifestListWriter::v2(
        list_writer,
        input.snapshot_id,
        parent,
        input.sequence_number,
    );
    mlw.add_manifests(std::iter::once(manifest_file))
        .context("add manifest to list")?;
    mlw.close().await.context("write manifest list avro")?;
    let list_bytes = io.new_input(&list_uri)?.read().await?.to_vec();

    // 3. Snapshot + table metadata (serde JSON — no FileIO needed).
    let summary = Summary {
        operation: Operation::Append,
        additional_properties: HashMap::new(),
    };
    let snapshot_schema_id = match &input.prior {
        Some(prior) if schema_changed => {
            let previous: TableMetadata = serde_json::from_slice(&prior.metadata_json)?;
            let matching_schema_id = previous
                .schemas_iter()
                .find(|existing| existing.as_struct() == schema.as_struct())
                .map(|existing| existing.schema_id());
            matching_schema_id.unwrap_or_else(|| {
                previous
                    .schemas_iter()
                    .map(|existing| existing.schema_id())
                    .max()
                    .unwrap_or(0)
                    + 1
            })
        }
        Some(prior) => {
            serde_json::from_slice::<TableMetadata>(&prior.metadata_json)?.current_schema_id()
        }
        // The initial TableMetadataBuilder schema has id zero, including when
        // it reassigns field IDs to the same level-first values we emitted.
        None => 0,
    };
    let snapshot = Snapshot::builder()
        .with_snapshot_id(input.snapshot_id)
        .with_parent_snapshot_id(parent)
        .with_sequence_number(input.sequence_number)
        .with_timestamp_ms(input.timestamp_ms)
        .with_manifest_list(list_uri.clone())
        .with_summary(summary)
        .with_schema_id(snapshot_schema_id)
        .build();

    let metadata = build_table_metadata(input, schema, spec, snapshot, properties, schema_changed)?;
    let metadata_json = serde_json::to_vec_pretty(&metadata).context("serialize metadata.json")?;

    let metadata_name = format!("v{}.metadata.json", input.version);
    let metadata_uri = format!("{}/metadata/{}", input.table_location, metadata_name);

    let files = vec![
        (format!("metadata/{manifest_name}"), manifest_bytes),
        (format!("metadata/{list_name}"), list_bytes),
        (format!("metadata/{metadata_name}"), metadata_json),
        (
            "metadata/version-hint.text".to_string(),
            input.version.to_string().into_bytes(),
        ),
    ];

    Ok(AuthoredMetadata {
        files,
        version: input.version,
        metadata_location: metadata_uri,
    })
}

/// Pull the current snapshot id out of the prior metadata, to set as this
/// snapshot's parent.
fn prior_snapshot_id(input: &SnapshotInput<'_>) -> Option<i64> {
    let prior = input.prior.as_ref()?;
    let meta: TableMetadata = serde_json::from_slice(&prior.metadata_json).ok()?;
    meta.current_snapshot_id()
}

/// Build the table metadata, chaining onto the prior snapshot log when a
/// previous run's metadata is supplied, else creating a fresh table.
fn build_table_metadata(
    input: &SnapshotInput<'_>,
    schema: Schema,
    spec: PartitionSpec,
    snapshot: Snapshot,
    properties: HashMap<String, String>,
    schema_changed: bool,
) -> Result<TableMetadata> {
    let snapshot_id = input.snapshot_id;
    let reference =
        SnapshotReference::new(snapshot_id, SnapshotRetention::branch(None, None, None));

    let builder = if let Some(prior) = &input.prior {
        let previous: TableMetadata =
            serde_json::from_slice(&prior.metadata_json).context("parse prior metadata.json")?;
        // Snapshots from the prior log older than the cutoff are expired so
        // the metadata never references data files a GCS lifecycle rule has
        // already deleted. The new snapshot (added below, and the only one
        // `main` will point at) is never in this set.
        let to_expire: Vec<i64> = match input.expire_before_ms {
            Some(cutoff) => previous
                .snapshots()
                .filter(|s| s.timestamp_ms() < cutoff)
                .map(|s| s.snapshot_id())
                .collect(),
            None => Vec::new(),
        };
        let mut b = TableMetadataBuilder::new_from_metadata(previous, Some(prior.location.clone()));
        if schema_changed {
            b = b
                .add_current_schema(schema)
                .context("add evolved Iceberg schema")?;
        }
        let b = b
            .set_properties(properties)
            .context("set table properties")?
            .add_snapshot(snapshot)
            .context("add snapshot")?
            .set_ref(MAIN_BRANCH, reference)
            .context("set main ref")?;
        if to_expire.is_empty() {
            b
        } else {
            b.remove_snapshots(&to_expire)
        }
    } else {
        let b = TableMetadataBuilder::new(
            schema.clone(),
            spec.into_unbound(),
            SortOrder::unsorted_order(),
            input.table_location.to_string(),
            FormatVersion::V2,
            properties,
        )
        .context("new table metadata")?;
        b.add_snapshot(snapshot)
            .context("add snapshot")?
            .set_ref(MAIN_BRANCH, reference)
            .context("set main ref")?
    };

    let result = builder.build().context("build table metadata")?;
    Ok(result.metadata)
}

// ---------------------------------------------------------------------------
// File-sourced authoring — author Iceberg metadata over Parquet that already
// lives in object storage: the telemetry lake (the OTel collector writes
// `telemetry/otel_{logs,traces,metrics}/data/dt=<date>/*.parquet`) and the
// entity-table snapshots (`application/<table>/data/dt=<date>/*.parquet`,
// written by `archives::runner::snapshot_all`). Reads the day's data files
// through `cloud::StorageService` — never a GCS SDK — derives the schema and
// row counts from the Parquet footers, chains onto the prior metadata, and
// persists the new metadata objects back through the same trait.
// ---------------------------------------------------------------------------

use cloud::{StorageError, StorageService};
use parquet::file::reader::{FileReader, SerializedFileReader};

/// Read the Arrow schema from Parquet bytes (footer only — no row decode).
fn parquet_arrow_schema(bytes: &[u8]) -> Result<ArrowSchema> {
    let reader = SerializedFileReader::new(bytes::Bytes::copy_from_slice(bytes))
        .context("open parquet for schema")?;
    let fm = reader.metadata().file_metadata();
    parquet::arrow::parquet_to_arrow_schema(fm.schema_descr(), fm.key_value_metadata())
        .context("parquet schema -> arrow schema")
}

/// Read the row count from a Parquet footer (no row decode).
fn parquet_num_rows(bytes: &[u8]) -> Result<u64> {
    let reader = SerializedFileReader::new(bytes::Bytes::copy_from_slice(bytes))
        .context("open parquet for row count")?;
    u64::try_from(reader.metadata().file_metadata().num_rows()).context("negative row count")
}

/// Content type for a persisted metadata object, by suffix. Keys are
/// lowercase strings we generate, so a case-sensitive suffix match is exact.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn content_type_for(rel: &str) -> &'static str {
    if rel.ends_with(".json") {
        "application/json"
    } else if rel.ends_with(".avro") {
        "application/avro"
    } else {
        "text/plain"
    }
}

/// Load the prior run's metadata for `table_prefix` via the
/// `version-hint.text` pointer. Returns `(prior, N)` or `None` on first run.
async fn read_prior(
    storage: &dyn StorageService,
    table_prefix: &str,
    location_base: &str,
) -> Result<Option<(PriorMetadata, u64)>> {
    let Some((bytes, n)) = read_prior_metadata(storage, table_prefix).await? else {
        return Ok(None);
    };
    Ok(Some((
        PriorMetadata {
            metadata_json: bytes,
            location: format!("{location_base}/{table_prefix}/metadata/v{n}.metadata.json"),
        },
        n,
    )))
}

/// Read prior metadata bytes by table prefix for exporters that must know
/// field IDs before writing the next Parquet file.
pub(crate) async fn read_prior_metadata(
    storage: &dyn StorageService,
    table_prefix: &str,
) -> Result<Option<(Vec<u8>, u64)>> {
    let hint_key = format!("{table_prefix}/metadata/version-hint.text");
    let hint = match storage.get(&hint_key).await {
        Ok(o) => o,
        Err(StorageError::NotFound(_)) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let n: u64 = String::from_utf8_lossy(&hint.bytes)
        .trim()
        .parse()
        .context("parse version-hint.text")?;
    let meta_key = format!("{table_prefix}/metadata/v{n}.metadata.json");
    let meta = storage.get(&meta_key).await?;
    Ok(Some((meta.bytes, n)))
}

/// Inputs to [`author_iceberg_for_prefix`] — one table's day of Parquet
/// under its lane prefix. Grouped into a struct (rather than a long argument
/// list) because every field is a distinct scalar an unlabeled positional
/// argument would be easy to transpose.
#[derive(Debug, Clone, Copy)]
pub struct PromotionRequest<'a> {
    /// [`crate::snapshot::APPLICATION_LANE`] for a `SurrealDB` entity table
    /// or [`crate::snapshot::TELEMETRY_LANE`] for an `OTel` table — the two
    /// lanes never share a prefix, so a table name that happens to collide
    /// between them still resolves to distinct storage keys.
    pub lane: &'a str,
    pub table: &'a str,
    /// The absolute store base a reader resolves (e.g.
    /// `gs://<project>-exports`); it prefixes every path recorded in the
    /// manifest.
    pub location_base: &'a str,
    pub run_date: NaiveDate,
    /// Select one full application snapshot; telemetry still uses the whole day.
    pub data_key: Option<&'a str>,
    /// Passed in rather than read from the clock — the caller stamps it
    /// inside the journaled workflow step, so a replay reuses the cached
    /// result.
    pub snapshot_id: i64,
    pub timestamp_ms: i64,
    pub expire_before_ms: Option<i64>,
}

/// Author (and persist) Iceberg metadata for one table whose day's Parquet
/// data files already live under `<lane>/<table>/data/dt=<run_date>/`.
/// Returns `None` when there are no data files for the date (a clean no-op —
/// e.g. a day the collector wrote nothing, or before the OTLP->Parquet shim
/// exists).
pub async fn author_iceberg_for_prefix(
    storage: &dyn StorageService,
    request: PromotionRequest<'_>,
) -> Result<Option<AuthoredMetadata>> {
    let PromotionRequest {
        lane,
        table,
        location_base,
        run_date,
        data_key,
        snapshot_id,
        timestamp_ms,
        expire_before_ms,
    } = request;
    let table_prefix = format!("{lane}/{table}");
    let data_prefix = format!("{table_prefix}/data/dt={run_date}/");

    let mut objects: Vec<_> = storage
        .list(&data_prefix)
        .await
        .with_context(|| format!("list {data_prefix}"))?
        .into_iter()
        .filter(|o| o.key.ends_with(".parquet") && data_key.is_none_or(|key| o.key == key))
        .collect();
    if objects.is_empty() {
        anyhow::ensure!(data_key.is_none(), "journaled snapshot data is missing");
        return Ok(None);
    }
    // Deterministic order so the manifest is stable across replays.
    objects.sort_by(|a, b| a.key.cmp(&b.key));

    // Every selected file must share one schema before metadata is authored.
    let first = storage.get(&objects[0].key).await?;
    let arrow_schema = parquet_arrow_schema(&first.bytes)?;

    let prior = read_prior(storage, &table_prefix, location_base).await?;
    if let Some((metadata, version)) = &prior {
        let table_metadata: TableMetadata = serde_json::from_slice(&metadata.metadata_json)
            .context("parse prior Iceberg metadata for replay detection")?;
        if table_metadata.snapshot_by_id(snapshot_id).is_some() {
            return Ok(Some(AuthoredMetadata {
                files: Vec::new(),
                version: *version,
                metadata_location: metadata.location.clone(),
            }));
        }
    }
    let version = prior.as_ref().map_or(1, |(_, n)| n + 1);
    let sequence_number = i64::try_from(version).context("version exceeds i64")?;
    let prior_meta = prior.map(|(m, _)| m);

    let mut data_files = Vec::with_capacity(objects.len());
    for (i, obj) in objects.iter().enumerate() {
        let bytes = if i == 0 {
            first.bytes.clone()
        } else {
            storage.get(&obj.key).await?.bytes
        };
        if parquet_arrow_schema(&bytes)? != arrow_schema {
            bail!("selected Parquet schemas differ; refusing Iceberg publication");
        }
        data_files.push(DataFileSpec {
            path: format!("{location_base}/{}", obj.key),
            record_count: parquet_num_rows(&bytes)?,
            file_size_in_bytes: obj.size_bytes,
        });
    }

    let table_location = format!("{location_base}/{table_prefix}");
    let authored = author_snapshot(&SnapshotInput {
        table,
        arrow_schema: &arrow_schema,
        table_location: &table_location,
        data_files: &data_files,
        snapshot_id,
        sequence_number,
        timestamp_ms,
        version,
        prior: prior_meta,
        expire_before_ms,
    })
    .await?;

    for (rel, bytes) in &authored.files {
        let key = format!("{table_prefix}/{rel}");
        storage
            .put(&key, bytes, content_type_for(rel))
            .await
            .with_context(|| format!("persist {key}"))?;
    }
    Ok(Some(authored))
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::datatypes::Field;

    fn sample_arrow_schema() -> ArrowSchema {
        ArrowSchema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new("rows", DataType::Int64, true),
            Field::new("active", DataType::Boolean, true),
        ])
    }

    #[test]
    fn unsupported_arrow_types_are_refused_instead_of_declared_as_strings() {
        for data_type in [
            DataType::Null,
            DataType::Duration(arrow::datatypes::TimeUnit::Microsecond),
            DataType::Struct(vec![Field::new("nested", DataType::Utf8, true)].into()),
        ] {
            let schema = ArrowSchema::new(vec![Field::new("value", data_type, true)]);
            let error = arrow_schema_to_iceberg(&schema).unwrap_err();
            assert!(
                error.to_string().contains("unsupported Arrow type"),
                "{error:#}"
            );
        }
    }

    #[test]
    fn arrow_schema_maps_types_and_assigns_ids() {
        let schema = arrow_schema_to_iceberg(&sample_arrow_schema()).unwrap();
        assert_eq!(schema.as_struct().fields().len(), 3);
        // Field ids are 1..=N in column order.
        let ids: Vec<i32> = schema.as_struct().fields().iter().map(|f| f.id).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    fn input<'a>(arrow: &'a ArrowSchema, files: &'a [DataFileSpec]) -> SnapshotInput<'a> {
        SnapshotInput {
            table: "persons",
            arrow_schema: arrow,
            table_location: "gs://proj-exports/iceberg/persons",
            data_files: files,
            snapshot_id: 1001,
            sequence_number: 1,
            timestamp_ms: 1_700_000_000_000,
            version: 1,
            prior: None,
            expire_before_ms: None,
        }
    }

    fn postgres_arrow_schema(relation: &str, mapping: &str, field_id: &str) -> ArrowSchema {
        postgres_arrow_schema_named(relation, mapping, field_id, "id")
    }

    fn postgres_arrow_schema_named(
        relation: &str,
        mapping: &str,
        field_id: &str,
        name: &str,
    ) -> ArrowSchema {
        postgres_arrow_schema_typed(relation, mapping, field_id, name, DataType::Utf8)
    }

    fn postgres_arrow_schema_typed(
        relation: &str,
        mapping: &str,
        field_id: &str,
        name: &str,
        data_type: DataType,
    ) -> ArrowSchema {
        let mut field_metadata = HashMap::new();
        field_metadata.insert(
            crate::postgres::POSTGRES_RELATION_ID_KEY.to_string(),
            relation.into(),
        );
        field_metadata.insert(
            crate::postgres::POSTGRES_ATTRIBUTE_NUMBER_KEY.to_string(),
            "1".into(),
        );
        field_metadata.insert(PARQUET_FIELD_ID_KEY.to_string(), field_id.into());
        let mut schema_metadata = HashMap::new();
        schema_metadata.insert(POSTGRES_RELATION_PROPERTY.to_string(), relation.into());
        schema_metadata.insert(POSTGRES_FIELD_IDS_PROPERTY.to_string(), mapping.into());
        ArrowSchema::new_with_metadata(
            vec![Field::new(name, data_type, false).with_metadata(field_metadata)],
            schema_metadata,
        )
    }

    fn postgres_field(relation: &str, attnum: &str, name: &str, field_id: &str) -> Field {
        let mut metadata = HashMap::new();
        metadata.insert(
            crate::postgres::POSTGRES_RELATION_ID_KEY.to_string(),
            relation.into(),
        );
        metadata.insert(
            crate::postgres::POSTGRES_ATTRIBUTE_NUMBER_KEY.to_string(),
            attnum.into(),
        );
        metadata.insert(PARQUET_FIELD_ID_KEY.to_string(), field_id.into());
        Field::new(name, DataType::Utf8, true).with_metadata(metadata)
    }

    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "one guard regression covers identity, mapping, and retype refusal"
    )]
    async fn postgres_field_identity_guards_fail_closed_and_initial_ids_match_snapshot() {
        let relation = r#"["navigator","16403","24576","901"]"#;
        let mapping =
            serde_json::to_string(&HashMap::from([(format!("{relation}/1"), 1)])).unwrap();
        let arrow = postgres_arrow_schema(relation, &mapping, "1");
        let first = author_snapshot(&input(&arrow, &[])).await.unwrap();
        let first_json = first
            .files
            .iter()
            .find(|(name, _)| name == "metadata/v1.metadata.json")
            .unwrap()
            .1
            .clone();
        let metadata: TableMetadata = serde_json::from_slice(&first_json).unwrap();
        assert_eq!(
            metadata.snapshot_by_id(1001).unwrap().schema_id(),
            Some(metadata.current_schema_id()),
            "the first snapshot must reference the schema that carries physical IDs"
        );
        let prior = PriorMetadata {
            metadata_json: first_json.clone(),
            location: first.metadata_location.clone(),
        };

        let renamed = postgres_arrow_schema_named(relation, &mapping, "1", "caption");
        let mut evolved = input(&renamed, &[]);
        evolved.snapshot_id = 1002;
        evolved.sequence_number = 2;
        evolved.version = 2;
        evolved.prior = Some(prior.clone());
        let second = author_snapshot(&evolved).await.unwrap();
        let second_json = second
            .files
            .iter()
            .find(|(name, _)| name == "metadata/v2.metadata.json")
            .unwrap()
            .1
            .clone();
        let second_metadata: TableMetadata = serde_json::from_slice(&second_json).unwrap();
        assert_eq!(
            second_metadata.snapshot_by_id(1002).unwrap().schema_id(),
            Some(1)
        );

        let mut reverted = input(&arrow, &[]);
        reverted.snapshot_id = 1003;
        reverted.sequence_number = 3;
        reverted.version = 3;
        reverted.prior = Some(PriorMetadata {
            metadata_json: second_json,
            location: second.metadata_location,
        });
        let third = author_snapshot(&reverted).await.unwrap();
        let third_json = third
            .files
            .iter()
            .find(|(name, _)| name == "metadata/v3.metadata.json")
            .unwrap()
            .1
            .clone();
        let third_metadata: TableMetadata = serde_json::from_slice(&third_json).unwrap();
        assert_eq!(third_metadata.current_schema_id(), 0);
        assert_eq!(
            third_metadata.snapshot_by_id(1003).unwrap().schema_id(),
            Some(0)
        );

        let changed_relation = r#"["navigator","16403","24576","902"]"#;
        let changed_mapping =
            serde_json::to_string(&HashMap::from([(format!("{changed_relation}/1"), 1)])).unwrap();
        let changed = postgres_arrow_schema(changed_relation, &changed_mapping, "1");
        let mut next = input(&changed, &[]);
        next.prior = Some(prior.clone());
        let error = author_snapshot(&next).await.unwrap_err();
        assert!(
            error.to_string().contains("relation identity changed"),
            "{error:#}"
        );

        let missing = postgres_arrow_schema(relation, "{}", "1");
        let error = author_snapshot(&input(&missing, &[])).await.unwrap_err();
        assert!(
            error.to_string().contains("field ID mapping incomplete"),
            "{error:#}"
        );

        let wrong_id = postgres_arrow_schema(relation, &mapping, "2");
        let mut next = input(&wrong_id, &[]);
        next.prior = Some(prior.clone());
        let error = author_snapshot(&next).await.unwrap_err();
        assert!(error.to_string().contains("Parquet field ID"), "{error:#}");

        let mut malformed_prior: serde_json::Value = serde_json::from_slice(&first_json).unwrap();
        malformed_prior["properties"][POSTGRES_FIELD_IDS_PROPERTY] = serde_json::json!("{}");
        let mut next = input(&renamed, &[]);
        next.prior = Some(PriorMetadata {
            metadata_json: serde_json::to_vec(&malformed_prior).unwrap(),
            location: prior.location.clone(),
        });
        let error = author_snapshot(&next).await.unwrap_err();
        assert!(
            error.to_string().contains("prior PostgreSQL field mapping")
                || error
                    .to_string()
                    .contains("prior PostgreSQL field identity mapping"),
            "{error:#}"
        );

        let mut missing_relation_prior: serde_json::Value =
            serde_json::from_slice(&first_json).unwrap();
        missing_relation_prior["properties"]
            .as_object_mut()
            .unwrap()
            .remove(POSTGRES_RELATION_PROPERTY);
        let mut next = input(&renamed, &[]);
        next.prior = Some(PriorMetadata {
            metadata_json: serde_json::to_vec(&missing_relation_prior).unwrap(),
            location: prior.location.clone(),
        });
        let error = author_snapshot(&next).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("prior PostgreSQL relation identity missing"),
            "{error:#}"
        );

        let retyped = postgres_arrow_schema_typed(relation, &mapping, "1", "id", DataType::Boolean);
        let mut next = input(&retyped, &[]);
        next.prior = Some(prior.clone());
        let error = author_snapshot(&next).await.unwrap_err();
        assert!(
            error.to_string().contains("existing PostgreSQL field type"),
            "{error:#}"
        );

        let malformed_id = postgres_arrow_schema(relation, &mapping, "zero");
        let error = author_snapshot(&input(&malformed_id, &[]))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Parquet field ID"), "{error:#}");

        for missing_property in [POSTGRES_RELATION_PROPERTY, POSTGRES_FIELD_IDS_PROPERTY] {
            let mut schema_metadata = arrow.metadata().clone();
            schema_metadata.remove(missing_property);
            let incomplete = ArrowSchema::new_with_metadata(
                arrow.fields().iter().cloned().collect::<Vec<_>>(),
                schema_metadata,
            );
            let error = author_snapshot(&input(&incomplete, &[])).await.unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("incomplete PostgreSQL source identity"),
                "{error:#}"
            );
        }

        let stripped_fields: Vec<_> = arrow
            .fields()
            .iter()
            .map(|field| {
                let mut metadata = field.metadata().clone();
                metadata.remove(crate::postgres::POSTGRES_RELATION_ID_KEY);
                metadata.remove(crate::postgres::POSTGRES_ATTRIBUTE_NUMBER_KEY);
                field.as_ref().clone().with_metadata(metadata)
            })
            .collect();
        let mut stripped_metadata = arrow.metadata().clone();
        stripped_metadata.remove(POSTGRES_RELATION_PROPERTY);
        stripped_metadata.remove(POSTGRES_FIELD_IDS_PROPERTY);
        let stripped = ArrowSchema::new_with_metadata(stripped_fields, stripped_metadata);
        let mut next = input(&stripped, &[]);
        next.prior = Some(prior.clone());
        let error = author_snapshot(&next).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("PostgreSQL source identity missing"),
            "{error:#}"
        );

        let mut invalid_attribute_field = arrow.field(0).clone();
        let mut invalid_attribute_metadata = invalid_attribute_field.metadata().clone();
        invalid_attribute_metadata.insert(
            crate::postgres::POSTGRES_ATTRIBUTE_NUMBER_KEY.to_string(),
            "0",
        );
        invalid_attribute_field = invalid_attribute_field.with_metadata(invalid_attribute_metadata);
        let invalid_attribute =
            ArrowSchema::new_with_metadata(vec![invalid_attribute_field], arrow.metadata().clone());
        let error = author_snapshot(&input(&invalid_attribute, &[]))
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("attribute identity is malformed"),
            "{error:#}"
        );

        let malformed_relation = postgres_arrow_schema("not-json", "{}", "1");
        let error = author_snapshot(&input(&malformed_relation, &[]))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("relation identity"), "{error:#}");

        let element_metadata =
            HashMap::from([(PARQUET_FIELD_ID_KEY.to_string(), "not-an-id".into())]);
        let mut tag_metadata = HashMap::new();
        tag_metadata.insert(
            crate::postgres::POSTGRES_RELATION_ID_KEY.to_string(),
            relation.into(),
        );
        tag_metadata.insert(
            crate::postgres::POSTGRES_ATTRIBUTE_NUMBER_KEY.to_string(),
            "1".into(),
        );
        tag_metadata.insert(PARQUET_FIELD_ID_KEY.to_string(), "1".into());
        let element = Field::new_list_field(DataType::Utf8, true).with_metadata(element_metadata);
        let tags =
            Field::new("tags", DataType::List(Arc::new(element)), true).with_metadata(tag_metadata);
        let list_mapping = serde_json::to_string(&HashMap::from([
            (format!("{relation}/1"), 1),
            (format!("{relation}/1/element"), 2),
        ]))
        .unwrap();
        let list_arrow = ArrowSchema::new_with_metadata(
            vec![tags],
            HashMap::from([
                (POSTGRES_RELATION_PROPERTY.to_string(), relation.into()),
                (POSTGRES_FIELD_IDS_PROPERTY.to_string(), list_mapping),
            ]),
        );
        let error = author_snapshot(&input(&list_arrow, &[])).await.unwrap_err();
        assert!(error.to_string().contains("list element ID"), "{error:#}");
    }

    #[tokio::test]
    async fn postgres_field_ids_survive_arrow_column_reordering() {
        let relation = r#"["navigator","16403","24576","901"]"#;
        let mapping = serde_json::to_string(&HashMap::from([
            (format!("{relation}/1"), 1),
            (format!("{relation}/2"), 2),
        ]))
        .unwrap();
        let make_schema = |fields| {
            ArrowSchema::new_with_metadata(
                fields,
                HashMap::from([
                    (POSTGRES_RELATION_PROPERTY.to_string(), relation.into()),
                    (POSTGRES_FIELD_IDS_PROPERTY.to_string(), mapping.clone()),
                ]),
            )
        };
        let first_arrow = make_schema(vec![
            postgres_field(relation, "1", "left", "1"),
            postgres_field(relation, "2", "right", "2"),
        ]);
        let first = author_snapshot(&input(&first_arrow, &[])).await.unwrap();
        let first_json = first
            .files
            .iter()
            .find(|(name, _)| name == "metadata/v1.metadata.json")
            .unwrap()
            .1
            .clone();
        let second_arrow = make_schema(vec![
            postgres_field(relation, "2", "right", "2"),
            postgres_field(relation, "1", "left", "1"),
        ]);
        let mut next = input(&second_arrow, &[]);
        next.snapshot_id = 1002;
        next.sequence_number = 2;
        next.version = 2;
        next.prior = Some(PriorMetadata {
            metadata_json: first_json,
            location: first.metadata_location,
        });
        let second = author_snapshot(&next).await.unwrap();
        let metadata_json = second
            .files
            .iter()
            .find(|(name, _)| name == "metadata/v2.metadata.json")
            .unwrap()
            .1
            .clone();
        let metadata: TableMetadata = serde_json::from_slice(&metadata_json).unwrap();
        let fields = metadata.current_schema().as_struct().fields();
        assert_eq!(fields[0].name, "right");
        assert_eq!(fields[0].id, 2);
        assert_eq!(fields[1].name, "left");
        assert_eq!(fields[1].id, 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn authors_a_valid_iceberg_table_whose_metadata_round_trips() {
        let arrow = sample_arrow_schema();
        let files = vec![DataFileSpec {
            path: "gs://proj-exports/iceberg/persons/data/dt=2026-06-14/part-abc.parquet"
                .to_string(),
            record_count: 42,
            file_size_in_bytes: 4096,
        }];
        let authored = author_snapshot(&input(&arrow, &files)).await.unwrap();

        // Four objects: manifest, manifest list, metadata.json, version-hint.
        assert_eq!(authored.files.len(), 4);
        assert_eq!(authored.version, 1);
        let names: Vec<&str> = authored.files.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.iter().any(|n| n.ends_with("-m0.avro")));
        assert!(names.iter().any(|n| n.contains("snap-1001-")));
        assert!(names.contains(&"metadata/v1.metadata.json"));
        assert!(names.contains(&"metadata/version-hint.text"));

        // metadata.json round-trips back into a real iceberg TableMetadata with
        // our one snapshot current and the schema's three columns — the
        // strongest offline correctness signal short of a live BigLake read.
        let meta_bytes = &authored
            .files
            .iter()
            .find(|(n, _)| n == "metadata/v1.metadata.json")
            .unwrap()
            .1;
        let meta: TableMetadata = serde_json::from_slice(meta_bytes).unwrap();
        assert_eq!(meta.current_snapshot_id(), Some(1001));
        assert_eq!(meta.current_schema().as_struct().fields().len(), 3);
        // The snapshot points at the manifest-list URI we wrote.
        let snap = meta.snapshot_by_id(1001).unwrap();
        assert!(snap.manifest_list().contains("/metadata/snap-1001-"));

        // Avro bytes are non-empty.
        for (name, bytes) in &authored.files {
            assert!(!bytes.is_empty(), "{name} should not be empty");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn file_sourced_arm_authors_over_parquet_in_storage_and_no_ops_when_empty() {
        use arrow::array::{RecordBatch, StringArray};
        use cloud::{FsStorage, StorageService};

        let dir = tempfile::TempDir::new().unwrap();
        let storage = FsStorage::new(dir.path().to_path_buf()).await.unwrap();
        let run_date = chrono::NaiveDate::from_ymd_opt(2026, 6, 14).unwrap();

        // A real 2-row Parquet under the otel_logs data prefix.
        let schema = Arc::new(ArrowSchema::new(vec![Field::new(
            "id",
            DataType::Utf8,
            false,
        )]));
        let batch = RecordBatch::try_new(schema, vec![Arc::new(StringArray::from(vec!["a", "b"]))])
            .unwrap();
        let parquet = crate::encode_parquet(&batch).unwrap();
        storage
            .put(
                "telemetry/otel_logs/data/dt=2026-06-14/part-1.parquet",
                &parquet,
                "application/octet-stream",
            )
            .await
            .unwrap();

        let authored = author_iceberg_for_prefix(
            &storage,
            PromotionRequest {
                lane: "telemetry",
                table: "otel_logs",
                location_base: "gs://exports",
                run_date,
                data_key: None,
                snapshot_id: 5001,
                timestamp_ms: 1_700_000_000_000,
                expire_before_ms: None,
            },
        )
        .await
        .unwrap();
        assert!(authored.is_some(), "data present -> metadata authored");

        // version-hint + metadata.json landed in storage and parse back.
        let hint = storage
            .get("telemetry/otel_logs/metadata/version-hint.text")
            .await
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&hint.bytes).trim(), "1");
        let meta = storage
            .get("telemetry/otel_logs/metadata/v1.metadata.json")
            .await
            .unwrap();
        let tm: TableMetadata = serde_json::from_slice(&meta.bytes).unwrap();
        assert_eq!(tm.current_snapshot_id(), Some(5001));
        // The data-file path recorded is the absolute store URI.
        let snap = tm.snapshot_by_id(5001).unwrap();
        assert!(snap
            .manifest_list()
            .starts_with("gs://exports/telemetry/otel_logs/metadata/"));

        // A prefix with no data files is a clean no-op.
        let none = author_iceberg_for_prefix(
            &storage,
            PromotionRequest {
                lane: "telemetry",
                table: "otel_traces",
                location_base: "gs://exports",
                run_date,
                data_key: None,
                snapshot_id: 5002,
                timestamp_ms: 1,
                expire_before_ms: None,
            },
        )
        .await
        .unwrap();
        assert!(none.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_store_table_and_a_telemetry_table_of_the_same_name_do_not_collide() {
        use arrow::array::{RecordBatch, StringArray};
        use cloud::{FsStorage, StorageService};

        let dir = tempfile::TempDir::new().unwrap();
        let storage = FsStorage::new(dir.path().to_path_buf()).await.unwrap();
        let run_date = chrono::NaiveDate::from_ymd_opt(2026, 6, 14).unwrap();

        let schema = Arc::new(ArrowSchema::new(vec![Field::new(
            "id",
            DataType::Utf8,
            false,
        )]));
        let batch =
            RecordBatch::try_new(schema, vec![Arc::new(StringArray::from(vec!["a"]))]).unwrap();
        let parquet = crate::encode_parquet(&batch).unwrap();

        // The same table name, "shared", written under both lanes.
        storage
            .put(
                "application/shared/data/dt=2026-06-14/part-1.parquet",
                &parquet,
                "application/octet-stream",
            )
            .await
            .unwrap();
        storage
            .put(
                "telemetry/shared/data/dt=2026-06-14/part-1.parquet",
                &parquet,
                "application/octet-stream",
            )
            .await
            .unwrap();

        let application = author_iceberg_for_prefix(
            &storage,
            PromotionRequest {
                lane: "application",
                table: "shared",
                location_base: "gs://exports",
                run_date,
                data_key: None,
                snapshot_id: 1,
                timestamp_ms: 1_700_000_000_000,
                expire_before_ms: None,
            },
        )
        .await
        .unwrap();
        let telemetry = author_iceberg_for_prefix(
            &storage,
            PromotionRequest {
                lane: "telemetry",
                table: "shared",
                location_base: "gs://exports",
                run_date,
                data_key: None,
                snapshot_id: 2,
                timestamp_ms: 1_700_000_000_000,
                expire_before_ms: None,
            },
        )
        .await
        .unwrap();
        assert!(application.is_some());
        assert!(telemetry.is_some());

        // Each lane's metadata is independent — authoring telemetry's did not
        // clobber (or chain onto) application's version-hint.
        let application_hint = storage
            .get("application/shared/metadata/version-hint.text")
            .await
            .unwrap();
        let telemetry_hint = storage
            .get("telemetry/shared/metadata/version-hint.text")
            .await
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&application_hint.bytes).trim(), "1");
        assert_eq!(String::from_utf8_lossy(&telemetry_hint.bytes).trim(), "1");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn second_run_chains_onto_the_prior_snapshot_log() {
        let arrow = sample_arrow_schema();
        let files = vec![DataFileSpec {
            path: "gs://proj-exports/iceberg/persons/data/dt=2026-06-14/part-1.parquet".to_string(),
            record_count: 10,
            file_size_in_bytes: 100,
        }];
        let first = author_snapshot(&input(&arrow, &files)).await.unwrap();
        let prior_json = first
            .files
            .iter()
            .find(|(n, _)| n == "metadata/v1.metadata.json")
            .unwrap()
            .1
            .clone();

        let mut second_in = input(&arrow, &files);
        second_in.snapshot_id = 1002;
        second_in.sequence_number = 2;
        second_in.version = 2;
        second_in.prior = Some(PriorMetadata {
            metadata_json: prior_json,
            location: first.metadata_location.clone(),
        });
        let second = author_snapshot(&second_in).await.unwrap();
        let meta_bytes = &second
            .files
            .iter()
            .find(|(n, _)| n == "metadata/v2.metadata.json")
            .unwrap()
            .1;
        let meta: TableMetadata = serde_json::from_slice(meta_bytes).unwrap();
        // Both snapshots present; the new one is current and parents on the old.
        assert_eq!(meta.current_snapshot_id(), Some(1002));
        assert!(
            meta.snapshot_by_id(1001).is_some(),
            "prior snapshot retained"
        );
        assert_eq!(
            meta.snapshot_by_id(1002).unwrap().parent_snapshot_id(),
            Some(1001)
        );
    }

    #[tokio::test]
    async fn unchanged_lists_can_extend_the_snapshot_log() {
        let arrow = ArrowSchema::new(vec![
            Field::new(
                "tags",
                DataType::List(Arc::new(Field::new_list_field(DataType::Utf8, true))),
                true,
            ),
            Field::new("id", DataType::Utf8, true),
        ]);
        let first = author_snapshot(&input(&arrow, &[])).await.unwrap();
        let metadata_json = first
            .files
            .iter()
            .find(|(name, _)| name == "metadata/v1.metadata.json")
            .unwrap()
            .1
            .clone();
        let mut next = input(&arrow, &[]);
        next.snapshot_id = 1002;
        next.sequence_number = 2;
        next.version = 2;
        next.prior = Some(PriorMetadata {
            metadata_json,
            location: first.metadata_location,
        });
        author_snapshot(&next).await.unwrap();
    }

    #[tokio::test]
    async fn changed_columns_cannot_reuse_a_prior_tables_field_ids() {
        let arrow = sample_arrow_schema();
        let first = author_snapshot(&input(&arrow, &[])).await.unwrap();
        let metadata_json = first
            .files
            .iter()
            .find(|(name, _)| name == "metadata/v1.metadata.json")
            .unwrap()
            .1
            .clone();
        let fields = arrow.fields().to_vec();
        let mut added = fields.clone();
        added.push(Arc::new(Field::new("new_column", DataType::Utf8, true)));
        let mut renamed = fields.clone();
        renamed[0] = Arc::new(Field::new(
            "renamed",
            fields[0].data_type().clone(),
            fields[0].is_nullable(),
        ));
        let mut reordered = fields.clone();
        reordered.swap(0, 1);
        let mut retyped = fields.clone();
        retyped[0] = Arc::new(Field::new(fields[0].name(), DataType::Boolean, true));
        for changed in [added, fields[1..].to_vec(), renamed, reordered, retyped] {
            let schema = ArrowSchema::new(changed);
            let mut next = input(&schema, &[]);
            next.snapshot_id = 1002;
            next.sequence_number = 2;
            next.version = 2;
            next.prior = Some(PriorMetadata {
                metadata_json: metadata_json.clone(),
                location: first.metadata_location.clone(),
            });
            let error = author_snapshot(&next).await.unwrap_err();
            assert!(error.to_string().contains("schema evolution"), "{error:#}");
        }
    }

    #[tokio::test]
    async fn mixed_parquet_schemas_do_not_publish_a_metadata_pointer() {
        use arrow::array::{RecordBatch, StringArray};
        use cloud::{FsStorage, StorageService};

        let dir = tempfile::TempDir::new().unwrap();
        let storage = FsStorage::new(dir.path().to_path_buf()).await.unwrap();
        for (part, column) in [(1, "id"), (2, "renamed_id")] {
            let schema = Arc::new(ArrowSchema::new(vec![Field::new(
                column,
                DataType::Utf8,
                true,
            )]));
            let batch =
                RecordBatch::try_new(schema, vec![Arc::new(StringArray::from(vec!["synthetic"]))])
                    .unwrap();
            storage
                .put(
                    &format!("application/person/data/dt=2026-06-14/part-{part}.parquet"),
                    &crate::encode_parquet(&batch).unwrap(),
                    "application/octet-stream",
                )
                .await
                .unwrap();
        }
        let error = author_iceberg_for_prefix(
            &storage,
            PromotionRequest {
                lane: "application",
                table: "person",
                location_base: "gs://exports",
                run_date: NaiveDate::from_ymd_opt(2026, 6, 14).unwrap(),
                data_key: None,
                snapshot_id: 5001,
                timestamp_ms: 1_700_000_000_000,
                expire_before_ms: None,
            },
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("Parquet schemas"), "{error:#}");
        assert!(storage
            .list("application/person/metadata/")
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn an_explicit_snapshot_key_excludes_other_daily_and_manual_extractions() {
        use arrow::array::{RecordBatch, StringArray};
        use cloud::{FsStorage, StorageService};
        use iceberg::spec::ManifestList;
        let dir = tempfile::tempdir().unwrap();
        let storage = FsStorage::new(dir.path()).await.unwrap();
        let schema = Arc::new(ArrowSchema::new(vec![Field::new(
            "id",
            DataType::Utf8,
            false,
        )]));
        let batch = RecordBatch::try_new(schema, vec![Arc::new(StringArray::from(vec!["sample"]))])
            .unwrap();
        let parquet = crate::encode_parquet(&batch).unwrap();
        let selected = "application/person/data/dt=2026-06-14/part-manual.parquet";
        for key in [
            selected,
            "application/person/data/dt=2026-06-14/run=0555/snapshot.parquet",
        ] {
            storage
                .put(key, &parquet, "application/octet-stream")
                .await
                .unwrap();
        }
        let request = PromotionRequest {
            lane: "application",
            table: "person",
            location_base: "gs://sample-archives",
            run_date: NaiveDate::from_ymd_opt(2026, 6, 14).unwrap(),
            data_key: Some(selected),
            snapshot_id: 1,
            timestamp_ms: 1_700_000_000_000,
            expire_before_ms: None,
        };
        let authored = author_iceberg_for_prefix(&storage, request)
            .await
            .unwrap()
            .unwrap();
        let metadata = storage
            .get("application/person/metadata/v1.metadata.json")
            .await
            .unwrap();
        let table: TableMetadata = serde_json::from_slice(&metadata.bytes).unwrap();
        let list_key = table
            .current_snapshot()
            .unwrap()
            .manifest_list()
            .strip_prefix("gs://sample-archives/")
            .unwrap();
        let list = storage.get(list_key).await.unwrap();
        let parsed = ManifestList::parse_with_version(&list.bytes, FormatVersion::V2).unwrap();
        assert_eq!(authored.version, 1);
        assert_eq!(parsed.entries()[0].added_rows_count, Some(1));
        assert!(author_iceberg_for_prefix(
            &storage,
            PromotionRequest {
                data_key: Some("application/person/data/dt=2026-06-14/missing.parquet"),
                ..request
            }
        )
        .await
        .is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn expire_before_ms_drops_aged_out_snapshots_from_the_log() {
        let arrow = sample_arrow_schema();
        let files = vec![DataFileSpec {
            path: "gs://e/iceberg/otel_traces/data/dt=2026-06-14/p.parquet".to_string(),
            record_count: 1,
            file_size_in_bytes: 10,
        }];
        // First snapshot at an OLD timestamp.
        let mut first_in = input(&arrow, &files);
        first_in.table = "otel_traces";
        first_in.table_location = "gs://e/iceberg/otel_traces";
        first_in.timestamp_ms = 1_000_000_000_000; // old
        let first = author_snapshot(&first_in).await.unwrap();
        let prior_json = first
            .files
            .iter()
            .find(|(n, _)| n == "metadata/v1.metadata.json")
            .unwrap()
            .1
            .clone();

        // Second run, NEW timestamp, with a cutoff between the two: the old
        // snapshot is expired, only the current one survives.
        let mut second_in = input(&arrow, &files);
        second_in.table = "otel_traces";
        second_in.table_location = "gs://e/iceberg/otel_traces";
        second_in.snapshot_id = 1002;
        second_in.sequence_number = 2;
        second_in.version = 2;
        second_in.timestamp_ms = 1_700_000_000_000; // new
        second_in.expire_before_ms = Some(1_500_000_000_000); // between old and new
        second_in.prior = Some(PriorMetadata {
            metadata_json: prior_json,
            location: first.metadata_location.clone(),
        });
        let second = author_snapshot(&second_in).await.unwrap();
        let meta_bytes = &second
            .files
            .iter()
            .find(|(n, _)| n == "metadata/v2.metadata.json")
            .unwrap()
            .1;
        let meta: TableMetadata = serde_json::from_slice(meta_bytes).unwrap();
        assert_eq!(meta.current_snapshot_id(), Some(1002));
        assert!(
            meta.snapshot_by_id(1001).is_none(),
            "aged-out snapshot must be expired from the log"
        );
    }
}

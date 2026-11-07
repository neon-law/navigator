//! One immutable, fixed-location Iceberg layout over verified reference files.

use std::path::Path;

use anyhow::{ensure, Context, Result};
use arrow::array::{Array, RecordBatch};
use arrow::datatypes::DataType;
use futures::TryStreamExt;
use iceberg::io::FileIO;
use iceberg::spec::{DataFileFormat, FormatVersion, TableMetadata};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ephemeris_tables::{
    digest, read_bounded, write_new, MAX_FILE_BYTES, MAX_MANIFEST_BYTES,
};

pub(super) const RECEIPT: &str = "iceberg-manifest.json";
const METADATA: &str = "metadata/v1.metadata.json";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LayoutReceipt {
    artifact_id: String,
    content: Layout,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Layout {
    receipt_schema_version: u32,
    iceberg_format_version: u32,
    reference_snapshot_id: String,
    table_location: String,
    metadata_location: String,
    native_snapshot_id: i64,
    native_table_uuid: Uuid,
    native_timestamp_ms: i64,
    files: Vec<MetadataFile>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MetadataFile {
    name: String,
    bytes: usize,
    sha256: String,
}

/// Called after catalog authorization, before source reads or staging.
pub(super) fn location(dir: &Path) -> Result<String> {
    let name = dir
        .file_name()
        .context("choose a fixed Iceberg output directory")?;
    let parent = dir
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()?;
    let path = parent.join(name);
    let text = path
        .to_str()
        .context("Iceberg output requires a URI-safe ASCII path")?;
    let uri = url::Url::from_file_path(&path)
        .map_err(|()| anyhow::anyhow!("Iceberg output requires an absolute local path"))?;
    // The native filesystem reader strips file: but does not percent-decode its path.
    ensure!(
        text.is_ascii() && !text.contains('%') && uri.path() == text,
        "Iceberg output requires a fixed URI-safe ASCII path without URI escaping"
    );
    Ok(uri.into())
}

fn artifact_id(layout: &Layout) -> Result<String> {
    let mut value = serde_json::to_value(layout)?;
    value.sort_all_objects();
    Ok(digest(&serde_json::to_vec(&value)?))
}

fn canonical_uuid(text: &str) -> bool {
    Uuid::parse_str(text).is_ok_and(|id| id.to_string() == text)
}

fn valid_name(name: &str, snapshot: i64) -> bool {
    let Some(name) = name.strip_prefix("metadata/") else {
        return false;
    };
    if matches!(name, "v1.metadata.json" | "version-hint.text") {
        return true;
    }
    if let Some(id) = name.strip_suffix("-m0.avro") {
        return canonical_uuid(id);
    }
    name.strip_prefix(&format!("snap-{snapshot}-"))
        .and_then(|s| s.strip_suffix(".avro"))
        .is_some_and(canonical_uuid)
}

fn check_names(files: &[MetadataFile], snapshot: i64) -> Result<()> {
    let names: std::collections::BTreeSet<_> = files.iter().map(|f| f.name.as_str()).collect();
    ensure!(
        snapshot > 0
            && files.len() == 4
            && names.len() == 4
            && names.contains(METADATA)
            && names.contains("metadata/version-hint.text")
            && names.iter().all(|n| valid_name(n, snapshot))
            && names.iter().filter(|n| n.ends_with("-m0.avro")).count() == 1
            && names
                .iter()
                .filter(|n| n.starts_with("metadata/snap-"))
                .count()
                == 1,
        "Iceberg metadata inventory differs from one immutable snapshot"
    );
    Ok(())
}

pub(super) async fn author(
    stage: &Path,
    location: &str,
    reference_id: &str,
    timestamp: i64,
    expected: &RecordBatch,
) -> Result<()> {
    // This identifier needs uniqueness only within this newly created one-snapshot table.
    let snapshot = i64::try_from(rand::random::<u64>() >> 1)?.max(1);
    let parquet = read_bounded(&stage.join("reference.parquet"), MAX_FILE_BYTES)?;
    let data_uri = format!("{location}/reference.parquet");
    let schema = expected.schema();
    let authored = archives::iceberg::author_snapshot(&archives::iceberg::SnapshotInput {
        table: "ephemeris_reference_table",
        arrow_schema: &schema,
        table_location: location,
        data_files: &[archives::iceberg::DataFileSpec {
            path: data_uri,
            record_count: u64::try_from(expected.num_rows())?,
            file_size_in_bytes: u64::try_from(parquet.len())?,
        }],
        snapshot_id: snapshot,
        sequence_number: 1,
        timestamp_ms: timestamp,
        version: 1,
        prior: None,
        expire_before_ms: None,
    })
    .await?;
    let metadata_bytes = &authored
        .files
        .iter()
        .find(|(n, _)| n == METADATA)
        .context("native metadata missing")?
        .1;
    let metadata: TableMetadata = serde_json::from_slice(metadata_bytes)?;
    let files = authored
        .files
        .iter()
        .map(|(name, bytes)| MetadataFile {
            name: name.clone(),
            bytes: bytes.len(),
            sha256: digest(bytes),
        })
        .collect::<Vec<_>>();
    check_names(&files, snapshot)?;
    ensure!(
        files.iter().all(|f| f.bytes <= MAX_MANIFEST_BYTES),
        "Iceberg metadata exceeds its byte bound"
    );
    std::fs::create_dir(stage.join("metadata"))?;
    for (name, bytes) in authored.files {
        write_new(&stage.join(name), &bytes)?;
    }
    let content = Layout {
        receipt_schema_version: 1,
        iceberg_format_version: 2,
        reference_snapshot_id: reference_id.into(),
        table_location: location.into(),
        metadata_location: authored.metadata_location,
        native_snapshot_id: snapshot,
        native_table_uuid: metadata.uuid(),
        native_timestamp_ms: timestamp,
        files,
    };
    let receipt = LayoutReceipt {
        artifact_id: artifact_id(&content)?,
        content,
    };
    let bytes = serde_json::to_vec_pretty(&receipt)?;
    ensure!(
        bytes.len() <= MAX_MANIFEST_BYTES,
        "Iceberg receipt exceeds its byte bound"
    );
    write_new(&stage.join(RECEIPT), &bytes)?;
    std::fs::File::open(stage.join("metadata"))?.sync_all()?;
    Ok(())
}

fn load_closed_graph(
    dir: &Path,
    layout: &Layout,
    location: &str,
    reference_id: &str,
) -> Result<Vec<(String, Vec<u8>)>> {
    ensure!(
        layout.receipt_schema_version == 1
            && layout.iceberg_format_version == 2
            && layout.reference_snapshot_id == reference_id
            && layout.table_location == location
            && layout.metadata_location == format!("{location}/{METADATA}"),
        "Iceberg layout identity or final location differs"
    );
    check_names(&layout.files, layout.native_snapshot_id)?;
    let metadata_dir = dir.join("metadata");
    ensure!(
        std::fs::symlink_metadata(&metadata_dir)?.is_dir(),
        "Iceberg metadata must be an ordinary directory"
    );
    let mut names = std::fs::read_dir(&metadata_dir)?
        .take(5)
        .map(|e| e.map(|e| e.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    names.sort();
    let mut expected = layout
        .files
        .iter()
        .map(|f| std::ffi::OsString::from(f.name.strip_prefix("metadata/").unwrap_or("")))
        .collect::<Vec<_>>();
    expected.sort();
    ensure!(
        names == expected,
        "Iceberg metadata directory differs from its receipt"
    );
    let mut graph = Vec::new();
    for file in &layout.files {
        ensure!(
            file.bytes <= MAX_MANIFEST_BYTES,
            "Iceberg metadata exceeds its byte bound"
        );
        let bytes = read_bounded(&dir.join(&file.name), MAX_MANIFEST_BYTES)?;
        ensure!(
            bytes.len() == file.bytes && digest(&bytes) == file.sha256,
            "Iceberg metadata checksum differs"
        );
        graph.push((format!("{location}/{}", file.name), bytes));
    }
    ensure!(
        read_bounded(&metadata_dir.join("version-hint.text"), 2)? == b"1",
        "Iceberg version hint differs"
    );
    Ok(graph)
}

pub(super) async fn verify(
    dir: &Path,
    location: &str,
    reference_id: &str,
    expected: &RecordBatch,
) -> Result<String> {
    let bytes = read_bounded(&dir.join(RECEIPT), MAX_MANIFEST_BYTES)?;
    let receipt: LayoutReceipt = serde_json::from_slice(&bytes)?;
    ensure!(
        receipt.artifact_id == artifact_id(&receipt.content)?,
        "Iceberg artifact identity differs"
    );
    let layout = &receipt.content;
    let graph = load_closed_graph(dir, layout, location, reference_id)?;
    let metadata_bytes = &graph
        .iter()
        .find(|(name, _)| name == &layout.metadata_location)
        .context("Iceberg metadata is outside its closed graph")?
        .1;
    let metadata: TableMetadata = serde_json::from_slice(metadata_bytes)?;
    validate_metadata(&metadata, metadata_bytes, layout, expected)?;
    let io = FileIO::new_with_memory();
    // Only these checksum-verified bytes are reachable: no ambient filesystem or network factory.
    for (name, bytes) in graph {
        io.new_output(name)?.write(bytes.into()).await?;
    }
    let data_uri = format!("{location}/reference.parquet");
    let parquet = read_bounded(&dir.join("reference.parquet"), MAX_FILE_BYTES)?;
    let size = u64::try_from(parquet.len())?;
    io.new_output(&data_uri)?.write(parquet.into()).await?;
    let table = iceberg::table::StaticTable::from_metadata_file(
        &layout.metadata_location,
        iceberg::TableIdent::from_strs(["reference", "ephemeris_reference_table"])?,
        io,
    )
    .await?;
    let expected_rows = u64::try_from(expected.num_rows())?;
    let tasks: Vec<_> = table
        .scan()
        .build()?
        .plan_files()
        .await?
        .try_collect()
        .await?;
    ensure!(
        tasks.len() == 1
            && tasks.iter().all(|task| task.data_file_path() == data_uri
                && task.file_size_in_bytes == size
                && task.start == 0
                && task.length == size
                && task.record_count == Some(expected_rows)
                && task.data_file_format == DataFileFormat::Parquet
                && task.deletes.is_empty()),
        "Iceberg scan plan leaves the exact reference data file or snapshot"
    );
    let batches: Vec<_> = table
        .reader_builder()
        .build()
        .read(Box::pin(futures::stream::iter(
            tasks.into_iter().map(Ok::<_, iceberg::Error>),
        )))?
        .stream()
        .try_collect()
        .await?;
    compare(&batches, expected)?;
    Ok(digest(&bytes))
}

fn validate_metadata(
    metadata: &TableMetadata,
    bytes: &[u8],
    layout: &Layout,
    expected: &RecordBatch,
) -> Result<()> {
    validate_envelope(metadata, layout)?;
    ensure!(
        serde_json::from_slice::<serde_json::Value>(bytes)? == serde_json::to_value(metadata)?,
        "Iceberg metadata contains unsupported fields"
    );
    let snapshot = metadata
        .current_snapshot()
        .context("Iceberg snapshot absent")?;
    ensure!(
        metadata.format_version() == FormatVersion::V2
            && metadata.location() == layout.table_location
            && metadata.uuid() == layout.native_table_uuid
            && metadata.last_updated_ms() == layout.native_timestamp_ms
            && metadata.last_column_id() == i32::try_from(expected.num_columns())?
            && metadata.last_sequence_number() == 1
            && metadata.snapshots().len() == 1
            && metadata.properties().is_empty()
            && snapshot.snapshot_id() == layout.native_snapshot_id
            && snapshot.timestamp_ms() == layout.native_timestamp_ms
            && snapshot.parent_snapshot_id().is_none()
            && snapshot.sequence_number() == 1
            && snapshot.schema_id() == Some(0)
            && metadata.current_schema().as_struct()
                == archives::iceberg::arrow_schema_to_iceberg(&expected.schema())?.as_struct(),
        "Iceberg schema, settings or immutable snapshot identity differs"
    );
    let manifest_list = snapshot.manifest_list();
    ensure!(
        layout
            .files
            .iter()
            .any(
                |f| format!("{}/{}", layout.table_location, f.name) == manifest_list
                    && f.name.starts_with("metadata/snap-")
            ),
        "Iceberg manifest list leaves the closed graph"
    );
    Ok(())
}

pub(super) fn compare(
    batches: &[arrow_array58::RecordBatch],
    expected: &RecordBatch,
) -> Result<()> {
    use arrow_array58::Array as _;
    let mut offset = 0;
    for actual in batches {
        ensure!(
            actual.num_columns() == expected.num_columns()
                && offset + actual.num_rows() <= expected.num_rows(),
            "Iceberg row or column count differs"
        );
        for (column, field) in expected.schema().fields().iter().enumerate() {
            ensure!(
                actual.schema().field(column).name() == field.name()
                    && actual.schema().field(column).data_type().to_string()
                        == field.data_type().to_string(),
                "Iceberg ordered physical schema differs"
            );
            let a = actual.column(column);
            let b = expected.column(column);
            for row in 0..actual.num_rows() {
                ensure!(
                    a.is_null(row) == b.is_null(offset + row),
                    "Iceberg null mask differs"
                );
                if a.is_null(row) {
                    continue;
                }
                macro_rules! equal {
                    ($a:ty, $b:ty) => {
                        a.as_any()
                            .downcast_ref::<$a>()
                            .context("Iceberg physical type mismatch")?
                            .value(row)
                            == b.as_any()
                                .downcast_ref::<$b>()
                                .context("reference physical type mismatch")?
                                .value(offset + row)
                    };
                }
                let same = match field.data_type() {
                    DataType::Utf8 => equal!(arrow_array58::StringArray, arrow::array::StringArray),
                    DataType::Int64 => equal!(arrow_array58::Int64Array, arrow::array::Int64Array),
                    DataType::Int32 => equal!(arrow_array58::Int32Array, arrow::array::Int32Array),
                    DataType::Float64 => {
                        a.as_any()
                            .downcast_ref::<arrow_array58::Float64Array>()
                            .context("Iceberg Float64 mismatch")?
                            .value(row)
                            .to_bits()
                            == b.as_any()
                                .downcast_ref::<arrow::array::Float64Array>()
                                .context("reference Float64 mismatch")?
                                .value(offset + row)
                                .to_bits()
                    }
                    _ => anyhow::bail!("unsupported reference physical type"),
                };
                ensure!(same, "Iceberg value differs from exact reference");
            }
        }
        offset += actual.num_rows();
    }
    ensure!(offset == expected.num_rows(), "Iceberg rows are missing");
    Ok(())
}

fn validate_envelope(metadata: &TableMetadata, layout: &Layout) -> Result<()> {
    use iceberg::spec::{
        Operation, PartitionSpec, SnapshotReference, SnapshotRetention, SortOrder, Summary,
    };
    let canonical = serde_json::to_value(metadata)?;
    let refs = std::collections::HashMap::from([(
        "main",
        SnapshotReference::new(
            layout.native_snapshot_id,
            SnapshotRetention::branch(None, None, None),
        ),
    )]);
    let summary = Summary {
        operation: Operation::Append,
        additional_properties: std::collections::HashMap::new(),
    };
    ensure!(
        metadata.schemas_iter().len() == 1
            && metadata.current_schema_id() == 0
            && metadata.partition_specs_iter().len() == 1
            && metadata.default_partition_spec().as_ref() == &PartitionSpec::unpartition_spec()
            && metadata.sort_orders_iter().len() == 1
            && metadata.default_sort_order().as_ref() == &SortOrder::unsorted_order()
            && metadata.metadata_log().is_empty()
            && metadata.statistics_iter().len() == 0
            && metadata.partition_statistics_iter().len() == 0
            && metadata.encryption_keys_iter().len() == 0
            && metadata.history().len() == 1
            && metadata
                .history()
                .first()
                .is_some_and(|h| h.snapshot_id == layout.native_snapshot_id
                    && h.timestamp_ms == layout.native_timestamp_ms)
            && canonical.get("refs") == Some(&serde_json::to_value(refs)?)
            && canonical["snapshots"][0]["summary"] == serde_json::to_value(summary)?,
        "Iceberg immutable metadata envelope differs from one new table"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::location;

    #[test]
    fn output_uri_rejects_escaping_without_creating_paths() {
        let root = tempfile::tempdir().unwrap();
        for name in [
            "with space",
            "literal%20",
            "hash#name",
            "query?name",
            "unicode-é",
        ] {
            let path = root.path().join(name);
            assert!(location(&path).is_err(), "{name}");
            assert!(!path.exists());
        }
        assert!(location(&root.path().join("safe-output_1"))
            .unwrap()
            .starts_with("file:///"));
        let unsafe_parent = root.path().join("parent with spaces");
        std::fs::create_dir(&unsafe_parent).unwrap();
        assert!(location(&unsafe_parent.join("safe-child")).is_err());
        assert!(!unsafe_parent.join("safe-child").exists());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
}

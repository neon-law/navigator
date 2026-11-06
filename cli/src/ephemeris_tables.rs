//! Immutable ordinary Parquet/CSV reference files, separate from recovery archives.

use std::io::{Read, Seek, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{ensure, Context, Result};
use arrow::array::{
    Array, ArrayRef, Float64Array, Int32Array, Int64Array, RecordBatch, StringArray,
};
use arrow::datatypes::{DataType, Field, Schema};
use chrono::{Datelike, NaiveDate};
use ephemeris::{EphemerisManifest, EphemerisReferenceTable, EphemerisStore};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::ephemeris_download::{hex_digest, open_regular, verify_bundle};

const MAX_FILE_BYTES: usize = 4_194_304;
const MAX_MANIFEST_BYTES: usize = 65_536;
const MANIFEST: &str = "derived-manifest.json";
const PARQUET: &str = "reference.parquet";
const CSV: &str = "reference.csv";
const SOURCE: &str = "source-manifest.json";
// The encoder's pinned major version is part of the artifact byte-format identity.
const FORMAT: &str = "navigator-reference-parquet60-csv-v1";

struct PinnedSource {
    file: Mutex<std::fs::File>,
    bytes: u64,
}

impl EphemerisStore for PinnedSource {
    fn read(&self, name: &str) -> Result<Vec<u8>, ephemeris::EphemerisError> {
        let read = || -> std::io::Result<Vec<u8>> {
            if name != "de440.bsp" {
                return Err(std::io::Error::other(
                    "only the pinned DE440 source is eligible",
                ));
            }
            let mut file = self
                .file
                .lock()
                .map_err(|_| std::io::Error::other("source lock unavailable"))?;
            if file.metadata()?.len() != self.bytes {
                return Err(std::io::Error::other("source size differs from pin"));
            }
            file.rewind()?;
            let mut bytes = Vec::new();
            Read::by_ref(&mut *file)
                .take(self.bytes + 1)
                .read_to_end(&mut bytes)?;
            Ok(bytes)
        };
        read().map_err(|source| ephemeris::EphemerisError::Io {
            path: "de440.bsp".into(),
            source,
        })
    }
}

fn label(value: &impl serde::Serialize) -> Result<String> {
    serde_json::to_value(value)?
        .as_str()
        .map(str::to_owned)
        .context("reference label is not a string")
}

fn batch(rows: &[ephemeris::EphemerisReferenceRow]) -> Result<RecordBatch> {
    let arrays: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from(
            rows.iter()
                .map(|r| r.reference_calendar.to_string())
                .collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from(
            rows.iter()
                .map(|r| label(&r.reference_scale))
                .collect::<Result<Vec<_>>>()?,
        )),
        Arc::new(Float64Array::from(
            rows.iter().map(|r| r.julian_day_utc).collect::<Vec<_>>(),
        )),
        Arc::new(Int64Array::from(
            rows.iter()
                .map(|r| r.tt_nanoseconds_since_j2000)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Int64Array::from(
            rows.iter()
                .map(|r| r.tdb_nanoseconds_since_j2000)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            rows.iter()
                .map(|r| r.spk_tdb_seconds_since_j2000)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            rows.iter()
                .map(|r| r.model_julian_day_ut1)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            rows.iter().map(|r| r.delta_t_seconds).collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from(
            rows.iter()
                .map(|r| label(&r.delta_t_kind))
                .collect::<Result<Vec<_>>>()?,
        )),
        Arc::new(Float64Array::from(
            rows.iter()
                .map(|r| r.delta_t_reported_error_seconds)
                .collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from(
            rows.iter().map(|r| r.position.body).collect::<Vec<_>>(),
        )),
        Arc::new(Int32Array::from(
            rows.iter()
                .map(|r| r.position.source_id)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            rows.iter()
                .map(|r| r.position.longitude_deg)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            rows.iter()
                .map(|r| r.position.latitude_deg)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            rows.iter()
                .map(|r| r.position.right_ascension_deg)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            rows.iter()
                .map(|r| r.position.declination_deg)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            rows.iter()
                .map(|r| r.position.distance_km)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            rows.iter()
                .map(|r| r.position.longitude_speed_deg_per_day)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            rows.iter()
                .map(|r| r.position.latitude_speed_deg_per_day)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            rows.iter()
                .map(|r| r.position.distance_speed_km_per_day)
                .collect::<Vec<_>>(),
        )),
    ];
    Ok(RecordBatch::try_new(schema(), arrays)?)
}

// Fixed field order/IDs belong to artifact schema v1; row JSON never infers physical types.
fn schema() -> Arc<Schema> {
    let fields = [
        ("reference_calendar", DataType::Utf8, false),
        ("reference_scale", DataType::Utf8, false),
        ("julian_day_utc", DataType::Float64, true),
        ("tt_nanoseconds_since_j2000", DataType::Int64, false),
        ("tdb_nanoseconds_since_j2000", DataType::Int64, false),
        ("spk_tdb_seconds_since_j2000", DataType::Float64, false),
        ("model_julian_day_ut1", DataType::Float64, false),
        ("delta_t_seconds", DataType::Float64, false),
        ("delta_t_kind", DataType::Utf8, false),
        ("delta_t_reported_error_seconds", DataType::Float64, true),
        ("body", DataType::Utf8, false),
        ("source_id", DataType::Int32, true),
        ("longitude_deg", DataType::Float64, false),
        ("latitude_deg", DataType::Float64, false),
        ("right_ascension_deg", DataType::Float64, false),
        ("declination_deg", DataType::Float64, false),
        ("distance_km", DataType::Float64, true),
        ("longitude_speed_deg_per_day", DataType::Float64, false),
        ("latitude_speed_deg_per_day", DataType::Float64, false),
        ("distance_speed_km_per_day", DataType::Float64, true),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, (name, ty, nullable))| {
        Field::new(name, ty, nullable).with_metadata(std::collections::HashMap::from([(
            "PARQUET:field_id".into(),
            (i + 1).to_string(),
        )]))
    })
    .collect::<Vec<_>>();
    Arc::new(Schema::new(fields))
}

#[derive(Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Column {
    name: String,
    #[serde(rename = "type")]
    data_type: String,
    nullable: bool,
    field_id: String,
}

fn schema_description() -> Vec<Column> {
    schema()
        .fields()
        .iter()
        .map(|field| Column {
            name: field.name().clone(),
            data_type: field.data_type().to_string(),
            nullable: field.is_nullable(),
            field_id: field.metadata()["PARQUET:field_id"].clone(),
        })
        .collect()
}

#[derive(Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct FileIdentity {
    name: String,
    bytes: usize,
    sha256: String,
}

#[derive(Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Content {
    format: String,
    row_schema_version: u32,
    from: NaiveDate,
    to_exclusive: NaiveDate,
    row_count: usize,
    metadata: Value,
    columns: Vec<Column>,
    source_manifest_sha256: String,
    csv_null: String,
    files: Vec<FileIdentity>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DerivedManifest {
    snapshot_id: String,
    generated_at: chrono::DateTime<chrono::Utc>,
    content: Content,
}

fn cell(array: &dyn Array, row: usize) -> Result<String> {
    if array.is_null(row) {
        return Ok(String::new());
    }
    macro_rules! value {
        ($ty:ty) => {
            array
                .as_any()
                .downcast_ref::<$ty>()
                .context("reference column type mismatch")?
                .value(row)
                .to_string()
        };
    }
    Ok(match array.data_type() {
        DataType::Utf8 => value!(StringArray),
        DataType::Int64 => value!(Int64Array),
        DataType::Int32 => value!(Int32Array),
        DataType::Float64 => value!(Float64Array),
        _ => anyhow::bail!("unsupported reference column type"),
    })
}

fn csv_bytes(batch: &RecordBatch) -> Result<Vec<u8>> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    writer.write_record(batch.schema().fields().iter().map(|f| f.name()))?;
    for row in 0..batch.num_rows() {
        writer.write_record(
            batch
                .columns()
                .iter()
                .map(|array| cell(array.as_ref(), row))
                .collect::<Result<Vec<_>>>()?,
        )?;
    }
    Ok(writer.into_inner()?)
}

fn read_bounded(path: &Path, cap: usize) -> Result<Vec<u8>> {
    let file = open_regular(path)?;
    ensure!(
        file.metadata()?.len() <= cap as u64,
        "bundle file exceeds byte bound"
    );
    let mut bytes = Vec::new();
    file.take((cap + 1) as u64).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= cap, "bundle file exceeds byte bound");
    Ok(bytes)
}

fn digest(bytes: &[u8]) -> String {
    hex_digest(&Sha256::digest(bytes))
}

fn existing_manifest(
    dir: &Path,
    from: NaiveDate,
    to: NaiveDate,
    source: &[u8],
) -> Result<Option<DerivedManifest>> {
    match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
        Ok(metadata) => ensure!(
            metadata.is_dir(),
            "table bundle is not an ordinary directory"
        ),
    }
    let mut names = std::fs::read_dir(dir)?
        .take(5)
        .map(|e| e.map(|e| e.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    names.sort();
    let mut expected: Vec<_> = [MANIFEST, SOURCE, PARQUET, CSV]
        .map(std::ffi::OsString::from)
        .into();
    expected.sort();
    ensure!(
        names == expected,
        "existing path is not a complete reference bundle"
    );
    let saved: DerivedManifest =
        serde_json::from_slice(&read_bounded(&dir.join(MANIFEST), MAX_MANIFEST_BYTES)?)?;
    let content = &saved.content;
    ensure!(
        saved.snapshot_id == content_digest(content)?
            && content.format == FORMAT
            && content.row_schema_version == 1
            && content.from == from
            && content.to_exclusive == to
            && content.columns == schema_description()
            && content.metadata["provider_version"] == env!("CARGO_PKG_VERSION")
            && content.source_manifest_sha256 == digest(source),
        "existing bundle semantics or identity differ from the requested table"
    );
    Ok(Some(saved))
}

/// Called only after the canonical protected catalogue has authorized the operator.
pub(crate) fn publish(
    source_dir: &Path,
    manifest: &EphemerisManifest,
    from: NaiveDate,
    to: NaiveDate,
    dir: &Path,
) -> Result<()> {
    ensure!(
        from.year() >= 1900
            && to <= NaiveDate::from_ymd_opt(2050, 1, 1).context("valid reference boundary")?
            && (1..=ephemeris::MAX_REFERENCE_BATCH_DAYS)
                .contains(&to.signed_duration_since(from).num_days()),
        "invalid bounded reference interval"
    );
    let source = serde_json::to_vec_pretty(manifest)?;
    let saved = existing_manifest(dir, from, to, &source)?;
    verify_bundle(source_dir, manifest)?;
    let pin = manifest
        .files
        .first()
        .context("eligible source pin missing")?;
    let store = PinnedSource {
        file: Mutex::new(open_regular(&source_dir.join("de440.bsp"))?),
        bytes: pin.bytes,
    };
    // The evaluator re-verifies the bytes read from this same descriptor, not a reopened path.
    let table = EphemerisReferenceTable::calculate(&store, from, to)?;
    let batch = batch(&table.rows)?;
    let files = [
        (SOURCE, source),
        (PARQUET, archives::parquet_io::encode_parquet(&batch)?),
        (CSV, csv_bytes(&batch)?),
    ];
    ensure!(
        files.iter().all(|(_, bytes)| bytes.len() <= MAX_FILE_BYTES),
        "reference artifact exceeds byte bound"
    );
    let mut metadata = serde_json::to_value(&table.metadata)?;
    metadata
        .as_object_mut()
        .context("reference metadata object")?
        .remove("source_manifest");
    let content = Content {
        format: FORMAT.into(),
        row_schema_version: table.schema_version,
        from,
        to_exclusive: to,
        row_count: batch.num_rows(),
        metadata,
        columns: schema_description(),
        source_manifest_sha256: digest(&files[0].1),
        csv_null: "empty field".into(),
        files: files
            .iter()
            .map(|(name, bytes)| FileIdentity {
                name: (*name).into(),
                bytes: bytes.len(),
                sha256: digest(bytes),
            })
            .collect(),
    };
    if let Some(saved) = saved {
        ensure!(
            saved.content == content,
            "existing bundle settings or file identities differ"
        );
        for (name, expected) in &files {
            ensure!(
                read_bounded(&dir.join(name), MAX_FILE_BYTES)? == *expected,
                "existing bundle data differs from its checksum"
            );
        }
        return Ok(());
    }
    publish_new(dir, &files, content, &batch)
}

fn publish_new(
    dir: &Path,
    files: &[(&str, Vec<u8>)],
    content: Content,
    batch: &RecordBatch,
) -> Result<()> {
    let from = content.from;
    let to = content.to_exclusive;
    let name = dir
        .file_name()
        .context("choose a new table bundle directory")?;
    let parent = dir
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()?;
    let destination = parent.join(name);
    let stage = tempfile::Builder::new()
        .prefix(".navigator-reference-")
        .tempdir_in(&parent)?;
    for (name, bytes) in files {
        write_new(&stage.path().join(name), bytes)?;
    }
    let receipt = DerivedManifest {
        snapshot_id: content_digest(&content)?,
        generated_at: chrono::Utc::now(),
        content,
    };
    let receipt_bytes = serde_json::to_vec_pretty(&receipt)?;
    ensure!(
        receipt_bytes.len() <= MAX_MANIFEST_BYTES,
        "derived manifest exceeds byte bound"
    );
    write_new(&stage.path().join(MANIFEST), &receipt_bytes)?;
    let verified = existing_manifest(stage.path(), from, to, &files[0].1)?
        .context("staged receipt missing")?;
    ensure!(
        verified.content == receipt.content && verified.snapshot_id == receipt.snapshot_id,
        "staged receipt differs"
    );
    for (name, bytes) in files {
        ensure!(
            read_bounded(&stage.path().join(name), MAX_FILE_BYTES)? == *bytes,
            "staged artifact verification failed"
        );
    }
    validate_readers(stage.path(), batch)?;
    std::fs::File::open(stage.path())?.sync_all()?;
    crate::atomic_directory::publish_no_clobber(stage.path(), &destination)?;
    let _published = stage.keep();
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn validate_readers(dir: &Path, expected: &RecordBatch) -> Result<()> {
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    let reader = ParquetRecordBatchReaderBuilder::try_new(open_regular(&dir.join(PARQUET))?)?;
    ensure!(
        reader.schema() == &expected.schema(),
        "Parquet schema or field IDs differ"
    );
    let mut offset = 0;
    for actual in reader.build()? {
        let actual = actual?;
        ensure!(
            offset + actual.num_rows() <= expected.num_rows(),
            "Parquet contains extra rows"
        );
        let slice = expected.slice(offset, actual.num_rows());
        ensure!(
            actual
                .columns()
                .iter()
                .zip(slice.columns())
                .all(|(a, b)| a.to_data() == b.to_data()),
            "Parquet values or nulls differ"
        );
        offset += actual.num_rows();
    }
    ensure!(offset == expected.num_rows(), "Parquet rows are missing");
    let mut csv = csv::Reader::from_reader(open_regular(&dir.join(CSV))?);
    ensure!(
        csv.headers()?
            .iter()
            .eq(expected.schema().fields().iter().map(|f| f.name().as_str())),
        "CSV columns differ"
    );
    let mut row = 0;
    for record in csv.records() {
        let record = record?;
        ensure!(row < expected.num_rows(), "CSV contains extra rows");
        ensure!(
            record.len() == expected.num_columns(),
            "CSV column count differs"
        );
        for (column, value) in record.iter().enumerate() {
            ensure!(
                value == cell(expected.column(column).as_ref(), row)?,
                "CSV value or null differs"
            );
        }
        row += 1;
    }
    ensure!(row == expected.num_rows(), "CSV rows are missing");
    Ok(())
}

fn content_digest(content: &Content) -> Result<String> {
    // Normalize object ordering through serde_json before hashing; generated_at is outside content.
    Ok(digest(&serde_json::to_vec(&serde_json::to_value(
        content,
    )?)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_descriptor_does_not_follow_a_replaced_source_path() {
        use std::os::unix::fs::symlink;
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("de440.bsp");
        std::fs::write(&path, b"original source").unwrap();
        let store = PinnedSource {
            file: Mutex::new(open_regular(&path).unwrap()),
            bytes: 15,
        };
        std::fs::remove_file(&path).unwrap();
        let other = parent.path().join("other");
        std::fs::write(&other, b"changed source!").unwrap();
        symlink(&other, &path).unwrap();
        assert_eq!(store.read("de440.bsp").unwrap(), b"original source");
        assert!(store.read("../other").is_err());
        assert!(open_regular(&path).is_err());
    }
}

#[cfg(test)]
mod artifact_tests {
    use super::*;

    fn rows() -> Vec<ephemeris::EphemerisReferenceRow> {
        // Representation fixtures only; these do not claim to be calculated celestial states.
        let physical = ephemeris::EphemerisReferenceRow {
            reference_calendar: NaiveDate::from_ymd_opt(1900, 1, 1)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap(),
            reference_scale: ephemeris::BirthTimeScale::Ut1,
            julian_day_utc: None,
            tt_nanoseconds_since_j2000: -3_155_673_600_000_000_001,
            tdb_nanoseconds_since_j2000: -3_155_673_600_000_000_003,
            spk_tdb_seconds_since_j2000: -3_155_673_600.123_456,
            model_julian_day_ut1: 2_415_020.5,
            delta_t_seconds: -2.72,
            delta_t_kind: ephemeris::DeltaTKind::HistoricalModel,
            delta_t_reported_error_seconds: None,
            position: ephemeris::BodyPosition {
                body: "sun",
                source_id: Some(10),
                longitude_deg: 359.999_999_999,
                latitude_deg: -0.0,
                right_ascension_deg: 264.123_456_789,
                declination_deg: -23.25,
                distance_km: Some(149_000_000.123_456),
                longitude_speed_deg_per_day: 0.985_123_456_789,
                latitude_speed_deg_per_day: -0.000_012_345_678_9,
                distance_speed_km_per_day: Some(-240.123_456_789),
            },
        };
        let mut node = physical.clone();
        node.position.body = "mean_node";
        node.position.source_id = None;
        node.position.distance_km = None;
        node.position.distance_speed_km_per_day = None;
        node.position.longitude_speed_deg_per_day = -0.052_953_77;
        vec![physical, node]
    }

    fn content(batch: &RecordBatch, files: &[(&str, Vec<u8>)]) -> Content {
        Content {
            format: FORMAT.into(),
            row_schema_version: 1,
            from: NaiveDate::from_ymd_opt(1900, 1, 1).unwrap(),
            to_exclusive: NaiveDate::from_ymd_opt(1900, 1, 2).unwrap(),
            row_count: batch.num_rows(),
            metadata: serde_json::json!({"provider_version": env!("CARGO_PKG_VERSION"), "algorithm": "synthetic-encoding-fixture"}),
            columns: schema_description(),
            source_manifest_sha256: digest(&files[0].1),
            csv_null: "empty field".into(),
            files: files
                .iter()
                .map(|(name, bytes)| FileIdentity {
                    name: (*name).into(),
                    bytes: bytes.len(),
                    sha256: digest(bytes),
                })
                .collect(),
        }
    }

    fn files(batch: &RecordBatch) -> Vec<(&'static str, Vec<u8>)> {
        vec![
            (
                SOURCE,
                serde_json::to_vec_pretty(&crate::ephemeris_download::expected_manifest().unwrap())
                    .unwrap(),
            ),
            (
                PARQUET,
                archives::parquet_io::encode_parquet(batch).unwrap(),
            ),
            (CSV, csv_bytes(batch).unwrap()),
        ]
    }

    #[test]
    fn ordinary_artifacts_preserve_integer_bits_null_types_identity_and_existing_data() {
        let rows = rows();
        let batch = batch(&rows).unwrap();
        let files = files(&batch);
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("completed");
        let original_content = content(&batch, &files);
        let id = content_digest(&original_content).unwrap();
        publish_new(&dir, &files, original_content, &batch).unwrap();
        validate_readers(&dir, &batch).unwrap();
        let receipt = existing_manifest(
            &dir,
            content(&batch, &files).from,
            content(&batch, &files).to_exclusive,
            &files[0].1,
        )
        .unwrap()
        .unwrap();
        assert_eq!(receipt.snapshot_id, id);
        let ints = batch
            .column(3)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        assert_eq!(ints.value(0), rows[0].tt_nanoseconds_since_j2000);
        assert!(batch.column(2).null_count() == 2 && batch.column(9).null_count() == 2);
        assert_eq!(batch.column(2).data_type(), &DataType::Float64);
        let bytes = read_bounded(&dir.join(PARQUET), MAX_FILE_BYTES).unwrap();
        let modified = std::fs::metadata(dir.join(PARQUET))
            .unwrap()
            .modified()
            .unwrap();
        assert!(publish_new(&dir, &files, content(&batch, &files), &batch).is_err());
        assert_eq!(
            read_bounded(&dir.join(PARQUET), MAX_FILE_BYTES).unwrap(),
            bytes
        );
        assert_eq!(
            std::fs::metadata(dir.join(PARQUET))
                .unwrap()
                .modified()
                .unwrap(),
            modified
        );
        assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 1);
    }

    #[test]
    fn malformed_staged_values_fail_before_promotion_and_remove_only_own_stage() {
        let batch = batch(&rows()).unwrap();
        let parent = tempfile::tempdir().unwrap();
        let unrelated = parent.path().join("unrelated");
        std::fs::write(&unrelated, b"preserve").unwrap();
        for bad_index in [1, 2] {
            let mut files = files(&batch);
            files[bad_index].1 = b"malformed reference artifact".to_vec();
            let dir = parent.path().join("refused");
            // Matching hashes are insufficient: a fresh reader must reject invalid physical data.
            assert!(publish_new(&dir, &files, content(&batch, &files), &batch).is_err());
            assert!(!dir.exists());
            assert_eq!(std::fs::read(&unrelated).unwrap(), b"preserve");
            assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 1);
        }
    }
}

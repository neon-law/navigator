//! Pure preflight: parse only supplied bytes; perform no `FileIO` or network access.
use std::collections::{BTreeSet, HashMap};

use anyhow::{ensure, Context, Result};
use iceberg::spec::{
    DataContentType, DataFileFormat, FormatVersion, Manifest, ManifestContentType, ManifestList,
    ManifestStatus, TableMetadata,
};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

use super::Inventory;
use crate::iceberg::{POSTGRES_FIELD_IDS_PROPERTY, POSTGRES_RELATION_PROPERTY};

struct Closed<'a> {
    root: &'a str,
    attempt: &'a str,
    objects: HashMap<&'a str, &'a [u8]>,
    visited: BTreeSet<String>,
}

impl<'a> Closed<'a> {
    fn bytes(&mut self, uri: &str) -> Result<&'a [u8]> {
        let key = uri
            .strip_prefix(&format!("{}/", self.root))
            .context("foreign Iceberg URI")?;
        ensure!(
            super::operations::safe_key(key) && key.starts_with(&format!("{}/", self.attempt)),
            "Iceberg reference escapes the bound attempt"
        );
        let bytes = self
            .objects
            .get(key)
            .copied()
            .context("Iceberg reference is absent from sealed closure")?;
        self.visited.insert(key.to_owned());
        Ok(bytes)
    }

    fn table(&mut self, uri: &str) -> Result<()> {
        let metadata: TableMetadata = serde_json::from_slice(self.bytes(uri)?)?;
        let location = uri
            .strip_suffix("/metadata/v1.metadata.json")
            .context("unexpected independent metadata version")?;
        ensure!(
            metadata.format_version() == FormatVersion::V2
                && metadata.location() == location
                && metadata.current_snapshot_id() == Some(1)
                && metadata.snapshots().len() == 1
                && metadata.metadata_log().is_empty()
                && metadata.statistics_iter().len() == 0
                && metadata.partition_statistics_iter().len() == 0
                && metadata.encryption_keys_iter().len() == 0,
            "unsupported history/statistics/encryption references in independent snapshot"
        );
        ensure!(
            metadata
                .properties()
                .keys()
                .all(|key| key == POSTGRES_RELATION_PROPERTY || key == POSTGRES_FIELD_IDS_PROPERTY),
            "unsupported independent table properties"
        );
        let snapshot = metadata
            .current_snapshot()
            .context("Iceberg snapshot missing")?;
        let list = ManifestList::parse_with_version(
            self.bytes(snapshot.manifest_list())?,
            FormatVersion::V2,
        )?;
        ensure!(
            list.entries().len() == 1,
            "independent snapshot requires its one complete manifest"
        );
        let file = list.entries().first().context("Iceberg manifest missing")?;
        ensure!(
            file.content == ManifestContentType::Data
                && file.key_metadata.is_none()
                && file.deleted_files_count == Some(0)
                && file.existing_files_count == Some(0),
            "unexpected delete/existing/encrypted manifest content"
        );
        let bytes = self.bytes(&file.manifest_path)?;
        ensure!(
            u64::try_from(file.manifest_length)? == bytes.len() as u64,
            "manifest length mismatch"
        );
        let manifest = Manifest::parse_avro(bytes)?;
        ensure!(
            manifest.entries().len() == 1
                && manifest.metadata().format_version == FormatVersion::V2
                && manifest.metadata().content == ManifestContentType::Data
                && manifest.metadata().schema.as_ref() == metadata.current_schema().as_ref(),
            "manifest schema/content differs from table metadata"
        );
        let entry = manifest
            .entries()
            .first()
            .context("Iceberg data file missing")?;
        ensure!(
            entry.status == ManifestStatus::Added
                && entry.content_type() == DataContentType::Data
                && entry.file_format() == DataFileFormat::Parquet
                && entry.data_file().key_metadata().is_none()
                && entry.data_file().referenced_data_file().is_none()
                && entry.file_path() == format!("{location}/data/snapshot.parquet"),
            "unexpected or foreign data/delete/encryption reference"
        );
        let data = self.bytes(entry.file_path())?;
        let reader = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::copy_from_slice(data))?;
        let rows = u64::try_from(reader.metadata().file_metadata().num_rows())?;
        ensure!(
            entry.file_size_in_bytes() == data.len() as u64
                && entry.record_count() == rows
                && file.added_rows_count == Some(rows)
                && file.added_files_count == Some(1),
            "Parquet size or row count differs from Iceberg manifest"
        );
        let schema = crate::iceberg::arrow_schema_to_iceberg(reader.schema())?;
        ensure!(
            &schema == metadata.current_schema().as_ref(),
            "Parquet field schema differs from Iceberg"
        );
        for property in [POSTGRES_RELATION_PROPERTY, POSTGRES_FIELD_IDS_PROPERTY] {
            ensure!(
                metadata.properties().get(property) == reader.schema().metadata().get(property),
                "PostgreSQL field provenance differs from Parquet"
            );
        }
        if let Some(relation) = reader.schema().metadata().get(POSTGRES_RELATION_PROPERTY) {
            crate::iceberg::validate_postgres_metadata_mapping(&metadata, relation)?;
        }
        Ok(())
    }
}

/// All typed URI references must be bound, supplied, hashed, schema/row consistent
/// BEFORE a reader is constructed or plans a scan. Reject unreferenced extra objects.
pub(super) fn validate(
    inventory: &Inventory,
    root: &str,
    descriptor: &str,
    files: &[(String, Vec<u8>, &'static str)],
) -> Result<()> {
    let attempt = descriptor
        .strip_suffix("/inventory.json")
        .context("invalid attempt inventory")?;
    ensure!(
        !inventory.metadata.is_empty() && inventory.objects.len() == files.len(),
        "incomplete inventory"
    );
    let mut objects = HashMap::new();
    for (key, bytes, _) in files {
        ensure!(
            super::operations::safe_key(key)
                && key.starts_with(&format!("{attempt}/"))
                && objects.insert(key.as_str(), bytes.as_slice()).is_none(),
            "duplicate or foreign object key"
        );
        let declared = inventory
            .objects
            .iter()
            .find(|object| object.key == *key)
            .context("object absent from inventory")?;
        ensure!(
            declared.bytes == bytes.len() && declared.sha256 == crate::postgres::sha256_hex(bytes),
            "object integrity mismatch"
        );
    }
    let mut closed = Closed {
        root,
        attempt,
        objects,
        visited: BTreeSet::new(),
    };
    for metadata in &inventory.metadata {
        closed.table(metadata)?;
    }
    ensure!(
        closed.visited.len() == closed.objects.len(),
        "unreferenced object outside complete Iceberg closure"
    );
    Ok(())
}

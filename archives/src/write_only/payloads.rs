//! Non-table bytes are referenced by rows, not magically included in Parquet.
use anyhow::{Context, Result};
use arrow::array::{Array as _, Int64Array, RecordBatch, StringArray};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct Dependencies {
    /// Only genuine external bytes copied and verified would belong here. This lane copies none.
    pub copied_external_payloads: Vec<store::project_archives::ArchiveItem>,
    pub referenced: Vec<Reference>,
    /// Opaque/provider references and all key dependencies require their own inventory/drill.
    pub references_exhaustive: bool,
    #[serde(rename = "missing_recovery_dependencies")]
    pub missing: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Reference {
    pub data_file: String,
    pub row: usize,
    pub column: String,
    pub object_or_repository: String,
    pub expected_sha256: Option<String>,
    pub expected_size: Option<u64>,
    /// Existing Git receipt, preserved as a reference; no Git bytes are fetched here.
    pub source_backup: Option<store::project_archives::SourceBackupReference>,
}

impl Dependencies {
    pub(crate) fn required() -> Self {
        Self {
            missing: vec![
                "Asset/document/attachment/Notation bytes and archive item payloads are referenced, not copied".into(),
                "Git repositories, bundles and LFS payloads need their separate verified backup".into(),
                "Source/destination KMS keys, recovery IAM/credentials and provider dependencies are not included".into(),
                "Physical PITR, sequence reconciliation, cross-database consistency and restore activation require separate acceptance".into(),
            ],
            ..Self::default()
        }
    }

    /// Read only the actual frozen Parquet. No fresh source-side query or remote blob API.
    pub(crate) fn observe(&mut self, relation: &str, key: &str, parquet: &[u8]) -> Result<()> {
        let columns: &[&str] = match relation {
            "asset" => &["storage_key", "secondary_storage_key"],
            "project" => &["repository_url"],
            "project_archive_generation" => &["manifest_key", "source_backup"],
            "project_archive_item" => &["object_key", "source_key"],
            _ => return Ok(()),
        };
        let reader =
            ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::copy_from_slice(parquet))?
                .build()?;
        let mut offset = 0;
        for batch in reader {
            let batch = batch?;
            for row in 0..batch.num_rows() {
                for column in columns {
                    let Some(value) = text(&batch, column, row)? else {
                        continue;
                    };
                    let source_backup = if *column == "source_backup" {
                        let Ok(receipt) = serde_json::from_str::<
                            store::project_archives::SourceBackupReference,
                        >(&value) else {
                            self.missing.push("A frozen Project source-backup receipt is malformed; its original row remains in Parquet".into());
                            continue;
                        };
                        Some(receipt)
                    } else {
                        None
                    };
                    let object = source_backup
                        .as_ref()
                        .map_or_else(|| value.clone(), |reference| reference.object.clone());
                    self.referenced.push(Reference {
                        data_file: key.to_owned(),
                        row: offset + row,
                        column: (*column).to_owned(),
                        object_or_repository: object,
                        expected_sha256: if let Some(receipt) = &source_backup {
                            Some(receipt.sha256.clone())
                        } else {
                            text(
                                &batch,
                                match relation {
                                    "asset" => "sha256_hex",
                                    "project_archive_generation" => "manifest_sha256",
                                    _ => "sha256",
                                },
                                row,
                            )?
                        },
                        expected_size: source_backup
                            .as_ref()
                            .map(|receipt| receipt.size_bytes)
                            .or_else(|| {
                                size(
                                    &batch,
                                    match relation {
                                        "asset" => "byte_size",
                                        "project_archive_generation" => "manifest_size_bytes",
                                        _ => "size_bytes",
                                    },
                                    row,
                                )
                            }),
                        source_backup,
                    });
                }
            }
            offset += batch.num_rows();
        }
        Ok(())
    }
}

fn text(batch: &RecordBatch, column: &str, row: usize) -> Result<Option<String>> {
    let Some(array) = batch.column_by_name(column) else {
        return Ok(None);
    };
    if array.is_null(row) {
        return Ok(None);
    }
    let array = array
        .as_any()
        .downcast_ref::<StringArray>()
        .context("frozen payload reference must retain its declared text/JSON type")?;
    Ok(Some(array.value(row).to_owned()))
}

fn size(batch: &RecordBatch, column: &str, row: usize) -> Option<u64> {
    let array = batch
        .column_by_name(column)?
        .as_any()
        .downcast_ref::<Int64Array>()?;
    if array.is_null(row) {
        None
    } else {
        u64::try_from(array.value(row)).ok()
    }
}

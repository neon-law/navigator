//! Independent Iceberg snapshot bundles; destination credentials only create objects.
//!
//! This does not publish a catalog or certify retention/recovery. A separately
//! privileged verifier consumes the explicit inventory, never a mutable latest key.
use std::{collections::HashMap, path::Path};

use anyhow::{ensure, Context, Result};
use cloud::StorageService;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde::{Deserialize, Serialize};
use store::postgres::PostgresDb;
use tokio::io::AsyncWriteExt as _;
use uuid::Uuid;

use crate::iceberg::{author_snapshot, DataFileSpec, SnapshotInput};

/// One transaction's frozen rows. No destination credentials are used to capture it.
pub(crate) struct Capture(
    crate::postgres::Extracted,
    Option<crate::coverage::PairedManifest>,
);

impl Capture {
    /// Export the registered Navigator domain tables under normal application admission.
    /// DBOS system history, Forgejo, physical PITR, and restore activation are separate.
    pub(crate) async fn navigator(pg: &PostgresDb) -> Result<Self> {
        PostgresDb::with_application_db(pg.clone(), async {
            crate::postgres::extract(
                pg.pool(),
                crate::postgres::POSTGRES_TABLES,
                &crate::postgres::NoPause,
                &HashMap::new(),
            )
            .await
            .map(|extracted| Self(extracted, None))
        })
        .await?
    }

    /// Reuse the existing full reviewed relation extractor for both databases.
    /// Each database has its own RR watermark; this is NOT a cross-database snapshot.
    pub(crate) async fn paired(
        navigator: crate::coverage::DatabaseSource<'_>,
        forgejo: crate::coverage::DatabaseSource<'_>,
    ) -> Result<Self> {
        Self::paired_with_bounds(navigator, forgejo, None).await
    }

    pub(crate) async fn paired_with_bounds(
        navigator: crate::coverage::DatabaseSource<'_>,
        forgejo: crate::coverage::DatabaseSource<'_>,
        bounds: Option<&crate::postgres::ExportBounds>,
    ) -> Result<Self> {
        ensure!(
            navigator.name == "navigator" && forgejo.name == "forgejo",
            "invalid paired source names"
        );
        let generation = Uuid::new_v4();
        let mut databases = Vec::new();
        let mut tables = Vec::new();
        let mut encoded_bytes = 0usize;
        for source in [navigator, forgejo] {
            let mut extracted =
                crate::coverage::extract_database_with_bounds(source, generation, bounds).await?;
            let mut objects: HashMap<_, _> = extracted.objects.into_iter().collect();
            for entry in &mut extracted.manifest.tables {
                let bytes = objects
                    .remove(&entry.key)
                    .context("missing extracted coverage object")?;
                let encoded = entry
                    .key
                    .split("/data/")
                    .next()
                    .and_then(|path| path.rsplit('/').next())
                    .context("invalid extracted coverage identity")?;
                let name = format!("{}_{encoded}", extracted.manifest.database);
                // This capture-local identity is rebound to the final destination path below.
                entry.key.clone_from(&name);
                encoded_bytes = encoded_bytes
                    .checked_add(bytes.len())
                    .context("capture byte count overflow")?;
                if let Some(bounds) = bounds {
                    ensure!(
                        encoded_bytes <= bounds.generation_bytes,
                        "capture byte bound exceeded"
                    );
                }
                tables.push((name, entry.rows, bytes));
            }
            ensure!(
                objects.is_empty(),
                "unclassified extracted coverage objects"
            );
            databases.push(extracted.manifest);
        }
        ensure!(
            databases[0].database_name != databases[1].database_name,
            "paired databases must differ"
        );
        let extracted = crate::postgres::Extracted {
            cutoff_at: databases[0].cutoff_at,
            snapshot: "paired; see individual database watermarks".to_owned(),
            server_version: databases[0].server_version.clone(),
            tables,
        };
        Ok(Self(
            extracted,
            Some(crate::coverage::PairedManifest {
                format_version: 1,
                generation,
                databases,
                cross_database_atomic: false,
            }),
        ))
    }

    /// Author a complete independent Iceberg closure for THIS destination URI.
    /// Reauthor for each destination; copying metadata from another bucket is unsafe.
    pub(crate) async fn bundle(&self, root_uri: &str) -> Result<Bundle> {
        ensure!(
            (root_uri.starts_with("gs://") || root_uri.starts_with("file:///"))
                && !root_uri.ends_with('/')
                && !root_uri.contains(['?', '#', '\n', '\r', '\\'])
                && !root_uri.split('/').any(|part| part == "." || part == ".."),
            "archive root must be an absolute file or GCS URI without traversal"
        );
        self.bundle_at(root_uri, &format!("attempts/{}", Uuid::new_v4()))
            .await
    }

    pub(crate) async fn bundle_at(&self, root_uri: &str, prefix: &str) -> Result<Bundle> {
        ensure!(
            operations::safe_key(prefix),
            "invalid archive attempt identity"
        );
        let mut bundle = Bundle {
            root: root_uri.to_owned(),
            inventory: Inventory {
                format_version: 1,
                cutoff_at: self.0.cutoff_at,
                snapshot: self.0.snapshot.clone(),
                source_identity_sha256: None,
                forgejo_identity_sha256: None,
                local_generation_sha256: None,
                server_version: self.0.server_version.clone(),
                objects: Vec::new(),
                metadata: Vec::new(),
                paired: self.1.clone(),
                dependencies: payloads::Dependencies::required(),
            },
            files: Vec::new(),
            descriptor: format!("{prefix}/inventory.json"),
        };
        for (table, rows, bytes) in &self.0.tables {
            let table_key = format!("{prefix}/{table}");
            let data_key = format!("{table_key}/data/snapshot.parquet");
            let reader =
                ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::copy_from_slice(bytes))?;
            let data = [DataFileSpec {
                path: format!("{root_uri}/{data_key}"),
                record_count: (*rows).try_into()?,
                file_size_in_bytes: bytes.len().try_into()?,
            }];
            let table_location = format!("{root_uri}/{table_key}");
            let authored = author_snapshot(&SnapshotInput {
                table,
                arrow_schema: reader.schema(),
                table_location: &table_location,
                data_files: &data,
                snapshot_id: 1,
                sequence_number: 1,
                timestamp_ms: self.0.cutoff_at.timestamp_millis(),
                version: 1,
                prior: None,
                expire_before_ms: None,
            })
            .await?;
            let relation = self
                .1
                .as_ref()
                .and_then(|paired| {
                    paired
                        .databases
                        .iter()
                        .flat_map(|database| &database.tables)
                        .find(|entry| entry.key == *table)
                })
                .and_then(|entry| entry.table.strip_prefix("public."))
                .unwrap_or(table);
            bundle
                .inventory
                .dependencies
                .observe(relation, &data_key, bytes)?;
            bundle.add(
                data_key.clone(),
                bytes.clone(),
                "application/vnd.apache.parquet",
            );
            if let Some(paired) = &mut bundle.inventory.paired {
                for database in &mut paired.databases {
                    for entry in &mut database.tables {
                        if entry.key == *table {
                            entry.key.clone_from(&data_key);
                            database
                                .iceberg_metadata_keys
                                .push(format!("{table_key}/metadata/v1.metadata.json"));
                        }
                    }
                }
            }
            bundle.inventory.metadata.push(authored.metadata_location);
            for (relative, bytes) in authored.files {
                // No mutable hint or latest pointer, including for a create-only writer.
                if relative != "metadata/version-hint.text" {
                    let content_type = object_content_type(&relative)?;
                    bundle.add(format!("{table_key}/{relative}"), bytes, content_type);
                }
            }
        }
        Ok(bundle)
    }
}

fn object_content_type(key: &str) -> Result<&'static str> {
    match Path::new(key).extension().and_then(std::ffi::OsStr::to_str) {
        Some("parquet") => Ok("application/vnd.apache.parquet"),
        Some("avro") => Ok("application/avro"),
        Some("json") => Ok("application/json"),
        _ => anyhow::bail!("unsupported archive object type"),
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ObjectDigest {
    pub key: String,
    pub bytes: usize,
    pub sha256: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Inventory {
    pub format_version: u8,
    pub cutoff_at: chrono::DateTime<chrono::Utc>,
    pub snapshot: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_identity_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forgejo_identity_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_generation_sha256: Option<String>,
    pub server_version: String,
    pub objects: Vec<ObjectDigest>,
    /// Exact immutable table metadata URIs; a verifier need not list a bucket.
    pub metadata: Vec<String>,
    /// Complete coverage/schema/non-MVCC records, when paired extraction was requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paired: Option<crate::coverage::PairedManifest>,
    pub dependencies: payloads::Dependencies,
}

/// Prepared bytes; immutable attempt prefix and destination-specific references.
pub(crate) struct Bundle {
    root: String,
    inventory: Inventory,
    files: Vec<(String, Vec<u8>, &'static str)>,
    descriptor: String,
}

/// Upload acknowledgements only. NOT catalog promotion, read verification or retention proof.
#[derive(Debug)]
pub(crate) struct Acknowledged {
    pub inventory_uri: String,
    pub inventory_key: String,
    pub generation: u64,
}

/// An error may occur AFTER the server creates an object. Never infer its absence,
/// read it back with writer credentials, or publish a successful generation.
#[derive(Debug, thiserror::Error)]
#[error("archive destination upload unconfirmed after {acknowledged} acknowledgements")]
pub(crate) struct Unconfirmed {
    pub inventory_uri: String,
    /// Attempt-scoped inventory key for the separate verifier, even if its ACK was lost.
    pub inventory_key: String,
    pub acknowledged: usize,
}

impl Bundle {
    fn unconfirmed(&self, acknowledged: usize) -> Unconfirmed {
        Unconfirmed {
            inventory_uri: format!("{}/{}", self.root, self.descriptor),
            inventory_key: self.descriptor.clone(),
            acknowledged,
        }
    }

    fn add(&mut self, key: String, bytes: Vec<u8>, content_type: &'static str) {
        self.inventory.objects.push(ObjectDigest {
            key: key.clone(),
            bytes: bytes.len(),
            sha256: crate::postgres::sha256_hex(&bytes),
        });
        self.files.push((key, bytes, content_type));
    }

    /// Create raw local files at their actual file:// location, without .bin sidecars.
    /// Caller retains failed/partial attempts for diagnosis; nothing is overwritten/deleted.
    #[cfg(test)]
    pub(crate) async fn write_local(&self, root: &Path) -> Result<()> {
        let absolute = root
            .canonicalize()
            .context("local archive root must already exist")?;
        ensure!(
            self.root == format!("file://{}", absolute.display()),
            "local Iceberg URI must bind the actual local root"
        );
        self.write_files(&absolute).await
    }

    async fn write_files(&self, absolute: &Path) -> Result<()> {
        // Claim a new attempt directory before any writes. Existing attempts and
        // their possible symlinks are never adopted. The caller owns this root.
        let attempt = self
            .descriptor
            .strip_suffix("/inventory.json")
            .context("archive attempt inventory path")?;
        let attempts = absolute.join("attempts");
        match tokio::fs::create_dir(&attempts).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = tokio::fs::symlink_metadata(&attempts).await?;
                ensure!(
                    metadata.file_type().is_dir(),
                    "archive attempts must not be a symlink"
                );
            }
            Err(error) => return Err(error.into()),
        }
        tokio::fs::create_dir(absolute.join(attempt)).await?;
        for (key, bytes, _) in &self.files {
            local_object(&absolute.join(key), bytes).await?;
        }
        // Persist every directory entry before the final inventory commit point.
        sync_object_directories(absolute, &self.files).await?;
        let descriptor = absolute.join(&self.descriptor);
        local_object(&descriptor, &serde_json::to_vec(&self.inventory)?).await?;
        sync_parents(absolute, &descriptor).await
    }

    /// Check the local raw-file copy against this sealed, captured closure.
    /// This is a local reader operation, never delegated to a cloud writer.
    pub(crate) async fn verify_local(&self, root: &Path) -> Result<()> {
        let inventory = tokio::fs::read(root.join(&self.descriptor)).await?;
        ensure!(
            inventory == serde_json::to_vec(&self.inventory)?,
            "local inventory differs"
        );
        for entry in &self.inventory.objects {
            let bytes = tokio::fs::read(root.join(&entry.key)).await?;
            ensure!(
                bytes.len() == entry.bytes && crate::postgres::sha256_hex(&bytes) == entry.sha256,
                "local archive object integrity mismatch"
            );
        }
        closure::validate(&self.inventory, &self.root, &self.descriptor, &self.files)?;
        Ok(())
    }

    /// Only create-only object RPCs. Never calls policy/project/get/head/list/delete/put.
    /// Provisioning must bind this handle to the independently approved destination.
    async fn upload(&self, destination: &dyn StorageService) -> Result<Acknowledged, Unconfirmed> {
        let mut acknowledged = 0;
        for (key, bytes, content_type) in &self.files {
            let created = destination
                .put_immutable(key, bytes, content_type)
                .await
                .map_err(|_| self.unconfirmed(acknowledged))?;
            if created.generation == 0 || created.size_bytes != bytes.len() as u64 {
                return Err(self.unconfirmed(acknowledged));
            }
            acknowledged += 1;
        }
        let descriptor =
            serde_json::to_vec(&self.inventory).map_err(|_| self.unconfirmed(acknowledged))?;
        let created = destination
            .put_immutable(&self.descriptor, &descriptor, "application/json")
            .await
            .map_err(|_| self.unconfirmed(acknowledged))?;
        if created.generation == 0 || created.size_bytes != descriptor.len() as u64 {
            return Err(self.unconfirmed(acknowledged));
        }
        Ok(Acknowledged {
            inventory_uri: format!("{}/{}", self.root, self.descriptor),
            inventory_key: self.descriptor.clone(),
            generation: created.generation,
        })
    }
}

async fn local_object(path: &Path, bytes: &[u8]) -> Result<()> {
    tokio::fs::create_dir_all(path.parent().context("archive object parent")?).await?;
    let mut file = tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .await?;
    file.write_all(bytes).await?;
    file.sync_all().await?;
    Ok(())
}

pub mod operations;

#[cfg(test)]
pub(crate) mod tests;

pub(crate) mod payloads;

mod closure;

fn safe_bucket(bucket: &str) -> bool {
    (3..=222).contains(&bucket.len())
        && bucket.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
        && bucket.as_bytes()[0].is_ascii_alphanumeric()
        && bucket.as_bytes()[bucket.len() - 1].is_ascii_alphanumeric()
}

/// Public operator boundaries never expose SQL values or cloud error chains.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("archive configuration is invalid")]
    Configuration,
    #[error("archive source identity is unavailable")]
    Source,
    #[error("sealed archive generation is unavailable or changed")]
    Generation,
    #[error("archive verification refused")]
    Verification,
}

async fn sync_parents(root: &Path, file: &Path) -> Result<()> {
    let mut directory = file.parent().context("archive object parent missing")?;
    loop {
        ensure!(
            directory.starts_with(root),
            "archive directory escapes owned root"
        );
        tokio::fs::File::open(directory).await?.sync_all().await?;
        if directory == root {
            return Ok(());
        }
        directory = directory.parent().context("archive root missing")?;
    }
}

async fn sync_object_directories(
    root: &Path,
    files: &[(String, Vec<u8>, &'static str)],
) -> Result<()> {
    let mut directories = std::collections::BTreeSet::new();
    for (key, _, _) in files {
        let file = root.join(key);
        let mut directory = file.parent().context("archive object parent missing")?;
        loop {
            ensure!(
                directory.starts_with(root),
                "archive directory escapes owned root"
            );
            directories.insert(directory.to_owned());
            if directory == root {
                break;
            }
            directory = directory.parent().context("archive root missing")?;
        }
    }
    for directory in directories.into_iter().rev() {
        tokio::fs::File::open(directory).await?.sync_all().await?;
    }
    Ok(())
}

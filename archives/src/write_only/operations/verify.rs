//! Read/control-plane identity, run separately from the backup writer.
use std::collections::BTreeSet;

use anyhow::{ensure, Context, Result};
use cloud::StorageService as _;
use futures::TryStreamExt as _;
use serde::{Deserialize, Serialize};

use super::{Config, LocalReceipt};
use crate::write_only::Inventory;

#[derive(Debug, Serialize, Deserialize)]
pub struct QueryCount {
    pub metadata_uri: String,
    pub rows: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Verification {
    pub inventory_uri: String,
    pub inventory_sha256: String,
    pub objects: usize,
    pub query_counts: Vec<QueryCount>,
    /// Project/UBLA/locked retention checked by the verifier, not by the writer.
    pub bucket_policy_checked: bool,
    /// Effective inherited IAM and writer credential-denial acceptance is separate.
    pub writer_authorization_review_required: bool,
    /// This operation reads and verifies only. It never promotes a catalog.
    pub catalog_promoted: bool,
    pub restore_scope: RestoreScope,
    pub payloads: PayloadCoverage,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PayloadCoverage {
    pub copied: usize,
    pub referenced: usize,
    pub references_exhaustive: bool,
    pub missing_recovery_dependencies: Vec<String>,
}

impl From<&crate::write_only::payloads::Dependencies> for PayloadCoverage {
    fn from(value: &crate::write_only::payloads::Dependencies) -> Self {
        Self {
            copied: value.copied_external_payloads.len(),
            referenced: value.referenced.len(),
            references_exhaustive: value.references_exhaustive,
            missing_recovery_dependencies: value.missing.clone(),
        }
    }
}

/// A read-only scope inventory, not authorization to restore or activate a target.
#[derive(Debug, Serialize, Deserialize)]
pub struct RestoreScope {
    pub capture_kind: String,
    pub source_identity_sha256: Option<String>,
    pub forgejo_identity_sha256: Option<String>,
    pub cutoff_at: String,
    pub snapshot: String,
    /// Exact captured schemas, relation coverage, per-database cutoffs and repair obligations.
    pub paired_manifest: Option<serde_json::Value>,
    pub target_writes_authorized: bool,
    pub activation_accepted: bool,
    pub provider_effect_recovery_accepted: bool,
}

fn restore_scope(inventory: &Inventory) -> Result<RestoreScope> {
    Ok(RestoreScope {
        capture_kind: if inventory.paired.is_some() {
            "paired_relational"
        } else {
            "registered_navigator"
        }
        .into(),
        source_identity_sha256: inventory.source_identity_sha256.clone(),
        forgejo_identity_sha256: inventory.forgejo_identity_sha256.clone(),
        cutoff_at: inventory.cutoff_at.to_string(),
        snapshot: inventory.snapshot.clone(),
        paired_manifest: inventory
            .paired
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?,
        target_writes_authorized: false,
        activation_accepted: false,
        provider_effect_recovery_accepted: false,
    })
}

async fn queries(io: iceberg::io::FileIO, inventory: &Inventory) -> Result<Vec<QueryCount>> {
    let mut counts = Vec::new();
    for metadata in &inventory.metadata {
        let table = iceberg::table::StaticTable::from_metadata_file(
            metadata,
            iceberg::TableIdent::from_strs(["backup", "verification"])?,
            io.clone(),
        )
        .await?;
        let tasks = table.scan().build()?.plan_files().await?;
        let batches: Vec<_> = table
            .reader_builder()
            .build()
            .read(tasks)?
            .stream()
            .try_collect()
            .await?;
        counts.push(QueryCount {
            metadata_uri: metadata.clone(),
            rows: batches.into_iter().map(|batch| batch.num_rows()).sum(),
        });
    }
    if let Some(paired) = &inventory.paired {
        ensure!(
            !paired.cross_database_atomic && paired.databases.len() == 2,
            "invalid paired coverage claim"
        );
        for database in &paired.databases {
            ensure!(
                database.tables.len() == database.iceberg_metadata_keys.len(),
                "incomplete per-table metadata closure"
            );
            for (table, metadata) in database.tables.iter().zip(&database.iceberg_metadata_keys) {
                let count = counts
                    .iter()
                    .find(|count| count.metadata_uri.ends_with(&format!("/{metadata}")))
                    .context("paired Iceberg table is missing")?;
                ensure!(
                    count.rows == table.rows,
                    "Iceberg query row count differs from captured relation"
                );
                let object = inventory
                    .objects
                    .iter()
                    .find(|object| object.key == table.key)
                    .context("paired Parquet object is missing")?;
                ensure!(
                    object.bytes == table.bytes && object.sha256 == table.sha256,
                    "paired relation digest differs from sealed object"
                );
            }
        }
    }
    Ok(counts)
}

pub async fn local(
    receipt: &LocalReceipt,
    config: &Config,
) -> std::result::Result<Verification, crate::write_only::Error> {
    let result: anyhow::Result<_> = async {
        let bundle = super::open_local(receipt, &config.local_root).await?;
        super::validate_sources(&bundle.inventory, config)?;
        ensure!(
            bundle.root == format!("file://{}", receipt.root.display()),
            "not a raw readable local backup"
        );
        from_bundle(bundle, receipt).await
    }
    .await;
    result.map_err(|_| crate::write_only::Error::Verification)
}

/// Query an exact sealed source generation; no destination credential is used.
pub async fn sealed(
    receipt: &LocalReceipt,
    config: &Config,
) -> std::result::Result<Verification, crate::write_only::Error> {
    if let Some(object) = &config.object_store {
        let result: Result<_> = async {
            let store = super::object_store::connect(object).await?;
            let bundle = super::object_store::open(store.as_ref(), config, receipt).await?;
            from_bundle(bundle, receipt).await
        }
        .await;
        result.map_err(|_| crate::write_only::Error::Verification)
    } else {
        local(receipt, config).await
    }
}

pub(crate) async fn from_bundle(
    bundle: super::Bundle,
    receipt: &LocalReceipt,
) -> Result<Verification> {
    // Query the exact validated bytes, not a second mutable filesystem read.
    let io = iceberg::io::FileIO::new_with_memory();
    for (key, bytes, _) in &bundle.files {
        io.new_output(format!("{}/{}", bundle.root, key))?
            .write(bytes.clone().into())
            .await?;
    }
    Ok(Verification {
        inventory_uri: format!("{}/{}", bundle.root, bundle.descriptor),
        inventory_sha256: receipt.inventory_sha256.clone(),
        objects: bundle.files.len(),
        query_counts: queries(io, &bundle.inventory).await?,
        bucket_policy_checked: false,
        writer_authorization_review_required: true,
        catalog_promoted: false,
        payloads: (&bundle.inventory.dependencies).into(),
        restore_scope: restore_scope(&bundle.inventory)?,
    })
}

fn integer(value: &serde_json::Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}

pub(super) fn policy(value: &serde_json::Value, destination: &super::Destination) -> Result<()> {
    ensure!(
        value["location"]
            .as_str()
            .is_some_and(|location| location.eq_ignore_ascii_case(&destination.location)),
        "wrong destination location"
    );
    ensure!(
        integer(&value["projectNumber"]) == Some(destination.project_number),
        "wrong destination project"
    );
    ensure!(
        value["iamConfiguration"]["uniformBucketLevelAccess"]["enabled"] == true,
        "archive destination requires uniform bucket-level access"
    );
    ensure!(
        value["iamConfiguration"]["publicAccessPrevention"] == "enforced",
        "archive destination requires public access prevention"
    );
    ensure!(
        value["retentionPolicy"]["isLocked"] == true
            && integer(&value["retentionPolicy"]["retentionPeriod"])
                .is_some_and(|seconds| seconds >= destination.minimum_retention_seconds),
        "archive destination retention is not locked to the required duration"
    );
    Ok(())
}

/// Uses this process's separate verifier ADC. No writer handle is accepted here.
/// The recorded inventory generation and SHA bind the exact acknowledgement/local intent.
pub async fn cloud(
    config: &Config,
    destination: usize,
    inventory_key: &str,
    generation: Option<u64>,
    inventory_sha256: &str,
    source_inventory_sha256: &str,
) -> std::result::Result<Verification, crate::write_only::Error> {
    let result: anyhow::Result<_> = async {
        config.validate()?;
        ensure!(
            super::safe_key(inventory_key) && generation != Some(0),
            "invalid exact inventory receipt"
        );
        let target = config
            .destinations
            .get(destination)
            .context("unknown destination")?;
        let storage = cloud::GcsStorage::archive_from_adc(target.bucket.clone()).await?;
        policy(&storage.archive_policy_for_verifier().await?, target)?;
        let generation = match generation {
            Some(generation) => generation,
            None => {
                storage
                    .archive_generation_for_verifier(inventory_key)
                    .await?
            }
        };
        let descriptor = storage.get_generation(inventory_key, generation).await?;
        ensure!(
            crate::postgres::sha256_hex(&descriptor.object.bytes) == inventory_sha256,
            "destination inventory differs from sealed local intent"
        );
        let inventory: Inventory = serde_json::from_slice(&descriptor.object.bytes)?;
        super::validate_sources(&inventory, config)?;
        ensure!(
            inventory.local_generation_sha256.as_deref() == Some(source_inventory_sha256),
            "remote attempt belongs to a different local generation"
        );
        ensure!(
            inventory.format_version == 1 && !inventory.metadata.is_empty(),
            "unsupported or empty archive inventory"
        );
        let attempt = inventory_key
            .strip_suffix("/inventory.json")
            .context("invalid inventory key")?;
        let mut seen = BTreeSet::new();
        let mut files = Vec::new();
        let root = format!("gs://{}", target.bucket);
        for entry in &inventory.objects {
            ensure!(
                super::safe_key(&entry.key)
                    && entry.key.starts_with(&format!("{attempt}/"))
                    && seen.insert(&entry.key),
                "foreign or duplicate archive object"
            );
            let object = storage.get(&entry.key).await?;
            ensure!(
                object.bytes.len() == entry.bytes
                    && crate::postgres::sha256_hex(&object.bytes) == entry.sha256,
                "destination archive object integrity mismatch"
            );
            files.push((entry.key.clone(), object.bytes, "application/octet-stream"));
        }
        for metadata in &inventory.metadata {
            let key = metadata
                .strip_prefix(&format!("{root}/"))
                .context("foreign metadata location")?;
            ensure!(
                seen.contains(&key.to_owned()),
                "metadata is outside the verified closure"
            );
        }
        // Pure metadata + Avro + Parquet validation precedes ANY StaticTable scan.
        super::super::closure::validate(&inventory, &root, inventory_key, &files)?;
        let io = iceberg::io::FileIO::new_with_memory();
        for (key, bytes, _) in files {
            io.new_output(format!("{root}/{key}"))?
                .write(bytes.into())
                .await?;
        }
        let query_counts = queries(io, &inventory).await?;
        // Recheck the control-plane state after reading the complete closure.
        policy(&storage.archive_policy_for_verifier().await?, target)?;
        Ok(Verification {
            inventory_uri: format!("{root}/{inventory_key}"),
            inventory_sha256: inventory_sha256.to_owned(),
            objects: inventory.objects.len(),
            query_counts,
            bucket_policy_checked: true,
            writer_authorization_review_required: true,
            catalog_promoted: false,
            payloads: (&inventory.dependencies).into(),
            restore_scope: restore_scope(&inventory)?,
        })
    }
    .await;
    result.map_err(|_| crate::write_only::Error::Verification)
}

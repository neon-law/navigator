//! Explicit immutable configuration and versioned sealed receipts for operator/DBOS use.
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};

use super::{Acknowledged, Bundle, Capture, Inventory, Unconfirmed};

/// Non-secret configuration. Provisioning and verification credentials are NOT writer config.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub format_version: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_store: Option<object_store::Config>,
    /// Version-one local backup root; empty for the version-two object-backed source.
    #[serde(default)]
    pub local_root: PathBuf,
    /// None explicitly denotes a non-GCP source (for example local PostgreSQL).
    pub source_project_number: Option<u64>,
    pub source_identity_sha256: String,
    pub capture: CaptureScope,
    pub destinations: Vec<Destination>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum CaptureScope {
    RegisteredNavigator,
    Paired {
        coverage: crate::coverage::ConfiguredCoverage,
        forgejo_identity_sha256: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Destination {
    pub project_number: u64,
    pub bucket: String,
    pub location: String,
    /// Checked by the separate verifier, never by a create-only writer.
    pub minimum_retention_seconds: u64,
}

impl Config {
    pub fn validate(&self) -> std::result::Result<(), crate::write_only::Error> {
        self.validate_inner()
            .map_err(|_| crate::write_only::Error::Configuration)
    }

    fn validate_inner(&self) -> anyhow::Result<()> {
        match (&self.object_store, self.format_version) {
            (None, 1) => ensure!(
                self.local_root.is_absolute(),
                "persistent local root must be absolute"
            ),
            (Some(store), 2) => {
                store.validate()?;
                ensure!(
                    self.local_root.as_os_str().is_empty(),
                    "object storage must not claim a local mount"
                );
            }
            _ => bail!("unsupported archive configuration/storage version"),
        }
        ensure!(
            self.source_project_number != Some(0),
            "source project identity is required"
        );
        ensure!(
            self.source_identity_sha256.len() == 64
                && self
                    .source_identity_sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "source identity SHA256 is required"
        );
        if let CaptureScope::Paired {
            forgejo_identity_sha256,
            ..
        } = &self.capture
        {
            ensure!(
                forgejo_identity_sha256.len() == 64
                    && forgejo_identity_sha256
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
                "Forgejo source identity SHA256 is required"
            );
        }
        if let Some(store) = &self.object_store {
            let tables = match &self.capture {
                CaptureScope::RegisteredNavigator => crate::postgres::POSTGRES_TABLES.len(),
                CaptureScope::Paired { coverage, .. } => coverage
                    .navigator
                    .relations
                    .len()
                    .checked_add(coverage.forgejo.relations.len())
                    .context("coverage count overflow")?,
            };
            ensure!(
                tables
                    .checked_mul(4)
                    .is_some_and(|objects| objects <= store.max_objects),
                "capture coverage exceeds object budget"
            );
        }
        let mut projects = BTreeSet::new();
        let mut buckets = BTreeSet::new();
        ensure!(
            self.destinations.len() <= 4,
            "too many archive destinations"
        );
        for destination in &self.destinations {
            ensure!(
                self.object_store
                    .as_ref()
                    .is_none_or(|store| store.bucket != destination.bucket),
                "sealed source must not share an offsite bucket"
            );
            ensure!(
                !destination.location.is_empty()
                    && destination.location.len() <= 63
                    && destination
                        .location
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
                "explicit destination location is required"
            );
            ensure!(
                destination.project_number > 0
                    && Some(destination.project_number) != self.source_project_number
                    && projects.insert(destination.project_number),
                "archive destinations must be independent projects"
            );
            ensure!(
                destination.minimum_retention_seconds
                    >= cloud::archives_project::OFFSITE_ARCHIVES_RETENTION_SECONDS,
                "retention requirement is missing"
            );
            ensure!(
                super::safe_bucket(&destination.bucket) && buckets.insert(&destination.bucket),
                "archive destination buckets must be distinct"
            );
        }
        Ok(())
    }

    /// Digest is bound into the request/checkpoint. Later environment changes cannot retarget it.
    pub fn digest(&self) -> std::result::Result<String, crate::write_only::Error> {
        self.validate()?;
        let bytes =
            serde_json::to_vec(self).map_err(|_| crate::write_only::Error::Configuration)?;
        Ok(crate::postgres::sha256_hex(&bytes))
    }
}

/// Checkpointed sealed inventory identity with explicit local or versioned object backing.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LocalReceipt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_store: Option<object_store::Receipt>,
    pub root: PathBuf,
    pub root_uri: String,
    pub inventory_key: String,
    pub inventory_sha256: String,
    pub cutoff_at: chrono::DateTime<chrono::Utc>,
    pub tables: usize,
    pub rows: usize,
}

pub(super) fn safe_key(key: &str) -> bool {
    !key.is_empty()
        && key.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        })
}

/// A receipt can only resolve inside the explicitly configured persistent root.
pub(crate) async fn open_local(receipt: &LocalReceipt, configured_root: &Path) -> Result<Bundle> {
    ensure!(
        receipt.object_store.is_none(),
        "object-backed receipt cannot be opened as local files"
    );
    let root = configured_root
        .canonicalize()
        .context("persistent archive storage is unavailable")?;
    ensure!(
        receipt.root == root && safe_key(&receipt.inventory_key),
        "foreign local archive receipt"
    );
    let descriptor = read_local_object(&root, &receipt.inventory_key)
        .await
        .context("sealed local archive inventory is missing")?;
    ensure!(
        crate::postgres::sha256_hex(&descriptor) == receipt.inventory_sha256,
        "sealed local archive inventory changed"
    );
    let inventory: Inventory = serde_json::from_slice(&descriptor)?;
    ensure!(
        inventory.format_version == 1,
        "unknown local inventory format"
    );
    ensure!(
        inventory.cutoff_at == receipt.cutoff_at,
        "local cutoff differs from checkpoint"
    );
    let attempt = receipt
        .inventory_key
        .strip_suffix("/inventory.json")
        .context("invalid local inventory key")?;
    let mut seen = BTreeSet::new();
    let mut files = Vec::new();
    for entry in &inventory.objects {
        ensure!(
            safe_key(&entry.key)
                && entry.key.starts_with(&format!("{attempt}/"))
                && seen.insert(&entry.key),
            "invalid or duplicate local archive object"
        );
        let bytes = read_local_object(&root, &entry.key)
            .await
            .context("sealed local archive object is missing")?;
        ensure!(
            bytes.len() == entry.bytes && crate::postgres::sha256_hex(&bytes) == entry.sha256,
            "sealed local archive object changed"
        );
        let content_type = super::object_content_type(&entry.key)?;
        files.push((entry.key.clone(), bytes, content_type));
    }
    ensure!(
        !inventory.metadata.is_empty(),
        "local archive has no Iceberg tables"
    );
    for uri in &inventory.metadata {
        let key = uri
            .strip_prefix(&format!("{}/", receipt.root_uri))
            .context("foreign Iceberg metadata URI")?;
        ensure!(
            seen.contains(&key.to_owned()),
            "Iceberg metadata is outside the sealed closure"
        );
    }
    let (tables, rows) = cardinality(&files)?;
    ensure!(
        tables == receipt.tables && rows == receipt.rows,
        "local row counts differ from checkpoint"
    );
    super::closure::validate(
        &inventory,
        &receipt.root_uri,
        &receipt.inventory_key,
        &files,
    )?;
    Ok(Bundle {
        root: receipt.root_uri.clone(),
        inventory,
        files,
        descriptor: receipt.inventory_key.clone(),
    })
}

async fn seal(bundle: &Bundle, root: &Path) -> Result<LocalReceipt> {
    bundle.write_files(root).await?;
    bundle.verify_local(root).await?;
    let (tables, rows) = cardinality(&bundle.files)?;
    Ok(LocalReceipt {
        object_store: None,
        cutoff_at: bundle.inventory.cutoff_at,
        tables,
        rows,
        root: root.to_owned(),
        root_uri: bundle.root.clone(),
        inventory_key: bundle.descriptor.clone(),
        inventory_sha256: crate::postgres::sha256_hex(&serde_json::to_vec(&bundle.inventory)?),
    })
}

/// Capture once or reuse ONLY the exact journaled closure. No missing-copy re-extraction.
pub(crate) async fn prepare(
    pg: Option<&store::postgres::PostgresDb>,
    config: &Config,
    existing: Option<&LocalReceipt>,
) -> Result<LocalReceipt> {
    config.validate()?;
    let root = config
        .local_root
        .canonicalize()
        .context("configured persistent root must exist")?;
    if let Some(existing) = existing {
        let bundle = open_local(existing, &root).await?;
        validate_sources(&bundle.inventory, config)?;
        ensure!(
            bundle.root == format!("file://{}", root.display()),
            "recovery requires a local readable snapshot"
        );
        ensure!(
            bundle.inventory.source_identity_sha256.as_ref()
                == Some(&config.source_identity_sha256),
            "local generation belongs to a different source identity"
        );
        return Ok(existing.clone());
    }
    let pg = pg.context("source PostgreSQL is required for a new capture")?;
    store::postgres::PostgresDb::with_application_db(pg.clone(), async {}).await?;
    ensure!(
        source_identity(pg.pool()).await? == config.source_identity_sha256,
        "source database identity differs from immutable request"
    );
    let capture = capture(pg, config).await?;
    let mut bundle = capture
        .bundle(&format!("file://{}", root.display()))
        .await?;
    bind_sources(&mut bundle, config);
    seal(&bundle, &root).await
}

pub(super) async fn capture(pg: &store::postgres::PostgresDb, config: &Config) -> Result<Capture> {
    Ok(match &config.capture {
        CaptureScope::RegisteredNavigator => match &config.object_store {
            Some(store) => Capture(
                crate::postgres::extract_with_bounds(
                    pg.pool(),
                    crate::postgres::POSTGRES_TABLES,
                    &crate::postgres::NoPause,
                    &std::collections::HashMap::new(),
                    Some(&store.bounds()),
                )
                .await?,
                None,
            ),
            None => Capture::navigator(pg).await?,
        },
        CaptureScope::Paired {
            coverage,
            forgejo_identity_sha256,
        } => capture_pair(pg, config, coverage, forgejo_identity_sha256).await?,
    })
}

pub(super) fn bind_sources(bundle: &mut Bundle, config: &Config) {
    bundle.inventory.source_identity_sha256 = Some(config.source_identity_sha256.clone());
    bundle.inventory.forgejo_identity_sha256 = match &config.capture {
        CaptureScope::Paired {
            forgejo_identity_sha256,
            ..
        } => Some(forgejo_identity_sha256.clone()),
        CaptureScope::RegisteredNavigator => None,
    };
}

/// Reconstruct only the frozen Parquet from the sealed receipt, not from PostgreSQL.
async fn frozen(receipt: &LocalReceipt, root: &Path) -> Result<Capture> {
    let bundle = open_local(receipt, root).await?;
    frozen_bundle(bundle)
}

pub(super) fn frozen_bundle(bundle: Bundle) -> Result<Capture> {
    let mut tables = Vec::new();
    for (key, bytes, _) in &bundle.files {
        if !key.ends_with("/data/snapshot.parquet") {
            continue;
        }
        let table = key
            .strip_suffix("/data/snapshot.parquet")
            .and_then(|key| key.rsplit('/').next())
            .context("missing frozen table identity")?;
        ensure!(
            !table.is_empty()
                && table
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
            "invalid frozen table identity"
        );
        let parquet = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(
            bytes::Bytes::copy_from_slice(bytes),
        )?;
        tables.push((
            table.to_owned(),
            usize::try_from(parquet.metadata().file_metadata().num_rows())?,
            bytes.clone(),
        ));
    }
    let names: BTreeSet<_> = tables.iter().map(|(name, _, _)| name.as_str()).collect();
    let mut paired = bundle.inventory.paired;
    let expected = if let Some(manifest) = &mut paired {
        ensure!(
            !manifest.cross_database_atomic && manifest.databases.len() == 2,
            "invalid paired snapshot claim"
        );
        let mut expected = BTreeSet::new();
        for database in &mut manifest.databases {
            database.iceberg_metadata_keys.clear();
            for entry in &mut database.tables {
                let identity = entry
                    .key
                    .strip_suffix("/data/snapshot.parquet")
                    .and_then(|path| path.rsplit('/').next())
                    .context("invalid paired data identity")?;
                expected.insert(identity.to_owned());
                entry.key = identity.to_owned();
            }
        }
        expected
    } else {
        crate::postgres::POSTGRES_TABLES
            .iter()
            .map(|name| (*name).to_owned())
            .collect()
    };
    ensure!(
        names == expected.iter().map(String::as_str).collect(),
        "incomplete frozen export"
    );
    Ok(Capture(
        crate::postgres::Extracted {
            cutoff_at: bundle.inventory.cutoff_at,
            snapshot: bundle.inventory.snapshot,
            server_version: bundle.inventory.server_version,
            tables,
        },
        paired,
    ))
}

/// Prepare and checkpoint THIS destination closure before any cloud RPC.
pub(crate) async fn prepare_destination(
    local: &LocalReceipt,
    config: &Config,
    destination: usize,
) -> Result<LocalReceipt> {
    config.validate()?;
    let target = config
        .destinations
        .get(destination)
        .context("unknown backup destination")?;
    let original = open_local(local, &config.local_root).await?;
    validate_sources(&original.inventory, config)?;
    ensure!(
        original.inventory.source_identity_sha256.as_ref() == Some(&config.source_identity_sha256),
        "local source identity differs from request"
    );
    let capture = frozen(local, &config.local_root).await?;
    let mut bundle = capture.bundle(&format!("gs://{}", target.bucket)).await?;
    bundle.inventory.source_identity_sha256 = original.inventory.source_identity_sha256;
    bundle.inventory.forgejo_identity_sha256 = original.inventory.forgejo_identity_sha256;
    bundle.inventory.local_generation_sha256 = Some(local.inventory_sha256.clone());
    seal(&bundle, &local.root).await
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DestinationOutcome {
    Acknowledged {
        inventory_uri: String,
        inventory_key: String,
        generation: u64,
    },
    Unconfirmed {
        inventory_uri: String,
        inventory_key: String,
        acknowledged: usize,
    },
    Refused {
        inventory_uri: String,
    },
}

impl From<Acknowledged> for DestinationOutcome {
    fn from(value: Acknowledged) -> Self {
        Self::Acknowledged {
            inventory_uri: value.inventory_uri,
            inventory_key: value.inventory_key,
            generation: value.generation,
        }
    }
}
impl From<Unconfirmed> for DestinationOutcome {
    fn from(value: Unconfirmed) -> Self {
        Self::Unconfirmed {
            inventory_uri: value.inventory_uri,
            inventory_key: value.inventory_key,
            acknowledged: value.acknowledged,
        }
    }
}

/// Used only after the request binds config digest and prepared inventory SHA.
/// A started marker makes a crash/lost checkpoint explicitly unconfirmed. An
/// operator retry prepares a NEW destination attempt from the SAME local receipt.
pub(crate) async fn upload(
    prepared: &LocalReceipt,
    config: &Config,
    destination: usize,
    source_inventory_sha256: &str,
) -> Result<DestinationOutcome> {
    config.validate()?;
    let target = config
        .destinations
        .get(destination)
        .context("unknown backup destination")?;
    let bundle = open_local(prepared, &config.local_root).await?;
    validate_sources(&bundle.inventory, config)?;
    ensure!(
        bundle.inventory.local_generation_sha256.as_deref() == Some(source_inventory_sha256),
        "prepared attempt belongs to a different sealed local generation"
    );
    ensure!(
        bundle.root == format!("gs://{}", target.bucket),
        "prepared destination changed"
    );
    ensure!(
        bundle.inventory.source_identity_sha256.as_ref() == Some(&config.source_identity_sha256),
        "prepared source identity differs from request"
    );
    let started = prepared
        .root
        .join(format!("{}.started", prepared.inventory_key));
    match tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&started)
        .await
    {
        Ok(file) => {
            file.sync_all().await?;
            super::sync_parents(&prepared.root, &started).await?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Ok(bundle.unconfirmed(0).into());
        }
        Err(error) => return Err(error.into()),
    }
    let Ok(storage) = cloud::GcsStorage::archive_from_adc(target.bucket.clone()).await else {
        return Ok(bundle.unconfirmed(0).into());
    };
    Ok(match bundle.upload(&storage).await {
        Ok(ack) => ack.into(),
        Err(unknown) => unknown.into(),
    })
}

pub mod object_store;
pub mod verify;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Request {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_id: Option<String>,
    #[serde(default, skip_serializing_if = "delivery_is_zero")]
    pub delivery_attempt: u8,
    pub config: Config,
    pub config_sha256: String,
    /// Explicit recovery/retry refers to an already sealed generation, never recaptures.
    #[serde(default)]
    pub reuse_local: Option<LocalReceipt>,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // Serde skip predicate takes a borrowed field.
fn delivery_is_zero(value: &u8) -> bool {
    *value == 0
}

impl Request {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            self.delivery_attempt <= 3,
            "delivery attempt bound exceeded"
        );
        ensure!(
            self.config.digest()? == self.config_sha256,
            "archive request configuration changed"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DestinationReport {
    pub destination: usize,
    pub prepared: Option<LocalReceipt>,
    pub outcome: Option<DestinationOutcome>,
    pub preparation_failed: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Report {
    pub config_sha256: String,
    pub local: LocalReceipt,
    pub destinations: Vec<DestinationReport>,
    /// Transport state only; this is NEVER a Verified/catalog-promoted indicator.
    pub all_destinations_acknowledged: bool,
    pub catalog_promoted: bool,
}

/// Shared CLI operation. DBOS uses the same prepare/destination/upload functions
/// in separate durable steps and checkpoints their exact receipts between phases.
pub async fn execute(request: &Request) -> std::result::Result<Report, crate::write_only::Error> {
    let result: anyhow::Result<_> = async {
        request.validate()?;
        if request.config.object_store.is_some() {
            return object_store::execute(request).await;
        }
        let pg = if request.reuse_local.is_none() {
            Some(store::postgres::PostgresDb::application().await?)
        } else {
            None
        };
        let local = prepare(pg.as_ref(), &request.config, request.reuse_local.as_ref()).await?;
        let mut destinations = Vec::new();
        for index in 0..request.config.destinations.len() {
            let Ok(prepared) = prepare_destination(&local, &request.config, index).await else {
                destinations.push(DestinationReport {
                    destination: index,
                    prepared: None,
                    outcome: None,
                    preparation_failed: true,
                });
                continue;
            };
            let outcome = upload(&prepared, &request.config, index, &local.inventory_sha256)
                .await
                .unwrap_or_else(|_| DestinationOutcome::Refused {
                    inventory_uri: format!("{}/{}", prepared.root_uri, prepared.inventory_key),
                });
            destinations.push(DestinationReport {
                destination: index,
                prepared: Some(prepared),
                outcome: Some(outcome),
                preparation_failed: false,
            });
        }
        let acknowledged = destinations.iter().all(|report| {
            matches!(
                &report.outcome,
                Some(DestinationOutcome::Acknowledged { .. })
            )
        });
        Ok(Report {
            config_sha256: request.config_sha256.clone(),
            local,
            destinations,
            all_destinations_acknowledged: acknowledged,
            catalog_promoted: false,
        })
    }
    .await;
    result.map_err(|_| crate::write_only::Error::Generation)
}

/// Configuration rendering only: a separate deployment operator reviews/applies
/// these bucket/IAM/lock changes. Neither writer nor verifier applies this plan.
pub fn provisioning_plan(
    config: &Config,
    writer: &str,
    verifier: &str,
) -> std::result::Result<serde_json::Value, crate::write_only::Error> {
    config.validate()?;
    if !(writer != verifier
        && writer.starts_with("serviceAccount:")
        && verifier.starts_with("serviceAccount:"))
    {
        return Err(crate::write_only::Error::Configuration);
    }
    let destinations: Vec<_> = config.destinations.iter().map(|target| serde_json::json!({
        "project_number": target.project_number,
        "bucket": {
            "name": target.bucket, "storageClass": "STANDARD", "location":target.location,
            "iamConfiguration": {"uniformBucketLevelAccess": {"enabled":true}, "publicAccessPrevention":"enforced"},
            "retentionPolicy": {"retentionPeriod":target.minimum_retention_seconds.to_string()}
        },
        "writer_binding": {"role":"roles/storage.objectCreator", "member":writer},
        "verifier_custom_role_permissions": ["storage.buckets.get", "storage.buckets.getIamPolicy", "storage.objects.get", "storage.objects.list"],
        "verifier_member": verifier,
        "retention_lock": "separate irreversible control-plane action AFTER identity/policy verification",
        "effective_iam_acceptance_required": "include inherited/conditional bindings; writer must have no object get/list/delete/update permission",
        "automatic_apply": false
    })).collect();
    Ok(
        serde_json::json!({"format_version":1,"destinations":destinations,
        "warning":"proposal only; no roles/IAM/buckets/retention state changed; do not remove operational PITR"}),
    )
}

/// Bind the source to existing immutable authority, without DDL or SDK access.
pub async fn source_identity(
    pool: &sqlx::PgPool,
) -> std::result::Result<String, crate::write_only::Error> {
    let result: anyhow::Result<_> = async {
    let identity: serde_json::Value = sqlx::query_scalar(
        "SELECT jsonb_build_object('database',current_database(),'oid',(SELECT oid::bigint FROM pg_database WHERE datname=current_database()),'origin',(SELECT creation_operation FROM public.database_origin LIMIT 1),'captured',(SELECT jsonb_agg(jsonb_build_object('id',id,'scope',scope,'receipt',source_receipt) ORDER BY scope,id) FROM public.mirror_backfill_run WHERE scope<>'global' AND status='completed'))"
    ).fetch_one(pool).await?;
    Ok(crate::postgres::sha256_hex(&serde_json::to_vec(&identity)?))

        }.await;
    result.map_err(|_| crate::write_only::Error::Source)
}

async fn capture_pair(
    pg: &store::postgres::PostgresDb,
    config: &Config,
    coverage: &crate::coverage::ConfiguredCoverage,
    forgejo_identity_sha256: &str,
) -> Result<Capture> {
    let setting = |key: &str| {
        std::env::var(key)
            .ok()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("required archive reader setting is missing"))
    };
    let base = pg.pool().connect_options().as_ref().clone();
    let navigator = crate::postgres::reader_pool(
        base.clone()
            .username(&setting("NAVIGATOR_ARCHIVE_POSTGRES_USER")?)
            .password(&setting("NAVIGATOR_ARCHIVE_POSTGRES_PASSWORD")?),
    )
    .await?;
    let result = async {
        ensure!(
            source_identity(&navigator).await? == config.source_identity_sha256,
            "archive reader connected to a different Navigator source"
        );
        let forgejo = crate::postgres::reader_pool(
            base.database("forgejo")
                .username(&setting("NAVIGATOR_ARCHIVE_FORGEJO_USER")?)
                .password(&setting("NAVIGATOR_ARCHIVE_FORGEJO_PASSWORD")?),
        )
        .await?;
        let result = async {
            ensure!(
                database_identity(&forgejo).await? == forgejo_identity_sha256,
                "Forgejo reader connected to a different bound source"
            );
            let navigator_source = crate::coverage::DatabaseSource {
                name: "navigator",
                pool: &navigator,
                coverage: &coverage.navigator,
            };
            let forgejo_source = crate::coverage::DatabaseSource {
                name: "forgejo",
                pool: &forgejo,
                coverage: &coverage.forgejo,
            };
            match &config.object_store {
                Some(store) => {
                    Capture::paired_with_bounds(
                        navigator_source,
                        forgejo_source,
                        Some(&store.bounds()),
                    )
                    .await
                }
                None => Capture::paired(navigator_source, forgejo_source).await,
            }
        }
        .await;
        forgejo.close().await;
        result
    }
    .await;
    navigator.close().await;
    result
}

fn cardinality(files: &[(String, Vec<u8>, &'static str)]) -> Result<(usize, usize)> {
    let mut tables = 0;
    let mut rows = 0_usize;
    for (key, bytes, _) in files {
        if key.ends_with(".parquet") {
            let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(
                bytes::Bytes::copy_from_slice(bytes),
            )?;
            tables += 1;
            rows = rows
                .checked_add(usize::try_from(
                    reader.metadata().file_metadata().num_rows(),
                )?)
                .context("archive row count overflow")?;
        }
    }
    Ok((tables, rows))
}

async fn read_local_object(root: &Path, key: &str) -> Result<Vec<u8>> {
    ensure!(safe_key(key), "unsafe local archive key");
    let mut path = root.to_owned();
    let parts: Vec<_> = key.split('/').collect();
    for (index, part) in parts.iter().enumerate() {
        path.push(part);
        let metadata = tokio::fs::symlink_metadata(&path).await?;
        ensure!(
            if index + 1 == parts.len() {
                metadata.file_type().is_file()
            } else {
                metadata.file_type().is_dir()
            },
            "archive symlink/non-file is refused"
        );
    }
    Ok(tokio::fs::read(path).await?)
}

#[cfg(test)]
mod tests;

/// Companion database identity. Its TLS/server options are inherited from the
/// admitted Navigator server; the actual logical name/OID/version are request-bound.
async fn database_identity(pool: &sqlx::PgPool) -> Result<String> {
    let identity: serde_json::Value = sqlx::query_scalar(
        "SELECT jsonb_build_object('database',current_database(),'oid',(SELECT oid::bigint FROM pg_database WHERE datname=current_database()),'server_version',current_setting('server_version'))"
    ).fetch_one(pool).await?;
    Ok(crate::postgres::sha256_hex(&serde_json::to_vec(&identity)?))
}

pub async fn forgejo_source_identity(
    pg: &store::postgres::PostgresDb,
) -> std::result::Result<String, crate::write_only::Error> {
    let result: anyhow::Result<_> = async {
        let setting = |key: &str| {
            std::env::var(key)
                .ok()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!("required Forgejo archive reader setting is missing")
                })
        };
        let base = pg.pool().connect_options().as_ref().clone();
        let pool = crate::postgres::reader_pool(
            base.database("forgejo")
                .username(&setting("NAVIGATOR_ARCHIVE_FORGEJO_USER")?)
                .password(&setting("NAVIGATOR_ARCHIVE_FORGEJO_PASSWORD")?),
        )
        .await?;
        let result = database_identity(&pool).await;
        pool.close().await;
        result
    }
    .await;
    result.map_err(|_| crate::write_only::Error::Source)
}

fn validate_sources(inventory: &Inventory, config: &Config) -> Result<()> {
    ensure!(
        inventory.source_identity_sha256.as_ref() == Some(&config.source_identity_sha256),
        "sealed Navigator source identity differs from request"
    );
    match (&config.capture, &inventory.paired) {
        (CaptureScope::RegisteredNavigator, None) => {
            ensure!(
                inventory.forgejo_identity_sha256.is_none(),
                "unexpected companion source"
            );
        }
        (
            CaptureScope::Paired {
                coverage,
                forgejo_identity_sha256,
            },
            Some(paired),
        ) => {
            ensure!(
                !paired.cross_database_atomic
                    && paired.databases.len() == 2
                    && inventory.forgejo_identity_sha256.as_ref() == Some(forgejo_identity_sha256),
                "sealed paired source identities differ"
            );
            let navigator = paired
                .databases
                .iter()
                .find(|database| database.database == "navigator")
                .context("Navigator watermark missing")?;
            let forgejo = paired
                .databases
                .iter()
                .find(|database| database.database == "forgejo")
                .context("Forgejo watermark missing")?;
            ensure!(
                navigator.database_name != forgejo.database_name
                    && !navigator.snapshot.is_empty()
                    && !forgejo.snapshot.is_empty()
                    && navigator.coverage == coverage.navigator
                    && forgejo.coverage == coverage.forgejo,
                "paired inventory/watermark mismatch"
            );
        }
        _ => bail!("sealed export scope differs from request"),
    }
    Ok(())
}

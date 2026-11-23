//! Immutable source objects and durable attempt fences; offsite handles only create.
use super::{Bundle, Capture, DestinationOutcome, Inventory, LocalReceipt};
use anyhow::{ensure, Context, Result};
use cloud::{ImmutableObject, StorageService, VersionedObject};
use serde::{Deserialize, Serialize};
use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
};

const MAX_DESCRIPTOR: usize = 8 * 1024 * 1024;

/// Source identity selection is independent of offsite create-only handles.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Authentication {
    #[default]
    CredentialsFile,
    GkeMetadata,
}
impl Authentication {
    fn is_file(&self) -> bool {
        matches!(self, Self::CredentialsFile)
    }
}
fn empty_path(path: &Path) -> bool {
    path.as_os_str().is_empty()
}

/// Source identity mode and location are bound independently of offsite handles.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub bucket: String,
    pub prefix: String,
    #[serde(default, skip_serializing_if = "Authentication::is_file")]
    pub authentication: Authentication,
    #[serde(default, skip_serializing_if = "empty_path")]
    pub credentials_file: PathBuf,
    pub max_generation_bytes: usize,
    pub max_object_bytes: usize,
    pub max_objects: usize,
    pub max_rows_per_table: usize,
}
impl Config {
    pub(super) fn validate(&self) -> Result<()> {
        ensure!(
            super::super::safe_bucket(&self.bucket)
                && self.prefix.len() <= 128
                && super::safe_key(&self.prefix),
            "invalid sealed source location"
        );
        ensure!(
            match self.authentication {
                Authentication::CredentialsFile => self.credentials_file.is_absolute(),
                Authentication::GkeMetadata => self.credentials_file.as_os_str().is_empty(),
            },
            "sealed-source identity mode and credentials differ"
        );
        ensure!(
            (1..=128 * 1024 * 1024).contains(&self.max_generation_bytes),
            "invalid generation byte bound"
        );
        ensure!(
            (1..=32 * 1024 * 1024).contains(&self.max_object_bytes)
                && self.max_object_bytes <= self.max_generation_bytes,
            "invalid object byte bound"
        );
        ensure!(
            (1..=8192).contains(&self.max_objects)
                && (1..=1_000_000).contains(&self.max_rows_per_table),
            "invalid object/row bound"
        );
        Ok(())
    }
    pub(super) fn bounds(&self) -> crate::postgres::ExportBounds {
        crate::postgres::ExportBounds {
            generation_bytes: self.max_generation_bytes,
            object_bytes: self.max_object_bytes,
            rows_per_table: self.max_rows_per_table,
        }
    }
    fn key(&self, relative: &str) -> Result<String> {
        ensure!(
            super::safe_key(relative)
                && self
                    .prefix
                    .len()
                    .checked_add(relative.len())
                    .is_some_and(|size| size < 1024),
            "invalid sealed source key"
        );
        Ok(format!("{}/{}", self.prefix, relative))
    }
    fn root(&self) -> String {
        format!("gs://{}/{}", self.bucket, self.prefix)
    }
}

/// Versioned cloud backing; absent on supported version-one filesystem receipts.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub format_version: u8,
    pub bucket: String,
    pub prefix: String,
    pub inventory_generation: u64,
}

/// Only the source-bucket-bound client reads exact objects through this seam.
#[async_trait::async_trait]
pub trait Store: Send + Sync {
    async fn read(
        &self,
        key: &str,
        generation: Option<u64>,
        limit: usize,
    ) -> Result<Option<VersionedObject>>;
    async fn create(&self, key: &str, bytes: &[u8], content_type: &str) -> Result<ImmutableObject>;
}
struct Gcs(cloud::GcsStorage);
#[async_trait::async_trait]
impl Store for Gcs {
    async fn read(
        &self,
        key: &str,
        generation: Option<u64>,
        limit: usize,
    ) -> Result<Option<VersionedObject>> {
        match self.0.archive_read_bounded(key, generation, limit).await {
            Ok(value) => Ok(Some(value)),
            Err(cloud::StorageError::NotFound(_)) => Ok(None),
            Err(_) => anyhow::bail!("sealed source read unavailable"),
        }
    }
    async fn create(&self, key: &str, bytes: &[u8], content_type: &str) -> Result<ImmutableObject> {
        self.0
            .put_immutable(key, bytes, content_type)
            .await
            .map_err(|_| anyhow::anyhow!("sealed source create unconfirmed"))
    }
}
pub async fn connect(config: &Config) -> Result<Arc<dyn Store>> {
    config.validate()?;
    let storage = match config.authentication {
        Authentication::CredentialsFile => {
            cloud::GcsStorage::archive_from_credentials(
                config.bucket.clone(),
                &config.credentials_file,
            )
            .await?
        }
        Authentication::GkeMetadata => {
            cloud::GcsStorage::archive_from_gke_metadata(config.bucket.clone()).await?
        }
    };
    Ok(Arc::new(Gcs(storage)))
}
fn settings(config: &super::Config) -> Result<&Config> {
    config.validate()?;
    config
        .object_store
        .as_ref()
        .context("object storage configuration required")
}
fn identity(config: &super::Config, id: &str) -> Result<String> {
    ensure!(
        !id.is_empty() && id.len() <= 512,
        "bounded explicit capture identity required"
    );
    Ok(crate::postgres::sha256_hex(&serde_json::to_vec(&(
        config.digest()?,
        id,
    ))?))
}
fn receipt(config: &Config, bundle: &Bundle, generation: u64) -> Result<LocalReceipt> {
    let (tables, rows) = super::cardinality(&bundle.files)?;
    Ok(LocalReceipt {
        object_store: Some(Receipt {
            format_version: 2,
            bucket: config.bucket.clone(),
            prefix: config.prefix.clone(),
            inventory_generation: generation,
        }),
        root: PathBuf::new(),
        root_uri: bundle.root.clone(),
        inventory_key: bundle.descriptor.clone(),
        inventory_sha256: crate::postgres::sha256_hex(&serde_json::to_vec(&bundle.inventory)?),
        cutoff_at: bundle.inventory.cutoff_at,
        tables,
        rows,
    })
}
fn bounds(config: &Config, inventory: &Inventory) -> Result<()> {
    ensure!(
        inventory.objects.len() <= config.max_objects
            && inventory.metadata.len() <= config.max_objects,
        "sealed generation object bound exceeded"
    );
    let mut total = 0usize;
    for object in &inventory.objects {
        ensure!(
            object.bytes <= config.max_object_bytes,
            "sealed object byte bound exceeded"
        );
        total = total
            .checked_add(object.bytes)
            .context("sealed byte count overflow")?;
        ensure!(
            total <= config.max_generation_bytes,
            "sealed generation byte bound exceeded"
        );
    }
    Ok(())
}
async fn exact(
    store: &dyn Store,
    key: &str,
    bytes: &[u8],
    content_type: &str,
) -> Result<ImmutableObject> {
    if let Ok(created) = store.create(key, bytes, content_type).await {
        ensure!(
            created.generation > 0 && created.size_bytes == bytes.len() as u64,
            "invalid sealed source acknowledgement"
        );
        Ok(created)
    } else {
        // Only the source-store reader can resolve its own lost create ACK.
        let existing = store
            .read(key, None, bytes.len())
            .await?
            .context("sealed source create remains unconfirmed")?;
        ensure!(
            existing.object.bytes == bytes && existing.generation > 0,
            "sealed source collision"
        );
        Ok(ImmutableObject {
            generation: existing.generation,
            size_bytes: bytes.len() as u64,
        })
    }
}
async fn claim(store: &dyn Store, key: &str) -> Result<bool> {
    let nonce = uuid::Uuid::new_v4().to_string();
    if let Ok(created) = store.create(key, nonce.as_bytes(), "text/plain").await {
        ensure!(
            created.generation > 0 && created.size_bytes == nonce.len() as u64,
            "invalid attempt acknowledgement"
        );
        Ok(true)
    } else {
        let previous = store
            .read(key, None, 128)
            .await?
            .context("source attempt fence unconfirmed")?;
        Ok(previous.object.bytes == nonce.as_bytes())
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Reference {
    #[serde(rename = "inventory_key")]
    key: String,
    #[serde(rename = "inventory_generation")]
    generation: u64,
    #[serde(rename = "inventory_sha256")]
    sha256: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Sealed {
    format_version: u8,
    root_uri: String,
    inventory: Inventory,
    object_generations: std::collections::BTreeMap<String, u64>,
    parquet_source: Option<Reference>,
}
fn is_data(key: &str) -> bool {
    key.ends_with("/data/snapshot.parquet")
}
fn relative_table(key: &str) -> Result<&str> {
    key.splitn(3, '/')
        .nth(2)
        .context("invalid independent object identity")
}
async fn read_seal(
    store: &dyn Store,
    config: &super::Config,
    key: &str,
    generation: Option<u64>,
) -> Result<Option<(Sealed, u64)>> {
    let settings = settings(config)?;
    let Some(descriptor) = store
        .read(&settings.key(key)?, generation, MAX_DESCRIPTOR)
        .await?
    else {
        return Ok(None);
    };
    ensure!(
        descriptor.object.bytes.len() <= MAX_DESCRIPTOR && descriptor.generation > 0,
        "invalid sealed inventory"
    );
    let sealed: Sealed = serde_json::from_slice(&descriptor.object.bytes)?;
    ensure!(
        sealed.format_version == 2 && sealed.inventory.format_version == 1,
        "unsupported sealed inventory"
    );
    super::validate_sources(&sealed.inventory, config)?;
    ensure!(
        sealed.inventory.source_identity_sha256.as_ref() == Some(&config.source_identity_sha256),
        "sealed source identity differs"
    );
    bounds(settings, &sealed.inventory)?;
    ensure!(
        sealed.object_generations.len() <= settings.max_objects
            && sealed
                .object_generations
                .values()
                .all(|generation| *generation > 0),
        "invalid source object versions"
    );
    Ok(Some((sealed, descriptor.generation)))
}
async fn seal(
    store: &dyn Store,
    config: &super::Config,
    mut bundle: Bundle,
    origin: Option<&LocalReceipt>,
) -> Result<LocalReceipt> {
    let settings = settings(config)?;
    super::validate_sources(&bundle.inventory, config)?;
    bounds(settings, &bundle.inventory)?;
    super::super::closure::validate(
        &bundle.inventory,
        &bundle.root,
        &bundle.descriptor,
        &bundle.files,
    )?;
    let mut versions = std::collections::BTreeMap::new();
    for (key, bytes, content_type) in &bundle.files {
        if origin.is_some() && is_data(key) {
            continue;
        }
        let created = exact(store, &settings.key(key)?, bytes, content_type).await?;
        versions.insert(key.clone(), created.generation);
    }
    let parquet_source = origin
        .map(|saved| -> Result<Reference> {
            Ok(Reference {
                key: saved.inventory_key.clone(),
                generation: saved
                    .object_store
                    .as_ref()
                    .context("object source receipt required")?
                    .inventory_generation,
                sha256: saved.inventory_sha256.clone(),
            })
        })
        .transpose()?;
    let sealed = Sealed {
        format_version: 2,
        root_uri: bundle.root.clone(),
        inventory: bundle.inventory,
        object_generations: versions,
        parquet_source,
    };
    let descriptor = serde_json::to_vec(&sealed)?;
    ensure!(
        descriptor.len() <= MAX_DESCRIPTOR,
        "sealed inventory too large"
    );
    let created = exact(
        store,
        &settings.key(&bundle.descriptor)?,
        &descriptor,
        "application/json",
    )
    .await?;
    bundle.inventory = sealed.inventory;
    receipt(settings, &bundle, created.generation)
}
async fn read_origin(
    store: &dyn Store,
    config: &super::Config,
    root: &str,
    sealed: &Sealed,
) -> Result<Option<Sealed>> {
    let settings = settings(config)?;
    let origin = match &sealed.parquet_source {
        Some(reference) => {
            ensure!(
                root != settings.root(),
                "a source generation cannot reference another generation"
            );
            let (origin, _) = read_seal(store, config, &reference.key, Some(reference.generation))
                .await?
                .context("frozen source generation missing")?;
            ensure!(
                origin.root_uri == settings.root()
                    && origin.parquet_source.is_none()
                    && crate::postgres::sha256_hex(&serde_json::to_vec(&origin.inventory)?)
                        == reference.sha256,
                "foreign frozen source reference"
            );
            Some(origin)
        }
        None => None,
    };
    Ok(origin)
}
async fn read_bundle(
    store: &dyn Store,
    config: &super::Config,
    root: &str,
    key: &str,
    generation: Option<u64>,
) -> Result<Option<(Bundle, u64)>> {
    let settings = settings(config)?;
    let Some((sealed, generation)) = read_seal(store, config, key, generation).await? else {
        return Ok(None);
    };
    ensure!(
        sealed.root_uri == root,
        "sealed URI differs from bound receipt"
    );
    let origin = read_origin(store, config, root, &sealed).await?;
    let inventory = sealed.inventory;
    let attempt = key
        .strip_suffix("/inventory.json")
        .context("invalid inventory key")?;
    let mut seen = std::collections::BTreeSet::new();
    let mut used_versions = std::collections::BTreeSet::new();
    let mut files = Vec::with_capacity(inventory.objects.len());
    for entry in &inventory.objects {
        ensure!(
            entry.key.starts_with(&format!("{attempt}/"))
                && super::safe_key(&entry.key)
                && seen.insert(&entry.key),
            "foreign or duplicate source object"
        );
        let (source_key, version) =
            if let Some(origin) = origin.as_ref().filter(|_| is_data(&entry.key)) {
                let suffix = relative_table(&entry.key)?;
                let original = origin
                    .inventory
                    .objects
                    .iter()
                    .find(|candidate| relative_table(&candidate.key).ok() == Some(suffix))
                    .context("frozen source data absent")?;
                ensure!(
                    original.sha256 == entry.sha256 && original.bytes == entry.bytes,
                    "frozen source data differs"
                );
                (
                    &original.key,
                    *origin
                        .object_generations
                        .get(&original.key)
                        .context("source data version missing")?,
                )
            } else {
                used_versions.insert(entry.key.clone());
                (
                    &entry.key,
                    *sealed
                        .object_generations
                        .get(&entry.key)
                        .context("source object version missing")?,
                )
            };
        let object = store
            .read(&settings.key(source_key)?, Some(version), entry.bytes)
            .await?
            .context("sealed source object missing")?;
        ensure!(
            object.object.bytes.len() == entry.bytes
                && crate::postgres::sha256_hex(&object.object.bytes) == entry.sha256,
            "sealed source object changed"
        );
        files.push((
            entry.key.clone(),
            object.object.bytes,
            super::super::object_content_type(&entry.key)?,
        ));
    }
    ensure!(
        used_versions.len() == sealed.object_generations.len(),
        "unreferenced source object versions"
    );
    super::super::closure::validate(&inventory, root, key, &files)?;
    Ok(Some((
        Bundle {
            root: root.to_owned(),
            inventory,
            files,
            descriptor: key.to_owned(),
        },
        generation,
    )))
}

pub(crate) async fn open(
    store: &dyn Store,
    config: &super::Config,
    saved: &LocalReceipt,
) -> Result<Bundle> {
    let settings = settings(config)?;
    let backing = saved
        .object_store
        .as_ref()
        .context("filesystem receipt cannot be opened as object storage")?;
    ensure!(
        backing.format_version == 2
            && backing.bucket == settings.bucket
            && backing.prefix == settings.prefix
            && backing.inventory_generation > 0
            && saved.root.as_os_str().is_empty(),
        "foreign object-backed receipt"
    );
    ensure!(
        saved.root_uri == settings.root()
            || config
                .destinations
                .iter()
                .any(|d| saved.root_uri == format!("gs://{}", d.bucket)),
        "foreign sealed root"
    );
    let (bundle, generation) = read_bundle(
        store,
        config,
        &saved.root_uri,
        &saved.inventory_key,
        Some(backing.inventory_generation),
    )
    .await?
    .context("sealed generation unavailable")?;
    let observed = receipt(settings, &bundle, generation)?;
    ensure!(
        observed.inventory_sha256 == saved.inventory_sha256
            && observed.cutoff_at == saved.cutoff_at
            && observed.tables == saved.tables
            && observed.rows == saved.rows,
        "sealed receipt differs from bound generation"
    );
    Ok(bundle)
}

/// The stable request identity locates completed capture after process/checkpoint loss.
/// A fence without a sealed inventory refuses extraction; a new cutoff needs a new identity.
pub(crate) async fn prepare<F, Fut>(
    store: &dyn Store,
    config: &super::Config,
    id: &str,
    existing: Option<&LocalReceipt>,
    capture: F,
) -> Result<LocalReceipt>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Capture>>,
{
    if let Some(saved) = existing {
        open(store, config, saved).await?;
        return Ok(saved.clone());
    }
    let settings = settings(config)?;
    let attempt = format!("attempts/{}", identity(config, id)?);
    let key = format!("{attempt}/inventory.json");
    if let Some((bundle, generation)) =
        read_bundle(store, config, &settings.root(), &key, None).await?
    {
        return receipt(settings, &bundle, generation);
    }
    ensure!(
        claim(
            store,
            &settings.key(&format!("fences/{}.capture", identity(config, id)?))?
        )
        .await?,
        "capture interrupted or already owned; no re-extraction permitted"
    );
    let capture = capture().await?;
    let mut bundle = capture.bundle_at(&settings.root(), &attempt).await?;
    super::bind_sources(&mut bundle, config);
    seal(store, config, bundle, None).await
}
pub(crate) async fn prepare_destination(
    store: &dyn Store,
    config: &super::Config,
    source: &LocalReceipt,
    destination: usize,
    delivery_attempt: u8,
) -> Result<LocalReceipt> {
    let source_bundle = open(store, config, source).await?;
    let target = config
        .destinations
        .get(destination)
        .context("unknown destination")?;
    ensure!(delivery_attempt <= 3, "delivery attempt bound exceeded");
    let id = format!(
        "{}:{destination}:{delivery_attempt}",
        source.inventory_sha256
    );
    let attempt = format!("attempts/{}", identity(config, &id)?);
    let key = format!("{attempt}/inventory.json");
    let root = format!("gs://{}", target.bucket);
    if let Some((bundle, generation)) = read_bundle(store, config, &root, &key, None).await? {
        ensure!(
            bundle.inventory.local_generation_sha256.as_ref() == Some(&source.inventory_sha256),
            "prepared source differs"
        );
        return receipt(settings(config)?, &bundle, generation);
    }
    let mut bundle = super::frozen_bundle(source_bundle)?
        .bundle_at(&root, &attempt)
        .await?;
    super::bind_sources(&mut bundle, config);
    bundle.inventory.local_generation_sha256 = Some(source.inventory_sha256.clone());
    seal(store, config, bundle, Some(source)).await
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TransportReceipt {
    config_sha256: String,
    inventory_sha256: String,
    source_sha256: String,
    outcome: DestinationOutcome,
}
pub(crate) async fn upload(
    store: &dyn Store,
    config: &super::Config,
    prepared: &LocalReceipt,
    destination: usize,
    source_sha256: &str,
    target: &dyn StorageService,
) -> Result<DestinationOutcome> {
    let settings = settings(config)?;
    let bundle = open(store, config, prepared).await?;
    let destination = config
        .destinations
        .get(destination)
        .context("unknown destination")?;
    ensure!(
        bundle.root == format!("gs://{}", destination.bucket)
            && bundle.inventory.local_generation_sha256.as_deref() == Some(source_sha256),
        "prepared destination/source differs"
    );
    let id = crate::postgres::sha256_hex(prepared.inventory_key.as_bytes());
    let outcome_key = settings.key(&format!("outcomes/{id}.json"))?;
    if let Some(saved) = store.read(&outcome_key, None, MAX_DESCRIPTOR).await? {
        let saved: TransportReceipt = serde_json::from_slice(&saved.object.bytes)?;
        ensure!(
            saved.config_sha256 == config.digest()?
                && saved.inventory_sha256 == prepared.inventory_sha256
                && saved.source_sha256 == source_sha256,
            "foreign transport receipt"
        );
        return Ok(saved.outcome);
    }
    let outcome: DestinationOutcome =
        if claim(store, &settings.key(&format!("fences/{id}.upload"))?).await? {
            match bundle.upload(target).await {
                Ok(value) => value.into(),
                Err(value) => value.into(),
            }
        } else {
            bundle.unconfirmed(0).into()
        };
    let saved = TransportReceipt {
        config_sha256: config.digest()?,
        inventory_sha256: prepared.inventory_sha256.clone(),
        source_sha256: source_sha256.to_owned(),
        outcome: outcome.clone(),
    };
    exact(
        store,
        &outcome_key,
        &serde_json::to_vec(&saved)?,
        "application/json",
    )
    .await?;
    Ok(outcome)
}

/// Source admission and one extraction at a time across replicas, independent of transport retries.
pub(crate) async fn capture_native(
    pg: &store::postgres::PostgresDb,
    config: &super::Config,
) -> Result<Capture> {
    store::postgres::PostgresDb::with_application_db(pg.clone(), async {}).await?;
    ensure!(
        super::source_identity(pg.pool()).await? == config.source_identity_sha256,
        "source identity differs from immutable request"
    );
    let mut gate = pg.pool().begin().await?;
    sqlx::query("SET LOCAL statement_timeout='30s'")
        .execute(&mut *gate)
        .await?;
    let locked: bool =
        sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1,0))")
            .bind(&config.source_identity_sha256)
            .fetch_one(&mut *gate)
            .await?;
    ensure!(locked, "another source capture owns the extraction gate");
    let captured = super::capture(pg, config).await;
    gate.rollback().await?;
    captured
}

pub(super) async fn execute(request: &super::Request) -> Result<super::Report> {
    request.validate()?;
    let config = &request.config;
    let store = connect(settings(config)?).await?;
    let id = request
        .capture_id
        .as_deref()
        .context("object capture requires an explicit capture identity")?;
    let local = prepare(
        store.as_ref(),
        config,
        id,
        request.reuse_local.as_ref(),
        || async {
            let pg = store::postgres::PostgresDb::application().await?;
            capture_native(&pg, config).await
        },
    )
    .await?;
    let mut destinations = Vec::new();
    for destination in 0..config.destinations.len() {
        let prepared = prepare_destination(
            store.as_ref(),
            config,
            &local,
            destination,
            request.delivery_attempt,
        )
        .await?;
        let target =
            cloud::GcsStorage::archive_from_adc(config.destinations[destination].bucket.clone())
                .await?;
        let outcome = upload(
            store.as_ref(),
            config,
            &prepared,
            destination,
            &local.inventory_sha256,
            &target,
        )
        .await?;
        destinations.push(super::DestinationReport {
            destination,
            prepared: Some(prepared),
            outcome: Some(outcome),
            preparation_failed: false,
        });
    }
    let acknowledged = destinations
        .iter()
        .all(|entry| matches!(entry.outcome, Some(DestinationOutcome::Acknowledged { .. })));
    Ok(super::Report {
        config_sha256: request.config_sha256.clone(),
        local,
        destinations,
        all_destinations_acknowledged: acknowledged,
        catalog_promoted: false,
    })
}

#[cfg(test)]
pub(crate) mod tests;

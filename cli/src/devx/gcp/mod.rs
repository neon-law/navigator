//! `devx gcp setup --project-id <PROJECT_ID>` — provision the GCP
//! resources `web` depends on (VPC, GCS buckets, runtime Workload
//! Identity, and a GKE Autopilot cluster) from a single Rust binary. Config
//! Sync is an explicit optional seam.
//!
//! Six buckets are unconditional — assets, documents, exports, logs, the
//! PostgreSQL backups bucket, and the private applications bucket that holds
//! each Project's published Portal bundle. Two more are created only when the
//! deployment names them: the long-term Iceberg archive of its Surreal store,
//! and the 90-day telemetry landing zone the nightly lane promotes into that
//! archive. A deployment naming neither keeps exactly the six-bucket shape.
//!
//! Every step is **idempotent**: each `ensure_*` function issues a
//! create call and treats HTTP 409 Conflict (REST steps) or
//! `"already exists"` stderr (gcloud shell-out steps) as success. The
//! same `setup` invocation can therefore be re-run after a partial
//! failure without producing duplicates.
//!
//! ## Pipeline order
//!
//! 1. [`services::enable_services`] — `serviceusage.batchEnable`.
//!    Must run first; nothing else works without the APIs enabled.
//! 2. [`network::ensure_network`] — custom-mode VPC.
//! 3. [`network::ensure_router_and_nat`] — regional Cloud Router and NAT for
//!    private-node outbound connectivity.
//! 4. [`buckets::ensure_bucket`] for assets, documents, exports, logs, the
//!    PostgreSQL backups bucket (with its backup Kubernetes account's grant),
//!    and the applications bucket, then — when named — the archive and
//!    telemetry buckets, each followed by [`buckets::ensure_lifecycle`] so the
//!    telemetry expiry is in place before the first object lands.
//! 5. [`workload_identity::ensure_runtime_identity`] — per-deployment GSA
//!    and direct GCP access.
//! 6. Registry access, then
//!    [`gke::ensure_autopilot_cluster_foundation`] — GKE Autopilot cluster
//!    and Gateway static IP.
//! 7. [`workload_identity::bind_kubernetes_accounts`] — workload identity
//!    bindings after the cluster has created the project pool.
//! 8. [`gke::ensure_cluster_integrations`] — Fleet membership and optional
//!    Config Sync `RootSync`.
//! 9. [`kms::ensure`] — the Cloud KMS key this deployment's
//!    `secrets.enc.yaml` is encrypted against, in this deployment's own
//!    project. Last because it is cheap and depends on nothing above it
//!    except step 1; it is not a prerequisite of any earlier stage.
//! 10. Log retention — the logs bucket's lifecycle and Cloud Logging's
//!     `_Default` bucket, both at [`logging::LOG_RETENTION_DAYS`].
//! 11. [`release_ship::ensure`] — the release-ship identity's roles, its
//!     configuration-secret grant, and its GitHub Workload Identity provider,
//!     when `--ship-deployment`, `--ship-repository`, and `--ship-environment`
//!     name them. Skipped, with a stage line, otherwise.
//! 12. [`release_ship::ensure_postgres_backup_target`] — the
//!     `navigator-postgres-backup-target` `ConfigMap` in the row namespace.
//!
//! Steps 1–3 talk to GCP REST APIs via [`client::GcpClient`]; steps 4–5
//! shell out to `gcloud` and `kubectl` (the Container API alone is
//! ~200 lines of cluster JSON). Tests stand up wiremock and override
//! base URLs per service for the REST steps, and use the dry-run
//! recorder for the shell-out step — no traffic ever leaves the host,
//! no GCP credentials needed. See `docs/cloud-operations.md` for the
//! layered CI strategy.

pub mod artifact_registry;
pub mod auth;
pub mod buckets;
pub mod client;
pub mod error;
pub mod gke;
pub mod hub;
pub mod iap;
pub mod kms;
pub mod logging;
pub mod lro;
pub mod network;
pub mod release_ship;
pub mod secret_manager;
pub mod services;
pub mod tenants;
pub mod workload_identity;

pub use error::{SetupError, SetupResult};

use self::client::GcpClient;

const SETUP_STAGE_COUNT: usize = 19;

fn progress_line(project_id: &str, stage: usize, stage_count: usize, detail: &str) -> String {
    format!("gcp setup [{project_id}] {stage:02}/{stage_count:02} {detail}")
}

fn progress(project_id: &str, stage: usize, detail: &str) {
    eprintln!(
        "{}",
        progress_line(project_id, stage, SETUP_STAGE_COUNT, detail)
    );
}

/// Default region. Overridable via `NAVIGATOR_GCP_LOCATION`.
pub const DEFAULT_REGION: &str = "example-region";

/// Bucket name suffixes appended to the project ID.
pub const ASSETS_BUCKET_SUFFIX: &str = "-assets";
pub const DOCUMENTS_BUCKET_SUFFIX: &str = "-documents";
pub const EXPORTS_BUCKET_SUFFIX: &str = "-exports";
pub const LOGS_BUCKET_SUFFIX: &str = "-logs";
/// The private bucket holding this deployment's PostgreSQL WAL archive and
/// base backups. Only the `navigator-postgres-backup` Kubernetes service
/// account reads or writes it; the web runtime identity has no access.
pub const POSTGRES_BACKUPS_BUCKET_SUFFIX: &str = "-postgres-backups";
/// The private, per-deployment applications bucket. It held each Project's
/// published Portal bundle at `<project_code>/portal/` until Portal serving
/// was retired; nothing reads it now. Mandatory like the four above, and never
/// public.
pub const APPLICATIONS_BUCKET_SUFFIX: &str = "-applications";
pub const TELEMETRY_BUCKET_SUFFIX: &str = "-telemetry";
/// The private Project Recovery Archive bucket, `<deployment>-archived-projects`.
pub const ARCHIVED_PROJECTS_BUCKET_SUFFIX: &str = "-archived-projects";
/// Archive buckets are `neon-law-archives-<deployment>`, a PREFIX rather than the
/// suffix every other lane uses. The archive of a project is named for the
/// archive family first and the deployment second, matching the existing
/// `neon-law-archives-neon-law-420305`.
pub const ARCHIVES_BUCKET_PREFIX: &str = "neon-law-archives-";

/// Default Artifact Registry repository name that holds every image.
pub const DEFAULT_ARTIFACT_REGISTRY_REPO: &str = "navigator";
/// Default `owner/repo` the Workload Identity provider trusts.
pub const DEFAULT_GITHUB_REPO: &str = "neon-law/navigator";
/// Default account id of the CI service account that pushes images.
pub const DEFAULT_CI_PUSHER_ACCOUNT_ID: &str = "navigator-ci-pusher";

/// Per-deployment overrides for `devx gcp setup`. Every field defaults
/// to the workspace's preferred name; OSS forks change them via CLI
/// flags or env vars without forking the code. The struct is built by
/// `devx/src/main.rs::gcp_setup` from clap-parsed `--region`,
/// `--cluster-name`, etc. flags that fall back to env vars.
#[derive(Clone)]
pub struct SetupConfig {
    /// Explicit IAM member granted SOPS encrypt/decrypt on the deployment key.
    /// `None` leaves operator IAM unchanged; no identity is inferred.
    pub kms_operator: Option<String>,
    /// GCP region used for GKE Autopilot and bucket location.
    /// Default: `example-region`.
    pub region: String,
    /// GKE Autopilot cluster name. Default: `navigator-prod`.
    pub cluster_name: String,
    /// VPC network name. Default: `navigator-vpc`.
    pub vpc_name: String,
    /// Regional subnetwork used by the GKE cluster. Default:
    /// `navigator-subnet`.
    pub subnetwork_name: String,
    /// Reserved global static-IP name attached to the Gateway.
    /// Default: `navigator-gateway-ip`.
    pub gateway_ip_name: String,
    /// HTTPS URL of the GitHub (or other Git host) repo that `Config
    /// Sync` should reconcile from. `None` skips the `RootSync` step —
    /// the right default for OSS forks that don't run `GitOps` yet.
    pub config_sync_repo: Option<String>,
    /// Path inside `config_sync_repo` that the `RootSync` watches.
    /// Default: `examples/deploy/k8s/gke` to match the parameterized
    /// overlay shipped with the workspace.
    pub config_sync_dir: String,
    /// Canonical HTTPS origin allowed to fetch public bucket fonts. Supplied
    /// by the `NAV_BASE_URL` deployment setting.
    pub public_base_url: Option<String>,
    /// Artifact Registry repository name that holds every container
    /// image. Default: `navigator`.
    pub artifact_registry_repo: String,
    /// `owner/repo` slug the CI pusher service account accepts from GitHub
    /// Actions. The existing provider condition is a coarse owner gate; the
    /// impersonation grant stays pinned to this repository.
    /// Default: `neon-law/navigator`.
    pub github_repo: String,
    /// Account id (local part of the SA email) of the CI pusher service
    /// account. Default: `navigator-ci-pusher`.
    pub ci_pusher_account_id: String,
    /// Hub project holding the shared Artifact Registry. When set, this
    /// environment creates no registry, CI identity, or WIF pool of its own —
    /// it only takes `roles/artifactregistry.reader` on the hub repository.
    /// `None` keeps the single-project shape, where the registry lives
    /// alongside the workloads that pull from it.
    pub images_project_id: Option<String>,
    /// Per-deployment bucket names. When absent, the single-project OSS
    /// convention (`<project>-{assets,documents,exports,logs}`) is retained. The
    /// staging project sets these explicitly so its three deployments never
    /// share an object-storage lane.
    pub assets_bucket: Option<String>,
    pub documents_bucket: Option<String>,
    pub exports_bucket: Option<String>,
    pub logs_bucket: Option<String>,
    /// Private applications bucket holding each Project's published
    /// Portal bundle. Mandatory like the four above; when absent it
    /// derives `<project>-applications`.
    pub applications_bucket: Option<String>,
    /// Long-term Iceberg archive for this deployment's Surreal store, and the
    /// 90-day telemetry landing zone the nightly lane promotes into it.
    ///
    /// Neither derives from `project_id`. `neon-law-archives-<deployment>` is
    /// prefix-shaped, and `<deployment>-telemetry` names the deployment while
    /// the project may be named for the entity — `example-project` lives in
    /// `neon-law`, so a derived `neon-law-telemetry` would be a bucket nobody
    /// configured. Absent means the deployment declines that lane.
    pub archives_bucket: Option<String>,
    pub telemetry_bucket: Option<String>,
    /// Private Project Recovery Archive bucket. Absent means the deployment
    /// declines the lane; archive commands then fail closed.
    pub archived_projects_bucket: Option<String>,
    /// Google service-account id used by the web and workflows Kubernetes
    /// service accounts through GKE Workload Identity. Deployments sharing a
    /// GCP project set this explicitly so their principals stay isolated.
    pub google_service_account_id: String,
    /// Dedicated service-account id whose JSON credential is granted Google
    /// Workspace domain-wide delegation. It deliberately receives no runtime
    /// GCP roles; a leaked Drive key must not also unlock SQL, secrets, or GCS.
    pub drive_service_account_id: String,
    /// Kubernetes namespace containing the workload service accounts.
    pub kubernetes_namespace: String,
    /// The organization that owns this deployment's Project repositories —
    /// the one `repository_owner` the deployment's Workload Identity provider
    /// admits. One provider's `attributeCondition` names exactly one owner, so
    /// this is singular.
    pub applications_publisher_org: Option<String>,
    /// The row, repository, and GitHub Environment the release-ship identity
    /// serves. `None` skips that stage; nothing infers these.
    pub release_ship: Option<release_ship::ReleaseShipConfig>,
}

impl Default for SetupConfig {
    fn default() -> Self {
        Self {
            kms_operator: None,
            region: DEFAULT_REGION.to_string(),
            cluster_name: gke::DEFAULT_CLUSTER_NAME.to_string(),
            vpc_name: network::DEFAULT_NETWORK_NAME.to_string(),
            subnetwork_name: network::DEFAULT_SUBNETWORK_NAME.to_string(),
            gateway_ip_name: gke::DEFAULT_GATEWAY_IP_NAME.to_string(),
            config_sync_repo: None,
            config_sync_dir: "examples/deploy/k8s/gke".to_string(),
            public_base_url: None,
            artifact_registry_repo: DEFAULT_ARTIFACT_REGISTRY_REPO.to_string(),
            github_repo: DEFAULT_GITHUB_REPO.to_string(),
            ci_pusher_account_id: DEFAULT_CI_PUSHER_ACCOUNT_ID.to_string(),
            images_project_id: None,
            assets_bucket: None,
            documents_bucket: None,
            exports_bucket: None,
            logs_bucket: None,
            applications_bucket: None,
            archives_bucket: None,
            telemetry_bucket: None,
            archived_projects_bucket: None,
            google_service_account_id: "navigator-web".to_string(),
            drive_service_account_id: "navigator-drive".to_string(),
            kubernetes_namespace: "navigator".to_string(),
            applications_publisher_org: None,
            release_ship: None,
        }
    }
}

/// Every bucket name this pipeline provisions, resolved once.
///
/// The first five fall back to the single-project OSS convention
/// `<project>-<lane>`, and the PostgreSQL backups bucket is always
/// `<project>-postgres-backups`. The last two never do — see
/// [`SetupConfig::archives_bucket`] — so `None` there means the deployment
/// declined that lane rather than that a default applies.
struct BucketNames {
    assets: String,
    documents: String,
    exports: String,
    logs: String,
    postgres_backups: String,
    applications: String,
    archives: Option<String>,
    telemetry: Option<String>,
    archived_projects: Option<String>,
}

impl BucketNames {
    fn resolve(project_id: &str, config: &SetupConfig) -> Self {
        let derived = |configured: &Option<String>, suffix: &str| {
            configured
                .clone()
                .unwrap_or_else(|| format!("{project_id}{suffix}"))
        };
        Self {
            assets: derived(&config.assets_bucket, ASSETS_BUCKET_SUFFIX),
            documents: derived(&config.documents_bucket, DOCUMENTS_BUCKET_SUFFIX),
            exports: derived(&config.exports_bucket, EXPORTS_BUCKET_SUFFIX),
            logs: derived(&config.logs_bucket, LOGS_BUCKET_SUFFIX),
            postgres_backups: format!("{project_id}{POSTGRES_BACKUPS_BUCKET_SUFFIX}"),
            applications: derived(&config.applications_bucket, APPLICATIONS_BUCKET_SUFFIX),
            archives: config.archives_bucket.clone(),
            telemetry: config.telemetry_bucket.clone(),
            archived_projects: config.archived_projects_bucket.clone(),
        }
    }

    /// Every bucket this deployment's runtime identity is granted `objectUser`
    /// on. A lane the deployment declined contributes no binding, so the grant
    /// set stays exactly as narrow as the bucket set. The PostgreSQL backups
    /// bucket is deliberately absent: the web runtime gets no access to
    /// database backups.
    fn granted(&self) -> Vec<&str> {
        let mut granted: Vec<&str> = vec![
            &self.assets,
            &self.documents,
            &self.exports,
            &self.logs,
            &self.applications,
        ];
        granted.extend(self.archives.as_deref());
        granted.extend(self.telemetry.as_deref());
        granted
    }
}

/// Stages 5-11: the six buckets every deployment gets, then the two it may
/// have declined. Split out of [`run`] as one unit because they are one
/// concern, and because `run` is otherwise a list of unrelated stages.
async fn ensure_buckets(
    client: &GcpClient,
    project_id: &str,
    config: &SetupConfig,
    names: &BucketNames,
    public_base_url: &str,
) -> SetupResult<()> {
    let region = &config.region;
    progress(
        project_id,
        5,
        &format!("private assets bucket {}", names.assets),
    );
    buckets::ensure_bucket(client, project_id, &names.assets, region).await?;
    buckets::ensure_assets_cors(client, &names.assets, public_base_url).await?;

    progress(
        project_id,
        6,
        &format!("private documents bucket {}", names.documents),
    );
    buckets::ensure_bucket(client, project_id, &names.documents, region).await?;

    progress(
        project_id,
        7,
        &format!("private exports bucket {}", names.exports),
    );
    buckets::ensure_bucket(client, project_id, &names.exports, region).await?;

    progress(
        project_id,
        8,
        &format!(
            "private logs bucket {} and PostgreSQL backups bucket {}",
            names.logs, names.postgres_backups
        ),
    );
    buckets::ensure_bucket(client, project_id, &names.logs, region).await?;
    workload_identity::ensure_ship_transcript_writer(client, project_id, &names.logs).await?;
    // The backups bucket's expiry exists before the first WAL segment lands,
    // and only the backup Kubernetes account is granted on it.
    buckets::ensure_bucket(client, project_id, &names.postgres_backups, region).await?;
    buckets::ensure_lifecycle(client, &names.postgres_backups).await?;
    workload_identity::ensure_postgres_backup_writer(
        client,
        project_id,
        config,
        &names.postgres_backups,
    )
    .await?;

    progress(
        project_id,
        9,
        &format!("private applications bucket {}", names.applications),
    );
    buckets::ensure_bucket(client, project_id, &names.applications, region).await?;
    // The ten-year orphaned-asset expiry. The publish must still overwrite
    // unconditionally (see `APPLICATIONS_RETENTION_DAYS`).
    buckets::ensure_lifecycle(client, &names.applications).await?;

    ensure_optional_bucket(
        client,
        project_id,
        region,
        10,
        "Iceberg archive bucket",
        names.archives.as_deref(),
    )
    .await?;
    ensure_optional_bucket(
        client,
        project_id,
        region,
        10,
        "Project Recovery Archive bucket",
        names.archived_projects.as_deref(),
    )
    .await?;
    ensure_optional_bucket(
        client,
        project_id,
        region,
        11,
        &format!(
            "telemetry landing bucket ({}-day expiry)",
            buckets::TELEMETRY_RETENTION_DAYS
        ),
        names.telemetry.as_deref(),
    )
    .await
}

/// Stage 17: logs, not backups — the backups bucket's window is set with its
/// bucket in stage 8. Last because nothing depends on it, and an expiry that
/// lands after the first objects still governs them.
async fn ensure_log_retention(
    client: &GcpClient,
    project_id: &str,
    logs_bucket: &str,
) -> SetupResult<()> {
    progress(
        project_id,
        17,
        &format!(
            "log retention ({}-day expiry) on {logs_bucket} and Cloud Logging _Default",
            logging::LOG_RETENTION_DAYS
        ),
    );
    buckets::ensure_lifecycle(client, logs_bucket).await?;
    logging::ensure_default_log_retention(client, project_id).await
}

/// Create one bucket the deployment may have declined, and reconcile its
/// lifecycle to whatever its kind calls for.
///
/// `None` is a first-class answer, not an error: a deployment that names no
/// archive or telemetry bucket keeps the original four-bucket shape. It still
/// prints a stage line, so the operator sees that the stage ran and chose to
/// do nothing — a silently absent stage reads as a stage that was forgotten.
///
/// Lifecycle is reconciled immediately after the create rather than in a later
/// pass, so a telemetry bucket cannot accept its first object before the expiry
/// that governs it exists.
async fn ensure_optional_bucket(
    client: &GcpClient,
    project_id: &str,
    region: &str,
    stage: usize,
    label: &str,
    name: Option<&str>,
) -> SetupResult<()> {
    match name {
        Some(name) => {
            progress(project_id, stage, &format!("{label} {name}"));
            buckets::ensure_bucket(client, project_id, name, region).await?;
            buckets::ensure_lifecycle(client, name).await?;
        }
        None => progress(
            project_id,
            stage,
            &format!("{label} (not configured; skipped)"),
        ),
    }
    Ok(())
}

/// Run the full setup pipeline. See module docs for the order.
pub async fn run(client: &GcpClient, project_id: &str, config: &SetupConfig) -> SetupResult<()> {
    tenants::validate_target(tenants::TenantRole::Environment, project_id)?;
    if let Some(images_project_id) = config.images_project_id.as_deref() {
        tenants::validate_images_project(project_id, images_project_id)?;
    }
    let public_base_url = config
        .public_base_url
        .as_deref()
        .ok_or(SetupError::MissingConfiguration("NAV_BASE_URL"))?;
    buckets::validate_public_base_url(public_base_url)?;
    if let Some(ship) = &config.release_ship {
        ship.validate()?;
    }
    if let Some(member) = &config.kms_operator {
        kms::validate_operator(member)?;
    }
    let names = BucketNames::resolve(project_id, config);

    progress(project_id, 1, "enable required APIs");
    services::enable_services(client, project_id).await?;

    progress(project_id, 2, &format!("VPC {}", config.vpc_name));
    network::ensure_network(client, project_id, config).await?;

    progress(
        project_id,
        3,
        &format!(
            "regional subnet {} ({})",
            config.subnetwork_name, config.region
        ),
    );
    network::ensure_named_subnetwork(
        client,
        project_id,
        &config.region,
        &config.vpc_name,
        &config.subnetwork_name,
    )
    .await?;

    let router_name = format!("{}-router", config.cluster_name);
    let nat_name = format!("{}-nat", config.cluster_name);
    progress(
        project_id,
        4,
        &format!("Cloud Router {router_name} and NAT {nat_name}"),
    );
    network::ensure_router_and_nat(client, project_id, config).await?;

    ensure_buckets(client, project_id, config, &names, public_base_url).await?;

    progress(
        project_id,
        12,
        &format!(
            "runtime identities and IAM bindings ({})",
            config.google_service_account_id
        ),
    );
    workload_identity::ensure_runtime_identity(client, project_id, config, &names.granted())
        .await?;

    // Registry before the cluster: GKE nodes pull the app images from
    // it, and the reader binding must exist before the first pull.
    progress(project_id, 13, "container registry access");
    // The provider admits the deployment's one Project-repository owner.
    artifact_registry::ensure(
        client,
        project_id,
        config,
        config.applications_publisher_org.as_deref(),
    )
    .await?;

    progress(
        project_id,
        14,
        &format!("GKE Autopilot cluster {}", config.cluster_name),
    );
    gke::ensure_autopilot_cluster_foundation(client, project_id, config).await?;

    progress(
        project_id,
        15,
        "Kubernetes workload identity and cluster integrations",
    );
    workload_identity::bind_kubernetes_accounts(client, project_id, config).await?;
    gke::ensure_cluster_integrations(client, project_id, config).await?;

    // The key this deployment's `secrets.enc.yaml` is encrypted against, in
    // this deployment's own project. It depends on nothing above it except
    // stage 1 enabling the API — it is last because it is cheap and
    // independent, not because anything waits on it.
    // Printed in full: this is the exact string that must appear as `kms_key`
    // in this deployment's `config.toml` and in its `.sops.yaml` creation
    // rule, so an operator can compare the three without deriving anything.
    progress(project_id, 16, &kms::key_name(project_id, &config.region));
    kms::ensure(client, project_id, &config.region).await?;
    if let Some(member) = &config.kms_operator {
        kms::ensure_operator(client, project_id, &config.region, member).await?;
    }

    ensure_log_retention(client, project_id, &names.logs).await?;
    ensure_release_ship(client, project_id, config, &names).await?;

    eprintln!("gcp setup [{project_id}] COMPLETE");
    Ok(())
}

/// Stages 18-19: the release-ship identity (only when its flags name it) and
/// the backup-target `ConfigMap` the ship requires.
async fn ensure_release_ship(
    client: &GcpClient,
    project_id: &str,
    config: &SetupConfig,
    names: &BucketNames,
) -> SetupResult<()> {
    match &config.release_ship {
        Some(ship) => {
            progress(
                project_id,
                18,
                &format!(
                    "release ship identity for {} ({} / {})",
                    ship.deployment, ship.repository, ship.environment
                ),
            );
            release_ship::ensure(client, project_id, config, ship, &names.assets).await?;
        }
        None => progress(
            project_id,
            18,
            "release ship identity (not configured; skipped)",
        ),
    }

    progress(
        project_id,
        19,
        &format!(
            "{} ConfigMap in {}",
            release_ship::POSTGRES_BACKUP_TARGET_CONFIGMAP,
            config.kubernetes_namespace
        ),
    );
    release_ship::ensure_postgres_backup_target(client, project_id, config, &names.postgres_backups)
        .await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::client::{GcpClient, GcpService, RecordedCall, StaticToken};

    /// A dry-run client with every service pointed at an unreachable address,
    /// so a real HTTP call would fail loudly instead of escaping the test.
    fn offline_dry_run_client() -> GcpClient {
        GcpClient::new(Arc::new(StaticToken("t".into())))
            .with_base_url(GcpService::ServiceUsage, "http://127.0.0.1:1")
            .with_base_url(GcpService::Compute, "http://127.0.0.1:1")
            .with_base_url(GcpService::Storage, "http://127.0.0.1:1")
            .with_base_url(GcpService::ArtifactRegistry, "http://127.0.0.1:1")
            .with_base_url(GcpService::Iam, "http://127.0.0.1:1")
            .with_base_url(GcpService::CloudResourceManager, "http://127.0.0.1:1")
            .with_base_url(GcpService::CloudKms, "http://127.0.0.1:1")
            .with_dry_run()
    }

    /// Assert a recorded request body mentions `needle`, naming the pipeline
    /// step in the failure so a mismatch says which call drifted.
    fn assert_body_contains(call: &RecordedCall, needle: &str, step: &str) {
        let body = call.body.as_deref().unwrap_or_default();
        assert!(
            body.contains(needle),
            "{step}: expected `{needle}` in {body:?}"
        );
    }

    #[test]
    fn progress_lines_name_the_project_stage_and_resource_without_secrets() {
        let line = super::progress_line(
            "neon-law-test",
            5,
            super::SETUP_STAGE_COUNT,
            "private assets bucket example-a-assets",
        );
        assert_eq!(
            line,
            "gcp setup [neon-law-test] 05/19 private assets bucket example-a-assets"
        );
        assert!(!line.contains("password"));
    }

    #[tokio::test]
    async fn invalid_kms_operator_stops_setup_before_any_cloud_call() {
        let client = offline_dry_run_client();
        let config = super::SetupConfig {
            public_base_url: Some("https://www.example.test".into()),
            kms_operator: Some("allUsers".into()),
            ..super::SetupConfig::default()
        };
        let error = super::run(&client, "my-project", &config)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("--kms-operator"), "{error}");
        assert!(client.recorded_calls().is_empty());
    }

    #[tokio::test]
    async fn setup_records_operator_grant_only_on_its_deployment_key() {
        let client = offline_dry_run_client();
        let config = super::SetupConfig {
            public_base_url: Some("https://www.example.test".into()),
            kms_operator: Some("user:operator@example.com".into()),
            ..super::SetupConfig::default()
        };
        super::run(&client, "my-project", &config).await.unwrap();
        let calls = client.recorded_calls();
        let grants: Vec<_> = calls
            .iter()
            .filter(|call| {
                call.body
                    .as_deref()
                    .is_some_and(|body| body.contains("roles/cloudkms.cryptoKeyEncrypterDecrypter"))
            })
            .collect();
        assert_eq!(grants.len(), 1);
        assert_eq!(
            grants[0].url,
            format!(
                "http://127.0.0.1:1/v1/{}:setIamPolicy",
                super::kms::key_name("my-project", &config.region)
            )
        );
        assert!(grants[0]
            .body
            .as_deref()
            .unwrap()
            .contains("user:operator@example.com"));
    }

    #[tokio::test]
    async fn a_malformed_public_base_url_is_rejected_before_any_gcp_call() {
        let client = offline_dry_run_client();
        let config = super::SetupConfig {
            config_sync_repo: Some("https://example.com/your-org/your-repo".into()),
            // A path-bearing origin: valid to `Url::parse`, invalid as a CORS
            // origin. The pipeline must refuse it up front rather than fail at
            // step 4a against an already half-provisioned project.
            public_base_url: Some("https://www.example.test/app".into()),
            ..super::SetupConfig::default()
        };
        let err = super::run(&client, "my-project", &config)
            .await
            .expect_err("a path-bearing NAV_BASE_URL must not provision anything");

        assert!(err.to_string().contains("NAV_BASE_URL"), "{err}");
        assert!(
            client.recorded_calls().is_empty(),
            "no GCP call may precede the NAV_BASE_URL check, got {:?}",
            client.recorded_calls()
        );
    }

    #[tokio::test]
    async fn the_environment_pipeline_refuses_the_hub_project_before_any_gcp_call() {
        let client = offline_dry_run_client();
        let config = super::SetupConfig {
            public_base_url: Some("https://www.example.test".into()),
            ..super::SetupConfig::default()
        };
        let err = super::run(&client, super::tenants::HUB_PROJECT_ID, &config)
            .await
            .expect_err("the hub must never receive buckets or GKE");

        assert!(err.to_string().contains("the image hub"), "{err}");
        assert!(
            client.recorded_calls().is_empty(),
            "the tenant guard must precede every GCP call, got {:?}",
            client.recorded_calls()
        );
    }

    #[tokio::test]
    async fn an_environment_may_not_name_itself_as_its_own_image_hub() {
        let client = offline_dry_run_client();
        let config = super::SetupConfig {
            public_base_url: Some("https://www.example.test".into()),
            images_project_id: Some("neon-law".into()),
            ..super::SetupConfig::default()
        };
        let err = super::run(&client, "neon-law", &config)
            .await
            .expect_err("an environment never hosts the shared registry");

        assert!(err.to_string().contains("neon-law"), "{err}");
        assert!(client.recorded_calls().is_empty(), "guard runs first");
    }

    /// With `--images-project-id`, the environment takes a reader binding on
    /// the *hub's* repository and creates no registry, CI pusher, or WIF pool
    /// of its own — those live in the hub, provisioned by `ops gcp hub setup`.
    #[tokio::test]
    async fn images_project_id_records_only_a_cross_project_reader_grant() {
        let client = offline_dry_run_client();
        let config = super::SetupConfig {
            public_base_url: Some("https://www.example.test".into()),
            images_project_id: Some(super::tenants::HUB_PROJECT_ID.into()),
            ..super::SetupConfig::default()
        };
        super::run(&client, "neon-law", &config).await.unwrap();

        let calls = client.recorded_calls();
        let registry_calls: Vec<_> = calls
            .iter()
            .filter(|c| c.url.contains("/repositories") || c.url.contains("workloadIdentityPools"))
            .collect();
        // Exactly the getIamPolicy read and the setIamPolicy write on the hub
        // repository — down from the eleven the single-project shape records.
        assert_eq!(
            registry_calls.len(),
            2,
            "expected only the hub reader binding, got {registry_calls:?}"
        );
        for call in &registry_calls {
            assert!(
                call.url.contains("/projects/ghcr/"),
                "the binding is written in the hub project, not the environment: {call:?}"
            );
        }
        assert!(
            registry_calls[1].url.ends_with(":setIamPolicy"),
            "{:?}",
            registry_calls[1]
        );
        assert_body_contains(
            registry_calls[1],
            "roles/artifactregistry.reader",
            "cross-project reader role",
        );
        assert_body_contains(
            registry_calls[1],
            "-compute@developer.gserviceaccount.com",
            "the environment's own workload puller is the granted principal",
        );

        // The service-account create path is a bare POST to `/serviceAccounts`;
        // the IAM verbs suffix the resource with `:getIamPolicy`/`:setIamPolicy`.
        assert!(
            !calls.iter().any(|c| c.url.ends_with("/serviceAccounts")),
            "the CI pusher lives in the hub, not the environment: {calls:?}"
        );
    }

    // One end-to-end assertion intentionally names every provisioning stage;
    // splitting it would weaken the guarantee that a dry run covers the full
    // empty-project pipeline without network traffic.
    #[allow(clippy::too_many_lines)]
    #[tokio::test]
    async fn dry_run_records_full_pipeline_with_no_network_traffic() {
        let client = offline_dry_run_client();
        let config = super::SetupConfig {
            config_sync_repo: Some("https://example.com/your-org/your-repo".into()),
            public_base_url: Some("https://www.example.test".into()),
            ..super::SetupConfig::default()
        };
        super::run(&client, "my-project", &config).await.unwrap();

        let calls = client.recorded_calls();
        // REST: 2 services.batchEnable + network + subnet + router/NAT + 6 storage inserts
        // (assets, documents, exports, logs, postgres-backups, applications) + 1
        // assets CORS read + 1 assets CORS patch + 1 postgres-backups lifecycle
        // patch + 1 applications lifecycle patch = 15.
        // Runtime identity: runtime GSA + isolated Drive GSA + 2 project roles
        // + 5 bucket roles + self-signing role = 9.
        // Artifact Registry: repo
        // create + cleanup patch + SA create +
        // writer get/set + reader get/set + WIF pool + WIF provider +
        // impersonation get/set = 11 (project number is short-circuited in
        // dry-run). SHELL (gke): gateway IP + create-auto = 2, followed by 2
        // KSA bindings, then fleet-enable + fleet-register + RootSync = 3.
        // KMS: key ring + crypto key = 2. The release transcript identity and
        // prefix-scoped binding add two calls after the logs bucket, and the
        // PostgreSQL backup Kubernetes account's bucket binding adds one.
        // Log retention: logs lifecycle patch + Cloud Logging `_Default` = 2.
        // No ship flags, so the release-ship stage records nothing; the
        // backup-target ConfigMap adds get-credentials + apply = 2.
        assert_eq!(calls.len(), 52, "expected 52 calls, got {calls:?}");
        let urls: Vec<&str> = calls.iter().map(|c| c.url.as_str()).collect();
        let methods: Vec<&str> = calls.iter().map(|c| c.method).collect();

        // The claim in this test's name, asserted rather than assumed.
        //
        // `offline_dry_run_client` overrides one base URL per service, so a
        // service added to `GcpService` without a matching override keeps its
        // production URL and this "offline" pipeline aims at real GCP. Dry-run
        // only records, so nothing escapes today — but the omission is
        // invisible in a passing call count, and the next reader would inherit
        // a client that is offline only by coincidence. `CloudKms` arrived
        // exactly that way.
        for (method, url) in methods.iter().zip(urls.iter()) {
            assert!(
                *method == "SHELL" || url.starts_with("http://127.0.0.1:1"),
                "{method} {url} escapes the offline client — add its GcpService \
                 to offline_dry_run_client"
            );
        }

        assert!(
            urls[0].contains("services:batchEnable"),
            "step 1 services: {}",
            urls[0]
        );
        assert!(
            urls[1].contains("services:batchEnable"),
            "step 1b services: {}",
            urls[1]
        );
        assert!(
            urls[2].contains("/global/networks"),
            "step 2 network: {}",
            urls[2]
        );
        assert!(
            urls[3].contains("/regions/example-region/subnetworks"),
            "step 2b subnet: {}",
            urls[3]
        );
        assert!(
            urls[4].contains("/regions/example-region/routers"),
            "step 3 router and NAT: {}",
            urls[4]
        );
        assert_body_contains(&calls[4], "AUTO_ONLY", "step 3 NAT auto-allocated IPs");
        assert_body_contains(
            &calls[4],
            "ALL_SUBNETWORKS_ALL_IP_RANGES",
            "step 3 NAT all subnet ranges",
        );
        // No database stage sits between the subnet and the buckets: the
        // provisioner creates no instance, and opens no route to one.
        for (method, url) in methods.iter().zip(urls.iter()) {
            assert!(
                !url.contains("/instances") && !url.contains("sql"),
                "the retired managed-database stage must record nothing: {method} {url}"
            );
        }
        assert_body_contains(&calls[5], "my-project-assets", "step 4a assets bucket");
        assert_eq!(methods[6], "GET", "step 4a CORS read: {}", urls[6]);
        assert_eq!(methods[7], "PATCH", "step 4a CORS: {}", urls[7]);
        assert_body_contains(&calls[7], "maxAgeSeconds", "step 4a CORS body");
        assert_body_contains(
            &calls[7],
            "https://www.example.test",
            "step 4a CORS must use the configured public origin",
        );
        assert_body_contains(
            &calls[8],
            "my-project-documents",
            "step 4b documents bucket",
        );
        assert_body_contains(&calls[9], "my-project-exports", "step 4c exports bucket");
        assert_body_contains(&calls[10], "my-project-logs", "step 4d logs bucket");
        assert_body_contains(
            &calls[13],
            "my-project-postgres-backups",
            "step 4e PostgreSQL backups bucket",
        );
        assert_eq!(
            methods[14], "PATCH",
            "step 4e PostgreSQL backups lifecycle: {}",
            urls[14]
        );
        assert_body_contains(
            &calls[14],
            &format!(
                "\"age\":{}",
                super::buckets::POSTGRES_BACKUPS_RETENTION_DAYS
            ),
            "step 4e PostgreSQL backups expire at the retention window",
        );
        assert!(
            urls[15].contains("gs://my-project-postgres-backups")
                && urls[15].contains("my-project.svc.id.goog[navigator/navigator-postgres-backup]")
                && urls[15].contains("roles/storage.objectAdmin"),
            "step 4e PostgreSQL backup Kubernetes account grant: {}",
            urls[15]
        );
        // The web runtime identity gets no access to database backups.
        assert!(
            !calls
                .iter()
                .any(|call| call.url.contains("gs://my-project-postgres-backups")
                    && call.url.contains("navigator-web@")),
            "the runtime identity must not be granted the backups bucket: {calls:?}"
        );
        assert_body_contains(
            &calls[16],
            "my-project-applications",
            "step 4f applications bucket",
        );
        assert_eq!(
            methods[17], "PATCH",
            "step 4f applications lifecycle: {}",
            urls[17]
        );
        assert_body_contains(
            &calls[17],
            &format!("\"age\":{}", super::buckets::APPLICATIONS_RETENTION_DAYS),
            "step 4f applications bucket expires orphaned assets at the retention limit",
        );
        // Steps 18..=26 are direct runtime and Workspace identity shell-outs.
        for (i, m) in methods.iter().enumerate().take(27).skip(18) {
            assert_eq!(*m, "SHELL", "step {i} should be SHELL, got {m}");
        }
        assert!(urls[18].contains("service-accounts create navigator-web"));
        assert!(urls[19].contains("service-accounts create navigator-drive"));
        assert!(urls[20].contains("roles/secretmanager.secretAccessor"));
        assert!(urls[21].contains("roles/aiplatform.user"));
        assert!(urls[27].contains("roles/iam.serviceAccountTokenCreator"));

        // Steps 28..=38 are the Artifact Registry REST calls.
        assert!(
            urls[28].contains("/repositories?repositoryId=navigator"),
            "step 5a repo create: {}",
            urls[28]
        );
        assert_eq!(methods[29], "PATCH", "step 5b cleanup policy: {}", urls[29]);
        // Retention is a version COUNT, not an age. Both halves are asserted
        // because the DELETE half matches every version and would empty the
        // repository without its KEEP partner.
        assert_body_contains(&calls[29], "\"keepCount\":10", "step 5b retained versions");
        assert_body_contains(&calls[29], "\"action\":\"KEEP\"", "step 5b keep policy");
        assert!(
            urls[30].ends_with("/serviceAccounts"),
            "step 5c CI service account: {}",
            urls[30]
        );
        assert!(
            urls[36].contains("workloadIdentityPools/github/providers"),
            "step 5j WIF provider: {}",
            urls[36]
        );
        assert_body_contains(
            &calls[36],
            "assertion.repository_owner == 'neon-law' && assertion.ref == 'refs/heads/main'",
            "step 5j WIF provider owner condition",
        );
        assert_body_contains(
            &calls[36],
            &super::artifact_registry::wif_attribute_condition(super::DEFAULT_GITHUB_REPO),
            "step 5j WIF provider Navigator grant",
        );
        assert_body_contains(
            &calls[36],
            super::artifact_registry::GITHUB_OIDC_ISSUER,
            "step 5j WIF provider issuer",
        );
        // Steps 39..=45 create GKE, bind its pool, then add integrations.
        // Bounded rather than open-ended: these are the shell-out steps, not
        // "everything after 36" — the KMS stage below is REST and follows them.
        for (i, m) in methods.iter().enumerate().take(46).skip(39) {
            assert_eq!(*m, "SHELL", "step {i} should be SHELL, got {m}");
        }
        assert!(
            urls[39].contains("compute addresses create"),
            "step 6a static IP: {}",
            urls[39]
        );
        assert!(
            urls[40].contains("container clusters create-auto"),
            "step 6b cluster: {}",
            urls[40]
        );
        assert!(urls[41].contains("navigator/navigator-web"));
        assert!(urls[42].contains("navigator/workflows-service"));
        assert!(
            urls[43].contains("fleet config-management enable"),
            "step 6c fleet enable: {}",
            urls[43]
        );
        assert!(
            urls[44].contains(&format!(
                "container clusters update {}",
                config.cluster_name
            )) && urls[44].contains("--enable-fleet"),
            "step 6d fleet reconciliation through the GKE cluster API: {}",
            urls[44]
        );
        assert!(
            urls[45].starts_with("kubectl apply"),
            "step 6e kubectl apply: {}",
            urls[45]
        );
        // Step 8: the key this deployment's `secrets.enc.yaml` is encrypted
        // against. Ring before key — the key create 404s otherwise.
        assert!(
            urls[46].contains("keyRings?keyRingId=navigator-secrets"),
            "step 8a key ring: {}",
            urls[46]
        );
        assert!(
            urls[47]
                .contains("keyRings/navigator-secrets/cryptoKeys?cryptoKeyId=deployment-config"),
            "step 8b crypto key: {}",
            urls[47]
        );
        assert_body_contains(&calls[47], "ENCRYPT_DECRYPT", "step 8b key purpose");
        // Step 10: logs keep for the log window, independent of backups.
        assert_eq!(
            methods[48], "PATCH",
            "step 10a logs lifecycle: {}",
            urls[48]
        );
        assert!(urls[48].contains("my-project-logs"), "{}", urls[48]);
        assert_body_contains(
            &calls[48],
            &format!("\"age\":{}", super::logging::LOG_RETENTION_DAYS),
            "step 10a logs bucket expires at the log window",
        );
        assert!(
            urls[49].contains("logging buckets update _Default")
                && urls[49].contains("--retention-days 180")
                && urls[49].contains("--project my-project"),
            "step 10b Cloud Logging retention: {}",
            urls[49]
        );
        assert!(
            urls[50].contains("container clusters get-credentials navigator-prod")
                && urls[50].contains("--project my-project"),
            "step 12a cluster credentials: {}",
            urls[50]
        );
        assert!(
            urls[51].contains("kubectl --context gke_my-project_example-region_navigator-prod")
                && urls[51].contains("--server-side"),
            "step 12b backup target apply: {}",
            urls[51]
        );
        assert_body_contains(
            &calls[51],
            "name: navigator-postgres-backup-target\n  namespace: navigator\ndata:\n  bucket: my-project-postgres-backups",
            "step 12b backup target names the backups bucket",
        );
    }

    #[tokio::test]
    async fn hostable_setup_pipeline_covers_every_cloud_substrate_stage() {
        let client = offline_dry_run_client();
        let config = super::SetupConfig {
            public_base_url: Some("https://www.example.test".into()),
            ..super::SetupConfig::default()
        };
        super::run(&client, "my-project", &config).await.unwrap();
        let calls = client.recorded_calls();
        let records = |needle: &str| {
            calls.iter().any(|call| {
                call.url.contains(needle)
                    || call
                        .body
                        .as_deref()
                        .is_some_and(|body| body.contains(needle))
            })
        };

        for (stage, needle) in [
            ("required APIs", "services:batchEnable"),
            ("custom VPC", "/global/networks"),
            ("regional subnet", "/regions/example-region/subnetworks"),
            ("Cloud Router and NAT", "/regions/example-region/routers"),
            ("runtime identity", "service-accounts create navigator-web"),
            ("registry access", "/repositories?repositoryId=navigator"),
            ("Autopilot cluster", "container clusters create-auto"),
            ("workload identity", "navigator/navigator-web"),
            ("Fleet membership", "container clusters update"),
            ("deployment KMS key", "keyRings?keyRingId=navigator-secrets"),
        ] {
            assert!(records(needle), "hostable setup omitted {stage}: {calls:?}");
        }
        for suffix in [
            super::ASSETS_BUCKET_SUFFIX,
            super::DOCUMENTS_BUCKET_SUFFIX,
            super::EXPORTS_BUCKET_SUFFIX,
            super::LOGS_BUCKET_SUFFIX,
            super::POSTGRES_BACKUPS_BUCKET_SUFFIX,
            super::APPLICATIONS_BUCKET_SUFFIX,
        ] {
            assert!(
                records(&format!("my-project{suffix}")),
                "hostable setup omitted the {suffix} bucket: {calls:?}"
            );
        }
        let nat = calls
            .iter()
            .find(|call| call.url.contains("/regions/example-region/routers"))
            .expect("hostable setup records the Cloud Router/NAT create");
        assert_body_contains(nat, "AUTO_ONLY", "hostable setup NAT IP allocation");
        assert_body_contains(
            nat,
            "ALL_SUBNETWORKS_ALL_IP_RANGES",
            "hostable setup NAT subnet coverage",
        );
    }

    #[tokio::test]
    async fn cluster_pool_exists_before_kubernetes_service_account_bindings() {
        let client = offline_dry_run_client();
        let config = super::SetupConfig {
            public_base_url: Some("https://www.example.test".into()),
            images_project_id: Some(super::tenants::HUB_PROJECT_ID.into()),
            ..super::SetupConfig::default()
        };
        super::run(&client, "my-project", &config).await.unwrap();

        let calls = client.recorded_calls();
        let cluster = calls
            .iter()
            .position(|call| call.url.contains("container clusters create-auto"))
            .expect("the pipeline creates the Autopilot cluster");
        let navigator_web_binding = calls
            .iter()
            .position(|call| {
                call.url.contains("roles/iam.workloadIdentityUser")
                    && call.url.contains("navigator/navigator-web")
            })
            .expect("the pipeline binds the navigator-web Kubernetes account");
        let workflows_binding = calls
            .iter()
            .position(|call| {
                call.url.contains("roles/iam.workloadIdentityUser")
                    && call.url.contains("navigator/workflows-service")
            })
            .expect("the pipeline binds the workflows-service Kubernetes account");

        assert!(
            cluster < navigator_web_binding,
            "the GKE workload identity pool must exist before navigator-web is bound: {calls:?}"
        );
        assert!(
            cluster < workflows_binding,
            "the GKE workload identity pool must exist before workflows-service is bound: {calls:?}"
        );
    }

    #[track_caller]
    fn assert_live_run_sheet(prose: &str) {
        assert!(
            prose.matches("navigator ops gcp hub setup").count() >= 2,
            "DEPLOY.md must show both dry-run and live image-hub setup commands",
        );
        // One live setup per deployments/ directory: the coordinates are
        // exported from that directory's config.toml, so the command sheet
        // never hardcodes a provider shell-out or a deployment that has no
        // config to provision from.
        assert!(
            prose.contains("$NAVIGATOR_DEPLOYMENTS_DIR/deployments/$ROW/config.toml"),
            "DEPLOY.md must load setup coordinates from the operator-managed deployment root",
        );
        assert!(
            prose.contains("navigator ops gcp setup --dry-run"),
            "DEPLOY.md must show the per-deployment dry-run setup command",
        );
        assert!(
            prose.contains("navigator ops gcp setup\n"),
            "DEPLOY.md must show the per-deployment live setup command",
        );
        assert!(
            prose.contains(
                "record the public value as `NAVIGATOR_GATEWAY_IP` in the matching private"
            ),
            "DEPLOY.md must keep gateway coordinates in the operator-managed row",
        );
    }

    #[test]
    fn gcp_setup_docs_name_the_adjacent_hosting_owners() {
        let cloud_operations = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../docs/cloud-operations.md"
        ))
        .expect("read cloud-operations.md");
        let deploy_workshop = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../server/content/workshops/navigator/DEPLOY.md"
        ))
        .expect("read DEPLOY.md");

        for (surface, prose) in [
            ("cloud-operations.md", cloud_operations.as_str()),
            ("DEPLOY.md", deploy_workshop.as_str()),
        ] {
            for expected in [
                "Cloud Router",
                "Cloud NAT",
                "`navigator ops dns setup`",
                "TLS and Gateway",
                "`navigator ops ship`",
                "Restate operator",
                "`navigator ops secrets apply`",
            ] {
                assert!(prose.contains(expected), "{surface} must name {expected}");
            }
        }
    }

    fn assert_workshop_setup_stage_prose(prose: &str) {
        assert!(
            prose.contains("Live setup prints nineteen secret-free stages"),
            "DEPLOY.md must keep the current setup-stage count",
        );
        assert!(
            prose.contains("04/19 Cloud Router"),
            "DEPLOY.md must include the Cloud Router/NAT setup stage",
        );
    }

    /// Cross-reference the "Deploy the Neon Law Navigator" workshop prose
    /// (`server/content/workshops/navigator/DEPLOY.md`) against the pipeline
    /// it teaches. If the prose names a service, bucket, or command the
    /// dry-run does not actually use — or omits one it does — this test
    /// fails and the workshop is stale. The *renderable* half of the
    /// contract (route, brand, stepped-content shape) lives in
    /// `features/tests/features/deploy_the_navigator_walkthrough.feature`;
    /// this is the half that can only run where `super::run` is reachable.
    #[tokio::test]
    async fn deploy_workshop_prose_matches_the_dry_run_pipeline() {
        use super::services::REQUIRED_SERVICES;

        let deploy_md = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../server/content/workshops/navigator/DEPLOY.md"
        );
        let prose = std::fs::read_to_string(deploy_md)
            .expect("read DEPLOY.md — the deploy workshop must exist for this grounding test");

        // Use the same placeholder project id the prose prints, so the
        // recorded bucket names line up with the names the workshop shows.
        let project_id = "your-project-id";
        let client = offline_dry_run_client();
        let config = super::SetupConfig {
            public_base_url: Some("https://www.example.test".into()),
            ..super::SetupConfig::default()
        };
        super::run(&client, project_id, &config).await.unwrap();
        let calls = client.recorded_calls();

        // 1. The command the workshop prints is the real invocation —
        //    `cargo run -p cli -- ops gcp setup`, with cargo's `--`
        //    separator (the orchestration commands live in the `navigator`
        //    CLI; there is no separate `devx` binary).
        assert!(
            prose.contains("cargo run -p cli -- ops gcp setup --project-id"),
            "DEPLOY.md must print the real `cargo run -p cli -- ops gcp setup --project-id` command",
        );
        assert!(
            prose.contains("--dry-run"),
            "DEPLOY.md must teach the --dry-run preview",
        );
        assert_workshop_setup_stage_prose(&prose);

        // 2. The prose's API count matches REQUIRED_SERVICES exactly,
        //    and each short name is named in the prose.
        assert_eq!(
            REQUIRED_SERVICES.len(),
            23,
            "the workshop says twenty-three APIs; keep prose and code in lockstep",
        );
        assert!(
            prose.contains("twenty-three"),
            "DEPLOY.md must state the API count in words (twenty-three)",
        );
        for svc in REQUIRED_SERVICES {
            let short = svc.strip_suffix(".googleapis.com").unwrap_or(svc);
            assert!(
                prose.contains(short),
                "DEPLOY.md must name the {svc} API (looked for `{short}`)",
            );
        }
        assert!(
            prose.contains("batchEnable"),
            "DEPLOY.md must name the serviceusage.batchEnable call",
        );

        // 3. Exactly six buckets are created, and the prose names each
        //    (this is what kills the stale "two buckets" drift).
        let mut bucket_names: Vec<String> = calls
            .iter()
            .filter_map(|c| c.body.as_deref())
            .flat_map(|body| {
                [
                    super::ASSETS_BUCKET_SUFFIX,
                    super::DOCUMENTS_BUCKET_SUFFIX,
                    super::EXPORTS_BUCKET_SUFFIX,
                    super::LOGS_BUCKET_SUFFIX,
                    super::POSTGRES_BACKUPS_BUCKET_SUFFIX,
                    super::APPLICATIONS_BUCKET_SUFFIX,
                ]
                .into_iter()
                .map(|suffix| format!("{project_id}{suffix}"))
                .filter(|name| body.contains(name.as_str()))
                .collect::<Vec<_>>()
            })
            .collect();
        bucket_names.sort();
        bucket_names.dedup();
        assert_eq!(
            bucket_names.len(),
            6,
            "pipeline must create exactly six buckets, got {bucket_names:?}",
        );
        for name in &bucket_names {
            assert!(
                prose.contains(name.as_str()),
                "DEPLOY.md must name the {name} bucket the pipeline creates",
            );
        }

        // 4. Idempotency: the prose teaches the 409-is-success rule.
        assert!(
            prose.contains("409"),
            "DEPLOY.md must explain that HTTP 409 means already-exists/success",
        );

        // 5. The run sheet contains one live command for the shared
        //    image hub and every isolated deployment. Matching the command
        //    through its newline distinguishes the live setup from the
        //    preceding `--dry-run` copy.
        assert_live_run_sheet(&prose);

        // 6. Scorpio's trust claim: the provisioner now generates no secret at
        //    all, so the workshop must not teach one being printed — and no
        //    literal credential may be baked into the prose either. A live
        //    secret is long and unbroken; honest prose is not, so a
        //    32-character alphanumeric run is the tell.
        assert!(
            !prose.contains("printed exactly once"),
            "setup prints no generated credential any more; DEPLOY.md must not promise one",
        );
        assert!(
            prose.contains("secrets.enc.yaml"),
            "DEPLOY.md must still name the sops destination for a deployment's own credentials",
        );
        let longest_alnum_run = prose
            .split(|c: char| !c.is_ascii_alphanumeric())
            .map(str::len)
            .max()
            .unwrap_or(0);
        assert!(
            longest_alnum_run < 32,
            "DEPLOY.md must not bake in a literal credential (found a {longest_alnum_run}-char token)",
        );
    }

    #[test]
    fn deploy_workshop_records_the_current_storage_checkpoint() {
        let deploy_md = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../server/content/workshops/navigator/DEPLOY.md"
        );
        let raw = std::fs::read_to_string(deploy_md)
            .expect("read DEPLOY.md — the deploy workshop must exist for this grounding test");
        // Collapse whitespace before matching. These are prose sentences, and
        // `navigator project gate` reflows prose to fill 120 columns — so a wording
        // edit anywhere earlier in a paragraph can push a pinned sentence across
        // a line break and fail this test for a reason that is not a regression.
        // The sibling grounding test in `cli/tests/workshop_command_grounding.rs`
        // already normalizes; this one matched raw bytes and broke exactly that
        // way. Line breaks are not part of the claim being pinned.
        let prose = raw.split_whitespace().collect::<Vec<_>>().join(" ");

        for observed in [
            "Six unconditional private buckets",
            "optional archive and telemetry lanes",
            "never creates a cloud bucket per Project",
            "The retired Portal's objects remain under a `{project-code}/portal/` prefix",
            "The documents bucket stores Project-scoped content-addressed `projects/<code>/documents/<sha>` objects",
            "Surreal asset rows carry the Project association",
        ] {
            assert!(
                prose.contains(observed),
                "DEPLOY.md must preserve the current storage checkpoint `{observed}`",
            );
        }
        for retired in [
            "An operator must export the two legacy production Postgres 15 instances",
            "created but unprovisioned project",
            "The two production clusters currently have no application namespace",
            "one private object-storage bucket per deployment",
            "issues/1103",
            "Each portal lives under its `{project-code}/portal/` prefix",
        ] {
            assert!(
                !prose.contains(retired),
                "DEPLOY.md must not preserve the retired substrate claim `{retired}`",
            );
        }
        for forbidden_domain in [
            "@example.invalid",
            "@example-project.iam.gserviceaccount.com",
        ] {
            assert!(
                !prose.contains(forbidden_domain),
                "DEPLOY.md must describe IAM principals without shipping email-shaped addresses from `{forbidden_domain}`",
            );
        }
        assert!(
            !prose.contains("remaining production rollout is incomplete")
                && !prose.contains("public DNS records are not reconciled")
                && !prose.contains("`example-project` completed stages 1–11")
                && !prose.contains("No Navigator release has been shipped")
                && !prose.contains("no Deployments, Pods, Services, Gateways, or HTTPRoutes"),
            "DEPLOY.md must not regress the completed substrates, DNS cutover, or staging rollout",
        );
    }
}

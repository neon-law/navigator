//! Machine-bound devx orchestration.
//!
//! Every function here drives KIND, Kubernetes, Docker, or a host process:
//! it shells out to `kind`/`kubectl`/`helm`/`docker`, spawns and reaps
//! port-forward children, probes host TCP ports, or reads and writes the
//! `.devx/` state directory. None of it is unit-testable — it is exercised
//! by the live `deploy.yml` / `dev up` gates against the persistent KIND
//! fixture, which is why the coverage gate in `ci.yml` excludes this file.
//! The pure/testable logic (`KindConfig` derivation, env and kind-config
//! rendering, `resolve_workflows_url`, the deploy-order guards) stays in the
//! parent module with its tests.
//!
//! `dispatch` and the sibling devx modules reach these functions through the
//! narrow `pub(super)` seam re-exported from `super`.

use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

use super::{
    normalize_docker_arch, parse_terminating, render_env, render_kind_config, restate_crd_path,
    seaweedfs, terminating_wedge_message, KindConfig, Terminating, DEFAULT_RAUTHY_HOST_PORT,
    INGRESS_MANIFEST, RESTATE_CRDS, RESTATE_OPERATOR_VERSION, WEB_IMAGE, WORKFLOWS_SERVICE_IMAGE,
};

/// `dev up [--path <checkout>]`: bring up the tier for `path`, or for the
/// workspace containing the current directory.
pub(super) fn up(path: Option<&Path>, cfg: &KindConfig) -> Result<()> {
    super::worktree_env::up_kind(&checkout_root(path)?, cfg)
}

/// Verify the exact existing origin before another local service changes a
/// database-targeted roles Job. This performs no provisioning or restamping.
pub(super) fn verified_native_database(root: &Path, cfg: &KindConfig) -> Result<String> {
    use super::forgejo::cluster::Kube;
    use sqlx::postgres::{PgConnectOptions, PgSslMode};
    super::worktree_env::verify_postgres_fixture_target(root, cfg)?;
    let intent = super::native_profile::Intent::read(root)?.context("native intent is missing")?;
    intent.verify_target(root, cfg)?;
    verify_fixture_port(cfg)?;
    let kube = Kube::new(cfg);
    let namespace: serde_json::Value =
        serde_json::from_str(&kube.output(&["get", "namespace", &cfg.namespace, "-o", "json"])?)?;
    let uid = namespace["metadata"]["uid"]
        .as_str()
        .context("namespace UID is missing")?
        .parse()?;
    let binding = intent.binding(uid)?;
    let password = kube
        .secret("navigator-postgres-auth")?
        .and_then(|mut data| data.remove("password"))
        .filter(|value| !value.is_empty())
        .context("native provisioner credential is missing")?;
    let options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(cfg.postgres_port)
        .database("postgres")
        .username("postgres")
        .password(&password)
        .ssl_mode(PgSslMode::VerifyFull)
        .ssl_root_cert(super::postgres_fixture::ca_path(root));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    Ok(runtime
        .block_on(store::postgres::native_origin::resume(
            options,
            intent.creation,
            binding,
        ))?
        .database())
}

/// Provision only a genuinely fresh native database, or verify the exact
/// resource recorded by this worktree. No Source client or Restate CLI is used.
pub(super) fn up_native_in(root: &Path, cfg: &KindConfig, no_deps: bool) -> Result<()> {
    use super::native_profile::{self, Intent};
    use store::postgres::native_origin;
    require_tools(&["kind", "kubectl", "docker", "openssl"])?;
    super::worktree_env::verify_postgres_fixture_target(root, cfg)?;
    let mut intent = Intent::read(root)?.context("native tier creation intent is missing")?;
    intent.verify_target(root, cfg)?;
    anyhow::ensure!(
        !no_deps || intent.was_ready(),
        "native tier has not completed initialization"
    );
    let mut reconcile = !no_deps && !intent.was_ready();
    let tier = prepare_native_dependencies(root, cfg, &mut intent, reconcile)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let resumed = runtime.block_on(async {
        if tier.created.is_some() {
            native_origin::create(tier.admin.clone(), intent.creation, tier.binding.clone())
                .await?
                .initialize()
                .await
        } else {
            native_origin::resume(tier.admin.clone(), intent.creation, tier.binding.clone()).await
        }
    });
    let native = match resumed {
        Ok(native) => native,
        Err(_error) if !no_deps && intent.was_ready() => {
            // A persisted intent authorizes only the canonical schema-only
            // upgrade API, never replacement of an unknown database.
            anyhow::ensure!(
                std::net::TcpStream::connect(("127.0.0.1", cfg.web_port)).is_err()
                    && tier
                        .kube
                        .output(&[
                            "get",
                            "deployment/navigator-web",
                            "--ignore-not-found",
                            "-o",
                            "name"
                        ])?
                        .trim()
                        .is_empty()
                    && std::net::TcpStream::connect(("127.0.0.1", native_profile::dbos_port(cfg)?))
                        .is_err(),
                "stop the owned app and durable worker before a native schema upgrade"
            );
            let operation = intent.prepare_upgrade(root)?;
            let upgraded = runtime.block_on(native_origin::upgrade(tier.admin.clone(), intent.creation,
                tier.binding.clone(), operation)).map_err(|_| anyhow!(
                    "native origin cannot resume or upgrade; explicit owned-resource recovery is required"
                ))?;
            reconcile = true;
            upgraded
        }
        Err(error) => return Err(error.into()),
    };
    let NativeEnvironment { state, app, values } =
        native_role_environment(root, cfg, &intent, &native, &tier, &runtime, reconcile)?;
    if reconcile {
        seed_native_application(root, &runtime, &app, &values)?;
        intent.mark_ready(root, &native)?;
    }
    let environment = super::postgres_fixture::render_environment(&values);
    state.write_env(&environment)?;
    write_fixture_private(
        &super::postgres_fixture::environment_path(root),
        super::postgres_fixture::render_environment(&tier.credentials.environment_for(
            cfg,
            root,
            &native.database(),
        ))
        .as_bytes(),
    )?;
    Ok(())
}

struct NativeDependencies {
    kube: super::forgejo::cluster::Kube,
    created: Option<CreatedKindCluster>,
    binding: store::postgres::native_origin::TierBinding,
    credentials: super::postgres_fixture::Credentials,
    rendered: String,
    admin: sqlx::postgres::PgConnectOptions,
}

fn prepare_native_dependencies(
    root: &Path,
    cfg: &KindConfig,
    intent: &mut super::native_profile::Intent,
    reconcile: bool,
) -> Result<NativeDependencies> {
    use super::forgejo::cluster::{prepare_postgres, Kube};
    use super::native_profile;
    use sqlx::postgres::{PgConnectOptions, PgSslMode};
    let created = if reconcile {
        kind_platform_steps(root, cfg)?
    } else {
        anyhow::ensure!(
            cluster_exists(&cfg.cluster)?,
            "native dependency tier does not exist"
        );
        configure_worktree_kubeconfig(root, cfg)?;
        None
    };
    let kube = Kube::new(cfg);
    if created.is_some() {
        ensure_namespace(&cfg.namespace)?;
    }
    let namespace: serde_json::Value =
        serde_json::from_str(&kube.output(&["get", "namespace", &cfg.namespace, "-o", "json"])?)?;
    let namespace = namespace["metadata"]["uid"]
        .as_str()
        .context("native namespace UID is missing")?
        .parse()?;
    if let Some(proof) = created.as_ref() {
        intent.bind_created_namespace(root, cfg, proof, namespace)?;
    }
    let binding = intent.binding(namespace)?;
    let container = verify_fixture_port(cfg)?;
    if reconcile {
        seaweedfs::prepare(&cfg.namespace)?;
    }
    let credentials = if reconcile {
        prepare_postgres(cfg)?
    } else {
        let password = |name| -> Result<String> {
            kube.secret(name)?
                .and_then(|mut data| data.remove("password"))
                .filter(|value| !value.is_empty())
                .context("native PostgreSQL credential is missing")
        };
        let tls = kube
            .secret("navigator-postgres-tls")?
            .context("native PostgreSQL TLS is missing")?;
        super::postgres_fixture::Credentials {
            app_password: password("navigator-postgres-app")?,
            owner_password: password("navigator-postgres-migrator")?,
            ca: super::postgres_fixture::trust_anchor(&tls)?.to_owned(),
        }
    };
    write_fixture_private(
        &super::postgres_fixture::ca_path(root),
        credentials.ca.as_bytes(),
    )?;
    let rendered = super::forgejo::cluster::kustomize(root, native_profile::OVERLAY)?;
    if reconcile {
        kube.apply(&native_profile::infrastructure(&rendered)?)?;
        apply_postgres_host_ingress(cfg, &kube, &container)?;
        for resource in [
            "statefulset/navigator-postgres",
            "statefulset/seaweedfs",
            "deployment/rauthy",
            "deployment/clamav",
            "deployment/openobserve",
        ] {
            kube.wait_rollout(resource, "1200s")?;
        }
        align_rauthy_public_url(cfg)?;
    }
    wait_for_tcp("127.0.0.1", cfg.postgres_port)?;
    let bootstrap = kube
        .secret("navigator-postgres-auth")?
        .and_then(|mut data| data.remove("password"))
        .filter(|value| !value.is_empty())
        .context("native database provisioner credential is missing")?;
    let admin = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(cfg.postgres_port)
        .database("postgres")
        .username("postgres")
        .password(&bootstrap)
        .ssl_mode(PgSslMode::VerifyFull)
        .ssl_root_cert(super::postgres_fixture::ca_path(root));
    Ok(NativeDependencies {
        kube,
        created,
        binding,
        credentials,
        rendered,
        admin,
    })
}

struct NativeEnvironment {
    state: StateDir,
    app: store::postgres::PostgresDb,
    values: std::collections::BTreeMap<String, String>,
}

fn native_role_environment(
    root: &Path,
    cfg: &KindConfig,
    intent: &super::native_profile::Intent,
    native: &store::postgres::native_origin::NativeDatabase,
    tier: &NativeDependencies,
    runtime: &tokio::runtime::Runtime,
    reconcile: bool,
) -> Result<NativeEnvironment> {
    use super::forgejo::cluster::secret_manifest;
    use super::native_profile;
    // No credential rotation or schema mutation is permitted by --no-deps.
    let login = |name: &str| -> Result<String> {
        if let Some(mut data) = tier.kube.secret(name)? {
            return data
                .remove("password")
                .filter(|value| !value.is_empty())
                .context("existing native login credential is invalid");
        }
        anyhow::ensure!(reconcile, "native login credential is missing");
        let password = seaweedfs::random_hex();
        tier.kube.apply(&secret_manifest(
            name,
            &cfg.namespace,
            "Opaque",
            &std::collections::BTreeMap::from([
                ("password".into(), password.clone()),
                ("ca.crt".into(), tier.credentials.ca.clone()),
            ]),
        )?)?;
        Ok(password)
    };
    let dbos_password = login("navigator-postgres-dbos")?;
    let archive_password = login("navigator-postgres-archive")?;
    if reconcile {
        tier.kube.run(&[
            "delete",
            "job",
            "navigator-postgres-roles",
            "--ignore-not-found",
            "--wait=true",
        ])?;
        tier.kube.apply(&native_profile::roles_for_database(
            &tier.rendered,
            &native.database(),
        )?)?;
        tier.kube.wait_job("navigator-postgres-roles")?;
    }
    let app = runtime.block_on(async {
        let pg = store::postgres::PostgresDb::connect(
            tier.admin
                .clone()
                .database(&native.database())
                .username("navigator_app")
                .password(&tier.credentials.app_password),
        )
        .await?;
        store::postgres::PostgresDb::with_application_db(pg.clone(), async {}).await?;
        Ok::<_, anyhow::Error>(pg)
    })?;
    let state = StateDir::new(root)?;
    state.kill_pids();
    state.write_pids(&[])?;
    let mut pids = Vec::new();
    for (service, local, remote) in [
        ("service/navigator-seaweedfs", cfg.seaweedfs_s3_port, 8333),
        ("service/clamav", cfg.clamav_port, 3310),
        ("service/openobserve", cfg.openobserve_port, 5080),
    ] {
        pids.push(port_forward(service, local, remote, cfg, &state)?);
        state.write_pids(&pids)?;
    }
    wait_for_tcp("127.0.0.1", cfg.seaweedfs_s3_port)?;
    let storage = native_storage_credentials(cfg, &tier.kube, reconcile)?;
    let values = native_profile::environment(
        root,
        cfg,
        intent,
        tier.credentials
            .environment_for(cfg, root, &native.database()),
        native_profile::RolePasswords {
            dbos: &dbos_password,
            archive: &archive_password,
        },
        &storage,
        |key| env::var(key).ok(),
    )?;
    Ok(NativeEnvironment { state, app, values })
}

fn native_storage_credentials(
    cfg: &KindConfig,
    kube: &super::forgejo::cluster::Kube,
    reconcile: bool,
) -> Result<seaweedfs::Credentials> {
    if reconcile {
        seaweedfs::provision(&cfg.namespace)
    } else {
        let data = kube
            .secret("navigator-seaweedfs-s3")?
            .context("native storage credentials are missing")?;
        let keys = seaweedfs::LANES
            .iter()
            .map(|(lane, prefix)| {
                let read = |suffix| {
                    data.get(&format!("{prefix}{suffix}"))
                        .cloned()
                        .filter(|value| !value.is_empty())
                        .context("native storage credential is incomplete")
                };
                Ok((
                    (*lane).to_owned(),
                    seaweedfs::LaneCredentials {
                        access_key: read("access_key")?,
                        secret_key: read("secret_key")?,
                    },
                ))
            })
            .collect::<Result<std::collections::BTreeMap<_, _>>>()?;
        seaweedfs::credentials(&keys)
    }
}

fn seed_native_application(
    root: &Path,
    runtime: &tokio::runtime::Runtime,
    app: &store::postgres::PostgresDb,
    values: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    // Preserve the same staged sample-source input as the compatibility
    // bootstrap. The following host boot reads it through its explicit env.
    super::sample_project::run_for_root(false, root)?;
    runtime.block_on(async {
        let pg = (*app).clone();
        store::clauses::seed(
            &pg,
            &rules::clauses::library_clauses().map_err(anyhow::Error::msg)?,
        )
        .await?;
        let storage: std::sync::Arc<dyn cloud::StorageService> =
            std::sync::Arc::new(cloud::S3Storage::new(cloud::S3StorageConfig::from_lookup(
                |key| values.get(key).cloned(),
            )?)?);
        store::postgres::PostgresDb::with_application_db(pg, async {
            store::seed::seed_environment_with_stage(
                &store::surreal::SurrealDb::uninitialized(),
                &storage,
                store::DeploymentEnvironment::Dev,
                store::seed::BrandSeed::Neon,
                values
                    .get(store::sample_project::STAGE_ENV)
                    .map(String::as_str),
            )
            .await
        })
        .await??;
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(())
}

/// Rebuild the current checkout's worker image and replace the running KIND
/// worker. This is the focused edit loop for `workflows-service` and shared
/// workflow code; host-side `web` edits do not need an image rebuild.
pub(super) fn reload_worker(cfg: &KindConfig) -> Result<()> {
    require_tools(&["kind", "kubectl", "docker"])?;
    let root = workspace_root()?;
    // Pin every kubectl call below to this checkout's cluster before touching
    // it. `build_and_load_worker` loads the image into `cfg.cluster` by name,
    // but the pod delete and readiness wait use whatever kubeconfig context is
    // ambient, so a stale or unrelated context would restart the wrong worker
    // and leave the rebuilt image unused. The development tier also pins its own
    // kubectl operations via `kind_up_steps`.
    configure_worktree_kubeconfig(&root, cfg)?;
    build_and_load_worker(&root, cfg)?;
    eprintln!("==> restarting workflows-service in {}", cfg.cluster);
    // Deleting the pod forces the RestateDeployment's ReplicaSet to recreate it
    // against the freshly `kind load`ed `:dev` image (the tag is unchanged, so
    // only a new pod picks it up under `IfNotPresent`). `--wait` is kubectl's
    // default, so this blocks until the outgoing pod is gone.
    run(Command::new("kubectl")
        .arg("--namespace")
        .arg(&cfg.namespace)
        .arg("delete")
        .arg("pod")
        .arg("--selector")
        .arg("app=workflows-service")
        .arg("--ignore-not-found"))?;
    // Gate on the *replacement* pod's readiness before trusting the
    // RestateDeployment CR: its `Ready` condition lags pod deletion, so waiting
    // on it alone can return against the pre-deletion state while the new pod is
    // still starting. Once the new pod is Ready, confirm the operator has
    // finished reconciling the deployment too.
    wait_for_selected_pod_ready(&cfg.namespace, "app=workflows-service")?;
    wait_for_condition(
        &cfg.namespace,
        "restatedeployment/workflows-service",
        "Ready",
    )
}

/// Build the worker from `root` and make its local `:dev` tag available to
/// every node of this KIND cluster. The manifest's `IfNotPresent` policy then
/// starts this exact source image without contacting a registry.
fn build_and_load_worker(root: &Path, cfg: &KindConfig) -> Result<()> {
    build_and_load_local_image(
        root,
        cfg,
        "images/Containerfile.workflows-service",
        WORKFLOWS_SERVICE_IMAGE,
    )
}

pub(super) fn build_and_load_local_image(
    root: &Path,
    cfg: &KindConfig,
    containerfile_path: &str,
    image: &str,
) -> Result<()> {
    let containerfile = root.join(containerfile_path);
    eprintln!(
        "==> docker build -f {} -t {image} {}",
        containerfile.display(),
        root.display()
    );
    run(Command::new("docker")
        .arg("build")
        .arg("--file")
        .arg(containerfile)
        .arg("--tag")
        .arg(image)
        .arg(root))?;
    kind_load_image_into_cluster(image, cfg, &format!("linux/{}", docker_daemon_arch()))
}

/// Point Rauthy's canonical public URL at this tier's browser-reachable origin.
///
/// Rauthy uses one `PUB_URL` for discovery, token validation, and browser
/// redirects. Host-run web reaches the mapped `NodePort` directly; full KIND web
/// reaches the same issuer through [`align_rauthy_in_cluster_client`].
///
/// The Deployment keeps `valueFrom` references to this Secret. Patching the
/// Secret instead of replacing those entries with literal values preserves a
/// shape that a later `kubectl apply` can reconcile. Restarting the Deployment
/// then makes the new values visible to the process.
pub(super) fn align_rauthy_public_url(cfg: &KindConfig) -> Result<()> {
    let origin = super::rauthy_origin(cfg.rauthy_port);
    eprintln!("==> aligning Rauthy public URL → {origin}");
    let output = Command::new("kubectl")
        .args(["--context", &cfg.kind_context(), "-n", &cfg.namespace])
        .args(["patch", "secret", "rauthy-secrets", "--type=merge", "-p"])
        .arg(super::rauthy_public_url_patch(cfg.rauthy_port))
        .output()
        .context("patch Rauthy public URL Secret")?;
    if !output.status.success() {
        bail!(
            "patch Rauthy public URL Secret failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    eprint!("{stdout}");
    if stdout.contains("(no change)") {
        return Ok(());
    }
    run(Command::new("kubectl")
        .args(["--context", &cfg.kind_context(), "-n", &cfg.namespace])
        .args(["rollout", "restart", "deployment/rauthy"]))
}

/// Return whether this local cluster has already converged on Rauthy.
///
/// Worktree reuse normally skips the expensive dependency apply when every
/// host port is reachable. A pre-Rauthy cluster also satisfies that probe
/// because its identity provider owns the same `NodePort`, so the deployment
/// identity is the additional convergence signal.
pub(super) fn rauthy_deployment_exists(cfg: &KindConfig) -> bool {
    Command::new("kubectl")
        .args(["--context", &cfg.kind_context(), "-n", &cfg.namespace])
        .args(["get", "deployment", "rauthy"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Release the local `NodePort` and remove the retired dependency-tier objects
/// before applying Rauthy. This is idempotent and touches only the disposable
/// identity-provider fixture in the selected KIND cluster.
fn retire_legacy_identity_provider(cfg: &KindConfig) -> Result<()> {
    run(Command::new("kubectl")
        .args(["--context", &cfg.kind_context(), "-n", &cfg.namespace])
        .args([
            "delete",
            "deployment/keycloak",
            "service/keycloak",
            "configmap/keycloak-realm",
            "secret/keycloak-realm-admin",
            "ingress/keycloak",
            "--ignore-not-found",
        ]))
}

/// Remove the Garage object store that `SeaweedFS` replaced. `kubectl apply`
/// never prunes, so a KIND cluster created before the switch keeps running
/// Garage beside `SeaweedFS`. Its claims go too: the bytes were disposable
/// fixtures. Idempotent.
fn retire_legacy_object_store(cfg: &KindConfig) -> Result<()> {
    run(Command::new("kubectl")
        .args(["--context", &cfg.kind_context(), "-n", &cfg.namespace])
        .args([
            "delete",
            "statefulset/garage",
            "service/navigator-garage",
            "service/navigator-garage-internal",
            "configmap/navigator-garage",
            "secret/navigator-garage-control",
            "secret/navigator-garage-s3",
            "persistentvolumeclaim/data-garage-0",
            "persistentvolumeclaim/meta-garage-0",
            "--ignore-not-found",
        ]))
}

/// Align a full-stack `navigator-web` pod with Rauthy's single public issuer.
///
/// The `ConfigMap` changes the sidecar's loopback listen port; the Deployment
/// patch changes web's issuer and triggers the rollout that remounts that
/// `ConfigMap`. Deps-only host-web environments never call this path.
fn align_rauthy_in_cluster_client(cfg: &KindConfig) -> Result<()> {
    let issuer = super::rauthy_issuer(cfg.rauthy_port);
    eprintln!("==> aligning in-cluster Rauthy client → {issuer}");
    run(Command::new("kubectl")
        .args(["--context", &cfg.kind_context(), "-n", &cfg.namespace])
        .args([
            "patch",
            "configmap",
            "rauthy-loopback-proxy",
            "--type=merge",
            "-p",
        ])
        .arg(super::rauthy_loopback_config_patch(cfg.rauthy_port)))?;
    run(Command::new("kubectl")
        .args(["--context", &cfg.kind_context(), "-n", &cfg.namespace])
        .args([
            "patch",
            "deployment",
            "navigator-web",
            "--type=strategic",
            "-p",
        ])
        .arg(super::navigator_web_rauthy_patch(cfg.rauthy_port)))
}

pub(super) fn hydrate_seaweedfs_environment(cfg: &KindConfig) -> Result<()> {
    seaweedfs::export(&seaweedfs::provision(&cfg.namespace)?);
    Ok(())
}

/// `dev kind up`: just create the cluster + install ingress +
/// install the Restate Operator. Don't apply application manifests.
pub(super) fn kind_up_only(cfg: &KindConfig) -> Result<()> {
    require_tools(&["kind", "kubectl", "helm"])?;
    let root = workspace_root()?;
    kind_up_steps(&root, cfg)
}

/// `dev kind down`: delete the KIND cluster.
pub(super) fn kind_down_only(cfg: &KindConfig) -> Result<()> {
    require_tools(&["kind"])?;
    let cluster = &cfg.cluster;
    if cluster_exists(cluster)? {
        eprintln!("==> deleting KIND cluster '{cluster}'");
        run(Command::new("kind")
            .arg("delete")
            .arg("cluster")
            .arg("--name")
            .arg(cluster))?;
    } else {
        eprintln!("==> KIND cluster '{cluster}' not found; nothing to delete");
    }
    Ok(())
}

pub(super) fn resolve_demo_image(tag_override: Option<&str>) -> Result<String> {
    let registry = super::registry::registry_from_env()?;
    let tag = match local_image_tag(tag_override)? {
        Some(tag) => tag,
        None => {
            super::registry::resolve_latest_tag(&registry, "neon-server").or_else(|list_error| {
                // Some registries permit an authenticated Docker pull but deny
                // the anonymous tag listing. The release embeds its immutable tag.
                let latest = super::registry::image_ref(&registry, "neon-server", "latest");
                run(Command::new("docker").args(["pull", "--platform", "linux/amd64", &latest]))
                    .with_context(|| format!("release tag lookup failed: {list_error}"))?;
                let inspect = Command::new("docker")
                    .args([
                        "image",
                        "inspect",
                        "--format",
                        "{{json .Config.Env}}",
                        &latest,
                    ])
                    .output()?;
                anyhow::ensure!(
                    inspect.status.success(),
                    "could not inspect the released Neon server image"
                );
                let fields: Vec<String> = serde_json::from_slice(&inspect.stdout)?;
                let tag = fields
                    .iter()
                    .find_map(|field| field.strip_prefix("NAVIGATOR_RELEASE_TAG="))
                    .context("published Neon server has no release tag")?;
                super::registry::validate_release_tag(tag)?;
                Ok::<_, anyhow::Error>(tag.to_owned())
            })?
        }
    };
    let image = super::registry::image_ref(&registry, "neon-server", &tag);
    run(Command::new("docker").args(["pull", "--platform", "linux/amd64", &image]))?;
    Ok(image)
}

pub(super) fn deploy_native_image(root: &Path, cfg: &KindConfig, image: &str) -> Result<()> {
    use super::forgejo::cluster::{kustomize, secret_manifest, Kube};
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use sha2::{Digest, Sha256};

    kind_load_image_into_cluster(image, cfg, "linux/amd64")?;
    let values = dotenvy::from_path_iter(root.join(".devx/env"))?
        .collect::<Result<std::collections::BTreeMap<_, _>, _>>()?;
    let values = super::native_profile::demo_environment(values, cfg);
    let secret = secret_manifest(
        "navigator-demo-environment",
        &cfg.namespace,
        "Opaque",
        &values,
    )?;
    let kube = Kube::new(cfg);
    kube.apply(&secret)?;
    kube.run(&[
        "patch",
        "configmap",
        "rauthy-loopback-proxy",
        "--type=merge",
        "-p",
        &super::rauthy_loopback_config_patch(cfg.rauthy_port),
    ])?;
    let rendered = kustomize(root, "k8s/overlays/kind-postgres-demo")?;
    let manifest = super::native_profile::demo_manifest(
        &rendered,
        image,
        &STANDARD.encode(Sha256::digest(secret.as_bytes())),
    )?;
    kube.apply(&manifest)?;
    kube.wait_rollout("deployment/navigator-web", "600s")?;
    Ok(())
}

/// `devx deploy`: full in-cluster stack from published Artifact Registry
/// images. Pulls both images at a resolved immutable release tag, retags + loads them
/// into KIND, applies every manifest under `k8s/`, waits for the
/// navigator-web rollout to settle. CI builds and publishes the images;
/// this pulls them. `tag_override` (e.g. `worktree-env --demo --tag`)
/// pins the release; else `NAVIGATOR_IMAGE_TAG`, else the latest
/// published tag is pulled.
pub(super) fn deploy_captured(
    root: &Path,
    cfg: &KindConfig,
    tag_override: Option<&str>,
) -> Result<()> {
    require_tools(&["kind", "kubectl", "docker", "helm"])?;
    // `kind_up_steps` is idempotent — safe to call when the cluster
    // is already up. Establishes the Operator + nginx-ingress
    // invariants `deploy` relies on.
    kind_up_steps(root, cfg)?;
    let image_registry = super::registry::registry_from_env()?;
    let tag = match local_image_tag(tag_override)? {
        Some(tag) => tag,
        None => super::registry::resolve_latest_tag(&image_registry, "navigator-web")?,
    };
    // Fail fast before any apply if either service image is missing the
    // resolved tag — a missing image would wedge the rollout in
    // ImagePullBackOff.
    super::registry::ensure_tag_published(&image_registry, "navigator-web", &tag)?;
    super::registry::ensure_tag_published(&image_registry, "navigator-workflows-service", &tag)?;
    pull_retag_load(&image_registry, "navigator-web", &tag, WEB_IMAGE, cfg)?;
    pull_retag_load(
        &image_registry,
        "navigator-workflows-service",
        &tag,
        WORKFLOWS_SERVICE_IMAGE,
        cfg,
    )?;
    seaweedfs::prepare(&cfg.namespace)?;
    retire_legacy_identity_provider(cfg)?;
    retire_legacy_object_store(cfg)?;
    apply_kustomize(root, &cfg.full_overlay)?;
    align_rauthy_public_url(cfg)?;
    align_rauthy_in_cluster_client(cfg)?;
    wait_rollout("statefulset", "seaweedfs", cfg)?;
    seaweedfs::provision(&cfg.namespace)?;
    bootstrap_postgres_in(root, cfg, "navigator")?;
    eprintln!("==> waiting for navigator-web rollout");
    run(Command::new("kubectl")
        .arg("--namespace")
        .arg(&cfg.namespace)
        .arg("rollout")
        .arg("status")
        .arg("deployment/navigator-web")
        .arg("--timeout=300s"))
}

/// `devx undeploy`: kubectl delete namespace navigator. Does NOT
/// touch the cluster — use `dev kind down` for that. The `--context` pin is
/// load-bearing: this deletes a whole namespace, and without it the delete
/// lands on whatever context is current, which on an operator's machine is as
/// likely to be prod as KIND.
pub(super) fn undeploy(cfg: &KindConfig) -> Result<()> {
    require_tools(&["kubectl"])?;
    run(Command::new("kubectl").args(super::undeploy_args(&cfg.kind_context(), &cfg.namespace)))
}

/// `devx logs`: tail navigator-web logs.
pub(super) fn logs(cfg: &KindConfig) -> Result<()> {
    require_tools(&["kubectl"])?;
    use_kind_context(cfg)?;
    run(Command::new("kubectl")
        .arg("--namespace")
        .arg(&cfg.namespace)
        .arg("logs")
        .arg("-f")
        .arg("deployment/navigator-web")
        .arg("-c")
        .arg("web"))
}

/// Switch kubectl to the KIND context for this cluster before any
/// cluster-mutating apply. `devx` is KIND-only; a stale GKE/EKS
/// context being current is otherwise indistinguishable from a fresh
/// KIND boot and would land manifests in the wrong place.
pub(super) fn use_kind_context(cfg: &KindConfig) -> Result<()> {
    let context = cfg.kind_context();
    let out = Command::new("kubectl")
        .args(["config", "get-contexts", "-o", "name"])
        .output()
        .with_context(|| "kubectl config get-contexts failed")?;
    if !out.status.success() {
        anyhow::bail!(
            "kubectl config get-contexts failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let contexts = String::from_utf8_lossy(&out.stdout);
    if !contexts.lines().any(|c| c == context) {
        anyhow::bail!(
            "kubectl context '{context}' not found; bring the KIND cluster up first \
             (`dev kind up`)."
        );
    }
    eprintln!("==> pinning kubectl context → {context}");
    run(Command::new("kubectl").args(super::use_context_args(&context)))
}

/// `dev kustomize kind` / `dev kustomize gke`: render a Kustomize
/// overlay to stdout. Inspect what `kubectl apply -k` would send
/// before sending it.
pub(super) fn kustomize_render(overlay: &str) -> Result<()> {
    require_tools(&["kubectl"])?;
    let root = workspace_root()?;
    run(Command::new("kubectl")
        .arg("kustomize")
        .arg(root.join(overlay)))
}

// ---------- shared helpers ----------

fn kind_nested_dind_requested() -> bool {
    matches!(
        env::var("NAVIGATOR_KIND_NO_HOSTPORT"),
        Ok(value) if !value.is_empty() && value != "0"
    )
}

/// Nested Docker (Cursor Cloud `DinD`) has no `xt_statistic` module, so
/// kube-proxy's default iptables mode fails `iptables-restore` and `ClusterIP`
/// DNS never works. On an already-created cluster, flip the `ConfigMap` to
/// nftables and restart the `DaemonSet`. New clusters get the same mode from
/// [`super::inject_kube_proxy_nftables`] in the KIND config.
fn ensure_kube_proxy_nftables_if_requested() -> Result<()> {
    if !kind_nested_dind_requested() {
        return Ok(());
    }
    let output = Command::new("kubectl")
        .args([
            "--namespace",
            "kube-system",
            "get",
            "configmap",
            "kube-proxy",
            "-o",
            "json",
        ])
        .output()
        .context("get kube-proxy ConfigMap")?;
    if !output.status.success() {
        bail!(
            "kubectl get configmap kube-proxy failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let mut cm: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("parse kube-proxy ConfigMap json")?;
    let Some(conf) = cm.pointer_mut("/data/config.conf") else {
        return Ok(());
    };
    let Some(text) = conf.as_str() else {
        return Ok(());
    };
    if text.lines().any(|line| line.trim() == "mode: nftables") {
        return Ok(());
    }
    eprintln!("==> switching kube-proxy to nftables (NAVIGATOR_KIND_NO_HOSTPORT)");
    let patched = text.replace("mode: iptables\n", "mode: nftables\n");
    if patched == text {
        bail!("kube-proxy ConfigMap has no `mode: iptables` line to replace with nftables");
    }
    *conf = serde_json::Value::String(patched);
    if let Some(meta) = cm.get_mut("metadata").and_then(|v| v.as_object_mut()) {
        meta.remove("resourceVersion");
        meta.remove("uid");
        meta.remove("generation");
        meta.remove("creationTimestamp");
        meta.remove("managedFields");
    }
    let tmp = tempfile::NamedTempFile::new().context("temp file for kube-proxy ConfigMap")?;
    let bytes = serde_json::to_vec(&cm).context("serialize kube-proxy ConfigMap")?;
    std::fs::write(tmp.path(), bytes).context("write kube-proxy ConfigMap")?;
    run(Command::new("kubectl")
        .arg("apply")
        .arg("-f")
        .arg(tmp.path()))?;
    run(Command::new("kubectl").args([
        "--namespace",
        "kube-system",
        "rollout",
        "restart",
        "daemonset/kube-proxy",
    ]))?;
    run(Command::new("kubectl").args([
        "--namespace",
        "kube-system",
        "rollout",
        "status",
        "daemonset/kube-proxy",
        "--timeout=120s",
    ]))?;
    Ok(())
}

/// Nested Docker (Cursor Cloud `DinD`) has no `xt_multiport` module, so kindnet
/// cannot DNAT the ingress-nginx `hostPort` 80/443 bindings. When
/// `NAVIGATOR_KIND_NO_HOSTPORT` is set, drop those hostPorts after apply so
/// the controller can become Ready. `dev up` reaches Rauthy and the other
/// deps through kubectl port-forwards, not those hostPorts.
fn strip_ingress_controller_host_ports_if_requested() -> Result<()> {
    if !kind_nested_dind_requested() {
        return Ok(());
    }
    eprintln!("==> stripping ingress-nginx hostPorts (NAVIGATOR_KIND_NO_HOSTPORT)");
    let output = Command::new("kubectl")
        .args([
            "--namespace",
            "ingress-nginx",
            "get",
            "deploy",
            "ingress-nginx-controller",
            "-o",
            "json",
        ])
        .output()
        .context("get ingress-nginx-controller for hostPort strip")?;
    if !output.status.success() {
        bail!(
            "kubectl get deploy ingress-nginx-controller failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let mut deploy: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("parse ingress-nginx-controller json")?;
    let Some(ports) = deploy.pointer_mut("/spec/template/spec/containers/0/ports") else {
        return Ok(());
    };
    let Some(arr) = ports.as_array_mut() else {
        return Ok(());
    };
    let mut stripped = false;
    for port in arr.iter_mut() {
        if let Some(obj) = port.as_object_mut() {
            if obj.remove("hostPort").is_some() {
                stripped = true;
            }
        }
    }
    if !stripped {
        return Ok(());
    }
    if let Some(meta) = deploy.get_mut("metadata").and_then(|v| v.as_object_mut()) {
        meta.remove("resourceVersion");
        meta.remove("uid");
        meta.remove("generation");
        meta.remove("creationTimestamp");
        meta.remove("managedFields");
    }
    if let Some(obj) = deploy.as_object_mut() {
        obj.remove("status");
    }
    if let Some(ann) = deploy.pointer_mut("/spec/template/metadata/annotations") {
        if let Some(obj) = ann.as_object_mut() {
            obj.insert(
                "navigator.devx/no-hostport".to_string(),
                serde_json::Value::String("1".into()),
            );
        }
    }
    let tmp = tempfile::NamedTempFile::new().context("temp file for ingress patch")?;
    let bytes = serde_json::to_vec(&deploy).context("serialize ingress patch")?;
    std::fs::write(tmp.path(), bytes).context("write ingress patch")?;
    run(Command::new("kubectl")
        .arg("apply")
        .arg("-f")
        .arg(tmp.path()))?;
    Ok(())
}

/// Proof that this invocation successfully created this exact KIND cluster.
/// Existing clusters never receive this capability.
pub(super) struct CreatedKindCluster {
    cluster: String,
}
impl CreatedKindCluster {
    pub(super) fn matches(&self, cfg: &KindConfig) -> bool {
        self.cluster == cfg.cluster
    }
}

pub(super) fn kind_platform_steps(
    root: &Path,
    cfg: &KindConfig,
) -> Result<Option<CreatedKindCluster>> {
    let cluster = &cfg.cluster;
    let mut created = None;
    if cluster_exists(cluster)? {
        eprintln!("==> KIND cluster '{cluster}' already exists, reusing");
    } else {
        eprintln!("==> creating KIND cluster '{cluster}'");
        let config_path = kind_config_path(root, cfg)?;
        run(Command::new("kind")
            .arg("create")
            .arg("cluster")
            .arg("--name")
            .arg(cluster)
            .arg("--kubeconfig")
            .arg(worktree_kubeconfig(root))
            .arg("--config")
            .arg(&config_path))?;
        created = Some(CreatedKindCluster {
            cluster: cluster.clone(),
        });
    }
    configure_worktree_kubeconfig(root, cfg)?;
    ensure_kube_proxy_nftables_if_requested()?;

    eprintln!("==> installing nginx-ingress");
    run(Command::new("kubectl")
        .arg("apply")
        .arg("-f")
        .arg(root.join(INGRESS_MANIFEST)))?;
    strip_ingress_controller_host_ports_if_requested()?;
    run(Command::new("kubectl")
        .arg("--namespace")
        .arg("ingress-nginx")
        .arg("wait")
        .arg("--for=condition=available")
        .arg("deployment/ingress-nginx-controller")
        // A cold KIND node can need several minutes just to pull the
        // controller image; on a loaded local Docker host its probes can take
        // longer still. Leave enough room for a healthy cold start. Wait on
        // the Deployment, not a pod selector: a rolling hostPort strip can
        // leave a terminating replica that makes `wait pod` exit NotFound.
        .arg("--timeout=1200s"))?;

    Ok(created)
}

fn kind_up_steps(root: &Path, cfg: &KindConfig) -> Result<()> {
    kind_platform_steps(root, cfg)?;
    eprintln!("==> installing Restate Operator (chart v{RESTATE_OPERATOR_VERSION})");
    run(Command::new("helm")
        .arg("upgrade")
        .arg("--install")
        .arg("restate-operator")
        .arg("oci://ghcr.io/restatedev/restate-operator-helm")
        .arg("--version")
        .arg(RESTATE_OPERATOR_VERSION)
        .arg("--namespace")
        .arg("restate-operator")
        .arg("--create-namespace"))?;
    // Helm installs a native `crds/` entry once and never upgrades it, so the
    // chart bump alone leaves an existing cluster on its original CRD schema.
    // Server-side apply because the chart already owns these fields under the
    // `helm` field manager (`--force-conflicts` takes ownership) and because
    // `restateclusters` serializes to ~236 KB — close enough to the 262 KB
    // annotation ceiling that a client-side apply's `last-applied-configuration`
    // is one upstream field away from breaking.
    eprintln!("==> applying Restate CRDs (v{RESTATE_OPERATOR_VERSION})");
    for crd in RESTATE_CRDS {
        run(Command::new("kubectl")
            .arg("apply")
            .arg("--server-side")
            .arg("--force-conflicts")
            .arg("-f")
            .arg(root.join(restate_crd_path(crd))))?;
    }
    run(Command::new("kubectl")
        .arg("--namespace")
        .arg("restate-operator")
        .arg("wait")
        .arg("--for=condition=available")
        .arg("--timeout=1200s")
        .arg("deployment")
        .arg("--all"))
}

/// Keep every Kubernetes subprocess for one local setup on a kubeconfig that
/// belongs to the target worktree. Unlike `kubectl config use-context`, this
/// never mutates the host-wide current context, so concurrent clones cannot
/// redirect one another's apply, `SeaweedFS`, or port-forward commands.
pub(super) fn configure_worktree_kubeconfig(root: &Path, cfg: &KindConfig) -> Result<()> {
    let kubeconfig = worktree_kubeconfig(root);
    fs::create_dir_all(root.join(".devx")).with_context(|| {
        format!(
            "create worktree state directory for {}",
            kubeconfig.display()
        )
    })?;
    if !kubeconfig.exists() {
        run(Command::new("kind")
            .arg("export")
            .arg("kubeconfig")
            .arg("--name")
            .arg(&cfg.cluster)
            .arg("--kubeconfig")
            .arg(&kubeconfig))?;
    }
    env::set_var("KUBECONFIG", &kubeconfig);
    use_kind_context(cfg)
}

fn worktree_kubeconfig(root: &Path) -> PathBuf {
    root.join(".devx/kubeconfig")
}

/// Create the application namespace before provisioning `SeaweedFS`'s credentials
/// secret. A reused cluster already has it, while a fresh worktree cluster has
/// not yet had its Kustomize overlay applied.
fn ensure_namespace(namespace: &str) -> Result<()> {
    let exists = Command::new("kubectl")
        .args(["get", "namespace", namespace])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("inspect Kubernetes namespace {namespace}"))?;
    if exists.success() {
        return Ok(());
    }
    eprintln!("==> creating namespace {namespace}");
    run(Command::new("kubectl").args(["create", "namespace", namespace]))
}

/// Load a local Docker image tag into every KIND node.
///
/// `kind load docker-image` hardcodes `ctr images import --all-platforms
/// --digests`, which means it tries to import *every* manifest the local
/// image index references. Since CI began publishing multi-arch indexes
/// (`linux/amd64` + `linux/arm64` + buildx `unknown/unknown` attestation
/// manifests), that breaks on any single-arch host: `docker pull` only
/// materializes the host platform's blobs, so `--all-platforms` aborts with
/// `ctr: content digest <other-arch manifest>: not found`. `OrbStack` and
/// Docker 29 use the containerd image store, which keeps the full index, so
/// this is the *default* failure on Apple Silicon — not an opt-in misconfig.
///
/// Flatten to the daemon's own platform with `docker save --platform` first,
/// then `kind load image-archive` the single-platform tar — `--all-platforms`
/// then finds exactly one platform and succeeds.
///
/// Only a *failed `docker save`* triggers the legacy fallback: a save that
/// can't produce `<platform>` means the image is older/single-arch, where
/// `kind load docker-image` still works (one platform, nothing to mismatch).
/// A failure of the `kind load image-archive` step is deliberately *not*
/// caught — KIND being unreachable or out of disk would fail the legacy load
/// the same way, and on a multi-arch image the fallback would re-trigger the
/// very `--all-platforms` digest bug this exists to avoid. That error
/// propagates directly instead of hiding behind a "flatten failed" message.
fn kind_load_image_into_cluster(tag: &str, cfg: &KindConfig, platform: &str) -> Result<()> {
    let archive = tempfile::Builder::new()
        .prefix("navigator-kind-load-")
        .suffix(".tar")
        .tempfile()
        .context("create temp image archive for kind load")?;
    eprintln!("==> docker save --platform {platform} {tag} (single-platform for kind)");
    let saved = run(Command::new("docker")
        .arg("save")
        .arg("--platform")
        .arg(platform)
        .arg(tag)
        .arg("-o")
        .arg(archive.path()));
    if let Err(err) = saved {
        eprintln!(
            "==> `docker save --platform {platform}` failed ({err:#}); \
             falling back to `kind load docker-image` (older single-arch image?)"
        );
        return run(Command::new("kind")
            .arg("load")
            .arg("docker-image")
            .arg(tag)
            .arg("--name")
            .arg(&cfg.cluster));
    }
    eprintln!("==> kind load image-archive ({tag} → {})", cfg.cluster);
    let loaded = run(Command::new("kind")
        .arg("load")
        .arg("image-archive")
        .arg(archive.path())
        .arg("--name")
        .arg(&cfg.cluster));
    if let Err(err) = loaded {
        // `image-archive` is preferred (it is what makes the platform flatten
        // above worth doing), but it is not universally accepted: on Docker
        // 29.7.2 + containerd image store, `ctr images import` rejects the
        // archive `docker save --platform` produces with `unrecognized image
        // format`, even though the archive is well-formed — one manifest, one
        // platform, `manifest.json` present.
        //
        // `docker load`-ing the same image through `kind load docker-image`
        // succeeds against the identical daemon and cluster, so the fallback is
        // not a lesser path here — it is the one that works. Measured: the
        // archive load fails and the docker-image load then lands the image in
        // the node's containerd (`crictl images` shows it) in the same run.
        //
        // Kept as a fallback rather than a replacement so the flattened-archive
        // path stays primary where it works, and so this stops being reached
        // the moment the upstream mismatch is fixed.
        eprintln!(
            "==> `kind load image-archive` failed ({err:#}); \
             falling back to `kind load docker-image`"
        );
        return run(Command::new("kind")
            .arg("load")
            .arg("docker-image")
            .arg(tag)
            .arg("--name")
            .arg(&cfg.cluster));
    }
    Ok(())
}

/// Architecture the Docker daemon runs as (`arm64` / `amd64`), used to build
/// the `linux/<arch>` platform passed to `docker save`. Queries the daemon
/// directly (`docker version`) so it is correct even when the CLI binary runs
/// under a different arch (Rosetta); falls back to the host arch if the daemon
/// can't be reached.
fn docker_daemon_arch() -> String {
    Command::new("docker")
        .args(["version", "--format", "{{.Server.Arch}}"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|arch| !arch.is_empty())
        .unwrap_or_else(|| normalize_docker_arch(std::env::consts::ARCH))
}

/// Resolve the immutable release tag the local cluster should
/// pull, in precedence order: an explicit `override_tag` (e.g.
/// `worktree-env --demo --tag`), then `NAVIGATOR_IMAGE_TAG`, then the
/// latest published tag from the registry. CI builds and publishes the
/// images (`deploy.yml`); the local loop pulls them.
fn local_image_tag(override_tag: Option<&str>) -> Result<Option<String>> {
    if let Some(tag) = override_tag
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .or_else(|| {
            env::var("NAVIGATOR_IMAGE_TAG")
                .ok()
                .filter(|v| !v.trim().is_empty())
        })
    {
        super::registry::validate_release_tag(&tag)?;
        return Ok(Some(tag));
    }
    Ok(None)
}

/// Pull a published Artifact Registry image, retag it to the local `:dev`
/// tag the KIND manifests reference, and load it into the cluster.
/// Retagging (rather than rewriting every manifest to a registry ref)
/// keeps the overlays byte-identical whether an image was built locally
/// or pulled. Published images are amd64-only (see
/// `deploy.yml`), matching prod's amd64 GKE Autopilot nodes, so `docker
/// pull` fetches the amd64 variant on any host; an Apple-Silicon laptop
/// loads it under emulation into its KIND node. The registry is private,
/// so the host's docker must be authorized
/// (`gcloud auth configure-docker <region>-docker.pkg.dev`).
fn pull_retag_load(
    registry: &str,
    image: &str,
    tag: &str,
    dev_tag: &str,
    cfg: &KindConfig,
) -> Result<()> {
    let remote = super::registry::image_ref(registry, image, tag);
    eprintln!("==> docker pull {remote}");
    run(Command::new("docker").arg("pull").arg(&remote))?;
    run(Command::new("docker").arg("tag").arg(&remote).arg(dev_tag))?;
    kind_load_image_into_cluster(dev_tag, cfg, &format!("linux/{}", docker_daemon_arch()))
}

/// Path to the `kind create cluster --config` file. At default host ports this
/// is the committed `k8s/kind-config.yaml` verbatim (so a standalone `kind
/// create` against it still works). When any mapped host port is overridden
/// or nested `DinD` needs nftables kube-proxy, render a copy under `.devx/`.
fn kind_config_path(root: &Path, cfg: &KindConfig) -> Result<PathBuf> {
    let committed = root.join("k8s/kind-config.yaml");
    let nested = kind_nested_dind_requested();
    if cfg.ingress_http_port == super::DEFAULT_INGRESS_HTTP_HOST_PORT
        && cfg.ingress_https_port == super::DEFAULT_INGRESS_HTTPS_HOST_PORT
        && cfg.rauthy_port == DEFAULT_RAUTHY_HOST_PORT
        && !nested
    {
        return Ok(committed);
    }
    let template =
        fs::read_to_string(&committed).with_context(|| format!("read {}", committed.display()))?;
    let mut rendered = render_kind_config(&template, cfg);
    if nested {
        rendered = super::inject_kube_proxy_nftables(&rendered);
    }
    let dir = root.join(".devx");
    fs::create_dir_all(&dir).with_context(|| format!("create state dir {}", dir.display()))?;
    let path = dir.join("kind-config.yaml");
    fs::write(&path, rendered).with_context(|| format!("write {}", path.display()))?;
    eprintln!(
        "==> rendered kind-config with ingress={}/{} rauthy={} host ports → {}",
        cfg.ingress_http_port,
        cfg.ingress_https_port,
        cfg.rauthy_port,
        path.display()
    );
    Ok(path)
}

fn apply_kustomize(root: &Path, overlay: &str) -> Result<()> {
    eprintln!("==> kubectl apply -k {overlay}");
    run(Command::new("kubectl")
        .arg("apply")
        .arg("-k")
        .arg(root.join(overlay)))
}

pub(super) fn wait_for_condition(namespace: &str, resource: &str, condition: &str) -> Result<()> {
    if let Some(state) = terminating_state(namespace, resource) {
        bail!("{}", terminating_wedge_message(namespace, resource, &state));
    }
    run(Command::new("kubectl")
        .arg("--namespace")
        .arg(namespace)
        .arg("wait")
        .arg(format!("--for=condition={condition}"))
        .arg(resource)
        .arg("--timeout=300s"))
}

/// Probe whether `resource` is mid-deletion, so [`wait_for_condition`] can
/// fail fast with a real diagnosis instead of blocking for the full timeout.
///
/// Best-effort by construction: a probe that cannot answer (`NotFound`, no
/// cluster, no kubectl) yields `None` and lets the `wait` produce the
/// authoritative error. This only ever *adds* a diagnosis; it never invents a
/// failure of its own.
fn terminating_state(namespace: &str, resource: &str) -> Option<Terminating> {
    let out = Command::new("kubectl")
        .arg("--namespace")
        .arg(namespace)
        .arg("get")
        .arg(resource)
        .arg("-o")
        .arg("jsonpath={.metadata.deletionTimestamp}|{.metadata.finalizers}")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_terminating(&String::from_utf8_lossy(&out.stdout))
}

/// Wait for a pod matching `selector` in `namespace` to reach `Ready`.
///
/// `reload_worker` deletes the running worker pod and lets the
/// `RestateDeployment`'s `ReplicaSet` recreate it. A bare
/// `kubectl wait --for=condition=Ready pod` errors with "no matching resources
/// found" if it runs before the `ReplicaSet` has created the replacement, so
/// first poll until a pod exists, then block on its readiness. This observes
/// the actual replacement pod rather than the `RestateDeployment` CR's lagging
/// `Ready` condition.
pub(super) fn wait_for_selected_pod_ready(namespace: &str, selector: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_mins(5);
    loop {
        let out = Command::new("kubectl")
            .arg("--namespace")
            .arg(namespace)
            .arg("get")
            .arg("pod")
            .arg("--selector")
            .arg(selector)
            .arg("--output")
            .arg("jsonpath={.items[*].metadata.name}")
            .output()
            .with_context(|| format!("list pods matching {selector} in {namespace}"))?;
        if out.status.success() && !String::from_utf8_lossy(&out.stdout).trim().is_empty() {
            break;
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for a pod matching '{selector}' to appear in {namespace}");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    run(Command::new("kubectl")
        .arg("--namespace")
        .arg(namespace)
        .arg("wait")
        .arg("--for=condition=Ready")
        .arg("pod")
        .arg("--selector")
        .arg(selector)
        .arg("--timeout=300s"))
}

/// `dev down [--path <checkout>]`: the inverse of [`up`] for the same root.
pub(super) fn down(path: Option<&Path>, cfg: &KindConfig) -> Result<()> {
    let root = checkout_root(path)?;
    if super::native_profile::Intent::read(&root)?.is_some()
        || root.join(".devx/worktree.json").is_file()
    {
        super::worktree_env::down(&root, cfg)
    } else {
        down_in(&root, cfg)
    }
}

/// Physical identity check for explicit native cleanup, including a partially
/// initialized database. A changed cluster namespace is not an owned target.
pub(super) fn verify_native_teardown(root: &Path, cfg: &KindConfig) -> Result<()> {
    let intent = super::native_profile::Intent::read(root)?.context("native intent is missing")?;
    intent.verify_target(root, cfg)?;
    if cluster_exists(&cfg.cluster)? {
        verify_fixture_port(cfg)?;
        configure_worktree_kubeconfig(root, cfg)?;
        let kube = super::forgejo::cluster::Kube::new(cfg);
        let namespace: serde_json::Value = serde_json::from_str(&kube.output(&[
            "get",
            "namespace",
            &cfg.namespace,
            "-o",
            "json",
        ])?)?;
        let uid = namespace["metadata"]["uid"]
            .as_str()
            .context("native namespace UID is missing")?
            .parse()?;
        intent.binding(uid)?;
    }
    Ok(())
}

/// Tear down a KIND dependency tier whose port-forward state belongs to
/// `root`, which can differ from the caller’s current directory.
pub(super) fn down_in(root: &Path, cfg: &KindConfig) -> Result<()> {
    let state = StateDir::new(root)?;
    eprintln!("==> killing port-forwards");
    state.kill_pids();
    let cluster = &cfg.cluster;
    if cluster_exists(cluster)? {
        eprintln!("==> deleting KIND cluster '{cluster}'");
        run(Command::new("kind")
            .arg("delete")
            .arg("cluster")
            .arg("--name")
            .arg(cluster))?;
    }
    state.clear();
    let postgres_state = root.join(".devx/postgres");
    if postgres_state.exists() {
        fs::remove_dir_all(postgres_state)
            .context("remove the deleted KIND fixture's PostgreSQL state")?;
    }
    Ok(())
}

pub(super) fn print_env(cfg: &KindConfig) -> Result<()> {
    print!("{}", render_env(cfg, &workspace_root()?));
    Ok(())
}

pub(super) fn status(cfg: &KindConfig) {
    let cluster = cluster_exists(&cfg.cluster).unwrap_or(false);
    println!("KIND cluster '{}': {}", cfg.cluster, yes_no(cluster));
    if let Ok(root) = workspace_root() {
        if let Ok(state) = StateDir::new(&root) {
            let pids = state.read_pids().unwrap_or_default();
            println!("Port-forward PIDs ({}): {pids:?}", pids.len());
            for &port in &[
                cfg.restate_ingress_port,
                cfg.restate_admin_port,
                cfg.clamav_port,
                cfg.rauthy_port,
                cfg.seaweedfs_s3_port,
                cfg.surreal_port,
            ] {
                let listening = std::net::TcpStream::connect_timeout(
                    &format!("127.0.0.1:{port}").parse().unwrap(),
                    Duration::from_millis(200),
                )
                .is_ok();
                println!("  127.0.0.1:{port}: {}", yes_no(listening));
            }
        }
    }
}

fn yes_no(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

pub(super) fn wait_rollout(kind: &str, name: &str, cfg: &KindConfig) -> Result<()> {
    run(Command::new("kubectl")
        .arg("--namespace")
        .arg(&cfg.namespace)
        .arg("rollout")
        .arg("status")
        .arg(format!("{kind}/{name}"))
        .arg("--timeout=300s"))
}

fn port_forward(
    target: &str,
    host_port: u16,
    svc_port: u16,
    cfg: &KindConfig,
    state: &StateDir,
) -> Result<u32> {
    spawn_port_forward(
        &[
            "--context",
            &cfg.kind_context(),
            "--namespace",
            &cfg.namespace,
            "port-forward",
            target,
            &format!("{host_port}:{svc_port}"),
        ],
        state,
    )
}

fn spawn_port_forward(args: &[&str], state: &StateDir) -> Result<u32> {
    let log_offset = state.log_size();
    let log = state.open_log()?;
    let log_err = log.try_clone()?;
    let mut cmd = Command::new("kubectl");
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    detach(&mut cmd);
    let child: Child = cmd
        .spawn()
        .with_context(|| format!("spawn kubectl {args:?}"))?;
    let pid = child.id();
    // Detach: don't wait on it. `Child::drop` does not kill the
    // process; the OS adopts it via the new process group.
    std::mem::forget(child);

    // Give kubectl a moment to either bind or fail, then scan the log
    // it appended. Without this check, a `bind: address already in
    // use` error is hidden by anything else that happens to be
    // listening on that port.
    std::thread::sleep(Duration::from_millis(800));
    if let Some(err) = state.log_tail_error(log_offset)? {
        StateDir::kill(&[pid]);
        bail!("kubectl port-forward {args:?} failed: {err}");
    }
    Ok(pid)
}

#[cfg(unix)]
fn detach(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

#[cfg(not(unix))]
fn detach(_cmd: &mut Command) {}

pub(super) fn wait_for_tcp(host: &str, port: u16) -> Result<()> {
    let addr = format!("{host}:{port}");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(stream) = std::net::TcpStream::connect_timeout(
            &addr
                .parse()
                .with_context(|| format!("parse socket addr {addr}"))?,
            Duration::from_millis(500),
        ) {
            drop(stream);
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for {addr} to accept connections");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn cluster_exists(name: &str) -> Result<bool> {
    let out = Command::new("kind")
        .arg("get")
        .arg("clusters")
        .output()
        .context("run `kind get clusters`")?;
    if !out.status.success() {
        bail!(
            "kind get clusters failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|line| line.trim() == name))
}

pub(super) fn require_tools(tools: &[&str]) -> Result<()> {
    for tool in tools {
        let ok = Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {tool}"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            bail!("required tool not on PATH: {tool}");
        }
    }
    Ok(())
}

/// Verify that auth-bearing CLIs are not just present but **authenticated**.
///
/// A present-but-unauthenticated `gcloud` / `restate` doesn't
/// fail until deep in a flow — mid-push to Artifact Registry, mid-Restate
/// re-register — where the error is cryptic and a half-finished ship is
/// already on the cluster. This runs the cheapest read-only probe per CLI
/// up front so the whole flow aborts in one clear line before it touches
/// anything. Each probe's output is discarded; only its exit status counts.
pub(super) fn require_auth(tools: &[&str]) -> Result<()> {
    for &tool in tools {
        // (probe command, what to run to fix it)
        let (probe, hint) = match tool {
            // Prints an access token iff a credential is active; non-zero
            // when logged out or no ADC / service account is available.
            "gcloud" => (
                "gcloud auth print-access-token",
                "run `gcloud auth login` (or activate a service account)",
            ),
            // `whoami` succeeds only when an environment is configured. This
            // catches "never logged in"; a stale Cloud token can still slip
            // through, so the register step remains the real proof.
            "restate" => (
                "restate whoami",
                "run `restate cloud login` then `restate cloud environments configure <env>`",
            ),
            // The daemon must be reachable to build/push images.
            "docker" => ("docker info", "start the Docker daemon"),
            other => bail!("require_auth: no auth probe defined for `{other}`"),
        };
        eprintln!("==> auth check: {tool}");
        let ok = Command::new("sh")
            .arg("-c")
            .arg(probe)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            bail!("`{tool}` is present but not authenticated — {hint}");
        }
    }
    Ok(())
}

pub(super) fn run(cmd: &mut Command) -> Result<()> {
    let program = Path::new(cmd.get_program()).display().to_string();
    let status = cmd.status().with_context(|| format!("spawn {program}"))?;
    if !status.success() {
        bail!("command failed ({status}): {program}");
    }
    Ok(())
}

/// The checkout a `--path` names, or the workspace containing the current
/// directory when none is given. A named path must itself be a checkout, so a
/// mistyped directory is refused before any tool or credential is consulted.
fn checkout_root(path: Option<&Path>) -> Result<PathBuf> {
    let Some(path) = path else {
        return workspace_root();
    };
    let root = path
        .canonicalize()
        .with_context(|| format!("resolve checkout path {}", path.display()))?;
    if root.join("Cargo.toml").is_file() && root.join("k8s").is_dir() {
        Ok(root)
    } else {
        bail!(
            "{} is not a Navigator checkout (it needs Cargo.toml and k8s/ at its root)",
            root.display()
        )
    }
}

pub(super) fn workspace_root() -> Result<PathBuf> {
    let mut dir = env::current_dir().context("get current directory")?;
    loop {
        if dir.join("Cargo.toml").is_file() && dir.join("k8s").is_dir() {
            return Ok(dir);
        }
        match dir.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => bail!("could not find workspace root containing Cargo.toml and k8s/"),
        }
    }
}

// ---------- state directory (.devx/) ----------

struct StateDir {
    dir: PathBuf,
}

impl StateDir {
    fn new(root: &Path) -> Result<Self> {
        let dir = root.join(".devx");
        fs::create_dir_all(&dir).with_context(|| format!("create state dir {}", dir.display()))?;
        Ok(Self { dir })
    }

    fn pids_path(&self) -> PathBuf {
        self.dir.join("pids")
    }

    fn env_path(&self) -> PathBuf {
        self.dir.join("env")
    }

    fn log_path(&self) -> PathBuf {
        self.dir.join("port-forwards.log")
    }

    fn open_log(&self) -> Result<fs::File> {
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_path())
            .with_context(|| format!("open {}", self.log_path().display()))
    }

    fn log_size(&self) -> u64 {
        fs::metadata(self.log_path()).map_or(0, |m| m.len())
    }

    /// Return the first error-shaped line appended to the log past
    /// `since`, or None if nothing notable showed up. "Error-shaped"
    /// means kubectl's `error:` or `Unable to listen` prefixes — the
    /// happy path emits `Forwarding from …`.
    fn log_tail_error(&self, since: u64) -> Result<Option<String>> {
        let path = self.log_path();
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        let tail = bytes
            .get(usize::try_from(since).unwrap_or(usize::MAX)..)
            .unwrap_or(&[]);
        let text = String::from_utf8_lossy(tail);
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("error:") || trimmed.starts_with("Unable to listen") {
                return Ok(Some(trimmed.to_string()));
            }
        }
        Ok(None)
    }

    fn write_pids(&self, pids: &[u32]) -> Result<()> {
        let mut f = fs::File::create(self.pids_path())
            .with_context(|| format!("write {}", self.pids_path().display()))?;
        for pid in pids {
            writeln!(f, "{pid}")?;
        }
        Ok(())
    }

    fn read_pids(&self) -> Result<Vec<u32>> {
        let path = self.pids_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let f = fs::File::open(&path).with_context(|| format!("read {}", path.display()))?;
        let mut out = Vec::new();
        for line in BufReader::new(f).lines() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            out.push(
                trimmed
                    .parse::<u32>()
                    .map_err(|e| anyhow!("malformed PID '{trimmed}': {e}"))?,
            );
        }
        Ok(out)
    }

    fn kill_pids(&self) {
        Self::kill(&self.read_pids().unwrap_or_default());
        let _ = fs::remove_file(self.pids_path());
    }

    fn kill(pids: &[u32]) {
        for pid in pids {
            // Best-effort: process may already have exited.
            let _ = Command::new("kill")
                .arg("-TERM")
                .arg(pid.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }

    fn write_env(&self, body: &str) -> Result<()> {
        write_fixture_private(&self.env_path(), body.as_bytes())
    }

    fn clear(&self) {
        let _ = fs::remove_file(self.pids_path());
        let _ = fs::remove_file(self.env_path());
    }
}

/// Prepare the one synthetic PostgreSQL store shared by KIND and host fixtures.
pub(super) fn postgres_bootstrap(path: Option<&Path>, cfg: &KindConfig) -> Result<()> {
    let root = checkout_root(path)?;
    let cfg = super::worktree_env::postgres_fixture_config(&root, cfg)?;
    configure_worktree_kubeconfig(&root, &cfg)?;
    bootstrap_postgres_in(&root, &cfg, "navigator")
}

pub(in crate::devx) fn verify_fixture_port(cfg: &KindConfig) -> Result<serde_json::Value> {
    let published = Command::new("docker")
        .args(["inspect", "--format", "{{json .}}"])
        .arg(format!("{}-control-plane", cfg.cluster))
        .output()?;
    anyhow::ensure!(
        published.status.success(),
        "inspect the owned KIND node's published ports"
    );
    let container: serde_json::Value = serde_json::from_slice(&published.stdout)?;
    super::postgres_fixture::validate_published_port(
        &container["NetworkSettings"]["Ports"],
        cfg.postgres_port,
    )?;
    Ok(container)
}

fn fixture_kube(
    root: &Path,
    cfg: &KindConfig,
    database: &str,
) -> Result<(super::forgejo::cluster::Kube, serde_json::Value)> {
    use super::forgejo::cluster::Kube;
    require_tools(&["kind", "kubectl", "docker", "openssl"])?;
    super::worktree_env::verify_postgres_fixture_target(root, cfg)?;
    let clusters = Command::new("kind").args(["get", "clusters"]).output()?;
    anyhow::ensure!(
        clusters.status.success()
            && String::from_utf8_lossy(&clusters.stdout)
                .lines()
                .any(|name| name == cfg.cluster),
        "PostgreSQL fixture requires this task's existing KIND cluster"
    );
    let container = verify_fixture_port(cfg)?;
    let kube = Kube::new(cfg);
    let read = |kind: &str| -> Result<serde_json::Value> {
        Ok(serde_json::from_str(
            &kube.output(&["get", kind, "-o", "json"])?,
        )?)
    };
    super::postgres_fixture::validate_store_identity(
        &read("configmap/navigator-surreal")?,
        &read("service/surreal")?,
        &read("deployment/surreal")?,
        &cfg.namespace,
        database,
        "surreal",
        super::surreal::SERVICE_PORT,
    )?;
    if kube.succeeds(&["get", "service/navigator-postgres"]) {
        super::postgres_fixture::validate_local_service(
            &read("service/navigator-postgres")?,
            "navigator-postgres",
        )?;
    }
    Ok((kube, container))
}

pub(in crate::devx) fn apply_postgres_host_ingress(
    cfg: &KindConfig,
    kube: &super::forgejo::cluster::Kube,
    container: &serde_json::Value,
) -> Result<()> {
    let node: serde_json::Value = serde_json::from_str(&kube.output(&[
        "get",
        "node",
        &format!("{}-control-plane", cfg.cluster),
        "-o",
        "json",
    ])?)?;
    let ingress = super::postgres_fixture::host_ingress(cfg, &node, container)?;
    kube.apply(&ingress.to_string())?;
    Ok(())
}

fn bootstrap_postgres_in(root: &Path, cfg: &KindConfig, database: &str) -> Result<()> {
    use super::forgejo::cluster::prepare_postgres;
    let (kube, container) = fixture_kube(root, cfg, database)?;
    let held = hold_fixture_writers(root, cfg, &kube)?;
    apply_postgres_host_ingress(cfg, &kube, &container)?;
    let state = StateDir::new(root)?;
    state.kill_pids();
    state.write_pids(&[])?;
    let credentials = prepare_postgres(cfg)?;
    let directory = root.join(".devx/postgres");
    fs::create_dir_all(&directory)?;
    write_fixture_private(
        &super::postgres_fixture::ca_path(root),
        credentials.ca.as_bytes(),
    )?;
    kube.run(&[
        "delete",
        "job",
        "navigator-postgres-roles",
        "--ignore-not-found",
        "--wait=true",
    ])?;
    let rendered = super::forgejo::cluster::kustomize(root, "k8s/overlays/kind/postgres")?;
    kube.apply(&rendered)?;
    kube.wait_rollout("statefulset/navigator-postgres", "300s")?;
    kube.wait_job("navigator-postgres-roles")?;
    kube.wait_rollout("deployment/surreal", "300s")?;
    kube.wait_rollout("statefulset/seaweedfs", "300s")?;
    let mut all_pids = state.read_pids()?;
    for (service, local, remote) in [
        (
            "service/surreal",
            cfg.surreal_port,
            super::surreal::SERVICE_PORT,
        ),
        ("service/navigator-seaweedfs", cfg.seaweedfs_s3_port, 8333),
    ] {
        all_pids.push(port_forward(service, local, remote, cfg, &state)?);
        state.write_pids(&all_pids)?;
    }
    wait_for_tcp("127.0.0.1", cfg.postgres_port)?;
    wait_for_tcp("127.0.0.1", cfg.surreal_port)?;
    wait_for_tcp("127.0.0.1", cfg.seaweedfs_s3_port)?;
    hydrate_seaweedfs_environment(cfg)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(enroll_fixture(root, cfg, database, &credentials))?;
    StateDir::kill(&all_pids[all_pids.len() - 2..]);
    all_pids.truncate(all_pids.len() - 2);
    state.write_pids(&all_pids)?;
    let environment = credentials.environment(cfg, root);
    write_fixture_private(
        &super::postgres_fixture::environment_path(root),
        super::postgres_fixture::render_environment(&environment).as_bytes(),
    )?;
    kube.apply(
        &serde_json::json!({
            "apiVersion":"v1", "kind":"ConfigMap",
            "metadata":{"name":"navigator-kind-postgres-ready","namespace":cfg.namespace},
            "data":{"ready":"true"}
        })
        .to_string(),
    )?;
    for (resource, replicas) in held {
        kube.run(&["scale", &resource, &format!("--replicas={replicas}")])?;
    }
    fs::remove_file(directory.join("held-writers.json"))?;
    Ok(())
}

pub(super) fn write_fixture_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    Ok(())
}

fn hold_fixture_writers(
    root: &Path,
    cfg: &KindConfig,
    kube: &super::forgejo::cluster::Kube,
) -> Result<std::collections::BTreeMap<String, u32>> {
    let directory = root.join(".devx/postgres");
    fs::create_dir_all(&directory)?;
    let path = directory.join("held-writers.json");
    let namespace: serde_json::Value =
        serde_json::from_str(&kube.output(&["get", "namespace", &cfg.namespace, "-o", "json"])?)?;
    let namespace_uid = namespace["metadata"]["uid"]
        .as_str()
        .context("KIND namespace has no identity")?;
    let mut snapshot = if path.exists() {
        serde_json::from_slice::<super::postgres_fixture::HeldWriters>(&fs::read(&path)?)?
    } else {
        super::postgres_fixture::HeldWriters::new(
            cfg,
            namespace_uid,
            std::collections::BTreeMap::new(),
        )
    };
    snapshot.validate(cfg, namespace_uid)?;
    let held = &mut snapshot.replicas;
    for resource in [
        "deployment/navigator-web",
        "restatedeployment/workflows-service",
    ] {
        if !kube.succeeds(&["get", resource]) {
            continue;
        }
        let value: serde_json::Value =
            serde_json::from_str(&kube.output(&["get", resource, "-o", "json"])?)?;
        let replicas = value["spec"]["replicas"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(1);
        held.entry(resource.into()).or_insert(replicas);
    }
    write_fixture_private(&path, &serde_json::to_vec(&snapshot)?)?;
    let held = snapshot.replicas;
    for resource in held.keys() {
        kube.run(&["scale", resource, "--replicas=0"])?;
    }
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let pods: serde_json::Value = serde_json::from_str(&kube.output(&[
            "get",
            "pods",
            "-l",
            "app in (navigator-web,workflows-service)",
            "-o",
            "json",
        ])?)?;
        let mut observed = true;
        for resource in held.keys() {
            let state: serde_json::Value =
                serde_json::from_str(&kube.output(&["get", resource, "-o", "json"])?)?;
            observed &= if state["kind"] == "RestateDeployment" {
                let workloads = serde_json::from_str(&kube.output(&[
                    "get",
                    "deployments,replicasets",
                    "-o",
                    "json",
                ])?)?;
                let all_pods = serde_json::from_str(&kube.output(&["get", "pods", "-o", "json"])?)?;
                super::postgres_fixture::restate_writer_observed_stopped(
                    &state, &workloads, &all_pods,
                )
            } else {
                super::postgres_fixture::writer_observed_stopped(&state)
            };
        }
        if observed && pods["items"].as_array().is_some_and(Vec::is_empty) {
            break;
        }
        if Instant::now() >= deadline {
            bail!("KIND application writers did not stop; capture refused");
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(held)
}

/// Census the tables present on the frozen source before the archive reader runs.
async fn fixture_source_counts(
    source: &store::surreal::SurrealDb,
) -> Result<std::collections::BTreeMap<String, i64>> {
    let mut response = source.query("RETURN (INFO FOR DB).tables").await?;
    let tables: Option<std::collections::BTreeMap<String, String>> = response.take(0)?;
    let tables = tables.context("synthetic source omitted its live table inventory")?;
    let mut counts = std::collections::BTreeMap::new();
    for table in tables.keys() {
        let mut response = source
            .query("SELECT VALUE count() FROM type::table($table) GROUP ALL")
            .bind(("table", table.clone()))
            .await?;
        let rows: Vec<i64> = response.take(0)?;
        counts.insert(table.clone(), rows.first().copied().unwrap_or_default());
    }
    Ok(counts)
}

async fn enroll_fixture(
    root: &Path,
    cfg: &KindConfig,
    database: &str,
    credentials: &super::postgres_fixture::Credentials,
) -> Result<()> {
    use store::postgres_backfill::{self, family};
    let pg = store::postgres::PostgresDb::connect(credentials.options(cfg, root)).await?;
    pg.migrate_embedded(None)
        .await
        .context("migrate the synthetic PostgreSQL store")?;
    store::clauses::seed(
        &pg,
        &rules::clauses::library_clauses().map_err(anyhow::Error::msg)?,
    )
    .await
    .context("seed the canonical PostgreSQL Clause catalog")?;
    for item in
        dotenvy::from_read_iter(super::render_env_for(cfg, database, cfg.web_port, root).as_bytes())
    {
        let (key, value) = item?;
        env::set_var(key, value);
    }
    for (key, value) in credentials.environment(cfg, root) {
        env::set_var(key, value);
    }
    let ready = family::require_runtime_scopes(&pg).await;
    if ready.is_err() {
        // An invalid/pending present origin is not an invitation to capture Source.
        family::require_capture_target(&pg).await?;
        anyhow::ensure!(
            std::net::TcpStream::connect(("127.0.0.1", cfg.web_port)).is_err(),
            "stop the task host web before capturing PostgreSQL fixture history"
        );
        let enrolled: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM mirror_backfill_run WHERE scope <> 'global')",
        )
        .fetch_one(pg.pool())
        .await?;
        anyhow::ensure!(
            !enrolled,
            "existing family history needs explicit operator recovery; fixture bootstrap will not replace it"
        );
        let config = super::surreal::host_config(cfg, database);
        let source = crate::surreal_archive::connect_http(&config).await?;
        let counts = fixture_source_counts(&source).await?;
        super::postgres_fixture::require_fresh_source(&counts)?;
        let (bytes, receipt) = crate::surreal_archive::capture_quiesced(&source).await?;
        super::postgres_fixture::require_fresh_source(&receipt.table_counts)?;
        write_fixture_private(&root.join(".devx/postgres/capture.surql"), &bytes)?;
        write_fixture_private(
            &root.join(".devx/postgres/capture.receipt.json"),
            &serde_json::to_vec(&receipt)?,
        )?;
        let global = postgres_backfill::start(&source, &pg, 500, None).await?;
        anyhow::ensure!(
            global.is_clean(),
            "synthetic global import did not verify cleanly"
        );
        for family in family::Family::all() {
            let proof = receipt.clone().verify_export(&bytes)?;
            let report =
                postgres_backfill::start_family(&source, &pg, *family, proof, 500, None, true)
                    .await?;
            anyhow::ensure!(
                report.is_clean(),
                "synthetic scoped import did not verify cleanly"
            );
        }
    }
    family::require_runtime_scopes(&pg).await?;
    // Only after real activation may ordinary accessors seed the sample portfolio.
    let source = store::surreal::SurrealDb::uninitialized();
    let storage = cloud::from_env().await?;
    store::seed::seed_environment(
        &source,
        &storage,
        store::DeploymentEnvironment::Dev,
        store::seed::BrandSeed::Neon,
    )
    .await?;
    Ok(())
}

//! Fresh PostgreSQL/DBOS KIND tier identity and its explicit local environment.
//! Intent files reserve an operation; only actual database-origin verification
//! admits a resumed tier. A local ready bit is never authority.
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, ensure, Result};
use serde::{Deserialize, Serialize};
use store::postgres::native_origin::{self, NativeDatabase, TierBinding};
use uuid::Uuid;

use super::KindConfig;

pub(super) const OVERLAY: &str = "k8s/overlays/kind-postgres-dbos-deps";
const STATE: &str = ".devx/postgres/native-origin.json";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Intent {
    version: u8,
    worktree: PathBuf,
    cluster: String,
    tier: Uuid,
    pub creation: Uuid,
    namespace: Option<Uuid>,
    ready: bool,
    token: String,
    pub upgrade: Option<Uuid>,
}

impl Intent {
    pub(super) fn prepare(root: &Path, cfg: &KindConfig) -> Result<Self> {
        if let Some(intent) = Self::read(root)? {
            intent.verify_target(root, cfg)?;
            return Ok(intent);
        }
        let intent = Self {
            version: 1,
            worktree: root.canonicalize()?,
            cluster: cfg.cluster.clone(),
            tier: Uuid::new_v4(),
            creation: Uuid::new_v4(),
            namespace: None,
            ready: false,
            token: super::seaweedfs::random_hex(),
            upgrade: None,
        };
        intent.write(root)?;
        Ok(intent)
    }

    pub(super) fn read(root: &Path) -> Result<Option<Self>> {
        let path = root.join(STATE);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(anyhow!("native tier intent is unavailable")),
        };
        ensure!(
            metadata.is_file()
                && metadata.permissions().mode().trailing_zeros() >= 6
                && metadata.len() <= 32768,
            "native tier intent must be a bounded private regular file"
        );
        let bytes = fs::read(path).map_err(|_| anyhow!("native tier intent is unavailable"))?;
        let intent: Self =
            serde_json::from_slice(&bytes).map_err(|_| anyhow!("native tier intent is invalid"))?;
        ensure!(
            intent.version == 1
                && !intent.tier.is_nil()
                && !intent.creation.is_nil()
                && intent.namespace.is_none_or(|id| !id.is_nil())
                && intent.upgrade.is_none_or(|id| !id.is_nil())
                && intent.token.len() == 64
                && intent.token.bytes().all(|b| b.is_ascii_hexdigit()),
            "native tier intent is invalid"
        );
        Ok(Some(intent))
    }

    fn write(&self, root: &Path) -> Result<()> {
        let path = root.join(STATE);
        let directory = path
            .parent()
            .ok_or_else(|| anyhow!("native intent directory is missing"))?;
        fs::create_dir_all(directory)?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
        serde_json::to_writer(file.as_file_mut(), self)
            .map_err(|_| anyhow!("could not encode native tier intent"))?;
        file.as_file_mut().flush()?;
        file.as_file().sync_all()?;
        file.persist(&path)
            .map_err(|_| anyhow!("could not persist native tier intent"))?;
        fs::File::open(directory)?.sync_all()?;
        Ok(())
    }

    pub(super) fn verify_target(&self, root: &Path, cfg: &KindConfig) -> Result<()> {
        ensure!(
            self.worktree == root.canonicalize()? && self.cluster == cfg.cluster,
            "native tier belongs to another worktree or cluster"
        );
        Ok(())
    }

    pub(super) fn binding(&self, actual_namespace: Uuid) -> Result<TierBinding> {
        ensure!(
            self.namespace == Some(actual_namespace),
            "native tier namespace changed; existing data cannot be adopted"
        );
        Ok(TierBinding {
            tier_id: self.tier,
            namespace_uid: actual_namespace,
            cluster_name: self.cluster.clone(),
        })
    }

    // Called only after the machine layer holds actual newly-created KIND proof.
    pub(super) fn bind_created_namespace(
        &mut self,
        root: &Path,
        cfg: &KindConfig,
        created: &super::orchestrate::CreatedKindCluster,
        namespace: Uuid,
    ) -> Result<()> {
        self.verify_target(root, cfg)?;
        ensure!(
            created.matches(cfg),
            "creation capability belongs to another KIND tier"
        );
        ensure!(
            !namespace.is_nil() && self.namespace.is_none() && !self.ready,
            "native tier namespace cannot be replaced"
        );
        self.namespace = Some(namespace);
        self.write(root)
    }

    pub(super) fn mark_ready(&mut self, root: &Path, native: &NativeDatabase) -> Result<()> {
        ensure!(
            native.operation() == self.creation
                && native.tier() == &self.binding(native.tier().namespace_uid)?,
            "native origin does not match the reserved creation"
        );
        self.ready = true;
        self.upgrade = None;
        self.write(root)
    }

    pub(super) fn prepare_upgrade(&mut self, root: &Path) -> Result<Uuid> {
        let operation = *self.upgrade.get_or_insert_with(Uuid::new_v4);
        self.write(root)?;
        Ok(operation)
    }

    pub(super) fn database(&self) -> Result<String> {
        Ok(native_origin::database_name(self.creation)?)
    }
    pub(super) const fn was_ready(&self) -> bool {
        self.ready
    }
}

pub(super) fn dbos_port(cfg: &KindConfig) -> Result<u16> {
    cfg.web_port
        .checked_add(700)
        .ok_or_else(|| anyhow!("native DBOS listener exceeds the local port range"))
}

fn timer_coordinates(get: &impl Fn(&str) -> Option<String>) -> Result<BTreeMap<String, String>> {
    let mut values = BTreeMap::new();
    for key in [
        "NAVIGATOR_DBOS_BILLING_CANARY_SCHEDULE_ENABLED",
        "NAVIGATOR_DBOS_RECONCILE_INVOICES_SCHEDULE_ENABLED",
    ] {
        let value = get(key).unwrap_or_else(|| "false".into());
        ensure!(
            matches!(value.trim(), "true" | "false"),
            "{key} must select an explicit timer state"
        );
        values.insert(key.into(), value.trim().into());
    }
    Ok(values)
}

#[derive(Clone, Copy)]
pub(super) struct RolePasswords<'a> {
    pub dbos: &'a str,
    pub archive: &'a str,
}

pub(super) fn environment(
    root: &Path,
    cfg: &KindConfig,
    intent: &Intent,
    app: BTreeMap<String, String>,
    passwords: RolePasswords<'_>,
    storage: &super::seaweedfs::Credentials,
    get: impl Fn(&str) -> Option<String>,
) -> Result<BTreeMap<String, String>> {
    intent.verify_target(root, cfg)?;
    let database = intent.database()?;
    ensure!(
        app.get("NAVIGATOR_POSTGRES_DATABASE") == Some(&database),
        "application credentials target another native database"
    );
    let base = super::render_env_for(cfg, &database, cfg.web_port, root);
    let mut values = dotenvy::from_read_iter(base.as_bytes())
        .collect::<Result<BTreeMap<String, String>, _>>()?;
    values.retain(|key, _| {
        !key.starts_with("NAVIGATOR_SURREAL_")
            && !key.starts_with("RESTATE_")
            && !matches!(
                key.as_str(),
                "NAVIGATOR_KIND_SURREAL_PORT"
                    | "NAVIGATOR_KIND_RESTATE_INGRESS_PORT"
                    | "NAVIGATOR_KIND_RESTATE_ADMIN_PORT"
            )
    });
    values.extend(app);
    values.extend(timer_coordinates(&get)?);
    values.insert("NAVIGATOR_KIND_DEPS_OVERLAY".into(), OVERLAY.into());
    values.insert("NAVIGATOR_DBOS_HOST".into(), "127.0.0.1".into());
    values.insert("NAVIGATOR_DBOS_PORT".into(), cfg.postgres_port.to_string());
    values.insert("NAVIGATOR_DBOS_DATABASE".into(), intent.database()?);
    values.insert("NAVIGATOR_DBOS_USER".into(), "navigator_dbos".into());
    values.insert("NAVIGATOR_DBOS_PASSWORD".into(), passwords.dbos.into());
    values.insert("NAVIGATOR_DBOS_SSLMODE".into(), "verify-full".into());
    values.insert(
        "NAVIGATOR_DBOS_ROOT_CERT".into(),
        super::postgres_fixture::ca_path(root).display().to_string(),
    );
    values.insert(
        "NAVIGATOR_DBOS_EXECUTOR_ID".into(),
        format!("native-tier-{}", intent.tier),
    );
    values.insert(
        "NAVIGATOR_RELEASE_TAG".into(),
        env!("CARGO_PKG_VERSION").into(),
    );
    values.insert("NAVIGATOR_DBOS_INGRESS_TOKEN".into(), intent.token.clone());
    values.insert(
        "NAVIGATOR_DBOS_INGRESS_URL".into(),
        format!("http://127.0.0.1:{}", dbos_port(cfg)?),
    );
    values.insert(
        "WORKFLOWS_SERVICE_HEALTH_LISTEN".into(),
        format!("127.0.0.1:{}", dbos_port(cfg)?),
    );
    values.insert(
        "NAVIGATOR_ARCHIVE_POSTGRES_USER".into(),
        "navigator_archive".into(),
    );
    values.insert(
        "NAVIGATOR_ARCHIVE_POSTGRES_PASSWORD".into(),
        passwords.archive.into(),
    );
    storage_environment(&mut values, storage);
    values.insert(
        "NAVIGATOR_ARCHIVE_FORGEJO_USER".into(),
        "navigator_archive".into(),
    );
    values.insert(
        "NAVIGATOR_ARCHIVE_FORGEJO_PASSWORD".into(),
        passwords.archive.into(),
    );
    let coverage = fs::read_to_string(root.join("k8s/postgres/archive/coverage.json"))?;
    let coverage: serde_json::Value = serde_json::from_str(&coverage)?;
    values.insert(
        "NAVIGATOR_ARCHIVE_COVERAGE".into(),
        serde_json::to_string(&coverage)?,
    );
    // The existing development capturing backend is explicit; no fabricated
    // SendGrid credential is needed to satisfy the application harness.
    values.remove("SENDGRID_API_KEY");
    values.insert("NAVIGATOR_EMAIL_BACKEND".into(), "capturing".into());
    values.insert("NAVIGATOR_SIMULATED_MATTERS".into(), "true".into());
    values
        .entry("NAVIGATOR_FORGE_BACKEND".into())
        .or_insert_with(|| "forgejo".into());
    values.insert(
        "NAVIGATOR_FORGEJO_ROOT_URL".into(),
        format!("http://localhost:{}", cfg.forgejo_port),
    );
    Ok(values)
}

fn storage_environment(
    values: &mut BTreeMap<String, String>,
    storage: &super::seaweedfs::Credentials,
) {
    for (key, value) in super::seaweedfs::credential_env(storage) {
        values.insert(key.into(), value.into());
    }
    for (prefix, credentials) in [
        ("NAVIGATOR_STORAGE", &storage.documents),
        ("NAVIGATOR_ASSETS", &storage.assets),
        ("NAVIGATOR_APPLICATIONS", &storage.applications),
        ("NAVIGATOR_ARCHIVES", &storage.archives),
        ("NAVIGATOR_LFS", &storage.lfs),
        ("NAVIGATOR_EXPORTS", &storage.exports),
        ("NAVIGATOR_TELEMETRY", &storage.telemetry),
        ("NAVIGATOR_ARCHIVED_PROJECTS", &storage.archived_projects),
    ] {
        values.insert(
            format!("{prefix}_ACCESS_KEY"),
            credentials.access_key.clone(),
        );
        values.insert(
            format!("{prefix}_SECRET_KEY"),
            credentials.secret_key.clone(),
        );
    }
}

/// Translate the verified host tier's application settings to pod coordinates.
/// Staged sample files have already been seeded from the worktree into PostgreSQL.
pub(super) fn demo_environment(
    mut values: BTreeMap<String, String>,
    cfg: &KindConfig,
) -> BTreeMap<String, String> {
    values.retain(|key, _| {
        !key.starts_with("NAVIGATOR_KIND_")
            && !matches!(
                key.as_str(),
                "KUBECONFIG"
                    | "NAVIGATOR_K8S_NAMESPACE"
                    | "NAVIGATOR_GKE_OVERLAY"
                    | "NAVIGATOR_RELEASE_TAG"
                    | store::sample_project::STAGE_ENV
            )
    });
    for (key, value) in [
        ("PORT", "3001"),
        ("DIOXUS_PUBLIC_PATH", "/app/public/dioxus"),
        ("NAVIGATOR_POSTGRES_HOST", "navigator-postgres"),
        ("NAVIGATOR_POSTGRES_PORT", "5432"),
        (
            "NAVIGATOR_POSTGRES_ROOT_CERT",
            "/etc/navigator/postgres/ca.crt",
        ),
        ("NAVIGATOR_DBOS_HOST", "navigator-postgres"),
        ("NAVIGATOR_DBOS_PORT", "5432"),
        ("NAVIGATOR_DBOS_ROOT_CERT", "/etc/navigator/postgres/ca.crt"),
        (
            "NAVIGATOR_STORAGE_ENDPOINT",
            "http://navigator-seaweedfs:8333",
        ),
        ("NAVIGATOR_CLAMD_ADDR", "clamav:3310"),
        ("NAVIGATOR_OPENOBSERVE_URL", "http://openobserve:5080"),
        ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://openobserve:5081"),
        (
            "NAVIGATOR_FORGEJO_ROOT_URL",
            "http://navigator-forgejo:3000",
        ),
        ("NAVIGATOR_GIT_REPO_ROOT", "/var/lib/navigator/repos"),
        (
            "NAVIGATOR_FAKE_SIGNATURE_DIR",
            "/var/lib/navigator/repos/.fake-signature",
        ),
    ] {
        values.insert(key.into(), value.into());
    }
    values.insert(
        "OAUTH_REDIRECT_URI".into(),
        format!("http://localhost:{}/auth/callback", cfg.ingress_http_port),
    );
    values
}

/// Inject the verified release into the initial pod template so Kubernetes
/// never tries to start the placeholder tag. Rotate the pod on Secret changes.
pub(super) fn demo_manifest(rendered: &str, image: &str, environment_hash: &str) -> Result<String> {
    let mut output = String::new();
    let mut matched = 0;
    for doc in serde_yaml::Deserializer::from_str(rendered) {
        let mut value = serde_yaml::Value::deserialize(doc)?;
        if value["kind"].as_str() == Some("Deployment")
            && value["metadata"]["name"].as_str() == Some("navigator-web")
        {
            let template = &mut value["spec"]["template"];
            template["metadata"]["annotations"]["navigator.neonlaw.org/demo-environment"] =
                environment_hash.into();
            let containers = template["spec"]["containers"]
                .as_sequence_mut()
                .ok_or_else(|| anyhow!("missing demo containers"))?;
            for container in containers {
                if container["name"].as_str() == Some("web") {
                    container["image"] = image.into();
                    matched += 1;
                }
            }
        }
        output.push_str("---\n");
        output.push_str(&serde_yaml::to_string(&value)?);
    }
    ensure!(
        matched == 1,
        "demo must contain one published web container"
    );
    Ok(output)
}

pub(super) fn select_database(
    root: &Path,
    cfg: &KindConfig,
    existing: Option<&str>,
) -> Result<String> {
    if let Some(database) = existing {
        ensure!(
            database != "navigator",
            "this worktree has a captured KIND tier; use a fresh worktree for PostgreSQL and DBOS"
        );
        let intent = Intent::read(root)?
            .ok_or_else(|| anyhow!("native descriptor lacks its reserved creation intent"))?;
        intent.verify_target(root, cfg)?;
        ensure!(
            intent.database()? == database,
            "native descriptor names another database"
        );
        return Ok(database.into());
    }
    ensure!(
        !root.join(".devx/env").exists() || Intent::read(root)?.is_some(),
        "an untracked environment requires explicit recovery before a new native tier"
    );
    Intent::prepare(root, cfg)?.database()
}

pub(super) fn selected(root: &Path, cfg: &KindConfig) -> Result<bool> {
    match Intent::read(root)? {
        Some(intent) => {
            intent.verify_target(root, cfg)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

pub(super) fn roles_for_database(rendered: &str, database: &str) -> Result<String> {
    let operation = database
        .strip_prefix("navigator_native_")
        .and_then(|text| Uuid::parse_str(text).ok())
        .ok_or_else(|| anyhow!("role target is not a native creation identifier"))?;
    ensure!(
        native_origin::database_name(operation)? == database,
        "role target differs from its creation operation"
    );
    let mut output = String::new();
    let mut roles = 0;
    for doc in serde_yaml::Deserializer::from_str(rendered) {
        let mut value = serde_yaml::Value::deserialize(doc)?;
        if value["kind"].as_str() == Some("Job")
            && value["metadata"]["name"].as_str() == Some("navigator-postgres-roles")
        {
            let container = &mut value["spec"]["template"]["spec"]["containers"][0];
            container["command"]
                .as_sequence_mut()
                .ok_or_else(|| anyhow!("roles command is missing"))?
                .push(serde_yaml::Value::String(format!(
                    "--set=navigator_database={database}"
                )));
            let env = container["env"]
                .as_sequence_mut()
                .ok_or_else(|| anyhow!("roles environment is missing"))?;
            let database_env = env
                .iter_mut()
                .find(|entry| entry["name"].as_str() == Some("PGDATABASE"))
                .ok_or_else(|| anyhow!("roles database setting is missing"))?;
            *database_env =
                serde_yaml::to_value(serde_json::json!({"name":"PGDATABASE","value":database}))?;
            roles += 1;
        }
        output.push_str("---\n");
        output.push_str(&serde_yaml::to_string(&value)?);
    }
    ensure!(
        roles == 1,
        "native role overlay must contain exactly one role initializer"
    );
    Ok(output)
}

/// Initial infrastructure does not run role grants against an unmigrated database.
/// The same initializer is applied separately after the origin transaction commits.
pub(super) fn infrastructure(rendered: &str) -> Result<String> {
    let mut output = String::new();
    let mut jobs = 0;
    for doc in serde_yaml::Deserializer::from_str(rendered) {
        let value = serde_yaml::Value::deserialize(doc)?;
        if value["kind"].as_str() == Some("Job")
            && value["metadata"]["name"].as_str() == Some("navigator-postgres-roles")
        {
            jobs += 1;
            continue;
        }
        ensure!(
            value["kind"].as_str() != Some("RestateDeployment"),
            "native dependencies cannot register a Restate workload"
        );
        let name = value["metadata"]["name"].as_str().unwrap_or_default();
        ensure!(
            !matches!(
                name,
                "surreal" | "navigator-surreal" | "restate" | "workflows-service"
            ),
            "native dependencies contain a legacy engine workload"
        );
        output.push_str("---\n");
        output.push_str(&serde_yaml::to_string(&value)?);
    }
    ensure!(
        jobs == 1,
        "native dependencies require exactly one deferred role initializer"
    );
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_reaches_the_seeded_database_and_retains_the_cohost_identity() {
        let cfg = KindConfig::from_env();
        let values = BTreeMap::from([
            ("NAVIGATOR_POSTGRES_DATABASE".into(), "owned-db".into()),
            ("NAVIGATOR_DBOS_DATABASE".into(), "owned-db".into()),
            (
                "NAVIGATOR_DBOS_INGRESS_TOKEN".into(),
                "fixture-token".into(),
            ),
            ("KUBECONFIG".into(), "/host/config".into()),
            ("NAVIGATOR_RELEASE_TAG".into(), "host-build".into()),
            (
                store::sample_project::STAGE_ENV.into(),
                "/host/stage".into(),
            ),
        ]);
        let pod = demo_environment(values, &cfg);
        for prefix in ["NAVIGATOR_POSTGRES", "NAVIGATOR_DBOS"] {
            assert_eq!(pod[&format!("{prefix}_HOST")], "navigator-postgres");
            assert_eq!(pod[&format!("{prefix}_PORT")], "5432");
            assert_eq!(pod[&format!("{prefix}_DATABASE")], "owned-db");
            assert_eq!(
                pod[&format!("{prefix}_ROOT_CERT")],
                "/etc/navigator/postgres/ca.crt"
            );
        }
        assert_eq!(pod["NAVIGATOR_DBOS_INGRESS_TOKEN"], "fixture-token");
        assert_eq!(
            pod["OAUTH_REDIRECT_URI"],
            format!("http://localhost:{}/auth/callback", cfg.ingress_http_port)
        );
        for key in [
            "KUBECONFIG",
            "NAVIGATOR_RELEASE_TAG",
            store::sample_project::STAGE_ENV,
        ] {
            assert!(!pod.contains_key(key));
        }
    }

    #[test]
    fn demo_pins_only_the_application_container_and_rotates_its_environment() {
        let source = "kind: Deployment\nmetadata: {name: navigator-web}\nspec:\n  template:\n    spec:\n      containers: [{name: proxy, image: nginx}, {name: web, image: placeholder}]";
        let result = demo_manifest(source, "registry/neon-server:26.11.24", "hash").unwrap();
        let value: serde_yaml::Value = serde_yaml::from_str(&result).unwrap();
        let template = &value["spec"]["template"];
        assert_eq!(template["spec"]["containers"][0]["image"], "nginx");
        assert_eq!(
            template["spec"]["containers"][1]["image"],
            "registry/neon-server:26.11.24"
        );
        assert_eq!(
            template["metadata"]["annotations"]["navigator.neonlaw.org/demo-environment"],
            "hash"
        );
        assert!(demo_manifest("kind: Service", "release", "hash").is_err());
    }

    #[test]
    fn new_tier_reserves_one_private_operation_and_refuses_foreign_resume() {
        let root = tempfile::tempdir().unwrap();
        let cfg = KindConfig::from_env();
        let first = Intent::prepare(root.path(), &cfg).unwrap();
        let second = Intent::prepare(root.path(), &cfg).unwrap();
        assert_eq!(first.creation, second.creation);
        assert_eq!(first.token, second.token);
        assert!(
            first.binding(Uuid::new_v4()).is_err(),
            "unbound namespace is not empty-resource authority"
        );
        let mut foreign = cfg.clone();
        foreign.cluster = "other-native-tier".into();
        assert!(Intent::prepare(root.path(), &foreign).is_err());
        let other = tempfile::tempdir().unwrap();
        assert!(first.verify_target(other.path(), &cfg).is_err());
        fs::set_permissions(root.path().join(STATE), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Intent::read(root.path()).is_err());
    }

    #[test]
    fn native_timers_default_off_and_require_boolean_coordinates() {
        let flags = timer_coordinates(&|_| None).unwrap();
        assert_eq!(
            flags["NAVIGATOR_DBOS_BILLING_CANARY_SCHEDULE_ENABLED"],
            "false"
        );
        assert_eq!(
            flags["NAVIGATOR_DBOS_RECONCILE_INVOICES_SCHEDULE_ENABLED"],
            "false"
        );
        assert!(timer_coordinates(
            &|key| if key == "NAVIGATOR_DBOS_BILLING_CANARY_SCHEDULE_ENABLED" {
                Some("invalid".into())
            } else {
                None
            }
        )
        .is_err());
        let opted = timer_coordinates(&|key| {
            if key == "NAVIGATOR_DBOS_BILLING_CANARY_SCHEDULE_ENABLED" {
                Some("true".into())
            } else {
                None
            }
        })
        .unwrap();
        assert_eq!(
            opted["NAVIGATOR_DBOS_BILLING_CANARY_SCHEDULE_ENABLED"],
            "true"
        );
    }

    #[test]
    fn captured_kind_descriptor_cannot_start_or_reserve_a_replacement_tier() {
        let root = tempfile::tempdir().unwrap();
        let cfg = KindConfig::from_env();
        let error = select_database(root.path(), &cfg, Some("navigator")).unwrap_err();
        assert!(error.to_string().contains("use a fresh worktree"));
        assert!(Intent::read(root.path()).unwrap().is_none());
    }

    #[test]
    fn a_descriptor_cannot_replace_a_reserved_native_database() {
        let root = tempfile::tempdir().unwrap();
        let cfg = KindConfig::from_env();
        let database = select_database(root.path(), &cfg, None).unwrap();
        assert_eq!(
            select_database(root.path(), &cfg, Some(&database)).unwrap(),
            database
        );
        assert!(select_database(root.path(), &cfg, Some("navigator")).is_err());
        let foreign = native_origin::database_name(Uuid::new_v4()).unwrap();
        assert!(select_database(root.path(), &cfg, Some(&foreign)).is_err());
        let other = tempfile::tempdir().unwrap();
        fs::create_dir_all(other.path().join(".devx")).unwrap();
        fs::write(other.path().join(".devx/env"), "existing tier").unwrap();
        assert!(select_database(other.path(), &cfg, None).is_err());
    }

    #[test]
    fn native_environment_keeps_role_credentials_local_and_rejects_another_database() {
        let root = tempfile::tempdir().unwrap();
        let coverage = root.path().join("k8s/postgres/archive");
        fs::create_dir_all(&coverage).unwrap();
        fs::write(
            coverage.join("coverage.json"),
            include_str!("../../../k8s/postgres/archive/coverage.json"),
        )
        .unwrap();
        let cfg = KindConfig::from_env();
        let intent = Intent::prepare(root.path(), &cfg).unwrap();
        let credentials = super::super::postgres_fixture::Credentials {
            app_password: "synthetic-app-password".into(),

            ca: "synthetic-local-ca".into(),
        };
        let lanes = super::super::seaweedfs::LANES
            .iter()
            .map(|(name, _)| {
                (
                    (*name).into(),
                    super::super::seaweedfs::LaneCredentials {
                        access_key: format!("synthetic-{name}-access"),
                        secret_key: format!("synthetic-{name}-secret"),
                    },
                )
            })
            .collect();
        let storage = super::super::seaweedfs::credentials(&lanes).unwrap();
        let build = |database: &str| {
            environment(
                root.path(),
                &cfg,
                &intent,
                credentials.environment_for(&cfg, root.path(), database),
                RolePasswords {
                    dbos: "synthetic-dbos-password",
                    archive: "synthetic-archive-password",
                },
                &storage,
                |_| None,
            )
        };
        assert!(build(&native_origin::database_name(Uuid::new_v4()).unwrap()).is_err());
        let values = build(&intent.database().unwrap()).unwrap();
        assert!(values
            .keys()
            .all(|key| !key.starts_with("NAVIGATOR_SURREAL_") && !key.starts_with("RESTATE_")));
        assert_eq!(
            values["NAVIGATOR_POSTGRES_PASSWORD"],
            credentials.app_password
        );
        assert_ne!(
            values["NAVIGATOR_POSTGRES_PASSWORD"],
            values["NAVIGATOR_DBOS_PASSWORD"]
        );
        assert_ne!(
            values["NAVIGATOR_DBOS_PASSWORD"],
            values["NAVIGATOR_ARCHIVE_POSTGRES_PASSWORD"]
        );
        let ingress = reqwest::Url::parse(&values["NAVIGATOR_DBOS_INGRESS_URL"]).unwrap();
        assert_eq!(ingress.host_str(), Some("127.0.0.1"));
        assert_eq!(ingress.port(), Some(dbos_port(&cfg).unwrap()));
        assert_eq!(values["NAVIGATOR_DBOS_INGRESS_TOKEN"], intent.token);
        assert!(!values.contains_key("SENDGRID_API_KEY"));
        assert!(!values.contains_key("XERO_ACCESS_TOKEN"));
        assert_eq!(
            values["NAVIGATOR_STORAGE_ACCESS_KEY"],
            storage.documents.access_key
        );
        assert_ne!(
            values["NAVIGATOR_STORAGE_ACCESS_KEY"],
            values["NAVIGATOR_ARCHIVES_ACCESS_KEY"]
        );
    }

    #[test]
    fn infrastructure_defers_roles_and_refuses_a_legacy_workload() {
        let job = "kind: Job\nmetadata: {name: navigator-postgres-roles}\n";
        let postgres = "kind: StatefulSet\nmetadata: {name: navigator-postgres}\n";
        let rendered = infrastructure(&format!("{job}---\n{postgres}")).unwrap();
        let resources = serde_yaml::Deserializer::from_str(&rendered)
            .map(|doc| serde_yaml::Value::deserialize(doc).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(resources.len(), 1);
        assert_eq!(
            resources[0]["metadata"]["name"].as_str(),
            Some("navigator-postgres")
        );
        assert!(infrastructure(postgres).is_err());
        for legacy in ["surreal", "restate", "workflows-service"] {
            assert!(infrastructure(&format!(
                "{job}---\nkind: Deployment\nmetadata: {{name: {legacy}}}\n"
            ))
            .is_err());
        }
        assert!(infrastructure(&format!(
            "{job}---\nkind: RestateDeployment\nmetadata: {{name: any}}\n"
        ))
        .is_err());
    }

    #[test]
    fn native_role_target_replaces_the_old_database_reference() {
        let database = native_origin::database_name(Uuid::new_v4()).unwrap();
        let source = r"apiVersion: batch/v1
kind: Job
metadata:
  name: navigator-postgres-roles
spec:
  template:
    spec:
      containers:
        - command: [psql, --file=/roles/roles.sql]
          env:
            - name: PGDATABASE
              valueFrom:
                secretKeyRef: {name: old, key: database}
";
        let rendered = roles_for_database(source, &database).unwrap();
        let value: serde_yaml::Value = serde_yaml::from_str(&rendered).unwrap();
        let container = &value["spec"]["template"]["spec"]["containers"][0];
        assert_eq!(
            container["env"][0]["value"].as_str(),
            Some(database.as_str())
        );
        assert!(container["env"][0].get("valueFrom").is_none());
        assert_eq!(
            container["command"][2].as_str(),
            Some(format!("--set=navigator_database={database}").as_str())
        );
        assert!(roles_for_database(source, "navigator; DROP DATABASE other").is_err());
        assert!(roles_for_database("", &database).is_err());
        assert!(roles_for_database(&format!("{source}\n---\n{source}"), &database).is_err());
    }

    #[test]
    fn native_listener_uses_its_own_slot_instead_of_a_retired_engine_port() {
        let mut cfg = KindConfig::from_env();
        cfg.web_port = 20637;
        assert_eq!(dbos_port(&cfg).unwrap(), 21337);
        cfg.web_port = u16::MAX;
        assert!(dbos_port(&cfg).is_err());
    }
}

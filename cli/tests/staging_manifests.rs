use std::{io::ErrorKind, path::Path, process::Command};

use serde::Deserialize;
use serde_yaml::Value;

const KIND: &str = "k8s/overlays/kind";
const KIND_DEPS: &str = "k8s/overlays/kind-deps";
const GKE: &str = "examples/deploy/k8s/gke";

const CLI_README: &str = "cli/README.md";

fn workspace() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli crate has a workspace parent")
}

fn render(path: &str) -> Option<Vec<Value>> {
    let output = match Command::new("kubectl")
        .args(["kustomize", path])
        .current_dir(workspace())
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            eprintln!("kubectl unavailable; skipping staging manifest contract test");
            return None;
        }
        Err(error) => panic!("running kubectl kustomize {path}: {error}"),
    };
    assert!(
        output.status.success(),
        "kubectl kustomize {path} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(
        serde_yaml::Deserializer::from_slice(&output.stdout)
            .map(|document| Value::deserialize(document).expect("rendered resource is valid YAML"))
            .collect(),
    )
}

fn rendered_text(resources: &[Value]) -> String {
    serde_yaml::to_string(resources).expect("resources serialize")
}

fn deployment_names(resources: &[Value]) -> Vec<&str> {
    resources
        .iter()
        .filter(|resource| resource["kind"].as_str() == Some("Deployment"))
        .filter_map(|resource| resource["metadata"]["name"].as_str())
        .collect()
}

fn resource<'a>(resources: &'a [Value], kind: &str, name: &str) -> &'a Value {
    resources
        .iter()
        .find(|resource| {
            resource["kind"].as_str() == Some(kind)
                && resource["metadata"]["name"].as_str() == Some(name)
        })
        .unwrap_or_else(|| panic!("{kind}/{name} must be rendered"))
}

/// The store endpoint both workloads must resolve to the same `ConfigMap`.
///
/// `web` and the worker journal to one database: a Notation `web` commits
/// has to be visible to the worker that advances it, so a manifest that
/// points them at different coordinates is a `RecordNotFound` on every
/// Restate-backed flow rather than a config nit.
fn store_endpoint(resource: &Value) -> &str {
    resource["spec"]["template"]["spec"]["containers"]
        .as_sequence()
        .expect("workload containers")
        .iter()
        .flat_map(|container| container["env"].as_sequence().into_iter().flatten())
        .find(|entry| entry["name"].as_str() == Some("NAVIGATOR_SURREAL_ENDPOINT"))
        .and_then(|entry| entry["valueFrom"]["configMapKeyRef"]["name"].as_str())
        .expect("NAVIGATOR_SURREAL_ENDPOINT must be ConfigMap-backed")
}

fn env_entry<'a>(resource: &'a Value, name: &str) -> &'a Value {
    resource["spec"]["template"]["spec"]["containers"]
        .as_sequence()
        .expect("workload containers")
        .iter()
        .flat_map(|container| container["env"].as_sequence().into_iter().flatten())
        .find(|entry| entry["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("{name} must be configured"))
}

fn container<'a>(resource: &'a Value, name: &str) -> &'a Value {
    resource["spec"]["template"]["spec"]["containers"]
        .as_sequence()
        .expect("workload containers")
        .iter()
        .find(|container| container["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("{name} container must be rendered"))
}

fn config_map_key<'a>(resource: &'a Value, env_name: &str) -> &'a str {
    let matches = resource["spec"]["template"]["spec"]["containers"]
        .as_sequence()
        .expect("workload containers")
        .iter()
        .flat_map(|container| container["env"].as_sequence().into_iter().flatten())
        .filter(|entry| entry["name"].as_str() == Some(env_name))
        .filter_map(|entry| entry["valueFrom"]["configMapKeyRef"]["key"].as_str())
        .collect::<Vec<_>>();
    // Kubernetes resolves duplicate env names to the last entry, so accepting the
    // first match would let a stray duplicate ship the wrong bucket while the pod
    // boots with a different value. Require the coordinate to be defined exactly once.
    match matches.as_slice() {
        [key] => key,
        [] => panic!("{env_name} must be ConfigMap-backed"),
        _ => panic!("{env_name} must be defined exactly once"),
    }
}

#[test]
fn staging_contracts_keep_secrets_lanes_brand_and_store_coordinates_aligned() {
    for overlay in [KIND] {
        let Some(resources) = render(overlay) else {
            return;
        };
        let text = rendered_text(&resources);
        let web = resources
            .iter()
            .find(|resource| {
                resource["kind"].as_str() == Some("Deployment")
                    && resource["metadata"]["name"]
                        .as_str()
                        .is_some_and(|name| name.ends_with("navigator-web"))
            })
            .expect("staging web deployment");
        let worker = resources
            .iter()
            .find(|resource| resource["kind"].as_str() == Some("RestateDeployment"))
            .expect("staging worker");
        assert_eq!(store_endpoint(web), store_endpoint(worker));
        assert_eq!(
            config_map_key(worker, "NAVIGATOR_DOCUMENTS_BUCKET"),
            "documents_bucket"
        );
        assert_eq!(
            config_map_key(worker, "NAVIGATOR_STORAGE_BUCKET"),
            "exports_bucket"
        );
        assert_eq!(
            config_map_key(worker, "NAVIGATOR_ARCHIVES_BUCKET"),
            "archives_bucket"
        );
        // The second half of the two-bucket proof: a worker where the
        // Iceberg-archive and exports keys resolve to the same ConfigMap
        // value would pass the key-name assertion above while proving
        // nothing about a two-bucket change (ENG-214).
        let seaweedfs_config_map = resource(&resources, "ConfigMap", "navigator-seaweedfs");
        let archives_bucket_value = seaweedfs_config_map["data"]["archives_bucket"]
            .as_str()
            .expect("navigator-seaweedfs ConfigMap must define archives_bucket");
        let exports_bucket_value = seaweedfs_config_map["data"]["exports_bucket"]
            .as_str()
            .expect("navigator-seaweedfs ConfigMap must define exports_bucket");
        assert_ne!(
            archives_bucket_value, exports_bucket_value,
            "{overlay} the Iceberg-archive bucket must differ from the exports bucket \
             NAVIGATOR_STORAGE_BUCKET resolves to"
        );
        assert!(text.contains("NAVIGATOR_CUSTOM_BRANDING"));
        assert!(!text.contains("NAVIGATOR_BRAND_BUNDLE_DIR"));
        assert_eq!(
            env_entry(web, "NAVIGATOR_BOOTSTRAP_COMPANY")["value"].as_str(),
            Some(store::seed::FIRM_ENTITY_NAME),
            "{overlay} must protect the firm anchor entity — the legal person, \
             not the `Neon Law` mark it trades under"
        );
        // The application environment value is `dev` for the cloud staging
        // lane: it shares the one development profile with local KIND.
        // "staging" survives only as the deployment-lane label asserted
        // below, never as a `NAVIGATOR_ENVIRONMENT` value.
        assert_eq!(
            env_entry(web, "NAVIGATOR_ENVIRONMENT")["value"].as_str(),
            Some("dev"),
            "{overlay} web must emit NAVIGATOR_ENVIRONMENT=dev"
        );
        assert_eq!(
            env_entry(worker, "NAVIGATOR_ENVIRONMENT")["value"].as_str(),
            Some("dev"),
            "{overlay} worker must emit NAVIGATOR_ENVIRONMENT=dev"
        );
        assert!(text.contains("readOnly: true"));
        for lane in ["documents", "assets", "exports", "archives", "lfs"] {
            assert!(text.contains(lane), "{overlay} must preserve {lane} lane");
        }
        assert!(text.contains("navigator.neonlaw.org/environment: staging"));
        assert!(
            !rendered_text(std::slice::from_ref(web)).contains("restate.restate.svc.cluster.local")
        );
    }
    let Some(kind_deps) = render(KIND_DEPS) else {
        return;
    };
    assert!(kind_deps
        .iter()
        .all(|resource| { resource["metadata"]["name"].as_str() != Some("navigator-web") }));
}

#[test]
fn staging_operator_docs_match_the_implemented_dev_lifecycle() {
    let cli_readme = std::fs::read_to_string(workspace().join(CLI_README)).expect("CLI README");
    assert!(
        cli_readme.contains("NAVIGATOR_ENVIRONMENT=dev"),
        "the local staging command must require the dev application profile"
    );
    assert!(
        !cli_readme.contains("NAVIGATOR_ENVIRONMENT=staging"),
        "staging is a lifecycle target, not an application environment"
    );
}

#[test]
fn kind_identity_provider_is_rauthy() {
    for overlay in [KIND, KIND_DEPS] {
        let Some(resources) = render(overlay) else {
            return;
        };
        let deployments = deployment_names(&resources);
        assert!(
            deployments.contains(&"rauthy"),
            "{overlay} must deploy Rauthy: {deployments:?}"
        );
        assert!(
            !deployments.contains(&"keycloak"),
            "{overlay} must not retain the retired identity provider: {deployments:?}"
        );
    }
}

#[test]
fn kind_rauthy_bootstrap_matches_the_local_login_contract() {
    let Some(resources) = render(KIND) else {
        return;
    };
    let bootstrap = resource(&resources, "ConfigMap", "rauthy-bootstrap");
    let users: serde_json::Value = serde_json::from_str(
        bootstrap["data"]["users.json"]
            .as_str()
            .expect("Rauthy users.json"),
    )
    .expect("valid users.json");
    let users = users.as_array().expect("Rauthy users are an array");
    for (email, username, password) in [
        ("owner@neonlaw.com", "owner", "password"),
        ("admin@neonlaw.com", "admin", "password"),
        ("lawyer@neonlaw.com", "lawyer", "password"),
        ("clerk@neonlaw.com", "clerk", "password"),
        ("client@neonlaw.com", "client", "password"),
    ] {
        let user = users
            .iter()
            .find(|user| user["email"].as_str() == Some(email))
            .unwrap_or_else(|| panic!("Rauthy must seed {email}"));
        assert_eq!(user["preferred_username"].as_str(), Some(username));
        assert_eq!(user["password"]["Plain"].as_str(), Some(password));
        assert_eq!(user["email_verified"].as_bool(), Some(true));
    }

    let clients: serde_json::Value = serde_json::from_str(
        bootstrap["data"]["clients.json"]
            .as_str()
            .expect("Rauthy clients.json"),
    )
    .expect("valid clients.json");
    let client = clients
        .as_array()
        .expect("Rauthy clients are an array")
        .iter()
        .find(|client| client["id"].as_str() == Some("navigator-web"))
        .expect("navigator-web client");
    assert_eq!(
        client["secret"]["Plain"]
            .as_str()
            .expect("confidential client secret")
            .len(),
        64,
        "Rauthy requires a 64-character confidential-client secret"
    );
    assert_eq!(
        client["redirect_uris"],
        serde_json::json!(["http://localhost:*"])
    );
    assert!(
        client.get("allowed_origins").is_none(),
        "Rauthy permits loopback wildcards for redirects, not CORS origins"
    );
    assert_eq!(client["challenges"], serde_json::json!(["S256"]));
    assert_eq!(client["id_token_alg"].as_str(), Some("RS256"));
    assert_eq!(client["force_mfa"].as_bool(), Some(false));

    let config = bootstrap["data"]["config.toml"]
        .as_str()
        .expect("Rauthy config.toml");
    assert!(config.contains("insecure_cookie = true"));
    assert!(config.contains("enable = false"));

    let secrets = resource(&resources, "Secret", "rauthy-secrets");
    assert_eq!(
        secrets["stringData"]["BOOTSTRAP_ADMIN_EMAIL"].as_str(),
        Some("nick@neonlaw.com")
    );
    assert_eq!(
        secrets["stringData"]["BOOTSTRAP_ADMIN_PASSWORD_PLAIN"].as_str(),
        Some("admin")
    );
    assert_eq!(
        secrets["stringData"]["OAUTH_ISSUER_URL"].as_str(),
        Some("http://localhost:30080/auth/v1/"),
        "the application issuer must exactly match Rauthy's discovery metadata"
    );
    let encryption_key = secrets["stringData"]["ENC_KEYS"]
        .as_str()
        .expect("Rauthy encryption key");
    let (key_id, _) = encryption_key
        .split_once('/')
        .expect("Rauthy encryption key embeds its ID");
    assert_eq!(
        key_id.len(),
        6,
        "Rauthy encryption-key IDs are exactly six characters"
    );
    assert_eq!(
        secrets["stringData"]["ENC_KEY_ACTIVE"].as_str(),
        Some(key_id),
        "the active Rauthy encryption-key ID must identify the fixture key"
    );
}

#[test]
fn reusable_rauthy_layer_contains_no_kind_credentials() {
    let staging =
        std::fs::read_to_string(workspace().join("k8s/staging/rauthy.yaml")).expect("Rauthy");
    for local_only in [
        "\"Plain\": \"password\"",
        "BOOTSTRAP_ADMIN_PASSWORD_PLAIN: admin",
        "http://localhost:*",
        "navigatorwebsecret",
    ] {
        assert!(
            !staging.contains(local_only),
            "reusable Rauthy layer must not contain `{local_only}`"
        );
    }
    for environment_owned in ["rauthy-secrets", "rauthy-client", "rauthy-bootstrap"] {
        assert!(
            staging.contains(environment_owned),
            "reusable Rauthy layer must require {environment_owned}"
        );
    }
}

#[test]
fn staging_brand_hosts_are_present_in_the_staging_manifest_contract() {
    let staging = std::fs::read_to_string(workspace().join("docs/environments.md"))
        .expect("staging environment contract");
    for host in ["staging.deleteyourdata.com", "staging.lawyershook.com"] {
        assert!(
            staging.contains(host),
            "staging manifest documentation must name {host}"
        );
    }
}

#[test]
fn production_overlay_excludes_the_local_rauthy_fixture() {
    let Some(resources) = render(GKE) else {
        return;
    };
    let production = rendered_text(&resources);
    for local_only in [
        "rauthy-bootstrap",
        "users.json",
        "\"Plain\": \"password\"",
        "BOOTSTRAP_ADMIN_PASSWORD_PLAIN",
    ] {
        assert!(
            !production.contains(local_only),
            "production must exclude the local Rauthy fixture value `{local_only}`"
        );
    }
}

#[test]
fn gke_web_rolls_with_a_surge_pod_on_per_pod_repository_scratch() {
    let Some(resources) = render(GKE) else {
        return;
    };
    let deployment = resource(&resources, "Deployment", "navigator-web");
    assert_eq!(
        deployment["spec"]["replicas"].as_u64(),
        Some(1),
        "the committed overlay runs one pod; `ops ship` sets the row's count"
    );
    let strategy = &deployment["spec"]["strategy"];
    assert_eq!(strategy["type"].as_str(), Some("RollingUpdate"));
    assert_eq!(
        strategy["rollingUpdate"]["maxSurge"].as_u64(),
        Some(1),
        "the new pod comes up before the old one leaves"
    );
    assert_eq!(
        strategy["rollingUpdate"]["maxUnavailable"].as_u64(),
        Some(0)
    );

    let web = container(deployment, "web");
    assert_eq!(
        web["readinessProbe"]["httpGet"]["path"].as_str(),
        Some("/app/readyz"),
        "repository storage remains part of readiness"
    );
    assert_eq!(
        env_entry(deployment, "NAVIGATOR_GIT_REPO_ROOT")["value"].as_str(),
        Some("/var/lib/navigator/repos"),
        "the GKE overlay must restore the base repo-root setting after replacing env"
    );

    let repos_volume = deployment["spec"]["template"]["spec"]["volumes"]
        .as_sequence()
        .expect("web volumes")
        .iter()
        .find(|volume| volume["name"].as_str() == Some("repos"))
        .expect("web repository volume");
    assert!(
        repos_volume.get("emptyDir").is_some(),
        "the repository directory is per-pod scratch: {repos_volume:?}"
    );
    assert!(
        !resources
            .iter()
            .any(|r| r["kind"].as_str() == Some("PersistentVolumeClaim")
                && r["metadata"]["name"].as_str() == Some("navigator-web-repos")),
        "no single-writer repository claim pins web to one pod"
    );
    assert!(
        !resources
            .iter()
            .any(|r| r["kind"].as_str() == Some("PodDisruptionBudget")),
        "a one-pod render carries no disruption budget"
    );
    assert!(
        !resources
            .iter()
            .any(|r| r["kind"].as_str() == Some("StorageClass")),
        "GKE must not provision Filestore again"
    );
}

#[test]
fn workflows_service_resources_rollout_and_probes_are_rendered() {
    let Some(resources) = render(GKE) else {
        return;
    };
    let deployment = resource(&resources, "Deployment", "workflows-service");

    for (name, cpu, memory) in [("worker", "100m", "256Mi"), ("envoy", "50m", "128Mi")] {
        let container = container(deployment, name);
        assert_eq!(
            container["resources"]["requests"]["cpu"].as_str(),
            Some(cpu),
            "{name} CPU request"
        );
        assert_eq!(
            container["resources"]["requests"]["memory"].as_str(),
            Some(memory),
            "{name} memory request"
        );
    }

    assert_eq!(
        deployment["spec"]["strategy"]["type"].as_str(),
        Some("RollingUpdate")
    );
    assert_eq!(
        deployment["spec"]["strategy"]["rollingUpdate"]["maxSurge"].as_u64(),
        Some(0)
    );
    assert_eq!(
        deployment["spec"]["strategy"]["rollingUpdate"]["maxUnavailable"].as_u64(),
        Some(1)
    );

    let worker = container(deployment, "worker");
    assert_eq!(
        worker["readinessProbe"]["httpGet"]["path"].as_str(),
        Some("/healthz")
    );
    assert_eq!(
        worker["readinessProbe"]["httpGet"]["port"].as_u64(),
        Some(9083)
    );
    assert_eq!(
        worker["livenessProbe"]["httpGet"]["path"].as_str(),
        Some("/healthz")
    );
    assert_eq!(
        worker["livenessProbe"]["httpGet"]["port"].as_u64(),
        Some(9083)
    );

    let envoy = container(deployment, "envoy");
    assert_eq!(
        envoy["readinessProbe"]["httpGet"]["path"].as_str(),
        Some("/restate/health")
    );
    assert_eq!(
        envoy["readinessProbe"]["httpGet"]["port"].as_u64(),
        Some(9081)
    );
    assert_eq!(
        envoy["livenessProbe"]["tcpSocket"]["port"].as_u64(),
        Some(9081)
    );
}

#[test]
fn postgres_gke_overlays_target_the_deployment_namespace_and_retain_storage() {
    for (environment, namespace, capacity) in [
        ("staging", "navigator", "100Gi"),
        ("production", "navigator", "100Gi"),
    ] {
        let overlay = format!("examples/deploy/k8s/gke/postgres/{environment}");
        let Some(resources) = render(&overlay) else {
            return;
        };

        let postgres = resource(&resources, "StatefulSet", "navigator-postgres");
        assert_eq!(postgres["metadata"]["namespace"].as_str(), Some(namespace));
        for resource in resources.iter().filter(|resource| {
            matches!(
                resource["kind"].as_str(),
                Some("ConfigMap" | "NetworkPolicy" | "Service" | "StatefulSet")
            )
        }) {
            assert_eq!(
                resource["metadata"]["namespace"].as_str(),
                Some(namespace),
                "{} must stay in the {environment} namespace",
                resource["kind"].as_str().unwrap_or("unknown")
            );
        }
        assert_eq!(
            postgres["spec"]["persistentVolumeClaimRetentionPolicy"]["whenDeleted"].as_str(),
            Some("Retain")
        );
        assert_eq!(
            postgres["spec"]["persistentVolumeClaimRetentionPolicy"]["whenScaled"].as_str(),
            Some("Retain")
        );
        assert_eq!(
            container(postgres, "postgres")["image"].as_str(),
            Some(
                "ghcr.io/neon-law-source-code/neon-law-postgres@sha256:4cc70fee3aa9e53455bd1612d0a5be1f06962f5bcdcda4c6bff47e2b46db969e"
            ),
            "GKE must consume the tested immutable PostgreSQL image digest"
        );
        let claim = &postgres["spec"]["volumeClaimTemplates"][0];
        assert_eq!(claim["metadata"]["name"].as_str(), Some("data"));
        assert_eq!(
            claim["spec"]["storageClassName"].as_str(),
            Some("navigator-postgres-retain")
        );
        assert_eq!(
            claim["spec"]["resources"]["requests"]["storage"].as_str(),
            Some(capacity)
        );

        let storage_class = resource(&resources, "StorageClass", "navigator-postgres-retain");
        assert_eq!(
            storage_class["provisioner"].as_str(),
            Some("pd.csi.storage.gke.io")
        );
        assert_eq!(storage_class["reclaimPolicy"].as_str(), Some("Retain"));
        assert_eq!(
            storage_class["parameters"]["replication-type"].as_str(),
            Some("regional-pd")
        );
    }
}

/// Once a real `RESTATE_IDENTITY_KEY` is configured, the worker's Restate SDK
/// endpoint requires a valid Restate Cloud signature on every request it
/// serves, including its own `/restate/health`. Neither kubelet's readiness
/// probe nor the GCE LB's `BackendConfig` health check ever carries that
/// signature, so a health check proxied through to the worker 401s forever.
/// Envoy must answer this specific path itself instead of forwarding it.
#[test]
fn workflows_service_envoy_answers_the_health_probe_without_proxying_to_the_worker() {
    let Some(resources) = render(GKE) else {
        return;
    };
    let config_map = resource(&resources, "ConfigMap", "workflows-service-envoy");
    let envoy_yaml = config_map["data"]["envoy.yaml"]
        .as_str()
        .expect("envoy.yaml embedded config");
    let envoy_config: Value = serde_yaml::from_str(envoy_yaml).expect("valid envoy config YAML");
    let routes = envoy_config["static_resources"]["listeners"][0]["filter_chains"][0]["filters"][0]
        ["typed_config"]["route_config"]["virtual_hosts"][0]["routes"]
        .as_sequence()
        .expect("configured routes");
    let health_route = routes
        .iter()
        .find(|route| route["match"]["path"].as_str() == Some("/restate/health"))
        .expect("a dedicated /restate/health route");
    assert!(
        health_route["direct_response"]["status"].as_u64() == Some(200),
        "envoy must answer /restate/health itself with a direct_response, not a proxy"
    );
    assert_ne!(
        health_route["route"]["cluster"].as_str(),
        Some("restate_worker"),
        "the health check must never reach the identity-gated worker endpoint"
    );
}

const POSTGRES_OVERLAYS: [(&str, &str); 2] =
    [("staging", "navigator"), ("production", "navigator")];

fn secret_volumes(resource: &Value) -> Vec<&str> {
    resource["spec"]["template"]["spec"]["volumes"]
        .as_sequence()
        .into_iter()
        .flatten()
        .filter_map(|volume| volume["secret"]["secretName"].as_str())
        .collect()
}

fn digest_pin(kustomization: &str) -> String {
    let text = std::fs::read_to_string(workspace().join(kustomization))
        .unwrap_or_else(|error| panic!("read {kustomization}: {error}"));
    let parsed: Value = serde_yaml::from_str(&text).expect("kustomization is YAML");
    parsed["images"]
        .as_sequence()
        .expect("images pin")
        .iter()
        .find(|image| image["name"].as_str() == Some("neon-law-postgres"))
        .and_then(|image| image["digest"].as_str())
        .unwrap_or_else(|| panic!("{kustomization} pins neon-law-postgres by digest"))
        .to_owned()
}

#[test]
fn postgres_client_workloads_share_the_server_image_digest() {
    let server = digest_pin("k8s/postgres/kustomization.yaml");
    for kustomization in [
        "k8s/postgres/access/kustomization.yaml",
        "k8s/postgres/backup/kustomization.yaml",
        "k8s/postgres/restore-drill/kustomization.yaml",
        "k8s/overlays/postgres-spike/kustomization.yaml",
        "k8s/forgejo/database/kustomization.yaml",
    ] {
        assert_eq!(digest_pin(kustomization), server, "{kustomization}");
    }
}

#[test]
fn postgres_overlays_converge_least_privilege_roles_without_the_superuser_elsewhere() {
    for (environment, namespace) in POSTGRES_OVERLAYS {
        let Some(resources) = render(&format!("examples/deploy/k8s/gke/postgres/{environment}"))
        else {
            return;
        };
        let client = resource(&resources, "ConfigMap", "navigator-postgres-client");
        assert_eq!(
            client["data"]["host"].as_str(),
            Some(format!("navigator-postgres.{namespace}.svc.cluster.local").as_str()),
            "verify-full needs the namespace-qualified Service name"
        );
        assert_eq!(client["data"]["sslmode"].as_str(), Some("verify-full"));

        let roles = resource(&resources, "Job", "navigator-postgres-roles");
        assert_eq!(roles["metadata"]["namespace"].as_str(), Some(namespace));
        assert_eq!(
            roles["spec"]["template"]["metadata"]["labels"]
                ["navigator.neonlaw.org/postgres-client"]
                .as_str(),
            Some("true")
        );
        assert_eq!(
            roles["spec"]["template"]["spec"]["automountServiceAccountToken"].as_bool(),
            Some(false)
        );
        assert_eq!(config_map_key(roles, "PGSSLMODE"), "sslmode");
        assert_eq!(config_map_key(roles, "PGHOST"), "host");
        let mut mounted = secret_volumes(roles);
        mounted.sort_unstable();
        assert_eq!(
            mounted,
            vec![
                "navigator-postgres-app",
                "navigator-postgres-backup",
                "navigator-postgres-dbos",
                "navigator-postgres-migrator"
            ]
        );
        let sql = resource(&resources, "ConfigMap", "navigator-postgres-roles")["data"]
            ["roles.sql"]
            .as_str()
            .expect("roles.sql is rendered into the ConfigMap");
        for statement in [
            "ALTER ROLE navigator_owner WITH LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION",
            "ALTER ROLE navigator_app WITH LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION",
            "ALTER ROLE navigator_backup WITH LOGIN REPLICATION NOSUPERUSER",
            "ALTER DATABASE navigator OWNER TO navigator_owner",
            "REVOKE ALL ON DATABASE navigator FROM PUBLIC",
            "REVOKE ALL ON SCHEMA public FROM PUBLIC",
            "GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO navigator_app",
            "SET log_min_error_statement = panic",
            "ALTER ROLE navigator_dbos WITH LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION",
            "CREATE SCHEMA IF NOT EXISTS dbos AUTHORIZATION navigator_dbos",
            "REVOKE ALL ON SCHEMA dbos FROM navigator_owner, navigator_app, navigator_backup",
            "REVOKE ALL ON SCHEMA public FROM navigator_dbos",
        ] {
            assert!(sql.contains(statement), "roles.sql must keep `{statement}`");
        }
        // The DBOS login's database CREATE is the one DDL grant: its SDK runs
        // `CREATE SCHEMA IF NOT EXISTS dbos` on every launch.
        let creates: Vec<&str> = sql
            .lines()
            .filter(|line| line.contains("GRANT") && line.contains("CREATE"))
            .collect();
        assert_eq!(
            creates,
            ["GRANT CONNECT, CREATE ON DATABASE navigator TO navigator_dbos;"],
            "no other login gains DDL"
        );
        assert!(
            !sql.contains("WITH GRANT OPTION"),
            "no login gains grant options"
        );
        let dbos_volume = roles["spec"]["template"]["spec"]["volumes"]
            .as_sequence()
            .expect("roles Job volumes")
            .iter()
            .find(|volume| volume["name"].as_str() == Some("dbos"))
            .expect("the roles Job mounts the DBOS login's Secret");
        assert_eq!(
            dbos_volume["secret"]["optional"].as_bool(),
            Some(true),
            "a deployment without the DBOS login still converges its roles"
        );
    }
}

#[test]
fn postgres_migrate_job_runs_the_entrypoint_as_the_migration_login() {
    for (environment, namespace) in POSTGRES_OVERLAYS {
        let Some(resources) = render(&format!(
            "examples/deploy/k8s/gke/postgres/migrate/{environment}"
        )) else {
            return;
        };
        assert_eq!(
            resources.len(),
            1,
            "the migrate overlay renders only its Job"
        );
        let job = resource(&resources, "Job", "navigator-migrate");
        assert_eq!(job["metadata"]["namespace"].as_str(), Some(namespace));
        assert_eq!(job["spec"]["backoffLimit"].as_u64(), Some(0));
        assert_eq!(
            job["spec"]["template"]["metadata"]["labels"]["navigator.neonlaw.org/postgres-client"]
                .as_str(),
            Some("true")
        );
        let migrate = container(job, "migrate");
        assert_eq!(
            migrate["image"].as_str(),
            Some("navigator-migrate"),
            "the operator pins the release image at apply time"
        );
        assert_eq!(
            migrate["command"].as_sequence().map(Vec::len),
            Some(1),
            "the migration entrypoint takes no arguments"
        );
        assert_eq!(
            env_entry(job, "NAVIGATOR_POSTGRES_USER")["value"].as_str(),
            Some("navigator_owner")
        );
        assert_eq!(
            env_entry(job, "NAVIGATOR_POSTGRES_PASSWORD")["valueFrom"]["secretKeyRef"]["name"]
                .as_str(),
            Some("navigator-postgres-migrator")
        );
        assert_eq!(config_map_key(job, "NAVIGATOR_POSTGRES_SSLMODE"), "sslmode");
        assert_eq!(config_map_key(job, "NAVIGATOR_POSTGRES_HOST"), "host");
        assert_eq!(
            env_entry(job, "NAVIGATOR_POSTGRES_ROOT_CERT")["value"].as_str(),
            Some("/etc/navigator/postgres/ca.crt")
        );
        assert_eq!(secret_volumes(job), vec!["navigator-postgres-migrator"]);
    }
}

#[test]
fn postgres_mirror_reconcile_job_runs_once_as_the_runtime_login() {
    for (environment, namespace) in POSTGRES_OVERLAYS {
        let Some(resources) = render(&format!(
            "examples/deploy/k8s/gke/postgres/mirror/{environment}"
        )) else {
            return;
        };
        assert_eq!(
            resources.len(),
            1,
            "the mirror overlay renders only its Job, never a CronJob"
        );
        let job = resource(&resources, "Job", "navigator-postgres-mirror-reconcile");
        assert_eq!(job["metadata"]["namespace"].as_str(), Some(namespace));
        assert_eq!(job["spec"]["backoffLimit"].as_u64(), Some(0));
        assert_eq!(
            job["spec"]["template"]["metadata"]["labels"]["navigator.neonlaw.org/postgres-client"]
                .as_str(),
            Some("true")
        );
        let reconcile = container(job, "reconcile");
        assert_eq!(
            reconcile["image"].as_str(),
            Some("navigator-surreal-archive"),
            "the operator pins the release image carrying the navigator CLI"
        );
        let args: Vec<&str> = reconcile["args"]
            .as_sequence()
            .expect("reconcile args")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(args, ["ops", "postgres", "mirror", "reconcile", "--json"]);
        assert_eq!(
            env_entry(job, "NAVIGATOR_POSTGRES_USER")["value"].as_str(),
            Some("navigator_app")
        );
        assert_eq!(
            env_entry(job, "NAVIGATOR_POSTGRES_PASSWORD")["valueFrom"]["secretKeyRef"]["name"]
                .as_str(),
            Some("navigator-postgres-app")
        );
        assert_eq!(config_map_key(job, "NAVIGATOR_POSTGRES_SSLMODE"), "sslmode");
        assert_eq!(config_map_key(job, "NAVIGATOR_POSTGRES_HOST"), "host");
        for surreal in [
            "NAVIGATOR_SURREAL_ENDPOINT",
            "NAVIGATOR_SURREAL_NAMESPACE",
            "NAVIGATOR_SURREAL_DATABASE",
            "NAVIGATOR_SURREAL_USER",
            "NAVIGATOR_SURREAL_PASSWORD",
        ] {
            let source = &env_entry(job, surreal)["valueFrom"]["secretKeyRef"];
            assert_eq!(source["name"].as_str(), Some("navigator-web-secrets"));
            assert_eq!(source["key"].as_str(), Some(surreal));
        }
        assert!(
            reconcile["envFrom"].is_null(),
            "the Job reads only the keys it names, not the whole deployment Secret"
        );
        assert_eq!(secret_volumes(job), vec!["navigator-postgres-app"]);
    }
}

#[test]
fn gke_web_without_the_postgres_opt_in_carries_no_postgres_wiring() {
    let Some(resources) = render(GKE) else {
        return;
    };
    let web = resource(&resources, "Deployment", "navigator-web");
    let text = serde_yaml::to_string(web).expect("web serializes");
    assert!(
        !text.contains("NAVIGATOR_POSTGRES_") && !text.contains("navigator-postgres"),
        "the shared GKE render must stay byte-identical for deployments without PostgreSQL"
    );
}

fn job_spec(resource: &Value) -> &Value {
    if resource["kind"].as_str() == Some("CronJob") {
        &resource["spec"]["jobTemplate"]["spec"]
    } else {
        &resource["spec"]
    }
}

fn pod_containers(resource: &Value) -> Vec<&Value> {
    let pod = match resource["kind"].as_str() {
        Some("CronJob") => &resource["spec"]["jobTemplate"]["spec"]["template"]["spec"],
        _ => &resource["spec"]["template"]["spec"],
    };
    ["initContainers", "containers"]
        .iter()
        .flat_map(|field| pod[*field].as_sequence().into_iter().flatten())
        .collect()
}

fn pod_env<'a>(resource: &'a Value, container_name: &str, env_name: &str) -> &'a Value {
    pod_containers(resource)
        .into_iter()
        .find(|container| container["name"].as_str() == Some(container_name))
        .unwrap_or_else(|| panic!("{container_name} container must be rendered"))["env"]
        .as_sequence()
        .expect("container env")
        .iter()
        .find(|entry| entry["name"].as_str() == Some(env_name))
        .unwrap_or_else(|| panic!("{container_name} must set {env_name}"))
}

#[test]
fn postgres_server_admits_only_the_backup_login_to_replication() {
    for (environment, _) in POSTGRES_OVERLAYS {
        let Some(resources) = render(&format!("examples/deploy/k8s/gke/postgres/{environment}"))
        else {
            return;
        };
        let server = resource(&resources, "StatefulSet", "navigator-postgres");
        let args: Vec<&str> = container(server, "postgres")["args"]
            .as_sequence()
            .expect("server args")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(args.contains(&"wal_level=replica"));
        assert!(
            args.contains(&"max_slot_wal_keep_size=4GB"),
            "a stalled archiver must not fill the data volume"
        );
        let hba = resource(&resources, "ConfigMap", "navigator-postgres-config")["data"]
            ["pg_hba.conf"]
            .as_str()
            .expect("pg_hba.conf");
        let replication: Vec<&str> = hba
            .lines()
            .filter(|line| line.contains("replication"))
            .collect();
        assert_eq!(replication.len(), 2, "IPv4 and IPv6 replication entries");
        assert!(
            replication.iter().all(|line| line
                .starts_with("hostssl replication navigator_backup ")
                && line.ends_with(" scram-sha-256")),
            "only the backup login may replicate, and only over TLS: {replication:?}"
        );
    }
}

#[test]
fn postgres_overlays_archive_wal_and_take_daily_base_backups() {
    for (environment, namespace) in POSTGRES_OVERLAYS {
        let Some(resources) = render(&format!("examples/deploy/k8s/gke/postgres/{environment}"))
        else {
            return;
        };
        let archive = resource(&resources, "StatefulSet", "navigator-postgres-wal-archive");
        assert_eq!(archive["metadata"]["namespace"].as_str(), Some(namespace));
        assert_eq!(
            archive["spec"]["template"]["metadata"]["labels"]
                ["navigator.neonlaw.org/postgres-client"]
                .as_str(),
            Some("true")
        );
        assert_eq!(
            archive["spec"]["template"]["spec"]["serviceAccountName"].as_str(),
            Some("navigator-postgres-backup")
        );
        assert_eq!(
            archive["spec"]["persistentVolumeClaimRetentionPolicy"]["whenDeleted"].as_str(),
            Some("Retain")
        );
        assert_eq!(
            pod_env(archive, "receive", "PGUSER")["value"].as_str(),
            Some("navigator_backup")
        );
        assert_eq!(
            pod_env(archive, "receive", "PGSSLMODE")["valueFrom"]["configMapKeyRef"]["key"]
                .as_str(),
            Some("sslmode")
        );
        assert_eq!(
            pod_env(archive, "upload", "BACKUP_BUCKET")["valueFrom"]["configMapKeyRef"]["name"]
                .as_str(),
            Some("navigator-postgres-backup-target"),
            "the bucket is an operator-provisioned coordinate, never committed"
        );

        let base = resource(&resources, "CronJob", "navigator-postgres-base-backup");
        assert_eq!(base["spec"]["timeZone"].as_str(), Some("Etc/UTC"));
        assert_eq!(base["spec"]["concurrencyPolicy"].as_str(), Some("Forbid"));
        assert_eq!(
            pod_env(base, "basebackup", "PGUSER")["value"].as_str(),
            Some("navigator_backup")
        );
        assert_eq!(
            pod_env(base, "upload", "BACKUP_BUCKET")["valueFrom"]["configMapKeyRef"]["name"]
                .as_str(),
            Some("navigator-postgres-backup-target")
        );
        assert!(job_spec(base)["activeDeadlineSeconds"].as_u64().is_some());

        let scripts =
            &resource(&resources, "ConfigMap", "navigator-postgres-backup-scripts")["data"];
        let receive = scripts["receive-wal.sh"].as_str().expect("receive-wal.sh");
        assert!(receive.contains("--slot=navigator_wal_archive"));
        assert!(receive.contains("--synchronous"));
        let base_script = scripts["base-backup.sh"].as_str().expect("base-backup.sh");
        assert!(base_script.contains("--wal-method=stream"));
        let upload = scripts["upload-base-backup.sh"]
            .as_str()
            .expect("upload-base-backup.sh");
        assert!(
            upload
                .trim_end()
                .lines()
                .rev()
                .nth(1)
                .is_some_and(|line| line.contains("_COMPLETE")),
            "the completion marker is written after every backup file"
        );
        for resource in &resources {
            let text = serde_yaml::to_string(resource).expect("resource serializes");
            assert!(
                !text.contains("gs://") || text.contains("${BACKUP_BUCKET}"),
                "no bucket name may be committed"
            );
        }
    }
}

#[test]
fn postgres_restore_drill_recovers_from_backups_without_touching_the_live_server() {
    for (environment, namespace) in POSTGRES_OVERLAYS {
        let Some(resources) = render(&format!(
            "examples/deploy/k8s/gke/postgres/restore-drill/{environment}"
        )) else {
            return;
        };
        let drill = resource(&resources, "Job", "navigator-postgres-restore-drill");
        assert_eq!(drill["metadata"]["namespace"].as_str(), Some(namespace));
        assert!(
            drill["spec"]["template"]["metadata"]["labels"]
                ["navigator.neonlaw.org/postgres-client"]
                .is_null(),
            "the drill must not be admitted to the live server"
        );
        assert_eq!(drill["spec"]["backoffLimit"].as_u64(), Some(0));
        assert_eq!(
            pod_env(drill, "fetch", "RESTORE_BASE_BACKUP")["value"].as_str(),
            Some("latest")
        );
        let fetch = pod_containers(drill)
            .into_iter()
            .find(|container| container["name"].as_str() == Some("fetch"))
            .expect("fetch container");
        assert_eq!(
            fetch["securityContext"]["runAsUser"].as_u64(),
            Some(1000),
            "gcloud downloads require its cloudsdk passwd entry"
        );
        assert_eq!(
            pod_env(drill, "restore", "RECOVERY_TARGET_TIME")["value"].as_str(),
            Some("")
        );
        let data = &resource(&resources, "ConfigMap", "navigator-postgres-restore-drill")["data"];
        let restore = data["restore.sh"].as_str().expect("restore.sh");
        for needle in [
            "recovery.signal",
            "restore_command",
            "recovery_target_action = 'promote'",
            "listen_addresses=''",
            "recovery_seconds=",
            "--dbname=forgejo",
        ] {
            assert!(restore.contains(needle), "restore.sh must keep `{needle}`");
        }
        let verify = data["verify.sql"].as_str().expect("verify.sql");
        let fetch = data["fetch-backup.sh"].as_str().expect("fetch-backup.sh");
        assert!(fetch.contains("chmod -R g+rX /restore/base /restore/wal"));
        assert!(fetch.contains("chmod g+w /restore/wal"));
        assert!(verify.contains("pg_last_xact_replay_timestamp()"));
        assert!(verify.contains("ag_catalog.ag_graph"));
        for needle in [
            "nspname = 'dbos'",
            "FROM dbos.workflow_status",
            "GROUP BY status, application_version",
            "GROUP BY executor_id, application_version",
        ] {
            assert!(
                verify.contains(needle),
                "verify.sql must report DBOS durable state: `{needle}`"
            );
        }
    }
}

/// Production must render exactly what staging renders, apart from nothing:
/// every hosted row runs the same PostgreSQL, data claim size included, so the
/// staging rollout rehearses the production one byte for byte.
#[test]
fn postgres_production_overlays_render_the_staging_shape() {
    for overlay in [
        "examples/deploy/k8s/gke/postgres/{}",
        "examples/deploy/k8s/gke/postgres/migrate/{}",
        "examples/deploy/k8s/gke/postgres/mirror/{}",
        "examples/deploy/k8s/gke/postgres/restore-drill/{}",
    ] {
        let (Some(staging), Some(production)) = (
            render(&overlay.replace("{}", "staging")),
            render(&overlay.replace("{}", "production")),
        ) else {
            return;
        };
        assert_eq!(
            rendered_text(&production),
            rendered_text(&staging),
            "{overlay}: production must match staging"
        );
    }
}

/// The 100 GiB data claim is defined once, in the base `StatefulSet`. A hosted
/// overlay that patched the size again would let rows drift apart.
#[test]
fn postgres_data_claim_size_is_defined_only_in_the_base() {
    let base = std::fs::read_to_string(workspace().join("k8s/postgres/postgres.yaml"))
        .expect("read the base PostgreSQL StatefulSet");
    assert_eq!(base.matches("storage: 100Gi").count(), 1);
    for environment in ["staging", "production"] {
        let overlay = std::fs::read_to_string(workspace().join(format!(
            "examples/deploy/k8s/gke/postgres/{environment}/kustomization.yaml"
        )))
        .expect("read the hosted PostgreSQL overlay");
        assert!(
            !overlay.contains("/resources/requests/storage"),
            "the {environment} overlay must not patch the data claim size"
        );
    }
}

#[test]
fn postgres_operational_scripts_parse() {
    for (shell, script) in [
        ("sh", "k8s/postgres/backup/scripts/receive-wal.sh"),
        ("bash", "k8s/postgres/backup/scripts/upload-wal.sh"),
        ("sh", "k8s/postgres/backup/scripts/base-backup.sh"),
        ("bash", "k8s/postgres/backup/scripts/upload-base-backup.sh"),
        ("bash", "k8s/postgres/restore-drill/scripts/fetch-backup.sh"),
        ("sh", "k8s/postgres/restore-drill/scripts/restore.sh"),
    ] {
        let status = Command::new(shell)
            .args(["-n", script])
            .current_dir(workspace())
            .status()
            .unwrap_or_else(|error| panic!("run {shell} -n {script}: {error}"));
        assert!(status.success(), "{script} must parse under {shell}");
    }
}

#[test]
fn forgejo_overlays_keep_oidc_coordinates_as_required_substitutions_and_actions_off() {
    for (environment, namespace) in [("staging", "navigator"), ("production", "navigator")] {
        let Some(resources) = render(&format!("examples/deploy/k8s/gke/forgejo/{environment}"))
        else {
            return;
        };
        let oidc = resource(&resources, "ConfigMap", "navigator-forgejo-oidc");
        assert_eq!(oidc["metadata"]["namespace"].as_str(), Some(namespace));
        assert_eq!(
            oidc["data"]["name"].as_str(),
            Some("YOUR_FORGEJO_OIDC_NAME"),
            "{environment}"
        );
        assert_eq!(
            oidc["data"]["discovery_url"].as_str(),
            Some("YOUR_FORGEJO_OIDC_DISCOVERY_URL"),
            "{environment}"
        );
        let admission = resources
            .iter()
            .find(|resource| {
                resource["kind"].as_str() == Some("ConfigMap")
                    && resource["metadata"]["name"]
                        .as_str()
                        .is_some_and(|name| name.starts_with("navigator-forgejo-admission"))
            })
            .unwrap_or_else(|| panic!("{environment}: admission ConfigMap is generated"));
        assert_eq!(
            admission["data"]["email_domain_allowlist"].as_str(),
            Some("YOUR_FORGEJO_ADMISSION_DOMAINS"),
            "{environment}"
        );
        assert!(
            oidc["data"]["client_secret"].is_null() && oidc["data"]["client_id"].is_null(),
            "{environment}: the client is environment-owned, never committed"
        );
        let database = resource(&resources, "Job", "navigator-forgejo-database");
        assert!(secret_volumes(database).contains(&"navigator-forgejo-database"));
        assert!(
            !deployment_names(&resources).contains(&"navigator-forgejo-runner"),
            "{environment}: Actions stays off until runner isolation is proven"
        );
        let forgejo = resource(&resources, "StatefulSet", "navigator-forgejo");
        let init: Vec<&str> = forgejo["spec"]["template"]["spec"]["initContainers"]
            .as_sequence()
            .into_iter()
            .flatten()
            .filter_map(|container| container["name"].as_str())
            .collect();
        assert_eq!(init, vec!["admission", "identity"], "{environment}");
    }
}

#[test]
fn kind_forgejo_mounts_its_signing_key_where_settings_load_in_init_order() {
    let Some(resources) = render("k8s/overlays/kind/forgejo") else {
        return;
    };
    let forgejo = resource(&resources, "StatefulSet", "navigator-forgejo");
    assert!(secret_volumes(forgejo).contains(&"navigator-forgejo-signing"));
    let spec = &forgejo["spec"]["template"]["spec"];
    let init: Vec<&Value> = spec["initContainers"]
        .as_sequence()
        .expect("init containers")
        .iter()
        .collect();
    let names: Vec<&str> = init.iter().filter_map(|c| c["name"].as_str()).collect();
    // admission renders app.ini before identity loads it.
    assert_eq!(names, ["rauthy-loopback-proxy", "admission", "identity"]);
    let mounts_signing = |container: &Value| {
        container["volumeMounts"]
            .as_sequence()
            .into_iter()
            .flatten()
            .any(|mount| {
                mount["name"].as_str() == Some("signing")
                    && mount["mountPath"].as_str() == Some("/etc/forgejo-signing")
                    && mount["readOnly"].as_bool() == Some(true)
            })
    };
    assert!(mounts_signing(init[2]), "identity loads Forgejo's settings");
    assert!(mounts_signing(container(forgejo, "forgejo")));
    assert!(!mounts_signing(init[1]), "admission only renders app.ini");
}

#[test]
fn forgejo_actions_document_oidc_trust_is_optional_and_has_no_manifest_defaults() {
    let expected = [
        ("NAVIGATOR_FORGEJO_ACTIONS_OIDC_ISSUER", "issuer"),
        ("NAVIGATOR_FORGEJO_ACTIONS_OIDC_AUDIENCE", "audience"),
        ("NAVIGATOR_FORGEJO_ACTIONS_OIDC_WORKFLOW", "workflow"),
    ];
    for (environment, path) in [
        ("base KIND", KIND),
        ("provider-neutral staging", "k8s/staging"),
        ("GKE production", GKE),
    ] {
        let Some(resources) = render(path) else {
            return;
        };
        let web = resource(&resources, "Deployment", "navigator-web");
        for (variable, key) in expected {
            let entry = env_entry(web, variable);
            let reference = &entry["valueFrom"]["configMapKeyRef"];
            assert_eq!(
                reference["name"].as_str(),
                Some("navigator-forgejo-actions-oidc"),
                "{environment}: {variable} must use the deployment-owned trust map"
            );
            assert_eq!(
                reference["key"].as_str(),
                Some(key),
                "{environment}: {variable}"
            );
            assert_eq!(
                reference["optional"].as_bool(),
                Some(true),
                "{environment}: an absent map must leave the verifier disabled"
            );
            assert!(
                entry["value"].is_null(),
                "{environment}: never commit a trusted Forgejo value"
            );
        }
        assert!(
            resources.iter().all(|resource| {
                resource["kind"].as_str() != Some("ConfigMap")
                    || resource["metadata"]["name"].as_str()
                        != Some("navigator-forgejo-actions-oidc")
            }),
            "{environment}: trust values belong to deployment configuration"
        );
    }
}

/// The shared GKE Ingress serves Forgejo's host from the Service and port the
/// Forgejo overlay actually renders, behind its own certificate and a health
/// check on Forgejo's probe path. Staging and production differ only in
/// namespace, so the staging rollout rehearses the production one.
#[test]
fn gke_ingress_routes_the_forgejo_host_to_the_forgejo_service() {
    let Some(application) = render(GKE) else {
        return;
    };
    let ingress = resource(&application, "Ingress", "navigator-web-gke");
    let rule = ingress["spec"]["rules"]
        .as_sequence()
        .expect("Ingress has host rules")
        .iter()
        .find(|rule| rule["host"].as_str() == Some("NAVIGATOR_FORGEJO_HOST"))
        .expect("the Forgejo host has an Ingress rule");
    let paths = rule["http"]["paths"].as_sequence().expect("rule has paths");
    assert_eq!(paths.len(), 1, "the whole Forgejo host goes to Forgejo");
    assert_eq!(paths[0]["path"].as_str(), Some("/*"));
    let backend = &paths[0]["backend"]["service"];
    assert!(
        ingress["metadata"]["annotations"]["networking.gke.io/managed-certificates"]
            .as_str()
            .is_some_and(|names| names.split(',').any(|name| name == "navigator-forgejo")),
        "the Ingress attaches the Forgejo certificate"
    );
    let certificate = resource(&application, "ManagedCertificate", "navigator-forgejo");
    assert_eq!(
        certificate["spec"]["domains"][0].as_str(),
        Some("NAVIGATOR_FORGEJO_HOST")
    );
    let health =
        &resource(&application, "BackendConfig", "navigator-forgejo")["spec"]["healthCheck"];
    assert_eq!(health["requestPath"].as_str(), Some("/api/healthz"));

    let (Some(staging), Some(production)) = (
        render("examples/deploy/k8s/gke/forgejo/staging"),
        render("examples/deploy/k8s/gke/forgejo/production"),
    ) else {
        return;
    };
    let service = resource(&staging, "Service", "navigator-forgejo");
    assert_eq!(backend["name"].as_str(), Some("navigator-forgejo"));
    assert_eq!(
        backend["port"]["number"], service["spec"]["ports"][0]["port"],
        "the Ingress targets the port the Forgejo Service exposes"
    );
    assert_eq!(
        service["metadata"]["annotations"]["cloud.google.com/neg"].as_str(),
        Some(r#"{"ingress": true}"#),
        "a ClusterIP Service is a GCE Ingress backend only as a NEG"
    );
    assert_eq!(
        service["metadata"]["annotations"]["cloud.google.com/backend-config"].as_str(),
        Some(r#"{"default": "navigator-forgejo"}"#)
    );
    let forgejo = container(
        resource(&staging, "StatefulSet", "navigator-forgejo"),
        "forgejo",
    );
    assert_eq!(
        health["port"], forgejo["ports"][0]["containerPort"],
        "the load balancer probes the container port its NEG targets"
    );
    assert_eq!(
        health["requestPath"], forgejo["readinessProbe"]["httpGet"]["path"],
        "the load balancer and Kubernetes agree on readiness"
    );
    assert_eq!(
        rendered_text(&production),
        rendered_text(&staging),
        "Forgejo production must match staging apart from namespace"
    );
}

#[test]
fn seaweedfs_pins_authenticated_s3_and_keeps_internal_services_on_loopback() {
    let Some(resources) = render(KIND_DEPS) else {
        return;
    };
    let store = resource(&resources, "StatefulSet", "seaweedfs");
    let container = container(store, "seaweedfs");
    assert_eq!(container["image"], "chrislusf/seaweedfs:4.48");
    let args = container["args"].as_sequence().unwrap();
    for required in [
        "-ip=127.0.0.1",
        "-ip.bind=127.0.0.1",
        "-s3.ip.bind=0.0.0.0",
        "-s3.config=/run/secrets/seaweedfs/s3.json",
        "-s3.port.iceberg=0",
        "-s3.port.lance=0",
    ] {
        assert!(
            args.iter().any(|arg| arg.as_str() == Some(required)),
            "missing {required}"
        );
    }
    let service = resource(&resources, "Service", "navigator-seaweedfs");
    let ports = service["spec"]["ports"].as_sequence().unwrap();
    assert_eq!(ports.len(), 1);
    assert_eq!(ports[0]["port"], 8333);
    let secret = &store["spec"]["template"]["spec"]["volumes"][0]["secret"];
    assert_eq!(secret["secretName"], "navigator-seaweedfs-s3");
    assert_eq!(secret["items"][0]["key"], "s3.json");
}

/// The PostgreSQL image is built here, published weekly, and dogfooded: the
/// publish is followed by a pull request moving every pin to the new digest,
/// and the compatibility gate tests this directory rather than another
/// repository's checkout.
#[test]
fn postgres_image_is_published_weekly_from_this_repository_and_pinned_back() {
    let read = |path: &str| {
        std::fs::read_to_string(workspace().join(path))
            .unwrap_or_else(|error| panic!("read {path}: {error}"))
    };
    let publish = read(".github/workflows/postgres-image.yml");
    let workflow: Value = serde_yaml::from_str(&publish).expect("postgres-image.yml is YAML");
    let schedule = workflow["on"]["schedule"][0]["cron"]
        .as_str()
        .expect("postgres-image.yml runs on a schedule");
    let fields: Vec<&str> = schedule.split_whitespace().collect();
    assert_eq!(fields.len(), 5, "cron {schedule}");
    assert_eq!(&fields[2..4], ["*", "*"], "weekly, not monthly: {schedule}");
    assert!(fields[4].parse::<u8>().is_ok(), "one weekday: {schedule}");
    for required in [
        "context: images/postgres",
        "REGISTRY: ghcr.io/${{ github.repository_owner }}/neon-law-postgres",
        "bash tests/run.sh",
        "cosign sign --yes",
        "scripts/postgres-image-pin.sh \"$REFERENCE\"",
        "actions/create-github-app-token",
    ] {
        assert!(
            publish.contains(required),
            "postgres-image.yml lost `{required}`"
        );
    }

    let compatibility = read(".github/workflows/postgres-compatibility.yml");
    assert!(
        !compatibility.contains("repository:"),
        "the compatibility gate must test images/postgres, not another repository's checkout"
    );
    assert!(compatibility.contains("images/postgres/tests/dbos/Cargo.toml"));

    let tests = workspace().join("images/postgres/tests");
    for file in [
        "run.sh",
        "extensions.sql",
        "persistence.sql",
        "dbos/Cargo.lock",
    ] {
        assert!(tests.join(file).is_file(), "images/postgres/tests/{file}");
    }
    assert!(
        read("images/postgres/Containerfile")
            .contains("org.opencontainers.image.source=\"https://github.com/neon-law/navigator\""),
        "the published image must link back to this repository"
    );
}

//! The synthetic KIND PostgreSQL connection and first-enrollment contract.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};
use sqlx::postgres::{PgConnectOptions, PgSslMode};

use super::KindConfig;

/// Kubernetes terminal conditions, independent of a retriable failed Pod count.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::devx) enum JobState {
    Pending,
    Complete,
    Failed,
}

pub(in crate::devx) fn job_state(job: &serde_json::Value) -> Result<JobState> {
    let Some(conditions) = job.pointer("/status/conditions") else {
        return Ok(JobState::Pending);
    };
    let conditions = conditions
        .as_array()
        .context("Job conditions are not an array")?;
    let has = |kind| {
        conditions
            .iter()
            .any(|c| c["type"] == kind && c["status"] == "True")
    };
    match (has("Complete"), has("Failed")) {
        (true, false) => Ok(JobState::Complete),
        (false, true) => Ok(JobState::Failed),
        (false, false) => Ok(JobState::Pending),
        (true, true) => anyhow::bail!("Job reports conflicting terminal conditions"),
    }
}

/// Retain existing TLS material; older local fixtures trust their leaf directly.
pub(in crate::devx) fn trust_anchor(tls: &BTreeMap<String, String>) -> Result<&str> {
    ensure!(
        tls.get("tls.key").is_some_and(|key| !key.is_empty()),
        "existing PostgreSQL TLS key is missing"
    );
    let leaf = tls
        .get("tls.crt")
        .filter(|cert| !cert.is_empty())
        .context("existing PostgreSQL TLS certificate is missing")?;
    let ca = tls.get("ca.crt").unwrap_or(leaf);
    ensure!(!ca.is_empty(), "existing PostgreSQL TLS root is empty");
    Ok(ca)
}

// Deliberately no Debug: these values never belong in command diagnostics.
pub(in crate::devx) struct Credentials {
    pub app_password: String,
    pub owner_password: String,
    pub ca: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct HeldWriters {
    context: String,
    namespace: String,
    namespace_uid: String,
    pub replicas: BTreeMap<String, u32>,
}

impl HeldWriters {
    pub(super) fn new(
        cfg: &KindConfig,
        namespace_uid: &str,
        replicas: BTreeMap<String, u32>,
    ) -> Self {
        Self {
            context: cfg.kind_context(),
            namespace: cfg.namespace.clone(),
            namespace_uid: namespace_uid.into(),
            replicas,
        }
    }
    pub(super) fn validate(&self, cfg: &KindConfig, namespace_uid: &str) -> Result<()> {
        ensure!(
            self.context == cfg.kind_context()
                && self.namespace == cfg.namespace
                && !namespace_uid.is_empty()
                && self.namespace_uid == namespace_uid,
            "held PostgreSQL fixture writers belong to another KIND target"
        );
        ensure!(
            self.replicas.keys().all(|name| matches!(
                name.as_str(),
                "deployment/navigator-web" | "restatedeployment/workflows-service"
            )),
            "held PostgreSQL fixture contains an unsupported writer"
        );
        Ok(())
    }
}

/// The host must reach this KIND node directly, without a pooled stream tunnel.
pub(super) fn validate_published_port(ports: &serde_json::Value, expected: u16) -> Result<()> {
    ensure!(
        ports["31532/tcp"].as_array().is_some_and(|bindings| {
            bindings.len() == 1
                && bindings[0]["HostIp"].as_str() == Some("127.0.0.1")
                && bindings[0]["HostPort"].as_str() == Some(expected.to_string().as_str())
        }),
        "KIND PostgreSQL requires its task-owned loopback mapping; recreate only this synthetic tier with the current KIND configuration"
    );
    Ok(())
}

/// Admit this fixture's node-facing transport; TLS and database roles still authenticate clients.
pub(super) fn host_ingress(
    cfg: &KindConfig,
    node: &serde_json::Value,
    container: &serde_json::Value,
) -> Result<serde_json::Value> {
    validate_published_port(&container["NetworkSettings"]["Ports"], cfg.postgres_port)?;
    let name = format!("{}-control-plane", cfg.cluster);
    ensure!(
        node["metadata"]["name"].as_str() == Some(name.as_str())
            && container["Name"].as_str() == Some(format!("/{name}").as_str())
            && container["Config"]["Labels"]["io.x-k8s.kind.cluster"].as_str()
                == Some(cfg.cluster.as_str())
            && container["Config"]["Labels"]["io.x-k8s.kind.role"].as_str()
                == Some("control-plane"),
        "PostgreSQL fixture ingress requires its verified KIND control-plane target"
    );
    let mut ips = Vec::new();
    for address in node["status"]["addresses"]
        .as_array()
        .context("fixture node omitted its addresses")?
    {
        if address["type"].as_str() == Some("InternalIP") {
            let ip: std::net::IpAddr = address["address"]
                .as_str()
                .context("fixture node address is invalid")?
                .parse()
                .context("fixture node address is invalid")?;
            if let std::net::IpAddr::V4(ip) = ip {
                ips.push(ip);
            }
        }
    }
    ensure!(
        ips.len() == 1 && ips[0].is_private(),
        "fixture node requires one private InternalIPv4"
    );
    let ip = ips[0];
    let networks = container["NetworkSettings"]["Networks"]
        .as_object()
        .context("fixture Docker node omitted its networks")?;
    ensure!(
        networks.values().any(|network| network["IPAddress"]
            .as_str()
            .and_then(|value| value.parse::<std::net::Ipv4Addr>().ok())
            == Some(ip)),
        "fixture node address does not match its owned Docker target"
    );
    Ok(serde_json::json!({
        "apiVersion":"networking.k8s.io/v1", "kind":"NetworkPolicy",
        "metadata":{"name":"navigator-postgres-fixture-host","namespace":cfg.namespace},
        "spec":{
            "podSelector":{"matchLabels":{"app":"navigator-postgres"}},
            "policyTypes":["Ingress"],
            "ingress":[{"from":[{"ipBlock":{"cidr":format!("{ip}/32")}}],
                "ports":[{"protocol":"TCP","port":5432}]}]
        }
    }))
}

pub(super) fn ca_path(root: &Path) -> PathBuf {
    root.join(".devx/postgres/ca.crt")
}

pub(super) fn environment_path(root: &Path) -> PathBuf {
    root.join(".devx/postgres/env")
}

impl Credentials {
    pub(super) fn options(&self, cfg: &KindConfig, root: &Path) -> PgConnectOptions {
        PgConnectOptions::new()
            .host("127.0.0.1")
            .port(cfg.postgres_port)
            .database("navigator")
            .username("navigator_owner")
            .password(&self.owner_password)
            .ssl_mode(PgSslMode::VerifyFull)
            .ssl_root_cert(ca_path(root))
    }

    pub(super) fn environment(&self, cfg: &KindConfig, root: &Path) -> BTreeMap<String, String> {
        self.environment_for(cfg, root, "navigator")
    }

    pub(super) fn environment_for(
        &self,
        cfg: &KindConfig,
        root: &Path,
        database: &str,
    ) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("NAVIGATOR_POSTGRES_HOST".into(), "127.0.0.1".into()),
            (
                "NAVIGATOR_POSTGRES_PORT".into(),
                cfg.postgres_port.to_string(),
            ),
            ("NAVIGATOR_POSTGRES_DATABASE".into(), database.into()),
            ("NAVIGATOR_POSTGRES_USER".into(), "navigator_app".into()),
            (
                "NAVIGATOR_POSTGRES_PASSWORD".into(),
                self.app_password.clone(),
            ),
            ("NAVIGATOR_POSTGRES_SSLMODE".into(), "verify-full".into()),
            (
                "NAVIGATOR_POSTGRES_ROOT_CERT".into(),
                ca_path(root).display().to_string(),
            ),
        ])
    }
}

/// A missing receipt is never permission to import an existing source automatically.
pub(super) fn require_fresh_source(counts: &BTreeMap<String, i64>) -> Result<()> {
    ensure!(
        store::schema::table_names().iter().all(|table| counts.contains_key(table)),
        "captured KIND enrollment requires a prepared source schema; use a fresh native KIND worktree for an empty environment or recover the compatibility fixture"
    );
    ensure!(
        counts.iter().all(|(table, count)| {
            *count == 0 || (*count > 0 && matches!(table.as_str(), "schema_version" | "migrations"))
        }),
        "KIND PostgreSQL enrollment requires a fresh source; preserve existing history with the explicit frozen-source backfill procedure"
    );
    Ok(())
}

fn writer_stop_request_observed(state: &serde_json::Value) -> bool {
    let Some(generation) = state["metadata"]["generation"].as_u64() else {
        return false;
    };
    state["spec"]["replicas"].as_u64() == Some(0)
        && state["status"]["observedGeneration"]
            .as_u64()
            .is_some_and(|seen| seen >= generation)
}

pub(super) fn writer_observed_stopped(state: &serde_json::Value) -> bool {
    writer_stop_request_observed(state) && state["status"]["replicas"].as_u64().unwrap_or(0) == 0
}

fn owned_writer_uids(
    root: &str,
    workloads: &[serde_json::Value],
) -> std::collections::BTreeSet<String> {
    let mut owned = std::collections::BTreeSet::from([root.to_owned()]);
    loop {
        let previous = owned.len();
        let descendants = workloads
            .iter()
            .filter(|workload| has_writer_owner(workload, &owned))
            .filter_map(|workload| workload["metadata"]["uid"].as_str())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        owned.extend(descendants);
        if owned.len() == previous {
            return owned;
        }
    }
}

fn has_writer_owner(state: &serde_json::Value, owned: &std::collections::BTreeSet<String>) -> bool {
    state["metadata"]["ownerReferences"]
        .as_array()
        .is_some_and(|owners| {
            owners
                .iter()
                .any(|owner| owner["uid"].as_str().is_some_and(|uid| owned.contains(uid)))
        })
}

fn stopped_controlled_workload(
    workload: &serde_json::Value,
    root: &str,
    workloads: &[serde_json::Value],
    lineage: &std::collections::BTreeSet<String>,
) -> bool {
    let Some(owners) = workload["metadata"]["ownerReferences"].as_array() else {
        return false;
    };
    let mut controllers = owners
        .iter()
        .filter(|owner| owner["controller"].as_bool() == Some(true));
    let Some(owner) = controllers.next() else {
        return false;
    };
    if controllers.next().is_some() {
        return false;
    }
    let Some(owner_uid) = owner["uid"].as_str() else {
        return false;
    };
    let linked = match (workload["kind"].as_str(), owner["kind"].as_str()) {
        (Some("Deployment" | "ReplicaSet"), Some("RestateDeployment")) => owner_uid == root,
        (Some("ReplicaSet"), Some("Deployment")) => {
            lineage.contains(owner_uid)
                && workloads.iter().any(|parent| {
                    parent["kind"] == "Deployment"
                        && parent["metadata"]["uid"].as_str() == Some(owner_uid)
                })
        }
        _ => false,
    };
    linked
        && writer_observed_stopped(workload)
        && workload["status"]["replicas"].as_u64() == Some(0)
        && workload["status"]
            .get("terminatingReplicas")
            .is_none_or(|value| value.as_u64() == Some(0))
}

/// Restate's aggregate counters can remain stale after its admin API rejects discovery.
/// Prove quiescence from the observed request and every UID-linked controller workload.
pub(super) fn restate_writer_observed_stopped(
    state: &serde_json::Value,
    workloads: &serde_json::Value,
    pods: &serde_json::Value,
) -> bool {
    if state["kind"] != "RestateDeployment" || !writer_stop_request_observed(state) {
        return false;
    }
    let (Some(uid), Some(name), Some(workloads), Some(pods)) = (
        state["metadata"]["uid"]
            .as_str()
            .filter(|uid| !uid.is_empty()),
        state["metadata"]["name"]
            .as_str()
            .filter(|name| !name.is_empty()),
        workloads["items"].as_array(),
        pods["items"].as_array(),
    ) else {
        return false;
    };
    let owned = owned_writer_uids(uid, workloads);
    let belongs = |value: &serde_json::Value| {
        value["metadata"]["labels"]["app"].as_str() == Some(name) || has_writer_owner(value, &owned)
    };
    let mut observed_workload = false;
    for workload in workloads.iter().filter(|workload| belongs(workload)) {
        observed_workload = true;
        if !workload["metadata"]["uid"]
            .as_str()
            .is_some_and(|id| !id.is_empty() && owned.contains(id))
            || !stopped_controlled_workload(workload, uid, workloads, &owned)
        {
            return false;
        }
    }
    (observed_workload || state["status"]["replicas"].as_u64() == Some(0))
        && !pods.iter().any(belongs)
}

/// Validate the fixture's advertised origin against its actual local workloads.
pub(super) fn validate_store_identity(
    config: &serde_json::Value,
    service: &serde_json::Value,
    deployment: &serde_json::Value,
    namespace: &str,
    database: &str,
    app: &str,
    port: u16,
) -> Result<()> {
    ensure!(
        config["data"]["endpoint"].as_str()
            == Some(format!("ws://{app}.{namespace}.svc.cluster.local:{port}").as_str())
            && config["data"]["namespace"].as_str() == Some(namespace)
            && config["data"]["database"].as_str() == Some(database),
        "fixture store coordinates do not name its task-owned service"
    );
    validate_local_service(service, app)?;
    ensure!(
        deployment["spec"]["template"]["spec"]["containers"][0]["args"]
            .as_array()
            .is_some_and(|args| args.last().and_then(serde_json::Value::as_str) == Some("memory")),
        "fixture history capture requires its disposable memory-backed workload"
    );
    Ok(())
}

pub(super) fn validate_local_service(service: &serde_json::Value, app: &str) -> Result<()> {
    ensure!(
        service["spec"]["externalName"].is_null()
            && service["spec"]["selector"]["app"].as_str() == Some(app),
        "fixture service must select its own local workload"
    );
    Ok(())
}

pub(super) fn render_environment(values: &BTreeMap<String, String>) -> String {
    let mut rendered = String::new();
    for (key, value) in values {
        rendered.push_str(key);
        rendered.push('=');
        rendered.push_str(&super::shell_single_quote(value));
        rendered.push('\n');
    }
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_mapping_refuses_missing_shared_or_redirected_bindings() {
        let valid = serde_json::json!({"31532/tcp":[{"HostIp":"127.0.0.1","HostPort":"21503"}]});
        validate_published_port(&valid, 21503).unwrap();
        assert!(validate_published_port(&valid, 21504).is_err());
        assert!(validate_published_port(&serde_json::json!({}), 21503).is_err());
        for host in ["0.0.0.0", "::", "192.0.2.1"] {
            let redirected = serde_json::json!({"31532/tcp":[{"HostIp":host,"HostPort":"21503"}]});
            assert!(validate_published_port(&redirected, 21503).is_err());
        }
    }

    #[test]
    fn host_ingress_is_bound_to_the_owned_node_and_one_database_port() {
        let mut cfg = KindConfig::from_env();
        cfg.cluster = "fixture-owner".into();
        cfg.namespace = "navigator".into();
        cfg.postgres_port = 21503;
        let mut node = serde_json::json!({"metadata":{"name":"fixture-owner-control-plane"},
            "status":{"addresses":[{"type":"InternalIP","address":"192.168.49.2"}]}});
        let mut container = serde_json::json!({
            "Name":"/fixture-owner-control-plane",
            "Config":{"Labels":{"io.x-k8s.kind.cluster":"fixture-owner","io.x-k8s.kind.role":"control-plane"}},
            "NetworkSettings":{
                "Ports":{"31532/tcp":[{"HostIp":"127.0.0.1","HostPort":"21503"}]},
                "Networks":{"kind":{"IPAddress":"192.168.49.2"}}
            }
        });
        let policy = host_ingress(&cfg, &node, &container).unwrap();
        assert_eq!(policy["metadata"]["namespace"], "navigator");
        assert_eq!(
            policy["spec"]["policyTypes"],
            serde_json::json!(["Ingress"])
        );
        assert_eq!(
            policy["spec"]["ingress"],
            serde_json::json!([{
                "from":[{"ipBlock":{"cidr":"192.168.49.2/32"}}],
                "ports":[{"protocol":"TCP","port":5432}]
            }])
        );
        for invalid in [
            "not-an-ip",
            "192.168.49.3",
            "192.0.2.1",
            "127.0.0.1",
            "0.0.0.0",
            "192.168.49.2/32",
        ] {
            node["status"]["addresses"][0]["address"] = serde_json::json!(invalid);
            assert!(host_ingress(&cfg, &node, &container).is_err());
        }
        node["status"]["addresses"][0]["address"] = serde_json::json!("192.168.49.2");
        node["metadata"]["name"] = serde_json::json!("another-control-plane");
        assert!(host_ingress(&cfg, &node, &container).is_err());
        node["metadata"]["name"] = serde_json::json!("fixture-owner-control-plane");
        container["Config"]["Labels"]["io.x-k8s.kind.cluster"] = serde_json::json!("another-owner");
        assert!(host_ingress(&cfg, &node, &container).is_err());
    }

    #[test]
    fn interrupted_holds_refuse_changed_target_or_namespace_identity() {
        let mut cfg = KindConfig::from_env();
        cfg.cluster = "synthetic-fixture".into();
        cfg.namespace = "navigator".into();
        let held = HeldWriters::new(
            &cfg,
            "original-uid",
            BTreeMap::from([("deployment/navigator-web".into(), 2)]),
        );
        held.validate(&cfg, "original-uid").unwrap();
        assert!(held.validate(&cfg, "replacement-uid").is_err());
        cfg.cluster = "another-fixture".into();
        assert!(held.validate(&cfg, "original-uid").is_err());
        cfg.cluster = "synthetic-fixture".into();
        cfg.namespace = "another-namespace".into();
        assert!(held.validate(&cfg, "original-uid").is_err());
    }

    #[test]
    fn held_writers_never_replay_an_unrecognized_resource() {
        let cfg = KindConfig::from_env();
        let held = HeldWriters::new(
            &cfg,
            "original-uid",
            BTreeMap::from([("deployment/unrelated".into(), 1)]),
        );
        assert!(held.validate(&cfg, "original-uid").is_err());
    }

    #[test]
    fn controller_must_observe_zero_writers_before_capture() {
        let mut state = serde_json::json!({
            "metadata":{"generation":5}, "spec":{"replicas":0},
            "status":{"observedGeneration":5,"replicas":0}
        });
        assert!(writer_observed_stopped(&state));
        state["status"]["observedGeneration"] = serde_json::json!(4);
        assert!(!writer_observed_stopped(&state));
        state["status"]["observedGeneration"] = serde_json::json!(5);
        state["status"]["replicas"] = serde_json::json!(1);
        assert!(!writer_observed_stopped(&state));
        state["status"]["replicas"] = serde_json::json!(0);
        state["spec"]["replicas"] = serde_json::json!(1);
        assert!(!writer_observed_stopped(&state));
    }

    fn held_restate_writer() -> (serde_json::Value, serde_json::Value, serde_json::Value) {
        (
            serde_json::json!({
                "kind":"RestateDeployment",
                "metadata":{"uid":"writer-uid","name":"worker","generation":2},
                "spec":{"replicas":0},
                "status":{"observedGeneration":2,"replicas":1}
            }),
            serde_json::json!({"items":[{
                "kind":"ReplicaSet",
                "metadata":{"uid":"replicaset-uid","generation":3,"labels":{"app":"worker"},
                    "ownerReferences":[{"uid":"writer-uid","kind":"RestateDeployment","controller":true}]},
                "spec":{"replicas":0},
                "status":{"observedGeneration":3,"replicas":0,"terminatingReplicas":0}
            }]}),
            serde_json::json!({"items":[]}),
        )
    }

    #[test]
    fn stale_restate_counts_require_observed_owned_zero_workloads() {
        let (state, workloads, pods) = held_restate_writer();
        assert!(!writer_observed_stopped(&state));
        assert!(restate_writer_observed_stopped(&state, &workloads, &pods));
        assert!(!restate_writer_observed_stopped(
            &state,
            &serde_json::json!({"items":[]}),
            &pods
        ));
        for (pointer, value) in [
            ("/metadata/generation", serde_json::json!(3)),
            ("/metadata/uid", serde_json::Value::Null),
            ("/kind", serde_json::json!("Unknown")),
            ("/spec/replicas", serde_json::json!(1)),
        ] {
            let mut invalid = state.clone();
            *invalid.pointer_mut(pointer).unwrap() = value;
            assert!(!restate_writer_observed_stopped(
                &invalid, &workloads, &pods
            ));
        }
        assert!(!restate_writer_observed_stopped(
            &state,
            &serde_json::json!({}),
            &pods
        ));
        assert!(!restate_writer_observed_stopped(
            &state,
            &workloads,
            &serde_json::json!({})
        ));
    }

    #[test]
    fn restate_capture_refuses_live_terminating_or_unobserved_children() {
        let (state, workloads, pods) = held_restate_writer();
        for (pointer, value) in [
            ("/spec/replicas", serde_json::json!(1)),
            ("/status/replicas", serde_json::json!(1)),
            ("/status/replicas", serde_json::Value::Null),
            ("/status/observedGeneration", serde_json::json!(2)),
            ("/status/terminatingReplicas", serde_json::json!(1)),
            ("/status/terminatingReplicas", serde_json::json!("unknown")),
            ("/kind", serde_json::json!("Unknown")),
        ] {
            let mut invalid = workloads.clone();
            *invalid["items"][0].pointer_mut(pointer).unwrap() = value;
            assert!(!restate_writer_observed_stopped(&state, &invalid, &pods));
        }
    }

    #[test]
    fn restate_capture_refuses_missing_or_foreign_controller_ownership() {
        let (state, workloads, pods) = held_restate_writer();
        for (pointer, value) in [
            ("/metadata/uid", serde_json::Value::Null),
            ("/metadata/ownerReferences", serde_json::json!([])),
            (
                "/metadata/ownerReferences/0/uid",
                serde_json::json!("foreign-uid"),
            ),
            (
                "/metadata/ownerReferences/0/controller",
                serde_json::json!(false),
            ),
            (
                "/metadata/ownerReferences/0/kind",
                serde_json::json!("Unknown"),
            ),
        ] {
            let mut invalid = workloads.clone();
            *invalid["items"][0].pointer_mut(pointer).unwrap() = value;
            assert!(!restate_writer_observed_stopped(&state, &invalid, &pods));
        }
    }

    #[test]
    fn restate_capture_follows_uids_beyond_labels_and_checks_writer_pods() {
        let (state, mut workloads, mut pods) = held_restate_writer();
        workloads["items"][0]["metadata"]["labels"]["app"] = serde_json::json!("other-label");
        assert!(restate_writer_observed_stopped(&state, &workloads, &pods));
        workloads["items"][0]["spec"]["replicas"] = serde_json::json!(1);
        assert!(!restate_writer_observed_stopped(&state, &workloads, &pods));
        workloads["items"][0]["spec"]["replicas"] = serde_json::json!(0);
        pods["items"] =
            serde_json::json!([{"metadata":{"ownerReferences":[{"uid":"replicaset-uid"}]}}]);
        assert!(!restate_writer_observed_stopped(&state, &workloads, &pods));
        pods["items"][0]["metadata"]["deletionTimestamp"] =
            serde_json::json!("2026-01-01T00:00:00Z");
        assert!(!restate_writer_observed_stopped(&state, &workloads, &pods));
        pods["items"] = serde_json::json!([{"metadata":{"labels":{"app":"worker"}}}]);
        assert!(!restate_writer_observed_stopped(&state, &workloads, &pods));
        pods["items"] = serde_json::json!([{"metadata":{"labels":{"app":"unrelated"}}}]);
        assert!(restate_writer_observed_stopped(&state, &workloads, &pods));
    }

    #[test]
    fn restate_capture_checks_deployments_and_transitive_replica_sets() {
        let (state, mut workloads, pods) = held_restate_writer();
        let mut deployment = workloads["items"][0].clone();
        deployment["kind"] = serde_json::json!("Deployment");
        deployment["metadata"]["uid"] = serde_json::json!("deployment-uid");
        workloads["items"][0]["metadata"]["ownerReferences"][0] = serde_json::json!({
            "uid":"deployment-uid","kind":"Deployment","controller":true
        });
        workloads["items"].as_array_mut().unwrap().push(deployment);
        assert!(restate_writer_observed_stopped(&state, &workloads, &pods));
        workloads["items"][1]["status"]["observedGeneration"] = serde_json::json!(2);
        assert!(!restate_writer_observed_stopped(&state, &workloads, &pods));
        workloads["items"][1]["status"]["observedGeneration"] = serde_json::json!(3);
        workloads["items"][1]["metadata"]["ownerReferences"][0]["uid"] =
            serde_json::json!("foreign-uid");
        workloads["items"][1]["metadata"]["labels"]["app"] = serde_json::json!("unrelated");
        workloads["items"][0]["metadata"]["ownerReferences"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "uid":"writer-uid","kind":"RestateDeployment","controller":false
            }));
        assert!(!restate_writer_observed_stopped(&state, &workloads, &pods));
    }

    #[test]
    fn fixture_identity_refuses_redirected_services_and_retained_source() {
        let mut config = serde_json::json!({"data":{
            "endpoint":"ws://fixture-store.navigator.svc.cluster.local:8000",
            "namespace":"navigator","database":"navigator"
        }});
        let mut service = serde_json::json!({"spec":{"selector":{"app":"fixture-store"}}});
        let mut deployment = serde_json::json!({"spec":{"template":{"spec":{
            "containers":[{"args":["start","memory"]}]
        }}}});
        let valid = |c: &serde_json::Value, s: &serde_json::Value, d: &serde_json::Value| {
            validate_store_identity(c, s, d, "navigator", "navigator", "fixture-store", 8000)
        };
        valid(&config, &service, &deployment).unwrap();
        config["data"]["endpoint"] = serde_json::json!("wss://outside.example.invalid");
        assert!(valid(&config, &service, &deployment).is_err());
        config["data"]["endpoint"] =
            serde_json::json!("ws://fixture-store.navigator.svc.cluster.local:8000");
        service["spec"]["externalName"] = serde_json::json!("outside.example.invalid");
        assert!(valid(&config, &service, &deployment).is_err());
        service["spec"]
            .as_object_mut()
            .unwrap()
            .remove("externalName");
        deployment["spec"]["template"]["spec"]["containers"][0]["args"] =
            serde_json::json!(["start", "disk"]);
        assert!(valid(&config, &service, &deployment).is_err());
    }

    #[test]
    fn first_enrollment_refuses_existing_or_invalid_source_history() {
        let mut counts = BTreeMap::new();
        assert!(require_fresh_source(&counts).is_err());
        counts.extend(
            store::schema::table_names()
                .into_iter()
                .map(|name| (name, 0)),
        );
        counts.insert("schema_version".into(), 1);
        counts.remove("citation");
        assert!(require_fresh_source(&counts).is_err());
        counts.insert("citation".into(), 0);
        require_fresh_source(&counts).unwrap();
        counts.insert("citation".into(), 1);
        assert!(require_fresh_source(&counts).is_err());
        counts.insert("citation".into(), -1);
        assert!(require_fresh_source(&counts).is_err());
        counts.insert("citation".into(), 0);
        counts.insert("unknown_source".into(), 1);
        assert!(require_fresh_source(&counts).is_err());
    }

    #[test]
    fn host_fixture_uses_runtime_role_and_tls_on_its_own_port() {
        let mut cfg = KindConfig::from_env();
        cfg.postgres_port = 21541;
        let credentials = Credentials {
            app_password: r#"synthetic app
quote' $HOME `printf forbidden` \ ""#
                .into(),
            owner_password: "synthetic-owner".into(),
            ca: "synthetic-certificate".into(),
        };
        let values = credentials.environment(&cfg, Path::new("/task with spaces"));
        assert_eq!(values["NAVIGATOR_POSTGRES_USER"], "navigator_app");
        assert_eq!(values["NAVIGATOR_POSTGRES_PORT"], "21541");
        assert_eq!(values["NAVIGATOR_POSTGRES_SSLMODE"], "verify-full");
        let rendered = render_environment(&values);
        let parsed: BTreeMap<_, _> = dotenvy::from_read_iter(rendered.as_bytes())
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(parsed, values);
        assert!(!rendered.contains("synthetic-owner"));
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), rendered).unwrap();
        let output = std::process::Command::new("sh")
            .args([
                "-c",
                ". \"$1\"; printf '%s' \"$NAVIGATOR_POSTGRES_PASSWORD\"",
                "fixture",
            ])
            .arg(file.path())
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, credentials.app_password.as_bytes());
    }
    #[test]
    fn jobs_allow_failed_pod_retries_and_use_terminal_conditions() {
        use serde_json::json;
        assert_eq!(job_state(&json!({})).unwrap(), JobState::Pending);
        assert_eq!(
            job_state(&json!({"status":{"failed":1}})).unwrap(),
            JobState::Pending
        );
        assert_eq!(
            job_state(
                &json!({"status":{"failed":1,"conditions":[{"type":"Complete","status":"True"}]}})
            )
            .unwrap(),
            JobState::Complete
        );
        assert_eq!(
            job_state(&json!({"status":{"conditions":[{"type":"Failed","status":"True"}]}}))
                .unwrap(),
            JobState::Failed
        );
        assert!(job_state(&json!({"status":{"conditions":[{"type":"Failed","status":"True"},{"type":"Complete","status":"True"}]}})).is_err());
        assert!(job_state(&json!({"status":{"conditions":{}}})).is_err());
    }

    #[test]
    fn existing_tls_keeps_its_root_or_legacy_leaf_without_rotation() {
        let mut tls = BTreeMap::from([
            ("tls.crt".into(), "legacy-leaf".into()),
            ("tls.key".into(), "private-key".into()),
        ]);
        assert_eq!(trust_anchor(&tls).unwrap(), "legacy-leaf");
        tls.insert("ca.crt".into(), "separate-root".into());
        assert_eq!(trust_anchor(&tls).unwrap(), "separate-root");
        tls.insert("ca.crt".into(), String::new());
        assert!(trust_anchor(&tls).is_err());
        tls.remove("tls.key");
        assert!(trust_anchor(&tls).is_err());
    }
}

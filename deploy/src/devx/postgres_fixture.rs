//! The synthetic KIND PostgreSQL connection and first-enrollment contract.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};

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

    pub ca: String,
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

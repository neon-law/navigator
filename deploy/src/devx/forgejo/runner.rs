//! Repository-scoped, single-job Forgejo runners in disposable Kubernetes pods.

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use anyhow::{ensure, Context, Result};
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::{json, Value};

use super::cluster::Kube;

const DEADLINE_SECS: u64 = 1800;
const RUNNER_IMAGE: &str = "data.forgejo.org/forgejo/runner@sha256:ca3d5eea46004789a175d1369eec6829d3ea9bfbe2011a06625b4bdaa55f7552";

pub(in crate::devx) fn repository_scope(scope: &str) -> Result<()> {
    let components: Vec<_> = scope.split('/').collect();
    ensure!(
        components.len() == 2
            && components.iter().all(|part| {
                !part.is_empty()
                    && *part != "."
                    && *part != ".."
                    && part
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
            }),
        "runner scope must be one explicit owner/repository"
    );
    Ok(())
}

fn public_ipv4(address: &str) -> Result<Ipv4Addr> {
    let ip: Ipv4Addr = address
        .parse()
        .context("egress destination must be one IPv4 address")?;
    let [a, b, c, _] = ip.octets();
    ensure!(
        !ip.is_private() && !ip.is_loopback() && !ip.is_link_local()
            && !ip.is_documentation() && !ip.is_unspecified() && !ip.is_multicast()
            && a != 0 && a < 224 && !(a == 100 && (64..128).contains(&b))
            && !(a == 192 && b == 0 && c == 0)
            && !(a == 192 && b == 88 && c == 99)
            && !(a == 198 && (18..20).contains(&b)),
        "egress destination must be a public IPv4 address; internal and metadata endpoints are refused"
    );
    Ok(ip)
}

/// Every public IPv4 address `host` resolves to now. A name that resolves
/// to any private, metadata or non-unicast address is refused outright rather
/// than filtered, so a poisoned answer cannot quietly widen egress.
fn resolve_public_ipv4(host: &str) -> Result<Vec<Ipv4Addr>> {
    use std::net::ToSocketAddrs as _;
    ensure!(
        !host.is_empty()
            && host
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-.".contains(&c)),
        "egress host must be a DNS name"
    );
    let mut addresses: Vec<Ipv4Addr> = Vec::new();
    for address in (host, 443)
        .to_socket_addrs()
        .with_context(|| format!("resolve egress host {host}"))?
    {
        if let std::net::IpAddr::V4(ip) = address.ip() {
            let ip = public_ipv4(&ip.to_string())?;
            if !addresses.contains(&ip) {
                addresses.push(ip);
            }
        }
    }
    ensure!(
        !addresses.is_empty(),
        "egress host {host} has no IPv4 address"
    );
    Ok(addresses)
}

/// The exact `/32` destinations: explicit addresses plus each named host's
/// current public IPv4 addresses, without duplicates.
pub(in crate::devx) fn egress_destinations(
    ips: &[String],
    hosts: &[String],
) -> Result<Vec<Ipv4Addr>> {
    let mut destinations: Vec<Ipv4Addr> = Vec::new();
    let explicit = ips
        .iter()
        .map(|ip| public_ipv4(ip))
        .collect::<Result<Vec<_>>>()?;
    let resolved = hosts
        .iter()
        .map(|host| resolve_public_ipv4(host))
        .collect::<Result<Vec<_>>>()?;
    for ip in explicit.into_iter().chain(resolved.into_iter().flatten()) {
        if !destinations.contains(&ip) {
            destinations.push(ip);
        }
    }
    Ok(destinations)
}

fn dns_ipv4(address: &str) -> Result<Ipv4Addr> {
    let ip: Ipv4Addr = address
        .parse()
        .context("cluster DNS must be one IPv4 address")?;
    ensure!(
        !ip.is_unspecified()
            && !ip.is_loopback()
            && !ip.is_multicast()
            && ip.octets()[0] != 0
            && ip.octets()[0] < 224
            && ip != Ipv4Addr::new(169, 254, 169, 254)
            && ip != Ipv4Addr::new(169, 254, 169, 252),
        "cluster DNS must be a trusted unicast address, never a metadata endpoint"
    );
    Ok(ip)
}

/// The Job annotations recovery reads: which repository-scoped registration
/// this Job holds. A numeric ID and a repository name, never a credential.
pub(in crate::devx) const REGISTRATION_ANNOTATION: &str =
    "navigator.neonlaw.org/runner-registration";
pub(in crate::devx) const REPOSITORY_ANNOTATION: &str = "navigator.neonlaw.org/runner-repository";
/// The label every disposable runner Job carries, so recovery lists exactly
/// these Jobs and nothing else in the namespace.
pub(in crate::devx) const RUNNER_JOB_LABEL: &str = "navigator.neonlaw.org/forgejo-runner";

fn job(name: &str, namespace: &str, secret: &str, repository: &str, registration: u64) -> Value {
    json!({
        "apiVersion": "batch/v1", "kind": "Job",
        "metadata": {
            "name": name, "namespace": namespace,
            "labels": {RUNNER_JOB_LABEL: "true"},
            "annotations": {
                REGISTRATION_ANNOTATION: registration.to_string(),
                REPOSITORY_ANNOTATION: repository
            }
        },
        "spec": {
            "suspend": true, "backoffLimit": 0, "activeDeadlineSeconds": DEADLINE_SECS,
            "ttlSecondsAfterFinished": 300,
            "template": {
                "metadata": {"labels": {"navigator.neonlaw.org/runner-job": name}},
                "spec": {
                    "restartPolicy": "Never", "automountServiceAccountToken": false,
                    "enableServiceLinks": false,
                    "serviceAccountName": name,
                    "securityContext": {
                        "runAsNonRoot": true, "runAsUser": 1000, "runAsGroup": 1000,
                        "fsGroup": 1000, "seccompProfile": {"type": "RuntimeDefault"}
                    },
                    "containers": [{
                        "name": "runner", "image": RUNNER_IMAGE,
                        "command": ["/bin/forgejo-runner", "one-job", "--wait", "--config", "/etc/forgejo-runner/config.yml"],
                        "env": [{"name": "HOME", "value": "/data"}, {"name": "TMPDIR", "value": "/tmp"}],
                        "securityContext": {
                            "allowPrivilegeEscalation": false, "readOnlyRootFilesystem": true,
                            "capabilities": {"drop": ["ALL"]}
                        },
                        "resources": {
                            "requests": {"cpu": "100m", "memory": "128Mi"},
                            "limits": {"cpu": "1", "memory": "512Mi"}
                        },
                        "volumeMounts": [
                            {"name": "config", "mountPath": "/etc/forgejo-runner", "readOnly": true},
                            {"name": "data", "mountPath": "/data"},
                            {"name": "tmp", "mountPath": "/tmp"}
                        ]
                    }],
                    "volumes": [
                        {"name": "config", "secret": {"secretName": secret, "defaultMode": 288}},
                        {"name": "data", "emptyDir": {"sizeLimit": "1Gi"}},
                        {"name": "tmp", "emptyDir": {"sizeLimit": "256Mi"}}
                    ]
                }
            }
        }
    })
}

/// GKE's `NodeLocal` DNS cache listener, which GKE writes into every pod's
/// `resolv.conf`. It is link-local, so a `kube-dns` pod selector never matches
/// it; without this rule the runner cannot resolve anything on GKE (ENG-1230).
#[cfg(test)]
const GKE_NODE_LOCAL_DNS: Ipv4Addr = Ipv4Addr::new(169, 254, 20, 10);

fn network_policy(name: &str, namespace: &str, ips: &[Ipv4Addr], dns: Ipv4Addr) -> Value {
    let destinations: Vec<_> = ips
        .iter()
        .map(|ip| json!({"ipBlock": {"cidr": format!("{ip}/32")}}))
        .collect();
    json!({
        "apiVersion": "networking.k8s.io/v1", "kind": "NetworkPolicy",
        "metadata": {"name": name, "namespace": namespace},
        "spec": {
            "podSelector": {"matchLabels": {"navigator.neonlaw.org/runner-job": name}},
            "policyTypes": ["Ingress", "Egress"], "ingress": [],
            "egress": [
                {"to": [{"namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": "kube-system"}},
                    "podSelector": {"matchLabels": {"k8s-app": "kube-dns"}}}],
                    "ports": [{"protocol": "UDP", "port": 53}, {"protocol": "TCP", "port": 53}]},
                {"to": destinations, "ports": [{"protocol": "TCP", "port": 443}]},
                // NodeLocal DNS uses a link-local listener rather than a selected
                // kube-system pod. This exception permits DNS only, not metadata HTTP.
                {"to": [{"ipBlock": {"cidr": format!("{dns}/32")}}],
                    "ports": [{"protocol": "UDP", "port": 53}, {"protocol": "TCP", "port": 53}]}
            ]
        }
    })
}

#[derive(Deserialize)]
pub(in crate::devx) struct Registration {
    pub(in crate::devx) id: u64,
    uuid: uuid::Uuid,
    token: String,
}

#[derive(Deserialize)]
struct RegisteredRunner {
    id: u64,
    uuid: uuid::Uuid,
    owner_id: u64,
    repo_id: u64,
    ephemeral: bool,
    #[serde(default)]
    status: String,
}

pub(in crate::devx) struct RunnerApi {
    client: reqwest::Client,
    endpoint: String,
    token: String,
}

impl RunnerApi {
    pub(in crate::devx) fn new(origin: &str, repository: &str, token: String) -> Result<Self> {
        repository_scope(repository)?;
        let url = url::Url::parse(origin).context("parse Forgejo origin")?;
        // Unit tests drive a loopback mock over HTTP; nothing else may.
        let loopback_test =
            cfg!(test) && url.scheme() == "http" && url.host_str() == Some("127.0.0.1");
        ensure!(
            (url.scheme() == "https" || loopback_test) && url.host_str().is_some()
                && url.username().is_empty() && url.password().is_none()
                && url.query().is_none() && url.fragment().is_none() && url.path() == "/",
            "Forgejo runner registration requires an HTTPS origin without credentials, path, query or fragment"
        );
        Ok(Self {
            client: reqwest::Client::builder()
                .https_only(!loopback_test)
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(30))
                .build()?,
            endpoint: format!(
                "{}/api/v1/repos/{repository}/actions/runners",
                origin.trim_end_matches('/')
            ),
            token,
        })
    }

    pub(in crate::devx) async fn register(&self, name: &str) -> Result<Registration> {
        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.token)
            .json(&json!({"name": name, "ephemeral": true}))
            .send()
            .await?;
        ensure!(
            response.status() == StatusCode::CREATED,
            "repository-scoped ephemeral runner registration refused ({})",
            response.status()
        );
        let registration: Registration =
            response.json().await.context("read runner registration")?;
        ensure!(
            registration.id > 0
                && !registration.uuid.is_nil()
                && !registration.token.trim().is_empty(),
            "incomplete runner registration"
        );
        if let Err(error) = self.verify_registration(&registration).await {
            self.remove(registration.id)
                .await
                .context("remove unverifiable runner registration")?;
            return Err(error);
        }
        Ok(registration)
    }

    async fn verify_registration(&self, registration: &Registration) -> Result<()> {
        let response = self
            .client
            .get(format!("{}/{}", self.endpoint, registration.id))
            .bearer_auth(&self.token)
            .send()
            .await?;
        ensure!(
            response.status() == StatusCode::OK,
            "runner registration cannot be read back"
        );
        let observed: RegisteredRunner = response.json().await.context("read registered runner")?;
        ensure!(
            observed.id == registration.id
                && observed.uuid == registration.uuid
                && observed.ephemeral
                && observed.owner_id == 0
                && observed.repo_id > 0,
            "registered runner is not the requested repository-scoped ephemeral identity"
        );
        Ok(())
    }

    pub(in crate::devx) async fn remove(&self, id: u64) -> Result<()> {
        let response = self
            .client
            .delete(format!("{}/{id}", self.endpoint))
            .bearer_auth(&self.token)
            .send()
            .await?;
        ensure!(
            matches!(
                response.status(),
                StatusCode::NO_CONTENT | StatusCode::NOT_FOUND
            ),
            "runner registration cleanup refused ({})",
            response.status()
        );
        Ok(())
    }

    /// Whether registration `id` has picked up its one job. `None` once the
    /// registration is gone. Forgejo reports `active` while a runner works.
    pub(in crate::devx) async fn busy(&self, id: u64) -> Result<Option<bool>> {
        let response = self
            .client
            .get(format!("{}/{id}", self.endpoint))
            .bearer_auth(&self.token)
            .send()
            .await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        ensure!(
            response.status() == StatusCode::OK,
            "runner status cannot be read ({})",
            response.status()
        );
        let runner: RegisteredRunner = response.json().await.context("read runner status")?;
        Ok(Some(runner.status == "active"))
    }

    pub(in crate::devx) async fn require_removed(&self, id: u64) -> Result<()> {
        let response = self
            .client
            .get(format!("{}/{id}", self.endpoint))
            .bearer_auth(&self.token)
            .send()
            .await?;
        ensure!(
            response.status() == StatusCode::NOT_FOUND,
            "completed runner remains registered; ephemeral enforcement was not proved"
        );
        Ok(())
    }
}

fn owned(mut object: Value, name: &str, uid: &str) -> Value {
    object["metadata"]["ownerReferences"] = json!([{
        "apiVersion": "batch/v1", "kind": "Job", "name": name, "uid": uid,
        "controller": false, "blockOwnerDeletion": false
    }]);
    object
}

/// The Kubernetes operations a disposable runner needs, scoped to the one
/// runner namespace. `kubectl` on an operator host ([`Kube`]) and the
/// in-cluster API from the runner controller ([`super::kubeapi::InCluster`])
/// both implement it, so the isolation below is the same code either way.
pub(in crate::devx) trait Cluster: Send + Sync {
    fn namespace(&self) -> &str;
    fn create(&self, object: &Value) -> Result<()>;
    /// The Job's JSON, or `None` when it does not exist.
    fn job(&self, name: &str) -> Result<Option<Value>>;
    fn unsuspend(&self, name: &str) -> Result<()>;
    /// Delete the Job and, by ownership, its pod, Secret, service account and policy.
    fn delete_job(&self, name: &str) -> Result<()>;
    fn runner_namespace(&self) -> Result<Value>;
    fn network_policies(&self) -> Result<Value>;
    /// Every Job labelled [`RUNNER_JOB_LABEL`].
    fn runner_jobs(&self) -> Result<Vec<Value>>;
    /// The stdout of the Job's pod: the runner's own lines and the job's steps.
    fn job_log(&self, name: &str) -> Result<String>;
}

impl Cluster for Kube {
    fn namespace(&self) -> &str {
        Kube::namespace(self)
    }

    fn create(&self, object: &Value) -> Result<()> {
        self.output_with_stdin(&["create", "-f", "-"], Some(&serde_json::to_vec(object)?))
            .map_err(|_| anyhow::anyhow!("Kubernetes resource creation failed; inspect operator permissions and private cluster diagnostics"))?;
        Ok(())
    }

    fn job(&self, name: &str) -> Result<Option<Value>> {
        let found = self.output(&["get", "job", name, "--ignore-not-found", "-o", "json"])?;
        if found.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_str(&found)?))
    }

    fn unsuspend(&self, name: &str) -> Result<()> {
        self.output(&[
            "patch",
            "job",
            name,
            "--type=merge",
            "-p",
            r#"{"spec":{"suspend":false}}"#,
        ])?;
        Ok(())
    }

    fn delete_job(&self, name: &str) -> Result<()> {
        self.output(&["delete", "job", name, "--ignore-not-found", "--wait=true"])?;
        Ok(())
    }

    fn runner_namespace(&self) -> Result<Value> {
        Ok(serde_json::from_str(&self.output(&[
            "get",
            "namespace",
            Kube::namespace(self),
            "-o",
            "json",
        ])?)?)
    }

    fn network_policies(&self) -> Result<Value> {
        Ok(serde_json::from_str(&self.output(&[
            "get",
            "networkpolicies",
            "-o",
            "json",
        ])?)?)
    }

    fn runner_jobs(&self) -> Result<Vec<Value>> {
        let listed: Value = serde_json::from_str(&self.output(&[
            "get",
            "jobs",
            "-l",
            &format!("{RUNNER_JOB_LABEL}=true"),
            "-o",
            "json",
        ])?)?;
        Ok(listed["items"].as_array().cloned().unwrap_or_default())
    }

    fn job_log(&self, name: &str) -> Result<String> {
        self.output(&["logs", &format!("job/{name}"), "--tail=-1"])
    }
}

/// Refuse to start `name` unless the runner namespace is declared isolated
/// and holds no broad `NetworkPolicy`.
pub(in crate::devx) fn require_isolated(kube: &dyn Cluster, name: &str) -> Result<()> {
    namespace_is_isolated(&kube.runner_namespace()?, &kube.network_policies()?, name)
}

fn namespace_is_isolated(namespace: &Value, policies: &Value, name: &str) -> Result<()> {
    ensure!(
        namespace["metadata"]["labels"]["navigator.neonlaw.org/runner-network-policy"] == "enforced"
            && namespace["metadata"]["labels"]["pod-security.kubernetes.io/enforce"] == "restricted",
        "runner namespace must declare enforced NetworkPolicy and restricted Pod Security admission"
    );
    let policies = policies["items"]
        .as_array()
        .context("NetworkPolicy inventory is incomplete")?;
    for policy in policies {
        // NetworkPolicy permissions are additive. Refuse every existing rule
        // except other invocations' exact, unique job selectors.
        let selector = &policy["spec"]["podSelector"];
        let labels = selector["matchLabels"]
            .as_object()
            .context("runner namespace has a broad NetworkPolicy")?;
        ensure!(
            labels.len() == 1
                && labels
                    .get("navigator.neonlaw.org/runner-job")
                    .and_then(Value::as_str)
                    .is_some_and(|other| other.starts_with("forgejo-job-") && other != name)
                && selector.get("matchExpressions").is_none(),
            "runner namespace has a broad NetworkPolicy; isolation cannot be established"
        );
    }
    Ok(())
}

/// Whether `name` is exactly one invocation's disposable Job name.
pub(in crate::devx) fn is_runner_job_name(name: &str) -> bool {
    name.strip_prefix("forgejo-job-")
        .is_some_and(|suffix| suffix.len() == 32 && uuid::Uuid::parse_str(suffix).is_ok())
}

pub(in crate::devx) fn cleanup(
    kube: &dyn Cluster,
    api: &RunnerApi,
    runtime: &tokio::runtime::Runtime,
    name: &str,
    id: u64,
) -> Result<()> {
    ensure!(
        id > 0 && is_runner_job_name(name),
        "cleanup requires the exact Job name and positive registration ID from one invocation"
    );
    let runner = runtime.block_on(api.remove(id));
    let job = kube.delete_job(name);
    runner.context("remove runner registration; retry explicit recovery cleanup")?;
    job.context("remove disposable runner Job; retry explicit recovery cleanup")?;
    Ok(())
}

/// The one-job runner's configuration. `log.job_level` puts every step's
/// output on the pod's stdout, which is how a job's log reaches Cloud Logging
/// and, through the controller, Dash0 (ENG-1238).
fn runner_config(origin: &str, registration: &Registration) -> Result<String> {
    Ok(serde_yaml::to_string(&json!({
        "log": {"level": "info", "job_level": "info"},
        "runner": {"capacity": 1, "labels": ["self-hosted:host"], "timeout": "20m"},
        "cache": {"enabled": false}, "host": {"workdir_parent": "/data/work"},
        "server": {"connections": {"navigator": {"url": origin, "uuid": registration.uuid, "token": registration.token}}}
    }))?)
}

/// Create the suspended Job, its owned identity, Secret and exact egress
/// policy, then start it and wait for it to finish.
pub(in crate::devx) fn execute(
    kube: &dyn Cluster,
    name: &str,
    repository: &str,
    origin: &str,
    ips: &[Ipv4Addr],
    dns: Ipv4Addr,
    registration: &Registration,
) -> Result<()> {
    let namespace = kube.namespace().to_owned();
    let namespace = namespace.as_str();
    kube.create(&job(name, namespace, name, repository, registration.id))?;
    let observed = kube.job(name)?.context("created Job cannot be read back")?;
    let uid = observed["metadata"]["uid"]
        .as_str()
        .filter(|uid| !uid.is_empty())
        .context("created Job has no UID")?;
    let config = runner_config(origin, registration)?;
    for object in [
        json!({"apiVersion": "v1", "kind": "ServiceAccount", "metadata": {"name": name, "namespace": namespace}, "automountServiceAccountToken": false}),
        json!({"apiVersion": "v1", "kind": "Secret", "metadata": {"name": name, "namespace": namespace}, "type": "Opaque", "stringData": {"config.yml": config}}),
        network_policy(name, namespace, ips, dns),
    ] {
        kube.create(&owned(object, name, uid))?;
    }
    kube.unsuspend(name)?;
    let started = Instant::now();
    loop {
        let observed = kube
            .job(name)?
            .context("runner Job disappeared while running")?;
        if observed["status"]["succeeded"].as_u64().unwrap_or(0) > 0 {
            return Ok(());
        }
        ensure!(
            observed["status"]["failed"].as_u64().unwrap_or(0) == 0,
            "one-job runner failed; inspect the private pod log"
        );
        ensure!(
            started.elapsed().as_secs() < DEADLINE_SECS + 60,
            "one-job runner timed out"
        );
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// A dedicated runner namespace, never Navigator's application namespace or
/// a system one, on an explicit context.
pub(in crate::devx) fn validate_namespace(context: &str, namespace: &str) -> Result<()> {
    ensure!(!context.trim().is_empty(), "Kubernetes context is required");
    ensure!(
        !namespace.trim().is_empty() && namespace != "navigator" && !namespace.starts_with("kube-"),
        "use a dedicated runner namespace, separate from application and system workloads"
    );
    Ok(())
}

/// Parse the `NodeLocal` DNS address a runner may query.
pub(in crate::devx) fn dns_address(address: &str) -> Result<Ipv4Addr> {
    dns_ipv4(address)
}

pub(in crate::devx) fn run(options: &crate::ForgejoRunnerArgs) -> Result<()> {
    validate_namespace(&options.context, &options.runner_namespace)?;
    let ips = egress_destinations(&options.egress_ip, &options.egress_host)?;
    let dns = dns_ipv4(&options.dns_ip)?;
    let api = RunnerApi::new(
        &options.url,
        &options.repository,
        std::env::var("FORGEJO_TOKEN")
            .context("FORGEJO_TOKEN must be configured on the trusted operator host")?,
    )?;
    let kube = Kube::selected(&options.context, &options.runner_namespace);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    if let (Some(id), Some(name)) = (options.cleanup_registration, &options.cleanup_job) {
        cleanup(&kube, &api, &runtime, name, id)?;
        println!("The selected runner registration and disposable Job were removed.");
        return Ok(());
    }
    ensure!(
        !ips.is_empty(),
        "explicit public egress destinations are required"
    );
    let name = format!("forgejo-job-{}", uuid::Uuid::new_v4().simple());
    require_isolated(&kube, &name)?;
    let registration = runtime.block_on(api.register(&name))?;
    // The numeric registration ID is recovery metadata, never a credential.
    eprintln!("One-job runner {name}, registration {}", registration.id);
    let result = execute(
        &kube,
        &name,
        &options.repository,
        &options.url,
        &ips,
        dns,
        &registration,
    )
    .and_then(|()| runtime.block_on(api.require_removed(registration.id)));
    let cleanup = cleanup(&kube, &api, &runtime, &name, registration.id);
    if let Err(error) = &result {
        eprintln!("One-job runner did not complete: {error}");
    }
    cleanup?;
    result?;
    println!("One-job runner completed and its registration and pod resources were removed. Inspect the workflow result separately.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn operator_client_refuses_cleartext_transport_before_sending_credentials() {
        let server = MockServer::start().await;
        let api = RunnerApi::new(
            "https://forge.example.test/",
            "example/sample",
            "synthetic-operator-token".into(),
        )
        .unwrap();
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(204))
            .expect(0)
            .mount(&server)
            .await;
        assert!(api
            .client
            .delete(server.uri())
            .bearer_auth(&api.token)
            .send()
            .await
            .is_err());
    }

    fn test_api(server: &MockServer) -> RunnerApi {
        RunnerApi {
            client: reqwest::Client::new(),
            endpoint: format!(
                "{}/api/v1/repos/example/sample/actions/runners",
                server.uri()
            ),
            token: "synthetic-operator-token".into(),
        }
    }

    #[tokio::test]
    async fn registration_is_repository_scoped_ephemeral_and_removed_after_one_job() {
        let server = MockServer::start().await;
        let endpoint = "/api/v1/repos/example/sample/actions/runners";
        Mock::given(method("POST")).and(path(endpoint))
            .and(header("authorization", "Bearer synthetic-operator-token"))
            .and(wiremock::matchers::body_json(json!({"name": "proof", "ephemeral": true})))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "id": 7, "uuid": "d012e532-c826-4e1d-adb8-58207c520f1b", "token": "synthetic-runner-token"
            }))).expect(1).mount(&server).await;
        Mock::given(method("GET"))
            .and(path(format!("{endpoint}/7")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 7, "uuid": "d012e532-c826-4e1d-adb8-58207c520f1b",
                "owner_id": 0, "repo_id": 3, "ephemeral": true
            })))
            .up_to_n_times(1)
            .with_priority(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("{endpoint}/7")))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path(format!("{endpoint}/7")))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        let api = test_api(&server);
        let registration = api.register("proof").await.unwrap();
        assert_eq!(registration.id, 7);
        assert_eq!(registration.token, "synthetic-runner-token");
        api.require_removed(registration.id).await.unwrap();
        api.remove(registration.id).await.unwrap();
    }

    #[tokio::test]
    async fn persistent_registration_and_failed_cleanup_cannot_report_success() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(403))
            .expect(1)
            .mount(&server)
            .await;
        let api = test_api(&server);
        assert!(api.require_removed(7).await.is_err());
        assert!(api.remove(7).await.is_err());
    }

    #[tokio::test]
    async fn registration_is_removed_when_ephemeral_read_back_is_not_confirmed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "id": 7, "uuid": "d012e532-c826-4e1d-adb8-58207c520f1b", "token": "synthetic-runner-token"
            }))).expect(1).mount(&server).await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 7, "uuid": "d012e532-c826-4e1d-adb8-58207c520f1b",
                "owner_id": 0, "repo_id": 3, "ephemeral": false
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        assert!(test_api(&server).register("proof").await.is_err());
    }

    #[test]
    fn registration_does_not_accept_redirects_or_credentials_in_origins() {
        for origin in [
            "http://forge.example.test",
            "https://user:secret@forge.example.test",
            "https://forge.example.test/path",
            "https://forge.example.test?token=secret",
        ] {
            assert!(RunnerApi::new(origin, "example/sample", "synthetic".into()).is_err());
        }
    }

    #[test]
    fn namespace_policy_unions_cannot_allow_internal_or_metadata_access() {
        let namespace = json!({"metadata": {"labels": {
            "navigator.neonlaw.org/runner-network-policy": "enforced",
            "pod-security.kubernetes.io/enforce": "restricted"
        }}});
        assert!(
            namespace_is_isolated(&namespace, &json!({"items": []}), "forgejo-job-current").is_ok()
        );
        assert!(namespace_is_isolated(
            &namespace,
            &json!({"items": [{"spec": {"podSelector": {}}}]}),
            "forgejo-job-current"
        )
        .is_err());
        assert!(
            namespace_is_isolated(&json!({}), &json!({"items": []}), "forgejo-job-current")
                .is_err()
        );
        assert!(namespace_is_isolated(&namespace, &json!({"items": [{"spec": {"podSelector": {"matchLabels": {"navigator.neonlaw.org/runner-job": "forgejo-job-current"}}}}]}), "forgejo-job-current").is_err());
        let policy = network_policy(
            "forgejo-job-proof",
            "proof",
            &["8.8.8.8".parse().unwrap()],
            GKE_NODE_LOCAL_DNS,
        );
        assert_eq!(policy["spec"]["ingress"], json!([]));
        assert_eq!(
            policy["spec"]["egress"][1]["to"],
            json!([{"ipBlock": {"cidr": "8.8.8.8/32"}}])
        );
        assert_eq!(
            policy["spec"]["egress"][1]["ports"],
            json!([{"protocol": "TCP", "port": 443}])
        );
        assert_eq!(
            policy["spec"]["egress"][0]["to"][0]["podSelector"]["matchLabels"]["k8s-app"],
            "kube-dns"
        );
    }

    #[test]
    fn node_local_dns_is_an_exact_dns_only_exception() {
        let dns = dns_ipv4("169.254.20.10").unwrap();
        let policy = network_policy(
            "forgejo-job-proof",
            "proof",
            &["8.8.8.8".parse().unwrap()],
            dns,
        );
        let rule = &policy["spec"]["egress"][2];
        assert_eq!(
            rule["to"],
            json!([{"ipBlock": {"cidr": "169.254.20.10/32"}}])
        );
        assert_eq!(
            rule["ports"],
            json!([{"protocol": "UDP", "port": 53}, {"protocol": "TCP", "port": 53}])
        );
        for endpoint in [
            "169.254.169.254",
            "169.254.169.252",
            "0.0.0.0",
            "127.0.0.1",
            "224.0.0.1",
            "169.254.0.0/16",
        ] {
            assert!(dns_ipv4(endpoint).is_err(), "{endpoint}");
        }
        assert_eq!(
            network_policy("proof", "proof", &[], dns)["spec"]["egress"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn default_policy_allows_both_kube_dns_and_gke_node_local_dns() {
        // ENG-1230: GKE pods resolve through 169.254.20.10, which the kube-dns
        // pod selector never matches; the default must still resolve there.
        assert_eq!(dns_ipv4("169.254.20.10").unwrap(), GKE_NODE_LOCAL_DNS);
        let policy = network_policy("proof", "proof", &[], GKE_NODE_LOCAL_DNS);
        let dns_ports = json!([{"protocol": "UDP", "port": 53}, {"protocol": "TCP", "port": 53}]);
        let egress = policy["spec"]["egress"].as_array().unwrap();
        assert!(egress.iter().any(|rule| rule["ports"] == dns_ports
            && rule["to"][0]["podSelector"]["matchLabels"]["k8s-app"] == "kube-dns"));
        assert!(egress.iter().any(|rule| rule["ports"] == dns_ports
            && rule["to"] == json!([{"ipBlock": {"cidr": "169.254.20.10/32"}}])));
    }

    #[test]
    fn all_runner_resources_are_collected_with_the_exact_job() {
        let resource = owned(
            json!({"metadata": {"name": "proof"}}),
            "proof",
            "synthetic-uid",
        );
        assert_eq!(
            resource["metadata"]["ownerReferences"][0]["uid"],
            "synthetic-uid"
        );
        assert_eq!(resource["metadata"]["ownerReferences"][0]["kind"], "Job");
        let pin: serde_yaml::Value = serde_yaml::from_str(include_str!(
            "../../../../k8s/forgejo/runner/kustomization.yaml"
        ))
        .unwrap();
        assert!(RUNNER_IMAGE.ends_with(pin["images"][0]["digest"].as_str().unwrap()));
    }

    #[test]
    fn one_job_has_no_shared_filesystem_or_platform_identity() {
        let job = job("runner-proof", "proof", "runner-proof", "example/sample", 7);
        assert_eq!(job["metadata"]["annotations"][REGISTRATION_ANNOTATION], "7");
        assert_eq!(
            job["metadata"]["annotations"][REPOSITORY_ANNOTATION],
            "example/sample"
        );
        assert_eq!(job["metadata"]["labels"][RUNNER_JOB_LABEL], "true");
        assert_eq!(job["spec"]["backoffLimit"], 0);
        assert_eq!(job["spec"]["activeDeadlineSeconds"], 1800);
        let pod = &job["spec"]["template"]["spec"];
        assert_eq!(pod["automountServiceAccountToken"], false);
        assert_eq!(pod["serviceAccountName"], "runner-proof");
        assert_eq!(pod["restartPolicy"], "Never");
        let runner = &pod["containers"][0];
        assert_eq!(runner["command"][1], "one-job");
        assert_eq!(runner["securityContext"]["allowPrivilegeEscalation"], false);
        assert_eq!(runner["securityContext"]["readOnlyRootFilesystem"], true);
        for volume in pod["volumes"].as_array().unwrap() {
            assert!(volume.get("hostPath").is_none());
            assert!(volume.get("persistentVolumeClaim").is_none());
        }
    }

    #[test]
    fn metadata_private_and_unbounded_egress_are_refused() {
        for address in [
            "169.254.169.254",
            "169.254.169.252",
            "10.0.0.1",
            "172.16.1.1",
            "192.168.1.1",
            "127.0.0.1",
            "100.64.0.1",
            "0.0.0.0",
            "224.0.0.1",
        ] {
            assert!(public_ipv4(address).is_err(), "{address}");
        }
        assert!(public_ipv4("8.8.8.8").is_ok());
        assert!(public_ipv4("0.0.0.0/0").is_err());
        assert!(public_ipv4("::1").is_err());
    }

    /// ENG-1234: a named host replaces hand-collected GitHub addresses, and a
    /// name that resolves to an internal address is refused, not filtered.
    #[test]
    fn egress_hosts_resolve_to_public_addresses_only() {
        assert_eq!(
            egress_destinations(&["8.8.8.8".into(), "8.8.8.8".into()], &[]).unwrap(),
            vec![Ipv4Addr::new(8, 8, 8, 8)]
        );
        assert!(egress_destinations(&[], &["localhost".into()]).is_err());
        assert!(egress_destinations(&[], &["https://forge.example".into()]).is_err());
    }

    #[test]
    fn repository_scope_cannot_be_global_or_an_organization() {
        for scope in [
            "",
            "example",
            "example/",
            "/sample",
            "a/b/c",
            "../sample",
            "a/b\n",
        ] {
            assert!(repository_scope(scope).is_err(), "{scope:?}");
        }
        assert!(repository_scope("example/sample-litigation").is_ok());
    }
}

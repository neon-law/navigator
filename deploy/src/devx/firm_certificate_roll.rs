//! The shared Firm `ManagedCertificate` across a change to its brand hosts.
//!
//! GKE re-issues a managed certificate whose `spec.domains` change, and
//! issuance takes 20 to 40 minutes, so the certificate is never edited. Its
//! name is a hash of its domain set ([`firm_certificate_name`]), and a
//! changed set is a new certificate:
//!
//! 1. [`begin`], early in `ops ship`, requests this build's certificate unless
//!    it already serves, and adds every Firm certificate serving today to the
//!    rendered Ingress annotation, so the apply never detaches one;
//! 2. the release rolls while Google issues the new certificate;
//! 3. [`finish`], after the rollouts, waits until the new certificate is
//!    `Active` for exactly its set and every host answers trusted TLS, then
//!    detaches and deletes every other Firm certificate.
//!
//! A brand host change therefore costs one issuance, overlapped with the
//! roll, and brand readiness runs only once every host serves.
//! `ops certificates pre-roll` runs the same two halves ahead of a release.
//! See `docs/dns.md`, "Changing shared certificate domains".

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, bail, ensure, Context, Result};

use super::ship::{
    firm_certificate_hosts, firm_certificate_name, kubectl, kubectl_json_if_present,
    managed_certificate_document, ShipConfig, FIRM_CERTIFICATE_PREFIX,
    TARGET_PROXY_CERTIFICATE_LIMIT,
};

/// The GKE Ingress every brand host routes through.
const INGRESS_NAME: &str = "navigator-web-gke";

const MANAGED_CERTIFICATES_ANNOTATION: &str = "networking.gke.io/managed-certificates";

/// The rendered Ingress, relative to the GKE kustomize root.
const INGRESS_FILE: &str = "ingress/ingress.yaml";

/// How long [`finish`] may wait for issuance by default. New Google-managed
/// certificates took 20 to 40 minutes per environment in November 2026.
pub(crate) const DEFAULT_ISSUANCE_TIMEOUT_MINUTES: u32 = 45;

/// The pause between two issuance polls.
const POLL_INTERVAL: Duration = if cfg!(test) {
    Duration::from_secs(1)
} else {
    Duration::from_secs(30)
};

/// What [`begin`] found, carried to [`finish`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Roll {
    /// No Ingress yet: a first deployment provisions everything at once.
    FirstDeployment,
    /// This build's certificate already serves its set.
    Serving,
    /// This build's certificate is issuing; `keep` stays attached until it
    /// serves.
    Issuing { keep: Vec<String> },
}

/// The cluster and network effects the roll needs, behind a seam so tests
/// script them.
pub(crate) trait Cluster {
    fn get_json(&mut self, kind: &str, name: &str) -> Result<Option<serde_json::Value>>;
    fn list_names(&mut self, kind: &str) -> Result<Vec<String>>;
    fn apply(&mut self, document: &str) -> Result<()>;
    fn delete(&mut self, kind: &str, name: &str) -> Result<()>;
    fn annotate_ingress(&mut self, ingress: &str, key: &str, value: &str) -> Result<()>;
    /// A trusted TLS handshake to `https://<host>/` through the ordinary
    /// trust store, given at most `max`. Never insecure.
    fn tls_trusted(&mut self, host: &str, max: Duration) -> bool;
    fn sleep(&mut self, duration: Duration);
    /// Time since the roll began, reads and checks included.
    fn elapsed(&self) -> Duration;
}

/// The live cluster through `kubectl`, and `curl` for the TLS check.
pub(crate) struct KubectlCluster<'a> {
    pub cfg: &'a ShipConfig,
    pub started: std::time::Instant,
}

impl KubectlCluster<'_> {
    fn run(&self, args: &[&str], what: &str) -> Result<()> {
        let status = kubectl(self.cfg)
            .args(args)
            .status()
            .with_context(|| format!("run kubectl {what}"))?;
        ensure!(status.success(), "kubectl {what} failed");
        Ok(())
    }
}

impl Cluster for KubectlCluster<'_> {
    fn get_json(&mut self, kind: &str, name: &str) -> Result<Option<serde_json::Value>> {
        kubectl_json_if_present(self.cfg, kind, name)
    }

    fn list_names(&mut self, kind: &str) -> Result<Vec<String>> {
        let out = kubectl(self.cfg)
            .args(["get", kind, "-o", "name"])
            .output()
            .with_context(|| format!("run kubectl get {kind}"))?;
        ensure!(
            out.status.success(),
            "kubectl get {kind} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| line.rsplit_once('/').map(|(_, name)| name.to_string()))
            .collect())
    }

    fn apply(&mut self, document: &str) -> Result<()> {
        use std::io::Write as _;
        let mut child = kubectl(self.cfg)
            .args(["apply", "-f", "-"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .context("run kubectl apply")?;
        child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("kubectl apply has no stdin"))?
            .write_all(document.as_bytes())
            .context("write the Firm certificate to kubectl apply")?;
        let status = child.wait().context("wait for kubectl apply")?;
        ensure!(
            status.success(),
            "kubectl apply of the Firm certificate failed"
        );
        Ok(())
    }

    fn delete(&mut self, kind: &str, name: &str) -> Result<()> {
        self.run(
            &["delete", kind, name, "--ignore-not-found=true"],
            &format!("delete {kind} {name}"),
        )
    }

    fn annotate_ingress(&mut self, ingress: &str, key: &str, value: &str) -> Result<()> {
        self.run(
            &[
                "annotate",
                "ingress",
                ingress,
                &format!("{key}={value}"),
                "--overwrite",
            ],
            &format!("annotate ingress {ingress}"),
        )
    }

    fn tls_trusted(&mut self, host: &str, max: Duration) -> bool {
        std::process::Command::new("curl")
            .args(["--silent", "--output", "/dev/null", "--max-time"])
            .arg(max.as_secs().max(1).to_string())
            .arg(format!("https://{host}/"))
            .status()
            .is_ok_and(|status| status.success())
    }

    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }

    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}

/// Whether `name` is a shared Firm certificate, current or replaced.
fn is_firm_certificate(name: &str) -> bool {
    name.strip_prefix(FIRM_CERTIFICATE_PREFIX)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('-'))
}

fn certificate_domains(certificate: &serde_json::Value) -> Result<BTreeSet<String>> {
    certificate
        .pointer("/spec/domains")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow!("the certificate has no spec.domains"))?
        .iter()
        .map(|domain| {
            domain
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| anyhow!("the certificate has a non-string domain"))
        })
        .collect()
}

/// The certificate names the Ingress annotation attaches, in order.
fn attached_certificates(ingress: &serde_json::Value) -> Vec<String> {
    ingress
        .pointer("/metadata/annotations")
        .and_then(|annotations| annotations.get(MANAGED_CERTIFICATES_ANNOTATION))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

fn certificate_status(certificate: &serde_json::Value) -> Option<&str> {
    certificate
        .pointer("/status/certificateStatus")
        .and_then(serde_json::Value::as_str)
}

/// Whether the controller reports `certificate` `Active` for exactly
/// `desired`, per domain as well as overall.
fn issued_for(certificate: &serde_json::Value, desired: &BTreeSet<String>) -> Result<bool> {
    if certificate_domains(certificate)? != *desired {
        return Ok(false);
    }
    let domains: BTreeSet<String> = certificate
        .pointer("/status/domainStatus")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| entry.get("status").and_then(serde_json::Value::as_str) == Some("Active"))
        .filter_map(|entry| entry.get("domain").and_then(serde_json::Value::as_str))
        .map(str::to_string)
        .collect();
    Ok(certificate_status(certificate) == Some("Active") && domains == *desired)
}

/// The most one host's TLS check may take.
const TLS_CHECK_LIMIT: Duration = Duration::from_secs(15);

/// Poll until `certificate` is issued for `desired` and every host answers
/// trusted TLS, or `timeout` elapses. The bound is wall time: reads and host
/// checks count, and each check gets no more than the time left.
fn wait_serving<C: Cluster>(
    cluster: &mut C,
    certificate: &str,
    desired: &BTreeSet<String>,
    timeout: Duration,
) -> Result<()> {
    let deadline = cluster.elapsed() + timeout;
    loop {
        let live = cluster
            .get_json("managedcertificate", certificate)?
            .ok_or_else(|| anyhow!("managedcertificate {certificate} disappeared mid-roll"))?;
        let issued = issued_for(&live, desired)?;
        let mut untrusted: Vec<&str> = Vec::new();
        for host in desired {
            let remaining = deadline.saturating_sub(cluster.elapsed());
            if !issued
                || remaining.is_zero()
                || !cluster.tls_trusted(host, remaining.min(TLS_CHECK_LIMIT))
            {
                untrusted.push(host);
            }
        }
        if untrusted.is_empty() {
            return Ok(());
        }
        let remaining = deadline.saturating_sub(cluster.elapsed());
        if remaining.is_zero() {
            bail!(
                "{certificate} did not serve trusted TLS within {} minutes; pending: {}. The Firm \
                 certificate serving before it stays attached, and a rerun resumes the wait. A \
                 pending host must resolve to the Ingress; rerun once it does, or raise the \
                 timeout.",
                timeout.as_secs() / 60,
                untrusted.join(", ")
            );
        }
        cluster.sleep(remaining.min(POLL_INTERVAL));
    }
}

/// Request the certificate for `desired` unless it already serves, and name
/// the Firm certificates that must stay attached until it does.
pub(crate) fn begin_with<C: Cluster>(
    cluster: &mut C,
    namespace: &str,
    desired: &[String],
    dry_run: bool,
) -> Result<Roll> {
    let target = firm_certificate_name(desired);
    let set: BTreeSet<String> = desired.iter().cloned().collect();
    if let Some(live) = cluster.get_json("managedcertificate", &target)? {
        if issued_for(&live, &set)? {
            return Ok(Roll::Serving);
        }
    }
    let Some(ingress) = cluster.get_json("ingress", INGRESS_NAME)? else {
        return Ok(Roll::FirstDeployment);
    };
    let mut keep = Vec::new();
    for name in attached_certificates(&ingress) {
        if !is_firm_certificate(&name) || name == target {
            continue;
        }
        let live = cluster.get_json("managedcertificate", &name)?;
        if live.as_ref().and_then(certificate_status) == Some("Active") {
            keep.push(name);
        }
    }
    if dry_run {
        eprintln!("DRY-RUN request {target} for [{}]", desired.join(", "));
    } else {
        cluster.apply(&managed_certificate_document(namespace, &target, desired))?;
    }
    eprintln!(
        "==> {target} requested for this build's brand hosts; {} serves until it is Active",
        if keep.is_empty() {
            "nothing else".to_string()
        } else {
            keep.join(", ")
        }
    );
    Ok(Roll::Issuing { keep })
}

/// The rendered annotation line with `keep` attached before the target.
fn annotation_keeping(rendered: &str, target: &str, keep: &[String]) -> Result<String> {
    let line = rendered
        .lines()
        .find(|line| {
            line.trim_start()
                .starts_with(MANAGED_CERTIFICATES_ANNOTATION)
        })
        .ok_or_else(|| anyhow!("the rendered Ingress has no {MANAGED_CERTIFICATES_ANNOTATION}"))?;
    let (key, value) = line
        .split_once(": ")
        .ok_or_else(|| anyhow!("unreadable rendered annotation `{line}`"))?;
    let mut names: Vec<&str> = value.split(',').map(str::trim).collect();
    let at = names
        .iter()
        .position(|name| *name == target)
        .ok_or_else(|| anyhow!("the rendered Ingress does not attach {target}"))?;
    for (offset, name) in keep.iter().enumerate() {
        names.insert(at + offset, name);
    }
    ensure!(
        names.len() <= TARGET_PROXY_CERTIFICATE_LIMIT,
        "cannot keep {} attached beside {target}: {INGRESS_NAME} would attach {} certificates \
         and a target HTTPS proxy accepts at most {TARGET_PROXY_CERTIFICATE_LIMIT}. Retire a \
         brand's own certificate first.",
        keep.join(", "),
        names.len()
    );
    Ok(rendered.replacen(line, &format!("{key}: {}", names.join(",")), 1))
}

/// Keep `roll`'s serving certificates in the rendered Ingress at `root`.
fn keep_attached(root: &Path, target: &str, roll: &Roll) -> Result<()> {
    let Roll::Issuing { keep } = roll else {
        return Ok(());
    };
    if keep.is_empty() {
        return Ok(());
    }
    let path = root.join(INGRESS_FILE);
    let rendered =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    std::fs::write(&path, annotation_keeping(&rendered, target, keep)?)
        .with_context(|| format!("write {}", path.display()))
}

/// Wait for the certificate [`begin_with`] requested, then detach the ones
/// it kept and delete every other Firm certificate.
pub(crate) fn finish_with<C: Cluster>(
    cluster: &mut C,
    desired: &[String],
    roll: &Roll,
    timeout: Duration,
    dry_run: bool,
) -> Result<()> {
    let target = firm_certificate_name(desired);
    if *roll == Roll::FirstDeployment {
        return Ok(());
    }
    if dry_run {
        eprintln!("DRY-RUN wait for {target} to serve, then delete every other Firm certificate");
        return Ok(());
    }
    if let Roll::Issuing { keep } = roll {
        let set: BTreeSet<String> = desired.iter().cloned().collect();
        wait_serving(cluster, &target, &set, timeout)?;
        eprintln!("==> {target} is Active and serves trusted TLS for every host");
        if !keep.is_empty() {
            let ingress = cluster
                .get_json("ingress", INGRESS_NAME)?
                .ok_or_else(|| anyhow!("ingress {INGRESS_NAME} is missing"))?;
            let names: Vec<String> = attached_certificates(&ingress)
                .into_iter()
                .filter(|name| !keep.contains(name))
                .collect();
            cluster.annotate_ingress(
                INGRESS_NAME,
                MANAGED_CERTIFICATES_ANNOTATION,
                &names.join(","),
            )?;
        }
    }
    for name in cluster.list_names("managedcertificate")? {
        if is_firm_certificate(&name) && name != target {
            cluster.delete("managedcertificate", &name)?;
            eprintln!("==> deleted {name}, replaced by {target}");
        }
    }
    Ok(())
}

fn live_cluster(cfg: &ShipConfig) -> KubectlCluster<'_> {
    KubectlCluster {
        cfg,
        started: std::time::Instant::now(),
    }
}

/// [`begin_with`] against `cfg`'s cluster, then keep the serving
/// certificates in the rendered tree at `root` the ship is about to apply.
pub(crate) fn begin(cfg: &ShipConfig, root: &Path, dry_run: bool) -> Result<Roll> {
    let desired = firm_certificate_hosts(&cfg.public_host);
    let roll = begin_with(&mut live_cluster(cfg), &cfg.namespace, &desired, dry_run)?;
    keep_attached(root, &firm_certificate_name(&desired), &roll)?;
    Ok(roll)
}

/// [`finish_with`] against `cfg`'s cluster.
pub(crate) fn finish(
    cfg: &ShipConfig,
    roll: &Roll,
    timeout_minutes: u32,
    dry_run: bool,
) -> Result<()> {
    finish_with(
        &mut live_cluster(cfg),
        &firm_certificate_hosts(&cfg.public_host),
        roll,
        Duration::from_secs(u64::from(timeout_minutes) * 60),
        dry_run,
    )
}

/// Attach `target` to the live Ingress beside what it attaches now, for a
/// roll that runs outside `ops ship`.
fn attach_live<C: Cluster>(cluster: &mut C, target: &str) -> Result<()> {
    let ingress = cluster
        .get_json("ingress", INGRESS_NAME)?
        .ok_or_else(|| anyhow!("ingress {INGRESS_NAME} is missing"))?;
    let mut names = attached_certificates(&ingress);
    if names.iter().any(|name| name == target) {
        return Ok(());
    }
    ensure!(
        names.len() < TARGET_PROXY_CERTIFICATE_LIMIT,
        "cannot attach {target}: {INGRESS_NAME} already attaches {} certificates and a target \
         HTTPS proxy accepts at most {TARGET_PROXY_CERTIFICATE_LIMIT}. Retire a brand's own \
         certificate first.",
        names.len()
    );
    names.push(target.to_string());
    cluster.annotate_ingress(
        INGRESS_NAME,
        MANAGED_CERTIFICATES_ANNOTATION,
        &names.join(","),
    )
}

/// `begin`, attach, and `finish` against any cluster: the whole roll ahead of
/// a release.
pub(crate) fn pre_roll_with<C: Cluster>(
    cluster: &mut C,
    namespace: &str,
    desired: &[String],
    timeout: Duration,
    dry_run: bool,
) -> Result<Roll> {
    let roll = begin_with(cluster, namespace, desired, dry_run)?;
    if matches!(roll, Roll::Issuing { .. }) && !dry_run {
        attach_live(cluster, &firm_certificate_name(desired))?;
    }
    finish_with(cluster, desired, &roll, timeout, dry_run)?;
    Ok(roll)
}

/// `hudson ops certificates pre-roll`: resolve the deployment, fetch its
/// cluster credentials, and roll the Firm certificate ahead of the release.
pub fn pre_roll_for_deployment(
    deployment: &str,
    deployments_dir: Option<&std::path::Path>,
    project_id: Option<&str>,
    timeout_minutes: u32,
    dry_run: bool,
) -> Result<()> {
    let loaded = if let Some(project) = project_id {
        super::deployment_config::load(project, deployment)?
    } else {
        let root = super::deployments::root(deployments_dir)?;
        super::deployments::Deployment::load(&root, deployment)?
    };
    let cfg = ShipConfig::from_deployment(&loaded)
        .with_context(|| format!("resolve ShipConfig for deployment {deployment}"))?;
    let _credentials = super::ship::ShipCredentials::cluster(&cfg)?;
    let desired = firm_certificate_hosts(&cfg.public_host);
    let target = firm_certificate_name(&desired);
    let roll = pre_roll_with(
        &mut live_cluster(&cfg),
        &cfg.namespace,
        &desired,
        Duration::from_secs(u64::from(timeout_minutes) * 60),
        dry_run,
    )?;
    match roll {
        Roll::FirstDeployment => {
            eprintln!("==> no Ingress on this deployment yet; ops ship provisions {target}");
        }
        Roll::Serving => eprintln!("==> {target} already serves this build's hosts"),
        Roll::Issuing { .. } if dry_run => {
            eprintln!("==> rerun without --dry-run to pre-roll this deployment");
        }
        Roll::Issuing { .. } => eprintln!("==> {target} pre-rolled; the release finds it serving"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, VecDeque};

    /// An in-memory cluster. Each `get_json` of a certificate returns the next
    /// scripted status for it, then keeps returning the last one.
    #[derive(Default)]
    struct FakeCluster {
        objects: BTreeMap<(String, String), serde_json::Value>,
        statuses: BTreeMap<String, VecDeque<serde_json::Value>>,
        untrusted: BTreeSet<String>,
        log: Vec<String>,
        slept: Duration,
        /// How long each TLS check takes, capped by the limit it is given.
        tls_cost: Duration,
        spent: Duration,
    }

    impl Cluster for FakeCluster {
        fn get_json(&mut self, kind: &str, name: &str) -> Result<Option<serde_json::Value>> {
            let key = (kind.to_string(), name.to_string());
            let Some(object) = self.objects.get_mut(&key) else {
                return Ok(None);
            };
            if let Some(queue) = self.statuses.get_mut(name) {
                if let Some(status) = queue.pop_front() {
                    object["status"] = status;
                }
            }
            Ok(Some(object.clone()))
        }

        fn list_names(&mut self, kind: &str) -> Result<Vec<String>> {
            Ok(self
                .objects
                .keys()
                .filter(|(k, _)| k == kind)
                .map(|(_, name)| name.clone())
                .collect())
        }

        fn apply(&mut self, document: &str) -> Result<()> {
            let doc: serde_json::Value = serde_yaml::from_str(document)?;
            let name = doc["metadata"]["name"].as_str().unwrap().to_string();
            self.log.push(format!("apply {name}"));
            self.objects
                .entry(("managedcertificate".into(), name))
                .or_insert(doc);
            Ok(())
        }

        fn delete(&mut self, kind: &str, name: &str) -> Result<()> {
            self.log.push(format!("delete {name}"));
            self.objects.remove(&(kind.to_string(), name.to_string()));
            Ok(())
        }

        fn annotate_ingress(&mut self, ingress: &str, key: &str, value: &str) -> Result<()> {
            self.log.push(format!("annotate {value}"));
            let object = self
                .objects
                .get_mut(&("ingress".to_string(), ingress.to_string()))
                .unwrap();
            object["metadata"]["annotations"][key] = value.into();
            Ok(())
        }

        fn tls_trusted(&mut self, host: &str, max: Duration) -> bool {
            self.spent += self.tls_cost.min(max);
            !self.untrusted.contains(host)
        }

        fn sleep(&mut self, duration: Duration) {
            self.slept += duration;
            self.spent += duration;
        }

        fn elapsed(&self) -> Duration {
            self.spent
        }
    }

    fn hosts(list: &[&str]) -> Vec<String> {
        list.iter().map(|host| (*host).to_string()).collect()
    }

    fn certificate(name: &str, domains: &[&str]) -> serde_json::Value {
        serde_json::json!({
            "metadata": { "name": name },
            "spec": { "domains": domains },
            "status": active(domains),
        })
    }

    fn active(domains: &[&str]) -> serde_json::Value {
        serde_json::json!({
            "certificateStatus": "Active",
            "domainStatus": domains
                .iter()
                .map(|domain| serde_json::json!({ "domain": domain, "status": "Active" }))
                .collect::<Vec<_>>(),
        })
    }

    fn provisioning(domains: &[&str]) -> serde_json::Value {
        serde_json::json!({
            "certificateStatus": "Provisioning",
            "domainStatus": domains
                .iter()
                .map(|domain| serde_json::json!({ "domain": domain, "status": "Provisioning" }))
                .collect::<Vec<_>>(),
        })
    }

    const NAMESPACE: &str = "navigator";
    const SERVICES: &str = "navigator-web,navigator-workflows,navigator-forgejo";
    const HOUR: Duration = Duration::from_secs(3600);

    fn name(domains: &[&str]) -> String {
        firm_certificate_name(&hosts(domains))
    }

    /// A cluster whose Ingress attaches the services and `serving`, a Firm
    /// certificate issued for `live`.
    fn cluster(serving: &str, live: &[&str]) -> FakeCluster {
        let mut fake = FakeCluster::default();
        fake.objects.insert(
            ("managedcertificate".into(), serving.into()),
            certificate(serving, live),
        );
        fake.objects.insert(
            ("ingress".into(), INGRESS_NAME.into()),
            serde_json::json!({
                "metadata": { "annotations": {
                    MANAGED_CERTIFICATES_ANNOTATION: format!("{SERVICES},{serving}"),
                } },
            }),
        );
        fake
    }

    fn annotation(fake: &FakeCluster) -> String {
        fake.objects[&("ingress".to_string(), INGRESS_NAME.to_string())]["metadata"]["annotations"]
            [MANAGED_CERTIFICATES_ANNOTATION]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn the_name_is_the_domain_set_whatever_its_order() {
        assert_eq!(name(&["b.test", "a.test"]), name(&["a.test", "b.test"]));
        assert_ne!(name(&["a.test"]), name(&["a.test", "b.test"]));
        assert!(name(&["a.test"]).starts_with("navigator-firm-"));
        assert!(is_firm_certificate("navigator-firm"));
        assert!(is_firm_certificate("navigator-firm-next"));
        assert!(!is_firm_certificate("navigator-firmament"));
        assert!(!is_firm_certificate("navigator-web"));
    }

    #[test]
    fn a_first_deployment_requests_nothing_and_waits_for_nothing() {
        let mut fake = FakeCluster::default();
        let desired = hosts(&["a.test"]);
        let roll = begin_with(&mut fake, NAMESPACE, &desired, false).unwrap();
        assert_eq!(roll, Roll::FirstDeployment);
        finish_with(&mut fake, &desired, &roll, HOUR, false).unwrap();
        assert!(fake.log.is_empty(), "{:?}", fake.log);
    }

    #[test]
    fn a_serving_certificate_is_left_alone_and_replaced_ones_are_deleted() {
        let target = name(&["a.test", "b.test"]);
        let mut fake = cluster(&target, &["a.test", "b.test"]);
        fake.objects.insert(
            ("managedcertificate".into(), "navigator-firm".into()),
            certificate("navigator-firm", &["a.test"]),
        );
        let desired = hosts(&["b.test", "a.test"]);
        let roll = begin_with(&mut fake, NAMESPACE, &desired, false).unwrap();
        assert_eq!(roll, Roll::Serving);
        finish_with(&mut fake, &desired, &roll, HOUR, false).unwrap();
        assert_eq!(fake.log, ["delete navigator-firm"]);
        assert_eq!(fake.slept, Duration::ZERO);
    }

    /// The 26.11.9 shape: a host retired. The new certificate is requested
    /// at the start without any wait, the serving one stays attached through
    /// the roll, and only after the new one serves is the old one detached
    /// and deleted.
    #[test]
    fn a_host_change_requests_early_and_swaps_only_once_serving() {
        let mut fake = cluster("navigator-firm", &["a.test", "gone.test"]);
        let desired = hosts(&["a.test"]);
        let target = name(&["a.test"]);
        fake.statuses.insert(
            target.clone(),
            VecDeque::from([
                provisioning(&["a.test"]),
                provisioning(&["a.test"]),
                active(&["a.test"]),
            ]),
        );

        let roll = begin_with(&mut fake, NAMESPACE, &desired, false).unwrap();
        assert_eq!(
            roll,
            Roll::Issuing {
                keep: hosts(&["navigator-firm"])
            }
        );
        assert_eq!(fake.log, [format!("apply {target}")]);
        assert_eq!(fake.slept, Duration::ZERO, "the request never waits");

        // The ship's apply attaches the rendered annotation, which keeps
        // the serving certificate beside the new one.
        fake.annotate_ingress(
            INGRESS_NAME,
            MANAGED_CERTIFICATES_ANNOTATION,
            &format!("{SERVICES},navigator-firm,{target}"),
        )
        .unwrap();
        fake.log.clear();

        finish_with(&mut fake, &desired, &roll, HOUR, false).unwrap();
        assert_eq!(
            fake.log,
            [
                format!("annotate {SERVICES},{target}"),
                "delete navigator-firm".to_string(),
            ]
        );
        assert!(fake.slept > Duration::ZERO);
    }

    #[test]
    fn the_serving_certificate_stays_while_a_host_lacks_trusted_tls() {
        let mut fake = cluster("navigator-firm", &["a.test", "gone.test"]);
        let desired = hosts(&["a.test"]);
        let target = name(&["a.test"]);
        fake.statuses
            .insert(target.clone(), VecDeque::from([active(&["a.test"])]));
        fake.untrusted.insert("a.test".into());
        let roll = begin_with(&mut fake, NAMESPACE, &desired, false).unwrap();
        fake.log.clear();
        let timeout = Duration::from_secs(5);
        let error = finish_with(&mut fake, &desired, &roll, timeout, false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("pending: a.test"), "{error}");
        assert!(fake.log.is_empty(), "{:?}", fake.log);
        assert_eq!(fake.slept, timeout);
        assert!(annotation(&fake).contains("navigator-firm"));
    }

    /// Slow or unreachable hosts count against the bound: the wait ends at
    /// the configured timeout, not at the timeout plus every host check.
    #[test]
    fn the_wait_bound_is_wall_time_including_host_checks() {
        let target_hosts = ["a.test", "b.test", "c.test"];
        let mut fake = cluster("navigator-firm", &["a.test"]);
        let desired = hosts(&target_hosts);
        fake.statuses
            .insert(name(&target_hosts), VecDeque::from([active(&target_hosts)]));
        fake.untrusted.extend(hosts(&target_hosts));
        fake.tls_cost = Duration::from_secs(15);
        let roll = begin_with(&mut fake, NAMESPACE, &desired, false).unwrap();
        let start = fake.elapsed();
        let timeout = Duration::from_secs(60);
        let error = finish_with(&mut fake, &desired, &roll, timeout, false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("did not serve trusted TLS"), "{error}");
        assert_eq!(fake.elapsed().saturating_sub(start), timeout);
    }

    /// A rerun after a timed-out wait resumes it: the certificate already
    /// requested is kept, and the one still serving is kept attached.
    #[test]
    fn a_rerun_resumes_the_issuing_certificate() {
        let target = name(&["a.test"]);
        let mut fake = cluster("navigator-firm", &["a.test", "gone.test"]);
        fake.objects.insert(
            ("managedcertificate".into(), target.clone()),
            certificate(&target, &["a.test"]),
        );
        fake.statuses
            .insert(target.clone(), VecDeque::from([provisioning(&["a.test"])]));
        let roll = begin_with(&mut fake, NAMESPACE, &hosts(&["a.test"]), false).unwrap();
        assert_eq!(
            roll,
            Roll::Issuing {
                keep: hosts(&["navigator-firm"])
            }
        );
    }

    #[test]
    fn only_an_active_firm_certificate_is_kept() {
        let mut fake = cluster("navigator-firm", &["a.test"]);
        fake.statuses.insert(
            "navigator-firm".into(),
            VecDeque::from([provisioning(&["a.test"])]),
        );
        let roll = begin_with(&mut fake, NAMESPACE, &hosts(&["b.test"]), false).unwrap();
        assert_eq!(roll, Roll::Issuing { keep: vec![] });
    }

    #[test]
    fn a_dry_run_writes_and_waits_for_nothing() {
        let mut fake = cluster("navigator-firm", &["a.test", "gone.test"]);
        let desired = hosts(&["a.test"]);
        let roll = begin_with(&mut fake, NAMESPACE, &desired, true).unwrap();
        finish_with(&mut fake, &desired, &roll, HOUR, true).unwrap();
        assert!(fake.log.is_empty(), "{:?}", fake.log);
        assert_eq!(fake.slept, Duration::ZERO);
    }

    #[test]
    fn the_rendered_annotation_keeps_the_serving_certificate_before_the_new_one() {
        let target = name(&["a.test"]);
        let rendered = format!(
            "metadata:\n  annotations:\n    {MANAGED_CERTIFICATES_ANNOTATION}: {SERVICES},{target}\n    other: x\n"
        );
        let kept = annotation_keeping(&rendered, &target, &hosts(&["navigator-firm"])).unwrap();
        assert_eq!(
            kept,
            format!(
                "metadata:\n  annotations:\n    {MANAGED_CERTIFICATES_ANNOTATION}: {SERVICES},navigator-firm,{target}\n    other: x\n"
            )
        );

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("ingress")).unwrap();
        std::fs::write(dir.path().join(INGRESS_FILE), &rendered).unwrap();
        keep_attached(
            dir.path(),
            &target,
            &Roll::Issuing {
                keep: hosts(&["navigator-firm"]),
            },
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join(INGRESS_FILE)).unwrap(),
            kept
        );
    }

    #[test]
    fn keeping_refuses_past_the_target_proxy_certificate_limit() {
        let target = name(&["a.test"]);
        let full = (1..TARGET_PROXY_CERTIFICATE_LIMIT)
            .map(|i| format!("cert-{i}"))
            .chain(std::iter::once(target.clone()))
            .collect::<Vec<_>>()
            .join(",");
        let rendered = format!("    {MANAGED_CERTIFICATES_ANNOTATION}: {full}\n");
        let error = annotation_keeping(&rendered, &target, &hosts(&["navigator-firm"]))
            .unwrap_err()
            .to_string();
        assert!(error.contains("at most 15"), "{error}");
    }

    #[test]
    fn a_pre_roll_attaches_waits_and_swaps_on_the_live_ingress() {
        let mut fake = cluster("navigator-firm", &["a.test", "gone.test"]);
        let desired = hosts(&["a.test"]);
        let target = name(&["a.test"]);
        fake.statuses.insert(
            target.clone(),
            VecDeque::from([provisioning(&["a.test"]), active(&["a.test"])]),
        );
        pre_roll_with(&mut fake, NAMESPACE, &desired, HOUR, false).unwrap();
        assert_eq!(
            fake.log,
            [
                format!("apply {target}"),
                format!("annotate {SERVICES},navigator-firm,{target}"),
                format!("annotate {SERVICES},{target}"),
                "delete navigator-firm".to_string(),
            ]
        );
        assert_eq!(annotation(&fake), format!("{SERVICES},{target}"));
    }

    #[test]
    fn issuance_counts_only_for_the_desired_domain_set() {
        let desired: BTreeSet<String> = hosts(&["a.test", "b.test"]).into_iter().collect();
        let mut cert = certificate("c", &["a.test", "b.test"]);
        assert!(issued_for(&cert, &desired).unwrap());
        cert["status"] = active(&["a.test"]);
        assert!(!issued_for(&cert, &desired).unwrap());
        cert["status"] = provisioning(&["a.test", "b.test"]);
        assert!(!issued_for(&cert, &desired).unwrap());
        cert["status"] = serde_json::Value::Null;
        assert!(!issued_for(&cert, &desired).unwrap());
    }

    /// The release workflow's ship jobs must outlast the one issuance wait at
    /// the default bound, plus the roll itself.
    #[test]
    fn the_release_ship_jobs_absorb_the_issuance_wait() {
        let workflow = include_str!("../../../.github/workflows/release-deploy.yml");
        let doc: serde_yaml::Value = serde_yaml::from_str(workflow).unwrap();
        let jobs = doc["jobs"].as_mapping().unwrap();
        let ships: Vec<u64> = jobs
            .values()
            .filter(|job| {
                job["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with("ship "))
            })
            .map(|job| job["timeout-minutes"].as_u64().unwrap())
            .collect();
        assert_eq!(ships.len(), 2, "ship staging and ship production");
        let floor = u64::from(DEFAULT_ISSUANCE_TIMEOUT_MINUTES) + 30;
        for minutes in ships {
            assert!(minutes >= floor, "ship job timeout {minutes} < {floor}");
        }
    }
}

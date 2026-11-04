//! The staged transition of the shared `navigator-firm` `ManagedCertificate`
//! when a brand host is added, moved or retired.
//!
//! GKE recreates a managed certificate whose `spec.domains` change, so
//! patching `navigator-firm` in place can drop HTTPS for every brand it
//! carries while the replacement provisions. The transition keeps trusted TLS
//! serving throughout:
//!
//! 1. stage [`NEXT_CERTIFICATE_NAME`] with the target domain set and attach it
//!    to the Ingress beside the serving certificates;
//! 2. wait for it to be `Active` for exactly that set, and for every target
//!    host to answer a trusted TLS handshake;
//! 3. patch `navigator-firm` to the target set, and wait the same way again.
//!
//! `ops ship` runs it whenever the rendered set differs from the live one, and
//! `ops certificates pre-roll` runs it ahead of a release so the ship finds
//! nothing to change. The ship's own render then drops the staged certificate
//! from the Ingress annotation and deletes it. See `docs/dns.md`, "Changing
//! shared certificate domains".

use std::collections::BTreeSet;
use std::time::Duration;

use anyhow::{anyhow, bail, ensure, Context, Result};

use super::ship::{
    firm_certificate_hosts, kubectl, kubectl_json_if_present, managed_certificate_document,
    ShipConfig, FIRM_CERTIFICATE_NAME, TARGET_PROXY_CERTIFICATE_LIMIT,
};

/// The replacement certificate staged beside `navigator-firm`.
pub(crate) const NEXT_CERTIFICATE_NAME: &str = "navigator-firm-next";

/// The GKE Ingress every brand host routes through.
const INGRESS_NAME: &str = "navigator-web-gke";

const MANAGED_CERTIFICATES_ANNOTATION: &str = "networking.gke.io/managed-certificates";

/// How long each of the transition's two waits may take by default. New
/// Google-managed certificates took 20–40 minutes per environment on
/// 2026-11-04; the release workflow's ship jobs budget two of these waits.
pub(crate) const DEFAULT_ISSUANCE_TIMEOUT_MINUTES: u32 = 45;

/// The pause between two issuance polls.
const POLL_INTERVAL: Duration = if cfg!(test) {
    Duration::from_secs(1)
} else {
    Duration::from_secs(30)
};

/// What one reconcile found and did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// No live `navigator-firm`: a first deployment provisions it normally.
    NoLiveCertificate,
    /// The live domain set already matches the target.
    Unchanged,
    /// The dry run found drift and printed the steps it would take.
    Planned {
        add: Vec<String>,
        remove: Vec<String>,
    },
    /// `navigator-firm` now carries the target set and is serving.
    Rolled {
        add: Vec<String>,
        remove: Vec<String>,
    },
}

/// The cluster and network effects the transition needs, behind a seam so
/// tests script them.
pub(crate) trait Cluster {
    fn get_json(&mut self, kind: &str, name: &str) -> Result<Option<serde_json::Value>>;
    fn apply(&mut self, document: &str) -> Result<()>;
    fn annotate_ingress(&mut self, ingress: &str, key: &str, value: &str) -> Result<()>;
    fn patch_domains(&mut self, certificate: &str, domains: &[String]) -> Result<()>;
    /// A trusted TLS handshake to `https://<host>/` through the ordinary
    /// trust store. Never insecure.
    fn tls_trusted(&mut self, host: &str) -> bool;
    fn sleep(&mut self, duration: Duration);
}

/// The live cluster through `kubectl`, and `curl` for the TLS check.
pub(crate) struct KubectlCluster<'a> {
    pub cfg: &'a ShipConfig,
}

impl Cluster for KubectlCluster<'_> {
    fn get_json(&mut self, kind: &str, name: &str) -> Result<Option<serde_json::Value>> {
        kubectl_json_if_present(self.cfg, kind, name)
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
            .context("write the staged certificate to kubectl apply")?;
        let status = child.wait().context("wait for kubectl apply")?;
        ensure!(
            status.success(),
            "kubectl apply of {NEXT_CERTIFICATE_NAME} failed"
        );
        Ok(())
    }

    fn annotate_ingress(&mut self, ingress: &str, key: &str, value: &str) -> Result<()> {
        let status = kubectl(self.cfg)
            .args([
                "annotate",
                "ingress",
                ingress,
                &format!("{key}={value}"),
                "--overwrite",
            ])
            .status()
            .context("run kubectl annotate")?;
        ensure!(
            status.success(),
            "kubectl annotate ingress {ingress} failed"
        );
        Ok(())
    }

    fn patch_domains(&mut self, certificate: &str, domains: &[String]) -> Result<()> {
        let patch = serde_json::json!({ "spec": { "domains": domains } }).to_string();
        let status = kubectl(self.cfg)
            .args([
                "patch",
                "managedcertificate",
                certificate,
                "--type=merge",
                "-p",
                &patch,
            ])
            .status()
            .context("run kubectl patch")?;
        ensure!(
            status.success(),
            "kubectl patch managedcertificate {certificate} failed"
        );
        Ok(())
    }

    fn tls_trusted(&mut self, host: &str) -> bool {
        std::process::Command::new("curl")
            .args(["--silent", "--output", "/dev/null", "--max-time", "15"])
            .arg(format!("https://{host}/"))
            .status()
            .is_ok_and(|status| status.success())
    }

    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
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

/// The annotation with [`NEXT_CERTIFICATE_NAME`] appended, or `None` when it
/// is already attached. Refuses past the target proxy's certificate limit.
fn annotation_with_next(attached: &[String]) -> Result<Option<String>> {
    if attached.iter().any(|name| name == NEXT_CERTIFICATE_NAME) {
        return Ok(None);
    }
    ensure!(
        attached.len() < TARGET_PROXY_CERTIFICATE_LIMIT,
        "cannot stage {NEXT_CERTIFICATE_NAME}: {INGRESS_NAME} already attaches {} certificates \
         and a target HTTPS proxy accepts at most {TARGET_PROXY_CERTIFICATE_LIMIT}. Retire a \
         brand's own certificate first.",
        attached.len()
    );
    let mut names = attached.to_vec();
    names.push(NEXT_CERTIFICATE_NAME.to_string());
    Ok(Some(names.join(",")))
}

/// Whether the controller reports `certificate` `Active` for exactly
/// `desired`. A status still naming the old domain set is not issuance of
/// the new one, so the per-domain list must match as well.
fn issued_for(certificate: &serde_json::Value, desired: &BTreeSet<String>) -> Result<bool> {
    if certificate_domains(certificate)? != *desired {
        return Ok(false);
    }
    let status = certificate.get("status");
    let active = status
        .and_then(|status| status.get("certificateStatus"))
        .and_then(serde_json::Value::as_str)
        == Some("Active");
    let domains: BTreeSet<String> = status
        .and_then(|status| status.get("domainStatus"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| entry.get("status").and_then(serde_json::Value::as_str) == Some("Active"))
        .filter_map(|entry| entry.get("domain").and_then(serde_json::Value::as_str))
        .map(str::to_string)
        .collect();
    Ok(active && domains == *desired)
}

/// Poll until `certificate` is issued for `desired` and every host answers
/// trusted TLS, or `timeout` elapses.
fn wait_serving<C: Cluster>(
    cluster: &mut C,
    certificate: &str,
    desired: &BTreeSet<String>,
    timeout: Duration,
) -> Result<()> {
    let mut waited = Duration::ZERO;
    loop {
        let live = cluster
            .get_json("managedcertificate", certificate)?
            .ok_or_else(|| {
                anyhow!("managedcertificate {certificate} disappeared mid-transition")
            })?;
        let untrusted: Vec<&str> = if issued_for(&live, desired)? {
            desired
                .iter()
                .map(String::as_str)
                .filter(|host| !cluster.tls_trusted(host))
                .collect()
        } else {
            desired.iter().map(String::as_str).collect()
        };
        if untrusted.is_empty() {
            return Ok(());
        }
        if waited >= timeout {
            bail!(
                "{certificate} did not serve trusted TLS within {} minutes; pending: {}. The \
                 serving certificates are untouched. A pending host must resolve to the \
                 Ingress; rerun once it does, or raise --certificate-timeout-minutes.",
                timeout.as_secs() / 60,
                untrusted.join(", ")
            );
        }
        cluster.sleep(POLL_INTERVAL);
        waited += POLL_INTERVAL;
    }
}

/// Move `navigator-firm` to `desired` without an HTTPS gap. `timeout` bounds
/// each of the two waits.
pub(crate) fn reconcile_with<C: Cluster>(
    cluster: &mut C,
    namespace: &str,
    desired: &[String],
    timeout: Duration,
    dry_run: bool,
) -> Result<Outcome> {
    let Some(firm) = cluster.get_json("managedcertificate", FIRM_CERTIFICATE_NAME)? else {
        return Ok(Outcome::NoLiveCertificate);
    };
    let live = certificate_domains(&firm)
        .with_context(|| format!("read the live {FIRM_CERTIFICATE_NAME} domains"))?;
    let target: BTreeSet<String> = desired.iter().cloned().collect();
    if live == target {
        return Ok(Outcome::Unchanged);
    }
    let add: Vec<String> = target.difference(&live).cloned().collect();
    let remove: Vec<String> = live.difference(&target).cloned().collect();
    eprintln!(
        "==> {FIRM_CERTIFICATE_NAME} domains change (add: [{}], remove: [{}]); staging \
         {NEXT_CERTIFICATE_NAME} first",
        add.join(", "),
        remove.join(", ")
    );

    let ingress = cluster
        .get_json("ingress", INGRESS_NAME)?
        .ok_or_else(|| anyhow!("ingress {INGRESS_NAME} is missing; nothing serves the brands"))?;
    let annotation = annotation_with_next(&attached_certificates(&ingress))?;
    let staged = cluster.get_json("managedcertificate", NEXT_CERTIFICATE_NAME)?;
    let staged_matches = staged
        .as_ref()
        .map(certificate_domains)
        .transpose()?
        .is_some_and(|domains| domains == target);

    if dry_run {
        if !staged_matches {
            eprintln!(
                "DRY-RUN stage {NEXT_CERTIFICATE_NAME} with [{}]",
                desired.join(", ")
            );
        }
        if let Some(value) = &annotation {
            eprintln!(
                "DRY-RUN annotate ingress {INGRESS_NAME} {MANAGED_CERTIFICATES_ANNOTATION}={value}"
            );
        }
        eprintln!(
            "DRY-RUN wait for {NEXT_CERTIFICATE_NAME}, patch {FIRM_CERTIFICATE_NAME}, wait again \
             (each wait bounded by the timeout)"
        );
        return Ok(Outcome::Planned { add, remove });
    }

    if !staged_matches {
        // Replacing a stale staged certificate is safe: `navigator-firm` is
        // still serving every live host.
        cluster.apply(&managed_certificate_document(
            namespace,
            NEXT_CERTIFICATE_NAME,
            desired,
        ))?;
    }
    if let Some(value) = annotation {
        cluster.annotate_ingress(INGRESS_NAME, MANAGED_CERTIFICATES_ANNOTATION, &value)?;
    }
    wait_serving(cluster, NEXT_CERTIFICATE_NAME, &target, timeout)?;
    eprintln!("==> {NEXT_CERTIFICATE_NAME} is Active and serves trusted TLS for every host");

    cluster.patch_domains(FIRM_CERTIFICATE_NAME, desired)?;
    wait_serving(cluster, FIRM_CERTIFICATE_NAME, &target, timeout)?;
    eprintln!("==> {FIRM_CERTIFICATE_NAME} is Active and serves trusted TLS for every host");
    Ok(Outcome::Rolled { add, remove })
}

/// [`reconcile_with`] against the live cluster `cfg` names.
pub(crate) fn reconcile(cfg: &ShipConfig, timeout_minutes: u32, dry_run: bool) -> Result<Outcome> {
    reconcile_with(
        &mut KubectlCluster { cfg },
        &cfg.namespace,
        &firm_certificate_hosts(&cfg.public_host),
        Duration::from_secs(u64::from(timeout_minutes) * 60),
        dry_run,
    )
}

/// `navigator ops certificates pre-roll`: resolve the deployment, fetch its
/// cluster credentials, and reconcile ahead of the release.
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
    match reconcile(&cfg, timeout_minutes, dry_run)? {
        Outcome::NoLiveCertificate => {
            eprintln!(
                "==> no live {FIRM_CERTIFICATE_NAME} on this deployment; ops ship provisions it"
            );
        }
        Outcome::Unchanged => {
            eprintln!("==> {FIRM_CERTIFICATE_NAME} already carries this build's hosts");
        }
        Outcome::Planned { .. } => {
            eprintln!("==> rerun without --dry-run to pre-roll this deployment");
        }
        Outcome::Rolled { .. } => {
            eprintln!(
                "==> {FIRM_CERTIFICATE_NAME} pre-rolled; the next ops ship drops \
                 {NEXT_CERTIFICATE_NAME}"
            );
        }
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

        fn apply(&mut self, document: &str) -> Result<()> {
            let doc: serde_json::Value = serde_yaml::from_str(document)?;
            let name = doc["metadata"]["name"].as_str().unwrap().to_string();
            self.log.push(format!("apply {name}"));
            self.objects
                .insert(("managedcertificate".into(), name), doc);
            Ok(())
        }

        fn annotate_ingress(&mut self, ingress: &str, key: &str, value: &str) -> Result<()> {
            self.log.push(format!("annotate {ingress} {value}"));
            let object = self
                .objects
                .get_mut(&("ingress".to_string(), ingress.to_string()))
                .unwrap();
            object["metadata"]["annotations"][key] = value.into();
            Ok(())
        }

        fn patch_domains(&mut self, certificate: &str, domains: &[String]) -> Result<()> {
            self.log.push(format!("patch {certificate}"));
            let object = self
                .objects
                .get_mut(&("managedcertificate".to_string(), certificate.to_string()))
                .unwrap();
            object["spec"]["domains"] = serde_json::json!(domains);
            Ok(())
        }

        fn tls_trusted(&mut self, host: &str) -> bool {
            !self.untrusted.contains(host)
        }

        fn sleep(&mut self, duration: Duration) {
            self.slept += duration;
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

    fn cluster(live: &[&str], attached: &str) -> FakeCluster {
        let mut fake = FakeCluster::default();
        fake.objects.insert(
            ("managedcertificate".into(), FIRM_CERTIFICATE_NAME.into()),
            certificate(FIRM_CERTIFICATE_NAME, live),
        );
        fake.objects.insert(
            ("ingress".into(), INGRESS_NAME.into()),
            serde_json::json!({
                "metadata": { "annotations": { MANAGED_CERTIFICATES_ANNOTATION: attached } },
            }),
        );
        fake
    }

    const ATTACHED: &str = "navigator-web,navigator-workflows,navigator-forgejo,navigator-firm";
    const HOUR: Duration = Duration::from_secs(3600);

    #[test]
    fn a_first_deployment_has_nothing_to_transition() {
        let mut fake = FakeCluster::default();
        let outcome = reconcile_with(&mut fake, "navigator", &hosts(&["a.test"]), HOUR, false);
        assert_eq!(outcome.unwrap(), Outcome::NoLiveCertificate);
        assert!(fake.log.is_empty());
    }

    #[test]
    fn an_unchanged_domain_set_touches_nothing_whatever_the_order() {
        let mut fake = cluster(&["b.test", "a.test"], ATTACHED);
        let outcome = reconcile_with(
            &mut fake,
            "navigator",
            &hosts(&["a.test", "b.test"]),
            HOUR,
            false,
        );
        assert_eq!(outcome.unwrap(), Outcome::Unchanged);
        assert!(fake.log.is_empty());
    }

    /// The 26.11.4 shape: one host retired and one renamed. The staged
    /// certificate must be attached and serving before `navigator-firm` is
    /// patched, and the patch waits for the new set's issuance, not the old
    /// set's lingering `Active`.
    #[test]
    fn a_host_change_stages_next_then_patches_the_firm_certificate() {
        let mut fake = cluster(&["a.test", "old.test", "gone.test"], ATTACHED);
        let target = ["a.test", "new.test"];
        fake.statuses.insert(
            NEXT_CERTIFICATE_NAME.into(),
            VecDeque::from([
                provisioning(&target),
                provisioning(&target),
                active(&target),
            ]),
        );
        fake.statuses.insert(
            FIRM_CERTIFICATE_NAME.into(),
            // The first read is the reconcile's own; the old set's status
            // then lingers after the patch before the new set provisions.
            VecDeque::from([
                active(&["a.test", "old.test", "gone.test"]),
                active(&["a.test", "old.test", "gone.test"]),
                provisioning(&target),
                active(&target),
            ]),
        );

        let outcome = reconcile_with(&mut fake, "navigator", &hosts(&target), HOUR, false).unwrap();

        assert_eq!(
            outcome,
            Outcome::Rolled {
                add: hosts(&["new.test"]),
                remove: hosts(&["gone.test", "old.test"]),
            }
        );
        assert_eq!(
            fake.log,
            [
                format!("apply {NEXT_CERTIFICATE_NAME}"),
                format!("annotate {INGRESS_NAME} {ATTACHED},{NEXT_CERTIFICATE_NAME}"),
                format!("patch {FIRM_CERTIFICATE_NAME}"),
            ]
        );
        let firm = &fake.objects[&(
            "managedcertificate".to_string(),
            FIRM_CERTIFICATE_NAME.to_string(),
        )];
        assert_eq!(
            certificate_domains(firm).unwrap(),
            target.iter().map(|h| (*h).to_string()).collect()
        );
    }

    #[test]
    fn the_firm_certificate_is_never_patched_while_a_host_lacks_trusted_tls() {
        let mut fake = cluster(&["a.test", "gone.test"], ATTACHED);
        fake.statuses.insert(
            NEXT_CERTIFICATE_NAME.into(),
            VecDeque::from([active(&["a.test"])]),
        );
        fake.untrusted.insert("a.test".into());
        let timeout = Duration::from_secs(5);
        let error = reconcile_with(&mut fake, "navigator", &hosts(&["a.test"]), timeout, false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("pending: a.test"), "{error}");
        assert!(
            !fake.log.iter().any(|line| line.starts_with("patch")),
            "{:?}",
            fake.log
        );
        assert_eq!(fake.slept, timeout);
    }

    #[test]
    fn a_staged_certificate_that_never_issues_times_out_within_its_bound() {
        let mut fake = cluster(&["a.test"], ATTACHED);
        let target = ["a.test", "unpointed.test"];
        fake.statuses.insert(
            NEXT_CERTIFICATE_NAME.into(),
            VecDeque::from(vec![provisioning(&target); 100]),
        );
        let timeout = Duration::from_secs(10);
        let error = reconcile_with(&mut fake, "navigator", &hosts(&target), timeout, false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("did not serve trusted TLS"), "{error}");
        assert_eq!(fake.slept, timeout);
    }

    /// A transition an operator already began (as on production 2026-11-04)
    /// is resumed, not restarted: the matching staged certificate is reused
    /// and the annotation is not rewritten.
    #[test]
    fn an_already_staged_and_attached_next_certificate_is_reused() {
        let attached = format!("{ATTACHED},{NEXT_CERTIFICATE_NAME}");
        let mut fake = cluster(&["a.test", "gone.test"], &attached);
        fake.objects.insert(
            ("managedcertificate".into(), NEXT_CERTIFICATE_NAME.into()),
            certificate(NEXT_CERTIFICATE_NAME, &["a.test"]),
        );
        fake.statuses.insert(
            FIRM_CERTIFICATE_NAME.into(),
            VecDeque::from([active(&["a.test", "gone.test"]), active(&["a.test"])]),
        );
        reconcile_with(&mut fake, "navigator", &hosts(&["a.test"]), HOUR, false).unwrap();
        assert_eq!(fake.log, [format!("patch {FIRM_CERTIFICATE_NAME}")]);
    }

    #[test]
    fn a_stale_staged_certificate_is_replaced_with_the_target_set() {
        let mut fake = cluster(&["a.test"], ATTACHED);
        fake.objects.insert(
            ("managedcertificate".into(), NEXT_CERTIFICATE_NAME.into()),
            certificate(NEXT_CERTIFICATE_NAME, &["stale.test"]),
        );
        fake.statuses.insert(
            NEXT_CERTIFICATE_NAME.into(),
            VecDeque::from([active(&["stale.test"]), active(&["a.test", "b.test"])]),
        );
        fake.statuses.insert(
            FIRM_CERTIFICATE_NAME.into(),
            VecDeque::from([active(&["a.test"]), active(&["a.test", "b.test"])]),
        );
        reconcile_with(
            &mut fake,
            "navigator",
            &hosts(&["a.test", "b.test"]),
            HOUR,
            false,
        )
        .unwrap();
        assert_eq!(fake.log[0], format!("apply {NEXT_CERTIFICATE_NAME}"));
    }

    #[test]
    fn staging_refuses_past_the_target_proxy_certificate_limit() {
        let full = (0..TARGET_PROXY_CERTIFICATE_LIMIT)
            .map(|i| format!("cert-{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let mut fake = cluster(&["a.test"], &full);
        let error = reconcile_with(&mut fake, "navigator", &hosts(&["b.test"]), HOUR, false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("at most 15"), "{error}");
        assert!(fake.log.is_empty());
    }

    #[test]
    fn a_dry_run_plans_the_transition_and_writes_nothing() {
        let mut fake = cluster(&["a.test", "gone.test"], ATTACHED);
        let outcome =
            reconcile_with(&mut fake, "navigator", &hosts(&["a.test"]), HOUR, true).unwrap();
        assert_eq!(
            outcome,
            Outcome::Planned {
                add: vec![],
                remove: hosts(&["gone.test"])
            }
        );
        assert!(fake.log.is_empty());
        assert_eq!(fake.slept, Duration::ZERO);
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

    /// The release workflow's ship jobs must outlast both waits at the
    /// default bound, plus the roll itself.
    #[test]
    fn the_release_ship_jobs_absorb_two_issuance_waits() {
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
        let floor = 2 * u64::from(DEFAULT_ISSUANCE_TIMEOUT_MINUTES) + 30;
        for minutes in ships {
            assert!(minutes >= floor, "ship job timeout {minutes} < {floor}");
        }
    }
}

//! `navigator-deploy dev forgejo` — the opt-in Forgejo in the local KIND tier.
//!
//! `up` adds Forgejo to a KIND tier: the retained PostgreSQL server with its
//! least-privilege roles, Forgejo's own login and database, Forgejo itself,
//! and one Actions runner, all from `k8s/overlays/kind/forgejo`. It signs in
//! through the tier's Rauthy. `smoke` proves that deployment end to end with
//! synthetic users and repositories only. `.github/workflows/forgejo-postgres.yml`
//! runs the same two commands against a fresh KIND cluster.
//!
//! This file holds the pure decisions and renderings, unit-tested here. The
//! cluster and HTTP orchestration lives in `cluster.rs` and `smoke.rs`, which
//! shell out to `kubectl`/`git` and are proven by the CI KIND job instead.
//! See `docs/forgejo-kind.md`.

pub(in crate::devx) mod bundle;
mod cluster;
pub(in crate::devx) mod controller;
mod kubeapi;
pub(in crate::devx) mod logs;
mod oidc;
pub(in crate::devx) mod policy;
pub(in crate::devx) mod release;
pub(in crate::devx) mod runner;
mod smoke;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use sha2::{Digest, Sha256};

pub(super) use cluster::up;
pub(super) use smoke::smoke;

/// The opt-in overlay `up` applies.
const OVERLAY: &str = "k8s/overlays/kind/forgejo";
/// Rauthy's KIND fixture and its reusable Deployment, applied when the
/// cluster has no Rauthy yet (a bare CI cluster).
const RAUTHY_FIXTURE: &str = "k8s/overlays/kind/rauthy";
const RAUTHY_DEPLOYMENT: &str = "k8s/staging/rauthy.yaml";
/// The issuer authority the committed KIND manifests carry; `up` rewrites it
/// to the tier's own Rauthy port.
const FIXTURE_RAUTHY_AUTHORITY: &str = "localhost:30080";
const FIXTURE_LOOPBACK_LISTEN: &str = "127.0.0.1:30080";

const STATEFULSET: &str = "navigator-forgejo";
const POD: &str = "navigator-forgejo-0";
const DATA_CLAIM: &str = "data-navigator-forgejo-0";
const POSTGRES_POD: &str = "navigator-postgres-0";
const RUNNER_DEPLOYMENT: &str = "navigator-forgejo-runner";
const RUNNER_NAME: &str = "kind-runner";
/// The runner's one label. Jobs that ask for `runs-on: self-hosted` run in
/// the runner container itself (the `host` executor).
const RUNNER_LABEL: &str = "self-hosted";

/// The Secret holding the synthetic material `up` generates once per tier.
/// Every other KIND Secret is derived from it, so re-running `up` converges
/// instead of rotating.
const SEED_SECRET: &str = "navigator-forgejo-kind";
const ADMISSION_CONFIG_MAP: &str = "navigator-forgejo-admission";
/// The KIND instance signing key: an SSH key pair `up` generates once and
/// mounts read-only, so Forgejo can sign the merges a branch requiring signed
/// commits accepts.
const SIGNING_SECRET: &str = "navigator-forgejo-signing";
const SIGNING_PUBLIC_KEY: &str = "/etc/forgejo-signing/signing_key.pub";
/// The committer identity on Forgejo-signed commits in the KIND tier.
const SIGNING_NAME: &str = "Navigator Forgejo KIND";
const SIGNING_EMAIL: &str = "forgejo-signing@example.test";
const ALLOWLIST_KEY: &str = "email_domain_allowlist";

/// Synthetic credentials for one KIND tier. Never deployment material.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Seed {
    postgres_password: String,
    app_password: String,
    migrator_password: String,
    backup_password: String,
    database_password: String,
    secret_key: String,
    internal_token: String,
    jwt_secret: String,
    runner_secret: String,
}

impl Seed {
    const KEYS: [&'static str; 9] = [
        "postgres_password",
        "app_password",
        "migrator_password",
        "backup_password",
        "database_password",
        "secret_key",
        "internal_token",
        "jwt_secret",
        "runner_secret",
    ];

    fn generate() -> Self {
        Self {
            postgres_password: random_hex(),
            app_password: random_hex(),
            migrator_password: random_hex(),
            backup_password: random_hex(),
            database_password: random_hex(),
            secret_key: random_hex(),
            internal_token: random_hex(),
            jwt_secret: URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>()),
            runner_secret: hex(&rand::random::<[u8; 20]>()),
        }
    }

    fn to_map(&self) -> BTreeMap<String, String> {
        Self::KEYS
            .iter()
            .zip([
                &self.postgres_password,
                &self.app_password,
                &self.migrator_password,
                &self.backup_password,
                &self.database_password,
                &self.secret_key,
                &self.internal_token,
                &self.jwt_secret,
                &self.runner_secret,
            ])
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect()
    }

    fn from_map(map: &BTreeMap<String, String>) -> Result<Self> {
        let get = |key: &str| {
            map.get(key)
                .filter(|value| !value.is_empty())
                .cloned()
                .with_context(|| format!("{SEED_SECRET} is missing `{key}`"))
        };
        let seed = Self {
            postgres_password: get("postgres_password")?,
            app_password: get("app_password")?,
            migrator_password: get("migrator_password")?,
            backup_password: get("backup_password")?,
            database_password: get("database_password")?,
            secret_key: get("secret_key")?,
            internal_token: get("internal_token")?,
            jwt_secret: get("jwt_secret")?,
            runner_secret: get("runner_secret")?,
        };
        ensure!(
            is_runner_secret(&seed.runner_secret),
            "{SEED_SECRET} `runner_secret` must be 40 lowercase hexadecimal characters"
        );
        Ok(seed)
    }
}

fn random_hex() -> String {
    hex(&rand::random::<[u8; 24]>())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn is_runner_secret(secret: &str) -> bool {
    secret.len() == 40
        && secret
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The browser origin Forgejo answers on in this tier.
fn root_url(forgejo_port: u16) -> String {
    format!("http://localhost:{forgejo_port}/")
}

/// Render the tier's base `app.ini` from the deployment template.
///
/// The template and its renderer are the ones `ops secrets apply --service
/// forgejo` uses, so KIND runs the production configuration. The only KIND
/// differences are the plain-HTTP loopback `ROOT_URL` and Actions, which a
/// deployment keeps off until its runner isolation is proven.
fn kind_app_ini(seed: &Seed, namespace: &str, forgejo_port: u16) -> Result<String> {
    let payloads = BTreeMap::from([
        (
            "NAVIGATOR_FORGEJO_DATABASE_PASSWORD".to_owned(),
            seed.database_password.clone(),
        ),
        (
            "NAVIGATOR_FORGEJO_SECRET_KEY".to_owned(),
            seed.secret_key.clone(),
        ),
        (
            "NAVIGATOR_FORGEJO_INTERNAL_TOKEN".to_owned(),
            seed.internal_token.clone(),
        ),
        (
            "NAVIGATOR_FORGEJO_OAUTH2_JWT_SECRET".to_owned(),
            seed.jwt_secret.clone(),
        ),
    ]);
    let rendered =
        super::deployments::render_forgejo_app_ini(&root_url(forgejo_port), namespace, &payloads)?;
    let disabled = "[actions]\nENABLED = false\n";
    ensure!(
        rendered.matches(disabled).count() == 1,
        "the Forgejo app.ini template must disable Actions exactly once"
    );
    // Forgejo signs authorized operations with its instance key; people sign
    // in through OIDC without enrolling a separate local factor or key.
    // The smoke's receiver is the host behind the Docker bridge — a private
    // address the forge's SSRF default (`external`) refuses to deliver to.
    Ok(format!(
        "{}[webhook]\nALLOWED_HOST_LIST = external,private\n[repository.signing]\nFORMAT = ssh\nSIGNING_KEY = {SIGNING_PUBLIC_KEY}\n\
         SIGNING_NAME = {SIGNING_NAME}\nSIGNING_EMAIL = {SIGNING_EMAIL}\n\
         INITIAL_COMMIT = always\nCRUD_ACTIONS = always\nMERGES = always\n",
        rendered.replacen(disabled, "[actions]\nENABLED = true\n", 1)
    ))
}

/// The runner UUID Forgejo derives from a shared registration secret: the
/// ASCII bytes of its first 16 characters.
fn runner_uuid(secret: &str) -> Result<String> {
    ensure!(
        is_runner_secret(secret),
        "a runner secret is 40 lowercase hexadecimal characters"
    );
    let bytes: [u8; 16] = secret.as_bytes()[..16].try_into().expect("checked length");
    Ok(uuid::Uuid::from_bytes(bytes).hyphenated().to_string())
}

/// The runner's `config.yml`: one connection to the in-cluster Service.
fn runner_config(seed: &Seed) -> Result<String> {
    let uuid = runner_uuid(&seed.runner_secret)?;
    Ok(format!(
        "log:\n  level: info\nrunner:\n  capacity: 1\n  labels:\n    - \"{RUNNER_LABEL}:host\"\n\
         cache:\n  enabled: false\nserver:\n  connections:\n    navigator:\n      \
         url: http://navigator-forgejo:3000/\n      uuid: {uuid}\n      token: {token}\n",
        token = seed.runner_secret,
    ))
}

/// The synthetic workflows the smoke's merge proof commits: the required
/// gate, the publication that runs on `main`, and the bot push that must fail.
const GATE_WORKFLOW: &str = include_str!("fixtures/gate.yaml");
const PUBLISH_WORKFLOW: &str = include_str!("fixtures/publish.yaml");
const BOT_PUSH_WORKFLOW: &str = include_str!("fixtures/bot-push.yaml");

/// The commit-status context Forgejo Actions posts for a one-job workflow on
/// `event`: `<workflow name> / <job name> (<event>)`, composed as in Forgejo
/// 15.0.9's `services/actions/commit_status.go`. A job without `name` is
/// named by its key.
fn actions_context(workflow: &str, event: &str) -> Result<String> {
    let parsed: serde_yaml::Value = serde_yaml::from_str(workflow).context("parse the workflow")?;
    let name = parsed["name"]
        .as_str()
        .filter(|name| !name.trim().is_empty())
        .context("the workflow names itself")?;
    let jobs = parsed["jobs"]
        .as_mapping()
        .context("the workflow has jobs")?;
    let mut jobs = jobs.iter();
    let (Some((key, job)), None) = (jobs.next(), jobs.next()) else {
        bail!("the workflow has exactly one job");
    };
    let job = job["name"]
        .as_str()
        .or_else(|| key.as_str())
        .context("the job has a name")?;
    Ok(format!("{name} / {job} ({event})"))
}

/// Point rendered KIND manifests at this tier's Rauthy port: the issuer
/// authority and the pod-local loopback bridge that serves it.
fn align_issuer(rendered: &str, rauthy_port: u16) -> String {
    rendered
        .replace(
            FIXTURE_RAUTHY_AUTHORITY,
            &format!("localhost:{rauthy_port}"),
        )
        .replace(FIXTURE_LOOPBACK_LISTEN, &format!("127.0.0.1:{rauthy_port}"))
}

/// Solve a Rauthy proof-of-work challenge (`spow` version 1): the shortest
/// decimal counter whose SHA-256 over `challenge || counter` begins with the
/// challenge's difficulty in zero bits. The solution is `challenge || counter`.
fn solve_pow(challenge: &str) -> Result<String> {
    let mut fields = challenge.split(':');
    ensure!(
        fields.next() == Some("1"),
        "unsupported Rauthy proof-of-work version in {challenge:?}"
    );
    let difficulty: u32 = fields
        .next()
        .and_then(|field| field.parse().ok())
        .filter(|difficulty| (10..99).contains(difficulty))
        .with_context(|| format!("unreadable proof-of-work difficulty in {challenge:?}"))?;
    for counter in 0u64.. {
        let candidate = format!("{challenge}{counter}");
        if leading_zero_bits(&Sha256::digest(candidate.as_bytes())) >= difficulty {
            return Ok(candidate);
        }
    }
    bail!("proof-of-work search exhausted")
}

fn leading_zero_bits(digest: &[u8]) -> u32 {
    let mut bits = 0;
    for byte in digest {
        if *byte == 0 {
            bits += 8;
        } else {
            return bits + byte.leading_zeros();
        }
    }
    bits
}

/// A minimal cookie jar: name → value, fed from `Set-Cookie` headers.
#[derive(Debug, Default, Clone)]
struct CookieJar(BTreeMap<String, String>);

impl CookieJar {
    fn store<'a>(&mut self, set_cookie: impl IntoIterator<Item = &'a str>) {
        for header in set_cookie {
            let pair = header.split(';').next().unwrap_or_default();
            if let Some((name, value)) = pair.split_once('=') {
                let name = name.trim();
                if value.is_empty() {
                    self.0.remove(name);
                } else {
                    self.0.insert(name.to_owned(), value.trim().to_owned());
                }
            }
        }
    }

    fn header(&self) -> Option<String> {
        (!self.0.is_empty()).then(|| {
            self.0
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join("; ")
        })
    }
}

/// A Rauthy login page's server-rendered `<template id="tpl_…">` value.
fn rauthy_template(html: &str, id: &str) -> Option<String> {
    let start = html.find(&format!("<template id=\"{id}\">"))? + id.len() + 16;
    let end = html[start..].find("</template>")? + start;
    Some(html[start..end].to_owned())
}

/// A Forgejo form's hidden `_csrf` value.
fn forgejo_csrf(html: &str) -> Option<String> {
    let at = html.find("name=\"_csrf\"")?;
    let rest = &html[at..];
    let value = rest.find("value=\"")? + 7;
    let end = rest[value..].find('"')? + value;
    Some(rest[value..end].to_owned())
}

/// The local port in a `kubectl port-forward` banner line.
fn forwarded_port(line: &str) -> Option<u16> {
    let rest = line.strip_prefix("Forwarding from 127.0.0.1:")?;
    rest.split_whitespace().next()?.parse().ok()
}

/// The path and query of an absolute URL, for replaying a redirect against a
/// port-forward rather than the host it names.
fn path_and_query(location: &str) -> Result<String> {
    let url = url::Url::parse(location).with_context(|| format!("parse URL {location}"))?;
    Ok(match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_owned(),
    })
}

// --- The push-webhook proof's decisions -------------------------------------
//
// Pure, so the unit tests below hold them; the KIND orchestration that
// produces their inputs lives in `smoke.rs`.

/// One journaled push receipt, as the smoke's psql row carries it:
/// `status|change_id|author|description|conflict|failure_detail`, empty
/// strings where the pipeline has not yet written.
#[derive(Debug)]
pub(super) struct PushReceiptRow {
    pub(super) status: String,
    pub(super) change_id: String,
    pub(super) author: String,
    pub(super) description_summary: String,
    pub(super) conflict_signal: String,
    pub(super) failure_detail: String,
}

/// Parse one pipe-joined receipt row. The failure detail is last and may
/// itself carry pipes, so the split stops at six fields.
pub(super) fn parse_push_receipt_row(row: &str) -> Result<PushReceiptRow, String> {
    let fields: [&str; 6] = row
        .splitn(6, '|')
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| format!("receipt row {row:?} is not six pipe-separated fields"))?;
    let [status, change_id, author, description, conflict, failure] = fields;
    Ok(PushReceiptRow {
        status: status.to_owned(),
        change_id: change_id.to_owned(),
        author: author.to_owned(),
        description_summary: description.to_owned(),
        conflict_signal: conflict.to_owned(),
        failure_detail: failure.to_owned(),
    })
}

/// True when the pipeline will not advance the receipt further — the
/// smoke's poll stops here.
pub(super) fn receipt_is_terminal(status: &str) -> bool {
    matches!(status, "delivered" | "failed" | "skipped")
}

/// The push-webhook proof: the receipt reached `delivered` — the terminal
/// state a clean push ends in — carrying the analysis columns: the pushed
/// bookmark's change id, its author, its one-line description, and no
/// conflict signal. `Err` names what the journal actually holds.
pub(super) fn assert_analyzed_push_receipt(
    row: &PushReceiptRow,
    change_id: &str,
    description: &str,
) -> Result<(), String> {
    match row.status.as_str() {
        "delivered" => {}
        "failed" => {
            return Err(format!(
                "the push receipt failed at consumption: {}",
                row.failure_detail
            ));
        }
        "skipped" => return Err("the push receipt was skipped, not analyzed".to_owned()),
        other => {
            return Err(format!(
                "the push receipt never finished consumption: {other}"
            ));
        }
    }
    if row.change_id != change_id {
        return Err(format!(
            "the journal analyzed change {} for this push, not {change_id}",
            row.change_id
        ));
    }
    if row.author.is_empty() {
        return Err("the journal holds no author for the analyzed push".to_owned());
    }
    if row.description_summary != description {
        return Err(format!(
            "the journal analyzed description {:?}, not {description:?}",
            row.description_summary
        ));
    }
    if row.conflict_signal != "false" {
        return Err(format!(
            "the clean push carried a conflict signal of {:?}",
            row.conflict_signal
        ));
    }
    Ok(())
}

/// The receiver probe a pod runs over the host gateway: one raw HTTP
/// request through bash's `/dev/tcp` (the image carries no curl), answered
/// by any status line — a wrong-secret refusal is reachability proven.
pub(super) fn http_probe_script(host: &str, port: u16, path: &str) -> String {
    format!(
        "exec 3<>/dev/tcp/{host}/{port} && \
         printf 'GET {path} HTTP/1.1\\r\\nHost: {host}\\r\\nConnection: close\\r\\n\\r\\n' >&3 && \
         head -n1 <&3"
    )
}

/// The in-cluster name of the receiver's Service: pods dial it, and
/// kube-proxy delivers to the host gateway from the node's own network
/// namespace — a path the pod overlay does not blackhole.
pub(super) const RECEIVER_SERVICE: &str = "forgejo-receiver";

/// The smoke's kubectl context namespace: every smoke resource, including
/// the receiver's Service, lives there.
pub(super) const RECEIVER_NAMESPACE: &str = "navigator";

/// The receiver's in-cluster Service and manual `EndpointSlice`: Forgejo
/// dials the Service name, and kube-proxy delivers to the address the
/// receiver listens on — the postgres pod hosting the smoke's receiver.
/// The backend is a manual `EndpointSlice` because modern Kubernetes
/// programs kube-proxy from `EndpointSlice` objects; a manual `Endpoints`
/// object is deprecated and gets no backends, leaving the Service VIP
/// blackholed.
pub(super) fn receiver_service_manifest(endpoint_ip: &str, port: u16) -> String {
    format!(
        "apiVersion: v1\nkind: Service\nmetadata:\n  name: {RECEIVER_SERVICE}\n  namespace: {RECEIVER_NAMESPACE}\nspec:\n  type: ClusterIP\n  ports:\n    - port: 80\n      targetPort: {port}\n---\napiVersion: discovery.k8s.io/v1\nkind: EndpointSlice\nmetadata:\n  name: {RECEIVER_SERVICE}\n  namespace: {RECEIVER_NAMESPACE}\n  labels:\n    kubernetes.io/service-name: {RECEIVER_SERVICE}\naddressType: IPv4\nendpoints:\n  - addresses:\n      - {endpoint_ip}\nports:\n  - port: {port}\n    protocol: TCP\n"
    )
}

/// The receiver's in-cluster URL: Forgejo delivers webhooks through the
/// Service, not to a host address pods cannot reach.
pub(super) fn receiver_service_url() -> String {
    format!("http://{RECEIVER_SERVICE}.{RECEIVER_NAMESPACE}.svc.cluster.local")
}

/// The fixed port the in-pod receiver serves on: the pod's network
/// namespace is private, so a fixed port never collides.
pub(super) const RECEIVER_PORT: u16 = 41080;

/// How long the Service probe waits for the Service to deliver before
/// giving up: Endpoint programming after the Endpoints apply is normally
/// sub-second, but a single six-second bet loses the whole CI cycle to a
/// slow controller.
pub(super) const SERVICE_PROBE_DEADLINE: Duration = Duration::from_secs(90);

/// The Service probe's timeout message: a direct dial to the receiver on
/// loopback splits a dead receiver from a broken route — a loopback answer
/// indicts the probe's pod-to-self route (which the real delivery never
/// rides), a silence indicts the receiver process.
pub(super) fn service_probe_failure_message(direct_answer: Option<&str>) -> String {
    match direct_answer {
        Some(answer) => format!(
            "the receiver still answers directly ({answer}) — the probe's \
             pod-to-self Service route is the suspect; the real push \
             delivery rides a peer pod's route and is the verdict"
        ),
        None => "the receiver no longer answers directly either — the \
                 receiver process died, not the route"
            .to_owned(),
    }
}

/// Where the receiver's binary, config, log, and pid live inside the
/// hosting pod's filesystem.
pub(super) const RECEIVER_BINARY: &str = "/tmp/forgejo-receiver/navigator-deploy";
pub(super) const RECEIVER_CONFIG: &str = "/tmp/forgejo-receiver/receiver.json";
pub(super) const RECEIVER_LOG: &str = "/tmp/forgejo-receiver/receiver.log";
pub(super) const RECEIVER_PID: &str = "/tmp/forgejo-receiver/receiver.pid";

/// The receiver pod's in-cluster Forgejo API base: the analysis reads
/// pushed changes over the cluster's own Service, in plain HTTP.
pub(super) const RECEIVER_FORGE_URL: &str = "http://navigator-forgejo:3000";

/// The pod command that (re)starts the receiver: any previous receiver dies
/// first so re-runs are idempotent, the new one detaches with nohup so the
/// exec session can end, and its pid is recorded for a later stop.
pub(super) fn receiver_start_script() -> String {
    format!(
        "mkdir -p $(dirname {RECEIVER_CONFIG}) && \
         kill $(cat {RECEIVER_PID} 2>/dev/null) 2>/dev/null; \
         nohup {RECEIVER_BINARY} dev forgejo smoke --receiver-config {RECEIVER_CONFIG} \
         >>{RECEIVER_LOG} 2>&1 & echo $! > {RECEIVER_PID}; echo started"
    )
}

/// The workspace root for a crate's manifest directory: the deploy crate
/// sits directly under the workspace root, so one ancestor up holds the
/// `target/` tree. Landing on a higher ancestor silently misses the
/// prebuilt musl receiver and falls back to a binary the pod cannot run.
pub(super) fn workspace_root_from(manifest_dir: &Path) -> Option<&Path> {
    manifest_dir.ancestors().nth(1)
}

/// The kubectl port-forward arguments for an in-cluster target: the target
/// reference and remote port as separate arguments, with an empty local port
/// so kubectl allocates a free one (the `up` phase may already hold the
/// fixed lanes).
pub(super) fn port_forward_args(target: &str, remote_port: u16) -> [String; 2] {
    [target.to_owned(), format!(":{remote_port}")]
}

/// The receiver binary to stage into the pod: an explicit override when the
/// caller points at one, else the workspace's prebuilt musl binary. A
/// glibc-linked binary cannot serve from the Debian pod — the runner's
/// libc is newer than the pod's — so the CI job stages a static musl build
/// at `target/receiver/navigator-deploy` and drops the target directory
/// behind it.
pub(super) fn receiver_binary_source(
    workspace_root: &Path,
    override_path: Option<&str>,
) -> Option<PathBuf> {
    if let Some(path) = override_path {
        return Some(PathBuf::from(path));
    }
    let prebuilt = workspace_root.join("target/receiver/navigator-deploy");
    prebuilt.is_file().then_some(prebuilt)
}

/// The synthetic email-domain policy the smoke opens, and the identities it
/// signs in with. Each pairs with a user in the Rauthy KIND fixture.
const SMOKE_ALLOWLIST: &str = "example.test";
const ALLOWED_IDENTITY: &str = "forge-allowed@example.test";
const DENIED_IDENTITIES: [(&str, &str); 3] = [
    ("forge-denied@example.org", "a domain off the allowlist"),
    (
        "forge-lookalike@notexample.test",
        "a suffix lookalike of an allowed domain",
    ),
    ("forge-unverified@example.test", "an unverified address"),
];
const FIXTURE_PASSWORD: &str = "password";

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// Synthetic material is assembled at runtime so no credential-shaped
    /// literal sits in the tree (see `.gitleaks.toml`).
    const SYNTHETIC_HEX: &str = "0123456789abcdef0123456789abcdef";

    fn synthetic_runner_secret() -> String {
        format!("{SYNTHETIC_HEX}{}", &SYNTHETIC_HEX[..8])
    }

    fn seed() -> Seed {
        Seed {
            postgres_password: "synthetic-postgres".into(),
            app_password: "synthetic-app".into(),
            migrator_password: "synthetic-migrator".into(),
            backup_password: "synthetic-backup".into(),
            database_password: "synthetic-forgejo".into(),
            secret_key: "synthetic-secret-key".into(),
            internal_token: "synthetic-internal-token".into(),
            jwt_secret: URL_SAFE_NO_PAD.encode(SYNTHETIC_HEX.as_bytes()),
            runner_secret: synthetic_runner_secret(),
        }
    }

    #[test]
    fn a_generated_seed_round_trips_through_its_secret() {
        let generated = Seed::generate();
        assert_eq!(Seed::from_map(&generated.to_map()).unwrap(), generated);
        assert!(is_runner_secret(&generated.runner_secret));
        assert_eq!(
            URL_SAFE_NO_PAD.decode(&generated.jwt_secret).unwrap().len(),
            32
        );
        assert_ne!(Seed::generate(), generated, "each tier gets its own seed");
    }

    #[test]
    fn the_receiver_probe_script_requests_one_url_over_dev_tcp() {
        let script = http_probe_script("172.18.0.1", 42903, "/webhook/forgejo/probe");
        assert!(
            script.contains("exec 3<>/dev/tcp/172.18.0.1/42903"),
            "{script}"
        );
        assert!(
            script.contains("GET /webhook/forgejo/probe HTTP/1.1"),
            "{script}"
        );
        assert!(script.contains("Host: 172.18.0.1"), "{script}");
        assert!(script.contains(r"\r\n"), "{script}");
        assert!(script.ends_with("head -n1 <&3"), "{script}");
    }

    #[test]
    fn the_receiver_manifest_publishes_the_gateway_endpoint() {
        let manifest = receiver_service_manifest("172.18.0.1", 42903);
        assert!(manifest.contains("name: forgejo-receiver"), "{manifest}");
        assert!(manifest.contains("namespace: navigator"), "{manifest}");
        assert!(manifest.contains("discovery.k8s.io/v1"), "{manifest}");
        assert!(manifest.contains("kind: EndpointSlice"), "{manifest}");
        assert!(
            manifest.contains("kubernetes.io/service-name: forgejo-receiver"),
            "{manifest}"
        );
        assert!(manifest.contains("- 172.18.0.1"), "{manifest}");
        assert!(manifest.contains("port: 42903"), "{manifest}");
        assert!(
            !manifest.contains("kind: Endpoints\n"),
            "a manual Endpoints object is deprecated and gets no backends: {manifest}"
        );
        assert_eq!(
            receiver_service_url(),
            "http://forgejo-receiver.navigator.svc.cluster.local"
        );
    }

    #[test]
    fn an_incomplete_seed_is_refused_by_name() {
        let mut map = seed().to_map();
        map.remove("runner_secret");
        let error = Seed::from_map(&map).unwrap_err().to_string();
        assert!(error.contains("runner_secret"), "{error}");
        let mut map = seed().to_map();
        map.insert("runner_secret".into(), "short".into());
        assert!(Seed::from_map(&map).is_err());
    }

    #[test]
    fn the_kind_app_ini_is_the_deployment_template_with_actions_on() {
        let ini = kind_app_ini(&seed(), "navigator", 3300).unwrap();
        assert!(ini.contains("ROOT_URL = http://localhost:3300/"));
        assert!(ini.contains("HOST = navigator-postgres.navigator.svc.cluster.local:5432"));
        assert!(ini.contains("PASSWD = synthetic-forgejo"));
        assert!(ini.contains("SSL_MODE = verify-full"));
        assert!(ini.contains("[actions]\nENABLED = true\n"));
        assert!(!ini.contains("ENABLED = false\n[log]"));
        // The smoke's receiver is a private Docker-bridge address; the forge
        // must be allowed to deliver there.
        assert!(ini.contains("[webhook]\nALLOWED_HOST_LIST = external,private\n"));
        assert!(ini.ends_with(
            "[repository.signing]\nFORMAT = ssh\n\
             SIGNING_KEY = /etc/forgejo-signing/signing_key.pub\n\
             SIGNING_NAME = Navigator Forgejo KIND\n\
             SIGNING_EMAIL = forgejo-signing@example.test\n\
             INITIAL_COMMIT = always\nCRUD_ACTIONS = always\nMERGES = always\n"
        ));
        // Registration stays exactly as closed as a deployment's.
        assert!(ini.contains("DISABLE_REGISTRATION = true"));
        assert!(ini.contains("ENABLE_INTERNAL_SIGNIN = false"));
        assert!(ini.contains("ENABLE_AUTO_REGISTRATION = false"));
        assert!(ini.contains("ACCOUNT_LINKING = disabled"));
        assert!(!ini.contains("__NAVIGATOR_"));
    }

    #[test]
    fn the_merge_fixtures_name_their_contexts_and_triggers() {
        assert_eq!(
            actions_context(GATE_WORKFLOW, "pull_request").unwrap(),
            "gate / gate (pull_request)"
        );
        assert_eq!(
            actions_context(PUBLISH_WORKFLOW, "push").unwrap(),
            "publish / publish (push)"
        );
        let on = |workflow: &str| {
            serde_yaml::from_str::<serde_yaml::Value>(workflow).unwrap()["on"].clone()
        };
        assert_eq!(
            on(GATE_WORKFLOW),
            serde_yaml::from_str::<serde_yaml::Value>("[pull_request]").unwrap()
        );
        assert_eq!(
            on(PUBLISH_WORKFLOW),
            serde_yaml::from_str::<serde_yaml::Value>("{push: {branches: [main]}}").unwrap()
        );
        assert_eq!(
            on(BOT_PUSH_WORKFLOW),
            serde_yaml::from_str::<serde_yaml::Value>("[workflow_dispatch]").unwrap()
        );
        // Each runs on the KIND runner's only label.
        for workflow in [GATE_WORKFLOW, PUBLISH_WORKFLOW, BOT_PUSH_WORKFLOW] {
            assert!(workflow.contains(&format!("runs-on: {RUNNER_LABEL}\n")));
        }
    }

    /// The gate a Forgejo-backed Project repository is scaffolded with posts
    /// the context `main`'s branch protection requires.
    #[test]
    fn the_scaffolded_project_gate_posts_the_required_context() {
        use project_scaffold::forgejo_gate_workflow;
        use project_scaffold::FORGEJO_REQUIRED_CHECK;
        let workflow = forgejo_gate_workflow();
        assert_eq!(
            actions_context(&workflow, "pull_request").unwrap(),
            FORGEJO_REQUIRED_CHECK
        );
        assert!(workflow.contains(&format!("runs-on: {RUNNER_LABEL}\n")));
    }

    #[test]
    fn an_actions_context_needs_a_named_workflow_with_one_job() {
        let named_job = "name: ci\njobs:\n  build:\n    name: Build\n    runs-on: x\n";
        assert_eq!(
            actions_context(named_job, "push").unwrap(),
            "ci / Build (push)"
        );
        for workflow in [
            "jobs:\n  build:\n    runs-on: x\n",
            "name: ci\n",
            "name: ci\njobs:\n  a:\n    runs-on: x\n  b:\n    runs-on: x\n",
            "name: \"\"\njobs:\n  a:\n    runs-on: x\n",
        ] {
            assert!(actions_context(workflow, "push").is_err(), "{workflow}");
        }
    }

    #[test]
    fn the_runner_uuid_matches_forgejos_shared_secret_derivation() {
        // `forgejo-runner create-runner-file --secret 0123…4567` writes this
        // UUID, and `forgejo forgejo-cli actions register` stores the same.
        assert_eq!(
            runner_uuid(&synthetic_runner_secret()).unwrap(),
            "30313233-3435-3637-3839-616263646566"
        );
        assert!(runner_uuid("not-a-secret").is_err());
    }

    #[test]
    fn the_runner_config_connects_to_the_service_with_its_secret() {
        let config = runner_config(&seed()).unwrap();
        assert!(config.contains("url: http://navigator-forgejo:3000/"));
        assert!(config.contains("uuid: 30313233-3435-3637-3839-616263646566"));
        assert!(config.contains(&format!("token: {}", synthetic_runner_secret())));
        assert!(config.contains("\"self-hosted:host\""));
        assert!(config.contains("cache:\n  enabled: false"));
    }

    #[test]
    fn align_issuer_moves_every_fixture_issuer_to_the_tier_port() {
        let rendered =
            "discovery_url: http://localhost:30080/auth/v1/.well-known/openid-configuration\n\
                        listen 127.0.0.1:30080;\nnodePort: 30080\n";
        let aligned = align_issuer(rendered, 20_445);
        assert!(aligned.contains("http://localhost:20445/auth/v1/"));
        assert!(aligned.contains("listen 127.0.0.1:20445;"));
        assert!(
            aligned.contains("nodePort: 30080"),
            "only the issuer authority moves; the NodePort stays fixed"
        );
        assert_eq!(align_issuer(rendered, 30_080), rendered);
    }

    #[test]
    fn solve_pow_meets_the_challenge_difficulty() {
        let challenge =
            "1:12:1791857091:ZPIaeOfkFH+clnVg:XzkjR30s2DMDxin9lN1QSoj86ixaxNfkRGRIrdPhfp4:";
        let solution = solve_pow(challenge).unwrap();
        let counter = solution.strip_prefix(challenge).unwrap();
        assert!(counter.parse::<u64>().is_ok());
        assert!(leading_zero_bits(&Sha256::digest(solution.as_bytes())) >= 12);
        assert!(solve_pow("2:12:x:y:z:").is_err());
        assert!(solve_pow("1:9:x:y:z:").is_err());
    }

    #[test]
    fn leading_zero_bits_counts_across_bytes() {
        assert_eq!(leading_zero_bits(&[0, 0b0001_0000, 0xff]), 11);
        assert_eq!(leading_zero_bits(&[0x80]), 0);
        assert_eq!(leading_zero_bits(&[0, 0]), 16);
    }

    #[test]
    fn the_cookie_jar_keeps_the_latest_value_and_drops_cleared_cookies() {
        let mut jar = CookieJar::default();
        assert_eq!(jar.header(), None);
        jar.store(["i_like_forgejo=abc; Path=/; HttpOnly", "_csrf=one; Path=/"]);
        jar.store(["_csrf=two; Path=/", "i_like_forgejo=; Max-Age=0"]);
        assert_eq!(jar.header().as_deref(), Some("_csrf=two"));
    }

    #[test]
    fn page_values_are_read_from_rauthy_and_forgejo_markup() {
        let rauthy = r#"<template id="tpl_is_reg_open">false</template><template id="tpl_csrf_token">Q4rY</template>"#;
        assert_eq!(
            rauthy_template(rauthy, "tpl_csrf_token").as_deref(),
            Some("Q4rY")
        );
        assert_eq!(rauthy_template(rauthy, "tpl_missing"), None);
        let forgejo = r#"<form><input type="hidden" name="_csrf" value="tok-123"><input name="user_name"></form>"#;
        assert_eq!(forgejo_csrf(forgejo).as_deref(), Some("tok-123"));
        assert_eq!(forgejo_csrf("<form></form>"), None);
    }

    #[test]
    fn forwarded_port_reads_kubectls_banner() {
        assert_eq!(
            forwarded_port("Forwarding from 127.0.0.1:54321 -> 3000"),
            Some(54_321)
        );
        assert_eq!(forwarded_port("Forwarding from [::1]:54321 -> 3000"), None);
        assert_eq!(forwarded_port("Handling connection for 54321"), None);
    }

    #[test]
    fn path_and_query_drops_the_origin() {
        assert_eq!(
            path_and_query("http://localhost:3300/user/oauth2/rauthy/callback?code=a&state=b")
                .unwrap(),
            "/user/oauth2/rauthy/callback?code=a&state=b"
        );
        assert_eq!(path_and_query("http://localhost:3300/").unwrap(), "/");
    }

    #[test]
    fn the_service_probe_failure_message_splits_dead_receiver_from_broken_path() {
        let with_answer = service_probe_failure_message(Some("HTTP/1.1 405 Method Not Allowed"));
        assert!(
            with_answer.contains("still answers directly"),
            "{with_answer}"
        );
        assert!(with_answer.contains("pod-to-self"), "{with_answer}");
        let without = service_probe_failure_message(None);
        assert!(without.contains("receiver process died"), "{without}");
    }

    #[test]
    fn the_receiver_start_script_detaches_and_records_its_pid() {
        let script = receiver_start_script();
        assert!(
            script.contains("kill $(cat /tmp/forgejo-receiver/receiver.pid"),
            "{script}"
        );
        assert!(
            script.contains("nohup /tmp/forgejo-receiver/navigator-deploy dev forgejo smoke"),
            "{script}"
        );
        assert!(
            script.contains("--receiver-config /tmp/forgejo-receiver/receiver.json"),
            "{script}"
        );
        assert!(
            script.contains("echo $! > /tmp/forgejo-receiver/receiver.pid"),
            "{script}"
        );
    }

    #[test]
    fn the_port_forward_asks_for_a_free_local_port() {
        let [target, local] = port_forward_args("pod/navigator-postgres-0", 5432);
        assert_eq!(target, "pod/navigator-postgres-0");
        assert_eq!(
            local, ":5432",
            "an empty local port lets kubectl allocate one; the fixed lanes may already be held"
        );
    }

    #[test]
    fn the_workspace_root_is_one_ancestor_above_the_manifest_dir() {
        let root = tempfile::tempdir().unwrap();
        let manifest_dir = root.path().join("deploy");
        std::fs::create_dir_all(&manifest_dir).unwrap();
        assert_eq!(
            workspace_root_from(&manifest_dir),
            Some(root.path()),
            "the prebuilt musl receiver lives under the root's target/; \
             a higher ancestor misses it and stages an unrunnable binary"
        );
    }

    #[test]
    fn the_receiver_binary_prefers_the_prebuilt_musl_build() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            receiver_binary_source(dir.path(), None).is_none(),
            "no prebuilt binary means the caller falls back to its own"
        );
        let prebuilt = dir.path().join("target/receiver");
        std::fs::create_dir_all(&prebuilt).unwrap();
        let binary = prebuilt.join("navigator-deploy");
        std::fs::write(&binary, b"binary").unwrap();
        assert_eq!(
            receiver_binary_source(dir.path(), None).as_deref(),
            Some(binary.as_path())
        );
        let override_target = dir.path().join("other");
        std::fs::write(&override_target, b"binary").unwrap();
        assert_eq!(
            receiver_binary_source(dir.path(), Some(override_target.to_str().unwrap())).as_deref(),
            Some(override_target.as_path()),
            "the explicit override wins over the prebuilt path"
        );
    }

    #[test]
    fn a_delivered_receipt_with_the_analysis_columns_is_the_proof() {
        let row = parse_push_receipt_row(
            "delivered|qsmoke|Forgejo Smoke <forge-allowed@example.test>|Amend the fee \
             schedule|false|",
        )
        .unwrap();
        assert!(receipt_is_terminal(&row.status));
        assert_analyzed_push_receipt(&row, "qsmoke", "Amend the fee schedule").unwrap();
    }

    #[test]
    fn every_unfinished_receipt_is_named() {
        let failed = parse_push_receipt_row(
            "failed|qsmoke|T-bot (tbot@neonlaw.org)|Amend the fee schedule|false|\
             Slack delivery failed: the Firm's Slack bot token is not configured",
        )
        .unwrap();
        let error =
            assert_analyzed_push_receipt(&failed, "qsmoke", "Amend the fee schedule").unwrap_err();
        assert!(error.contains("failed at consumption"), "{error}");
        assert!(error.contains("Slack bot token"), "{error}");

        let skipped = parse_push_receipt_row("skipped|||||").unwrap();
        assert!(receipt_is_terminal(&skipped.status));
        assert!(assert_analyzed_push_receipt(&skipped, "qsmoke", "x")
            .unwrap_err()
            .contains("skipped"));

        let pending = parse_push_receipt_row("pending|||||").unwrap();
        assert!(!receipt_is_terminal(&pending.status));
        assert!(assert_analyzed_push_receipt(&pending, "qsmoke", "x")
            .unwrap_err()
            .contains("never finished consumption"));
    }

    #[test]
    fn a_delivered_receipt_missing_its_analysis_is_refused() {
        let row = parse_push_receipt_row("delivered|||||").unwrap();
        assert!(
            assert_analyzed_push_receipt(&row, "qsmoke", "Amend the fee schedule")
                .unwrap_err()
                .contains("analyzed change ")
        );
        let wrong_description =
            parse_push_receipt_row("delivered|qsmoke|Forgejo Smoke|Amend nothing|false|").unwrap();
        assert!(assert_analyzed_push_receipt(
            &wrong_description,
            "qsmoke",
            "Amend the fee schedule"
        )
        .unwrap_err()
        .contains("analyzed description"));
        let conflicted =
            parse_push_receipt_row("delivered|qsmoke|Forgejo Smoke|Amend the fee schedule|true|")
                .unwrap();
        assert!(
            assert_analyzed_push_receipt(&conflicted, "qsmoke", "Amend the fee schedule")
                .unwrap_err()
                .contains("conflict signal")
        );
    }

    #[test]
    fn a_row_that_is_not_six_fields_is_refused() {
        assert!(parse_push_receipt_row("delivered|qsmoke")
            .unwrap_err()
            .contains("not six pipe-separated fields"));
        // The failure detail is last and may itself carry pipes.
        let detailed = parse_push_receipt_row("failed|qsmoke|||false|one|two").unwrap();
        assert_eq!(detailed.failure_detail, "one|two");
    }

    #[test]
    fn the_deployment_template_uses_oidc_without_a_local_second_factor() {
        let template =
            std::fs::read_to_string(workspace().join("k8s/forgejo/app.ini.example")).unwrap();
        let security = template
            .split("[security]")
            .nth(1)
            .and_then(|rest| rest.split("\n[").next())
            .unwrap();
        assert!(security
            .lines()
            .any(|line| line == "GLOBAL_TWO_FACTOR_REQUIREMENT = none"));
        let identity =
            std::fs::read_to_string(workspace().join("k8s/forgejo/identity.sh")).unwrap();
        assert!(
            identity.contains("--skip-local-2fa"),
            "the OIDC source must not add a local second-factor challenge"
        );
        // Forgejo checks the reserved administrator's address against
        // EMAIL_DOMAIN_ALLOWLIST, so a fixed `forgejo.invalid` crash-loops every
        // environment whose admission opened registration (staging, 26.10.16-rc.2).
        assert!(
            identity.contains("EMAIL_DOMAIN_ALLOWLIST")
                && identity.contains("navigator-admin@${admin_domain:-forgejo.invalid}"),
            "the reserved administrator must take an allowlisted domain"
        );
    }

    #[test]
    fn every_smoke_identity_is_a_synthetic_rauthy_fixture_user() {
        let fixture = std::fs::read_to_string(
            workspace().join("k8s/overlays/kind/rauthy/local-fixture.yaml"),
        )
        .unwrap();
        for email in std::iter::once(ALLOWED_IDENTITY).chain(DENIED_IDENTITIES.map(|(e, _)| e)) {
            assert!(
                fixture.contains(&format!("\"email\": \"{email}\"")),
                "{email} must be a Rauthy fixture user"
            );
            let domain = email.split('@').nth(1).unwrap();
            assert!(
                domain == "example.test" || domain == "notexample.test" || domain == "example.org",
                "{email} must use a reserved example domain"
            );
        }
        assert!(fixture.contains("\"id\": \"navigator-forgejo\""));
    }

    fn workspace() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the cli crate has a workspace parent")
            .to_path_buf()
    }

    /// Run `k8s/forgejo/admission.sh` against a synthetic base config.
    fn admission(allowlist: Option<&str>) -> (String, String) {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        let admission = dir.path().join("admission");
        let out = dir.path().join("out");
        for path in [&base, &admission, &out] {
            std::fs::create_dir(path).unwrap();
        }
        let ini = kind_app_ini(&seed(), "navigator", 3300).unwrap();
        std::fs::write(base.join("app.ini"), ini).unwrap();
        std::fs::write(base.join("oauth2_jwt_secret"), "synthetic").unwrap();
        if let Some(allowlist) = allowlist {
            std::fs::write(admission.join(ALLOWLIST_KEY), allowlist).unwrap();
        }
        let output = Command::new("sh")
            .arg(workspace().join("k8s/forgejo/admission.sh"))
            .env("FORGEJO_BASE_CONFIG_DIR", &base)
            .env("FORGEJO_ADMISSION_DIR", &admission)
            .env("FORGEJO_CONFIG_DIR", &out)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(out.join("oauth2_jwt_secret").is_file());
        (
            std::fs::read_to_string(out.join("app.ini")).unwrap(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    fn assert_closed(ini: &str) {
        assert!(ini.contains("DISABLE_REGISTRATION = true\n"), "{ini}");
        assert!(ini.contains("EMAIL_DOMAIN_ALLOWLIST = \n"), "{ini}");
        assert!(ini.contains("ENABLE_AUTO_REGISTRATION = false\n"), "{ini}");
    }

    #[test]
    fn a_missing_or_empty_allowlist_admits_no_one() {
        assert_closed(&admission(None).0);
        assert_closed(&admission(Some("")).0);
        assert_closed(&admission(Some(" \n")).0);
    }

    #[test]
    fn a_malformed_allowlist_admits_no_one() {
        for allowlist in [
            "*",
            "*.example.test",
            "example",
            "example.test,",
            "example.test;evil.test",
            "exa mple.test",
            "-example.test",
            "example.test\nDISABLE_REGISTRATION = false",
        ] {
            let (ini, stderr) = admission(Some(allowlist));
            assert_closed(&ini);
            assert!(
                stderr.contains("registration stays closed"),
                "{allowlist:?}: {stderr}"
            );
            assert_eq!(
                ini.matches("DISABLE_REGISTRATION").count(),
                1,
                "{allowlist:?}"
            );
        }
    }

    #[test]
    fn a_valid_allowlist_opens_only_external_registration_for_those_domains() {
        let (ini, _) = admission(Some("Example.TEST, brand.example.org\n"));
        assert!(ini.contains("DISABLE_REGISTRATION = false\n"));
        assert!(ini.contains("EMAIL_DOMAIN_ALLOWLIST = example.test,brand.example.org\n"));
        assert!(ini.contains("ENABLE_AUTO_REGISTRATION = true\n"));
        // Every other gate stays shut: no local signup, no password sign-in,
        // no OpenID 2.0 signup, and no linking an external login by email.
        assert!(ini.contains("ALLOW_ONLY_EXTERNAL_REGISTRATION = true"));
        assert!(ini.contains("ENABLE_INTERNAL_SIGNIN = false"));
        assert!(ini.contains("ENABLE_OPENID_SIGNUP = false"));
        assert!(ini.contains("ACCOUNT_LINKING = disabled"));
    }
}

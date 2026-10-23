//! The moving parts around one e2e run: the host `web` process, the two
//! switchable TCP proxies, the signed webhook client, and read access to the
//! `SurrealDB`, object storage, and Restate admin state the scenarios assert on.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use base64::Engine as _;
use p256::ecdsa::signature::Signer as _;
use p256::ecdsa::{Signature, SigningKey};
use p256::pkcs8::EncodePublicKey as _;
use store::email_deliveries::EmailDelivery;
use store::email_receipts::EmailReceipt;
use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;

use super::fixtures::Fixtures;
use super::KindConfig;

/// The envelope recipient the summary lane accepts.
pub const MAILBOX: &str = "support@e2e.example.test";
pub const DEPLOYMENT: &str = "email-summary-e2e";
pub const CHANNEL: &str = "C0E2ESYNTH";
const PROJECT: &str = "synthetic-project";
const IN_CLUSTER_RESTATE: &str = "http://restate.restate.svc.cluster.local:8080";
const SIGNING_SEED: [u8; 32] = [0x42; 32];
const WORKER_SELECTOR: &str = "app=workflows-service";

/// The base64 DER public key `web` verifies webhook signatures against.
pub fn public_key() -> Result<String> {
    let key = SigningKey::from_slice(&SIGNING_SEED).context("derive the synthetic signing key")?;
    let der = key
        .verifying_key()
        .to_public_key_der()
        .context("encode the synthetic public key")?;
    Ok(base64::engine::general_purpose::STANDARD.encode(der.as_bytes()))
}

fn summary_env(restate_url: &str, matter_project_id: Uuid) -> Result<Vec<(String, String)>> {
    let mut env: Vec<(String, String)> = [
        ("NAVIGATOR_SUMMARY_ENABLED", "1"),
        ("NAVIGATOR_SUMMARY_ENVELOPE_RECIPIENTS", MAILBOX),
        ("NAVIGATOR_DEPLOYMENT_ID", DEPLOYMENT),
        ("NAVIGATOR_GCP_PROJECT_ID", PROJECT),
        ("NAVIGATOR_SUMMARY_CHANNEL_ID", CHANNEL),
        (
            "NAVIGATOR_SUMMARY_FIRM_ID",
            "00000000-0000-4000-8000-000000000001",
        ),
        ("NAVIGATOR_SUMMARY_PROVIDER_ACCOUNT", "sendgrid-e2e"),
        ("RESTATE_BROKER_URL", restate_url),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_string(), value.to_string()))
    .collect();
    env.push((
        "NAVIGATOR_SUMMARY_MATTER_PROJECT_ID".to_string(),
        matter_project_id.to_string(),
    ));
    env.push(("SENDGRID_INBOUND_PUBLIC_KEY".to_string(), public_key()?));
    Ok(env)
}

/// What the in-cluster worker needs: the summary lane on, and Vertex and Slack
/// pointed at the fixture server (`fixtures` is its address from the cluster).
pub fn worker_env(fixtures: &str, matter_project_id: Uuid) -> Vec<(String, String)> {
    let mut env =
        summary_env(IN_CLUSTER_RESTATE, matter_project_id).expect("synthetic key encodes");
    env.extend(
        [
            ("GOOGLE_METADATA_URL", format!("{fixtures}/token")),
            ("NAVIGATOR_SUMMARY_VERTEX_BASE_URL", fixtures.to_string()),
            ("SLACK_BOT_TOKEN", "synthetic-slack-token".to_string()),
            ("SLACK_API_BASE_URL", format!("{fixtures}/slack")),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_string(), value)),
    );
    env
}

/// Fail unless the Restate admin API lists `service` among its registered
/// services.
pub async fn assert_discovery(admin: &str, service: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let found = async {
            let body: serde_json::Value = reqwest::get(format!("{admin}/services"))
                .await?
                .json()
                .await?;
            Ok::<_, reqwest::Error>(body["services"].as_array().is_some_and(|services| {
                services
                    .iter()
                    .any(|entry| entry["name"].as_str() == Some(service))
            }))
        }
        .await;
        if matches!(found, Ok(true)) {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "Restate discovery does not list {service}; the branch worker is not registered"
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// A TCP forwarder that can be switched off: while off it accepts and
/// immediately closes, which the caller sees as an unavailable dependency.
pub struct Proxy {
    port: u16,
    enabled: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

impl Proxy {
    pub async fn start(upstream: String) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("bind proxy")?;
        let port = listener.local_addr()?.port();
        let enabled = Arc::new(AtomicBool::new(true));
        let flag = Arc::clone(&enabled);
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut inbound, _)) = listener.accept().await else {
                    return;
                };
                if !flag.load(Ordering::SeqCst) {
                    drop(inbound);
                    continue;
                }
                let upstream = upstream.clone();
                tokio::spawn(async move {
                    if let Ok(mut outbound) = TcpStream::connect(&upstream).await {
                        let _ = copy_bidirectional(&mut inbound, &mut outbound).await;
                    }
                });
            }
        });
        Ok(Self {
            port,
            enabled,
            task,
        })
    }

    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::SeqCst);
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// One synthetic raw message and the digest its receipt is keyed on.
pub struct RawEmail {
    pub bytes: Vec<u8>,
    pub digest: String,
    pub body: String,
}

/// A deliberate defect in a webhook request.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    #[default]
    None,
    /// Flip a byte of the signed body after signing.
    Tampered,
    NoSignature,
    WrongPathSecret,
    InvalidEnvelope,
}

/// How one webhook POST is shaped.
#[derive(Default)]
pub struct Post {
    pub boundary: Option<&'static str>,
    pub fault: Fault,
    /// Add one attachment part, so the scanner is consulted.
    pub attachment: bool,
    pub envelope_to: Option<&'static str>,
}

pub struct Harness {
    pub fx: Fixtures,
    pub db: store::surreal::SurrealDb,
    pub storage: Arc<dyn cloud::StorageService>,
    pub restate_proxy: Proxy,
    pub clam_proxy: Proxy,
    pub namespace: String,
    pub admin: String,
    pub sender: String,
    pub matter_project_id: Uuid,
    web: Child,
    base_url: String,
    secret: String,
    http: reqwest::Client,
    log: tempfile::NamedTempFile,
}

impl Harness {
    #[allow(clippy::too_many_lines)]
    pub async fn start(cfg: &KindConfig, root: &Path, fx: Fixtures, admin: String) -> Result<Self> {
        let db = store::surreal::connect_from_env()
            .await
            .context("connect to SurrealDB")?;
        let storage = cloud::from_env()
            .await
            .context("configure object storage")?;
        let restate_proxy = Proxy::start(format!("127.0.0.1:{}", cfg.restate_ingress_port)).await?;
        let clam_addr = std::env::var("NAVIGATOR_CLAMD_ADDR")
            .unwrap_or_else(|_| format!("127.0.0.1:{}", cfg.clamav_port));
        let clam_proxy = Proxy::start(clam_addr).await?;

        let sender = format!("sender-{}@e2e.example.test", Uuid::now_v7().simple());
        store::persons::create(
            &db,
            &store::persons::NewPerson::new("Libra", sender.clone()),
        )
        .await
        .context("seed the synthetic sender as a known person")?;
        let person = store::persons::find_by_email_ci(&db, &sender)
            .await
            .context("find the synthetic sender")?
            .context("synthetic sender row exists")?;
        store::persons::set_email_confirmed(&db, person.id, true)
            .await
            .context("confirm the synthetic sender email")?;
        let firm_id = std::env::var("NAVIGATOR_SUMMARY_FIRM_ID")
            .context("summary Firm id is configured")?
            .parse::<Uuid>()?;
        let firm = store::firms::find_by_id(&db, firm_id)
            .await
            .context("look up the synthetic summary Firm")?
            .context("synthetic summary Firm exists")?;
        let project = store::projects::create(
            &db,
            &store::projects::NewProject {
                code: format!("summary-e2e-{}", Uuid::now_v7().simple()),
                name: "Synthetic summary matter".into(),
                status: "open".into(),
                entity_id: firm
                    .entity_id
                    .context("synthetic summary Firm has an Entity")?,
                firm_id: Some(firm_id),
                ..Default::default()
            },
        )
        .await
        .context("create synthetic summary Project")?;
        store::projects::designate_dri_in_surreal(
            &db,
            project.id,
            person.id,
            store::projects::DriSide::Client,
        )
        .await
        .context("designate the synthetic sender as Client DRI")?;
        let owner_id = store::persons::find_by_role(&db, store::persons::Role::Owner)
            .await
            .context("find the synthetic summary approver")?
            .into_iter()
            .next()
            .context("an Owner can authorize the synthetic summary sender")?
            .id;
        store::summary_sender_approvals::approve(
            &db,
            store::persons::Role::Owner,
            owner_id,
            firm_id,
            project.id,
            "sendgrid-e2e",
            &sender,
        )
        .await
        .context("approve the synthetic sender for this Firm and provider account")?;

        let neon = build_web(root)?;
        let port = free_port()?;
        let log = tempfile::NamedTempFile::new().context("web log file")?;
        let mut command = Command::new(neon);
        command
            .current_dir(root)
            .env("PORT", port.to_string())
            .env(
                "NAVIGATOR_CLAMD_ADDR",
                format!("127.0.0.1:{}", clam_proxy.port()),
            )
            .stdout(Stdio::from(log.reopen()?))
            .stderr(Stdio::from(log.reopen()?));
        for (name, value) in summary_env(
            &format!("http://127.0.0.1:{}", restate_proxy.port()),
            project.id,
        )? {
            command.env(name, value);
        }
        let web = command.spawn().context("start the host web process")?;
        let secret =
            std::env::var("SENDGRID_INBOUND_SECRET").unwrap_or_else(|_| "kind-stub".into());
        let mut harness = Self {
            fx,
            db,
            storage,
            restate_proxy,
            clam_proxy,
            namespace: cfg.namespace.clone(),
            admin,
            sender,
            matter_project_id: project.id,
            web,
            base_url: format!("http://127.0.0.1:{port}"),
            secret,
            http: reqwest::Client::new(),
            log,
        };
        harness.wait_for_web().await?;
        Ok(harness)
    }

    async fn wait_for_web(&mut self) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            if let Some(status) = self.web.try_wait()? {
                bail!(
                    "the host web process exited early ({status}): {}",
                    self.web_log_tail()
                );
            }
            if TcpStream::connect(self.base_url.trim_start_matches("http://"))
                .await
                .is_ok()
            {
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "the host web process did not listen in time: {}",
                self.web_log_tail()
            );
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    fn web_log_tail(&self) -> String {
        let text = std::fs::read_to_string(self.log.path()).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        lines[lines.len().saturating_sub(15)..].join("\n")
    }

    pub fn stop(&mut self) {
        let _ = self.web.kill();
        let _ = self.web.wait();
    }

    /// A unique, synthetic message from the seeded sender to `recipient`.
    #[must_use]
    pub fn new_email(&self, recipient: &str) -> RawEmail {
        let id = Uuid::now_v7();
        let body = format!("Synthetic matter update {id}. Please confirm the next step.");
        let bytes = format!(
            "From: Synthetic Sender <{}>\r\nTo: {recipient}\r\nSubject: Synthetic update {id}\r\n\
             Message-ID: <{id}@e2e.example.test>\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n",
            self.sender
        )
        .into_bytes();
        let digest = workflows::scoped_email_digest(DEPLOYMENT, MAILBOX, &bytes);
        RawEmail {
            bytes,
            digest,
            body,
        }
    }

    /// POST one signed (or deliberately damaged) inbound-parse request and
    /// return the HTTP status.
    pub async fn post(&self, email: &RawEmail, post: &Post) -> Result<u16> {
        let boundary = post.boundary.unwrap_or("----e2e-boundary-a");
        let envelope = if post.fault == Fault::InvalidEnvelope {
            "{not json".to_string()
        } else {
            format!(
                r#"{{"to":["{}"],"from":"{}"}}"#,
                post.envelope_to.unwrap_or(MAILBOX),
                self.sender
            )
        };
        let mut body = Vec::new();
        let mut field = |name: &str, value: &str| {
            body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
            );
            body.extend_from_slice(value.as_bytes());
            body.extend_from_slice(b"\r\n");
        };
        field("from", &self.sender);
        field("to", MAILBOX);
        field("envelope", &envelope);
        field("subject", "Synthetic update");
        field("text", &email.body);
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            b"Content-Disposition: form-data; name=\"email\"\r\nContent-Type: message/rfc822\r\n\r\n",
        );
        body.extend_from_slice(&email.bytes);
        body.extend_from_slice(b"\r\n");
        if post.attachment {
            body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            body.extend_from_slice(
                b"Content-Disposition: form-data; name=\"attachment1\"; filename=\"note.txt\"\r\n\
                  Content-Type: text/plain\r\n\r\nsynthetic attachment\r\n",
            );
        }
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs()
            .to_string();
        let mut payload = timestamp.as_bytes().to_vec();
        payload.extend_from_slice(&body);
        let key = SigningKey::from_slice(&SIGNING_SEED)?;
        let signature: Signature = key.sign(&payload);
        if post.fault == Fault::Tampered {
            let last = body.len() / 2;
            body[last] ^= 0x01;
        }
        let secret = if post.fault == Fault::WrongPathSecret {
            "not-the-secret"
        } else {
            &self.secret
        };
        let mut request = self
            .http
            .post(format!(
                "{}/webhook/sendgrid/inbound/{secret}",
                self.base_url
            ))
            .header(
                reqwest::header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .header(portal::inbound_email::SUMMARY_TIMESTAMP_HEADER, timestamp);
        if post.fault != Fault::NoSignature {
            request = request.header(
                portal::inbound_email::SUMMARY_SIGNATURE_HEADER,
                base64::engine::general_purpose::STANDARD.encode(signature.to_der().as_bytes()),
            );
        }
        let response = request
            .body(body)
            .send()
            .await
            .context("post the webhook")?;
        Ok(response.status().as_u16())
    }

    pub async fn receipt(&self, email: &RawEmail) -> Result<Option<EmailReceipt>> {
        store::email_receipts::find(&self.db, MAILBOX, DEPLOYMENT, &email.digest)
            .await
            .context("read the receipt")
    }

    pub async fn delivery(&self, receipt_id: Uuid) -> Result<Option<EmailDelivery>> {
        store::email_deliveries::find(&self.db, receipt_id)
            .await
            .context("read the delivery")
    }

    pub async fn receipt_count(&self) -> Result<i64> {
        store::email_receipts::count(&self.db)
            .await
            .context("count receipts")
    }

    pub async fn letter_count(&self) -> Result<i64> {
        store::letters::count(&self.db)
            .await
            .context("count letters")
    }

    /// Poll until `check` yields a value or the timeout passes.
    pub async fn eventually<T, F, Fut>(
        &self,
        what: &str,
        timeout: Duration,
        mut check: F,
    ) -> Result<T>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<Option<T>>>,
    {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(value) = check().await? {
                return Ok(value);
            }
            ensure!(Instant::now() < deadline, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// Wait for the receipt of `email` to exist, then return it.
    pub async fn wait_receipt(&self, email: &RawEmail) -> Result<EmailReceipt> {
        self.eventually("the receipt", Duration::from_secs(60), || async {
            self.receipt(email).await
        })
        .await
    }

    /// Wait until the receipt's delivery reaches `state`.
    pub async fn wait_delivery(
        &self,
        receipt_id: Uuid,
        state: &str,
        timeout: Duration,
    ) -> Result<EmailDelivery> {
        self.eventually(&format!("delivery state {state}"), timeout, || async {
            Ok(self
                .delivery(receipt_id)
                .await?
                .filter(|delivery| delivery.state == state))
        })
        .await
    }

    /// The statuses of every retained `EmailSummary` invocation for a receipt.
    pub async fn invocations(&self, receipt_id: Uuid) -> Result<Vec<String>> {
        let query = format!(
            "SELECT status FROM sys_invocation WHERE target_service_name = 'EmailSummary' \
             AND target_service_key = '{receipt_id}'"
        );
        let body: serde_json::Value = self
            .http
            .post(format!("{}/query", self.admin))
            .header(reqwest::header::ACCEPT, "application/json")
            .json(&serde_json::json!({ "query": query }))
            .send()
            .await
            .context("query Restate invocations")?
            .json()
            .await
            .context("parse the Restate query result")?;
        Ok(body["rows"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| row["status"].as_str().map(ToOwned::to_owned))
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn wait_invocation_completed(
        &self,
        receipt_id: Uuid,
        timeout: Duration,
    ) -> Result<()> {
        self.eventually("the invocation to complete", timeout, || async {
            let statuses = self.invocations(receipt_id).await?;
            Ok((statuses == ["completed"]).then_some(()))
        })
        .await
    }

    /// Force-delete the worker pod and wait for its replacement to be ready.
    pub async fn kill_worker(&self) -> Result<()> {
        super::run_kubectl(&[
            "--namespace",
            &self.namespace,
            "delete",
            "pod",
            "--selector",
            WORKER_SELECTOR,
            "--grace-period=0",
            "--force",
            "--wait=false",
        ])?;
        tokio::time::sleep(Duration::from_secs(2)).await;
        self.eventually("the replacement worker", Duration::from_secs(180), || async {
            let output = Command::new("kubectl")
                .args([
                    "--namespace",
                    &self.namespace,
                    "get",
                    "pods",
                    "--selector",
                    WORKER_SELECTOR,
                    "--output",
                    "jsonpath={range .items[*]}{.metadata.deletionTimestamp}/{.status.containerStatuses[0].ready} {end}",
                ])
                .output()?;
            let listing = String::from_utf8_lossy(&output.stdout).to_string();
            let pods: Vec<&str> = listing.split_whitespace().collect();
            Ok((!pods.is_empty() && pods.iter().all(|pod| *pod == "/true")).then_some(()))
        })
        .await
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.stop();
    }
}

fn free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").context("find a free port")?;
    Ok(listener.local_addr()?.port())
}

/// Build this branch's `neon` binary and return its path.
fn build_web(root: &Path) -> Result<std::path::PathBuf> {
    super::run_cargo(root, &["build", "--package", "neon"])?;
    let output = Command::new("cargo")
        .current_dir(root)
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .context("run cargo metadata")?;
    ensure!(output.status.success(), "cargo metadata failed");
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let target = metadata["target_directory"]
        .as_str()
        .context("cargo metadata has no target_directory")?;
    let binary = Path::new(target).join("debug").join("neon");
    ensure!(binary.exists(), "the neon binary was not built");
    Ok(binary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_worker_env_enables_the_lane_and_points_providers_at_the_fixtures() {
        let matter_project_id = Uuid::now_v7();
        let env = worker_env("http://host.docker.internal:4000", matter_project_id);
        let get = |name: &str| {
            env.iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(get("NAVIGATOR_SUMMARY_ENABLED"), Some("1"));
        assert_eq!(
            get("GOOGLE_METADATA_URL"),
            Some("http://host.docker.internal:4000/token")
        );
        assert_eq!(
            get("NAVIGATOR_SUMMARY_VERTEX_BASE_URL"),
            Some("http://host.docker.internal:4000")
        );
        assert_eq!(
            get("SLACK_API_BASE_URL"),
            Some("http://host.docker.internal:4000/slack")
        );
        assert_eq!(get("RESTATE_BROKER_URL"), Some(IN_CLUSTER_RESTATE));
        assert_eq!(
            get("NAVIGATOR_SUMMARY_MATTER_PROJECT_ID"),
            Some(matter_project_id.to_string().as_str())
        );
    }

    #[test]
    fn the_worker_env_passes_the_workers_own_summary_config_validation() {
        let env = worker_env("http://host.docker.internal:4000", Uuid::now_v7());
        let mut lookup: std::collections::HashMap<String, String> = env.into_iter().collect();
        lookup.insert("NAVIGATOR_CI_HARNESS".into(), "1".into());
        let config = workflows::EmailSummaryConfig::from_lookup(|key| lookup.get(key).cloned())
            .expect("valid config")
            .expect("enabled");
        assert_eq!(config.envelope_recipients, vec![MAILBOX.to_string()]);
        assert_eq!(config.deployment, DEPLOYMENT);
        assert_eq!(config.channel_id, CHANNEL);
    }

    #[tokio::test]
    async fn a_disabled_proxy_refuses_work_and_an_enabled_one_forwards_it() {
        let upstream = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let upstream_addr = upstream.local_addr().expect("addr").to_string();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = upstream.accept().await {
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut byte = [0_u8; 1];
                    if socket.read_exact(&mut byte).await.is_ok() {
                        let _ = socket.write_all(&byte).await;
                    }
                });
            }
        });
        let proxy = Proxy::start(upstream_addr).await.expect("proxy");
        let echo = |port: u16| async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
            stream.write_all(b"x").await?;
            let mut byte = [0_u8; 1];
            stream.read_exact(&mut byte).await?;
            Ok::<_, std::io::Error>(byte[0])
        };
        assert_eq!(echo(proxy.port()).await.expect("forwarded"), b'x');
        proxy.set_enabled(false);
        assert!(echo(proxy.port()).await.is_err());
        proxy.set_enabled(true);
        assert_eq!(echo(proxy.port()).await.expect("forwarded again"), b'x');
    }
}

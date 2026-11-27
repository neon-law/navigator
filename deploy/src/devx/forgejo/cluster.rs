//! `dev forgejo up`: converge the opt-in Forgejo into a KIND tier.
//!
//! Every step shells out to `kubectl` or `openssl` against the selected KIND
//! context, so this file is excluded from the coverage gate; the decisions it
//! applies are the tested functions in `mod.rs`, and the CI KIND job proves
//! the whole path.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;

use super::super::KindConfig;
use super::{
    align_issuer, kind_app_ini, root_url, runner_config, Seed, OVERLAY, POD, RAUTHY_DEPLOYMENT,
    RAUTHY_FIXTURE, RUNNER_DEPLOYMENT, RUNNER_LABEL, RUNNER_NAME, SEED_SECRET, SIGNING_EMAIL,
    SIGNING_SECRET, STATEFULSET,
};

/// `kubectl`, pinned to one KIND context and namespace.
pub(in crate::devx) struct Kube {
    context: String,
    namespace: String,
}

impl Kube {
    pub(in crate::devx) fn selected(context: &str, namespace: &str) -> Self {
        Self {
            context: context.to_owned(),
            namespace: namespace.to_owned(),
        }
    }

    pub(in crate::devx) fn new(cfg: &KindConfig) -> Self {
        Self {
            context: cfg.kind_context(),
            namespace: cfg.namespace.clone(),
        }
    }

    pub(in crate::devx) fn command(&self) -> Command {
        let mut command = Command::new("kubectl");
        command.args(["--context", &self.context, "--namespace", &self.namespace]);
        command
    }

    pub(in crate::devx) fn namespace(&self) -> &str {
        &self.namespace
    }

    pub(in crate::devx) fn run(&self, args: &[&str]) -> Result<()> {
        let status = self
            .command()
            .args(args)
            .status()
            .with_context(|| format!("spawn kubectl {args:?}"))?;
        if !status.success() {
            bail!("kubectl {args:?} failed ({status})");
        }
        Ok(())
    }

    /// Run and capture stdout; stderr is reported only on failure.
    pub(in crate::devx) fn output(&self, args: &[&str]) -> Result<String> {
        self.output_with_stdin(args, None)
    }

    pub(in crate::devx) fn output_with_stdin(
        &self,
        args: &[&str],
        stdin: Option<&[u8]>,
    ) -> Result<String> {
        let mut child = self
            .command()
            .args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawn kubectl {args:?}"))?;
        if let Some(input) = stdin {
            child
                .stdin
                .take()
                .context("kubectl stdin")?
                .write_all(input)
                .context("write kubectl stdin")?;
        }
        let output = child.wait_with_output().context("wait for kubectl")?;
        if !output.status.success() {
            bail!(
                "kubectl {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    pub(in crate::devx) fn succeeds(&self, args: &[&str]) -> bool {
        self.command()
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    /// `kubectl apply -f -`, returning what changed.
    pub(in crate::devx) fn apply(&self, manifest: &str) -> Result<String> {
        self.output_with_stdin(&["apply", "-f", "-"], Some(manifest.as_bytes()))
    }

    /// A Secret's decoded data, or `None` when it does not exist.
    pub(in crate::devx) fn secret(&self, name: &str) -> Result<Option<BTreeMap<String, String>>> {
        if !self.succeeds(&["get", "secret", name]) {
            return Ok(None);
        }
        let json = self.output(&["get", "secret", name, "-o", "json"])?;
        let value: serde_json::Value =
            serde_json::from_str(&json).with_context(|| format!("parse Secret {name}"))?;
        let mut data = BTreeMap::new();
        if let Some(map) = value.get("data").and_then(serde_json::Value::as_object) {
            for (key, encoded) in map {
                let encoded = encoded.as_str().unwrap_or_default();
                let decoded = STANDARD
                    .decode(encoded)
                    .with_context(|| format!("decode {name}/{key}"))?;
                data.insert(
                    key.clone(),
                    String::from_utf8(decoded).with_context(|| format!("{name}/{key} is UTF-8"))?,
                );
            }
        }
        Ok(Some(data))
    }

    /// Exec into the Forgejo container as its own user.
    pub(in crate::devx) fn forgejo(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<String> {
        let mut command_line = vec![
            "exec",
            if stdin.is_some() { "-i" } else { "--quiet" },
            POD,
            "-c",
            "forgejo",
            "--",
            "/usr/local/bin/forgejo",
            "--config",
            "/etc/forgejo/app.ini",
            "--work-path",
            "/var/lib/gitea",
        ];
        command_line.extend_from_slice(args);
        self.output_with_stdin(&command_line, stdin)
    }

    pub(in crate::devx) fn wait_rollout(&self, resource: &str, timeout: &str) -> Result<()> {
        self.run(&[
            "rollout",
            "status",
            resource,
            &format!("--timeout={timeout}"),
        ])
    }

    pub(in crate::devx) fn wait_job(&self, job: &str) -> Result<()> {
        use super::super::postgres_fixture::{job_state, JobState};
        let deadline = Instant::now() + Duration::from_secs(300);
        let result = (|| loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            anyhow::ensure!(
                !remaining.is_zero(),
                "job/{job} did not complete within 300 seconds"
            );
            let request_timeout = format!(
                "--request-timeout={}ms",
                remaining.as_millis().clamp(1, 10_000)
            );
            let state: serde_json::Value = serde_json::from_str(&self.output(&[
                &request_timeout,
                "get",
                &format!("job/{job}"),
                "-o",
                "json",
            ])?)?;
            match job_state(&state)? {
                JobState::Complete => return Ok(()),
                JobState::Failed => bail!("job/{job} failed"),
                JobState::Pending => {}
            }
            if Instant::now() >= deadline {
                bail!("job/{job} did not complete within 300 seconds");
            }
            std::thread::sleep(
                Duration::from_secs(1).min(deadline.saturating_duration_since(Instant::now())),
            );
        })();
        if result.is_err() {
            let _ = self.run(&["--request-timeout=10s", "describe", &format!("job/{job}")]);
            let _ = self.run(&[
                "--request-timeout=10s",
                "logs",
                &format!("job/{job}"),
                "--all-containers",
            ]);
        }
        result
    }
}

/// A Secret manifest from string data.
pub(in crate::devx) fn secret_manifest(
    name: &str,
    namespace: &str,
    secret_type: &str,
    data: &BTreeMap<String, String>,
) -> Result<String> {
    serde_yaml::to_string(&serde_json::json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {"name": name, "namespace": namespace},
        "type": secret_type,
        "stringData": data,
    }))
    .context("serialize Secret manifest")
}

/// `dev forgejo up`.
pub(in crate::devx) fn up(cfg: &KindConfig) -> Result<()> {
    super::super::require_tools(&["kubectl", "docker", "openssl", "ssh-keygen"])?;
    let root = super::super::workspace_root()?;
    let kube = Kube::new(cfg);
    if !kube.succeeds(&["get", "namespace", kube.namespace()]) {
        bail!(
            "namespace {} does not exist in context {}; create the KIND tier first \
             (`hudson dev worktree-env up` or `hudson dev up`)",
            kube.namespace(),
            cfg.kind_context()
        );
    }

    let container = super::super::orchestrate::verify_fixture_port(cfg)?;
    super::super::orchestrate::apply_postgres_host_ingress(cfg, &kube, &container)?;
    ensure_rauthy(&root, cfg, &kube)?;
    let seed = ensure_seed(&kube)?;
    ensure_signing_key(&kube)?;
    apply_secrets(cfg, &kube, &seed)?;

    let native_database = if super::super::native_profile::selected(&root, cfg)? {
        Some(super::super::orchestrate::verified_native_database(
            &root, cfg,
        )?)
    } else {
        None
    };

    eprintln!("==> applying {OVERLAY}");
    // Completed Jobs are immutable; delete them so a changed spec re-runs.
    kube.run(&[
        "delete",
        "job",
        "navigator-postgres-roles",
        "navigator-forgejo-database",
        "--ignore-not-found",
        "--wait=true",
    ])?;
    let rendered = kustomize(&root, OVERLAY)?;
    let rendered = match native_database {
        Some(database) => super::super::native_profile::roles_for_database(&rendered, &database)?,
        None => rendered,
    };
    let changes = kube.apply(&align_issuer(&rendered, cfg.rauthy_port))?;
    eprint!("{changes}");

    kube.wait_rollout("statefulset/navigator-postgres", "300s")?;
    kube.wait_job("navigator-postgres-roles")?;
    kube.wait_job("navigator-forgejo-database")?;
    kube.wait_rollout(&format!("statefulset/{STATEFULSET}"), "600s")?;

    eprintln!("==> registering the Actions runner");
    kube.forgejo(
        &[
            "forgejo-cli",
            "actions",
            "register",
            "--secret-stdin",
            "-",
            "--name",
            RUNNER_NAME,
            "--labels",
            RUNNER_LABEL,
        ],
        Some(seed.runner_secret.as_bytes()),
    )?;
    kube.run(&[
        "rollout",
        "restart",
        &format!("deployment/{RUNNER_DEPLOYMENT}"),
    ])?;
    kube.wait_rollout(&format!("deployment/{RUNNER_DEPLOYMENT}"), "300s")?;

    let pid = port_forward(&root, cfg, STATEFULSET, cfg.forgejo_port, 3000)?;
    eprintln!(
        "==> Forgejo is at {} (kubectl port-forward pid {pid}); sign in through Rauthy at {}",
        root_url(cfg.forgejo_port),
        super::super::rauthy_origin(cfg.rauthy_port),
    );
    super::super::orchestrate::verify_fixture_port(cfg)?;
    eprintln!("==> PostgreSQL uses the tier's verified loopback TLS mapping");
    Ok(())
}

pub(in crate::devx) fn kustomize(root: &Path, overlay: &str) -> Result<String> {
    let output = Command::new("kubectl")
        .arg("kustomize")
        .arg(root.join(overlay))
        .output()
        .with_context(|| format!("kubectl kustomize {overlay}"))?;
    if !output.status.success() {
        bail!(
            "kubectl kustomize {overlay} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Make sure the tier's Rauthy carries the current fixture, including the
/// `navigator-forgejo` client and the synthetic smoke users. A bare cluster
/// (CI) gets Rauthy itself. A fixture change restarts Rauthy, which
/// re-bootstraps from the `ConfigMap`.
fn ensure_rauthy(root: &Path, cfg: &KindConfig, kube: &Kube) -> Result<()> {
    eprintln!("==> converging the Rauthy fixture");
    let fixture = align_issuer(&kustomize(root, RAUTHY_FIXTURE)?, cfg.rauthy_port);
    let changes = kube.apply(&fixture)?;
    eprint!("{changes}");
    if super::super::rauthy_deployment_exists(cfg) {
        if changes
            .lines()
            .any(|line| line.ends_with(" configured") || line.ends_with(" created"))
        {
            kube.run(&["rollout", "restart", "deployment/rauthy"])?;
        }
    } else {
        kube.run(&[
            "apply",
            "-f",
            &root.join(RAUTHY_DEPLOYMENT).display().to_string(),
        ])?;
    }
    kube.wait_rollout("deployment/rauthy", "300s")
}

fn ensure_seed(kube: &Kube) -> Result<Seed> {
    if let Some(existing) = kube.secret(SEED_SECRET)? {
        return Seed::from_map(&existing);
    }
    eprintln!("==> generating synthetic credentials ({SEED_SECRET})");
    let seed = Seed::generate();
    kube.apply(&secret_manifest(
        SEED_SECRET,
        kube.namespace(),
        "Opaque",
        &seed.to_map(),
    )?)?;
    Ok(seed)
}

/// Generate Forgejo's synthetic SSH signing key once. Forgejo reads the
/// public half at start and signs with the private half beside it, so a
/// running tier keeps the key it started with.
fn ensure_signing_key(kube: &Kube) -> Result<()> {
    if kube.secret(SIGNING_SECRET)?.is_some() {
        return Ok(());
    }
    eprintln!("==> generating the synthetic signing key ({SIGNING_SECRET})");
    let dir = tempfile::tempdir().context("create a signing-key scratch directory")?;
    let key = dir.path().join("signing_key");
    let status = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", SIGNING_EMAIL, "-f"])
        .arg(&key)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("spawn ssh-keygen")?;
    if !status.success() {
        bail!("ssh-keygen could not create the synthetic signing key");
    }
    kube.apply(&secret_manifest(
        SIGNING_SECRET,
        kube.namespace(),
        "Opaque",
        &BTreeMap::from([
            (
                "signing_key".to_owned(),
                std::fs::read_to_string(&key).context("read the signing key")?,
            ),
            (
                "signing_key.pub".to_owned(),
                std::fs::read_to_string(key.with_extension("pub"))
                    .context("read the signing public key")?,
            ),
        ]),
    )?)?;
    Ok(())
}

/// Project every Secret the overlay reads from the seed. The PostgreSQL
/// server certificate is generated once and then reused, because a running
/// server keeps the certificate it started with.
fn apply_secrets(cfg: &KindConfig, kube: &Kube, seed: &Seed) -> Result<()> {
    let namespace = kube.namespace();
    let ca = apply_postgres_secrets(kube, seed)?;
    let one = |key: &str, value: &str| BTreeMap::from([(key.to_owned(), value.to_owned())]);
    let secrets = [
        (
            "navigator-forgejo-database",
            one("password", &seed.database_password),
        ),
        ("navigator-forgejo-postgres-ca", one("ca.crt", &ca)),
        (
            "navigator-forgejo-config",
            BTreeMap::from([
                (
                    "app.ini".to_owned(),
                    kind_app_ini(seed, namespace, cfg.forgejo_port)?,
                ),
                ("oauth2_jwt_secret".to_owned(), seed.jwt_secret.clone()),
            ]),
        ),
        (
            "navigator-forgejo-runner",
            one("config.yml", &runner_config(seed)?),
        ),
    ];
    let mut changed = Vec::new();
    for (name, data) in secrets {
        let result = kube.apply(&secret_manifest(name, namespace, "Opaque", &data)?)?;
        if !result.contains("unchanged") {
            changed.push(name);
        }
    }
    // A running Forgejo keeps the app.ini it rendered at start.
    if changed.contains(&"navigator-forgejo-config") && kube.succeeds(&["get", "pod", POD]) {
        kube.run(&["rollout", "restart", &format!("statefulset/{STATEFULSET}")])?;
    }
    Ok(())
}

/// Reuse the tier's synthetic PostgreSQL material without deploying Forgejo.
pub(in crate::devx) fn prepare_postgres(
    cfg: &KindConfig,
) -> Result<crate::devx::postgres_fixture::Credentials> {
    let kube = Kube::new(cfg);
    let existing_seed = kube.secret(SEED_SECRET)?;
    let seed = existing_seed.as_ref().map(Seed::from_map).transpose()?;
    for (name, expected) in [
        (
            "navigator-postgres-auth",
            seed.as_ref().map(|s| &s.postgres_password),
        ),
        (
            "navigator-postgres-app",
            seed.as_ref().map(|s| &s.app_password),
        ),
        (
            "navigator-postgres-migrator",
            seed.as_ref().map(|s| &s.migrator_password),
        ),
        (
            "navigator-postgres-backup",
            seed.as_ref().map(|s| &s.backup_password),
        ),
    ] {
        if let Some(secret) = kube.secret(name)? {
            anyhow::ensure!(
                expected.is_some_and(|password| secret.get("password") == Some(password)),
                "existing synthetic PostgreSQL credentials require explicit recovery; bootstrap will not replace them"
            );
        }
    }
    let seed = match seed {
        Some(seed) => seed,
        None => ensure_seed(&kube)?,
    };
    let ca = apply_postgres_secrets(&kube, &seed)?;
    Ok(crate::devx::postgres_fixture::Credentials {
        app_password: seed.app_password,

        ca,
    })
}

fn apply_postgres_secrets(kube: &Kube, seed: &Seed) -> Result<String> {
    let namespace = kube.namespace();
    let ca = if let Some(tls) = kube.secret("navigator-postgres-tls")? {
        super::super::postgres_fixture::trust_anchor(&tls)?.to_owned()
    } else {
        let material = server_certificate(namespace)?;
        kube.apply(&secret_manifest(
            "navigator-postgres-tls",
            namespace,
            "kubernetes.io/tls",
            &BTreeMap::from([
                ("tls.crt".to_owned(), material.certificate),
                ("tls.key".to_owned(), material.key),
                ("ca.crt".to_owned(), material.ca.clone()),
            ]),
        )?)?;
        material.ca
    };
    for (name, password) in [
        ("navigator-postgres-auth", &seed.postgres_password),
        ("navigator-postgres-app", &seed.app_password),
        ("navigator-postgres-migrator", &seed.migrator_password),
        ("navigator-postgres-backup", &seed.backup_password),
    ] {
        let mut data = BTreeMap::from([("password".to_owned(), password.clone())]);
        if name != "navigator-postgres-auth" {
            data.insert("ca.crt".to_owned(), ca.clone());
        }
        kube.apply(&secret_manifest(name, namespace, "Opaque", &data)?)?;
    }
    Ok(ca)
}

struct ServerCertificate {
    ca: String,
    certificate: String,
    key: String,
}

/// Explicit CA and server constraints avoid platform-dependent OpenSSL defaults.
fn server_certificate(namespace: &str) -> Result<ServerCertificate> {
    let dir = tempfile::tempdir().context("create a certificate scratch directory")?;
    issue_server_certificate(namespace, dir.path(), Path::new("openssl"))
}

fn issue_server_certificate(
    namespace: &str,
    directory: &Path,
    openssl: &Path,
) -> Result<ServerCertificate> {
    let path = |name: &str| directory.join(name);
    let config = path("ca.cnf");
    std::fs::write(
        &config,
        "[req]\nprompt=no\ndistinguished_name=dn\nx509_extensions=ca\n[dn]\nCN=navigator-postgres-ca\n[ca]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\nsubjectKeyIdentifier=hash\n",
    )?;
    let ca = path("ca.crt");
    let ca_key = path("ca.key");
    let certificate = path("tls.crt");
    let key = path("tls.key");
    let csr = path("tls.csr");
    let extensions = path("server.ext");
    std::fs::write(
        &extensions,
        format!(
            "basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid,issuer\nsubjectAltName=DNS:navigator-postgres,DNS:navigator-postgres.{namespace}.svc,DNS:navigator-postgres.{namespace}.svc.cluster.local,DNS:localhost,IP:127.0.0.1\n"
        ),
    )?;
    certificate_command(
        Command::new(openssl)
            .args([
                "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-sha256", "-days", "3650",
                "-config",
            ])
            .arg(&config)
            .arg("-keyout")
            .arg(&ca_key)
            .arg("-out")
            .arg(&ca),
    )?;
    certificate_command(
        Command::new(openssl)
            .args([
                "req",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-sha256",
                "-subj",
                "/CN=navigator-postgres",
                "-config",
            ])
            .arg(&config)
            .arg("-keyout")
            .arg(&key)
            .arg("-out")
            .arg(&csr),
    )?;
    certificate_command(
        Command::new(openssl)
            .args(["x509", "-req", "-sha256", "-days", "3650", "-in"])
            .arg(&csr)
            .arg("-CA")
            .arg(&ca)
            .arg("-CAkey")
            .arg(&ca_key)
            .arg("-CAcreateserial")
            .arg("-extfile")
            .arg(&extensions)
            .arg("-out")
            .arg(&certificate),
    )?;
    certificate_command(
        Command::new(openssl)
            .arg("verify")
            .arg("-CAfile")
            .arg(&ca)
            .arg(&certificate),
    )?;
    Ok(ServerCertificate {
        ca: std::fs::read_to_string(ca).context("read public root certificate")?,
        certificate: std::fs::read_to_string(certificate).context("read server certificate")?,
        key: std::fs::read_to_string(key).context("read server key")?,
    })
}

fn certificate_command(command: &mut Command) -> Result<()> {
    let status = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("run openssl for the synthetic PostgreSQL certificate")?;
    anyhow::ensure!(
        status.success(),
        "openssl could not create the synthetic PostgreSQL certificate"
    );
    Ok(())
}

#[cfg(test)]
#[path = "tls_tests.rs"]
mod tls_tests;

fn pid_file(root: &Path, service: &str) -> PathBuf {
    root.join(".devx")
        .join(format!("{service}-port-forward.pid"))
}

/// Keep `localhost:<host_port>` pointed at `service`: replace this checkout's
/// previous forward of it with a detached one.
fn port_forward(
    root: &Path,
    cfg: &KindConfig,
    service: &str,
    host_port: u16,
    service_port: u16,
) -> Result<u32> {
    let pid_path = pid_file(root, service);
    if let Ok(previous) = std::fs::read_to_string(&pid_path) {
        if let Ok(pid) = previous.trim().parse::<u32>() {
            let _ = Command::new("kill").arg(pid.to_string()).status();
        }
    }
    std::fs::create_dir_all(root.join(".devx")).context("create .devx")?;
    let mut command = Command::new("kubectl");
    command
        .args([
            "--context",
            &cfg.kind_context(),
            "--namespace",
            &cfg.namespace,
        ])
        .args(["port-forward", &format!("service/{service}")])
        .arg(format!("{host_port}:{service_port}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command.spawn().context("spawn kubectl port-forward")?;
    let pid = child.id();
    std::mem::forget(child);
    std::fs::write(&pid_path, pid.to_string()).context("record the port-forward pid")?;
    super::super::wait_for_tcp("127.0.0.1", host_port)?;
    Ok(pid)
}

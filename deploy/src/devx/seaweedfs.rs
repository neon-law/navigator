//! `SeaweedFS` provisioning for KIND and shared native development.
//! Credentials travel over stdin or protected files, never command arguments.

use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};

const S3_SECRET: &str = "navigator-seaweedfs-s3";
pub(super) const LANES: &[(&str, &str)] = &[
    ("documents", ""),
    ("assets", "assets_"),
    ("applications", "applications_"),
    ("exports", "exports_"),
    ("archives", "archives_"),
    ("telemetry", "telemetry_"),
    ("lfs", "lfs_"),
    ("archived-projects", "archived_projects_"),
];

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct LaneCredentials {
    pub access_key: String,
    pub secret_key: String,
}

pub(super) struct Credentials {
    pub documents: LaneCredentials,
    pub assets: LaneCredentials,
    pub applications: LaneCredentials,
    pub archives: LaneCredentials,
    pub lfs: LaneCredentials,
    pub exports: LaneCredentials,
    pub telemetry: LaneCredentials,
    pub archived_projects: LaneCredentials,
}

pub(super) fn credentials(keys: &BTreeMap<String, LaneCredentials>) -> Result<Credentials> {
    let key = |name: &str| {
        keys.get(name)
            .cloned()
            .context("missing SeaweedFS lane credentials")
    };
    Ok(Credentials {
        documents: key("documents")?,
        assets: key("assets")?,
        applications: key("applications")?,
        archives: key("archives")?,
        lfs: key("lfs")?,
        exports: key("exports")?,
        telemetry: key("telemetry")?,
        archived_projects: key("archived-projects")?,
    })
}

pub(super) fn new_key() -> LaneCredentials {
    LaneCredentials {
        access_key: random_hex(),
        secret_key: random_hex(),
    }
}

/// Scope each identity to exactly one bucket. No anonymous identity or global grant.
pub(super) fn identity(bucket: &str, key: &LaneCredentials) -> serde_json::Value {
    serde_json::json!({
        "name": bucket,
        "credentials": [{"accessKey": key.access_key, "secretKey": key.secret_key}],
        "actions": [format!("Read:{bucket}"), format!("Write:{bucket}"),
                    format!("List:{bucket}"), format!("Tagging:{bucket}")]
    })
}

pub(super) fn bootstrap(cfg: &super::KindConfig) -> Result<()> {
    super::require_tools(&["kubectl"])?;
    super::use_kind_context(cfg)?;
    prepare(&cfg.namespace)?;
    super::wait_rollout("statefulset", "seaweedfs", cfg)?;
    provision(&cfg.namespace)?;
    Ok(())
}

/// Create the credentials and startup authentication config atomically.
/// Kubernetes create, rather than apply, preserves the first writer's keys.
pub(super) fn prepare(namespace: &str) -> Result<()> {
    if secret_data(namespace)?.is_some() {
        return Ok(());
    }
    let mut data = BTreeMap::new();
    let mut identities = Vec::new();
    for (lane, prefix) in LANES {
        let key = new_key();
        identities.push(identity(&format!("navigator-{lane}"), &key));
        data.insert(format!("{prefix}access_key"), key.access_key);
        data.insert(format!("{prefix}secret_key"), key.secret_key);
    }
    data.insert(
        "s3.json".into(),
        serde_json::to_string(&serde_json::json!({"identities": identities}))?,
    );
    let manifest = serde_json::json!({
        "apiVersion": "v1", "kind": "Secret", "type": "Opaque",
        "metadata": {"name": S3_SECRET, "namespace": namespace}, "stringData": data
    });
    let mut command = Command::new("kubectl");
    command.args(["create", "-f", "-"]);
    if !stdin_status(&mut command, &manifest.to_string())? && secret_data(namespace)?.is_none() {
        bail!("could not create SeaweedFS credentials secret");
    }
    Ok(())
}

#[derive(Deserialize)]
struct Secret {
    data: BTreeMap<String, String>,
}

fn secret_data(namespace: &str) -> Result<Option<BTreeMap<String, String>>> {
    let output = Command::new("kubectl")
        .args([
            "--namespace",
            namespace,
            "get",
            "secret",
            S3_SECRET,
            "--ignore-not-found",
            "-o",
            "json",
        ])
        .output()
        .context("read SeaweedFS Kubernetes secret")?;
    if !output.status.success() {
        bail!("could not read SeaweedFS credentials secret");
    }
    if output.stdout.is_empty() {
        return Ok(None);
    }
    let secret: Secret =
        serde_json::from_slice(&output.stdout).context("decode SeaweedFS secret")?;
    let mut data = BTreeMap::new();
    for (name, encoded) in secret.data {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .context("decode secret value")?;
        data.insert(
            name,
            String::from_utf8(bytes).context("secret value is not UTF-8")?,
        );
    }
    Ok(Some(data))
}

pub(super) fn provision(namespace: &str) -> Result<Credentials> {
    let data = secret_data(namespace)?.context("SeaweedFS credentials have not been prepared")?;
    let mut keys = BTreeMap::new();
    for (lane, prefix) in LANES {
        let value = |suffix| {
            data.get(&format!("{prefix}{suffix}"))
                .cloned()
                .context("incomplete SeaweedFS secret")
        };
        keys.insert(
            (*lane).to_owned(),
            LaneCredentials {
                access_key: value("access_key")?,
                secret_key: value("secret_key")?,
            },
        );
        let bucket = format!("navigator-{lane}");
        let mut command = Command::new("kubectl");
        command.args([
            "--namespace",
            namespace,
            "exec",
            "-i",
            "seaweedfs-0",
            "--",
            "weed",
            "shell",
            "-master=localhost:9333",
            "-filer=localhost:8888",
        ]);
        shell(
            &mut command,
            &format!("s3.bucket.create -name={bucket} -owner={bucket}\n"),
            Some("already exists"),
        )?;
    }
    credentials(&keys)
}

pub(super) fn stdin_status(command: &mut Command, input: &str) -> Result<bool> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("start SeaweedFS provisioning command")?;
    child
        .stdin
        .take()
        .context("provisioning stdin unavailable")?
        .write_all(input.as_bytes())?;
    Ok(child
        .wait()
        .context("wait for provisioning command")?
        .success())
}

/// weed shell reports command errors in its output as well as its exit status.
/// Never include that output in diagnostics: IAM commands can echo credentials.
pub(super) fn shell(
    command: &mut Command,
    input: &str,
    expected_error: Option<&str>,
) -> Result<()> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("start SeaweedFS shell")?;
    child
        .stdin
        .take()
        .context("SeaweedFS shell stdin unavailable")?
        .write_all(input.as_bytes())?;
    let output = child
        .wait_with_output()
        .context("wait for SeaweedFS shell")?;
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if expected_error.is_some_and(|message| diagnostic.contains(message)) {
        return Ok(());
    }
    if !output.status.success() || diagnostic.contains("error:") || diagnostic.contains("Error:") {
        bail!("SeaweedFS provisioning command failed");
    }
    Ok(())
}

pub(super) fn credential_env(credentials: &Credentials) -> [(&'static str, &str); 16] {
    [
        (
            "NAVIGATOR_SEAWEEDFS_ACCESS_KEY",
            &credentials.documents.access_key,
        ),
        (
            "NAVIGATOR_SEAWEEDFS_SECRET_KEY",
            &credentials.documents.secret_key,
        ),
        (
            "NAVIGATOR_SEAWEEDFS_ASSETS_ACCESS_KEY",
            &credentials.assets.access_key,
        ),
        (
            "NAVIGATOR_SEAWEEDFS_ASSETS_SECRET_KEY",
            &credentials.assets.secret_key,
        ),
        (
            "NAVIGATOR_SEAWEEDFS_APPLICATIONS_ACCESS_KEY",
            &credentials.applications.access_key,
        ),
        (
            "NAVIGATOR_SEAWEEDFS_APPLICATIONS_SECRET_KEY",
            &credentials.applications.secret_key,
        ),
        (
            "NAVIGATOR_SEAWEEDFS_EXPORTS_ACCESS_KEY",
            &credentials.exports.access_key,
        ),
        (
            "NAVIGATOR_SEAWEEDFS_EXPORTS_SECRET_KEY",
            &credentials.exports.secret_key,
        ),
        (
            "NAVIGATOR_SEAWEEDFS_ARCHIVES_ACCESS_KEY",
            &credentials.archives.access_key,
        ),
        (
            "NAVIGATOR_SEAWEEDFS_ARCHIVES_SECRET_KEY",
            &credentials.archives.secret_key,
        ),
        (
            "NAVIGATOR_SEAWEEDFS_TELEMETRY_ACCESS_KEY",
            &credentials.telemetry.access_key,
        ),
        (
            "NAVIGATOR_SEAWEEDFS_TELEMETRY_SECRET_KEY",
            &credentials.telemetry.secret_key,
        ),
        (
            "NAVIGATOR_SEAWEEDFS_LFS_ACCESS_KEY",
            &credentials.lfs.access_key,
        ),
        (
            "NAVIGATOR_SEAWEEDFS_LFS_SECRET_KEY",
            &credentials.lfs.secret_key,
        ),
        (
            "NAVIGATOR_SEAWEEDFS_ARCHIVED_PROJECTS_ACCESS_KEY",
            &credentials.archived_projects.access_key,
        ),
        (
            "NAVIGATOR_SEAWEEDFS_ARCHIVED_PROJECTS_SECRET_KEY",
            &credentials.archived_projects.secret_key,
        ),
    ]
}

pub(super) fn export(credentials: &Credentials) {
    for (key, value) in credential_env(credentials) {
        std::env::set_var(key, value);
    }
}

pub(super) fn random_hex() -> String {
    rand::random::<[u8; 32]>()
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            use std::fmt::Write as _;
            let _ = write!(output, "{byte:02x}");
            output
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provisioning_errors_are_redacted_and_only_expected_failures_are_accepted() {
        let command = || {
            let mut command = Command::new("sh");
            command.args(["-c", "cat >/dev/null; printf 'credential-value\\nerror: bucket already exists\\n'; exit 1"]);
            command
        };
        let error = shell(&mut command(), "private-input", None).unwrap_err();
        assert!(!error.to_string().contains("credential-value"));
        assert!(!error.to_string().contains("private-input"));
        shell(&mut command(), "private-input", Some("already exists")).unwrap();
        assert!(shell(&mut command(), "private-input", Some("not found")).is_err());
    }

    #[test]
    fn lane_credentials_have_no_global_or_anonymous_grants() {
        let key = new_key();
        let config = identity("navigator-documents", &key);
        assert_eq!(config["name"], "navigator-documents");
        for action in config["actions"].as_array().unwrap() {
            assert!(action.as_str().unwrap().ends_with(":navigator-documents"));
        }
        assert_ne!(key.access_key, key.secret_key);
        assert_eq!(LANES.len(), 8);
    }
}

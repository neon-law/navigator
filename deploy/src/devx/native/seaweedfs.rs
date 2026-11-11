//! Shared native `SeaweedFS` with bucket-scoped, persistent tenant credentials.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use super::super::seaweedfs::{
    credential_env, credentials, identity, new_key, shell, LaneCredentials,
};
use super::supervisor::Service;

pub(super) struct Tenant {
    pub(super) buckets: BTreeMap<String, String>,
    pub(super) env: BTreeMap<String, String>,
}

const FORMULA: &str = "seaweedfs";
const BINARY: &str = "weed";
const LANES: &[(&str, &str)] = &[
    ("documents", "NAVIGATOR_STORAGE_BUCKET"),
    ("assets", "NAVIGATOR_ASSETS_BUCKET"),
    ("applications", "NAVIGATOR_APPLICATIONS_BUCKET"),
    ("exports", "NAVIGATOR_SURREAL_ARCHIVES_BUCKET"),
    ("archives", "NAVIGATOR_ARCHIVES_BUCKET"),
    ("telemetry", "NAVIGATOR_TELEMETRY_BUCKET"),
    ("lfs", "NAVIGATOR_LFS_BUCKET"),
    ("archived-projects", "NAVIGATOR_ARCHIVED_PROJECTS_BUCKET"),
];

pub(super) fn bucket_names(tenant: &str) -> BTreeMap<String, String> {
    LANES
        .iter()
        .map(|(suffix, name)| ((*name).to_string(), bucket_name(tenant, suffix)))
        .collect()
}

fn bucket_name(tenant: &str, suffix: &str) -> String {
    format!("{}-{suffix}", tenant.replace('_', "-"))
}

fn state_dir(root: &Path) -> PathBuf {
    super::supervisor::service_dir(root, super::SEAWEEDFS_LABEL)
}

fn private_file(path: &Path, contents: &[u8]) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .context("create private SeaweedFS config")?;
    file.write_all(contents)
        .context("write private SeaweedFS config")
}

fn server_args(data: &Path, s3: u16, master: u16, filer: u16) -> Vec<String> {
    vec![
        "server".into(),
        format!("-dir={}", data.display()),
        "-ip=127.0.0.1".into(),
        "-ip.bind=127.0.0.1".into(),
        "-s3".into(),
        format!("-s3.port={s3}"),
        format!("-master.port={master}"),
        format!("-master.port.grpc={}", master + 10_000),
        format!("-filer.port={filer}"),
        format!("-filer.port.grpc={}", filer + 10_000),
        format!("-volume.port={}", super::SEAWEEDFS_VOLUME_PORT_BASE),
        format!(
            "-volume.port.grpc={}",
            super::SEAWEEDFS_VOLUME_PORT_BASE + 10_000
        ),
        format!(
            "-s3.port.grpc={}",
            super::SEAWEEDFS_VOLUME_PORT_BASE + 10_001
        ),
        "-s3.port.iceberg=0".into(),
        "-s3.port.lance=0".into(),
        "-master.telemetry=false".into(),
        "-master.volumeSizeLimitMB=64".into(),
        "-volume.max=64".into(),
        format!("-s3.config={}", data.join("s3.json").display()),
    ]
}

pub(super) fn service(root: &Path, s3: u16, master: u16, filer: u16) -> Result<Service> {
    let data = state_dir(root);
    std::fs::create_dir_all(&data).context("create SeaweedFS data directory")?;
    let config = data.join("s3.json");
    // An inaccessible sentinel prevents anonymous S3 access before tenant IAM is applied.
    if !config.exists() {
        let contents = serde_json::to_vec(&serde_json::json!({
            "identities": [identity("navigator-bootstrap", &new_key())]
        }))?;
        private_file(&config, &contents)?;
    }
    Ok(Service {
        label: super::SEAWEEDFS_LABEL,
        program: super::preflight::binary(FORMULA, BINARY)?,
        args: server_args(&data, s3, master, filer),
        env: Vec::new(),
        cwd: data,
        port: s3,
    })
}

fn command() -> Result<Command> {
    let mut command = Command::new(super::preflight::binary(FORMULA, BINARY)?);
    command.args([
        "shell",
        &format!("-master=127.0.0.1:{}", super::SEAWEEDFS_MASTER_PORT_BASE),
        &format!("-filer=127.0.0.1:{}", super::SEAWEEDFS_FILER_PORT_BASE),
    ]);
    Ok(command)
}

pub(super) fn provision(root: &Path, tenant: &str) -> Result<Tenant> {
    // Caller holds the host registry lock throughout provisioning and teardown.
    let key_file = state_dir(root).join(format!("{tenant}.json"));
    let keys: BTreeMap<String, LaneCredentials> = if key_file.exists() {
        serde_json::from_slice(&std::fs::read(&key_file)?)
            .context("read persistent SeaweedFS tenant credentials")?
    } else {
        let keys: BTreeMap<_, _> = LANES
            .iter()
            .map(|(lane, _)| ((*lane).to_string(), new_key()))
            .collect();
        private_file(&key_file, &serde_json::to_vec(&keys)?)?;
        keys
    };
    let buckets = bucket_names(tenant);
    for (lane, env_name) in LANES {
        let bucket = &buckets[*env_name];
        let key = keys
            .get(*lane)
            .context("missing native SeaweedFS lane credentials")?;
        let script = format!("s3.configure -user={bucket} -actions=Read,Write,List,Tagging -buckets={bucket} -access_key={} -secret_key={} -apply\n", key.access_key, key.secret_key);
        shell(&mut command()?, &script, None)?;
        shell(
            &mut command()?,
            &format!("s3.bucket.create -name={bucket} -owner={bucket}\n"),
            Some("already exists"),
        )?;
    }
    let credentials = credentials(&keys)?;
    let mut env: BTreeMap<_, _> = credential_env(&credentials)
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    env.extend(buckets.clone());
    Ok(Tenant { buckets, env })
}

pub(super) fn remove_tenant(_root: &Path, tenant: &Tenant) -> Result<()> {
    for bucket in tenant.buckets.values() {
        if !bucket
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            bail!("invalid SeaweedFS tenant bucket name");
        }
        shell(
            &mut command()?,
            &format!("s3.bucket.delete -name={bucket}\n"),
            Some("not found"),
        )?;
        shell(
            &mut command()?,
            &format!("s3.configure -user={bucket} -delete -apply\n"),
            Some("not found"),
        )?;
    }
    // Retain the private key file so interrupted cleanup can be retried with stable credentials.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_credentials_are_private_and_cannot_be_overwritten() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tenant.json");
        private_file(&path, b"original").unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(private_file(&path, b"replacement").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
    }

    #[test]
    fn tenant_bucket_names_are_isolated_and_known_before_provisioning() {
        let first = bucket_names("navigator_alpha_1234");
        let second = bucket_names("navigator_beta_5678");
        assert_eq!(first.len(), LANES.len());
        assert_eq!(
            first["NAVIGATOR_STORAGE_BUCKET"],
            "navigator-alpha-1234-documents"
        );
        assert!(first
            .values()
            .all(|name| !second.values().any(|other| other == name)));
    }

    #[test]
    fn native_listeners_are_explicit_loopback_and_auxiliary_catalogs_are_disabled() {
        let args = server_args(Path::new("/host/runtime/seaweedfs"), 20559, 21300, 21400);
        for arg in [
            "-ip=127.0.0.1",
            "-ip.bind=127.0.0.1",
            "-s3.port=20559",
            "-master.port=21300",
            "-filer.port=21400",
            "-s3.port.iceberg=0",
            "-s3.port.lance=0",
        ] {
            assert!(args.iter().any(|value| value == arg), "missing {arg}");
        }
        assert!(args
            .iter()
            .any(|value| value == "-s3.config=/host/runtime/seaweedfs/s3.json"));
    }
}

//! Build, start, and reload the native `workflows-service` worker.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use super::supervisor::Service;

const BINARY: &str = "workflows-service";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Ports {
    pub(super) listen: u16,
    pub(super) health: u16,
}

pub(super) fn ports(slot: u16) -> Ports {
    Ports {
        listen: super::WORKFLOWS_SERVICE_LISTEN_PORT_BASE + slot,
        health: super::WORKFLOWS_SERVICE_HEALTH_PORT_BASE + slot,
    }
}

pub(super) fn build(root: &Path) -> Result<PathBuf> {
    let status = Command::new("cargo")
        .args(["build", "-p", BINARY])
        .current_dir(root)
        .status()
        .context("build workflows-service")?;
    if !status.success() {
        bail!("cargo build -p {BINARY} failed with {status}");
    }
    let metadata = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(root)
        .output()
        .context("locate the Cargo target directory")?;
    if !metadata.status.success() {
        bail!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&metadata.stderr).trim()
        );
    }
    built_binary(&metadata.stdout)
}

/// The debug worker binary under the target directory `cargo metadata` reports.
fn built_binary(metadata: &[u8]) -> Result<PathBuf> {
    let target = serde_json::from_slice::<serde_json::Value>(metadata)?
        .get("target_directory")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
        .context("cargo metadata returned no target directory")?;
    let binary = target.join("debug").join(BINARY);
    if !binary.is_file() {
        bail!("cargo built {BINARY}, but {} is missing", binary.display());
    }
    Ok(binary)
}

pub(super) fn service(root: &Path, slot: u16, binary: PathBuf) -> Service {
    let ports = ports(slot);
    let mut env: Vec<(String, String)> = dotenvy::from_path_iter(root.join(".devx/env"))
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .collect();
    env.retain(|(key, _)| {
        key != "WORKFLOWS_SERVICE_LISTEN" && key != "WORKFLOWS_SERVICE_HEALTH_LISTEN"
    });
    env.extend([
        (
            "WORKFLOWS_SERVICE_LISTEN".to_string(),
            format!("127.0.0.1:{}", ports.listen),
        ),
        (
            "WORKFLOWS_SERVICE_HEALTH_LISTEN".to_string(),
            format!("127.0.0.1:{}", ports.health),
        ),
    ]);
    Service {
        label: super::WORKFLOWS_LABEL,
        program: binary,
        args: Vec::new(),
        env,
        cwd: root.to_path_buf(),
        port: ports.listen,
    }
}

#[cfg(test)]
mod tests {
    use super::{built_binary, ports, service};
    use std::path::PathBuf;

    #[test]
    fn the_built_worker_is_found_in_the_reported_target_directory_or_named_missing() {
        let target = tempfile::tempdir().unwrap();
        let metadata = serde_json::json!({ "target_directory": target.path() }).to_string();
        let binary = target.path().join("debug/workflows-service");
        assert_eq!(
            built_binary(metadata.as_bytes()).unwrap_err().to_string(),
            format!(
                "cargo built workflows-service, but {} is missing",
                binary.display()
            )
        );
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, "").unwrap();
        assert_eq!(built_binary(metadata.as_bytes()).unwrap(), binary);
        assert_eq!(
            built_binary(b"{}").unwrap_err().to_string(),
            "cargo metadata returned no target directory"
        );
        assert!(built_binary(b"not json").is_err());
    }

    #[test]
    fn the_worker_inherits_the_worktree_environment_except_its_own_listeners() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".devx")).unwrap();
        std::fs::write(
            root.path().join(".devx/env"),
            "NAV_BASE_URL=http://localhost:3001\nWORKFLOWS_SERVICE_LISTEN=stale\n",
        )
        .unwrap();
        let service = service(root.path(), 2, PathBuf::from("/bin/workflows-service"));
        let listen = format!("127.0.0.1:{}", ports(2).listen);
        assert_eq!(
            service.env,
            [
                (
                    "NAV_BASE_URL".to_owned(),
                    "http://localhost:3001".to_owned()
                ),
                ("WORKFLOWS_SERVICE_LISTEN".to_owned(), listen),
                (
                    "WORKFLOWS_SERVICE_HEALTH_LISTEN".to_owned(),
                    format!("127.0.0.1:{}", ports(2).health)
                ),
            ]
        );
        assert_eq!(service.cwd, root.path());
    }

    #[test]
    fn worker_service_carries_worktree_specific_listener_environment() {
        let service = service(
            PathBuf::from("/tmp/native").as_path(),
            7,
            PathBuf::from("/bin/workflows-service"),
        );
        assert!(service
            .env
            .contains(&("WORKFLOWS_SERVICE_LISTEN".into(), "127.0.0.1:21807".into())));
        assert!(service.env.contains(&(
            "WORKFLOWS_SERVICE_HEALTH_LISTEN".into(),
            "127.0.0.1:21907".into()
        )));
        assert_eq!(service.port, ports(7).listen);
    }
}

//! Permission refusal must precede asset access and cluster mutation.
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

fn stub(root: &Path, name: &str, body: &str) {
    let path = root.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn refused_ship(lane: &[&str], review: &str, expected: &str) {
    let bin = tempfile::tempdir().unwrap();
    let log = bin.path().join("calls");
    stub(
        bin.path(),
        "gcloud",
        &format!("echo gcloud \"$*\" >> '{}'\nexit 0", log.display()),
    );
    stub(bin.path(), "cosign", "exit 0");
    stub(
        bin.path(),
        "kubectl",
        &format!(
            r#"
echo "$*" >> '{}'
case "$*" in
  *get-contexts*) echo gke_example-project_example-region_example-cluster; exit 0 ;;
  *kustomize*) printf 'apiVersion: v1\nkind: Secret\nmetadata:\n  name: example-secret\n'; exit 0 ;;
  *selfsubjectaccessreviews*) cat >/dev/null; {review} ;;
esac
exit 0
"#,
            log.display()
        ),
    );
    let output = Command::new(env!("CARGO_BIN_EXE_navigator"))
        .args([
            "ops",
            "ship",
            "--deployment",
            "example-deployment",
            "--deployments-dir",
        ])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/deployment-tree"))
        .args(lane)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.path().display()))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "permission denial must abort");
    assert!(stderr.contains(expected), "{stderr}");
    let calls = fs::read_to_string(log).unwrap();
    assert!(
        !calls.contains("storage"),
        "assets must not be checked before permission refusal"
    );
    assert!(
        !calls.contains("rollout"),
        "a denied operator must not mutate the cluster"
    );
}

#[test]
fn denied_restart_permissions_stop_before_rollout() {
    refused_ship(
        &["--restart-only"],
        "echo '{\"status\":{\"allowed\":false}}'; exit 0",
        "missing Kubernetes permissions",
    );
}

#[test]
fn denied_full_roll_permissions_stop_before_assets() {
    refused_ship(
        &["--tag", "26.10.21"],
        "echo '{\"status\":{\"allowed\":false}}'; exit 0",
        "missing Kubernetes permissions",
    );
}

#[test]
fn unavailable_reviews_are_not_reported_as_denials() {
    refused_ship(
        &["--restart-only"],
        "exit 1",
        "permissions could not be checked",
    );
}

#[test]
fn evaluation_errors_are_not_reported_as_denials() {
    refused_ship(
        &["--restart-only"],
        "echo '{\"status\":{\"allowed\":false,\"evaluationError\":\"unavailable\"}}'; exit 0",
        "permissions could not be checked",
    );
}

#[test]
fn denied_image_only_diff_permissions_stop_before_assets() {
    refused_ship(
        &["--image-only", "--tag", "26.10.21"],
        "echo '{\"status\":{\"allowed\":false}}'; exit 0",
        "missing Kubernetes permissions",
    );
}

//! A cloud `ops ship` holds its own cluster credentials. The CLI loads
//! `.devx/env`, whose `KUBECONFIG` names the local KIND cluster in a dev
//! checkout; that value must never reach the ship's `gcloud` or `kubectl`.
//!
//! Stub `gcloud` and `kubectl` record the `KUBECONFIG` they receive and fail,
//! so the ship stops at its first cluster step without any network.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const KIND_KUBECONFIG: &str = "/kind/kubeconfig";

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/deployment-tree")
}

fn stub(dir: &Path, name: &str, log: &Path) {
    let path = dir.join(name);
    fs::write(
        &path,
        format!(
            "#!/bin/sh\necho \"{name} ${{KUBECONFIG:-unset}}\" >> '{}'\nexit 1\n",
            log.display()
        ),
    )
    .expect("write stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod stub");
    }
}

#[test]
fn a_dotenv_kubeconfig_cannot_redirect_a_cloud_ship() {
    let checkout = tempfile::tempdir().expect("synthetic dev checkout");
    fs::create_dir_all(checkout.path().join(".devx")).expect("create .devx");
    fs::write(
        checkout.path().join(".devx/env"),
        format!("KUBECONFIG={KIND_KUBECONFIG}\n"),
    )
    .expect("write .devx/env");
    let bin = tempfile::tempdir().expect("stub bin");
    let log = bin.path().join("calls.log");
    stub(bin.path(), "gcloud", &log);
    stub(bin.path(), "kubectl", &log);

    let output = Command::new(env!("CARGO_BIN_EXE_navigator"))
        .args([
            "ops",
            "ship",
            "--deployment",
            "example-deployment",
            "--deployments-dir",
        ])
        .arg(fixture_root())
        .args(["--tag", "26.10.21"])
        .current_dir(checkout.path())
        .env("PATH", format!("{}:/usr/bin:/bin", bin.path().display()))
        .env_remove("KUBECONFIG")
        .env_remove("NAVIGATOR_DEPLOYMENTS_DIR")
        .output()
        .expect("run navigator ops ship");

    assert!(
        !output.status.success(),
        "the stub cluster refuses the ship"
    );
    let calls = fs::read_to_string(&log).unwrap_or_else(|_| {
        panic!(
            "the ship must fetch its own cluster credentials first: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    let first = calls.lines().next().expect("one recorded call");
    let (tool, kubeconfig) = first.split_once(' ').expect("tool and KUBECONFIG");
    assert_eq!(tool, "gcloud", "credentials come from get-credentials");
    assert_ne!(kubeconfig, KIND_KUBECONFIG, "the dotenv KUBECONFIG leaked");
    assert_ne!(kubeconfig, "unset");
    assert!(
        !Path::new(kubeconfig)
            .parent()
            .expect("scratch kubeconfig has a directory")
            .exists(),
        "the scratch kubeconfig directory is removed when the ship exits"
    );
}

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/deployment-tree")
}

fn synthetic_deployment_tree() -> tempfile::TempDir {
    let tree = tempfile::tempdir().expect("temporary synthetic deployment tree");
    let source = fixture_root().join("deployments/example-deployment");
    let row = tree.path().join("deployments/example-deployment");
    fs::create_dir_all(&row).expect("create synthetic deployment row");
    for filename in ["config.toml", "secrets.enc.yaml"] {
        fs::copy(source.join(filename), row.join(filename)).expect("copy synthetic fixture file");
    }
    write_encrypted_keys(
        &row.join("postgres-secrets.enc.yaml"),
        &[
            "NAVIGATOR_POSTGRES_PASSWORD",
            "NAVIGATOR_POSTGRES_TLS_CERTIFICATE",
            "NAVIGATOR_POSTGRES_TLS_PRIVATE_KEY",
            "NAVIGATOR_POSTGRES_CA_CERT",
            "NAVIGATOR_POSTGRES_APP_PASSWORD",
            "NAVIGATOR_POSTGRES_MIGRATOR_PASSWORD",
            "NAVIGATOR_POSTGRES_BACKUP_PASSWORD",
        ],
    );
    write_encrypted_keys(
        &row.join("forgejo-secrets.enc.yaml"),
        &[
            "NAVIGATOR_FORGEJO_DATABASE_PASSWORD",
            "NAVIGATOR_FORGEJO_SECRET_KEY",
            "NAVIGATOR_FORGEJO_INTERNAL_TOKEN",
            "NAVIGATOR_FORGEJO_OAUTH2_JWT_SECRET",
            "NAVIGATOR_FORGEJO_POSTGRES_CA_CERT",
            "NAVIGATOR_FORGEJO_OIDC_CLIENT_SECRET",
        ],
    );
    tree
}

fn write_encrypted_keys(path: &Path, keys: &[&str]) {
    let entries = keys
        .iter()
        .map(|key| format!("{key}: 'ENC[AES256_GCM,synthetic]'"))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(path, format!("{entries}\nsops: {{}}\n"))
        .expect("write synthetic encrypted-key metadata");
}

fn run_ship(deployments_dir: &Path, lane: &[&str]) -> Output {
    let no_tools = tempfile::tempdir().expect("temporary empty PATH");
    Command::new(env!("CARGO_BIN_EXE_navigator"))
        .env_clear()
        .env("PATH", no_tools.path())
        .arg("ops")
        .arg("ship")
        .arg("--deployment")
        .arg("example-deployment")
        .arg("--deployments-dir")
        .arg(deployments_dir)
        .args(lane)
        .arg("--dry-run")
        .output()
        .expect("run navigator with no external tools on PATH")
}

fn run_quiet_ship(deployments_dir: &Path, sink: &Path, lane: &[&str]) -> Output {
    let no_tools = tempfile::tempdir().expect("temporary empty PATH");
    Command::new(env!("CARGO_BIN_EXE_navigator"))
        .env_clear()
        .env("PATH", no_tools.path())
        .arg("ops")
        .arg("ship")
        .arg("--deployment")
        .arg("example-deployment")
        .arg("--deployments-dir")
        .arg(deployments_dir)
        .args(lane)
        .arg("--dry-run")
        .arg("--quiet")
        .arg("--log-sink")
        .arg(format!("file://{}", sink.display()))
        .output()
        .expect("run quiet navigator with no external tools on PATH")
}

#[test]
fn committed_example_row_produces_a_local_plan_without_external_tools() {
    let no_tools = tempfile::tempdir().expect("temporary empty PATH");
    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("CLI crate has a repository parent")
        .canonicalize()
        .expect("canonical repository root");
    let output = Command::new(env!("CARGO_BIN_EXE_navigator"))
        .env_clear()
        .env("PATH", no_tools.path())
        .arg("ops")
        .arg("ship")
        .arg("--deployment")
        .arg("example-deployment")
        .arg("--deployments-dir")
        .arg(&repository_root)
        .arg("--dry-run")
        .arg("--tag")
        .arg("26.10.19")
        .output()
        .expect("run committed example with no external tools on PATH");
    assert!(
        output.status.success(),
        "committed example offline plan failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = String::from_utf8_lossy(&output.stderr);
    assert!(report.contains("OFFLINE SHIP PLAN (roll)"), "{report}");
    let rendered_count = report
        .split("parsed ")
        .nth(1)
        .and_then(|summary| summary.split_whitespace().next())
        .and_then(|count| count.parse::<usize>().ok())
        .expect("plan reports its rendered document count");
    assert!(
        rendered_count > 0,
        "plan should parse rendered manifests; got {rendered_count}"
    );
    assert!(
        report.contains("would: reconcile PostgreSQL StatefulSet"),
        "{report}"
    );
    assert!(
        report.contains("not checked: live authentication"),
        "{report}"
    );
}

#[test]
fn each_dry_run_lane_plans_offline_with_no_external_tools() {
    let deployments = synthetic_deployment_tree();
    for lane in [
        vec!["--tag", "26.10.19"],
        vec!["--image-only", "--tag", "26.10.19"],
        vec!["--restart-only"],
    ] {
        let output = run_ship(deployments.path(), &lane);
        assert!(
            output.status.success(),
            "offline plan failed for {lane:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report = String::from_utf8_lossy(&output.stderr);
        assert!(report.contains("OFFLINE SHIP PLAN"), "{report}");
        assert!(report.contains("not checked:"), "{report}");
        assert!(
            report.contains("no cloud or cluster commands were run"),
            "{report}"
        );
    }
}

#[test]
fn quiet_offline_plan_does_not_upload_or_disclose_sink_details() {
    let temp = tempfile::tempdir().expect("temporary transcript sink");
    let deployments = synthetic_deployment_tree();
    let marker = "synthetic-private-sink-marker";
    let sink = temp.path().join(marker);
    let output = run_quiet_ship(deployments.path(), &sink, &["--tag", "26.10.19"]);
    assert!(
        output.status.success(),
        "quiet offline plan failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!sink.exists(), "offline plan must not contact its sink");
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !report.contains(marker),
        "quiet output must not disclose sink details"
    );
    assert!(
        report.contains("no remote transcript was uploaded"),
        "{report}"
    );
    assert!(report.contains("not performed"), "{report}");
}

#[test]
fn quiet_offline_plan_suppresses_validation_errors_and_never_contacts_sink() {
    let temp = tempfile::tempdir().expect("temporary transcript sink");
    let deployments = synthetic_deployment_tree();
    let marker = "synthetic-private-sink-marker";
    let sink = temp.path().join(marker);
    let output = run_quiet_ship(deployments.path(), &sink, &["--tag", "invalid-tag-marker"]);
    assert!(!output.status.success());
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        report.contains("details suppressed in quiet mode"),
        "{report}"
    );
    assert!(!report.contains(marker), "{report}");
    assert!(!report.contains("invalid-tag-marker"), "{report}");
    assert!(
        !sink.exists(),
        "failed offline plan must not contact its sink"
    );

    let invalid_tree = tempfile::tempdir().expect("temporary invalid deployment tree");
    let private_marker = "synthetic-private-path-marker";
    let row = invalid_tree
        .path()
        .join(private_marker)
        .join("deployments/example-deployment");
    fs::create_dir_all(&row).expect("create invalid synthetic row");
    let source = deployments.path().join("deployments/example-deployment");
    let mut config = fs::read_to_string(source.join("config.toml")).expect("read synthetic config");
    config = config.replace(
        "NAVIGATOR_IMAGE_REGISTRY = \"ghcr.io/example/navigator\"\n",
        "",
    );
    fs::write(row.join("config.toml"), config).expect("write invalid synthetic config");
    fs::copy(
        source.join("secrets.enc.yaml"),
        row.join("secrets.enc.yaml"),
    )
    .expect("copy synthetic encrypted-key metadata");
    let invalid_config = run_quiet_ship(
        &invalid_tree.path().join(private_marker),
        &sink,
        &["--tag", "26.10.19"],
    );
    assert!(!invalid_config.status.success());
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&invalid_config.stdout),
        String::from_utf8_lossy(&invalid_config.stderr)
    );
    assert!(
        report.contains("details suppressed in quiet mode"),
        "{report}"
    );
    assert!(!report.contains(private_marker), "{report}");
    assert!(
        !sink.exists(),
        "failed offline plan must not contact its sink"
    );
}

#[test]
fn offline_plan_still_rejects_invalid_local_configuration_and_tags() {
    let deployments = synthetic_deployment_tree();
    let invalid_tag = run_ship(deployments.path(), &["--tag", "not-a-release"]);
    assert!(!invalid_tag.status.success());
    assert!(String::from_utf8_lossy(&invalid_tag.stderr).contains("release tag"));

    let temp = tempfile::tempdir().expect("temporary deployment tree");
    let copied = temp.path().join("deployments/example-deployment");
    fs::create_dir_all(&copied).expect("create synthetic row");
    let mut config = fs::read_to_string(
        deployments
            .path()
            .join("deployments/example-deployment/config.toml"),
    )
    .expect("read synthetic config");
    config = config.replace(
        "NAVIGATOR_IMAGE_REGISTRY = \"ghcr.io/example/navigator\"\n",
        "",
    );
    fs::write(copied.join("config.toml"), config).expect("write invalid synthetic config");
    for filename in [
        "secrets.enc.yaml",
        "postgres-secrets.enc.yaml",
        "forgejo-secrets.enc.yaml",
    ] {
        fs::copy(
            deployments
                .path()
                .join("deployments/example-deployment")
                .join(filename),
            copied.join(filename),
        )
        .expect("copy synthetic encrypted-key metadata");
    }
    let invalid_config = run_ship(temp.path(), &["--tag", "26.10.19"]);
    assert!(!invalid_config.status.success());
    assert!(String::from_utf8_lossy(&invalid_config.stderr).contains("NAVIGATOR_IMAGE_REGISTRY"));
}

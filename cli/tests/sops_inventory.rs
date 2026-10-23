//! `scripts/validate-sops-inventory.sh` against synthetic two-row trees.
//!
//! Every tree here is generated in a temporary directory: the inventory is
//! the repository's own `deployments/sops-inventory.yaml` copied into a
//! synthetic external deployment root, and each row's `secrets.enc.yaml`
//! carries `ENC[FIXTURE]` in place of ciphertext. No test decrypts, and no
//! value is a credential.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PRODUCTION: &str = "example-production";
const STAGING: &str = "example-staging";

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the cli crate has a workspace parent")
        .to_path_buf()
}

/// The `common` keys both synthetic rows carry, read from the real inventory
/// so the test follows it rather than restating it.
fn common_keys() -> Vec<String> {
    let inventory = fs::read_to_string(workspace().join("deployments/sops-inventory.yaml"))
        .expect("read the SOPS inventory");
    let mut keys = Vec::new();
    let mut in_common = false;
    for line in inventory.lines() {
        if !line.starts_with(' ') && line.ends_with(':') {
            in_common = line == "common:";
        } else if in_common {
            if let Some(key) = line.strip_prefix("  ").and_then(|l| l.split(':').next()) {
                keys.push(key.to_owned());
            }
        }
    }
    assert!(!keys.is_empty(), "the inventory lists common keys");
    keys
}

fn inventory_keys(section_name: &str) -> Vec<String> {
    let inventory = fs::read_to_string(workspace().join("deployments/sops-inventory.yaml"))
        .expect("read the SOPS inventory");
    let mut keys = Vec::new();
    let mut active = false;
    for line in inventory.lines() {
        if !line.starts_with(' ') && line.ends_with(':') {
            active = line.trim_end_matches(':') == section_name;
        } else if active {
            if let Some(key) = line.strip_prefix("  ").and_then(|l| l.split(':').next()) {
                keys.push(key.to_owned());
            }
        }
    }
    keys
}

fn secrets_file(lines: &[String]) -> String {
    let mut body = lines.join("\n");
    body.push_str("\nsops:\n    gcp_kms: []\n    version: 3.9.4\n");
    body
}

/// A tree whose two rows carry `common` plus `production` / `staging` extras.
fn tree(production: &[&str], staging: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    let deployments = dir.path().join("deployments");
    fs::create_dir_all(&deployments).expect("create deployments/");
    fs::copy(
        workspace().join("deployments/sops-inventory.yaml"),
        deployments.join("sops-inventory.yaml"),
    )
    .expect("copy the inventory");
    for (row, extras) in [(PRODUCTION, production), (STAGING, staging)] {
        let row_dir = deployments.join(row);
        fs::create_dir_all(&row_dir).expect("create the row");
        let lines: Vec<String> = common_keys()
            .into_iter()
            .map(|key| format!("{key}: ENC[FIXTURE]"))
            .chain(extras.iter().map(|line| {
                if line.contains(':') {
                    (*line).to_owned()
                } else {
                    format!("{line}: ENC[FIXTURE]")
                }
            }))
            .collect();
        fs::write(row_dir.join("secrets.enc.yaml"), secrets_file(&lines)).expect("write row");
    }
    dir
}

fn validate(root: &Path) -> Output {
    Command::new("bash")
        .arg(workspace().join("scripts/validate-sops-inventory.sh"))
        .arg(root)
        .env("SOPS_PRODUCTION_ROW", PRODUCTION)
        .env("SOPS_STAGING_ROW", STAGING)
        .output()
        .expect("run the validator")
}

fn write_service_files(root: &Path, row_name: &str, postgres: &[String], forgejo: &[String]) {
    let row = root.join("deployments").join(row_name);
    for (file, keys) in [
        ("postgres-secrets.enc.yaml", postgres),
        ("forgejo-secrets.enc.yaml", forgejo),
    ] {
        if !keys.is_empty() {
            let lines = keys
                .iter()
                .map(|key| format!("{key}: ENC[FIXTURE]"))
                .collect::<Vec<_>>();
            fs::write(row.join(file), secrets_file(&lines)).expect("write service file");
        }
    }
}

fn assert_refused(output: &Output, needle: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "the validator must refuse: {stderr}"
    );
    assert!(stderr.contains(needle), "expected {needle:?} in: {stderr}");
}

const SOCIAL_SECRETS: &[&str] = &[
    "NAVIGATOR_META_APP_SECRET",
    "NAVIGATOR_META_WEBHOOK_VERIFY_TOKEN",
    "NAVIGATOR_META_WEBHOOK_APP_SECRET",
    "NAVIGATOR_THREADS_APP_SECRET",
];

#[test]
fn the_repository_inventory_is_valid_for_a_synthetic_external_root() {
    let dir = tree(&[], &[]);
    let output = validate(dir.path());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn production_may_carry_social_secrets_and_runtime_kms_without_notion() {
    let mut production = SOCIAL_SECRETS.to_vec();
    production.push("NAVIGATOR_RUNTIME_KMS_KEY");
    let dir = tree(&production, &[]);
    let output = validate(dir.path());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn each_row_may_hold_its_own_runtime_kms_key() {
    let dir = tree(
        &["NAVIGATOR_RUNTIME_KMS_KEY"],
        &["NAVIGATOR_RUNTIME_KMS_KEY", "NAVIGATOR_NOTION_DATABASE_ID"],
    );
    let output = validate(dir.path());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_social_secret_copied_to_staging_is_refused_as_a_leak() {
    for key in SOCIAL_SECRETS {
        let dir = tree(SOCIAL_SECRETS, &[key]);
        let output = validate(dir.path());
        assert_refused(&output, "production-only");
        assert_refused(&output, key);
    }
}

#[test]
fn a_plaintext_value_is_refused() {
    let dir = tree(&["NAVIGATOR_META_APP_SECRET: synthetic-plaintext"], &[]);
    let output = validate(dir.path());
    assert_refused(&output, "plaintext");
    assert_refused(&output, "NAVIGATOR_META_APP_SECRET");
}

#[test]
fn a_block_scalar_that_would_hide_plaintext_is_refused() {
    let dir = tree(&[], &["NAVIGATOR_RUNTIME_KMS_KEY: |"]);
    let output = validate(dir.path());
    assert_refused(&output, "plaintext");
}

#[test]
fn a_staging_only_key_in_production_is_refused() {
    let dir = tree(&["NAVIGATOR_NOTION_DATABASE_ID"], &[]);
    let output = validate(dir.path());
    assert_refused(&output, "NAVIGATOR_NOTION_DATABASE_ID");
}

#[test]
fn an_operator_only_key_in_either_row_is_refused() {
    let dir = tree(&["RESTATE_ADMIN_TOKEN"], &[]);
    assert_refused(&validate(dir.path()), "RESTATE_ADMIN_TOKEN");
}

#[test]
fn complete_service_only_files_are_allowed_but_remain_separate() {
    let dir = tree(&[], &[]);
    let postgres = inventory_keys("postgres_service_only");
    let forgejo = inventory_keys("forgejo_service_only");
    assert!(!postgres.is_empty() && !forgejo.is_empty());
    write_service_files(dir.path(), PRODUCTION, &postgres, &forgejo);
    write_service_files(dir.path(), STAGING, &postgres, &forgejo);
    let output = validate(dir.path());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn service_file_must_contain_the_complete_key_set() {
    let dir = tree(&[], &[]);
    let postgres = inventory_keys("postgres_service_only");
    write_service_files(dir.path(), PRODUCTION, &postgres[..1], &[]);
    assert_refused(
        &validate(dir.path()),
        "PostgreSQL service file is incomplete",
    );
}

#[test]
fn postgres_client_logins_are_allowed_as_a_complete_set() {
    let dir = tree(&[], &[]);
    let mut postgres = inventory_keys("postgres_service_only");
    let clients = inventory_keys("postgres_client_service_only");
    assert_eq!(clients.len(), 4);
    postgres.extend(clients);
    write_service_files(dir.path(), STAGING, &postgres, &[]);
    let output = validate(dir.path());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_partial_postgres_client_login_set_is_refused() {
    let dir = tree(&[], &[]);
    let mut postgres = inventory_keys("postgres_service_only");
    let clients = inventory_keys("postgres_client_service_only");
    postgres.push(clients[0].clone());
    write_service_files(dir.path(), STAGING, &postgres, &[]);
    let output = validate(dir.path());
    assert_refused(&output, "PostgreSQL service file is incomplete");
    assert_refused(&output, &clients[1]);
}

#[test]
fn the_forgejo_oidc_client_secret_is_optional_in_the_forgejo_file() {
    let dir = tree(&[], &[]);
    let mut forgejo = inventory_keys("forgejo_service_only");
    let oidc = inventory_keys("forgejo_oidc_service_only");
    assert_eq!(oidc, vec!["NAVIGATOR_FORGEJO_OIDC_CLIENT_SECRET"]);
    write_service_files(dir.path(), STAGING, &[], &forgejo);
    forgejo.extend(oidc);
    write_service_files(dir.path(), PRODUCTION, &[], &forgejo);
    let output = validate(dir.path());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn the_forgejo_oidc_client_secret_alone_is_an_incomplete_forgejo_file() {
    let dir = tree(&[], &[]);
    let oidc = inventory_keys("forgejo_oidc_service_only");
    write_service_files(dir.path(), PRODUCTION, &[], &oidc);
    assert_refused(&validate(dir.path()), "Forgejo service file is incomplete");
}

#[test]
fn the_forgejo_oidc_client_secret_in_the_web_file_is_refused() {
    let oidc = inventory_keys("forgejo_oidc_service_only");
    let dir = tree(&[], &[&oidc[0]]);
    assert_refused(&validate(dir.path()), "service-only key(s)");
}

#[test]
fn a_postgres_client_login_in_the_web_file_is_refused() {
    let clients = inventory_keys("postgres_client_service_only");
    let dir = tree(&[], &[&clients[1]]);
    assert_refused(&validate(dir.path()), "service-only key(s)");
}

#[test]
fn an_empty_service_file_is_refused() {
    let dir = tree(&[], &[]);
    let file = dir
        .path()
        .join("deployments")
        .join(PRODUCTION)
        .join("postgres-secrets.enc.yaml");
    fs::write(&file, "sops:\n    version: 3.9.4\n").expect("write empty synthetic service file");
    assert_refused(&validate(dir.path()), "has no encrypted service keys");
}

#[test]
fn service_only_key_in_the_web_file_is_refused() {
    let postgres = inventory_keys("postgres_service_only");
    let dir = tree(&[&postgres[0]], &[]);
    assert_refused(&validate(dir.path()), "service-only key(s)");
}

#[test]
fn postgres_key_in_forgejo_file_is_refused() {
    let postgres = inventory_keys("postgres_service_only");
    let dir = tree(&[], &[]);
    write_service_files(dir.path(), PRODUCTION, &[], &postgres);
    assert_refused(
        &validate(dir.path()),
        "Forgejo service file contains key(s)",
    );
}

#[test]
fn a_common_key_in_one_row_only_is_refused() {
    let dir = tree(&[], &[]);
    let staging = dir
        .path()
        .join("deployments")
        .join(STAGING)
        .join("secrets.enc.yaml");
    let body = fs::read_to_string(&staging).expect("read staging");
    let first = common_keys().remove(0);
    let mut trimmed = String::new();
    for line in body
        .lines()
        .filter(|line| !line.starts_with(&format!("{first}:")))
    {
        trimmed.push_str(line);
        trimmed.push('\n');
    }
    fs::write(&staging, trimmed).expect("write staging");
    assert_refused(&validate(dir.path()), "both rows or neither");
}

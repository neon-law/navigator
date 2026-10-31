//! `navigator project documents check` through the compiled binary (LAW-217).
//!
//! The checks live in `cli/src/projects/document_check.rs`. These tests pin
//! the wiring: the verb is read-only, reports over any documents root, gives
//! the two roots the same report, exits non-zero on findings, and emits JSON
//! on demand.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

const BODY: &str = "Synthetic fixture bytes.\n";
const BODY_SHA256: &str = "f986e072732f1c4370564347f687c57c80c366b4de12449e336230106bdc8b70";

fn navigator() -> Command {
    let mut command = Command::cargo_bin("navigator").unwrap();
    command.env_remove("GITHUB_REPOSITORY");
    // Exercise this build's rules; mismatched-release dispatch has its own suite.
    command.env("NAVIGATOR_RELEASE_TAG", env!("CARGO_PKG_VERSION"));
    command
}

fn write(root: &Path, relative: &str, bytes: impl AsRef<[u8]>) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
}

/// A tree laid out like `documents/`: one Matter Folder, a good invoice, and
/// a Finder stray.
fn matter_tree(root: &Path) {
    write(root, "Work Product/memo.md", BODY);
    write(root, "invoices/INV-1.pdf", BODY);
    write(root, ".DS_Store", "");
}

fn manifest(root: impl AsRef<Path>) {
    write(
        root.as_ref(),
        "navigator.yaml",
        format!(
            "version: \"{}\"\nproject:\n  host: navigator.test\n  name: shook-estate\n",
            env!("CARGO_PKG_VERSION")
        ),
    );
}

/// The checkout `--deep` reads pointers from: `navigator.yaml` plus
/// `blobs/memo.md.yaml` naming the fixture bytes.
fn checkout_with_pointer(root: impl AsRef<Path>, sha256: &str, size: usize) {
    manifest(root.as_ref());
    write(
        root.as_ref(),
        "blobs/memo.md.yaml",
        format!(
            "kind: filing\nvisibility: internal\ncurrent_version:\n  version: 1\n  \
             asset_id: 0197b82f-1c2f-7000-8000-000000000000\n  \
             created_at: \"2026-09-05T12:00:00Z\"\n  sha256: {sha256}\n  \
             size_bytes: {size}\n"
        ),
    );
}

fn findings(stdout: &str) -> Vec<(String, String)> {
    let report: Value = serde_json::from_str(stdout).expect("the report is valid JSON");
    report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|finding| {
            (
                finding["location"].as_str().unwrap().to_string(),
                finding["message"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn help_states_the_check_is_read_only() {
    navigator()
        .args(["project", "documents", "check", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Read-only"));
}

#[test]
fn a_misspelled_folder_and_a_bad_invoice_fail_with_their_rules() {
    let dir = TempDir::new().unwrap();
    matter_tree(dir.path());
    write(dir.path(), "Docket Filing/order.pdf", BODY);
    write(dir.path(), "invoices/receipt.pdf", BODY);

    navigator()
        .args(["project", "documents", "check"])
        .arg("--documents-dir")
        .arg(dir.path())
        .env("HOME", dir.path()) // keep the real CloudStorage mount out of it
        .assert()
        .failure()
        .code(1)
        .stdout(
            predicates::str::contains("Docket Filing: not a Matter Folder or sibling folder")
                .and(predicates::str::contains(
                    "invoices/receipt.pdf: does not match the invoice filename pattern",
                ))
                .and(predicates::str::contains("nothing was written")),
        );
}

#[test]
fn the_same_tree_gives_the_same_report_wherever_it_lives() {
    let checkout = TempDir::new().unwrap();
    matter_tree(&checkout.path().join("documents"));
    write(
        &checkout.path().join("documents"),
        "Docket Filing/order.pdf",
        BODY,
    );
    write(
        &checkout.path().join("documents"),
        "invoices/receipt.pdf",
        BODY,
    );

    let drive = TempDir::new().unwrap();
    matter_tree(&drive.path().join("shook-estate"));
    write(drive.path(), "shook-estate/Docket Filing/order.pdf", BODY);
    write(drive.path(), "shook-estate/invoices/receipt.pdf", BODY);

    let from_checkout = String::from_utf8(
        navigator()
            .args(["project", "documents", "check", "--json"])
            .arg("--documents-dir")
            .arg(checkout.path().join("documents"))
            .env("HOME", checkout.path())
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let from_drive = String::from_utf8(
        navigator()
            .args(["project", "documents", "check", "--json"])
            .arg("--documents-dir")
            .arg(drive.path().join("shook-estate"))
            .env("HOME", drive.path())
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();

    assert_eq!(findings(&from_checkout), findings(&from_drive));
    assert_eq!(
        findings(&from_checkout)
            .iter()
            .map(|(location, _)| location.as_str())
            .collect::<Vec<_>>(),
        vec!["Docket Filing", "invoices/receipt.pdf"]
    );
    for (_, message) in findings(&from_checkout) {
        assert!(
            message.starts_with("not a Matter Folder")
                || message.starts_with("does not match the invoice filename pattern"),
            "{message}"
        );
    }
}

#[test]
fn a_clean_tree_passes_and_writes_nothing() {
    let dir = TempDir::new().unwrap();
    matter_tree(dir.path());

    navigator()
        .args(["project", "documents", "check"])
        .arg("--documents-dir")
        .arg(dir.path())
        .env("HOME", dir.path())
        .assert()
        .success()
        .stdout(predicates::str::contains("documents check: no findings"));
    assert!(dir.path().join("Work Product/memo.md").is_file());
}

#[test]
fn deep_reads_pointer_bytes_from_a_foreign_tree() {
    let checkout = TempDir::new().unwrap();
    checkout_with_pointer(
        &checkout,
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        BODY.len(),
    );
    let drive = TempDir::new().unwrap();
    // `kind: filing` maps the pointer's bytes to `Docket Filings/` — the
    // same mapping the unit tests pin.
    write(drive.path(), "Docket Filings/memo.md", BODY);

    // The pointer's recorded sha256 names other bytes: a finding, wherever
    // the bytes live.
    navigator()
        .args(["project", "documents", "check", "--deep", "--json"])
        .arg("--documents-dir")
        .arg(drive.path())
        .env("HOME", drive.path())
        .current_dir(checkout.path())
        .assert()
        .failure()
        .code(1)
        .stdout(predicates::str::contains("blobs/memo.md.yaml"));

    // With the pointer agreeing with the bytes in the checked tree, clean.
    checkout_with_pointer(&checkout, BODY_SHA256, BODY.len());
    navigator()
        .args(["project", "documents", "check", "--deep", "--json"])
        .arg("--documents-dir")
        .arg(drive.path())
        .env("HOME", drive.path())
        .current_dir(checkout.path())
        .assert()
        .success();
}

#[test]
fn drive_local_needs_a_project_checkout() {
    let dir = TempDir::new().unwrap();
    navigator()
        .args(["project", "documents", "check", "--drive-local"])
        .env("HOME", dir.path())
        .current_dir(dir.path())
        .assert()
        .failure()
        .code(2)
        .stderr(predicates::str::contains("needs a Project checkout"));
}

#[test]
fn drive_local_names_the_missing_mount_or_asks_for_disambiguation() {
    let checkout = TempDir::new().unwrap();
    manifest(&checkout);

    // No CloudStorage at all under HOME.
    navigator()
        .args(["project", "documents", "check", "--drive-local"])
        .env("HOME", checkout.path())
        .current_dir(checkout.path())
        .assert()
        .failure()
        .code(2)
        .stderr(predicates::str::contains(
            "no Google Drive for desktop mount",
        ));

    // One mount, no folder named after the Project code.
    write(
        checkout.path(),
        "Library/CloudStorage/GoogleDrive-nick@firm/My Drive/other",
        "",
    );
    navigator()
        .args(["project", "documents", "check", "--drive-local"])
        .env("HOME", checkout.path())
        .current_dir(checkout.path())
        .assert()
        .failure()
        .code(2)
        .stderr(predicates::str::contains("no folder named `shook-estate`"));

    // The folder in the Shared drive is found and checked.
    let matter = checkout
        .path()
        .join("Library/CloudStorage/GoogleDrive-nick@firm/Shared drives/Legal/shook-estate");
    write(&matter, "Docket Filing/order.pdf", BODY);
    navigator()
        .args(["project", "documents", "check", "--drive-local", "--json"])
        .env("HOME", checkout.path())
        .current_dir(checkout.path())
        .assert()
        .failure()
        .code(1)
        .stdout(predicates::str::contains("Docket Filing"));
    assert_eq!(
        findings(
            &String::from_utf8(
                navigator()
                    .args(["project", "documents", "check", "--drive-local", "--json"])
                    .env("HOME", checkout.path())
                    .current_dir(checkout.path())
                    .output()
                    .unwrap()
                    .stdout,
            )
            .unwrap()
        )[0]
        .0,
        "Docket Filing"
    );
}

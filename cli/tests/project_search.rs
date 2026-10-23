use assert_cmd::Command;
use std::fs;

fn project() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("documents/filings")).unwrap();
    fs::create_dir_all(root.path().join("blobs/filings")).unwrap();
    fs::create_dir_all(root.path().join("documents/correspondence")).unwrap();
    fs::create_dir_all(root.path().join("blobs/correspondence")).unwrap();
    fs::write(
        root.path().join("documents/filings/order.txt"),
        "Order affirmed\n",
    )
    .unwrap();
    fs::write(
        root.path().join("documents/correspondence/mail.txt"),
        "Order discussed\n",
    )
    .unwrap();
    root
}

#[test]
fn exit_codes_and_court_filter_are_exposed_by_the_cli() {
    let root = project();
    Command::cargo_bin("navigator")
        .unwrap()
        .args(["project", "search", "affirmed", "--dir"])
        .arg(root.path())
        .assert()
        .success();
    Command::cargo_bin("navigator")
        .unwrap()
        .args(["project", "search", "absent", "--dir"])
        .arg(root.path())
        .assert()
        .code(1);
    Command::cargo_bin("navigator")
        .unwrap()
        .args(["project", "search", "discussed", "--dir"])
        .arg(root.path())
        .args(["--record", "court"])
        .assert()
        .code(1);
    fs::write(
        root.path().join("documents/filings/unknown.bin"),
        b"opaque bytes",
    )
    .unwrap();
    let incomplete = Command::cargo_bin("navigator")
        .unwrap()
        .args(["project", "search", "affirmed", "--dir"])
        .arg(root.path())
        .arg("--json")
        .assert();
    assert_eq!(incomplete.get_output().status.code(), Some(2));
    let stdout = String::from_utf8_lossy(&incomplete.get_output().stdout);
    assert!(stdout.contains("\"hits\""));
    assert!(stdout.contains("\"not_searched\""));
    Command::cargo_bin("navigator")
        .unwrap()
        .args(["project", "search", "affirmed", "--dir"])
        .arg(root.path())
        .args(["--record", "unsupported"])
        .assert()
        .code(2);
}

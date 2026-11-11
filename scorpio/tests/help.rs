//! Integration tests for `scorpio --help` and its subcommands.

use assert_cmd::Command;
use predicates::str;

fn help(args: &[&str]) -> String {
    let output = Command::cargo_bin("scorpio")
        .unwrap()
        .args(args)
        .output()
        .expect("run scorpio help");
    assert!(
        output.status.success(),
        "`scorpio {}` failed:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("help output is utf-8")
}

fn command_names(output: &str) -> Vec<&str> {
    let mut in_commands = false;
    let mut names = Vec::new();

    for line in output.lines() {
        match line.trim() {
            "Commands:" => {
                in_commands = true;
                continue;
            }
            "Options:" if in_commands => break,
            _ => {}
        }

        if in_commands {
            if let Some(command_row) = line
                .strip_prefix("  ")
                .filter(|rest| !rest.starts_with(char::is_whitespace))
            {
                let name = command_row
                    .split_whitespace()
                    .next()
                    .expect("command row has a command name");
                names.push(name);
            }
        }
    }

    names
}

#[test]
fn scorpio_help_lists_its_verbs_and_requires_an_explicit_bundle_directory() {
    assert_eq!(
        command_names(&help(&["--help"])),
        vec!["tables", "promote", "download", "help"]
    );
    let isolated = tempfile::tempdir().unwrap();
    let missing_credentials = isolated.path().join("missing-session.json");
    let output = Command::cargo_bin("scorpio")
        .unwrap()
        .env("NAVIGATOR_CREDENTIALS_FILE", &missing_credentials)
        .args(["download", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let output = String::from_utf8(output.stdout).unwrap();
    assert!(output.contains("--host <HOST>"));
    assert!(output.contains("--dir <DIR>"));
    Command::cargo_bin("scorpio")
        .unwrap()
        .env("NAVIGATOR_CREDENTIALS_FILE", &missing_credentials)
        .args(["download"])
        .assert()
        .failure()
        .stderr(str::contains("--dir <DIR>"));
}

#[test]
fn table_help_never_needs_credentials() {
    let isolated = tempfile::tempdir().unwrap();
    let missing = isolated.path().join("missing-session.json");
    let output = Command::cargo_bin("scorpio")
        .unwrap()
        .env("NAVIGATOR_CREDENTIALS_FILE", &missing)
        .args(["tables", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("--iceberg"));
    assert!(help.contains("--measure"));
    Command::cargo_bin("scorpio")
        .unwrap()
        .env("NAVIGATOR_CREDENTIALS_FILE", &missing)
        .args([
            "tables",
            "--measure",
            "--source-dir",
            "missing-source",
            "--from",
            "1900-01-01",
            "--to-exclusive",
            "1900-01-02",
            "--dir",
            "missing-output",
        ])
        .assert()
        .failure()
        .stderr(str::contains("--duckdb-bin"));
    assert!(help.contains("--duckdb-bin <DUCKDB_BIN>"));
    assert!(help.contains("verified executable"));
    assert!(help.contains("fixed-location"));
    assert!(help
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .contains("URI-safe ASCII"));
    for option in [
        "--source-dir <SOURCE_DIR>",
        "--from <FROM>",
        "--to-exclusive <TO_EXCLUSIVE>",
        "--dir <DIR>",
    ] {
        assert!(
            help.contains(option),
            "missing required table argument {option}"
        );
    }
    Command::cargo_bin("scorpio")
        .unwrap()
        .env("NAVIGATOR_CREDENTIALS_FILE", &missing)
        .args(["tables"])
        .assert()
        .failure()
        .stderr(str::contains("--source-dir <SOURCE_DIR>"));
}

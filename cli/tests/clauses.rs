//! The bundled Clause library is discoverable without a running app or database.

use assert_cmd::Command;

#[test]
fn list_reports_pinned_sources_and_their_document_contexts() {
    let output = Command::cargo_bin("navigator")
        .unwrap()
        .env_remove("NAVIGATOR_POSTGRES_HOST")
        .args(["clause", "list"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let listed = String::from_utf8(output.stdout).unwrap();
    for (source, kind) in rules::clauses::catalog().unwrap() {
        assert!(listed.contains(&format!("{source} — {kind}")));
    }
    assert_eq!(
        listed.lines().count(),
        rules::clauses::catalog().unwrap().len()
    );
}

#[test]
fn bundled_list_reports_version_kind_and_current_status() {
    let output = Command::cargo_bin("navigator")
        .unwrap()
        .env_remove("NAVIGATOR_POSTGRES_HOST")
        .args(["clause", "list"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let listed = String::from_utf8(output.stdout).unwrap();
    let clauses = rules::clauses::library_clauses().unwrap();
    for clause in &clauses {
        let newest = clauses
            .iter()
            .filter(|candidate| candidate.code == clause.code && candidate.retired_at.is_none())
            .map(|candidate| candidate.version)
            .max();
        let status = if clause.retired_at.is_some() {
            "retired"
        } else if Some(clause.version) == newest {
            "current"
        } else {
            "not current"
        };
        let row = format!(
            "{}@{} — {} — {status}",
            clause.code, clause.version, clause.kind
        );
        assert!(
            listed.lines().any(|line| line == row),
            "missing {row:?} in:\n{listed}"
        );
    }
}

#[test]
fn retired_versions_are_labelled_and_remain_readable() {
    for source in ["governing_law@1", "nevada_engagement_arbitration@1"] {
        let output = Command::cargo_bin("navigator")
            .unwrap()
            .env_remove("NAVIGATOR_POSTGRES_HOST")
            .args(["clause", "show", source])
            .output()
            .unwrap();
        assert!(output.status.success());
        let clause: serde_yaml::Value = serde_yaml::from_slice(&output.stdout).unwrap();
        assert!(clause["retired_at"].is_string());
        assert!(!clause["body"].as_str().unwrap().is_empty());
    }
}

#[test]
fn unavailable_postgres_falls_back_to_the_bundled_catalog() {
    let list = Command::cargo_bin("navigator")
        .unwrap()
        .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
        .env("NAVIGATOR_POSTGRES_PORT", "1")
        .env("NAVIGATOR_POSTGRES_USER", "navigator")
        .env("NAVIGATOR_POSTGRES_PASSWORD", "unused")
        .env("NAVIGATOR_POSTGRES_DATABASE", "navigator")
        .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
        .args(["clause", "list"])
        .output()
        .unwrap();
    assert!(list.status.success());
    let listed = String::from_utf8(list.stdout).unwrap();
    assert!(listed.contains("california_employment@1 — letter — current"));

    let show = Command::cargo_bin("navigator")
        .unwrap()
        .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
        .env("NAVIGATOR_POSTGRES_PORT", "1")
        .env("NAVIGATOR_POSTGRES_USER", "navigator")
        .env("NAVIGATOR_POSTGRES_PASSWORD", "unused")
        .env("NAVIGATOR_POSTGRES_DATABASE", "navigator")
        .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
        .args(["clause", "show", "engagement_arbitration@1"])
        .output()
        .unwrap();
    assert!(show.status.success());
    let clause: serde_yaml::Value = serde_yaml::from_slice(&show.stdout).unwrap();
    assert_eq!(clause["kind"], "onboarding");
}

#[test]
fn show_exposes_complete_wording_and_independent_arbitration_inputs() {
    let output = Command::cargo_bin("navigator")
        .unwrap()
        .env_remove("NAVIGATOR_POSTGRES_HOST")
        .args(["clause", "show", "engagement_arbitration@1"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let clause: serde_yaml::Value = serde_yaml::from_slice(&output.stdout).unwrap();
    assert_eq!(clause["kind"], "onboarding");
    assert!(clause["inputs"]["law"].is_mapping());
    assert!(clause["inputs"]["seat"].is_mapping());
    let body = clause["body"].as_str().unwrap();
    assert!(body.contains("{{law}}"));
    assert!(body.contains("{{seat}}"));
    assert!(body.contains("statutory fee-arbitration right that cannot be waived"));
    assert!(body.contains("consult independent counsel"));
}

#[test]
fn unknown_version_fails_with_an_actionable_error_and_no_prose() {
    let output = Command::cargo_bin("navigator")
        .unwrap()
        .env_remove("NAVIGATOR_POSTGRES_HOST")
        .args(["clause", "show", "engagement_arbitration@999"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("unknown Clause version `engagement_arbitration@999`"));
}

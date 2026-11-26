//! The native browser gate delegates resource ownership to the CLI and keeps
//! exact-image execution separate from source-host browser coverage.

use serde_yaml::Value;
use std::{fs, path::PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn native_browser_delegates_lifecycle_and_preserves_the_required_image_gate() {
    let workflow: Value = serde_yaml::from_str(
        &fs::read_to_string(root().join(".github/workflows/deploy.yml")).unwrap(),
    )
    .unwrap();
    let job = &workflow["jobs"]["integration"];
    let needs = job["needs"].as_sequence().unwrap();
    assert!(needs.iter().any(|item| item.as_str() == Some("build")));
    assert!(needs
        .iter()
        .any(|item| item.as_str() == Some("release-version")));
    assert!(workflow["jobs"]["release-tag"]["needs"]
        .as_sequence()
        .unwrap()
        .iter()
        .any(|item| item.as_str() == Some("integration")));
    let quickstart: Value = serde_yaml::from_str(
        &fs::read_to_string(root().join(".github/workflows/quickstart.yml")).unwrap(),
    )
    .unwrap();
    let canonical_dx = quickstart["jobs"]
        .as_mapping()
        .unwrap()
        .values()
        .flat_map(|job| job["steps"].as_sequence().unwrap())
        .find(|step| step["name"] == "Install pinned Dioxus CLI")
        .unwrap();
    let dx = job["steps"]
        .as_sequence()
        .unwrap()
        .iter()
        .find(|step| step["name"] == "Install pinned Dioxus CLI")
        .unwrap();
    assert_eq!(
        dx["run"], canonical_dx["run"],
        "reuse the verified pinned binary installer without another compiler graph"
    );
    let downloads = job["steps"]
        .as_sequence()
        .unwrap()
        .iter()
        .filter(|step| {
            step["uses"]
                .as_str()
                .is_some_and(|action| action.starts_with("actions/download-artifact@"))
        })
        .map(|step| {
            assert!(step["with"].get("pattern").is_none());
            assert_eq!(step["with"]["path"], "/tmp/ci-images");
            step["with"]["name"].as_str().unwrap()
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        downloads,
        std::collections::BTreeSet::from(["ci-image-navigator-web", "ci-image-neon-server"])
    );
    let scripts = job["steps"]
        .as_sequence()
        .unwrap()
        .iter()
        .filter_map(|step| step["run"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(scripts.contains("bash .github/scripts/native-browser-integration.sh"));
    for retired in ["postgres-bootstrap", "service/surreal", "restate-operator"] {
        assert!(
            !scripts.contains(retired),
            "native gate contains retired orchestration: {retired}"
        );
    }
}

#[test]
fn native_browser_checks_isolation_and_executes_the_owned_image_after_host_drain() {
    let helper =
        fs::read_to_string(root().join(".github/scripts/native-browser-integration.sh")).unwrap();
    for boundary in [
        "hudson quickstart",
        "hudson dev e2e",
        "hudson ops postgres ready --json",
        "source .devx/env",
        "hudson dev browser-e2e --base-url",
        "NAVIGATOR_FORBID_SURREAL=1",
        "NAVIGATOR_TEST_POSTGRES_PORT=",
        "k8s/postgres/kustomization.yaml",
        "skipping browser test:",
        "worktree-env down --path",
    ] {
        assert!(
            helper.contains(boundary),
            "missing lifecycle/admission boundary: {boundary}"
        );
    }
    let source_guard = helper.find("export NAVIGATOR_FORBID_SURREAL=1").unwrap();
    assert!(
        source_guard
            < helper
                .find("setsid target/debug/hudson quickstart")
                .unwrap(),
        "Source guard must cover the entire native gate"
    );
    assert!(helper.contains("--env CI=1 --env NAVIGATOR_FORBID_SURREAL=1"));
    let browsers = helper.find("hudson dev browser-e2e --base-url").unwrap();
    let stopped = helper.find("\nstop_host\n").unwrap();
    let image = helper
        .find("docker run -d --name \"$image_container\"")
        .unwrap();
    assert!(
        stopped < image && image < browsers,
        "browser suites must exercise the exact image after host drain"
    );
    assert!(helper[image..].contains("hudson ops assets verify"));
    assert!(helper.contains("--mount \"type=bind,source=$private/ca.crt"));
    assert!(
        !helper.contains("source=$root,target="),
        "whole checkout must not enter image"
    );
    for boundary in [
        "NAV_BROWSER_IMAGE_CONTAINER=",
        "NAV_BROWSER_IMAGE_ID=",
        "NAV_BROWSER_IMAGE_REPO_ROOT=/tmp/navigator-image-repos",
        "com.neonlaw.navigator.browser-fixture=git-runtime",
        "--env NAVIGATOR_GIT_REPO_ROOT=/tmp/navigator-image-repos",
        "--env NAVIGATOR_SAMPLE_PROJECTS_DIR=/native/sample-projects",
        "-d -o 65532 -g 65532 -m 700 /tmp/navigator-image-repos",
        "docker volume rm \"$repo_volume\"",
    ] {
        assert!(
            helper.contains(boundary),
            "missing owned image fixture boundary: {boundary}"
        );
    }
    assert!(!helper.contains("--group-add"));
    assert!(!helper.contains("source=$NAVIGATOR_GIT_REPO_ROOT"));
    assert!(helper.contains("trap cleanup EXIT"));
    assert!(helper.contains("if [ \"$result\" -eq 0 ]; then result=1; fi"));
}

#[test]
fn every_uploaded_native_diagnostic_log_has_a_sanitized_producer() {
    let workflow: Value = serde_yaml::from_str(
        &fs::read_to_string(root().join(".github/workflows/deploy.yml")).unwrap(),
    )
    .unwrap();
    let upload = workflow["jobs"]["integration"]["steps"]
        .as_sequence()
        .unwrap()
        .iter()
        .find(|step| step["with"]["name"].as_str() == Some("e2e-diagnostics"))
        .unwrap();
    let helper =
        fs::read_to_string(root().join(".github/scripts/native-browser-integration.sh")).unwrap();
    let paths = upload["with"]["path"].as_str().unwrap();
    assert!(paths.contains("target/nextest/ci/junit.xml"));
    assert!(paths.contains("/tmp/navigator-screenshots/"));
    let logs: Vec<_> = paths
        .lines()
        .filter(|line| {
            std::path::Path::new(line.trim())
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("log"))
        })
        .collect();
    assert!(
        !logs.is_empty(),
        "native diagnostics must name actual log producers"
    );
    for log in logs {
        let name = std::path::Path::new(log.trim())
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            helper.contains(&format!("$private/{name}")),
            "no producer for {name}"
        );
    }
    assert!(helper.contains("text.replace(value, '[REDACTED]')"));
    assert!(helper.contains("for path in source.glob('*.log')"));
    assert!(!paths.contains(".devx/env"));
}

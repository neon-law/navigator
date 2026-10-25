//! Guard where anonymous callers read releases from.
//!
//! `neon-law/navigator` is public, and `release-cli-publish` attaches every
//! CLI and LSP archive, checksum, `install.sh`, `LICENSE`, and `NOTICE` to its
//! own Release. `brew install`, `navigator lsp`, `/navigator`'s download links,
//! a Project repository's gate, and the standalone install script all read
//! that Release directly, so `deploy.yml` mirrors nothing onto the Homebrew
//! tap any more.
//!
//! This is the same invisible-breakage shape `cli/tests/homebrew_tap_dispatch.rs`
//! guards for the formula bump: the publisher and every reader live in
//! different places, so if the Release stops carrying a file nothing here goes
//! red while every anonymous download starts 404ing.

use std::fs;
use std::path::PathBuf;

const JOB: &str = "release-cli-publish";

fn repo_file(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn publish_run_script() -> String {
    let workflow: serde_yaml::Value =
        serde_yaml::from_str(&repo_file(".github/workflows/deploy.yml"))
            .expect("deploy.yml parses as YAML");
    workflow["jobs"][JOB]["steps"]
        .as_sequence()
        .unwrap_or_else(|| panic!("`{JOB}` declares steps"))
        .iter()
        .filter_map(|step| step.get("run")?.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every file a reader fetches must be attached to this repository's Release.
#[test]
fn the_release_carries_every_archive_checksum_and_supporting_file() {
    let run = publish_run_script();
    for required in [
        "navigator-${TAG}-linux.tar.gz",
        "navigator-${TAG}-macos.tar.gz",
        "navigator-lsp-${TAG}-linux.tar.gz",
        "navigator-lsp-${TAG}-macos.tar.gz",
        "navigator-${TAG}-linux.tar.gz.sha256",
        "navigator-${TAG}-macos.tar.gz.sha256",
        "navigator-${TAG}-install.sh",
        "LICENSE-APACHE-2",
        "NOTICE",
    ] {
        assert!(
            run.contains(required),
            "`{JOB}` must attach `{required}` to the Release, got:\n{run}"
        );
    }
}

/// The sparse checkout must name the files the standalone uploads read.
#[test]
fn license_and_notice_are_checked_out_for_the_release_to_upload() {
    let workflow: serde_yaml::Value =
        serde_yaml::from_str(&repo_file(".github/workflows/deploy.yml"))
            .expect("deploy.yml parses as YAML");
    let checkout = workflow["jobs"][JOB]["steps"]
        .as_sequence()
        .expect("steps")
        .iter()
        .find(|step| {
            step["uses"]
                .as_str()
                .unwrap_or_default()
                .starts_with("actions/checkout")
        })
        .expect("the job checks out the tree");
    let sparse = checkout["with"]["sparse-checkout"]
        .as_str()
        .expect("the checkout declares sparse-checkout paths");
    for required in ["LICENSE", "LICENSE-MIT", "LICENSE-APACHE-2", "NOTICE"] {
        assert!(
            sparse.lines().any(|line| line.trim() == required),
            "the sparse checkout must include `{required}`, got:\n{sparse}"
        );
    }
}

/// No step may copy the archives onto the tap: the tap holds the formula and
/// no binaries, and a second host is a second place to go stale.
#[test]
fn nothing_is_mirrored_onto_the_homebrew_tap() {
    let run = publish_run_script();
    assert!(
        !run.contains("PUBLIC_REPO") && !run.contains("homebrew-navigator"),
        "`{JOB}` must upload only to `${{GITHUB_REPOSITORY}}`, got:\n{run}"
    );
    assert_eq!(
        run.matches("gh release ").count(),
        run.matches("--repo \"${GITHUB_REPOSITORY}\"").count(),
        "every `gh release` command must pass `--repo \"${{GITHUB_REPOSITORY}}\"`, got:\n{run}"
    );
}

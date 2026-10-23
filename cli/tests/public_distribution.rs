//! Guard the mirror from a release to the public distribution host.
//!
//! ENG-931: `neon-law-source-code/navigator` will not stay public. Every
//! anonymous caller — `brew install`, `navigator lsp`, `/navigator`'s
//! download links, a Project repository's gate, and the standalone install
//! script — has to resolve the CLI and LSP archives from a host that outlives
//! that change, so `release-cli-publish` mirrors the same tag-exact
//! bytes it attaches to this repository's own Release onto
//! `neon-law-source-code/homebrew-navigator`'s Release as well.
//!
//! This is the same invisible-breakage shape `cli/tests/homebrew_tap_dispatch.rs`
//! guards for the formula bump: the mirror step and every public reader live in
//! different repositories that never reference each other, so if the mirror
//! stops running, nothing in this repository goes red while every anonymous
//! download starts 404ing.

use std::fs;
use std::path::PathBuf;

/// The job the mirror step rides — the same job that already creates this
/// repository's own Release, so the archives it mirrors are already on disk.
const JOB: &str = "release-cli-publish";

/// The public distribution host.
const PUBLIC_REPO: &str = "neon-law-source-code/homebrew-navigator";

fn deploy_workflow() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(".github")
        .join("workflows")
        .join("deploy.yml");
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn deploy_job(name: &str) -> serde_yaml::Value {
    let workflow: serde_yaml::Value =
        serde_yaml::from_str(&deploy_workflow()).expect("deploy.yml parses as YAML");
    workflow
        .get("jobs")
        .and_then(|jobs| jobs.get(name))
        .cloned()
        .unwrap_or_else(|| panic!("deploy.yml must define the `{name}` job"))
}

fn mirror_step() -> serde_yaml::Value {
    let job = deploy_job(JOB);
    job["steps"]
        .as_sequence()
        .expect("release-cli-publish declares steps")
        .iter()
        .find(|step| {
            step.get("name").and_then(serde_yaml::Value::as_str)
                == Some("mirror the archives to the public distribution host")
        })
        .cloned()
        .unwrap_or_else(|| panic!("`{JOB}` must carry the public-distribution mirror step"))
}

/// Every archive, checksum, and supporting file this repository's own Release
/// carries must also reach the public distribution host, tag-exact.
#[test]
fn the_mirror_step_publishes_every_archive_checksum_and_supporting_file() {
    let step = mirror_step();
    let run = step["run"]
        .as_str()
        .expect("the mirror step is a `run:` step");

    for required in [
        "dist/navigator-${TAG}-linux.tar.gz",
        "dist/navigator-${TAG}-macos.tar.gz",
        "dist/navigator-lsp-${TAG}-linux.tar.gz",
        "dist/navigator-lsp-${TAG}-macos.tar.gz",
        "dist/navigator-${TAG}-linux.tar.gz.sha256",
        "dist/navigator-${TAG}-macos.tar.gz.sha256",
        "\"navigator-${TAG}-install.sh\"",
    ] {
        assert!(
            run.contains(required),
            "the mirror step must publish `{required}` to the public distribution host, got:\n{run}"
        );
    }

    for standalone in [
        "upload \"${TAG}\" LICENSE ",
        "upload \"${TAG}\" LICENSE-MIT ",
        "upload \"${TAG}\" LICENSE-APACHE-2 ",
        "upload \"${TAG}\" NOTICE ",
    ] {
        assert!(
            run.contains(standalone),
            "the mirror step must publish `{standalone}` standalone, not only bundled inside an \
             archive, got:\n{run}"
        );
    }

    // Every upload in this step must address `${PUBLIC_REPO}`, never
    // `${GITHUB_REPOSITORY}` — that would just be the existing upload again,
    // to a repository that will not stay public. `gh` commands wrap onto a
    // continuation line, so this counts `--repo` flags against `gh` commands
    // rather than matching a single line.
    let gh_commands = run.matches("gh release ").count();
    let public_repo_flags = run.matches(r#"--repo "${PUBLIC_REPO}""#).count();
    assert!(
        gh_commands >= 11,
        "expected at least 11 gh commands, got {gh_commands}:\n{run}"
    );
    assert_eq!(
        gh_commands, public_repo_flags,
        "every `gh release` command in the mirror step must pass `--repo \"${{PUBLIC_REPO}}\"`, got:\n{run}"
    );
    assert!(
        !run.contains("${GITHUB_REPOSITORY}"),
        "the mirror step must not repeat the upload to this repository, got:\n{run}"
    );
}

/// The mirror authenticates with the tap-scoped token, never this run's own
/// `GITHUB_TOKEN`, which cannot reach another repository.
#[test]
fn the_mirror_authenticates_with_the_tap_scoped_token() {
    let step = mirror_step();
    assert_eq!(
        step["env"]["GH_TOKEN"].as_str(),
        Some("${{ secrets.HOMEBREW_TAP_TOKEN }}"),
        "the mirror step must authenticate with HOMEBREW_TAP_TOKEN"
    );
    assert_eq!(
        step["env"]["PUBLIC_REPO"].as_str(),
        Some(PUBLIC_REPO),
        "the mirror step must target {PUBLIC_REPO}"
    );
}

/// A missing token must fail the release, the same rule the formula dispatch
/// follows and for the same reason: a public host that silently stops
/// receiving archives serves a stale version to every anonymous caller while
/// nothing anywhere goes red.
#[test]
fn a_missing_token_fails_the_release_rather_than_skipping_the_mirror() {
    let step = mirror_step();
    let run = step["run"].as_str().expect("a run step");
    assert!(
        run.contains("HOMEBREW_TAP_TOKEN is unset"),
        "the mirror step must say what broke when the token is missing"
    );
    assert!(
        run.contains(r#"if [ -z "${GH_TOKEN}" ]"#),
        "the mirror step must check for the token before attempting any upload"
    );

    let job = deploy_job(JOB);
    assert!(
        job.get("continue-on-error").is_none(),
        "`{JOB}` must not swallow a mirror failure — a silent mirror is a stale public host"
    );
}

/// The mirror must run after this repository's own Release already carries
/// the archives — it re-uploads files the earlier steps in this job produced
/// or downloaded, not files of its own.
#[test]
fn the_mirror_runs_after_this_repositorys_own_release_is_created() {
    let job = deploy_job(JOB);
    let steps: Vec<String> = job["steps"]
        .as_sequence()
        .expect("the job declares steps")
        .iter()
        .filter_map(|step| step.get("name")?.as_str().map(str::to_owned))
        .collect();

    let create = steps
        .iter()
        .position(|name| name == "create the release if needed and attach the archive")
        .expect("the job creates this repository's own Release");
    let mirror = steps
        .iter()
        .position(|name| name == "mirror the archives to the public distribution host")
        .expect("the job mirrors to the public distribution host");
    assert!(
        mirror > create,
        "the mirror must follow the Release it re-uploads from, not precede it: {steps:?}"
    );
}

/// `LICENSE` and `NOTICE` have to be on disk for the mirror to upload them,
/// and this job's checkout is sparse — so both must be named in the
/// sparse-checkout list, not assumed present.
#[test]
fn license_and_notice_are_checked_out_for_the_mirror_to_upload() {
    let job = deploy_job(JOB);
    let checkout = job["steps"]
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
            "the sparse checkout must include `{required}` for the mirror step to upload, got:\n{sparse}"
        );
    }
}

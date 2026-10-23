//! Guard the two CLI release artifacts — the Linux archive CI and cloud
//! containers install, and the macOS archive humans download — and the install
//! commands advertised in the successful-release Slack message. Navigator is
//! built for Macs and containers in the cloud; no Windows archive ships.

use std::fs;
use std::path::PathBuf;

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

fn job_needs(name: &str) -> Vec<String> {
    let job = deploy_job(name);
    serde_yaml::from_value(job["needs"].clone())
        .unwrap_or_else(|error| panic!("`{name}` must declare a `needs` list: {error}"))
}

/// The composite gate every Project repository runs. It downloads the archive
/// `deploy.yml` publishes, and the two files never reference each other — so
/// the asset name is a contract only a test can hold.
fn validate_action() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(".github")
        .join("actions")
        .join("gate")
        .join("action.yml");
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// Navigator ships for macOS and Linux only, so no release job may build or
/// attach a Windows archive.
#[test]
fn no_release_job_builds_for_windows() {
    let workflow = deploy_workflow();

    for retired in [
        "windows-latest",
        "release-windows-cli-build",
        "windows-cli-lsp",
        "Compress-Archive",
        "-windows.zip",
    ] {
        assert!(
            !workflow.contains(retired),
            "deploy.yml must not carry the retired Windows release lane `{retired}`"
        );
    }
    for required in [
        "NAVIGATOR_RELEASE_TAG: ${{ needs.release-version.outputs.tag }}",
        "release-cli-publish:",
        "gh release create \"${TAG}\"",
    ] {
        assert!(
            workflow.contains(required),
            "deploy.yml must retain the CLI release contract `{required}`"
        );
    }
}

#[test]
fn every_cli_release_upload_uses_a_tag_exact_archive_name() {
    let workflow = deploy_workflow();
    let violating_lines: Vec<_> = workflow
        .lines()
        .map(str::trim_start)
        .filter(|line| {
            !line.starts_with('#')
                && line.contains("gh release upload")
                && !line.contains("-lsp-")
                && !line.contains("navigator-${TAG}-")
                // ENG-931: LICENSE, LICENSE-MIT, LICENSE-APACHE-2, and NOTICE travel
                // to the public distribution host standalone, under their own plain names —
                // they are supporting files, not tag-named CLI archives, and
                // the same names are correct on every release since each
                // Release has its own asset namespace.
                && !line.contains("upload \"${TAG}\" LICENSE ")
                && !line.contains("upload \"${TAG}\" LICENSE-MIT ")
                && !line.contains("upload \"${TAG}\" LICENSE-APACHE-2 ")
                && !line.contains("upload \"${TAG}\" NOTICE ")
                // The digest manifest is release metadata, not a CLI archive.
                && !line.contains("navigator-image-digests.json")
        })
        .collect();

    assert!(
        violating_lines.is_empty(),
        "every non-LSP CLI release upload must use a tag-exact archive name, got: \
         {violating_lines:?}"
    );
}

/// The two CLI archives compile in the SAME stage that publishes the images to
/// Artifact Registry: each waits on `integration` — the KIND e2e, interop, and
/// browser/accessibility suite — exactly as `publish-service` and
/// `publish-triggers` do. One gate decides whether a release produces artifacts
/// at all, and it is the e2e run.
///
/// That ordering is what makes a CLI archive mean the same thing an image tag
/// means. A release ships one version across four surfaces — the Artifact Registry images,
/// the two archives, `navigator --version`, and the Release page — and a
/// stranger reading that version has no way to tell which surfaces the e2e
/// suite actually stood behind. Compiling the archives before the gate made
/// "e2e-proven" true of the images and merely coincidental for the CLI. It also
/// occupied runners on every release whose integration job then went red —
/// building an archive for a Release that never gets cut.
///
/// `publishable` stays as the second half of the gate — the SAME condition the
/// publish jobs carry — so a run that publishes images always ships both
/// archives, and a `kind-ci/**` branch iteration stops at integration and
/// compiles none of them.
fn assert_builds_after_the_e2e_gate(job: &str) {
    let gate = deploy_job(job)["if"]
        .as_str()
        .unwrap_or_else(|| panic!("{job} must declare an `if:` gate"))
        .trim()
        .to_string();
    assert_eq!(
        gate, "needs.release-version.outputs.publishable == 'true'",
        "{job} must carry the same `publishable` gate the publish jobs do, so every run that \
         publishes images also ships a CLI, got: {gate:?}"
    );

    let needs = job_needs(job);
    for required in ["integration", "release-version"] {
        assert!(
            needs.iter().any(|entry| entry == required),
            "{job} must need `{required}` so the CLI is compiled in the same stage that pushes \
             the images to Artifact Registry — after the e2e run, never beside it. Got: {needs:?}"
        );
    }
}

/// Every run that reaches a CLI build is a tag release, so the archive is
/// built from the commit its tag names — `release-tag` creates it at this same
/// SHA before either archive is built. So the archive and the tag naming it
/// describe one commit rather than two that happened to be close.
///
/// The ref must be the run's SHA and nothing else. Shared by both CLI builds
/// because the requirement is identical: the archive's name carries the version,
/// so its bytes must come from the commit that version is cut at.
fn assert_builds_from_the_sha(job: &str) {
    let build = deploy_job(job);
    let steps = build["steps"]
        .as_sequence()
        .unwrap_or_else(|| panic!("{job} must declare steps"));
    let checkout = steps
        .iter()
        .find(|step| {
            step.get("uses")
                .and_then(serde_yaml::Value::as_str)
                .is_some_and(|uses| uses.starts_with("actions/checkout@"))
        })
        .unwrap_or_else(|| panic!("{job} must check the tree out"));
    let git_ref = checkout["with"]["ref"]
        .as_str()
        .expect("the checkout must pin a ref");

    assert!(
        !git_ref.contains('\n'),
        "the ref expression must be ONE line, got: {git_ref:?}"
    );
    assert_eq!(
        git_ref.trim(),
        "${{ github.sha }}",
        "{job} must build the run's own SHA — on a tag push that IS the tagged commit, and the \
         Release these archives attach to hangs off that same tag, so checking out anything else \
         would let an archive named for a version be compiled from a different commit"
    );
}

/// A GitHub Release hangs off an immutable Git tag, and `publishable` is true
/// only for a validated tag ref, so that one output is the whole gate. The
/// trigger-shaped clauses this carried existed to stop a clock- or
/// dispatch-driven run claiming a Release no tag stood behind; neither trigger
/// exists now, and naming a retired one here would be a gate on a condition that
/// can never be true.
#[test]
fn only_tagged_releases_attach_the_archive_to_a_github_release() {
    let gate = deploy_job("release-cli-publish")["if"]
        .as_str()
        .expect("release-cli-publish must declare an `if:` gate")
        .to_string();

    assert!(
        gate.contains("needs.release-version.outputs.publishable == 'true'"),
        "attaching to a GitHub Release must stay gated on a publishable run, got: {gate:?}"
    );
    for retired in ["github.event_name == 'schedule'", "workflow_dispatch"] {
        assert!(
            !gate.contains(retired),
            "the gate must not name the retired trigger `{retired}` — a release is a tag push"
        );
    }
}

/// #navigator's install message offers a download per platform, and keeps the
/// build-from-source path for the one Mac no archive covers.
///
/// The source build is the Intel fallback: `macos-latest` is arm64, so that is
/// the macOS archive that ships. Both halves are asserted: dropping the download
/// sends every Mac operator to a 20-minute compile, and dropping the fallback
/// leaves an Intel Mac with an instruction that produces a binary it cannot
/// execute.
#[test]
fn the_slack_message_offers_a_download_for_every_published_archive() {
    let workflow = deploy_workflow();

    for required in [
        "navigator-${TAG}-macos.tar.gz",
        "navigator-${TAG}-linux.tar.gz",
    ] {
        assert!(
            workflow.contains(required),
            "#navigator's install instructions must name the published archive `{required}`"
        );
    }

    assert!(
        workflow.contains("Intel macOS and arm64 Linux have no prebuilt Navigator archive or Homebrew install path."),
        "the message must identify platforms without a published binary"
    );
    assert!(workflow.contains("brew install neon-law-source-code/navigator/navigator"));
    assert!(!workflow.contains("git clone --depth 1 --branch"));
}

/// The macOS archive, and the 404 it closes.
///
/// `.github/actions/gate` has always mapped a macOS runner to
/// `platform=macos` and downloaded `navigator-<tag>-macos.tar.gz`. Nothing
/// built one, so the notation gate failed on any Project repository that ran
/// it on a macOS runner — the same breakage the Linux job's comment describes,
/// one platform over, and invisible from this repository because the failure
/// lands in the consumer's CI.
#[test]
fn releases_build_and_attach_a_macos_cli_archive() {
    let workflow = deploy_workflow();

    for required in [
        "release-cli-build-macos:",
        "runs-on: macos-latest",
        "install -m 0755 target/release/navigator dist/navigator-macos/navigator",
        "install -m 0644 LICENSE dist/navigator-macos/LICENSE",
        "-C dist/navigator-macos navigator LICENSE",
        "name: navigator-macos-cli",
        "gh release upload \"${TAG}\" dist/navigator-${TAG}-macos.tar.gz",
    ] {
        assert!(
            workflow.contains(required),
            "deploy.yml must retain the macOS CLI release contract `{required}`"
        );
    }
}

/// ENG-115: when the Apple secrets are present, the macOS build must sign,
/// package, notarize, and staple, in that order — a bare Mach-O cannot be
/// stapled, so the `.pkg` has to exist before `notarytool`/`stapler` touch it.
#[test]
fn releases_sign_notarize_and_staple_the_macos_cli_when_apple_secrets_are_present() {
    let workflow = deploy_workflow();

    for required in [
        "id: apple-signing",
        "security import",
        "codesign --force --options runtime --timestamp --sign",
        "pkgbuild --root dist/navigator-pkgroot",
        "xcrun notarytool submit",
        "xcrun stapler staple",
        "name: navigator-macos-pkg",
    ] {
        assert!(
            workflow.contains(required),
            "deploy.yml must sign, notarize, and staple the macOS release: missing `{required}`"
        );
    }

    let codesign_at = workflow
        .find("codesign --force --options runtime --timestamp --sign")
        .expect("codesign step present");
    let package_at = workflow
        .find("name: package the macOS CLI")
        .expect("package step present");
    let pkgbuild_at = workflow
        .find("pkgbuild --root dist/navigator-pkgroot")
        .expect("pkgbuild step present");
    let notarize_at = workflow
        .find("xcrun notarytool submit")
        .expect("notarize step present");
    let staple_at = workflow
        .find("xcrun stapler staple")
        .expect("staple step present");
    assert!(
        codesign_at < package_at,
        "the binary must be signed before it is copied into the tarball"
    );
    assert!(
        pkgbuild_at < notarize_at && notarize_at < staple_at,
        "a bare Mach-O cannot be stapled: build, then notarize, then staple, in that order"
    );
}

/// Find a step by its `name:` in a job's step list, parsed structurally
/// rather than by text position — the macOS job repeats several step names
/// (`build the tagged Navigator CLI and LSP`) across three jobs, so only a
/// same-job, same-step lookup is trustworthy.
fn job_step<'a>(job: &'a serde_yaml::Value, name: &str) -> Option<&'a serde_yaml::Value> {
    job.get("steps")?.as_sequence()?.iter().find(|step| {
        step.get("name")
            .and_then(|n| n.as_str())
            .is_some_and(|n| n == name)
    })
}

/// ENG-115: an operator who has not enrolled in the Apple Developer Program
/// yet must still get a complete release — an unsigned tarball, no `.pkg`,
/// and no failed job.
#[test]
fn macos_signing_skips_cleanly_when_apple_secrets_are_absent() {
    let workflow = deploy_workflow();
    assert!(
        workflow.contains("Apple Developer ID signing secrets are absent"),
        "deploy.yml must explain the skip when secrets are absent"
    );

    let job = deploy_job("release-cli-build-macos");
    let signing_steps = [
        "import the Developer ID certificate",
        "codesign the macOS CLI binary",
        "build the macOS installer package",
        "notarize the macOS installer package",
        "staple the notarization ticket",
        "upload the macOS installer package",
    ];
    for name in signing_steps {
        let step = job_step(&job, name).unwrap_or_else(|| panic!("missing step `{name}`"));
        assert_eq!(
            step.get("if").and_then(|v| v.as_str()),
            Some("steps.apple-signing.outputs.present == 'true'"),
            "`{name}` must be gated on the Apple signing secrets"
        );
    }

    // The unsigned-archive steps carry no `apple-signing` guard, so a missing
    // enrollment cannot break the tarball every Project repository downloads.
    for name in ["package the macOS CLI", "upload the macOS CLI archive"] {
        let step = job_step(&job, name).unwrap_or_else(|| panic!("missing step `{name}`"));
        assert!(
            step.get("if").is_none(),
            "`{name}` must not be gated on the Apple signing secrets"
        );
    }

    // The publish job must tolerate the artifact's absence rather than fail.
    let publish_job = deploy_job("release-cli-publish");
    let download_pkg = job_step(&publish_job, "download the macOS installer package")
        .expect("missing step `download the macOS installer package`");
    assert_eq!(
        download_pkg
            .get("continue-on-error")
            .and_then(serde_yaml::Value::as_bool),
        Some(true),
        "downloading the optional macOS installer package must not fail the publish job"
    );
    assert!(
        deploy_workflow().contains("if [ -f \"dist/navigator-${TAG}-macos.pkg\" ]; then"),
        "the release upload must only attach the `.pkg` when the signing job produced one"
    );
}

/// The same two-file contract the Linux archive is held to, for the platform
/// whose absence was the reason to write this test.
#[test]
fn the_macos_archive_name_matches_what_the_gate_action_downloads() {
    assert!(
        validate_action().contains("macOS)  platform=macos"),
        "the validate action must still map a macOS runner to the `macos` platform"
    );
    assert!(
        deploy_workflow().contains("dist/navigator-${TAG}-macos.tar.gz"),
        "deploy.yml must build the exact asset name the validate action downloads"
    );
}

/// Same rule as the Linux build: gated on `publishable`, and queued behind
/// the e2e run.
#[test]
fn every_published_release_builds_the_macos_cli() {
    assert_builds_after_the_e2e_gate("release-cli-build-macos");
}

#[test]
fn the_macos_build_checks_out_the_commit_it_claims() {
    assert_builds_from_the_sha("release-cli-build-macos");
}

/// A build that fails must page, and only the jobs `notify-failure` lists can.
///
/// The list is hand-maintained and the CLI builds are the easiest rows to
/// forget: they are peers of the publishes rather than dependencies of them, so
/// a green publish reads like a green release right up until the Release carries
/// one archive instead of two.
#[test]
fn a_failed_cli_build_pages_engineering() {
    let needs = job_needs("notify-failure");
    for required in ["release-cli-build-linux", "release-cli-build-macos"] {
        assert!(
            needs.iter().any(|entry| entry == required),
            "notify-failure cannot report a failure in `{required}` unless it needs it"
        );
    }
}

/// The Linux archive is the one CI and cloud containers actually consume:
/// `.github/actions/gate` asks for `navigator-<tag>-linux.tar.gz` on every
/// Linux runner.
#[test]
fn releases_build_and_attach_a_linux_cli_archive() {
    let workflow = deploy_workflow();

    for required in [
        "release-cli-build-linux:",
        "runs-on: blacksmith-4vcpu-ubuntu-2404",
        "install -m 0755 target/release/navigator dist/navigator-linux/navigator",
        "install -m 0644 LICENSE dist/navigator-linux/LICENSE",
        "-C dist/navigator-linux navigator LICENSE",
        "name: navigator-linux-cli",
        "gh release upload \"${TAG}\" dist/navigator-${TAG}-linux.tar.gz",
    ] {
        assert!(
            workflow.contains(required),
            "deploy.yml must retain the Linux CLI release contract `{required}`"
        );
    }
}

/// The archive name is a contract between two files that never reference each
/// other. `.github/actions/gate` composes
/// `navigator-${VERSION}-${platform}.tar.gz` with `platform=linux`; `deploy.yml`
/// has to produce exactly that. Nothing else in the tree ties them together,
/// and the drift cost every consuming repository its `ci` job.
#[test]
fn the_linux_archive_name_matches_what_the_gate_action_downloads() {
    let action = validate_action();

    assert!(
        action.contains("navigator-${VERSION}-${platform}.tar.gz"),
        "the validate action must still compose its asset name from VERSION and platform"
    );
    assert!(
        action.contains("Linux)  platform=linux"),
        "the validate action must still map a Linux runner to the `linux` platform"
    );
    assert!(
        deploy_workflow().contains("dist/navigator-${TAG}-linux.tar.gz"),
        "deploy.yml must build the exact asset name the validate action downloads"
    );
}

/// Every CLI archive carries LICENSE, LICENSE-MIT, LICENSE-APACHE-2, and NOTICE.
/// Check every platform because packaging uses distinct shells and paths.
#[test]
fn every_cli_archive_carries_the_licence_and_the_notice() {
    let workflow = deploy_workflow();

    for (platform, staged, archived) in [
        (
            "Linux",
            [
                "install -m 0644 LICENSE dist/navigator-linux/LICENSE",
                "install -m 0644 LICENSE-MIT dist/navigator-linux/LICENSE-MIT",
                "install -m 0644 LICENSE-APACHE-2 dist/navigator-linux/LICENSE-APACHE-2",
                "install -m 0644 NOTICE dist/navigator-linux/NOTICE",
            ],
            "-C dist/navigator-linux navigator LICENSE LICENSE-MIT LICENSE-APACHE-2 NOTICE",
        ),
        (
            "macOS",
            [
                "install -m 0644 LICENSE dist/navigator-macos/LICENSE",
                "install -m 0644 LICENSE-MIT dist/navigator-macos/LICENSE-MIT",
                "install -m 0644 LICENSE-APACHE-2 dist/navigator-macos/LICENSE-APACHE-2",
                "install -m 0644 NOTICE dist/navigator-macos/NOTICE",
            ],
            "-C dist/navigator-macos navigator LICENSE LICENSE-MIT LICENSE-APACHE-2 NOTICE",
        ),
    ] {
        for step in staged {
            assert!(
                workflow.contains(step),
                "the {platform} archive must stage LICENSE and NOTICE; `{step}` is \
                 missing"
            );
        }
        assert!(
            workflow.contains(archived),
            "the {platform} archive must carry LICENSE and NOTICE into the archive \
             itself — staging them beside the binary and then packing only the \
             binary ships an executable with no terms"
        );
    }
}

/// Same rule as the macOS build: the `publishable` gate is what makes a
/// publishing release ship a CLI for CI to install, so a run cannot publish
/// images while quietly shipping none.
#[test]
fn every_published_release_builds_the_linux_cli() {
    assert_builds_after_the_e2e_gate("release-cli-build-linux");
}

/// The archives and the images are peers in stage 2, so neither is cut into a
/// Release by the other finishing. `release-cli-publish` is the first
/// job in the run that publishes anything a stranger can fetch, and the CLI it
/// attaches is only half a release without the images the same tag names — so it
/// waits on both publish jobs as well as the two builds.
#[test]
fn the_release_is_cut_only_after_the_images_publish() {
    let needs = job_needs("release-cli-publish");
    for required in ["publish-service", "publish-triggers"] {
        assert!(
            needs.iter().any(|entry| entry == required),
            "the Release must not exist before the images it accompanies, so the attach job \
             needs `{required}`"
        );
    }
}

/// One publish job attaches every archive. Several would each run
/// `gh release create` behind a check-then-act `if ! gh release view` guard and
/// race on the same tag — but only within the SAME repository. ENG-931 added a
/// second `gh release create` in this job that targets
/// `neon-law-source-code/homebrew-navigator` rather than `${GITHUB_REPOSITORY}`,
/// which is a distinct Release under a distinct check-then-act guard, not a
/// second racer on this one.
#[test]
fn one_publish_job_attaches_every_cli_archive() {
    let needs = job_needs("release-cli-publish");
    for required in ["release-cli-build-linux", "release-cli-build-macos"] {
        assert!(
            needs.iter().any(|entry| entry == required),
            "the publish job attaches every archive, so it needs `{required}`"
        );
    }

    // Count real invocations, not prose: this file's own comments discuss the
    // command, and an earlier version of this assertion counted them. Grouped
    // by the repository each targets, since a race is only possible between
    // two creates of the SAME Release.
    let workflow = deploy_workflow();
    let creates: Vec<&str> = workflow
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !trimmed.starts_with('#') && trimmed.starts_with("gh release create")
        })
        .collect();
    for (repo, label) in [
        ("${GITHUB_REPOSITORY}", "this repository"),
        ("${PUBLIC_REPO}", "the public distribution host"),
    ] {
        let matching = creates.iter().filter(|line| line.contains(repo)).count();
        assert_eq!(
            matching, 1,
            "exactly one job may create the Release on {label}, or two runs race on the same tag; got {creates:?}"
        );
    }
    assert_eq!(
        creates.len(),
        2,
        "expected exactly the two creates above and no third, got {creates:?}"
    );
}

/// The Linux archive is the one CI installs, so it is held to the same rule as
/// the macOS one: built from the tag it is named for.
#[test]
fn the_linux_build_checks_out_the_commit_it_claims() {
    assert_builds_from_the_sha("release-cli-build-linux");
}

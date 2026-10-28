//! Fixture tests over `.github/workflows/release-cut.yml`, the scheduled
//! version-only pull request ENG-931 asks for. These read the workflow YAML
//! the same way `cli/tests/deploy_workflow.rs` reads `deploy.yml` — the
//! contract lives in the file this repository ships, not in a description of
//! it, so a test that only re-typed the prose would drift the moment either
//! one changed alone.
//!
//! THIS WORKFLOW IS DISABLED BY DEFAULT. `vars.RELEASE_TRAIN_ENABLED` is not
//! set anywhere in this repository, so every assertion here is about the
//! SHAPE this pipeline will run in once an operator deliberately turns it on
//! — see `docs/release-automation.md`.

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root is cli/'s parent")
        .to_path_buf()
}

fn workflow_text() -> String {
    let path = repo_root().join(".github/workflows/release-cut.yml");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn workflow() -> serde_yaml::Value {
    serde_yaml::from_str(&workflow_text()).expect("release-cut.yml parses as YAML")
}

fn job(name: &str) -> serde_yaml::Value {
    let workflow = workflow();
    let job = &workflow["jobs"][name];
    assert!(
        !job.is_null(),
        "release-cut.yml must declare the `{name}` job"
    );
    job.clone()
}

fn job_text(name: &str) -> String {
    serde_yaml::to_string(&job(name)).expect("the job re-serialises")
}

fn triggers() -> serde_yaml::Mapping {
    let workflow = workflow();
    // YAML 1.1 keys the bare `on:` mapping under the boolean `true`.
    let triggers = workflow
        .get(serde_yaml::Value::Bool(true))
        .or_else(|| workflow.get("on"))
        .expect("release-cut.yml must declare a trigger block");
    triggers
        .as_mapping()
        .expect("the trigger block must be a mapping")
        .clone()
}

#[test]
fn the_only_trigger_is_a_schedule() {
    let triggers = triggers();
    assert!(
        triggers.contains_key(serde_yaml::Value::String("schedule".into())),
        "release-cut.yml must run on a schedule — this is the one workflow allowed to propose a \
         release train PR unattended"
    );
    for forbidden in ["push", "pull_request", "workflow_dispatch"] {
        assert!(
            !triggers.contains_key(serde_yaml::Value::String(forbidden.into())),
            "release-cut.yml must not carry a `{forbidden}` trigger — a dispatch or a push would \
             let a run propose a bump nobody scheduled, and the probe already skips a covered \
             date"
        );
    }
}

#[test]
fn the_cut_job_is_disabled_until_an_operator_enables_the_release_train() {
    let cut = job("cut");
    assert_eq!(
        cut["if"].as_str(),
        Some("vars.RELEASE_TRAIN_ENABLED == 'true'"),
        "the `cut` job must refuse to run until `vars.RELEASE_TRAIN_ENABLED` is explicitly `true` \
         — this repository sets no such variable, so the schedule above fires into a no-op"
    );
}

#[test]
fn the_merge_job_is_equally_disabled_and_needs_an_open_pr() {
    let merge = job("merge");
    let condition = merge["if"].as_str().expect("merge must declare an if:");
    assert!(
        condition.contains("vars.RELEASE_TRAIN_ENABLED == 'true'"),
        "the `merge` job must be gated on the same enable flag as `cut`, or disabling one leaves \
         the other reachable: {condition}"
    );
    assert!(
        condition.contains("needs.cut.outputs.pr-number != ''"),
        "`merge` must not run when `cut` opened nothing: {condition}"
    );
}

#[test]
fn a_second_scheduled_run_cannot_open_a_second_pr() {
    let workflow = workflow();
    assert_eq!(
        workflow["concurrency"]["group"].as_str(),
        Some("release-cut"),
        "release-cut.yml must serialize on a fixed concurrency group, so an overlapping run \
         queues rather than opening a competing PR"
    );
    assert_eq!(
        workflow["concurrency"]["cancel-in-progress"].as_bool(),
        Some(false),
        "cancelling an in-flight cut would abandon a half-written branch rather than skip \
         cleanly — the job's own `existing` check is what should decide to skip, not a cancelled \
         run"
    );

    let cut_text = job_text("cut");
    assert!(
        cut_text.contains("gh pr list --base main --search 'head:release-train/cut- is:open'"),
        "the `cut` job must itself check for an already-open release PR before writing a new \
         branch, so a run that fires while yesterday's PR is still open does nothing rather than \
         opening a duplicate"
    );
}

#[test]
fn a_covered_date_writes_nothing() {
    let cut_text = job_text("cut");
    assert!(
        cut_text.contains("ops cut-release --dry-run"),
        "the `cut` job must probe with `--dry-run` before writing anything, so an already-covered \
         UTC date is reported rather than acted on"
    );
    assert!(
        cut_text.contains("if cargo run --locked -p cli --quiet -- ops cut-release --dry-run"),
        "the probe's exit code, not its stdout alone, must gate whether the job proceeds to write"
    );
    assert!(
        cut_text.contains("*-*) echo \"::notice::"),
        "a covered date makes `ops cut-release` name tomorrow's `-rc.N`; the train opens daily \
         bumps only, so a prerelease candidate must be reported rather than written"
    );
}

#[test]
fn the_release_train_never_derives_a_version_from_the_clock_directly() {
    let workflow_text = workflow_text();
    assert!(
        !workflow_text.contains("date -u"),
        "release-cut.yml must not read the system clock to name a version — `ops cut-release` is \
         the one place that decision is made, against the published tags, not against `date`"
    );
}

/// THE WHOLE SCOPE CHECK: the merge job's grant is `contents: write` +
/// `pull_requests: write`, so the diff-shape refusal must run BEFORE the
/// merge call, on every path that reaches it, or the grant can merge
/// anything.
#[test]
fn the_merge_job_verifies_the_diff_shape_before_merging() {
    let merge = job("merge");
    let steps = merge["steps"].as_sequence().expect("merge declares steps");

    let refuse_index = steps
        .iter()
        .position(|step| {
            step["name"].as_str() == Some("refuse to merge anything but a release-shaped diff")
        })
        .expect("merge must declare the diff-shape refusal step");
    let merge_index = steps
        .iter()
        .position(|step| step["name"].as_str() == Some("wait for the required check, then merge"))
        .expect("merge must declare the step that actually merges");
    assert!(
        refuse_index < merge_index,
        "the diff-shape refusal must run before the merge call, not after"
    );

    let refuse_script = steps[refuse_index]["run"]
        .as_str()
        .expect("the refusal step runs a script");
    for allowed in ["Cargo.toml", "Cargo.lock", ".github/workflows/*"] {
        assert!(
            refuse_script.contains(allowed),
            "the allowed-path list must include `{allowed}`, the shape `ops cut-release` and \
             `ops release pins` actually write"
        );
    }
    assert!(
        refuse_script.contains("exit 1"),
        "an out-of-shape path must fail the job rather than merely warn"
    );
}

#[test]
fn the_release_app_token_grant_never_falls_back_to_the_workflow_token() {
    let merge_text = job_text("merge");
    // Same failure shape `cli/tests/automerge_identity.rs` guards for the
    // ordinary merge queue: a fallback to `GITHUB_TOKEN` creates no workflow
    // run on the push it produces, silently losing the release the merge was
    // supposed to start.
    for spelling in ["github.token", "secrets.GITHUB_TOKEN"] {
        assert!(
            !merge_text.contains(spelling),
            "the `merge` job must not reference `{spelling}` anywhere — a merge armed under the \
             run's own token creates no workflow run for the push it makes, silently stranding \
             the release"
        );
    }
}

#[test]
fn an_unconfigured_release_app_leaves_the_pr_for_a_human_rather_than_arming_a_weaker_identity() {
    let merge = job("merge");
    let steps = merge["steps"].as_sequence().expect("merge declares steps");

    let mint = steps
        .iter()
        .find(|step| step["id"].as_str() == Some("app-token"))
        .expect("merge must declare the app-token mint step");
    assert_eq!(
        mint["if"].as_str(),
        Some("vars.RELEASE_APP_ID != ''"),
        "the mint step must be conditional on the App actually being configured"
    );

    let report = steps
        .iter()
        .find(|step| step["name"].as_str() == Some("report an unconfigured release App"))
        .expect("merge must report when the release App is not configured");
    assert_eq!(
        report["if"].as_str(),
        Some("steps.app-token.outputs.token == '' && vars.RELEASE_APP_ID == ''"),
        "the report step must fire exactly when minting produced no token because the App id is \
         unset"
    );

    for step in steps {
        let name = step["name"].as_str().unwrap_or_default();
        if matches!(
            name,
            "refuse to merge anything but a release-shaped diff"
                | "wait for the required check, then merge"
        ) {
            assert_eq!(
                step["if"].as_str(),
                Some("steps.app-token.outputs.token"),
                "`{name}` must be gated on a real token, or an absent App still attempts a merge \
                 with nothing to authenticate it"
            );
        }
    }
}

#[test]
fn merge_never_writes_repository_contents_at_the_workflow_or_cut_job_level() {
    let workflow = workflow();
    assert_eq!(
        workflow["permissions"]["contents"].as_str(),
        Some("read"),
        "the workflow-level permission must stay `read` — the release App token, not the \
         workflow's own GITHUB_TOKEN, is what merges"
    );
    assert_eq!(
        job("cut")["permissions"]["contents"].as_str(),
        Some("read"),
        "the `cut` job pushes its branch with the release App's own git credential helper via \
         `gh`/`git push`, authenticated by the ambient `GH_TOKEN`; it must not additionally hold \
         `contents: write` on the workflow token"
    );
}

#[test]
fn no_job_requests_the_workflows_permission() {
    let text = workflow_text();
    assert!(
        !text.contains("workflows: write") && !text.contains("workflows:write"),
        "no job here may hold the `workflows` permission — the same boundary \
         `docs/gitops.md`'s auto-merge identity callout draws for \
         `navigator-merge-queue`, so a PR touching `.github/workflows/**` still cannot merge \
         itself through a widened grant"
    );
}

#[test]
fn the_pr_body_names_eng_931_and_links_to_no_client_content() {
    let cut_text = job_text("cut");
    assert!(
        cut_text.contains("ENG-931"),
        "the opened PR should name the tracking issue for a reviewer who finds it later"
    );
    assert!(
        !cut_text.to_lowercase().contains("client"),
        "the automated PR body must carry no client-identifying language — this repository is \
         public today and the App that opens it must never narrate more than the mechanical bump"
    );
}

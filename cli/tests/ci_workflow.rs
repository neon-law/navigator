use std::fs;
use std::path::PathBuf;

fn ci_workflow() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(".github")
        .join("workflows")
        .join("ci.yml");
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn project_gate_workflow() -> serde_yaml::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(".github")
        .join("workflows")
        .join("project-gate.yml");
    let source = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_yaml::from_str(&source).expect("project-gate.yml parses as YAML")
}

fn project_publish_workflow() -> serde_yaml::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(".github")
        .join("workflows")
        .join("project-publish.yml");
    let source = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_yaml::from_str(&source).expect("project-publish.yml parses as YAML")
}

/// Portal publishing is retired: neither reusable Project workflow sets up
/// Node, builds a portal, reads publisher configuration, or uploads a bundle.
/// `project-publish.yml` stays callable so every existing `cd.yml` keeps
/// passing, but it does nothing.
#[test]
fn project_workflows_build_and_publish_no_portal() {
    for workflow in [project_gate_workflow(), project_publish_workflow()] {
        let source = serde_yaml::to_string(&workflow).expect("serialize workflow for assertions");
        for retired in [
            "navigator project applications",
            "navigator project portal",
            "setup-node",
            "pnpm",
            "application-publish",
            "NAVIGATOR_APP_PUBLISHER",
            "dist_dir",
        ] {
            assert!(
                !source.contains(retired),
                "a reusable Project workflow still carries `{retired}`:\n{source}"
            );
        }
    }
    let publish = project_publish_workflow();
    let jobs = publish["jobs"].as_mapping().expect("project publish jobs");
    assert_eq!(jobs.len(), 1, "one no-op job keeps callers green");
    assert!(publish["on"]["workflow_call"].is_mapping());
}

/// Every job installs the CLI through the shared composite action
/// (self-referenced with a pinned tag, since a step's `uses` key takes no
/// expression) rather than an inline download block, and threads
/// `read-manifest`'s resolved tag into it via `with: version:`, which `uses`
/// cannot carry but a step's `with:` block can.
#[test]
fn the_project_gate_verify_job_installs_the_cli_through_the_composite_action() {
    let workflow = project_gate_workflow();
    let jobs = workflow["jobs"].as_mapping().expect("project gate jobs");
    let verify_steps = jobs[&serde_yaml::Value::String("verify".to_string())]["steps"]
        .as_sequence()
        .expect("verify steps");

    assert!(
        verify_steps.iter().any(|step| {
            step["run"]
                .as_str()
                .is_some_and(|run| run.contains("navigator project gate --ci"))
        }),
        "verify must run the installed CLI directly"
    );
    let install_step = verify_steps.iter().find(|step| {
        step["uses"]
            .as_str()
            .is_some_and(|uses| uses.contains("/.github/actions/navigator-install@"))
    });
    assert!(
        install_step.is_some(),
        "verify must install the CLI through the shared composite action"
    );
    assert_eq!(
        install_step.unwrap()["with"]["version"].as_str(),
        Some("${{ needs.read-manifest.outputs.version }}"),
        "the composite action must receive the tag read-manifest already resolved and validated"
    );
}

/// A step's `uses` key is one of the two workflow keys that accept no context
/// expression, so an interpolated ref is not resolved — it is looked up
/// verbatim and the run fails. Nothing else catches this: the gate is a
/// `workflow_call` workflow Navigator's own CI never invokes.
#[test]
fn no_workflow_or_action_interpolates_a_uses_reference() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(".github");
    let mut checked = 0;
    let mut pending = vec![root];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if !matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("yml" | "yaml")
            ) {
                continue;
            }
            let source = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            checked += 1;
            for (number, line) in source.lines().enumerate() {
                let trimmed = line.trim_start();
                let Some(reference) = trimmed
                    .strip_prefix("- uses:")
                    .or_else(|| trimmed.strip_prefix("uses:"))
                else {
                    continue;
                };
                assert!(
                    !reference.contains("${{"),
                    "{}:{} interpolates a `uses` reference, which GitHub does not resolve: {trimmed}",
                    path.display(),
                    number + 1
                );
            }
        }
    }
    assert!(
        checked > 0,
        "expected to inspect at least one workflow file"
    );
}

/// Navigator is built for Macs and containers in the cloud, so nothing in
/// `ci.yml` may classify or run a Windows check.
#[test]
fn ci_yml_runs_no_windows_check() {
    let source = ci_workflow();
    let workflow: serde_yaml::Value = serde_yaml::from_str(&source).expect("ci.yml parses as YAML");

    assert!(
        workflow["jobs"]["windows-cli-lsp"].is_null(),
        "ci.yml must not declare a Windows CLI+LSP job"
    );
    assert!(
        !source.contains("windows-latest"),
        "ci.yml must not run on a Windows runner"
    );
    assert!(
        workflow["jobs"]["changes"]["outputs"]["windows"].is_null(),
        "ci.yml's changes job must not classify a Windows scope"
    );
    assert!(
        !source.contains("windows=true") && !source.contains("windows=false"),
        "ci.yml must not compute a Windows scope output anywhere"
    );
}

/// A green instrumented workspace job now finishes at 100% disk (run
/// 38068387566: 142 GB used, 244 MB free). The image sweep is what leaves
/// room for one more crate; without it the runner dies mid-nextest.
#[test]
fn rust_job_reclaims_image_disk_before_installing_lld() {
    let source = ci_workflow();
    let sweep = source
        .find("jlumbroso/free-disk-space")
        .expect("the workspace rust job must reclaim image disk");
    let lld = source
        .find("name: install lld")
        .expect("the workspace rust job must install lld");
    assert!(
        sweep < lld,
        "large-packages purges llvm-* apt packages, so the sweep must run before install lld"
    );
    assert!(
        source.contains("swap-storage: false"),
        "the sweep must not touch swap"
    );
}

/// ENG-671 moved the version-format guard out of five repeated inline CLI
/// downloads and into one composite action, so `read-manifest`'s own guard is
/// the only one left in this file — the guard that used to need to match five
/// other copies now has nothing to drift from.
#[test]
fn read_manifest_is_the_only_remaining_version_guard() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(".github")
        .join("workflows")
        .join("project-gate.yml");
    let source = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));

    let guards: Vec<(usize, &str)> = source
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let (_, rest) = line.split_once("=~ ")?;
            let (pattern, _) = rest.split_once(" ]]")?;
            Some((index + 1, pattern))
        })
        .collect();

    assert_eq!(
        guards.len(),
        1,
        "expected only read-manifest's guard now that the five CLI downloads install \
         through the composite action instead"
    );

    // A release candidate is the shape a Project migrating to the two thin
    // callers has to pin, because the input contract changed in one.
    let (line, pattern) = guards[0];
    let matched = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(r#"[[ "$1" =~ {pattern} ]]"#))
        .arg("bash")
        .arg("26.9.15-rc.1")
        .status()
        .expect("run bash");
    assert!(
        matched.success(),
        "line {line} refuses a release candidate the manifest read admits"
    );
}

/// The five jobs that install the CLI all reference the same
/// `navigator-install` tag. A job left pinned to a different tag than its
/// siblings would install a different composite action's behavior mid-run,
/// and because the five references are hand-maintained (a step's `uses` key
/// takes no expression), nothing else would catch that drift.
#[test]
fn every_navigator_install_reference_pins_the_same_tag() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(".github")
        .join("workflows")
        .join("project-gate.yml");
    let source = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));

    let refs: Vec<&str> = source
        .lines()
        .filter_map(|line| {
            let (_, rest) =
                line.split_once("uses: neon-law/navigator/.github/actions/navigator-install@")?;
            Some(rest.trim())
        })
        .collect();

    assert_eq!(
        refs.len(),
        4,
        "expected all four CLI-installing jobs (verify, documents, seeds, ci) \
         to reference the composite action"
    );
    assert!(
        refs.iter().all(|tag| *tag == refs[0]),
        "every navigator-install reference must pin the same tag: {refs:?}"
    );
}

/// The push-to-`main` notice (ENG-957) rides the shared gate so every Project
/// repository gets it on its next version bump: a `notify` job that runs only
/// on a push to `main`, mints the same document-scoped CI session the
/// `documents` job uses, posts to the main-push door, and can never fail the
/// run — it is `continue-on-error` and the required `ci` job does not depend
/// on it. It sends no commit subject and no publish outcome.
#[test]
fn the_project_gate_notifies_the_project_channel_on_a_main_push_without_failing_the_run() {
    let workflow = project_gate_workflow();
    let jobs = workflow["jobs"].as_mapping().expect("project gate jobs");
    let notify = &jobs[&serde_yaml::Value::String("notify".to_string())];
    assert_eq!(notify["continue-on-error"].as_bool(), Some(true));
    let condition = notify["if"].as_str().expect("notify declares a condition");
    assert!(
        condition.contains("github.event_name == 'push'"),
        "{condition}"
    );
    assert!(
        condition.contains("github.ref == 'refs/heads/main'"),
        "{condition}"
    );
    assert_eq!(notify["permissions"]["id-token"].as_str(), Some("write"));

    let ci_needs: Vec<&str> = jobs[&serde_yaml::Value::String("ci".to_string())]["needs"]
        .as_sequence()
        .expect("ci needs")
        .iter()
        .filter_map(serde_yaml::Value::as_str)
        .collect();
    assert!(
        !ci_needs.contains(&"notify"),
        "the required ci job must not wait on the notice: {ci_needs:?}"
    );

    let steps = notify["steps"].as_sequence().expect("notify steps");
    let run = steps
        .iter()
        .filter_map(|step| step["run"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(run.contains("/auth/ci/document-token"), "{run}");
    assert!(run.contains("/events/main-push"), "{run}");
    assert!(
        !run.contains("subject"),
        "no commit subject leaves the runner: {run}"
    );
    assert!(
        !run.contains("publish"),
        "the gate does not guess the publish outcome: {run}"
    );
    assert!(
        !run.contains("navigator "),
        "the notice is one curl, not a CLI verb the pinned release may lack: {run}"
    );
}

/// The required `ci` job holds the tree to `Y024` itself (ENG-1139): it runs
/// the installed CLI's `--tracked-files` pass with no script or interpreter of
/// its own, before it gathers every other job's result.
#[test]
fn the_required_ci_job_runs_the_markdown_and_yaml_rule_through_the_cli() {
    let workflow = project_gate_workflow();
    let jobs = workflow["jobs"].as_mapping().expect("project gate jobs");
    let ci = &jobs[&serde_yaml::Value::String("ci".to_string())];
    assert_eq!(ci["if"].as_str(), Some("always()"));
    let steps = ci["steps"].as_sequence().expect("ci steps");

    let install = steps
        .iter()
        .find(|step| {
            step["uses"]
                .as_str()
                .is_some_and(|uses| uses.contains("/.github/actions/navigator-install@"))
        })
        .expect("ci installs the CLI through the shared composite action");
    assert_eq!(
        install["with"]["version"].as_str(),
        Some("${{ needs.read-manifest.outputs.version }}")
    );

    let runs: Vec<&str> = steps
        .iter()
        .filter_map(|step| step["run"].as_str())
        .collect();
    let rule = runs
        .iter()
        .position(|run| run.trim() == "navigator project gate --tracked-files")
        .expect("ci runs `navigator project gate --tracked-files`");
    let gather = runs
        .iter()
        .position(|run| run.contains("$VERIFY_RESULT"))
        .expect("ci still requires every job");
    // Job results reach the shell through `env:`, never interpolated into the
    // script (CodeQL `actions/code-injection`).
    let gather_step = steps
        .iter()
        .find(|step| {
            step["run"]
                .as_str()
                .is_some_and(|run| run.contains("$VERIFY_RESULT"))
        })
        .unwrap();
    assert_eq!(
        gather_step["env"]["VERIFY_RESULT"].as_str(),
        Some("${{ needs.verify.result }}")
    );
    assert!(
        rule < gather,
        "the rule names its paths before the gather step"
    );
    let source = runs.join("\n");
    for interpreter in ["python", "node", "ruby", "pnpm", "npx"] {
        assert!(
            !source.contains(interpreter),
            "{interpreter} in ci: {source}"
        );
    }
}

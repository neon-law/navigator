//! Fixture tests over `.github/workflows/release-deploy.yml`, the
//! staging-then-production rollout `deploy.yml` dispatches once every image
//! and archive for a tag is published. Same discipline as
//! `cli/tests/deploy_workflow.rs`: read the workflow YAML this repository
//! actually ships and assert its structure, never its prose. See
//! `docs/release-automation.md`.

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root is cli/'s parent")
        .to_path_buf()
}

fn workflow_text() -> String {
    let path = repo_root().join(".github/workflows/release-deploy.yml");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn workflow() -> serde_yaml::Value {
    serde_yaml::from_str(&workflow_text()).expect("release-deploy.yml parses as YAML")
}

fn job(name: &str) -> serde_yaml::Value {
    let workflow = workflow();
    let job = &workflow["jobs"][name];
    assert!(
        !job.is_null(),
        "release-deploy.yml must declare the `{name}` job"
    );
    job.clone()
}

fn job_text(name: &str) -> String {
    serde_yaml::to_string(&job(name)).expect("the job re-serialises")
}

fn job_needs(name: &str) -> Vec<String> {
    match &job(name)["needs"] {
        serde_yaml::Value::String(need) => vec![need.clone()],
        serde_yaml::Value::Sequence(needs) => needs
            .iter()
            .map(|need| need.as_str().unwrap_or_default().to_string())
            .collect(),
        other => panic!("`{name}` needs must be a string or list, got {other:?}"),
    }
}

fn job_if(name: &str) -> String {
    job(name)["if"].as_str().unwrap_or_default().to_string()
}

fn step(job_name: &str, id: &str) -> serde_yaml::Value {
    job(job_name)["steps"]
        .as_sequence()
        .expect("the job declares steps")
        .iter()
        .find(|step| step["id"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("`{job_name}` must declare the `{id}` step"))
        .clone()
}

fn triggers() -> serde_yaml::Mapping {
    let workflow = workflow();
    let triggers = workflow
        .get(serde_yaml::Value::Bool(true))
        .or_else(|| workflow.get("on"))
        .expect("release-deploy.yml must declare a trigger block");
    triggers
        .as_mapping()
        .expect("the trigger block must be a mapping")
        .clone()
}

// ---------------------------------------------------------------------
// Version selection: an explicit dispatch input, never the clock.
// ---------------------------------------------------------------------

#[test]
fn the_only_trigger_is_a_workflow_dispatch() {
    let triggers = triggers();
    let names: Vec<&str> = triggers.keys().filter_map(|key| key.as_str()).collect();
    assert_eq!(
        names,
        ["workflow_dispatch"],
        "release-deploy.yml starts only from a dispatch. `deploy.yml` creates the Release with \
         `github.token`, whose events start no workflow, and the Release exists before the images \
         finish — so a `release` trigger would never fire, and would be early if it did"
    );
}

#[test]
fn the_dispatch_requires_a_tag_and_offers_a_rollback_flag() {
    let triggers = triggers();
    let inputs = &triggers[&serde_yaml::Value::String("workflow_dispatch".into())]["inputs"];
    assert_eq!(
        inputs["tag"]["required"].as_bool(),
        Some(true),
        "the `tag` dispatch input must be required"
    );
    assert_eq!(
        inputs["image-digests"]["required"].as_bool(),
        Some(true),
        "the digest manifest is required for every release dispatch"
    );
    assert!(
        inputs["tag"]["default"].is_null(),
        "the `tag` dispatch input must carry no default"
    );
    assert_eq!(
        inputs["rollback"]["type"].as_str(),
        Some("boolean"),
        "a `rollback` boolean input must exist for the manual recovery path"
    );
}

#[test]
fn the_tag_is_never_derived_from_the_system_clock() {
    let text = workflow_text();
    for forbidden in ["date -u", "date '+%y", "$(date"] {
        assert!(
            !text.contains(forbidden),
            "release-deploy.yml must not derive a tag from `{forbidden}`"
        );
    }
}

#[test]
fn the_resolved_tag_is_verified_against_a_real_published_release() {
    let preflight_text = job_text("preflight");
    assert!(
        preflight_text.contains("releases/tags/"),
        "preflight must read the named release back from the API"
    );
    assert!(
        preflight_text.contains("target_commitish"),
        "the verification must read back the release's actual target commit"
    );
    assert!(
        preflight_text.contains("if [ -z \"${tag}\" ]"),
        "resolving the tag must fail loudly when the dispatch supplies none"
    );
}

// ---------------------------------------------------------------------
// The gate, and no premature cloud credential.
// ---------------------------------------------------------------------

#[test]
fn preflight_gates_on_the_configured_repository_and_the_main_ref() {
    let gate = step("preflight", "gate");
    let script = gate["run"].as_str().expect("the gate step runs a script");
    for required in [
        "${EXPECTED_REPOSITORY}",
        "${ACTUAL_REPOSITORY}",
        "refs/heads/main",
    ] {
        assert!(
            script.contains(required),
            "the gate step must test `{required}`: {script}"
        );
    }
    assert_eq!(
        gate["env"]["EXPECTED_REPOSITORY"].as_str(),
        Some("${{ vars.DEPLOY_EXPECTED_REPOSITORY }}"),
        "the expected repository must come from a repository variable"
    );
    assert_eq!(
        gate["env"]["ACTUAL_REPOSITORY"].as_str(),
        Some("${{ github.repository }}")
    );
    assert!(!workflow_text().contains("steps.visibility"));

    for job_name in [
        "dedup",
        "resolve-rollback",
        "ship-staging",
        "hold-production",
        "ship-production",
    ] {
        assert!(
            job_needs(job_name).contains(&"preflight".to_string()),
            "`{job_name}` must need `preflight`, or it could run without the gate"
        );
    }
}

#[test]
fn ship_jobs_send_full_output_to_private_transcript_sinks() {
    for (job_name, sink_secret, deployment_secret, account_secret) in [
        (
            "ship-staging",
            "STAGING_SHIP_LOG_SINK",
            "STAGING_DEPLOYMENT",
            "STAGING_SHIP_SERVICE_ACCOUNT",
        ),
        (
            "ship-production",
            "PRODUCTION_SHIP_LOG_SINK",
            "PRODUCTION_DEPLOYMENT",
            "PRODUCTION_SHIP_SERVICE_ACCOUNT",
        ),
    ] {
        let body = job(job_name);
        let serialized = job_text(job_name);
        assert!(serialized.contains("--quiet --log-sink"));
        assert!(serialized.contains(&format!("secrets.{sink_secret}")));
        assert!(serialized.contains(&format!("secrets.{deployment_secret}")));
        assert!(serialized.contains(&format!("secrets.{account_secret}")));
        assert!(!serialized.contains("navigator-release-ship@"));
        assert!(!serialized.contains("neon-law-stg"));
        assert!(!serialized.contains("neon-law-prod"));
        assert!(body["environment"].is_string());
    }
}

#[test]
fn deployment_workflow_has_no_private_visibility_gate_or_pull_request_target() {
    let text = workflow_text();
    assert!(!text.contains("repos/${GITHUB_REPOSITORY} --jq .private"));
    assert!(!text.contains("pull_request_target"));
}

/// The unattended on-switch is `deploy.yml`'s to read, at the dispatch. A
/// manual dispatch is the rehearsal and recovery path, so this workflow reads
/// no enable flag and no kill switch of its own.
#[test]
fn this_workflow_reads_no_enable_flag_or_kill_switch() {
    let text = workflow_text();
    for variable in ["vars.DEPLOY_AUTOMATION_ENABLED", "DEPLOY_KILL_SWITCH"] {
        assert!(
            !text.contains(variable),
            "release-deploy.yml must not read `{variable}`; `deploy.yml`'s `release-deploy` job \
             is the one place the automation flag is evaluated"
        );
    }
}

#[test]
fn the_ship_jobs_hold_exactly_the_scoped_deploy_permissions() {
    for job_name in ["ship-staging", "ship-production"] {
        let permissions = job(job_name)["permissions"]
            .as_mapping()
            .expect("ship jobs declare permissions")
            .clone();
        let granted: Vec<(String, String)> = permissions
            .iter()
            .map(|(key, value)| {
                (
                    key.as_str().unwrap_or_default().to_string(),
                    value.as_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        assert_eq!(
            granted,
            [
                ("contents".to_string(), "read".to_string()),
                ("id-token".to_string(), "write".to_string()),
                ("deployments".to_string(), "write".to_string()),
            ],
            "`{job_name}` reads the checkout and the Release, federates into GCP, and records a \
             GitHub Deployment — nothing more"
        );
    }
}

#[test]
fn the_jobs_that_read_deployments_are_granted_deployments_read() {
    for job_name in ["dedup", "resolve-rollback"] {
        assert_eq!(
            job(job_name)["permissions"]["deployments"].as_str(),
            Some("read"),
            "`{job_name}` reads the Deployments API, which a token without `deployments: read` \
             answers with 403"
        );
    }
}

#[test]
fn staging_and_production_use_distinct_environments_and_distinct_identities() {
    assert_eq!(
        job("ship-staging")["environment"].as_str(),
        Some("staging-deploy")
    );
    assert_eq!(
        job("ship-production")["environment"].as_str(),
        Some("production-deploy")
    );

    let staging_text = job_text("ship-staging");
    let production_text = job_text("ship-production");
    assert!(
        staging_text.contains("STAGING_SHIP_WIF_PROVIDER")
            && staging_text.contains("STAGING_SHIP_SERVICE_ACCOUNT")
            && !staging_text.contains("PRODUCTION_"),
        "staging must authenticate with staging-scoped secrets only"
    );
    assert!(
        production_text.contains("PRODUCTION_SHIP_WIF_PROVIDER")
            && production_text.contains("PRODUCTION_SHIP_SERVICE_ACCOUNT")
            && !production_text.contains("STAGING_"),
        "production must authenticate with production-scoped secrets only"
    );
}

#[test]
fn each_ship_refuses_to_authenticate_without_its_service_account() {
    for (job_name, secret) in [
        ("ship-staging", "STAGING_SHIP_SERVICE_ACCOUNT"),
        ("ship-production", "PRODUCTION_SHIP_SERVICE_ACCOUNT"),
    ] {
        let steps = job(job_name)["steps"].as_sequence().cloned().unwrap();
        let guard = steps
            .iter()
            .position(|step| {
                serde_yaml::to_string(step)
                    .is_ok_and(|text| text.contains(secret) && text.contains("exit 1"))
            })
            .unwrap_or_else(|| panic!("`{job_name}` must fail fast when `{secret}` is unset"));
        let auth = steps
            .iter()
            .position(|step| {
                step["uses"]
                    .as_str()
                    .is_some_and(|uses| uses.starts_with("google-github-actions/auth@"))
            })
            .expect("the ship authenticates with google-github-actions/auth");
        assert!(
            guard < auth,
            "`{job_name}` must check `{secret}` before authenticating: without a service \
             account the action runs as the bare federated principal, which cannot pull images"
        );
    }
}

#[test]
fn each_ship_refuses_to_run_quiet_without_a_log_sink() {
    for (job_name, secret) in [
        ("ship-staging", "STAGING_SHIP_LOG_SINK"),
        ("ship-production", "PRODUCTION_SHIP_LOG_SINK"),
    ] {
        let steps = job(job_name)["steps"].as_sequence().cloned().unwrap();
        let text = |step: &serde_yaml::Value| serde_yaml::to_string(step).unwrap();
        let guard = steps
            .iter()
            .position(|step| text(step).contains(secret) && text(step).contains("exit 1"))
            .unwrap_or_else(|| panic!("`{job_name}` must fail fast when `{secret}` is unset"));
        let ship = steps
            .iter()
            .position(|step| {
                step["run"]
                    .as_str()
                    .is_some_and(|run| run.contains("navigator ops ship "))
            })
            .expect("the ship runs `navigator ops ship`");
        assert!(
            guard < ship,
            "`{job_name}` must check `{secret}` before `ops ship`: `--quiet` requires a sink and \
             an empty one is rejected as `unsupported --log-sink`"
        );
    }
}

#[test]
fn ship_jobs_read_no_deployment_tree() {
    for job_name in ["ship-staging", "ship-production"] {
        let text = job_text(job_name);
        for local in [
            "deployments/${DEPLOYMENT}",
            "--deployments-dir",
            "config.toml",
        ] {
            assert!(
                !text.contains(local),
                "`{job_name}` must read the row's published configuration, not `{local}`: the \
                 private rows are not in this repository"
            );
        }
    }
}

// ---------------------------------------------------------------------
// Stage ordering: staging blocks production, unconditionally.
// ---------------------------------------------------------------------

#[test]
fn production_needs_staging_with_no_escape_hatch() {
    assert!(job_needs("ship-production").contains(&"ship-staging".to_string()));

    let condition = job_if("ship-production");
    for escape in ["always()", "success() ||", "failure() ||", "!cancelled()"] {
        assert!(
            !condition.contains(escape),
            "`ship-production`'s `if:` must not contain `{escape}` — any status function here \
             could let production run after a failed staging roll: {condition:?}"
        );
    }
}

#[test]
fn production_rolls_exactly_the_tag_staging_served() {
    assert_eq!(
        job("ship-staging")["outputs"]["tag"].as_str(),
        Some("${{ env.TAG }}"),
        "`ship-staging` must publish the tag it rolled"
    );
    assert_eq!(
        job("ship-production")["env"]["TAG"].as_str(),
        Some("${{ needs.ship-staging.outputs.tag }}"),
        "`ship-production` must roll the tag staging served — the rollback target included — \
         not re-read the dispatch input"
    );
}

#[test]
fn a_failed_staging_roll_pages_before_production_could_ever_start() {
    assert!(job_text("ship-staging").contains("page #navigator on a failed staging roll"));
}

// ---------------------------------------------------------------------
// Prereleases roll to staging only.
// ---------------------------------------------------------------------

#[test]
fn a_prerelease_is_held_at_staging_and_records_no_production_deployment() {
    assert_eq!(
        job("preflight")["outputs"]["prerelease"].as_str(),
        Some("${{ steps.verify.outputs.prerelease }}"),
        "preflight must publish the prerelease answer it read back"
    );

    let production = job_if("ship-production");
    assert_eq!(
        production, "needs.preflight.outputs.prerelease != 'true'",
        "`ship-production` must skip every prerelease"
    );

    let hold = job("hold-production");
    assert_eq!(
        job_if("hold-production"),
        "needs.preflight.outputs.prerelease == 'true'",
        "`hold-production` must run exactly when production skips for a prerelease"
    );
    assert!(
        job_needs("hold-production").contains(&"ship-staging".to_string()),
        "the hold is reported only after staging succeeded"
    );
    let hold_text = serde_yaml::to_string(&hold).expect("hold-production re-serialises");
    assert!(
        hold_text.contains("./.github/actions/slack-progress")
            && hold_text.contains("held at staging"),
        "`hold-production` must post that the tag was held at staging"
    );
    assert!(
        !hold_text.contains("deployments")
            && hold["permissions"]["deployments"].is_null()
            && hold["environment"].is_null(),
        "`hold-production` must record no production Deployment and reach no environment"
    );
}

/// Runs preflight's real `verify` step against a stub `gh` that answers with a
/// Release, so the classification is exercised rather than pattern-matched.
fn classify(tag: &str, release_prerelease: bool) -> String {
    let script = step("preflight", "verify")["run"]
        .as_str()
        .expect("the verify step runs a script")
        .to_string();
    let dir = tempfile::tempdir().expect("a scratch directory");
    let stub = dir.path().join("gh");
    std::fs::write(
        &stub,
        format!(
            "#!/usr/bin/env bash\necho '{{\"target_commitish\":\"main\",\"assets\":[],\"prerelease\":{release_prerelease}}}'\n"
        ),
    )
    .expect("write the gh stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
            .expect("mark the gh stub executable");
    }
    let output = dir.path().join("github_output");
    let path = format!(
        "{}:{}",
        dir.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let status = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env("PATH", path)
        .env("TAG", tag)
        .env("GITHUB_REPOSITORY", "owner/repo")
        .env("GITHUB_OUTPUT", &output)
        .status()
        .expect("run the verify step");
    assert!(status.success(), "the verify step fails for {tag}");
    let written = std::fs::read_to_string(&output).expect("read GITHUB_OUTPUT");
    written
        .lines()
        .find_map(|line| line.strip_prefix("prerelease="))
        .expect("the verify step writes `prerelease`")
        .to_string()
}

#[cfg(unix)]
#[test]
fn preflight_classifies_every_semver_prerelease_as_held() {
    if Command::new("jq").arg("--version").output().is_err() {
        eprintln!("jq is not installed; the verify step cannot run here");
        return;
    }
    assert_eq!(
        classify("26.9.30", false),
        "false",
        "a stable tag rolls to production"
    );
    assert_eq!(
        classify("26.9.30-rc.1", false),
        "true",
        "an -rc tag is held"
    );
    assert_eq!(
        classify("26.9.30-hotfix.2", false),
        "true",
        "a -hotfix tag is held"
    );
    assert_eq!(
        classify("26.9.30", true),
        "true",
        "a Release `deploy.yml` flagged as a prerelease is held whatever its spelling"
    );
}

// ---------------------------------------------------------------------
// Asset read-back, and no rebuild.
// ---------------------------------------------------------------------

#[test]
fn every_ship_job_promotes_the_published_archive_rather_than_compiling() {
    for job_name in ["ship-staging", "ship-production"] {
        let text = job_text(job_name);
        assert!(
            text.contains("${NAVIGATOR_IMAGE_REGISTRY}/navigator-cli")
                && text.contains("docker pull \"${cli_ref}@${cli_digest}\"")
                && text.contains("docker cp")
                && text.contains("navigator --version"),
            "{job_name} must install the published CLI image and confirm it reports that tag"
        );
        assert!(
            !text.contains("cargo build") && !text.contains("docker build"),
            "`{job_name}` must not build anything"
        );
    }
}

#[test]
fn ship_jobs_require_digest_manifest_and_install_cosign() {
    for job_name in ["ship-staging", "ship-production"] {
        let text = job_text(job_name);
        assert!(text.contains("cosign-installer"));
        assert!(text.contains("--image-digests /tmp/image-digests.json"));
        // The manifest is read from the dispatch input, never a job output:
        // GitHub drops an output holding a secret, and the registry is one.
        assert!(text.contains("IMAGE_DIGESTS: ${{ inputs['image-digests'] }}"));
        assert!(!text.contains("needs.preflight.outputs.image_digests"));
    }
    let preflight = job_text("preflight");
    assert!(preflight.contains("DISPATCH_IMAGE_DIGESTS"));
    assert!(preflight.contains("sha256:[0-9a-f]{64}"));
    assert!(!preflight.contains("image_digests="));
}

#[test]
fn digest_manifest_keys_carry_no_registry() {
    let deploy = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.github/workflows/deploy.yml"),
    )
    .expect("read deploy.yml");
    for line in deploy
        .lines()
        .filter(|l| l.contains("image-digest.json") && l.contains("jq -n"))
    {
        assert!(
            !line.contains("NAVIGATOR_IMAGE_REGISTRY") && !line.contains("IMAGE_REF"),
            "a digest manifest key must be a registry-free image name: {line}"
        );
    }
    for text in [job_text("ship-staging"), job_text("ship-production")] {
        assert!(text.contains(".[\"navigator-cli\"] // .[$ref]"));
    }
}

#[test]
fn both_ship_jobs_run_the_full_ship_without_writing_iam() {
    for job_name in ["ship-staging", "ship-production"] {
        let text = job_text(job_name);
        assert!(
            text.contains("navigator ops ship --deployment") && !text.contains("--image-only"),
            "`{job_name}` must run the full `ops ship`"
        );
        assert!(
            text.contains("--assert-signing-iam"),
            "`{job_name}` must verify the signing binding rather than write IAM, so its identity \
             needs no `setIamPolicy`"
        );
        assert!(
            text.contains(r#"--project-id "${GCP_PROJECT_ID}""#),
            "`{job_name}` must ship from the row's published configuration"
        );
        assert!(
            !text.contains("get-credentials"),
            "`{job_name}` must leave cluster credentials to `ops ship`, which writes them to its \
             own scratch kubeconfig"
        );
    }
}

// ---------------------------------------------------------------------
// Duplicate runs and recovery.
// ---------------------------------------------------------------------

#[test]
fn a_duplicate_run_for_an_already_deployed_tag_ships_nothing_further() {
    assert!(job_needs("dedup").contains(&"preflight".to_string()));
    assert!(job_text("dedup").contains("deployments?environment=production"));
    let staging_condition = job_if("ship-staging");
    assert!(
        staging_condition.contains("needs.dedup.outputs.already-deployed != 'true'"),
        "`ship-staging` must refuse to run again once `dedup` reports this tag already succeeded \
         in production: {staging_condition}"
    );
}

#[test]
fn a_rollback_dispatch_requires_a_recorded_successful_production_tag() {
    assert_eq!(
        job_if("resolve-rollback"),
        "needs.preflight.outputs.proceed == 'true'",
        "the job must never be skipped on an ordinary roll: a skipped ancestor fails \
         `ship-production`'s implicit `success()`"
    );
    assert_eq!(
        step("resolve-rollback", "lkg")["if"].as_str(),
        Some("inputs.rollback == true"),
        "only the lookup step is gated on the rollback input"
    );
    let text = job_text("resolve-rollback");
    assert!(text.contains("deployments?environment=production"));
    assert!(
        text.contains("no successful production deployment is recorded"),
        "a rollback target with no successful production record must fail loudly"
    );
    assert_eq!(
        job("ship-staging")["env"]["TAG"].as_str(),
        Some("${{ needs.resolve-rollback.outputs.tag || needs.preflight.outputs.tag }}")
    );
}

#[test]
fn every_ship_job_records_its_outcome_even_on_failure() {
    for job_name in ["ship-staging", "ship-production"] {
        let job = job(job_name);
        let steps = job["steps"].as_sequence().expect("job declares steps");
        let record = steps
            .iter()
            .find(|step| {
                step["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with("record the") && name.ends_with("outcome"))
            })
            .unwrap_or_else(|| panic!("`{job_name}` must record its deployment outcome"));
        assert!(
            record["if"]
                .as_str()
                .is_some_and(|condition| condition.starts_with("always()")),
            "`{job_name}` must record its outcome on every path, including failure"
        );
    }
}

#[test]
fn releases_are_serialized_across_the_whole_pipeline() {
    let workflow = workflow();
    assert_eq!(
        workflow["concurrency"]["group"].as_str(),
        Some("release-deploy")
    );
    assert_eq!(
        workflow["concurrency"]["cancel-in-progress"].as_bool(),
        Some(false)
    );
}

// ---------------------------------------------------------------------
// Slack narration.
// ---------------------------------------------------------------------

#[test]
fn every_health_check_posts_its_result() {
    for job_name in ["ship-staging", "ship-production"] {
        let steps = job(job_name)["steps"]
            .as_sequence()
            .expect("job declares steps")
            .clone();
        let checks: Vec<String> = steps
            .iter()
            .filter(|step| {
                step["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with("health check:"))
            })
            .map(|step| {
                step["id"]
                    .as_str()
                    .expect("a health check has an id")
                    .to_string()
            })
            .collect();
        assert_eq!(
            checks,
            ["brand-readiness", "restate-registration"],
            "`{job_name}` runs these health checks"
        );
        for check in checks {
            let post = steps
                .iter()
                .find(|step| {
                    step["uses"].as_str() == Some("./.github/actions/slack-progress")
                        && step["with"]["step"].as_str().is_some_and(|line| {
                            line.starts_with(&format!("health check {check}: "))
                        })
                })
                .unwrap_or_else(|| panic!("`{job_name}` must post the `{check}` result"));
            assert_eq!(
                post["if"].as_str(),
                Some(format!("always() && steps.{check}.outcome != 'skipped'").as_str()),
                "the `{check}` result must post on pass and on fail"
            );
            let line = post["with"]["step"].as_str().unwrap_or_default();
            assert!(
                line.contains("'pass'") && line.contains("'fail'"),
                "the `{check}` post must name pass or fail: {line}"
            );
            assert_eq!(post["with"]["tag"].as_str(), Some("${{ env.TAG }}"));
        }
    }
}

/// Run one ship job's `restate-registration` script against a row, with a stub
/// `navigator` on `PATH` that echoes its arguments, and return what it printed.
fn run_registration_check(
    job_name: &str,
    row: &str,
    supports_project_id: bool,
) -> std::process::Output {
    let script = step(job_name, "restate-registration")["run"]
        .as_str()
        .expect("the registration check runs a script")
        .to_string();
    let stub_dir = tempfile::tempdir().expect("stub dir");
    let stub = stub_dir.path().join("navigator");
    let help = if supports_project_id {
        "--project-id <PROJECT_ID>"
    } else {
        "--url <URL>"
    };
    std::fs::write(
        &stub,
        format!(
            "#!/usr/bin/env bash\ncase \"$*\" in *--help*) echo '{help}' ;; *) echo \"navigator $*\" ;; esac\n"
        ),
    )
    .expect("write stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub");
    }
    let path = format!(
        "{}:{}",
        stub_dir.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    Command::new("bash")
        .args(["-e", "-c", &script])
        .current_dir(stub_dir.path())
        .env("PATH", path)
        .env("DEPLOYMENT", row)
        .env("GCP_PROJECT_ID", "example-project")
        .env("TAG", "26.10.22")
        .env_remove("NAVIGATOR_WORKFLOWS_URL")
        .output()
        .expect("run the registration check under bash")
}

/// The standalone registration check runs outside `ops ship`. Without the
/// row's own worker URL it would fall back to the brand's
/// `workflows.<primary_domain>` — production — and staging's Restate env then
/// fails to sign against the production worker (`META0003`, `401`). It must
/// name the row and its project, so the CLI reads that row's worker URL.
#[test]
fn the_registration_check_targets_the_rows_own_worker() {
    for (job_name, row) in [
        ("ship-staging", "example-staging"),
        ("ship-production", "example-production"),
    ] {
        let output = run_registration_check(job_name, row, true);
        assert!(
            output.status.success(),
            "`{job_name}` registration check failed for `{row}`: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            format!(
                "navigator ops restate register --deployment {row} --project-id example-project"
            ),
            "`{job_name}` must register `{row}`'s own worker"
        );
    }
}

/// The CLI is the tag being rolled. A tag older than `restate register
/// --project-id` must not fall back to the brand default; it skips the
/// re-check and leaves the registration `ops ship` already did.
#[test]
fn an_older_cli_skips_the_registration_check_rather_than_register_the_brand_default() {
    let output = run_registration_check("ship-staging", "example-staging", false);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("navigator ops restate register"),
        "nothing may be registered by a CLI that cannot read the row's worker URL: {stdout}"
    );
    assert!(stdout.contains("::warning::"));
}

#[test]
fn production_announces_that_staging_passed_before_it_begins() {
    let first_post = job("ship-production")["steps"]
        .as_sequence()
        .expect("job declares steps")
        .iter()
        .find(|step| step["uses"].as_str() == Some("./.github/actions/slack-progress"))
        .cloned()
        .expect("ship-production posts progress");
    assert!(
        first_post["with"]["step"].as_str().is_some_and(
            |line| line.contains("staging passed") && line.contains("production begins")
        ),
        "the first production post must say staging passed and production begins"
    );
}

#[test]
fn the_final_report_never_fails_the_workflow_on_a_missing_webhook() {
    let notify_text = job_text("notify");
    assert!(notify_text.contains("SLACK_WEBHOOK_URL is unset"));
    assert!(notify_text.contains("exit 0"));
    assert!(
        job_needs("notify").contains(&"hold-production".to_string()),
        "the final report must know whether production was held"
    );
}

#[test]
fn the_cloud_provider_actions_are_pinned_to_a_commit_of_the_major() {
    let text = workflow_text();
    for action in [
        "google-github-actions/auth",
        "google-github-actions/setup-gcloud",
    ] {
        let pinned = text.lines().any(|line| {
            line.split_once(&format!("{action}@"))
                .is_some_and(|(_, rest)| {
                    let (sha, comment) = rest.split_once(" # ").unwrap_or((rest, ""));
                    sha.len() == 40
                        && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
                        && comment.trim() == "v3"
                })
        });
        assert!(
            pinned,
            "`{action}` must be pinned to a full commit SHA annotated `# v3`"
        );
    }
}

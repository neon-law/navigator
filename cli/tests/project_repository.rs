//! End-to-end tests for `navigator project gate` against a Project repository.
//!
//! One repository per Project code, holding Drafts under `drafts/` (or its
//! `templates/` alias) and application source under `apps/<app>/`. There is one gate
//! for both. A legacy root `portal/` remains valid during the source-layout
//! transition.
//!
//! Nothing generates this shell anymore — `navigator project repository
//! scaffold` and `sync-skills` are retired; a real repository builds it by
//! hand (see `docs/project-repositories.md#a-repositorys-fixed-shell`). These
//! tests build the same shell directly with [`write_repository_shell`] so the
//! gate itself stays covered end to end.

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::{prelude::PredicateBooleanExt, str};
use tempfile::TempDir;

fn navigator() -> Command {
    let mut command = Command::cargo_bin("navigator").unwrap();
    command.env_remove("GITHUB_REPOSITORY");
    command
}

/// The pin these fixtures carry.
const FIXTURE_PIN: &str = "26.8.23";

const CODEOWNERS: &str = "# CODEOWNERS\n\n* @shicholas\n";
const GITATTRIBUTES: &str = "* text=auto eol=lf\n";
/// The canonical root `.gitignore`; it ignores `documents/` outright.
const ROOT_GITIGNORE: &str = "# Document bytes staged for or pulled down by `navigator project sync`.\n# Each document's pointer is committed under blobs/; its bytes never are.\n/documents/\n\n# Legal files and handoff notes never enter a Project repository.\n*.pdf\n*.docx\n*.local\n\n# Call notes and memos for `navigator project communication create --body-file`.\n# The Communication's body lives in Navigator's store, never in Git.\nnotes/\n\n# Email bodies, drafts, and saved copies of sent mail. Matter facts, never Git.\nmail/\n\n# Per-machine agent and operating-system state.\n.claude/settings.local.json\n.claude/worktrees/\n.agents/settings.local.json\n.agents/worktrees/\n.DS_Store\n";
/// A pointer as `project sync` writes one, for the fixtures below.
const POINTER_YAML: &str = "kind: exhibit\nvisibility: internal\ncurrent_version:\n  version: 1\n  asset_id: 0199b9e4-14b7-7ad0-87a5-71ef24a46d40\n  created_at: 2026-09-05T12:00:00Z\n  sha256: 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n  size_bytes: 42\n";

/// The one `AGENTS.md` every Project repository carries — read from the same
/// source `cli::projects::repository::AGENT_CONTRACT_BASE` embeds, so the
/// fixture and the gate's own canonical copy can never drift apart.
const AGENT_CONTRACT: &str = include_str!("../src/projects/agent_contract.md");
const CLAUDE_SETTINGS: &str = include_str!("../src/projects/claude_settings.json");

/// Byte-exact copy of `AUTOMERGE_WORKFLOW_CONTENTS` in
/// `cli/src/projects/repository.rs` — duplicated rather than imported, the
/// same way the rest of this file's fixed strings do, since integration
/// tests cannot reach a `pub(crate)` item.
const AUTOMERGE_WORKFLOW_CONTENTS: &str = r#"name: automerge

on:
  pull_request:
    types: [opened, synchronize, reopened, ready_for_review]

permissions:
  contents: read

jobs:
  enable-automerge:
    if: github.event.pull_request.draft == false
    runs-on: ubuntu-latest
    steps:
      - name: Look for the merge-queue App credentials
        id: credentials
        env:
          APP_ID: ${{ secrets.AUTOMERGE_APP_ID }}
          APP_PRIVATE_KEY: ${{ secrets.AUTOMERGE_APP_PRIVATE_KEY }}
        shell: bash
        run: |
          set -euo pipefail
          if [ -n "${APP_ID}" ] && [ -n "${APP_PRIVATE_KEY}" ]; then
              echo "present=true" >> "${GITHUB_OUTPUT}"
          else
              echo "present=false" >> "${GITHUB_OUTPUT}"
          fi
      - name: Mint a merge-queue App token
        id: app-token
        if: steps.credentials.outputs.present == 'true'
        uses: actions/create-github-app-token@bcd2ba49218906704ab6c1aa796996da409d3eb1 # v3.2.0
        with:
          app-id: ${{ secrets.AUTOMERGE_APP_ID }}
          private-key: ${{ secrets.AUTOMERGE_APP_PRIVATE_KEY }}
      - name: Arm auto-merge
        env:
          GH_TOKEN: ${{ steps.app-token.outputs.token }}
          PR_URL: ${{ github.event.pull_request.html_url }}
        shell: bash
        run: |
          set -euo pipefail
          if [ -z "${GH_TOKEN:-}" ]; then
              echo "::notice::merge-queue App credentials absent — arming nothing, merge by hand"
              exit 0
          fi
          gh pr merge --squash --auto "${PR_URL}"
          verified="$(gh pr view "${PR_URL}" --json autoMergeRequest,state --jq '(.autoMergeRequest != null) or (.state == "MERGED")')"
          if [ "${verified}" != "true" ]; then
              echo "::error::GitHub did not arm auto-merge for ${PR_URL}" >&2
              exit 1
          fi
"#;

/// The synced skill catalog every Project repository carries, named for the
/// canonical copies under Navigator's own `.agents/skills/` (see
/// `SYNCED_SKILLS` in `cli/src/projects/repository.rs`).
const SYNCED_SKILL_NAMES: &[&str] = &[
    "council",
    "legal-council",
    "client-council",
    "human-readable",
    "stay-in-repo",
    "legal-writing",
    "case-downloader",
    "project-setup",
    "deadline",
    "browser-handoff",
    "case-timeline",
    "opposing-communication-review",
    "record-search",
    "working-notes",
    "pull-changes",
    "word-editing",
];

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()
        .expect("workspace root exists")
}

fn workflow(action_version: &str) -> String {
    format!(
        r"name: ci

on:
  pull_request:

permissions:
  contents: read
  id-token: write

jobs:
  ci:
    uses: neon-law/navigator/.github/workflows/project-gate.yml@{action_version}
"
    )
}

fn cd_workflow(action_version: &str) -> String {
    format!(
        r"name: cd

on:
  push:
    branches: [main]
  workflow_dispatch:

permissions:
  contents: read
  id-token: write

jobs:
  gate:
    uses: neon-law/navigator/.github/workflows/project-gate.yml@{action_version}
  publish:
    needs: gate
    uses: neon-law/navigator/.github/workflows/project-publish.yml@{action_version}
",
    )
}

fn placeholder_template() -> String {
    [
        "---\n",
        "kind: onboarding\n",
        "title: Onboarding letter\n",
        "respondent_type: entity\n",
        "jurisdiction: NV\n",
        "confidential: true\n",
        "questionnaire:\n",
        "  BEGIN:\n",
        "    _: jurisdiction__governing_law\n",
        "  jurisdiction__governing_law:\n",
        "    _: jurisdiction__arbitration_seat\n",
        "  jurisdiction__arbitration_seat:\n",
        "    _: END\n",
        "  END: {}\n",
        "clauses:\n",
        "  arbitration:\n",
        "    source: engagement_arbitration@1\n",
        "    inputs:\n",
        "      law: jurisdiction__governing_law\n",
        "      seat: jurisdiction__arbitration_seat\n",
        "workflow:\n",
        "  BEGIN:\n",
        "    intake_submitted: lawyer_review\n",
        "  lawyer_review:\n",
        "    approved: END\n",
        "    rejected: END\n",
        "  END: {}\n",
        "---\n",
        "\n",
        "{{> arbitration}}\n",
        "\n",
        "Replace this placeholder with the notation this Project actually uses.\n",
    ]
    .concat()
}

/// Write the fixed Project-repository shell directly, matching
/// `docs/project-repositories.md#a-repositorys-fixed-shell`. Does not touch
/// git — see [`scaffold`] for the git-initialized form every other fixture in
/// this file wants.
fn write_repository_shell(dir: &Path, project_code: &str) {
    fs::create_dir_all(dir.join(".github/workflows")).unwrap();
    fs::write(dir.join(".gitattributes"), GITATTRIBUTES).unwrap();
    fs::write(dir.join(".github/CODEOWNERS"), CODEOWNERS).unwrap();
    fs::write(
        dir.join(".github/workflows/automerge.yml"),
        AUTOMERGE_WORKFLOW_CONTENTS,
    )
    .unwrap();
    fs::write(
        dir.join("README.md"),
        format!("# {project_code}\n\nSource-only material for this Project.\n"),
    )
    .unwrap();
    fs::write(dir.join("AGENTS.md"), AGENT_CONTRACT).unwrap();
    fs::create_dir_all(dir.join(".claude")).unwrap();
    fs::write(dir.join(".claude/settings.json"), CLAUDE_SETTINGS).unwrap();
    fs::write(dir.join(".github/workflows/ci.yml"), workflow(FIXTURE_PIN)).unwrap();
    fs::write(
        dir.join(".github/workflows/cd.yml"),
        cd_workflow(FIXTURE_PIN),
    )
    .unwrap();
    fs::write(
        dir.join("navigator.yaml"),
        format!(
            "version: {FIXTURE_PIN}\nproject:\n  host: staging.neonlaw.com\n  name: {project_code}\n  lawyer_dri: lawyer@neonlaw.com\n  client_dri: client@example.com\n  private_slack_channel: https://slack.example.com/private\n  private_notion_page: https://notion.example.com/private\n"
        ),
    )
    .unwrap();
    fs::create_dir_all(dir.join("drafts")).unwrap();
    fs::write(
        dir.join("drafts/onboarding__letter.md"),
        placeholder_template(),
    )
    .unwrap();
    fs::write(dir.join(".gitignore"), ROOT_GITIGNORE).unwrap();
    for name in SYNCED_SKILL_NAMES {
        let source = workspace_root()
            .join(".agents/skills")
            .join(name)
            .join("SKILL.md");
        let contents = fs::read_to_string(&source)
            .unwrap_or_else(|e| panic!("read {}: {e}", source.display()));
        let dest = dir.join(".agents/skills").join(name).join("SKILL.md");
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::write(dest, contents).unwrap();
    }
}

fn scaffold(dir: &Path, project_code: &str) {
    write_repository_shell(dir, project_code);
    init_git_repository(dir);
}

#[test]
fn generated_and_repaired_agent_contract_has_critical_email_rule() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");

    let generated = fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
    assert_eq!(generated, AGENT_CONTRACT);
    assert!(generated.contains("## Critical: email is draft-only"));
    assert!(generated.contains("Every email must remain a draft"));
    assert!(generated.contains("never sends, replies to, or forwards email"));
    assert_eq!(
        fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap(),
        CLAUDE_SETTINGS
    );

    let without_email_rule = AGENT_CONTRACT.replace(
        "Every email must remain a draft.",
        "Emails are prepared for review.",
    );
    assert_ne!(without_email_rule, AGENT_CONTRACT);
    fs::write(dir.path().join("AGENTS.md"), without_email_rule).unwrap();
    gate(dir.path()).failure().code(1).stderr(str::contains(
        "canonical contract embedded in Navigator CLI",
    ));

    navigator()
        .args(["project", "repository", "sync-skills", "--dir"])
        .arg(dir.path())
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(dir.path().join("AGENTS.md")).unwrap(),
        AGENT_CONTRACT
    );
    assert_eq!(
        fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap(),
        CLAUDE_SETTINGS
    );
    gate(dir.path());
}

#[test]
fn generated_contract_requires_ready_prs_and_client_visibility_review() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");

    let generated = fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
    assert!(generated.contains("squash-merged"));
    assert!(generated.contains("deleted on merge"));
    assert!(generated.contains("Open pull requests ready for review by default"));
    assert!(generated.contains("visibility: internal"));
    assert!(!generated.contains("apps/portal/"));
    assert!(generated.contains("Deadline"));

    gate(dir.path());
}

fn init_git_repository(dir: &Path) {
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git init failed in {}", dir.display());
}

fn run_git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

fn project_gate_source() -> String {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.github/workflows/project-gate.yml");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn the_reusable_gate_requires_live_checks_and_seed_validation() {
    let source = project_gate_source();
    assert!(source.contains("navigator project gate --ci"));
    assert!(!source.contains("enable-automerge:"));
    assert!(source.contains("needs: [read-manifest, verify, documents, seeds]"));
}

/// The gate is one job, because it is one command: `verify` builds every
/// application and then runs the whole check over the tree, origin pass
/// included. Nothing else may run it, or the repository is gated twice and
/// the second run is the one nobody reads.
#[test]
fn the_reusable_gate_runs_the_gate_exactly_once() {
    let source = project_gate_source();
    let workflow: serde_yaml::Value =
        serde_yaml::from_str(&source).expect("project gate parses as YAML");
    let jobs = workflow["jobs"].as_mapping().expect("jobs");
    let running_the_gate: Vec<&str> = jobs
        .iter()
        .filter_map(|(name, job)| {
            let runs = job["steps"].as_sequence()?.iter().any(|step| {
                step["run"]
                    .as_str()
                    .is_some_and(|run| run.contains("navigator project gate --ci"))
            });
            runs.then(|| name.as_str()).flatten()
        })
        .collect();
    assert_eq!(
        running_the_gate,
        vec!["verify"],
        "the gate belongs to `verify` alone:\n{source}"
    );
    // `notify` posts the push-to-`main` notice after the three checks and
    // before the required `ci` roll-up, which does not depend on it.
    let names: Vec<&str> = jobs.keys().filter_map(|key| key.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "read-manifest",
            "verify",
            "documents",
            "seeds",
            "notify",
            "ci"
        ]
    );
}

/// The live row is checked on main and pull-request merge refs. That rule lives
/// in the CLI, which reads the ref and event itself, so the verify job carries
/// no ref branch of its own.
#[test]
fn the_project_gate_needs_no_branch_to_stay_offline_on_prs() {
    let workflow: serde_yaml::Value =
        serde_yaml::from_str(&project_gate_source()).expect("project gate parses as YAML");
    let verify = &workflow["jobs"]["verify"];
    assert_eq!(
        verify["permissions"]["id-token"].as_str(),
        Some("write"),
        "the gate mints the live-row session from this job"
    );
    let steps = verify["steps"].as_sequence().expect("verify steps");
    let gate = steps
        .iter()
        .find(|step| {
            step["run"]
                .as_str()
                .is_some_and(|run| run.contains("navigator project gate"))
        })
        .expect("verify gate step");
    let run = gate["run"].as_str().expect("gate script");
    assert_eq!(
        run.trim(),
        "navigator project gate --ci",
        "the gate takes no host and no ref branch:\n{run}"
    );
}

/// The documents job is one CLI invocation. It reads the host from
/// `navigator.yaml` and does not keep a second verb for the same check.
#[test]
fn the_documents_job_runs_project_gate_check() {
    let source = project_gate_source();
    let workflow: serde_yaml::Value =
        serde_yaml::from_str(&source).expect("project gate parses as YAML");
    let steps = workflow["jobs"]["documents"]["steps"]
        .as_sequence()
        .expect("documents steps");
    let run = steps
        .iter()
        .find_map(|step| step["run"].as_str())
        .expect("documents run");
    assert_eq!(run.trim(), "navigator project gate --check --ci");
    assert!(!source.contains("site document verify"));
    let action = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../.github/actions/document-verify/action.yml"),
    )
    .unwrap();
    assert!(action.contains("navigator project gate --check --ci"));
    assert!(!action.contains("site document verify"));
}

/// The `seeds` job reconciles `seeds/` on a push to `main`, performs a
/// server-enforced dry-run on a pull request, never overwrites, and no-ops
/// cleanly with no `seeds/` directory. Its result is part of the required
/// `ci` check, so a failed live validation cannot surface only after merge.
#[test]
fn the_reusable_gate_reconciles_seeds_on_push_to_main_only() {
    let source = project_gate_source();
    assert!(source.contains("  seeds:"));
    assert!(source.contains("no seeds — nothing to reconcile"));
    assert!(source.contains(
        r#"elif { [ "${EVENT_NAME}" = "push" ] || [ "${EVENT_NAME}" = "workflow_dispatch" ]; } && [ "${REF}" = "refs/heads/main" ]; then"#
    ));
    assert!(source.contains("navigator site import --dry-run --ci --host \"${HOST}\""));
    assert!(source.contains(r#"navigator site import --ci --host "${HOST}""#));
    assert!(source.contains("needs: read-manifest"));
    assert!(source.contains("needs: [read-manifest, verify, documents, seeds]"));
}

#[test]
fn the_seed_import_action_uses_the_repository_root() {
    let source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.github/actions/seed-import/action.yml"),
    )
    .unwrap();

    assert!(
        source.contains(r#"navigator site import --ci --host "${HOST}""#),
        "the seed-import action must use the CLI's root-only import form",
    );
    assert!(
        !source.contains("inputs.dir"),
        "the seed-import action must not pass the removed directory input",
    );
}

/// ENG-1139's synthetic pull requests: the `ci` job's `--tracked-files` pass
/// fails one adding `package.json`, `x.py`, or `tests/a.mjs` by naming the
/// path, and passes the Markdown-and-YAML shell. It reads no network, so it
/// needs no login.
#[test]
fn tracked_files_gate_names_each_non_markdown_or_yaml_path() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    navigator()
        .current_dir(dir.path())
        .args(["project", "gate", "--tracked-files"])
        .assert()
        .success()
        .stdout(str::contains("Y024: 0 finding(s)"));

    for relative in ["package.json", "x.py", "tests/a.mjs"] {
        let path = dir.path().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "synthetic\n").unwrap();
    }
    let assert = navigator()
        .current_dir(dir.path())
        .args(["project", "gate", "--tracked-files"])
        .assert()
        .failure()
        .code(1)
        .stdout(str::contains("Y024: 3 finding(s)"));
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    for relative in ["package.json", "x.py", "tests/a.mjs"] {
        assert!(
            stderr.contains(&format!("Y024: `{relative}` cannot be committed")),
            "{stderr}"
        );
    }

    // The full gate refuses the same paths.
    gate(dir.path())
        .failure()
        .stderr(str::contains("Y024: `tests/a.mjs` cannot be committed"));
}

fn gate(dir: &Path) -> assert_cmd::assert::Assert {
    navigator()
        .current_dir(dir)
        .args(["project", "gate"])
        .assert()
}

fn offline_gate_is_clean(assert: assert_cmd::assert::Assert) -> assert_cmd::assert::Assert {
    assert
        .failure()
        .code(1)
        .stdout(str::contains("0 error(s)"))
        .stderr(str::contains("live Project check is inconclusive"))
}

fn gate_as(dir: &Path, repository: &str) -> assert_cmd::assert::Assert {
    navigator()
        .current_dir(dir)
        .args(["project", "gate"])
        .env("GITHUB_REPOSITORY", format!("org/{repository}"))
        .assert()
}

#[test]
fn the_flat_manifest_shape_is_read_with_a_deprecation_warning() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    fs::write(
        dir.path().join("navigator.yaml"),
        "host: staging.neonlaw.com\nproject: example-project\n",
    )
    .unwrap();

    offline_gate_is_clean(gate(dir.path()))
        .stdout(str::contains("Y013"))
        .stdout(str::contains("1 warning(s)"));
}

#[test]
fn a_malformed_manifest_is_reported_without_template_cascade() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    fs::write(
        dir.path().join("navigator.yaml"),
        "version: 26.9.14\nproject: [not, a, project]\n",
    )
    .unwrap();

    gate(dir.path())
        .failure()
        .stderr(str::contains("navigator.yaml"))
        .stderr(str::contains("Y005"))
        .stdout(str::contains("0 template(s)"))
        .stdout(str::contains("template `code`").not());
}

#[test]
fn the_ci_ref_must_match_the_manifest_version() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    let ci = dir.path().join(".github/workflows/ci.yml");
    let contents = fs::read_to_string(&ci).unwrap();
    fs::write(
        &ci,
        contents.replace("project-gate.yml@26.8.23", "project-gate.yml@26.8.22"),
    )
    .unwrap();

    gate(dir.path())
        .failure()
        .stderr(str::contains("must equal manifest version"))
        .stderr(str::contains("26.8.22"))
        .stderr(str::contains("26.8.23"));
}

/// The fixed shell — see [`write_repository_shell`] — is exactly what
/// `docs/project-repositories.md#a-repositorys-fixed-shell` documents, and it
/// passes its own gate.
#[test]
fn a_fresh_repository_shell_validates() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");

    offline_gate_is_clean(gate(dir.path())).stdout(str::contains("1 template(s), 0 error(s)"));

    assert!(dir.path().join("README.md").is_file());
    assert!(dir.path().join("AGENTS.md").is_file());
    assert!(
        !dir.path().join("CLAUDE.md").exists(),
        "the shell carries one contract file, and it is AGENTS.md"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join(".github/CODEOWNERS")).unwrap(),
        "# CODEOWNERS\n\n* @shicholas\n"
    );
    let instructions = fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
    assert!(instructions.contains("## Folders"));
    assert!(instructions.contains("navigator site import"));
    assert!(instructions.contains("## Tools"));
    assert!(instructions.contains("**Navigator CLI**"));
    assert!(instructions.contains("**Gmail**"));
    assert!(instructions.contains("**CourtListener**"));
    assert!(instructions.contains("The last four leave the firm."));
    assert!(instructions.contains("## Feedback"));
    assert!(!instructions.contains("navigator project gate --ci"));
    assert!(dir.path().join("drafts/onboarding__letter.md").is_file());
    assert!(
        fs::read_to_string(dir.path().join("drafts/onboarding__letter.md"))
            .unwrap()
            .contains("kind: onboarding\n"),
        "the stub must declare the kind it is so a filled-in placeholder inherits it"
    );
    assert!(!dir.path().join("documents/.gitignore").exists());
    assert!(fs::read_to_string(dir.path().join(".gitignore"))
        .unwrap()
        .lines()
        .any(|line| line == "/documents/"));
    assert_eq!(
        fs::read_to_string(dir.path().join(".gitattributes")).unwrap(),
        "* text=auto eol=lf\n"
    );
    let workflow = fs::read_to_string(dir.path().join(".github/workflows/ci.yml")).unwrap();
    assert!(workflow.contains("project-gate.yml@"));
    assert!(workflow.contains("on:\n  pull_request:"));
    assert!(!workflow.contains("push:"));
    let workflow_yaml: serde_yaml::Value =
        serde_yaml::from_str(&workflow).expect("scaffolded ci.yml parses as YAML");
    assert_eq!(
        workflow_yaml["permissions"],
        serde_yaml::from_str::<serde_yaml::Value>("{contents: read, id-token: write}").unwrap()
    );
    assert!(
        !workflow.contains("with:"),
        "the caller must carry no `with:` block; the reusable workflow reads project/host from navigator.yaml:\n{workflow}"
    );
    let cd = fs::read_to_string(dir.path().join(".github/workflows/cd.yml")).unwrap();
    assert!(cd.contains("id-token: write"));
    assert!(cd.contains("neon-law/navigator/.github/workflows/project-publish.yml@26.8.23"));
    assert!(cd.contains("workflow_dispatch:"));
    assert!(
        !cd.contains("with:"),
        "the gate/publish callers must carry no `with:` block; the reusable workflow reads project/host from navigator.yaml:\n{cd}"
    );

    // Neither retired manifest is written. `mount.json` and `navigator.toml`
    // declared a repository's own coordinates and every reader of them is gone.
    assert!(!dir.path().join("navigator.toml").exists());
    assert!(!dir.path().join("mount.json").exists());
}

/// ENG-675 reproduction 1: a freshly built repository's `.github/` tree is
/// closed to CODEOWNERS and the two thin workflow callers. Every one of the
/// five mutations below used to exit `0` against the CLI at
/// `a63c7a91aafa5159bfb5b4cf507537940d030db5`; each is now a real-CLI
/// regression against the fix.
#[test]
fn the_gate_refuses_an_extra_github_file() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    fs::write(dir.path().join(".github/extra.txt"), "scratch\n").unwrap();

    gate(dir.path())
        .failure()
        .stderr(str::contains(".github/extra.txt"))
        .stderr(str::contains("closed `.github` file set"));
}

/// ENG-675 reproduction 2: an arbitrary replacement `cd.yml` — a repository's
/// own deploy script standing in for the two pinned reusable-workflow calls
/// — was never validated at all.
#[test]
fn the_gate_refuses_an_arbitrary_replacement_cd_workflow() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    fs::write(
        dir.path().join(".github/workflows/cd.yml"),
        "name: cd\n\
         on:\n  push:\n    branches: [main]\n  workflow_dispatch:\n\
         permissions:\n  contents: read\n  id-token: write\n\
         jobs:\n  deploy:\n    runs-on: ubuntu-latest\n    steps:\n      - run: ./deploy.sh\n",
    )
    .unwrap();

    gate(dir.path())
        .failure()
        .stderr(str::contains("exactly the `gate` and `publish` jobs"));
}

/// ENG-675 reproduction 3: an extra job carrying `contents: write` and
/// `id-token: write` next to the thin project-gate caller is a second,
/// unreviewed door into the same required check.
#[test]
fn the_gate_refuses_an_extra_privileged_ci_job() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    let ci = dir.path().join(".github/workflows/ci.yml");
    let contents = fs::read_to_string(&ci).unwrap();
    fs::write(
        &ci,
        format!(
            "{contents}  smuggled:\n    permissions:\n      contents: write\n      id-token: write\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo pwned\n"
        ),
    )
    .unwrap();

    gate(dir.path())
        .failure()
        .stderr(str::contains("exactly one job"));
}

/// ENG-828 regression guard: the closed `.github` set used to make an
/// in-repo auto-merge job unrepresentable — `ci.yml` was held to exactly one
/// job (refusing the job smuggled in above), and `.github/workflows/automerge.yml`
/// was refused outright as outside the closed set. The only way to a green
/// gate was to delete the control. A repository shaped like the fleet's own
/// three `neon-law-staging` samples — the auto-merge job in its own file,
/// `ci.yml` carrying only `ci` — must gate green with no job moved back.
///
/// The content below is typed independently of
/// `AUTOMERGE_WORKFLOW_CONTENTS` in `cli/src/projects/repository.rs`, the
/// same way the fixed strings elsewhere in this file duplicate rather than
/// import the crate's own constants: the point is to prove the validator
/// accepts this exact fleet body, not that it agrees with itself.
#[test]
fn a_fleet_representative_automerge_workflow_gates_green() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    fs::write(
        dir.path().join(".github/workflows/automerge.yml"),
        "name: automerge\n\
         \n\
         on:\n  pull_request:\n    types: [opened, synchronize, reopened, ready_for_review]\n\
         \n\
         permissions:\n  contents: read\n\
         \n\
         jobs:\n  enable-automerge:\n    if: github.event.pull_request.draft == false\n    runs-on: ubuntu-latest\n    steps:\n      \
         - name: Look for the merge-queue App credentials\n        id: credentials\n        env:\n          APP_ID: ${{ secrets.AUTOMERGE_APP_ID }}\n          APP_PRIVATE_KEY: ${{ secrets.AUTOMERGE_APP_PRIVATE_KEY }}\n        shell: bash\n        run: |\n          set -euo pipefail\n          if [ -n \"${APP_ID}\" ] && [ -n \"${APP_PRIVATE_KEY}\" ]; then\n              echo \"present=true\" >> \"${GITHUB_OUTPUT}\"\n          else\n              echo \"present=false\" >> \"${GITHUB_OUTPUT}\"\n          fi\n      \
         - name: Mint a merge-queue App token\n        id: app-token\n        if: steps.credentials.outputs.present == 'true'\n        uses: actions/create-github-app-token@bcd2ba49218906704ab6c1aa796996da409d3eb1 # v3.2.0\n        with:\n          app-id: ${{ secrets.AUTOMERGE_APP_ID }}\n          private-key: ${{ secrets.AUTOMERGE_APP_PRIVATE_KEY }}\n      \
         - name: Arm auto-merge\n        env:\n          GH_TOKEN: ${{ steps.app-token.outputs.token }}\n          PR_URL: ${{ github.event.pull_request.html_url }}\n        shell: bash\n        run: |\n          set -euo pipefail\n          if [ -z \"${GH_TOKEN:-}\" ]; then\n              echo \"::notice::merge-queue App credentials absent — arming nothing, merge by hand\"\n              exit 0\n          fi\n          gh pr merge --squash --auto \"${PR_URL}\"\n",
    )
    .unwrap();

    offline_gate_is_clean(gate(dir.path())).stdout(str::contains("0 error(s)"));
}

/// Before ENG-828, this exact file was refused as outside the closed
/// `.github` set. Under `--ci`, where nothing is written, a missing
/// `automerge.yml` is now a required-file finding rather than silently
/// absent. (The local, non-`--ci` gate self-repairs it instead — see
/// `project_gate_rewrites_a_drifted_automerge_workflow` and
/// `a_fleet_representative_automerge_workflow_gates_green`, which relies on
/// the file the shell already carries.)
#[test]
fn the_ci_gate_requires_the_automerge_workflow() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    fs::remove_file(dir.path().join(".github/workflows/automerge.yml")).unwrap();

    navigator()
        .current_dir(dir.path())
        .env("CI", "true")
        .args(["project", "gate", "--ci"])
        .assert()
        .failure()
        .stderr(str::contains("missing required"))
        .stderr(str::contains(".github/workflows/automerge.yml"));
}

/// `automerge.yml` is machine-owned, so a hand edit is drift the local gate
/// self-repairs — the same `write_fixes` behavior the legacy `documents/` layout
/// already gets, and notably the one `validate_codeowners` does not.
#[test]
fn project_gate_rewrites_a_drifted_automerge_workflow() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    let path = dir.path().join(".github/workflows/automerge.yml");
    let canonical = fs::read_to_string(&path).unwrap();
    fs::write(&path, "name: automerge\n# hand-edited\n").unwrap();

    offline_gate_is_clean(gate(dir.path())).stdout(str::contains("fixed"));
    assert_eq!(fs::read_to_string(&path).unwrap(), canonical);
}

/// Under `--ci`, nothing is written: a drifted `automerge.yml` becomes a
/// finding instead, the same as every other `--ci` fix-vs-report split.
#[test]
fn gate_ci_reports_a_drifted_automerge_workflow_without_writing() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    let path = dir.path().join(".github/workflows/automerge.yml");
    fs::write(&path, "name: automerge\n# hand-edited\n").unwrap();

    navigator()
        .current_dir(dir.path())
        .env("CI", "true")
        .args(["project", "gate", "--ci"])
        .assert()
        .failure()
        .stderr(str::contains(
            "must match the canonical auto-merge workflow",
        ));
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "name: automerge\n# hand-edited\n"
    );
}

/// ENG-675 reproduction 4: `pull_request_target` runs with the base
/// repository's secrets and write token against a fork's checked-out code.
#[test]
fn the_gate_refuses_pull_request_target() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    let ci = dir.path().join(".github/workflows/ci.yml");
    let contents = fs::read_to_string(&ci).unwrap();
    fs::write(
        &ci,
        contents.replace("pull_request:", "pull_request_target:"),
    )
    .unwrap();

    gate(dir.path())
        .failure()
        .stderr(str::contains("pull_request_target"));
}

/// The reusable workflow reads `project`/`host` from the caller's own
/// `navigator.yaml`, so a caller has nothing left to repeat there — a
/// `with:` block of any shape is rejected outright, not merely checked for
/// agreement with the manifest.
#[test]
fn the_gate_refuses_a_with_block_on_the_ci_caller() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    let ci = dir.path().join(".github/workflows/ci.yml");
    let contents = fs::read_to_string(&ci).unwrap();
    fs::write(
        &ci,
        contents.replace(
            "uses: neon-law/navigator/.github/workflows/project-gate.yml@26.8.23\n",
            "uses: neon-law/navigator/.github/workflows/project-gate.yml@26.8.23\n    with:\n      project: \"example-project\"\n",
        ),
    )
    .unwrap();

    gate(dir.path())
        .failure()
        .stderr(str::contains("must not declare a `with:` block"));
}

/// After 26.9.23, a retired `gate.yml` is refused outright — the door
/// `docs/gate.md` said this release would close.
#[test]
fn the_gate_refuses_a_retired_workflow_filename() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    let ci = dir.path().join(".github/workflows/ci.yml");
    fs::rename(&ci, dir.path().join(".github/workflows/gate.yml")).unwrap();

    gate(dir.path())
        .failure()
        .stderr(str::contains("gate.yml"))
        .stderr(str::contains("this release refuses it"));
}

/// A `.yaml` spelling of either workflow reports the extension it actually
/// found and the one Navigator reads, not a bare "missing required" that
/// sends the operator looking for a typo they already made correctly.
#[test]
fn the_gate_reports_a_yaml_extension_workflow_precisely() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    fs::rename(
        dir.path().join(".github/workflows/ci.yml"),
        dir.path().join(".github/workflows/ci.yaml"),
    )
    .unwrap();

    gate(dir.path())
        .failure()
        .stderr(str::contains(".github/workflows/ci.yaml"))
        .stderr(str::contains(".github/workflows/ci.yml"));
}

#[test]
fn gate_without_oidc_leaves_the_live_row_alone() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    navigator()
        .current_dir(dir.path())
        .env("CI", "true")
        .args(["project", "gate", "--ci"])
        .env_remove("ACTIONS_ID_TOKEN_REQUEST_URL")
        .env("GITHUB_REF", "refs/heads/main")
        .env("GITHUB_EVENT_NAME", "push")
        .assert()
        .failure()
        .code(1)
        .stderr(str::contains("live Project check is inconclusive"))
        .stderr(str::contains(
            "this CI event/ref cannot mint an authorized session",
        ));
}

/// A malformed PR ref remains offline; only the exact merge-ref shape reaches
/// the OIDC door.
#[test]
fn gate_ci_malformed_pr_ref_leaves_the_live_row_alone() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    navigator()
        .current_dir(dir.path())
        .env("CI", "true")
        .args(["project", "gate", "--ci"])
        .env("ACTIONS_ID_TOKEN_REQUEST_URL", "http://127.0.0.1/oidc")
        .env("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "token")
        .env("GITHUB_REF", "refs/heads/topic")
        .env("GITHUB_EVENT_NAME", "pull_request")
        .assert()
        .failure()
        .code(1)
        .stderr(str::contains("live Project check is inconclusive"))
        .stderr(str::contains(
            "this CI event/ref cannot mint an authorized session",
        ));
}

#[test]
fn a_nested_template_is_refused_in_a_project_repository() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "acme");
    fs::rename(dir.path().join("drafts"), dir.path().join("templates")).unwrap();
    let flat = dir.path().join("templates/onboarding__letter.md");
    let nested = dir.path().join("templates/neon_law/onboarding__letter.md");
    fs::create_dir_all(nested.parent().unwrap()).unwrap();
    fs::rename(&flat, &nested).unwrap();
    gate(dir.path())
        .failure()
        .code(1)
        .stdout(str::contains("N110"))
        .stdout(str::contains(
            "Project templates must be direct `templates/<code>.md` files",
        ))
        .stderr(str::contains(
            "Project templates must be direct `templates/<code>.md` files",
        ));
}

#[test]
fn a_project_repository_accepts_the_templates_alias_with_a_rename_warning() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "acme");
    offline_gate_is_clean(gate(dir.path()))
        .stdout(str::contains("1 template(s), 0 error(s)"))
        .stdout(str::contains("alias spelling").not());

    fs::rename(dir.path().join("drafts"), dir.path().join("templates")).unwrap();
    offline_gate_is_clean(gate(dir.path()))
        .stdout(str::contains("1 template(s), 0 error(s)"))
        .stdout(str::contains("git mv templates drafts"));
}

#[test]
fn a_project_repository_refuses_both_blueprint_directories_even_when_one_is_empty() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "acme");
    fs::create_dir(dir.path().join("templates")).unwrap();

    gate(dir.path())
        .failure()
        .code(1)
        .stderr(str::contains("templates/"))
        .stderr(str::contains("drafts/"));
}

#[test]
fn drafts_use_the_same_flat_markdown_and_code_rules_as_templates() {
    let nested = TempDir::new().unwrap();
    scaffold(nested.path(), "acme");
    fs::create_dir(nested.path().join("drafts/nested")).unwrap();
    fs::rename(
        nested.path().join("drafts/onboarding__letter.md"),
        nested.path().join("drafts/nested/onboarding__letter.md"),
    )
    .unwrap();
    gate(nested.path())
        .failure()
        .stderr(str::contains("direct `drafts/<code>.md`"));

    let empty = TempDir::new().unwrap();
    scaffold(empty.path(), "acme");
    fs::remove_file(empty.path().join("drafts/onboarding__letter.md")).unwrap();
    gate(empty.path())
        .failure()
        .stderr(str::contains("`drafts/` is present but empty"));

    let non_markdown = TempDir::new().unwrap();
    scaffold(non_markdown.path(), "acme");
    fs::rename(
        non_markdown.path().join("drafts/onboarding__letter.md"),
        non_markdown.path().join("drafts/onboarding.txt"),
    )
    .unwrap();
    gate(non_markdown.path()).failure().stderr(str::contains(
        "only Markdown template blueprints belong in `drafts/`",
    ));

    let mismatched = TempDir::new().unwrap();
    scaffold(mismatched.path(), "acme");
    let path = mismatched.path().join("drafts/onboarding__letter.md");
    let body = fs::read_to_string(&path).unwrap().replace(
        "title: Onboarding letter",
        "title: Onboarding letter\ncode: other",
    );
    fs::write(&path, body).unwrap();
    offline_gate_is_clean(gate(mismatched.path()));
    assert!(!fs::read_to_string(path).unwrap().contains("code:"));
}

/// ENG-693: a Project template's `code` is already scoped to this repository's
/// own Project by `template.project_id`, so the filename carries no required
/// Project-code prefix — a bare stem validates like any other.
#[test]
fn a_template_filename_needs_no_project_code_prefix() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "acme");
    let path = dir.path().join("drafts/onboarding__letter.md");
    let body = fs::read_to_string(&path)
        .unwrap()
        .replace("code: onboarding", "code: project_template");
    fs::rename(&path, dir.path().join("drafts/project_template.md")).unwrap();
    fs::write(dir.path().join("drafts/project_template.md"), body).unwrap();
    offline_gate_is_clean(gate(dir.path())).stdout(str::contains("1 template(s), 0 error(s)"));
}

#[test]
fn legacy_template_code_is_removed_by_the_project_gate() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "acme");
    let path = dir.path().join("drafts/onboarding__letter.md");
    let body = fs::read_to_string(&path).unwrap().replace(
        "title: Onboarding letter",
        "title: Onboarding letter\ncode: other",
    );
    fs::write(&path, body).unwrap();
    offline_gate_is_clean(gate(dir.path()));
    let fixed = fs::read_to_string(path).unwrap();
    assert!(!fixed.contains("code:"));
}

/// A Project carrying no Drafts yet is reported distinctly and is not a failure.
///
/// A Project may legitimately open before either half exists, so this is a note
/// rather than an error — the same split the doctor keeps between a warning and
/// a failure.
#[test]
fn a_repository_carrying_no_drafts_is_reported_and_not_failed() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    let drafts = dir.path().join("drafts");
    if drafts.exists() {
        fs::remove_dir_all(drafts).unwrap();
    }

    offline_gate_is_clean(gate(dir.path())).stdout(str::contains("carries no `drafts/` yet"));
}

/// The repository name is the Project code, so a name that could not be one is
/// the error — there is no manifest to disagree with.
#[test]
fn a_repository_name_that_is_not_a_valid_project_code_is_refused() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");

    gate_as(dir.path(), "Not_A_Code")
        .failure()
        .code(1)
        .stderr(str::contains("is not a valid Navigator Project code"));

    // `new` is well-formed and still refused: `/app/projects/new` is
    // Navigator's matter-open form.
    gate_as(dir.path(), "new")
        .failure()
        .code(1)
        .stderr(str::contains("is not a valid Navigator Project code"));
}

/// The check the validator exists for, kept through the restructuring.
#[test]
fn client_uploads_and_generated_output_are_refused() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    fs::create_dir_all(dir.path().join("uploads")).unwrap();
    // Not a PDF: the canonical root `.gitignore` already hides every `*.pdf`
    // (LAW-181), so a PDF here would never reach the layout walk.
    fs::write(dir.path().join("uploads/client-document.txt"), "synthetic").unwrap();
    fs::create_dir_all(dir.path().join("target")).unwrap();
    fs::write(dir.path().join("target/build.log"), "synthetic").unwrap();
    fs::write(dir.path().join(".env.production"), "SECRET=x").unwrap();

    gate(dir.path())
        .failure()
        .code(1)
        .stderr(str::contains("forbidden `uploads` path"))
        .stderr(str::contains("forbidden `target` path"))
        .stderr(str::contains("must not be committed"));
}

#[test]
fn document_pointers_are_source_but_document_bytes_are_refused() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    fs::create_dir_all(dir.path().join("documents/exhibits/2026-09-05")).unwrap();
    fs::create_dir_all(dir.path().join("blobs/exhibits/2026-09-05")).unwrap();
    fs::write(
        dir.path()
            .join("blobs/exhibits/2026-09-05/screenshot.png.yaml"),
        POINTER_YAML,
    )
    .unwrap();

    offline_gate_is_clean(gate(dir.path())).stdout(str::contains("0 error(s)"));

    // A raw byte staged under the ignored `documents/` is the supported
    // workflow's own output — `project sync` puts it there — so it is not a
    // committed legal document and the gate leaves it alone (LAW-12).
    let binary = dir
        .path()
        .join("documents/exhibits/2026-09-05/screenshot.png");
    fs::write(&binary, b"synthetic image bytes").unwrap();
    // Ignored, so nothing proposes it and the gate stays green.
    offline_gate_is_clean(gate(dir.path())).stdout(str::contains("0 error(s)"));

    // Forcing it past the guard and into the index is the thing the rule
    // names, and that is still refused.
    run_git(
        dir.path(),
        &["add", "-f", "documents/exhibits/2026-09-05/screenshot.png"],
    );
    gate(dir.path())
        .failure()
        .code(1)
        .stderr(str::contains(binary.display().to_string()))
        .stderr(str::contains(
            "legal documents and raw document bytes must not be committed",
        ));
}

/// `project sync` exists to put bytes under `documents/`, and the root
/// `.gitignore` keeps that whole folder untracked. The gate
/// enumerates what Git would carry, so it reads them as absent rather than as
/// committed legal material — otherwise the only way to get a green run would
/// be to delete the bytes those commands exist to fetch (LAW-12).
#[test]
fn gate_ignores_raw_document_bytes_materialised_by_a_pull() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    fs::create_dir_all(dir.path().join("documents/memos")).unwrap();
    fs::create_dir_all(dir.path().join("blobs/memos")).unwrap();
    let raw = dir.path().join("documents/memos/agreement.md");
    fs::write(&raw, "synthetic pulled bytes\n").unwrap();

    navigator()
        .current_dir(dir.path())
        .args(["project", "gate"])
        .assert()
        .failure()
        .code(1)
        .stdout(str::contains("0 error(s)"))
        .stderr(str::contains("live Project check is inconclusive"))
        .stderr(predicates::str::contains(raw.display().to_string()).not());
}

#[test]
fn gate_leaves_ignored_document_bodies_byte_for_byte_unchanged() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    fs::create_dir_all(dir.path().join("documents/memos")).unwrap();
    fs::create_dir_all(dir.path().join("blobs/memos")).unwrap();
    let raw = dir.path().join("documents/memos/agreement.md");
    let contents = concat!(
        "The court applied §\n",
        "2033.300 in *Case Name* (2008), describing *Elston* and the\n",
        "applicable standard. See [the authority][authority].\n\n",
        "[authority]: <https://www.example.com/cases/california/supreme-court/",
        "extraordinarily-long-reference-path/2025/1234567890/",
        "opinion?download=1&format=html#section-2033-300>\n",
    );
    fs::write(&raw, contents.as_bytes()).unwrap();
    fs::write(
        dir.path().join("blobs/memos/agreement.pdf.yaml"),
        "kind: agreement\nvisibility: internal\ncurrent_version:\n  version: 1\n  asset_id: 0199b9e4-14b7-7ad0-87a5-71ef24a46d40\n  created_at: 2026-09-05T12:00:00Z\n  sha256: 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n  size_bytes: 42\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("authored.md"),
        "Authored repository content with an unwrapped URL https://example.com/path.\n",
    )
    .unwrap();

    let output = navigator()
        .current_dir(dir.path())
        .args(["project", "gate"])
        .output()
        .unwrap();

    assert_eq!(fs::read(&raw).unwrap(), contents.as_bytes());
    assert!(
        !output.status.success(),
        "the authored Markdown violation must still fail the gate:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("authored.md") && stdout.contains("M034"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Validated 1 document pointer(s), found 0 error(s)"),
        "{stdout}"
    );
    assert!(
        !stdout.contains(&raw.display().to_string()),
        "ignored document body entered the Markdown walk:\n{stdout}"
    );
}

#[test]
fn gate_reports_a_tracked_raw_document_byte() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    fs::create_dir_all(dir.path().join("documents/memos")).unwrap();
    fs::create_dir_all(dir.path().join("blobs/memos")).unwrap();
    let raw = dir.path().join("documents/memos/agreement.md");
    fs::write(&raw, "synthetic tracked bytes\n").unwrap();
    run_git(dir.path(), &["add", "-f", "documents/memos/agreement.md"]);

    navigator()
        .current_dir(dir.path())
        .args(["project", "gate"])
        .assert()
        .failure()
        .code(1)
        .stderr(str::contains(raw.display().to_string()))
        .stderr(str::contains(
            "legal documents and raw document bytes must not be committed",
        ))
        .stdout(str::contains("1 error(s)"));
}

#[test]
fn a_legacy_pointer_under_documents_warns_under_ci_for_one_release_and_is_left_alone() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    let legacy = dir.path().join("documents/memos/agreement.pdf.yaml");
    fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    fs::write(&legacy, POINTER_YAML).unwrap();

    navigator()
        .current_dir(dir.path())
        .env("CI", "true")
        .args(["project", "gate", "--ci"])
        .assert()
        .stdout(str::contains("warning: Y014"))
        .stdout(str::contains("1 pointer(s) still sit under `documents/`"))
        .stdout(str::contains("the next release refuses it"))
        .stderr(str::contains("Y014").not());

    assert_eq!(fs::read_to_string(&legacy).unwrap(), POINTER_YAML);
    assert!(!dir.path().join("blobs").exists());
}

#[test]
fn project_gate_moves_legacy_pointers_to_blobs_and_retires_the_guard() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    let documents = dir.path().join("documents");
    fs::create_dir_all(documents.join("memos")).unwrap();
    fs::write(
        documents.join(".gitignore"),
        "*\n!*/\n!*.yaml\n!.gitignore\n",
    )
    .unwrap();
    fs::write(documents.join("memos/agreement.pdf.yaml"), POINTER_YAML).unwrap();
    fs::write(documents.join("memos/agreement.pdf"), b"synthetic bytes").unwrap();
    // Tracked, as the legacy layout committed them: the run that moves them
    // must not then refuse the removals it leaves for the person to commit.
    run_git(
        dir.path(),
        &[
            "add",
            "-f",
            "documents/.gitignore",
            "documents/memos/agreement.pdf.yaml",
        ],
    );

    offline_gate_is_clean(gate(dir.path()))
        .stdout(str::contains("moved 1 pointer(s) to blobs/"))
        .stdout(str::contains("0 error(s)"));

    assert_eq!(
        fs::read_to_string(dir.path().join("blobs/memos/agreement.pdf.yaml")).unwrap(),
        POINTER_YAML
    );
    assert!(!documents.join("memos/agreement.pdf.yaml").exists());
    assert!(!documents.join(".gitignore").exists());
    assert_eq!(
        fs::read(documents.join("memos/agreement.pdf")).unwrap(),
        b"synthetic bytes"
    );
}

/// `.github/` is now closed to exactly CODEOWNERS and the two thin workflow
/// callers, so a nested ignore file there would itself be an unenumerated
/// path; `tests/` carries no such closed set and still proves the same
/// thing: `git ls-files --exclude-standard` honours a directory's own
/// `.gitignore`, not only the root one. `tests/` is an allowed root a
/// repository may create for its own source-level checks (LAW-49: the gate
/// neither requires nor generates one), so this test creates it itself.
#[test]
fn gate_honours_a_nested_ignore_file() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    fs::create_dir_all(dir.path().join("tests")).unwrap();
    fs::write(dir.path().join("tests/.gitignore"), "*.env\n").unwrap();
    let ignored = dir.path().join("tests/hidden.env");
    fs::write(&ignored, "synthetic secret\n").unwrap();

    navigator()
        .current_dir(dir.path())
        .args(["project", "gate"])
        .assert()
        .failure()
        .code(1)
        // The nested ignore file is itself outside `Y024`'s canonical list,
        // so it is the one error; what it ignores is still never read.
        .stdout(str::contains("1 error(s)"))
        .stderr(str::contains(
            "Y024: `tests/.gitignore` cannot be committed",
        ))
        .stderr(predicates::str::contains(ignored.display().to_string()).not());
}

#[test]
fn gate_refuses_a_repository_shell_that_is_not_a_git_repository() {
    let dir = TempDir::new().unwrap();
    write_repository_shell(dir.path(), "example-project");

    navigator()
        .current_dir(dir.path())
        .args(["project", "gate"])
        .assert()
        .failure()
        .code(2)
        .stderr(str::contains("has no .git"));
}

/// The retired `notation repository` command is gone rather than aliased.
#[test]
fn the_notation_repository_command_is_gone() {
    navigator()
        .args(["notation", "repository", "validate", "."])
        .assert()
        .failure();
}

/// The harness-specific mirrors are refused by name, in one place, for every
/// Project repository at once.
///
/// `CLAUDE.md` was already refused — as an anonymous unenumerated root, which
/// said nothing about what survives it. A `.claude/` or a `.codex/` was not
/// refused at all: the layout walk matched a path's first component against
/// the allowed roots only when that component was the whole path, so a
/// committed mirror directory was never examined. Each now names its survivor
/// — `AGENTS.md` or `.agents/skills/` — and the remedy.
#[test]
fn gate_refuses_the_retired_agent_mirrors_by_name() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    fs::write(dir.path().join("CLAUDE.md"), "AGENTS.md").unwrap();
    for mirror in [".claude", ".codex"] {
        let skill = dir.path().join(mirror).join("skills/council");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), "# mirrored council\n").unwrap();
    }

    gate(dir.path())
        .failure()
        .code(1)
        .stderr(str::contains(
            "`CLAUDE.md` is a retired agent-instruction mirror",
        ))
        .stderr(str::contains("`AGENTS.md` is the whole contract"))
        .stderr(str::contains(
            "`.codex/` is a retired agent-instruction mirror",
        ))
        .stderr(str::contains("`.agents/skills/` is the whole catalog"));
}

/// The canonical pair is what a repository is meant to carry, so a checkout
/// holding both passes: this refuses the mirrors, not agent tooling.
#[test]
fn gate_accepts_the_canonical_contract_and_catalog() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");

    assert!(dir.path().join("AGENTS.md").is_file());
    assert!(dir.path().join(".agents/skills").is_dir());
    offline_gate_is_clean(gate(dir.path())).stdout(str::contains("0 error(s)"));
}

#[test]
fn gate_requires_the_canonical_codeowners_file() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    let path = dir.path().join(".github/CODEOWNERS");

    fs::remove_file(&path).unwrap();
    gate(dir.path())
        .failure()
        .code(1)
        .stderr(str::contains("missing required `.github/CODEOWNERS`"));

    fs::write(&path, "* @nick\n").unwrap();
    gate(dir.path()).failure().code(1).stderr(str::contains(
        "must contain the canonical single-owner rule",
    ));
}

/// A synced skill that has drifted from the canonical copy fails `validate`
/// and names the file, so a hand edit is caught rather than silently
/// diverging across 19 repositories.
#[test]
fn gate_fails_on_a_drifted_synced_skill() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");

    fs::write(
        dir.path().join(".agents/skills/council/SKILL.md"),
        "drifted content",
    )
    .unwrap();

    gate(dir.path())
        .failure()
        .code(1)
        .stderr(str::contains("synced skill `council` has drifted"));
}

/// A repository carrying the full canonical catalog gates green with no
/// further step.
#[test]
fn gate_passes_with_the_full_skill_catalog() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    assert!(
        dir.path()
            .join(".agents/skills/record-search/SKILL.md")
            .is_file(),
        "the shell must include every synced skill"
    );

    offline_gate_is_clean(gate(dir.path())).stdout(str::contains("0 error(s)"));
}

/// The fleet's `browser-handoff` skill borrows the attorney's own signed-in
/// browser for a download, stops rather than handling a credential, files the
/// download as a `filing`, and never clicks a control that changes a filing.
#[test]
fn browser_handoff_is_synced_and_never_handles_credentials() {
    let path = workspace_root().join(".agents/skills/browser-handoff/SKILL.md");
    let skill =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    for expected in [
        "/chrome",
        "claude --chrome",
        "browser pane",
        "ask the attorney to reconnect",
        "Never read browser cookies",
        "Never script a login",
        "Never store a credential",
        "navigator site document upload",
        "--kind filing",
        "navigator site document log",
        "submit",
        "pay",
        "resubmit",
        "withdraw",
    ] {
        assert!(
            skill.contains(expected),
            "browser-handoff must say `{expected}`"
        );
    }
}

/// `.agents/` is the opt-in, and it opts into the whole catalog.
///
/// The moment a repository has one, an agent is working in it under whatever
/// skills it happens to find, and the ones it does not find are precisely the
/// conventions nobody told it about. That is why this is a finding rather
/// than a suggestion: a fleet-wide convention that lives only as a header
/// comment inside the file it governs has, here, already broken in sixteen
/// repositories at once.
#[test]
fn gate_fails_when_agents_exists_without_the_catalog() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    fs::remove_dir_all(dir.path().join(".agents/skills/deadline")).unwrap();
    fs::remove_dir_all(dir.path().join(".agents/skills/record-search")).unwrap();
    fs::remove_dir_all(dir.path().join(".agents/skills/browser-handoff")).unwrap();
    fs::remove_dir_all(
        dir.path()
            .join(".agents/skills/opposing-communication-review"),
    )
    .unwrap();

    gate(dir.path())
        .failure()
        .code(1)
        .stderr(str::contains("missing synced skill `deadline`"))
        .stderr(str::contains("missing synced skill `record-search`"))
        .stderr(str::contains("missing synced skill `browser-handoff`"))
        .stderr(str::contains(
            "missing synced skill `opposing-communication-review`",
        ));
}

/// An older `AGENTS.md` fails the gate. `sync-skills` rewrites that file to the
/// contract embedded in this CLI and leaves a custom skill alone. A second run
/// reports that the canonical files already match.
#[test]
fn sync_skills_restores_an_old_agents_contract_and_keeps_a_custom_skill() {
    let dir = TempDir::new().unwrap();
    scaffold(dir.path(), "example-project");
    fs::write(dir.path().join("AGENTS.md"), "# older project contract\n").unwrap();
    let custom = dir.path().join(".agents/skills/custom/SKILL.md");
    fs::create_dir_all(custom.parent().unwrap()).unwrap();
    fs::write(&custom, "# custom skill\n").unwrap();

    gate(dir.path())
        .failure()
        .code(1)
        .stderr(str::contains(
            "canonical contract embedded in Navigator CLI",
        ))
        .stderr(str::contains("navigator project repository sync-skills"))
        .stderr(str::contains("navigator project gate: markdown content"))
        .stderr(str::contains("navigator project gate: repository layout"));

    navigator()
        .args(["project", "repository", "sync-skills", "--dir"])
        .arg(dir.path())
        .assert()
        .success()
        .stdout(str::contains("repaired AGENTS.md"))
        .stdout(str::contains(".agents/skills/custom/SKILL.md").not());

    assert_eq!(fs::read(&custom).unwrap(), b"# custom skill\n");
    assert_eq!(
        fs::read_to_string(dir.path().join("AGENTS.md")).unwrap(),
        AGENT_CONTRACT
    );

    navigator()
        .args(["project", "repository", "sync-skills", "--dir"])
        .arg(dir.path())
        .assert()
        .success()
        .stdout(str::contains("already match"));

    gate(dir.path()).stderr(str::contains("canonical contract embedded").not());
}

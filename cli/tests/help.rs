//! Integration tests for the top-level `navigator --help` output.

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str;

fn help(args: &[&str]) -> String {
    let output = Command::cargo_bin("navigator")
        .unwrap()
        .args(args)
        .output()
        .expect("run navigator help");
    assert!(
        output.status.success(),
        "`navigator {}` failed:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("help output is utf-8")
}

/// Collapse every run of whitespace to a single space so assertions on help
/// prose survive clap's width-dependent line wrapping.
fn unwrapped(output: &str) -> String {
    output.split_whitespace().collect::<Vec<_>>().join(" ")
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
fn top_level_help_is_a_terse_legal_safe_headline() {
    Command::cargo_bin("navigator")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(str::contains("Navigator CLI, not legal advice."));
}

#[test]
fn top_level_help_keeps_orchestration_nested_under_groups() {
    let output = help(&["--help"]);

    assert_eq!(
        command_names(&output),
        vec![
            // These, and nothing else. Each names what it owns, so the top
            // layer IS the mental model rather than two dozen flat rows an
            // operator has to scan.
            "clause",
            "dev",
            "forms",
            "glossary",
            "journal",
            "lsp",
            "mail",
            "notation",
            "ops",
            "pii",
            "project",
            "quickstart",
            "site",
            "validate",
            "help",
        ]
    );

    // The one door a newcomer is told to open after `brew install`.
    assert!(output
        .lines()
        .any(|l| l.trim_start().starts_with("quickstart ")
            && l.contains("Run all of Navigator on this machine")));

    // Billing operations belong in the portal, not the notation author's CLI.
    assert!(!command_names(&output).contains(&"coupon"));
    assert!(!command_names(&output).contains(&"subscription"));

    // Command/description coupling, tolerant of clap's column width (which
    // grows with the longest command name).
    assert!(output.lines().any(|l| l.trim_start().starts_with("dev ")
        && l.contains("Local, reversible KIND developer loop")));
    assert!(output.lines().any(
        |l| l.trim_start().starts_with("ops ") && l.contains("Production and cloud operations")
    ));
    assert!(output
        .lines()
        .any(|l| l.trim_start().starts_with("validate ")
            && l.contains("Validate Markdown and YAML files")));
    assert!(command_names(&output)
        .iter()
        .all(|name| !name.starts_with("start-")));
    assert!(!output.contains("  ship"));
    assert!(!output.contains("  deploy"));
    assert!(!command_names(&output).contains(&"login"));
    assert!(!command_names(&output).contains(&"projects"));
}

#[test]
fn mail_help_exposes_only_read_draft_file_and_setup_actions() {
    let output = help(&["mail", "--help"]);

    assert_eq!(
        command_names(&output),
        vec!["draft", "file", "read", "search", "setup", "help"]
    );
    assert!(!command_names(&output).contains(&"send"));
    assert!(!command_names(&output).contains(&"forward"));
}

#[test]
fn catalog_seed_help_uses_a_headline() {
    let output = unwrapped(&help(&["site", "seed", "--help"]));

    assert!(
        output.contains("Seed the workspace-owned template and question catalog from clean files.")
    );
}

/// `notation` is the notation author's local workbench: every member
/// operates on files under `templates/notations/`, or on the `kind:` and
/// `output:` rules those files obey, offline. Pinning the
/// membership keeps a live-site command, or the forms-vendoring command
/// (its own top-level home), from drifting into it.
#[test]
fn notation_help_lists_the_notation_authoring_workbench() {
    let output = help(&["notation", "--help"]);

    assert_eq!(
        command_names(&output),
        vec!["kinds", "pdf", "preview", "word", "help"]
    );
}

/// `forms` owns vendoring, pinning, and inspecting the blank government
/// forms in the assets bucket. Pinning membership keeps it from drifting
/// beyond that one job.
#[test]
fn forms_help_lists_the_vendoring_operations() {
    let output = help(&["forms", "--help"]);

    assert_eq!(
        command_names(&output),
        vec!["fields", "re-author", "sync", "help"]
    );
}

#[test]
fn site_import_help_lists_the_seed_arguments() {
    Command::cargo_bin("navigator")
        .unwrap()
        .args(["site", "import", "--help"])
        .assert()
        .success()
        .stdout(str::contains("[MODEL_NAME] [SEED_FILE]"))
        .stdout(str::contains("--overwrite"))
        .stdout(str::contains("--dry-run"))
        .stdout(str::contains("--ci"))
        .stdout(str::contains("--dir").not());
}

#[test]
fn site_projects_close_accepts_an_effective_time() {
    let output = help(&["project", "close", "--help"]);

    assert!(output.contains("--effective-at <EFFECTIVE_AT>"));
    assert!(unwrapped(&output).contains("RFC 3339 time when the matter actually closed."));
}

/// The disclosure has to carry the *fallback* as well as the mappings it
/// enumerates. A lawyer who drops a file into a folder the conventions do not
/// name gets `unclassified`, and without that sentence the help reads as
/// though the enumerated three are the only accepted shapes — so the silent
/// pass looks like a bug and the folder looks like a mistake. Pinned
/// separately from the mappings because it is the half most easily lost when
/// someone tightens the prose.
#[test]
fn project_sync_help_discloses_defaults_and_preserved_keys() {
    let output = unwrapped(&help(&["project", "sync", "--help"]));
    assert!(output.contains("internal-visible"));
    assert!(output.contains("pleadings to filing"));
    assert!(output.contains("content-addressed"));
    assert!(output.contains("does not rename or migrate those keys"));
    assert!(output.contains("everything else to unclassified"));
    assert!(output.contains("intentionally unclassified, not an error"));
}

/// `projects` is the Project workspace group: the verbs that operate on
/// the Drive folder plus the one repository a code names live with the site's
/// project list and workbench.
///
/// `doctor` reads a machine, `drift` reconciles the checkouts against the
/// live rows, and `setup` composes the authenticated Drive, repository,
/// Slack, and Notion doors. The split matters, because `projects doctor` and
/// `projects drift` promise to change nothing, while `setup` reports each
/// resource independently. `close` moves a matter to `closed` and then
/// archives its repository as a `closed_repository` document, zipping the
/// working tree at its current commit — one command rather than a separate
/// deliberate step.
///
/// The retired `projects application` verb (singular, for registering one
/// application name) is asserted gone rather than merely absent from this
/// list: a Project has one portal, so there was no application name for an
/// operator to register. `build` and `applications` remain hidden aliases
/// during their one-release transitions to the portal-only interface.
#[test]
fn projects_help_lists_the_project_workspace_verbs() {
    assert_eq!(
        command_names(&help(&["project", "--help"])),
        vec![
            "adverse",
            "close",
            "communication",
            "create",
            "deadline",
            "docket",
            "doctor",
            "drift",
            "drive-ingest",
            "efiling",
            "feedback-check",
            "finance",
            "gate",
            "import-source",
            "notations",
            "offboard",
            "onboard",
            "pages",
            "participant",
            "preview",
            "reference",
            "repository",
            "scaffold",
            "search",
            "service",
            "set",
            "setup",
            "skill",
            "status",
            "sync",
            "help"
        ]
    );
    let notations = unwrapped(&help(&["project", "notations", "--help"]));
    assert!(notations.contains("--stale"));
    assert!(notations.contains("--json"));
    let setup = unwrapped(&help(&["project", "setup", "--help"]));
    assert!(setup.contains("--all"));
    assert!(setup.contains("--json"));
    assert!(
        setup.contains("Complete an existing Project's Drive, GitHub, Slack, Notion, and shared")
    );
    let set = unwrapped(&help(&["project", "set", "--help"]));
    assert!(set.contains("live"));
}

/// The Portal commands are retired with Portal publishing: `project portal`,
/// and its hidden `build` and `applications` aliases, no longer parse.
#[test]
fn the_retired_portal_commands_no_longer_parse() {
    for verb in ["portal", "build", "applications"] {
        let output = Command::cargo_bin("navigator")
            .unwrap()
            .args(["project", verb])
            .output()
            .expect("run the retired verb");
        assert!(!output.status.success(), "`project {verb}` still parses");
    }
}

/// Two commands are spelled `doctor` and they diagnose different things.
/// `ops doctor` reads a running Kubernetes namespace; `projects doctor` reads
/// deployment workspace coordinates and this machine. Neither help text may
/// leave an operator guessing which one they want.
#[test]
fn the_two_doctors_keep_distinct_headlines() {
    let projects = unwrapped(&help(&["project", "doctor", "--help"]));
    assert!(
        projects.contains("Verify this machine and a Project workspace before Navigator creates."),
        "projects doctor headline: {projects}"
    );

    let ops = unwrapped(&help(&["ops", "doctor", "--help"]));
    assert!(
        ops.contains("Diagnose ongoing scheduled-job health."),
        "ops doctor headline: {ops}"
    );
}

/// The glossary Notion helper prints the page for a push out of the repository.
#[test]
fn the_glossary_notion_helper_keeps_its_headline() {
    let notion = unwrapped(&help(&["glossary", "notion", "--help"]));
    assert!(
        notion.contains("Print the glossary as one Markdown page Notion can hold"),
        "glossary notion headline: {notion}"
    );
}

/// `glossary` is top level: reading the ontology needs no cluster, no
/// database, and no checkout — the terms are embedded in the binary.
#[test]
fn glossary_keeps_only_the_reference_helpers() {
    assert_eq!(
        command_names(&help(&["glossary", "--help"])),
        vec!["list", "notion", "show", "help"]
    );
}

#[test]
fn dev_help_lists_local_loop_members() {
    let output = help(&["dev", "--help"]);

    assert_eq!(
        command_names(&output),
        vec![
            "browser-e2e",
            "build-webapp",
            "deploy",
            "down",
            "e2e",
            "email-summary-e2e",
            "env",
            "flake-hunt",
            "forgejo",
            "grant-lawyer",
            "install",
            "kind",
            "kustomize",
            "logs",
            "sample-project",
            "seaweedfs-bootstrap",
            "sendgrid-openapi",
            "serve",
            "staging",
            "status",
            "undeploy",
            "up",
            "worker-reload",
            "worktree-env",
            "help",
        ]
    );
}

#[test]
fn ops_secrets_help_exposes_the_repo_apply_and_encrypted_import_commands() {
    assert_eq!(
        command_names(&help(&["ops", "secrets", "--help"])),
        // The repository remains the operator source; encrypted dashboard
        // requests can be imported into it before applying.
        vec!["apply", "import-social-changes", "set", "unset", "help"]
    );

    let apply = unwrapped(&help(&["ops", "secrets", "apply", "--help"]));
    assert!(apply.contains("--deployment"));
    assert!(apply.contains("--dry-run"));
    // The same tree-location flag `ops ship` takes. This command is the one an
    // operator runs FROM the deployment checkout, so it needs it at least as
    // much as ship does.
    assert!(apply.contains("--deployments-dir <DIR>"));

    let import = unwrapped(&help(&[
        "ops",
        "secrets",
        "import-social-changes",
        "--help",
    ]));
    assert!(import.contains("--deployment"));
    assert!(import.contains("--deployments-dir <DEPLOYMENTS_DIR>"));
    assert!(import.contains("--request"));
}

/// `ops ship` selects its deployment by an explicit flag — the whole safety
/// property, because an environment fallback is what lets a stale shell
/// silently ship the wrong deployment. The help must show the flag and never
/// mention the retired provider.
#[test]
fn ops_ship_help_requires_an_explicit_deployment() {
    let output = unwrapped(&help(&["ops", "ship", "--help"]));
    assert!(output.contains("--deployment"));
    assert!(output.contains("--tag"));
    assert!(output.contains("--dry-run"));
    assert!(output.contains("offline plan"));
    assert!(output.contains("without live deployment checks"));
    assert!(output.contains("--restart-only"));
    // ENG-311. The flag an operator under a no-IAM-changes rule reaches for,
    // so its spelling is part of the runbook rather than an implementation
    // detail: renaming it would compile, leave every test green, and break
    // every operator who had written the old spelling down.
    assert!(output.contains("--assert-signing-iam"));
    assert!(!output.to_lowercase().contains("doppler"));

    // Omitting --deployment is a parse error, not a silent environment read.
    Command::cargo_bin("navigator")
        .unwrap()
        .args(["ops", "ship", "--tag", "26.1.1"])
        .assert()
        .failure()
        .stderr(str::contains("--deployment"));
}

/// The deployment-tree gate `ci.yml` runs, and the one thing an operator
/// must be able to read off its help: that it changes nothing and decrypts
/// nothing, so pointing it at a production tree is safe.
#[test]
fn ops_deployments_help_promises_a_read_only_names_only_check() {
    let output = unwrapped(&help(&["ops", "deployments", "--help"]));
    assert!(output.contains("--deployments-dir <DIR>"));
    assert!(
        output.contains("without changing anything or decrypting anything"),
        "the help must state what it will not do: {output}"
    );

    // Pointed at a directory with no tree it fails at the flag, rather than
    // walking up out of it into some unrelated checkout.
    Command::cargo_bin("navigator")
        .unwrap()
        .args([
            "ops",
            "deployments",
            "--deployments-dir",
            env!("CARGO_MANIFEST_DIR"),
        ])
        .assert()
        .failure()
        .stderr(str::contains("no `deployments/` directory"));
}

/// The two flags an automated deploy runs under, and the one thing an operator
/// must be able to read off the help before trusting a scheduled roll: that
/// `--image-only` changes images and nothing else, and that it refuses rather
/// than half-applying when the manifests have moved on.
#[test]
fn ops_ship_help_describes_the_narrow_automated_lane() {
    let output = unwrapped(&help(&["ops", "ship", "--help"]));
    assert!(output.contains("--image-only"));
    assert!(output.contains("--deployments-dir <DIR>"));

    // Pointed at a directory with no tree, it fails at the flag rather than
    // walking up out of it into some unrelated checkout.
    Command::cargo_bin("navigator")
        .unwrap()
        .args([
            "ops",
            "ship",
            "--deployment",
            "neon-law-test",
            "--deployments-dir",
            env!("CARGO_MANIFEST_DIR"),
            "--image-only",
            "--tag",
            "26.1.1",
        ])
        .assert()
        .failure()
        .stderr(str::contains("no `deployments/` directory"));
}

#[test]
fn worktree_env_help_lists_its_lifecycle_and_the_reclaim_command() {
    let output = help(&["dev", "worktree-env", "--help"]);

    assert_eq!(
        command_names(&output),
        vec!["down", "status", "sweep", "up", "help"]
    );
}

#[test]
fn worktree_env_sweep_help_exposes_its_explicit_apply_flag() {
    let output = unwrapped(&help(&["dev", "worktree-env", "sweep", "--help"]));

    assert!(output.contains("--apply"));
}

#[test]
fn browser_e2e_help_lists_the_gate_overrides() {
    Command::cargo_bin("navigator")
        .unwrap()
        .args(["dev", "browser-e2e", "--help"])
        .assert()
        .success()
        .stdout(str::contains("Usage: navigator dev browser-e2e"))
        .stdout(str::contains("--base-url"))
        .stdout(str::contains("NAV_BASE_URL"));
}

#[test]
fn flake_hunt_help_lists_the_package_and_runs_flag() {
    Command::cargo_bin("navigator")
        .unwrap()
        .args(["dev", "flake-hunt", "--help"])
        .assert()
        .success()
        .stdout(str::contains("Usage: navigator dev flake-hunt"))
        .stdout(str::contains("--runs"))
        .stdout(str::contains("[TEST_FILTER]"));
}

#[test]
fn grant_lawyer_help_names_the_store_it_grants_in() {
    Command::cargo_bin("navigator")
        .unwrap()
        .args(["dev", "grant-lawyer", "--help"])
        .assert()
        .success()
        .stdout(str::contains("Usage: navigator dev grant-lawyer"));
}

/// `site` owns everything that authenticates to a running deployment with the
/// stored bearer token. `auth` is flattened away: once `site` names the group,
/// `site login` already reads as "log in to the site", and the extra noun only
/// lengthened the path the regroup exists to shorten.
///
/// The order is two runs, not one list. `login` / `logout` / `whoami` / `mcp`
/// operate on the stored credential itself — `mcp` sits with them because it
/// hands that credential to an agent client rather than driving a matter. The
/// rest are the matter nouns a session then acts on.
#[test]
fn site_help_lists_the_live_deployment_members() {
    let output = help(&["site", "--help"]);

    assert_eq!(
        command_names(&output),
        vec![
            "asset",
            "authorities",
            "blog",
            "document",
            "import",
            "login",
            "logout",
            "mail",
            "mcp",
            "notation",
            "person",
            "seed",
            "synthetic-portfolio",
            "whoami",
            "help",
        ]
    );
}

/// LAW-61: `create` alone left no way to correct a field on an existing
/// Authority. `update` is the fix; this pins its presence in `--help` the
/// way the issue's own repro checked for its absence.
#[test]
fn site_authorities_help_lists_create_and_update() {
    let output = help(&["site", "authorities", "--help"]);

    assert_eq!(command_names(&output), vec!["create", "update", "help"]);
}

#[test]
fn site_asset_prepare_video_is_explicit_and_local() {
    let output = unwrapped(&help(&["site", "asset", "prepare-video", "--help"]));
    assert!(output.contains("<INPUT>"), "{output}");
    assert!(output.contains("<OUTPUT>"), "{output}");
    assert!(
        !output.contains("--host"),
        "preparation must not publish: {output}"
    );
}

/// ENG-909: `navigator site asset upload` takes one or more `--host` values
/// and a local `ASSET_NAME` path/key, with no bucket argument at all — the
/// authenticated sibling of `ops assets upload`.
#[test]
fn site_asset_upload_help_accepts_repeated_host_and_no_bucket() {
    let output = unwrapped(&help(&["site", "asset", "upload", "--help"]));
    assert!(
        output.contains("--host <HOSTS>"),
        "usage must accept --host, got: {output}"
    );
    assert!(
        !output.contains("--bucket"),
        "the authenticated upload must never take a bucket argument, got: {output}"
    );
    assert!(
        output.contains("<ASSET_NAME>"),
        "usage must take ASSET_NAME, got: {output}"
    );
    assert!(
        output.contains("--key <KEY>"),
        "usage must accept an explicit --key for an asset outside server/public, got: {output}"
    );
    assert!(
        !output.contains("--content-type"),
        "content type is always derived from the key, got: {output}"
    );
}

#[test]
fn site_document_upload_help_requires_kind() {
    let output = unwrapped(&help(&["site", "document", "upload", "--help"]));
    assert!(
        output.contains("--kind <KIND>"),
        "usage must require --kind, got: {output}"
    );
    assert!(
        !output.contains("[--kind"),
        "kind must not be an optional flag, got: {output}"
    );
    assert!(output.contains("--quality <QUALITY>"), "{output}");
    for kind in rules::kind::Kind::ALL
        .iter()
        .filter(|k| k.valid_for(rules::kind::Lane::Asset))
    {
        assert!(
            output.contains(kind.as_str()),
            "long help must name asset-lane kind `{}`, got: {output}",
            kind.as_str()
        );
    }
    assert!(
        output.contains("default: internal"),
        "long help must state the default visibility, got: {output}"
    );
}

/// `reclassify` changes a chain between two accepted kinds; `kind` keeps its
/// legacy-only meaning beside it.
#[test]
fn site_document_reclassify_requires_an_accepted_kind() {
    let members = help(&["site", "document", "--help"]);
    let names = command_names(&members);
    assert!(names.contains(&"reclassify"), "{members}");
    assert!(names.contains(&"kind"), "{members}");

    let output = unwrapped(&help(&["site", "document", "reclassify", "--help"]));
    for expected in [
        "--project <PROJECT>",
        "--kind <KIND>",
        "--dry-run",
        "<ASSET_ID>",
    ] {
        assert!(output.contains(expected), "missing `{expected}`: {output}");
    }
    assert!(!output.contains("[--kind"), "{output}");
    for kind in rules::kind::Kind::ALL
        .iter()
        .filter(|k| k.valid_for(rules::kind::Lane::Asset))
    {
        assert!(output.contains(kind.as_str()), "{output}");
    }
}

#[test]
fn site_document_reclassify_refuses_a_template_lane_kind() {
    Command::cargo_bin("navigator")
        .unwrap()
        .args([
            "site",
            "document",
            "reclassify",
            "0190a1b2-0000-7000-8000-000000000000",
            "--project",
            "acme",
            "--kind",
            "review_queue_workbench",
        ])
        .assert()
        .failure()
        .stderr(str::contains("unclassified"));
}

#[test]
fn site_document_transcribe_reads_a_pointer_and_keeps_ocr_local() {
    let output = unwrapped(&help(&["site", "document", "transcribe", "--help"])).to_lowercase();
    assert!(output.contains("source document pointer"), "{output}");
    assert!(output.contains("no ocr provider"), "{output}");
}

/// The signing record is optional at upload, and its two flags travel
/// together: either alone is a usage error, as is a method outside the list.
#[test]
fn site_document_upload_takes_an_optional_signing_record() {
    let output = unwrapped(&help(&["site", "document", "upload", "--help"]));
    assert!(
        output.contains("--signing-method <SIGNING_METHOD>"),
        "{output}"
    );
    assert!(output.contains("--signed-on <SIGNED_ON>"), "{output}");

    let upload = |extra: &[&str]| {
        let mut args = vec![
            "site",
            "document",
            "upload",
            "--project",
            "acme",
            "--file",
            "note.txt",
            "--kind",
            "onboarding",
        ];
        args.extend_from_slice(extra);
        Command::cargo_bin("navigator")
            .unwrap()
            .args(args)
            .output()
            .expect("run navigator")
    };
    for (extra, expected) in [
        (&["--signing-method", "wet_ink"][..], "--signed-on"),
        (&["--signed-on", "2026-09-04"][..], "--signing-method"),
        (
            &["--signing-method", "notarized", "--signed-on", "2026-09-04"][..],
            "`wet_ink` or `docusign`",
        ),
        (
            &["--signing-method", "docusign", "--signed-on", "09/04/2026"][..],
            "YYYY-MM-DD",
        ),
    ] {
        let output = upload(extra);
        assert!(!output.status.success(), "{extra:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(expected), "{extra:?}: {stderr}");
    }
}

/// Notarization is optional and repeatable at upload; a malformed entry is a
/// usage error before any request.
#[test]
fn site_document_upload_takes_repeatable_notarizations() {
    let output = unwrapped(&help(&["site", "document", "upload", "--help"]));
    assert!(output.contains("--notarization <NOTARIZATION>"), "{output}");

    let entry = "method=in_person,acknowledged_on=2026-09-04,state=NV,county=Clark,\
                 commission_expires=2029-01-31";
    let output = Command::cargo_bin("navigator")
        .unwrap()
        .args([
            "site",
            "document",
            "upload",
            "--project",
            "acme",
            "--file",
            "will.pdf",
            "--kind",
            "will",
            "--notarization",
            entry,
            "--notarization",
            "method=in_person,state=NV",
        ])
        .output()
        .expect("run navigator");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("acknowledged_on="), "{stderr}");
}

#[test]
fn site_document_upload_refuses_a_missing_kind() {
    Command::cargo_bin("navigator")
        .unwrap()
        .args([
            "site",
            "document",
            "upload",
            "--project",
            "acme",
            "--file",
            "note.txt",
        ])
        .assert()
        .failure()
        .stderr(str::contains("--kind"));
}

#[test]
fn site_document_upload_refuses_a_template_lane_kind() {
    let output = Command::cargo_bin("navigator")
        .unwrap()
        .args([
            "site",
            "document",
            "upload",
            "--project",
            "acme",
            "--file",
            "note.txt",
            "--kind",
            "review_queue_workbench",
        ])
        .output()
        .expect("run navigator");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    for kind in rules::kind::Kind::ALL
        .iter()
        .filter(|k| k.valid_for(rules::kind::Lane::Asset))
    {
        assert!(
            stderr.contains(kind.as_str()),
            "kind error must name asset-lane kind `{}`, got: {stderr}",
            kind.as_str()
        );
    }
}

#[test]
fn site_login_help_exposes_headless_mode() {
    Command::cargo_bin("navigator")
        .unwrap()
        .args(["site", "login", "--help"])
        .assert()
        .success()
        .stdout(str::contains("--no-browser"));
}

#[test]
fn ops_cut_release_help_exposes_the_rehearsal_and_write_flags() {
    Command::cargo_bin("navigator")
        .unwrap()
        .args(["ops", "cut-release", "--help"])
        .assert()
        .success()
        .stdout(str::contains("--dry-run"))
        .stdout(str::contains("--no-commit"))
        .stdout(str::contains("--no-fetch"));
}

#[test]
fn ops_public_snapshot_help_names_the_source_output_and_identity_flags() {
    let output = help(&["ops", "public-snapshot", "--help"]);
    for flag in [
        "--ref",
        "--out",
        "--author-name",
        "--author-email",
        "--date",
        "--sign",
    ] {
        assert!(output.contains(flag), "{flag}: {output}");
    }
    assert!(unwrapped(&output).contains("never pushed"), "{output}");
}

#[test]
fn ops_help_lists_operator_members() {
    let output = help(&["ops", "--help"]);

    assert_eq!(
        command_names(&output),
        vec![
            "assets",
            "brand-readiness",
            "cut-release",
            "deployments",
            "dns",
            "doctor",
            "email-summary",
            "firms",
            "forgejo",
            "gcp",
            "github",
            "logs",
            "lsp",
            "notices",
            "observability",
            "postgres",
            "public-snapshot",
            "rebrand",
            "release",
            "release-default-tag",
            "restate",
            "sas",
            "secrets",
            "ship",
            "surreal-archive",
            "help",
        ]
    );
}

#[test]
fn forgejo_policy_help_requires_explicit_targets_and_names_the_guarded_actions() {
    let commands = help(&["ops", "forgejo", "--help"]);
    assert_eq!(
        command_names(&commands),
        [
            "archive",
            "auto-merge",
            "bundle",
            "restore",
            "runner-once",
            "schedule-open",
            "setup",
            "help"
        ]
    );
    let bundle = help(&["ops", "forgejo", "bundle", "--help"]);
    for required in ["<MIRROR>", "<OUTPUT>"] {
        assert!(
            unwrapped(&bundle).contains(required),
            "missing {required}: {bundle}"
        );
    }
    let setup = help(&["ops", "forgejo", "setup", "--help"]);
    for required in [
        "<REPOSITORY>",
        "--url <URL>",
        "--check-context <CHECK_CONTEXT>",
        "--dry-run",
    ] {
        assert!(
            unwrapped(&setup).contains(required),
            "missing {required}: {setup}"
        );
    }
    assert!(
        !setup.contains("[env:"),
        "the target must be explicit: {setup}"
    );
    let schedule = help(&["ops", "forgejo", "auto-merge", "--help"]);
    for required in [
        "<REPOSITORY>",
        "--url <URL>",
        "--check-context <CHECK_CONTEXT>",
        "--pull-request <PULL_REQUEST>",
    ] {
        assert!(
            unwrapped(&schedule).contains(required),
            "missing {required}: {schedule}"
        );
    }
    let schedule_open = help(&["ops", "forgejo", "schedule-open", "--help"]);
    for required in [
        "<REPOSITORY>",
        "--url <URL>",
        "--check-context <CHECK_CONTEXT>",
        "FORGEJO_TOKEN",
        "trusted operator host",
    ] {
        assert!(
            unwrapped(&schedule_open).contains(required),
            "missing {required}: {schedule_open}"
        );
    }
    assert!(
        !schedule_open.contains("[env:"),
        "repository, URL and context must be explicit: {schedule_open}"
    );
}

#[test]
fn forgejo_runner_help_names_explicit_isolation_and_recovery_controls() {
    let runner = help(&["ops", "forgejo", "runner-once", "--help"]);
    for required in [
        "<REPOSITORY>",
        "--url <URL>",
        "--context <CONTEXT>",
        "--runner-namespace <RUNNER_NAMESPACE>",
        "--egress-ip <EGRESS_IP>",
        "--dns-ip <DNS_IP>",
        "--cleanup-registration <CLEANUP_REGISTRATION>",
        "--cleanup-job <CLEANUP_JOB>",
        "FORGEJO_TOKEN",
        "trusted operator host",
    ] {
        assert!(
            unwrapped(&runner).contains(required),
            "missing {required}: {runner}"
        );
    }
    assert!(
        !runner.contains("[env:"),
        "runner targets and network destinations must be explicit: {runner}"
    );
}

#[test]
fn ops_gcp_help_lists_the_hub_alongside_the_environment_provisioner() {
    let output = help(&["ops", "gcp", "--help"]);

    assert_eq!(
        command_names(&output),
        vec![
            // The two provisioners, widest blast radius first: an environment,
            // then the shared registry.
            "hub", "iap", "setup", "help",
        ]
    );
}

#[test]
fn ops_gcp_hub_help_uses_a_headline() {
    let output = unwrapped(&help(&["ops", "gcp", "hub", "--help"]));

    assert!(output.contains("Provision the shared image hub."));
}

#[test]
fn ops_gcp_hub_setup_help_lists_its_flags() {
    Command::cargo_bin("navigator")
        .unwrap()
        .args(["ops", "gcp", "hub", "setup", "--help"])
        .assert()
        .success()
        .stdout(str::contains("--project-id"))
        .stdout(str::contains("--artifact-registry-repo"))
        .stdout(str::contains("--github-repo"))
        .stdout(str::contains("--ci-pusher-account-id"))
        .stdout(str::contains("--dry-run"))
        // Environment-only resources must be unreachable from this command.
        .stdout(str::contains("--cluster-name").not())
        .stdout(str::contains("--public-base-url").not());
}

#[test]
fn ops_gcp_setup_help_exposes_the_hub_registry_flag() {
    Command::cargo_bin("navigator")
        .unwrap()
        .args(["ops", "gcp", "setup", "--help"])
        .assert()
        .success()
        .stdout(str::contains("--images-project-id"));
}

/// The hidden `up` alias for `dev up` is gone. It cost nothing in `--help`,
/// but it was a second top-level spelling for a command that already has one,
/// and a surface that is exactly six entries cannot also carry a seventh only
/// some people know about. `dev up` is the spelling.
#[test]
fn the_hidden_up_alias_no_longer_parses() {
    Command::cargo_bin("navigator")
        .unwrap()
        .args(["up", "--help"])
        .assert()
        .failure()
        .stderr(str::contains("unrecognized subcommand"));

    Command::cargo_bin("navigator")
        .unwrap()
        .args(["dev", "up", "--help"])
        .assert()
        .success();
}

#[test]
fn notation_create_help_lists_template_and_client_flags() {
    Command::cargo_bin("navigator")
        .unwrap()
        .args(["site", "notation", "create", "--help"])
        .assert()
        .success()
        .stdout(str::contains("Usage: navigator site notation create"))
        .stdout(str::contains("<TEMPLATE_CODE>"))
        .stdout(str::contains("--client-email"))
        .stdout(str::contains("--project"));
}

/// `--entity-name` is genuinely optional on `navigator project create`
/// (ENG-473): omitting it opens the matter against a `Human` entity created
/// for the client instead, which then requires `--jurisdiction` — asserted
/// directly against the live command rather than a doc-comment scan, since
/// the flag's optionality is now real clap behavior, not a syntactic
/// loosening the prose has to narrow back down.
#[test]
fn projects_create_requires_a_jurisdiction_when_entity_name_is_omitted() {
    Command::cargo_bin("navigator")
        .unwrap()
        .args([
            "project",
            "create",
            "--name",
            "No Entity",
            "--code",
            "no-entity",
            "--client-email",
            "client@example.com",
            "--attest",
        ])
        .assert()
        .failure()
        .stderr(str::contains("--jurisdiction"));
}

/// `notation kinds` prints both tables `S103` and `N109` validate against,
/// so an author reads the frame a `kind:` gets without opening `rules`.
#[test]
fn notation_kinds_lists_every_kind_and_output_value() {
    let output = help(&["notation", "kinds"]);

    assert!(output.contains("kind: values"), "{output}");
    assert!(output.contains("output: values"), "{output}");
    assert!(output.contains("client_letter"), "{output}");
}

/// Run `navigator` the way a fresh clone does: no inherited environment, so
/// no cloud project, SOPS key, registry login, or token, and a `PATH` that
/// holds only the base system tools.
fn fresh_clone_run(args: &[&str]) -> (bool, String) {
    let output = Command::cargo_bin("navigator")
        .unwrap()
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .args(args)
        .output()
        .expect("run navigator");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// `docs/self-hosting.md` and `AGENTS.md` document `dev up --path` and
/// `dev down --path`. A directory that is not a checkout is refused by name,
/// before any tool or credential is consulted.
#[test]
fn dev_up_and_down_take_the_documented_path() {
    let empty = tempfile::tempdir().expect("temp dir");
    let path = empty.path().to_str().expect("utf-8 temp path");
    for verb in ["up", "down"] {
        let (ok, stderr) = fresh_clone_run(&["dev", verb, "--path", path]);
        assert!(!ok, "`dev {verb} --path` on an empty directory must fail");
        assert!(
            stderr.contains("is not a Navigator checkout"),
            "`dev {verb} --path` must name the refused directory, got:\n{stderr}"
        );
    }
}

/// From a real checkout with no private credentials and no cluster tools, the
/// documented bootstrap stops at the first missing prerequisite, named as
/// `docs/self-hosting.md` lists it, and never asks for a cloud credential.
#[test]
fn dev_up_from_a_fresh_clone_needs_only_public_tools() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root is cli/'s parent");
    let (ok, stderr) = fresh_clone_run(&["dev", "up", "--path", root.to_str().unwrap()]);
    assert!(!ok, "`dev up` without `kind` on PATH must fail");
    assert!(
        stderr.contains("required tool not on PATH: kind"),
        "the first missing prerequisite is `kind`, got:\n{stderr}"
    );
    for private in [
        "gcloud",
        "sops",
        "keychain",
        "Artifact Registry",
        "GITHUB_TOKEN",
    ] {
        assert!(
            !stderr.contains(private),
            "the fresh-clone bootstrap must not ask for {private}, got:\n{stderr}"
        );
    }
}

//! Verify the dual MIT / Apache-2.0 instruments, distributed metadata, contribution
//! terms, security reporting, and the separation of third-party rights and trademarks.

use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

const LICENSE: &str = "MIT OR Apache-2.0";
const LICENSE_FILES: &[&str] = &["LICENSE", "LICENSE-APACHE-2", "LICENSE-MIT"];
const NOTICE_FILE: &str = "NOTICE";
/// Directories that carry their own `LICENSE` and license their manifests
/// under it instead of the workspace license of record. `images/postgres` is
/// the Apache-2.0 PostgreSQL image, whose DBOS smoke application is a
/// standalone Cargo project outside the workspace.
const SEPARATELY_LICENSED: &[(&str, &str)] = &[("images/postgres/", "Apache-2.0")];
const OWNER: &str = "Neon Law IP LLC";
const REGISTRANT: &str = "Neon Law IP LLC";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn read(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn repository_relative_path(path: &Path) -> String {
    path.strip_prefix(repo_root())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
        .replace("../", "")
}

fn flat_lower(body: &str) -> String {
    body.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn unemphasized(body: &str) -> String {
    flat_lower(body).replace(['*', '_'], "")
}

fn retired_org_name() -> String {
    ["Neon", "Law", "Foundation"].join(" ")
}

fn retired_org_handle() -> String {
    ["neon", "law", "foundation"].join("-")
}

fn window(body: &str, at: usize, len: usize) -> &str {
    let mut start = at.saturating_sub(200);
    while start > 0 && !body.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (at + len + 200).min(body.len());
    while end < body.len() && !body.is_char_boundary(end) {
        end += 1;
    }
    &body[start..end]
}

fn is_skipped_dir(name: &str) -> bool {
    matches!(
        name,
        "target" | ".git" | "node_modules" | "vendor" | ".devx"
    ) || name.trim_start_matches('.') == "worktrees"
}

fn cargo_manifests() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if !is_skipped_dir(name.as_ref()) {
                    walk(&path, out);
                }
            } else if name == "Cargo.toml" {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(&repo_root(), &mut out);
    out.sort();
    out
}

fn walk_repo_files(keep: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_files_under(&repo_root(), &mut out, &keep);
    out.sort();
    out
}

fn walk_files_under(dir: &Path, out: &mut Vec<PathBuf>, keep: &impl Fn(&str) -> bool) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if !is_skipped_dir(name.as_ref()) {
                walk_files_under(&path, out, keep);
            }
        } else if keep(name.as_ref()) {
            out.push(path);
        }
    }
}

#[test]
fn repository_relative_paths_compare_consistently_across_separators() {
    let expected = "docs/licensing.md";
    for path in [PathBuf::from(expected), PathBuf::from(r"docs\licensing.md")] {
        assert_eq!(repository_relative_path(&path), expected);
    }
}

#[test]
fn every_crate_declares_or_inherits_the_license_of_record() {
    let manifests = cargo_manifests();
    assert!(
        manifests.len() > 20,
        "expected the whole workspace, found only {} manifests — the walk is \
         probably rooted wrong",
        manifests.len()
    );

    let mut offenders = Vec::new();
    for path in &manifests {
        let body =
            fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let doc: toml::Value =
            toml::from_str(&body).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));

        let package = doc
            .get("workspace")
            .and_then(|w| w.get("package"))
            .or_else(|| doc.get("package"));
        let Some(package) = package else {
            continue;
        };
        let Some(license) = package.get("license") else {
            offenders.push(format!("{}: no `license` field", path.display()));
            continue;
        };
        let inherits = license
            .get("workspace")
            .and_then(toml::Value::as_bool)
            .unwrap_or(false);
        if inherits {
            continue;
        }
        let relative = repository_relative_path(path);
        if let Some((dir, expected)) = SEPARATELY_LICENSED
            .iter()
            .find(|(dir, _)| relative.starts_with(dir))
        {
            if license.as_str() != Some(expected) {
                offenders.push(format!(
                    "{}: license is {:?}, expected {expected:?}, the license of {dir}",
                    path.display(),
                    license.as_str()
                ));
            }
            continue;
        }
        match license.as_str() {
            Some(LICENSE) => {}
            other => offenders.push(format!(
                "{}: license is {other:?}, expected {LICENSE:?} or \
                 `license.workspace = true`",
                path.display()
            )),
        }
    }

    assert!(
        offenders.is_empty(),
        "these manifests do not carry the workspace license of record:\n  {}",
        offenders.join("\n  ")
    );
}

#[test]
fn workspace_root_pins_the_license_of_record() {
    let doc: toml::Value = toml::from_str(&read("Cargo.toml")).expect("parse workspace Cargo.toml");
    let license = doc["workspace"]["package"]["license"]
        .as_str()
        .expect("workspace license is a string");
    assert_eq!(
        license, LICENSE,
        "the workspace license of record must stay `{LICENSE}`; every member \
         crate inherits this value"
    );
}

#[test]
fn editor_extension_manifest_declares_the_license_of_record() {
    let vscode: serde_json::Value =
        serde_json::from_str(&read("lsp/vscode-ext/package.json")).expect("parse package.json");
    assert_eq!(
        vscode["license"].as_str(),
        Some(LICENSE),
        "the VS Code extension manifest must declare `{LICENSE}`"
    );
}

#[test]
fn the_repository_root_carries_exactly_one_licence_file() {
    let mut found: Vec<String> = fs::read_dir(repo_root())
        .expect("read repository root")
        .flatten()
        .filter(|entry| entry.path().is_file())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| {
            let lower = name.to_lowercase();
            ["license", "licence", "copying"]
                .iter()
                .any(|stem| lower.starts_with(stem))
        })
        .collect();
    found.sort();

    assert_eq!(
        found,
        LICENSE_FILES
            .iter()
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>(),
        "the root must carry LICENSE plus LICENSE-MIT and LICENSE-APACHE-2; found \
         {found:?}. LICENSE states the dual grant, and the other two files are the \
         unmodified instruments."
    );
}

#[test]
fn every_published_image_declares_the_licence_and_stages_its_text() {
    let images = repo_root().join("images");
    let mut offenders = Vec::new();

    for entry in fs::read_dir(&images).expect("read images/").flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("Containerfile.") || name == "Containerfile.runner" {
            continue;
        }
        let body = fs::read_to_string(&path).expect("read Containerfile");

        for required in [
            &format!("org.opencontainers.image.licenses=\"{LICENSE}\"") as &str,
            "COPY LICENSE /app/LICENSE",
            "COPY LICENSE-MIT /app/LICENSE-MIT",
            "COPY LICENSE-APACHE-2 /app/LICENSE-APACHE-2",
            &format!("COPY {NOTICE_FILE}  /app/{NOTICE_FILE}") as &str,
        ] {
            if !body.contains(required) {
                offenders.push(format!("{name}: missing `{required}`"));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "every published image must declare {LICENSE} and carry its license \
         files; a puller holds no repository and no archive:\n  {}",
        offenders.join("\n  ")
    );
}

#[test]
fn the_image_push_is_unconditional_and_scoped_to_the_publish_jobs() {
    let workflow = read(".github/workflows/deploy.yml");

    for required in [
        "registry: ${{ secrets.NAVIGATOR_REGISTRY_HOST }}",
        "username: oauth2accesstoken",
        // Pinned to a full commit SHA, with the major in a trailing comment.
        "uses: google-github-actions/auth@",
        "# v3",
        "service_account: ${{ secrets.NAVIGATOR_PUBLISHER_SERVICE_ACCOUNT }}",
    ] {
        assert!(
            workflow.contains(required),
            "deploy.yml must retain the keyless Artifact Registry push contract `{required}`"
        );
    }
    for forbidden in ["credentials_json", "-docker.pkg.dev/"] {
        assert!(
            !workflow.contains(forbidden),
            "deploy.yml must not carry `{forbidden}`: private registry coordinates stay in secrets"
        );
    }

    // No toggle, under any spelling. `vars.` is the whole namespace a repository
    // variable can be read through, so excluding it from the publish jobs
    // catches a gate reintroduced under a different variable name too.
    //
    // Scoped to those jobs rather than the file: `notify` and `release-deploy`
    // read `vars.DEPLOY_AUTOMATION_ENABLED` legitimately. Dispatching a rollout
    // is not publishing it.
    let parsed: serde_yaml::Value =
        serde_yaml::from_str(&workflow).expect("deploy.yml parses as YAML");
    for job in ["publish-service", "publish-triggers"] {
        // Re-serialised from the parse so YAML comments are not part of the
        // text searched: the prose in this file necessarily names the toggle it
        // forbids, and the job's CONFIGURATION is what must not carry it.
        let body = serde_yaml::to_string(&parsed["jobs"][job])
            .unwrap_or_else(|error| panic!("`{job}` re-serialises: {error}"));
        assert!(
            !body.contains("vars."),
            "`{job}` must read no repository variable: publishing must not depend on one, \
             whose absence is a silent, file-free way to stop a release reaching the registry"
        );
    }
    assert!(
        !workflow.contains("GHCR_PUBLISH"),
        "the registry push carries no toggle; `GHCR_PUBLISH` must not return in any form"
    );

    // The image list must be composed unconditionally. An `if` inside the
    // heredoc is how a toggle would be spelled, and the heredoc decides what
    // `metadata-action` is handed.
    let composed = workflow
        .split("- name: compose registry list")
        .nth(1)
        .expect("deploy.yml must keep its `compose registry list` step")
        .split("- name: image metadata")
        .next()
        .expect("the composed list is followed by the metadata step");
    assert!(
        !composed.contains("if ["),
        "the registry list must be composed unconditionally; a conditional branch here is how \
         a publish is silently narrowed to nothing"
    );

    // The scope must be granted, and granted narrowly. A `permissions:` block
    // at column 2 is the workflow's; at column 4 it is a job's.
    let granted: Vec<&str> = workflow
        .lines()
        .filter(|line| line.trim() == "id-token: write")
        .collect();
    // The service, trigger, and CLI image publishers hold the scope. What must
    // not change is that each grant is a JOB's —
    // the count is allowed to track the publish jobs, the indent is not.
    assert_eq!(
        granted.len(),
        3,
        "`id-token: write` is expected only on the three image publish jobs"
    );
    for grant in &granted {
        assert!(
            grant.starts_with("      id-token: write"),
            "`id-token: write` must be granted at job level (six-space indent), \
             not at the top of the workflow where every job would inherit it"
        );
    }

    // One build, every tag. `metadata-action` fans the tags across every image
    // it is given, so the push step must read the composed list rather than a
    // single name — a second push step would mean a second full compile of
    // identical layers.
    assert!(
        workflow.contains("images: ${{ steps.imgs.outputs.list }}"),
        "the publish job's metadata step must read the composed registry list, so one build \
         serves every tag it publishes"
    );
}

#[test]
fn trademark_notices_name_the_firm_as_the_registrant() {
    /// The registration itself, used as the anchor for "this line claims
    /// ownership" rather than merely naming the brand.
    const REGISTRATION: &str = "6,325,650";

    let mut offenders = Vec::new();
    for rel in [
        NOTICE_FILE,
        "README.md",
        "docs/glossary/neon-law-navigator.md",
        // Where the ownership claim actually lives. `NOTICE` names the mark but
        // not every surface does, so this is the doc that makes the numbered
        // claim the licence deliberately does not grant.
        "docs/licensing.md",
        "templates/README.md",
    ] {
        // Prose wraps at the Markdown line width, so a claim routinely straddles
        // a line break, and "U.S. Reg. No." defeats splitting on sentence ends.
        // Collapse whitespace and read a window around each citation instead.
        let flat = read(rel).split_whitespace().collect::<Vec<_>>().join(" ");
        let citations: Vec<usize> = flat.match_indices(REGISTRATION).map(|(i, _)| i).collect();
        assert!(
            !citations.is_empty(),
            "{rel} cites the NEON LAW registration and is guarded here; if the \
             citation moved, move this list with it"
        );
        for at in citations {
            let claim = window(&flat, at, REGISTRATION.len());
            if !claim.contains(REGISTRANT) {
                offenders.push(format!(
                    "{rel}: cites {REGISTRATION} without naming `{REGISTRANT}` — …{claim}…"
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "NEON LAW is registered to {REGISTRANT}; these notices say otherwise:\n  {}",
        offenders.join("\n  ")
    );
}

#[test]
fn no_file_contains_the_retired_org_display_name() {
    let needles = [
        ("display name", retired_org_name().to_lowercase()),
        ("repository handle", retired_org_handle()),
    ];
    let mut offenders = Vec::new();
    for path in walk_repo_files(|_| true) {
        let Ok(body) = fs::read_to_string(&path) else {
            continue;
        };
        let body = body.to_lowercase();
        for (label, needle) in &needles {
            if body.contains(needle) {
                let rel = path
                    .strip_prefix(repo_root())
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace("../", "");
                offenders.push(format!("{rel}: retired {label}"));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the retired nonprofit's display name and repository handle are gone \
         from the tree; these files still contain one:\n  {}",
        offenders.join("\n  ")
    );
}

#[test]
fn release_automation_wif_uses_the_configured_repository_gate() {
    let body = read("docs/release-automation.md");
    let flat = unemphasized(&body);
    assert!(
        body.contains("Set the repository variable `DEPLOY_EXPECTED_REPOSITORY`")
            && body.contains("assertion.repository == '<owner>/<repository>'")
            && body.contains("assertion.environment == '<github-environment>'")
            && body.contains("assertion.ref == 'refs/heads/main'")
            && body.contains("Providers accept tokens only from the expected repository"),
        "docs/release-automation.md must bind Workload Identity to the configured expected repository"
    );
    assert!(
        flat.contains("do not reuse an unscoped provider."),
        "docs/release-automation.md must still name the unscoped provider an \
         operator must not reuse"
    );
    assert!(
        flat.contains("deployment environment on `main`"),
        "docs/release-automation.md must describe the environment and branch scope"
    );
}

#[test]
fn the_firm_entity_of_record_is_one_string_across_brand_seed_and_entity_row() {
    #[derive(serde::Deserialize)]
    struct Seed {
        records: Vec<Record>,
    }
    #[derive(serde::Deserialize)]
    struct Record {
        name: String,
    }
    let firm = store::seed::FIRM_ENTITY_NAME;
    assert!(
        !firm.trim().is_empty(),
        "the firm entity of record must be named"
    );

    for key in views::brand::BrandKey::ALL {
        let branding = key.resolve_branding(&views::brand::DEFAULT_BRANDING);
        assert_eq!(
            branding.firm.legal_entity,
            firm,
            "brand `{}` names a legal entity other than `store::seed::FIRM_ENTITY_NAME`; \
             the footer, the retainer, and the delete-guarded Entity row must name one \
             legal person",
            key.as_str()
        );
    }

    let seed: Seed = serde_yaml::from_str(store::seed::ENTITY_SEED_YAML)
        .expect("Entity.yaml parses as lookup_fields plus records");
    let names: Vec<&str> = seed.records.iter().map(|r| r.name.as_str()).collect();
    assert!(
        names.contains(&firm),
        "store/seeds/Entity.yaml has no record named `{firm}`; the seed looks the firm \
         row up by this exact name on every boot, so the row the delete guard protects \
         would never exist. Records: {names:?}"
    );
}

#[test]
fn the_walk_skips_generated_local_state() {
    for generated in [".devx", "target", ".git", "node_modules", "vendor"] {
        assert!(
            is_skipped_dir(generated),
            "`{generated}` is generated local state, so walking into it reports \
             files this repository does not own"
        );
    }
    for owned in ["cli", "rules", "docs", "templates", "store"] {
        assert!(
            !is_skipped_dir(owned),
            "`{owned}` is this workspace's own surface and must still be walked"
        );
    }
}

#[test]
fn the_walker_ignores_files_under_a_skipped_directory() {
    let fixture = tempfile::tempdir().expect("create fixture tree");
    let root = fixture.path();

    let generated = root
        .join(".devx")
        .join("sample-projects")
        .join("acme")
        .join("dist")
        .join("assets");
    fs::create_dir_all(&generated).expect("create generated tree");
    fs::write(
        generated.join("index-abc123.css"),
        "generated bundle output",
    )
    .expect("write generated file");

    let owned = root.join("cli").join("tests");
    fs::create_dir_all(&owned).expect("create owned tree");
    fs::write(
        owned.join("license_of_record.rs"),
        "owned repository source",
    )
    .expect("write owned file");

    let mut found = Vec::new();
    walk_files_under(root, &mut found, &|_name: &str| true);
    found.sort();

    let names: Vec<String> = found
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().to_string())
        .collect();

    assert_eq!(
        names,
        vec!["license_of_record.rs".to_string()],
        "the walker must skip every file under `.devx` while still reporting \
         files under an owned directory; found {names:?}"
    );
}

/// SHA-256 of <https://www.apache.org/licenses/LICENSE-2.0.txt>. This guards the
/// whole instrument, including sections 7–9, against extra terms or accidental
/// edits to the standard license.
#[test]
fn the_dual_grant_points_at_unmodified_mit_and_apache_texts() {
    let choice = read("LICENSE");
    let flat = flat_lower(&choice);
    assert!(flat.contains("mit license"));
    assert!(flat.contains("apache"));
    assert!(flat.contains("at your option"));
    assert!(choice.contains("LICENSE-MIT"));
    assert!(choice.contains("LICENSE-APACHE-2"));
    assert!(
        !choice.contains("TERMS AND CONDITIONS"),
        "LICENSE states the choice; the Apache instrument stays in LICENSE-APACHE-2"
    );

    let apache = read("LICENSE-APACHE-2");
    assert!(apache.contains("on an \"AS IS\" BASIS"));
    assert_eq!(
        Sha256::digest(apache.as_bytes()),
        [
            0xcf, 0xc7, 0x74, 0x9b, 0x96, 0xf6, 0x3b, 0xd3, 0x1c, 0x3c, 0x42, 0xb5, 0xc4, 0x71,
            0xbf, 0x75, 0x68, 0x14, 0x05, 0x3e, 0x84, 0x7c, 0x10, 0xf3, 0xeb, 0x00, 0x34, 0x17,
            0xbc, 0x52, 0x3d, 0x30
        ]
    );

    let mit = read("LICENSE-MIT");
    assert!(mit.contains("Permission is hereby granted, free of charge"));
    assert!(mit.contains("Neon Law IP LLC"));
    assert!(mit.contains("THE SOFTWARE IS PROVIDED \"AS IS\""));
}

#[test]
fn first_party_notice_and_distribution_docs_agree_with_metadata() {
    for rel in [
        NOTICE_FILE,
        "README.md",
        "docs/licensing.md",
        "templates/README.md",
    ] {
        let body = read(rel);
        assert!(
            flat_lower(&body).contains(&LICENSE.to_lowercase()),
            "{rel} must name {LICENSE}"
        );
        assert!(
            body.contains(OWNER),
            "{rel} must preserve first-party attribution"
        );
        assert!(
            !body.contains("BUSL-1.1"),
            "{rel} must state the current grant"
        );
    }
    let notices = read("THIRD-PARTY-NOTICES.txt");
    assert!(notices.lines().take(10).any(|line| line.contains(LICENSE)));
    assert!(notices.contains("MIT License"));
}

#[test]
fn contributions_use_the_same_license_without_assignment_or_a_cla() {
    for rel in ["CONTRIBUTING.md", "CONTRIBUTORS.md"] {
        let body = read(rel);
        let flat = flat_lower(&body);
        assert!(
            flat_lower(&body).contains(&LICENSE.to_lowercase()),
            "{rel} must name contribution terms"
        );
        assert!(
            flat.contains("retain"),
            "{rel} must preserve contributor ownership"
        );
        assert!(
            flat.contains("no separate cla"),
            "{rel} must state the contribution process"
        );
        assert!(
            !flat.contains("assigns to"),
            "{rel} must not invent an assignment"
        );
    }
    let terms = flat_lower(&read("CONTRIBUTORS.md"));
    assert!(terms.contains("section 5"));
    assert!(terms.contains("not a requirement"));
    assert!(terms.contains("a merge does not represent"));
    assert!(terms.contains("do not invent copyright"));
    assert!(terms.contains("third-party"));
}

#[test]
fn security_reports_have_a_private_route_and_safe_reproduction_guidance() {
    let body = read("SECURITY.md");
    assert!(body.contains("mailto:support@neonlaw.com"));
    let flat = flat_lower(&body);
    assert!(flat.contains("synthetic"));
    assert!(flat.contains("public issue"));
    assert!(flat.contains("client data"));
    assert!(read("CONTRIBUTING.md").contains("SECURITY.md"));
}

#[test]
fn warranty_guidance_points_to_the_license_without_promising_immunity() {
    let guide = flat_lower(&read("docs/licensing.md"));
    for required in [
        "section 7",
        "section 8",
        "section 9",
        "applicable law",
        "ai",
        "recommended, not required",
    ] {
        assert!(
            guide.contains(required),
            "licensing guide must cover {required}"
        );
    }
}

#[test]
fn font_and_vendored_terms_remain_separate_from_the_project_grant() {
    assert!(read("vendor/sendgrid/LICENSE").contains("MIT License"));
    let gorp = read("server/public/fonts/gorp-serif/LICENSE.txt");
    assert!(gorp.contains("Copyright TrashType LLC"));
    assert!(gorp.contains("proprietary font software"));
    assert!(gorp.contains(LICENSE));
    for rel in [
        "pdf/assets/fonts/Tinos/OFL.txt",
        "pdf/assets/fonts/NotoSerif/OFL.txt",
    ] {
        assert!(read(rel).contains("SIL OPEN FONT LICENSE Version 1.1"));
    }
}

//! The canonical Project repository scaffold, as bytes.
//!
//! One source of truth for the files a newly opened Project's repository
//! carries, so the two writers of that tree can never fork:
//!
//! * the CLI's `navigator project scaffold`, which writes the tree to disk
//!   (`cli/src/projects/repository.rs`'s `emit_scaffold_for`), and
//! * the server-side repository seeding, which commits the same file set as
//!   a Project repository's first commit through
//!   `ForgeService::initialize_source` (`cloud/src/forge.rs`).
//!
//! The bytes that are fixed for a release — the synced Agent Skills, the
//! root `.gitignore`, the Forgejo gate workflow — are embedded here. The
//! bytes that name the Project and its deployment are rendered from
//! [`ScaffoldSource`] by [`canonical_files`]. The Forgejo-backed seed set —
//! everything the server commits as a Project repository's first commit —
//! is [`forgejo_seed_files`].
//!
//! Callers validate the Project code themselves: the CLI's scaffold refuses
//! an invalid code through `store::projects::is_valid_code`, and the crate
//! stays a leaf below `store`.

/// The Project manifest every scaffold carries, at the repository root.
pub const PROJECT_MANIFEST: &str = "navigator.yaml";

/// The lifecycle kind of the letter Draft a scaffold seeds.
pub const ONBOARDING: &str = "onboarding";

/// The lifecycle kind of the offboarding letter Draft.
pub const OFFBOARDING: &str = "offboarding";

/// The git-ignored working folders of a Project repository. Document bytes
/// staged for or pulled down by `navigator project sync` live under
/// `/documents/`; each document's pointer is committed under blobs/ and its
/// bytes never are.
pub const ROOT_GITIGNORE: &str = "# Document bytes staged for or pulled down by `navigator project sync`.\n# Each document's pointer is committed under blobs/; its bytes never are.\n/documents/\n\n# Legal files and handoff notes never enter a Project repository.\n*.pdf\n*.docx\n*.local\n\n# Call notes and memos for `navigator project communication create --body-file`.\n# The Communication's body lives in Navigator's store, never in Git.\nnotes/\n\n# Email bodies, drafts, and saved copies of sent mail. Matter facts, never Git.\nmail/\n\n# Per-machine agent and operating-system state.\n.claude/settings.local.json\n.claude/worktrees/\n.agents/settings.local.json\n.agents/worktrees/\n.DS_Store\n";

/// The release-download base the generated Forgejo gate installs the pinned
/// `navigator` release from. It lives here — not beside its `lsp_download`
/// consumer — because the gate workflow embeds it, and the gate's bytes must
/// be identical whether the CLI wrote them to disk or the server committed
/// them.
pub const RELEASE_DOWNLOAD_BASE: &str = "https://github.com/neon-law/navigator/releases/download";

/// The gate workflow a Forgejo-backed Project repository carries. Forgejo
/// Actions reads `.forgejo/workflows/` and ignores `.github/workflows/` when
/// it exists, so `ci.yml` and `cd.yml` stay for GitHub and this file is the
/// whole Forgejo gate. The deployment's `NAVIGATOR_FORGEJO_ACTIONS_OIDC_WORKFLOW`
/// names this exact path: the Forgejo document-token door trusts no other.
pub const FORGEJO_GATE_WORKFLOW: &str = ".forgejo/workflows/gate.yml";

/// The commit-status context [`forgejo_gate_workflow`] posts, which `main`'s
/// branch protection requires: `<workflow name> / <job> (<event>)`.
pub const FORGEJO_REQUIRED_CHECK: &str = "gate / gate (pull_request)";

/// The Agent Skills every Project repository carries a synced copy of. The
/// canonical copies live in Navigator's own `.agents/skills/`; a scaffold
/// writes and `sync-skills` restores exactly these bytes.
///
/// `case-timeline` (LAW-133) keeps a litigation case record as Docket Entries
/// through `navigator project docket`, never as a hand-edited memo.
/// `opposing-communication-review` gives every Project the recipient-side
/// review discipline for opposing-party correspondence. `pull-changes` is the
/// one meaning of "pull changes": rebase the branch onto the latest `origin/main`.
/// `push-changes` carries the outbound loop through a ready PR and verified merge.
/// `word-editing` (LAW-229) carries a lawyer's edits in local Word back into
/// the notation template, with the template kept the source of truth.
/// `analyze-memo` (LAW-237) reviews a finished memo or letter and reports
/// findings before any correction is applied.
pub const SYNCED_SKILLS: &[(&str, &str)] = &[
    (
        "council",
        include_str!("../../.agents/skills/council/SKILL.md"),
    ),
    (
        "legal-council",
        include_str!("../../.agents/skills/legal-council/SKILL.md"),
    ),
    (
        "analyze-memo",
        include_str!("../../.agents/skills/analyze-memo/SKILL.md"),
    ),
    (
        "client-council",
        include_str!("../../.agents/skills/client-council/SKILL.md"),
    ),
    (
        "human-readable",
        include_str!("../../.agents/skills/human-readable/SKILL.md"),
    ),
    (
        "stay-in-repo",
        include_str!("../../.agents/skills/stay-in-repo/SKILL.md"),
    ),
    (
        "legal-writing",
        include_str!("../../.agents/skills/legal-writing/SKILL.md"),
    ),
    (
        "case-downloader",
        include_str!("../../.agents/skills/case-downloader/SKILL.md"),
    ),
    (
        "project-setup",
        include_str!("../../.agents/skills/project-setup/SKILL.md"),
    ),
    (
        "deadline",
        include_str!("../../.agents/skills/deadline/SKILL.md"),
    ),
    (
        "browser-handoff",
        include_str!("../../.agents/skills/browser-handoff/SKILL.md"),
    ),
    (
        "case-timeline",
        include_str!("../../.agents/skills/case-timeline/SKILL.md"),
    ),
    (
        "opposing-communication-review",
        include_str!("../../.agents/skills/opposing-communication-review/SKILL.md"),
    ),
    (
        "record-search",
        include_str!("../../.agents/skills/record-search/SKILL.md"),
    ),
    (
        "working-notes",
        include_str!("../../.agents/skills/working-notes/SKILL.md"),
    ),
    (
        "pull-changes",
        include_str!("../../.agents/skills/pull-changes/SKILL.md"),
    ),
    (
        "push-changes",
        include_str!("../../.agents/skills/push-changes/SKILL.md"),
    ),
    (
        "word-editing",
        include_str!("../../.agents/skills/word-editing/SKILL.md"),
    ),
];

/// What the generated scaffold bytes name: the Project's coordinates and the
/// release version the manifest pins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaffoldSource<'a> {
    /// The immutable Project code; the manifest's `project.name`.
    pub code: &'a str,
    /// The deployment host the manifest pins.
    pub host: &'a str,
    /// The opening lawyer's DRI (email).
    pub lawyer_dri: &'a str,
    /// The client DRI (email).
    pub client_dri: &'a str,
    /// The release version pinned in the manifest, which the Forgejo gate
    /// reads to install the matching release.
    pub version: &'a str,
}

/// The forge-agnostic scaffold files: the root `README.md`, the root
/// `.gitignore`, the Project manifest, and the onboarding letter Draft
/// placeholder. The synced Agent Skills are [`synced_skill_files`]; the
/// Forgejo gate is [`forgejo_gate_workflow`].
///
/// Repository-relative paths paired with exact bytes.
#[must_use]
pub fn canonical_files(source: &ScaffoldSource) -> Vec<(String, String)> {
    let (draft, stub) = scaffold_draft(ONBOARDING);
    vec![
        (
            "README.md".to_string(),
            format!(
                "# Project repository\n\nNavigator Project `{}`.\n",
                source.code
            ),
        ),
        (".gitignore".to_string(), ROOT_GITIGNORE.to_string()),
        (PROJECT_MANIFEST.to_string(), navigator_manifest(source)),
        (draft, stub),
    ]
}

/// The Project manifest's exact bytes for `source`: the release version plus
/// the Project coordinates provisioning recorded.
#[must_use]
pub fn navigator_manifest(source: &ScaffoldSource) -> String {
    format!(
        "version: {}\nproject:\n  host: {}\n  name: {}\n  lawyer_dri: {}\n  client_dri: {}\n  private_slack_channel: not provisioned\n  private_notion_page: not provisioned\n",
        source.version, source.host, source.code, source.lawyer_dri, source.client_dri
    )
}

/// The synced Agent Skills: `.agents/skills/<name>/SKILL.md` paired with the
/// canonical copy [`SYNCED_SKILLS`] embeds.
#[must_use]
pub fn synced_skill_files() -> Vec<(String, String)> {
    SYNCED_SKILLS
        .iter()
        .map(|(name, contents)| {
            (
                format!(".agents/skills/{name}/SKILL.md"),
                (*contents).to_string(),
            )
        })
        .collect()
}

/// The Forgejo gate: on every pull request, check out its head, install the
/// `navigator` release `navigator.yaml` pins, and run
/// `navigator project gate --ci`. `enable-openid-connect` makes the runner
/// export `ACTIONS_ID_TOKEN_REQUEST_*`; the CLI requests a token audienced to
/// the manifest's host and exchanges it at `/auth/ci/forgejo/document-token`.
///
/// Every step is plain shell, so the workflow needs no action repository the
/// Forgejo installation would have to mirror. The binary installs outside the
/// checkout, where the gate's closed root list would refuse it.
#[must_use]
pub fn forgejo_gate_workflow() -> String {
    format!(
        r#"# Generated by `navigator project scaffold`; `navigator project repository
# sync-skills` restores it. Posts the required check `{FORGEJO_REQUIRED_CHECK}`.
name: gate

on:
  pull_request:

jobs:
  gate:
    runs-on: self-hosted
    enable-openid-connect: true
    steps:
      - name: check out the pull request
        env:
          GIT_CONFIG_COUNT: "1"
          GIT_CONFIG_KEY_0: http.extraHeader
          GIT_CONFIG_VALUE_0: "Authorization: token ${{{{ forgejo.token }}}}"
          SERVER_URL: ${{{{ forgejo.server_url }}}}
          REPOSITORY: ${{{{ forgejo.repository }}}}
          REF: ${{{{ forgejo.ref }}}}
        run: |
          set -eu
          git init --quiet .
          git fetch --quiet --depth 1 "${{SERVER_URL%/}}/${{REPOSITORY}}.git" "${{REF}}"
          git checkout --quiet --force --detach FETCH_HEAD
          git clean --quiet -ffdx
      - name: install the pinned navigator release
        run: |
          set -eu
          version="$(sed -n 's/^version:[[:space:]]*//p' navigator.yaml | head -n 1 | tr -d "\"' \r")"
          if ! printf '%s\n' "${{version}}" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$'; then
            echo "::error::navigator.yaml version must be an exact release tag, not '${{version}}'."
            exit 1
          fi
          case "$(uname -s)" in
            Linux) platform=linux ;;
            Darwin) platform=macos ;;
            *) echo "::error::unsupported runner OS $(uname -s)"; exit 1 ;;
          esac
          asset="navigator-${{version}}-${{platform}}.tar.gz"
          release="{RELEASE_DOWNLOAD_BASE}/${{version}}"
          bin="$(mktemp -d)"
          cd "${{bin}}"
          for file in "${{asset}}" "${{asset}}.sha256"; do
            if command -v curl >/dev/null 2>&1; then
              curl -fsSL -o "${{file}}" "${{release}}/${{file}}"
            else
              wget -q -O "${{file}}" "${{release}}/${{file}}"
            fi
          done
          if command -v sha256sum >/dev/null 2>&1; then
            sha256sum -c "${{asset}}.sha256"
          else
            shasum -a 256 -c "${{asset}}.sha256"
          fi
          tar -xzf "${{asset}}" navigator
          chmod 0755 navigator
          echo "${{bin}}" >> "${{GITHUB_PATH}}"
      - name: navigator project gate
        run: navigator project gate --ci
"#
    )
}

/// Everything the server commits as a Forgejo-backed Project repository's
/// first commit: [`canonical_files`], the synced Agent Skills, and the
/// byte-exact [`FORGEJO_GATE_WORKFLOW`]. The CLI's scaffold emits these same
/// bytes plus GitHub-specific files (`AGENTS.md`, `.claude/settings.json`,
/// the thin `ci.yml`/`cd.yml` callers, `CODEOWNERS`, the licenses); a test
/// in the CLI crate asserts the seed set is byte-identical to that output.
#[must_use]
pub fn forgejo_seed_files(source: &ScaffoldSource) -> Vec<(String, String)> {
    canonical_files(source)
        .into_iter()
        .chain(synced_skill_files())
        .chain([(FORGEJO_GATE_WORKFLOW.to_string(), forgejo_gate_workflow())])
        .collect()
}

/// The stable digest binding a canonical file set to the commit that first
/// carries it: SHA-256 over each repository-relative path and its exact
/// bytes, taken in path order, each length-prefixed so no boundary between
/// a path and a body is ever ambiguous. The server seeds a Project
/// repository through `ForgeService::initialize_source` with this digest in
/// its `Source-Tree:` commit trailer; a retry that finds the same digest at
/// the repository's head writes nothing. The value must therefore depend on
/// the set and nothing else — not the caller, not the order, not the
/// platform.
#[must_use]
pub fn source_tree_digest(files: &[(String, String)]) -> String {
    use sha2::Digest;
    use std::fmt::Write as _;
    let mut ordered: Vec<(&String, &String)> =
        files.iter().map(|(path, body)| (path, body)).collect();
    ordered.sort_by_key(|(path, _)| *path);
    let mut hasher = sha2::Sha256::new();
    for (path, body) in ordered {
        hasher.update(u64::try_from(path.len()).unwrap_or(u64::MAX).to_le_bytes());
        hasher.update(path.as_bytes());
        hasher.update(u64::try_from(body.len()).unwrap_or(u64::MAX).to_le_bytes());
        hasher.update(body.as_bytes());
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// The repository-relative path and stub bytes of `kind`'s letter Draft, as
/// `repository scaffold` writes it into a new Project repository.
#[must_use]
pub fn scaffold_draft(kind: &str) -> (String, String) {
    (
        format!(
            "{}/{}.md",
            rules::project_blueprints::BlueprintDirectory::Drafts.as_str(),
            template_code(kind)
        ),
        template_stub(kind),
    )
}

/// The published letter template name `kind`'s Draft opens a Notation from.
#[must_use]
pub fn template_code(kind: &str) -> &str {
    match kind {
        ONBOARDING => "onboarding__letter",
        OFFBOARDING => "offboarding__letter",
        _ => kind,
    }
}

/// The placeholder letter a lifecycle command writes before the lawyer
/// drafts it. Opening a notation from these bytes is refused.
#[must_use]
pub fn template_stub(kind: &str) -> String {
    let title = if kind == ONBOARDING {
        "Onboarding letter"
    } else {
        "Offboarding letter"
    };
    let placeholder = template_placeholder(kind);
    // An onboarding letter always carries the engagement-arbitration Clause and
    // the two questions it binds; an offboarding letter has none.
    let (clauses, placement) = if kind == ONBOARDING {
        (
            "questionnaire:\n  BEGIN:\n    _: jurisdiction__governing_law\n  jurisdiction__governing_law:\n    _: jurisdiction__arbitration_seat\n  jurisdiction__arbitration_seat:\n    _: END\nclauses:\n  arbitration:\n    source: engagement_arbitration@1\n    inputs:\n      law: jurisdiction__governing_law\n      seat: jurisdiction__arbitration_seat\nworkflow:\n  BEGIN:\n    created: lawyer_review\n  lawyer_review:\n    approved: END\n",
            "{{> arbitration}}\n\n",
        )
    } else {
        ("", "")
    };
    format!(
        "---\nkind: {kind}\ntitle: {title}\nrespondent_type: person_and_entity\nconfidential: true\n{clauses}---\n\n{placement}{placeholder}\n"
    )
}

/// The inline hint telling the lawyer to draft the letter before rerunning
/// the lifecycle command. Also the placeholder marker `is_unedited_stub`
/// matches against.
#[must_use]
pub fn template_placeholder(kind: &str) -> String {
    let command = if kind == ONBOARDING {
        "onboard"
    } else {
        "offboard"
    };
    format!("Draft this letter before running `navigator project {command}` again.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn source() -> ScaffoldSource<'static> {
        ScaffoldSource {
            code: "matter-one",
            host: "staging.neonlaw.com",
            lawyer_dri: "lawyer@example.com",
            client_dri: "client@example.com",
            version: "26.10.29",
        }
    }

    #[test]
    fn canonical_files_carry_the_project_coordinates() {
        let files = canonical_files(&source());
        let read = |relative: &str| match files.iter().find(|(path, _)| path == relative) {
            Some((_, contents)) => contents.clone(),
            None => panic!("{relative} missing"),
        };
        assert_eq!(
            read("README.md"),
            "# Project repository\n\nNavigator Project `matter-one`.\n"
        );
        assert_eq!(read(".gitignore"), ROOT_GITIGNORE);
        let manifest = read(PROJECT_MANIFEST);
        let parsed: serde_yaml::Value = serde_yaml::from_str(&manifest).unwrap();
        assert_eq!(parsed["version"].as_str(), Some("26.10.29"));
        assert_eq!(
            parsed["project"]["host"].as_str(),
            Some("staging.neonlaw.com")
        );
        assert_eq!(parsed["project"]["name"].as_str(), Some("matter-one"));
        assert_eq!(
            parsed["project"]["lawyer_dri"].as_str(),
            Some("lawyer@example.com")
        );
        assert_eq!(
            parsed["project"]["client_dri"].as_str(),
            Some("client@example.com")
        );
        // The Draft placeholder is exactly the unedited stub lifecycle
        // commands recognize.
        assert_eq!(
            read("drafts/onboarding__letter.md"),
            template_stub(ONBOARDING)
        );
    }

    #[test]
    fn the_digest_depends_on_the_set_and_not_its_order() {
        let files = vec![
            ("b.md".to_string(), "second".to_string()),
            ("a.md".to_string(), "first".to_string()),
        ];
        let mut reordered = files.clone();
        reordered.reverse();
        assert_eq!(
            source_tree_digest(&files),
            source_tree_digest(&reordered),
            "the same set in a different order is the same source tree"
        );
    }

    #[test]
    fn a_changed_body_or_path_changes_the_digest() {
        let files = vec![("a.md".to_string(), "first".to_string())];
        let mut body_changed = files.clone();
        body_changed[0].1.push_str(" altered");
        assert_ne!(
            source_tree_digest(&files),
            source_tree_digest(&body_changed)
        );

        let mut path_changed = files.clone();
        path_changed[0].0 = "b.md".to_string();
        assert_ne!(
            source_tree_digest(&files),
            source_tree_digest(&path_changed)
        );
    }

    #[test]
    fn paths_are_unique_and_repository_relative() {
        let files = forgejo_seed_files(&source());
        let paths: BTreeSet<&String> = files.iter().map(|(path, _)| path).collect();
        assert_eq!(paths.len(), files.len(), "no duplicate paths: {paths:?}");
        assert!(paths.iter().all(|path| !path.starts_with('/')));
    }

    #[test]
    fn the_seed_set_is_canonical_plus_skills_plus_the_gate() {
        let source = source();
        let seeded = forgejo_seed_files(&source);
        let mut expected = canonical_files(&source);
        expected.extend(synced_skill_files());
        expected.push((FORGEJO_GATE_WORKFLOW.to_string(), forgejo_gate_workflow()));
        assert_eq!(seeded, expected);
        assert_eq!(
            seeded
                .iter()
                .filter(|(path, _)| path == FORGEJO_GATE_WORKFLOW)
                .count(),
            1
        );
    }

    #[test]
    fn every_synced_skill_is_a_canonical_copy_under_agents_skills() {
        for (relative, contents) in synced_skill_files() {
            assert!(relative.starts_with(".agents/skills/"), "{relative}");
            assert!(relative.ends_with("/SKILL.md"), "{relative}");
            assert!(contents.contains("name:"), "{relative}");
        }
        assert_eq!(synced_skill_files().len(), SYNCED_SKILLS.len());
    }

    #[test]
    fn the_gate_workflow_posts_the_required_check_with_plain_shell() {
        let workflow = forgejo_gate_workflow();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&workflow).unwrap();
        assert_eq!(parsed["name"].as_str(), Some("gate"));
        assert!(workflow.contains(FORGEJO_REQUIRED_CHECK));
        assert!(workflow.contains(RELEASE_DOWNLOAD_BASE));
        // Rendered, not templated: no Rust format braces survive.
        assert!(!workflow.contains("{{{"), "{workflow}");
        let steps = parsed["jobs"]["gate"]["steps"].as_sequence().unwrap();
        assert!(steps.iter().all(|step| step["uses"].is_null()));
        assert_eq!(
            steps.last().unwrap()["run"].as_str(),
            Some("navigator project gate --ci")
        );
    }

    #[test]
    fn the_root_gitignore_refuses_document_bytes_and_lifecycle_email() {
        for refused in ["/documents/", "*.pdf", "*.docx", "*.local", "mail/"] {
            assert!(ROOT_GITIGNORE.contains(refused), "{refused}");
        }
    }

    #[test]
    fn template_codes_map_lifecycle_kinds_to_published_letter_names() {
        assert_eq!(template_code(ONBOARDING), "onboarding__letter");
        assert_eq!(template_code(OFFBOARDING), "offboarding__letter");
        assert_eq!(template_code("engagement__letter"), "engagement__letter");
    }

    #[test]
    fn the_offboarding_stub_binds_no_arbitration_clause() {
        let onboarding = template_stub(ONBOARDING);
        let offboarding = template_stub(OFFBOARDING);
        assert!(onboarding.contains("engagement_arbitration@1"));
        assert!(onboarding.contains("kind: onboarding\n"));
        assert!(!offboarding.contains("clauses:"));
        assert!(offboarding.contains("kind: offboarding\n"));
        assert!(offboarding.contains("`navigator project offboard`"));
    }
}

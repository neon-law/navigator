//! One Project, one repository: layout and gate validation.
//!
//! A Project's repository is named for its Project code and holds its Drafts
//! under `drafts/` (`templates/` is the alias spelling, one per repository),
//! its document pointers, its seeds, and the `navigator.yaml` whose `pages:`
//! declare the matter pages Navigator renders on the matter show page. It
//! commits source only: nothing in it is built, and no Portal bundle is
//! published from it.
//!
//! ```text
//! <organization>/<project-code>
//! ├── .github/workflows/ci.yml
//! ├── .github/workflows/cd.yml
//! ├── .forgejo/workflows/gate.yml  # Forgejo-backed Projects only
//! ├── .agents/skills/    # kept identical to Navigator's own canonical copies
//! ├── drafts/            # *.md Drafts; templates/ is the alias spelling
//! ├── blobs/             # *.yaml Blobs only, one per document; bytes stay in the store
//! ├── documents/         # Git-ignored; the bytes each Blob names, staged or pulled down
//! ├── seeds/             # lookup_fields / records YAML for `navigator site import`
//! ├── .gitignore         # ignores documents/ and notes/, the local working folders
//! ├── AGENTS.md
//! ├── README.md
//! └── navigator.yaml     # the Project this repository declares
//! ```
//!
//! # Where the Project code comes from
//!
//! [`validate`] uses the repository name, and CI has that name as
//! `github.event.repository.name`.
//!
//! A repository also declares its release and Project coordinates in a root
//! manifest — `navigator.yaml`, with `version:` and a nested `project:` map —
//! and the gate checks the pinned `uses:` ref against it. The reusable
//! workflows themselves read `project`/`host` from that same manifest, so a
//! caller passes neither: the gate rejects any `with:` block on the `ci`,
//! `gate`, and `publish` jobs, closing off the duplicate coming back.
//!
//! [`validate`] runs inside one repository's own CI with no access to the live
//! row, so it cannot tell a repository whose manifest is wrong from one whose
//! name is; `navigator project drift` (`super::drift`) is where that
//! disagreement is reported, against the live rows it needs to judge it.
//!
//! One filename, one key: the earlier `.yml` spelling and its `name:` key are
//! retired.
//! `store::sample_project::MANIFEST_FILE` names the same file for the same
//! reason a locally staged sample reads it — a Project repository's manifest
//! and a staged sample's manifest are the same file, not two schemas that
//! happen to overlap.
//!
//! # Exemptions live here, not per repository
//!
//! [`ALLOWED_ROOTS`] is a closed list, and that is the gate rather than an
//! inconvenience: a Project repository holds client-adjacent source, so
//! "anything unenumerated is refused" is what keeps generated documents,
//! exports, and credentials out of one. A legitimate new root is admitted
//! here — reviewed once, for every repository — never by a per-repository
//! exemption file, which would make the gate advisory and let a repository
//! quietly exempt the thing the gate exists to catch.
//!
//! # The scaffold writes source only
//!
//! It writes the repository shell, the onboarding letter's stub Draft under
//! `drafts/`, and the root `.gitignore` that keeps `notes/` out of Git.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::devx::github_setup::REQUIRED_CHECK;
use anyhow::Context;

/// The alias spelling of [`DRAFT_DIRECTORY`]; the gate warns until it is renamed.
const TEMPLATE_DIRECTORY: &str = "templates";
/// The Drafts Navigator imports as Templates. The canonical spelling.
const DRAFT_DIRECTORY: &str = "drafts";
/// The git-ignored working folder for Communication bodies; see [`ROOT_GITIGNORE`].
const NOTES_DIRECTORY: &str = "notes";
/// The root `.gitignore` the scaffold writes. `notes/` holds call notes and
/// memos fed to `navigator project communication create --body-file`; the
/// Communication is the record and its body lives in the store, never in Git.
/// A Notation is a database record, so there is no `notations/` folder.
/// `documents/` holds the bytes each `blobs/` pointer names, staged for
/// `navigator project sync` or pulled down by it; ignoring the whole folder
/// is what keeps a legal document's bytes out of Git.
///
/// It also keeps out the per-machine files every checkout grows:
/// harness-local settings and worktrees, and macOS folder metadata. Each would
/// otherwise be a stageable file the gate refuses. And it is the backstop for
/// "no legal file ever enters a Project repository" (LAW-181): a PDF or Word
/// file saved outside `documents/`, and a `*.local` handoff note.
///
/// `Y024` reports a present root `.gitignore` that lacks any of these lines
/// and never rewrites it; `navigator project repository sync-skills` restores
/// them ([`repair_root_gitignore`]).
pub(crate) const ROOT_GITIGNORE: &str = "# Document bytes staged for or pulled down by `navigator project sync`.\n# Each document's pointer is committed under blobs/; its bytes never are.\n/documents/\n\n# Legal files and handoff notes never enter a Project repository.\n*.pdf\n*.docx\n*.local\n\n# Call notes and memos for `navigator project communication create --body-file`.\n# The Communication's body lives in Navigator's store, never in Git.\nnotes/\n\n# Per-machine agent and operating-system state.\n.claude/settings.local.json\n.claude/worktrees/\n.agents/settings.local.json\n.agents/worktrees/\n.DS_Store\n";
/// The [`ROOT_GITIGNORE`] lines whose absence is an error rather than drift:
/// without them a document's bytes, a rendered pleading, a downloaded
/// exhibit, or a handoff note can be committed (LAW-181).
const ROOT_GITIGNORE_PROTECTIONS: &[&str] = &["/documents/", "*.pdf", "*.docx", "*.local"];
/// The Git-ignored document bytes; see [`crate::document_sync::DOCUMENT_DIRECTORY`].
const DOCUMENT_DIRECTORY: &str = crate::document_sync::DOCUMENT_DIRECTORY;
/// The committed Blobs; see [`crate::document_sync::POINTER_DIRECTORY`].
const POINTER_DIRECTORY: &str = crate::document_sync::POINTER_DIRECTORY;
/// The legacy Portal's Vite workspace.
///
/// `pub(crate)` because [`super::super::devx::github_setup`] still uses its
/// presence while reconciling the supported application layouts.
pub(crate) const PORTAL_DIRECTORY: &str = "portal";
/// Application workspaces, each declared by `apps/<app>/package.json`.
const APPLICATIONS_DIRECTORY: &str = "apps";
/// `pub(crate)` because [`super::super::devx::github_setup`] reconciles the
/// live file at this path against [`workflow`]'s output, the same template
/// `scaffold` writes, read back rather than duplicated.
pub(crate) const WORKFLOW: &str = ".github/workflows/ci.yml";
/// The retired generated-gate filename. Reconcile still reads it so a
/// repository that has not yet been rewritten is classified, not ignored.
pub(crate) const RETIRED_WORKFLOW: &str = ".github/workflows/gate.yml";
/// `pub(crate)` for the same reason as [`WORKFLOW`], but for [`cd_workflow`].
pub(crate) const CD_WORKFLOW: &str = ".github/workflows/cd.yml";
/// The retired generated-publish filename, accepted the same one-release way
/// as [`RETIRED_WORKFLOW`]: `validate_layout` reports a warning naming the
/// replacement rather than refusing outright, so a repository has a release
/// to rename it in before the gate stops accepting the old name. See
/// [`docs/gate.md`](../../../docs/gate.md) for the documented transition.
pub(crate) const RETIRED_CD_WORKFLOW: &str = ".github/workflows/publish.yml";
/// The last Navigator CLI release that still accepts [`RETIRED_WORKFLOW`] or
/// [`RETIRED_CD_WORKFLOW`] with a warning, closing the "one further release"
/// transition [`docs/gate.md`](../../../docs/gate.md) documents. `resolve_workflow`
/// refuses either retired filename outright once the running binary's own
/// [`crate::cli_version`] names a release after this one — see
/// `retired_workflow_refused`. Bump this only to deliberately extend the
/// transition; the ordinary path is for it to stay put and the next release
/// closes the door.
const FINAL_RETIRED_WORKFLOW_RELEASE: &str = "26.9.23";
/// The manifest a Project repository declares its Project in.
///
/// `pub(crate)` rather than private because [`super::drift`] and
/// [`super::super::devx::github_setup`] each read the same file and must not
/// spell it a second time: two constants for one filename is a rename waiting
/// to leave one of them stale, and the gate that *admits* the file, the
/// command that *reads* it locally, and the command that *reads* it live over
/// the API are exactly the trio that must agree. The same filename
/// `store::sample_project::MANIFEST_FILE` names, a Project repository's
/// manifest and a staged sample bundle's manifest are the same file, read by
/// different tools, not two schemas that happen to overlap.
pub(crate) const PROJECT_MANIFEST: &str = "navigator.yaml";
const CODEOWNERS: &str = "# CODEOWNERS\n\n* @shicholas\n";
/// Seed-shaped YAML documents for `navigator site import`, one file per model.
const SEED_DIRECTORY: &str = "seeds";
const ALLOWED_ROOTS: &[&str] = &[
    ".github",
    // Forgejo Actions workflows; see [`FORGEJO_GATE_WORKFLOW`].
    ".forgejo",
    ".gitignore",
    ".gitattributes",
    "AGENTS.md",
    // Every one of these repositories is proprietary, and a licence belongs at
    // the root where a reader looks for it. A portal-bearing repository could
    // hide one inside `portal/`; a templates-only Project has nowhere to put it
    // at all, so refusing it here made the layout unsatisfiable for that shape.
    "LICENSE.md",
    "LICENSE",
    "LICENSE-APACHE-2",
    "LICENSE-MIT",
    "README.md",
    // A Vite application at the repository root (no `apps/` grouping) is named
    // `portal`. These are the files that shape requires at the root.
    "package.json",
    "vite.config.ts",
    "vite.config.js",
    "index.html",
    "pnpm-lock.yaml",
    "package-lock.json",
    "yarn.lock",
    "bun.lockb",
    "tsconfig.json",
    "tsconfig.node.json",
    "tsconfig.app.json",
    ".oxlintrc.json",
    "public",
    "src",
    APPLICATIONS_DIRECTORY,
    "fixtures",
    POINTER_DIRECTORY,
    DOCUMENT_DIRECTORY,
    // The manifest a Project repository declares its Project code in.
    //
    // One entry, not two: `PROJECT_MANIFEST` and `store::sample_project::MANIFEST_FILE`
    // name the same file. The retired `.yml` spelling is not admitted; see
    // `cli/tests/navigator_manifest_retired.rs`.
    PROJECT_MANIFEST,
    PORTAL_DIRECTORY,
    // A seed document names real people and real entities described by this
    // Project's matter — the input to a production write through `navigator
    // site import`, not test scaffolding. That is the one distinction `fixtures/`
    // cannot carry, which is why seed documents get their own root rather
    // than filing under it: one file per model, using the standard
    // `lookup_fields` / `records` shape, and nothing generated.
    SEED_DIRECTORY,
    TEMPLATE_DIRECTORY,
    DRAFT_DIRECTORY,
    "tests",
    // Navigator's own agent skills, synced in by `sync_skills` from this
    // binary's compiled-in copies (see [`SYNCED_SKILLS`]). Refusing it would
    // make the layout unsatisfiable for the thing ENG-383 exists to let a
    // Project repository carry: a Project's portal is client-facing legal
    // copy, and the skills synced here are the review councils that critique
    // exactly that. One catalog root, matching Navigator's own: a
    // harness-specific mirror beside it is what this repository retired.
    ".agents",
    // Only the exact `.claude/settings.json` permission file is allowed;
    // `retired_mirror` rejects all other `.claude/` content.
    ".claude",
];
/// The harness-specific agent-instruction mirrors this layout retired, paired
/// with what to do about one that comes back.
///
/// `AGENTS.md` is the contract and `.agents/skills/` the catalog: one file and
/// one directory, read directly by whichever harness is pointed at the tree. A
/// `CLAUDE.md`, a `.claude/`, or a `.codex/` beside them is the same
/// instructions under a name only one harness reads, and keeping two copies in
/// sync is the job nobody does — the mirrored-symlink arrangement Navigator
/// itself retired failed silently, checking out on a clone without symlink
/// support as a nine-byte `CLAUDE.md` whose *contents* were the string
/// `AGENTS.md`.
///
/// Named here rather than left to [`ALLOWED_ROOTS`] because that list reads
/// only a path's first component and says nothing about what to do: a root
/// `CLAUDE.md` or `.claude/` would be refused as an anonymous unenumerated
/// root, and an `apps/portal/CLAUDE.md` sits under an allowed root. Matched
/// on any path component, like [`FORBIDDEN_COMPONENTS`], so an
/// `apps/portal/CLAUDE.md` does not walk around a root-only rule — and over
/// the same git-visible files every other layout rule reads, so a developer's
/// own untracked, ignored `.claude/` — harness-local state, and where a
/// worktree checkout lives — is not this gate's business.
const RETIRED_AGENT_MIRRORS: &[(&str, &str)] = &[
    (
        "CLAUDE.md",
        "`CLAUDE.md` is a retired agent-instruction mirror; `AGENTS.md` is the \
         whole contract for this repository, so delete it rather than keep a \
         second copy in sync",
    ),
    (
        ".claude",
        "`.claude/` is a retired agent-instruction mirror; `.agents/skills/` \
         is the whole catalog, so delete it",
    ),
    (
        ".codex",
        "`.codex/` is a retired agent-instruction mirror; `.agents/skills/` is \
         the whole catalog, so delete it",
    ),
];
/// Claude Code's shared project permissions file is the narrow exception to
/// the retired `.claude/` instruction mirror. It supplies actual tool denial;
/// `AGENTS.md` remains the portable contract.
const CLAUDE_SETTINGS: &str = ".claude/settings.json";
const CLAUDE_SETTINGS_CONTENTS: &str = include_str!("claude_settings.json");
const FORBIDDEN_COMPONENTS: &[&str] = &[
    "answers",
    "build",
    "client_uploads",
    "dependencies",
    "dist",
    "generated",
    "node_modules",
    "output",
    "secrets",
    "target",
    "uploads",
    "vendor",
];
const FORBIDDEN_CREDENTIAL_EXTENSIONS: &[&str] = &["env", "key", "pem", "p12", "pfx"];
const FORBIDDEN_DOCUMENT_EXTENSIONS: &[&str] = &["doc", "docx", "odt", "pdf"];

/// The skill catalog synced into every Project repository, embedded at build
/// time from Navigator's own canonical `.agents/skills/` — never read from a
/// live checkout at runtime, so a downloaded release binary can write it with
/// no wider clone. Kept intentionally small: every skill synced here is one
/// to keep true in every repository that carries it. A Project's portal is
/// client-facing legal copy — an engagement summary, a documents tab, a
/// matter timeline — so the two review councils that critique exactly that,
/// plus the general engineering council, are the initial set. `stay-in-repo`
/// joins them as the shared scope rule every Project repository's `AGENTS.md`
/// used to hand-write on its own, three times, in three different voices.
/// `legal-writing` (LAW-58) is the writing discipline both review councils
/// and the litigation filing gate assume the underlying prose already
/// meets — every Project repository drafts memos, letters, and briefs, so
/// it is fleet-wide rather than repo-local. `case-downloader` (LAW-94) is the
/// verified-source procedure for filing the authority a matter relies on as
/// an Authority pointer, with the bytes kept out of Git. `deadline` (LAW-126)
/// records a date once through `navigator project deadline` and makes the
/// matter page and the private Slack channel views of that one row.
/// `browser-handoff` (LAW-129) borrows the attorney's own signed-in browser for
/// a third-party download, never handles a credential, and files the result.
/// `case-timeline` (LAW-133) keeps a litigation case record as Docket Entries
/// through `navigator project docket`, never as a hand-edited memo.
/// `opposing-communication-review` gives every Project the recipient-side
/// review discipline for opposing-party correspondence.
pub(crate) const SYNCED_SKILLS: &[(&str, &str)] = &[
    (
        "council",
        include_str!("../../../.agents/skills/council/SKILL.md"),
    ),
    (
        "legal-council",
        include_str!("../../../.agents/skills/legal-council/SKILL.md"),
    ),
    (
        "client-council",
        include_str!("../../../.agents/skills/client-council/SKILL.md"),
    ),
    (
        "human-readable",
        include_str!("../../../.agents/skills/human-readable/SKILL.md"),
    ),
    (
        "stay-in-repo",
        include_str!("../../../.agents/skills/stay-in-repo/SKILL.md"),
    ),
    (
        "legal-writing",
        include_str!("../../../.agents/skills/legal-writing/SKILL.md"),
    ),
    (
        "case-downloader",
        include_str!("../../../.agents/skills/case-downloader/SKILL.md"),
    ),
    (
        "project-setup",
        include_str!("../../../.agents/skills/project-setup/SKILL.md"),
    ),
    (
        "deadline",
        include_str!("../../../.agents/skills/deadline/SKILL.md"),
    ),
    (
        "browser-handoff",
        include_str!("../../../.agents/skills/browser-handoff/SKILL.md"),
    ),
    (
        "case-timeline",
        include_str!("../../../.agents/skills/case-timeline/SKILL.md"),
    ),
    (
        "opposing-communication-review",
        include_str!("../../../.agents/skills/opposing-communication-review/SKILL.md"),
    ),
    (
        "record-search",
        include_str!("../../../.agents/skills/record-search/SKILL.md"),
    ),
];

#[derive(Debug)]
struct Finding {
    path: PathBuf,
    message: String,
}

impl Finding {
    fn at(path: impl Into<PathBuf>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

/// Whether `dir` is a Project repository: it carries [`PROJECT_MANIFEST`] and
/// that manifest declares a `project`.
///
/// This is the one admission check every write into a repository's agent
/// contract owes before it touches disk — `cli/src/main.rs` applies the same
/// check before both `project gate`'s Project-repository layout pass and its
/// own document-pointer pass, so a tree with no `navigator.yaml` is read as
/// ordinary source, never as a Project repository missing its manifest.
pub(crate) fn is_project_repository(dir: &Path) -> bool {
    let Ok(raw) = fs::read_to_string(dir.join(PROJECT_MANIFEST)) else {
        return false;
    };
    serde_yaml::from_str::<serde_yaml::Value>(&raw)
        .ok()
        .and_then(|value| value.get("project").cloned())
        .is_some()
}

/// What [`validate_gate`] found, so a caller composing it with another pass
/// (LAW-86) can report one combined, accurate verdict instead of treating
/// this pass's own "0 error(s)" summary as though it spoke for the whole
/// `navigator project gate` run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LayoutOutcome {
    pub(crate) errors: usize,
    pub(crate) warnings: usize,
}

/// Validate one Project's repository.
///
/// Templates are intentionally passed to the rule engine under bare filenames:
/// they are Project blueprints, not members of Navigator's shared
/// `templates/notations/neon_law` / `templates/notations/forms` catalog. This mirrors
/// `store::template_source::persist_from_repo` exactly.
///
/// Templates and applications are independently optional, and a repository
/// carrying neither is reported distinctly rather than failed. A Project may
/// legitimately have opened before either half exists.
/// Commit-state rules read the files Git would carry, not the files on disk:
/// the gate judges what a pull request proposes, and an untracked scratch file
/// is not part of that.
pub(crate) fn validate_gate(
    root: &Path,
    repository: Option<&str>,
    write_fixes: bool,
) -> LayoutOutcome {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    if !root.is_dir() {
        eprintln!(
            "navigator: Project repository root is not a directory: {}",
            root.display()
        );
        return LayoutOutcome {
            errors: 1,
            warnings: 0,
        };
    }

    // The repository name is the Project code, and that is the value this
    // validator speaks about: a checkout named something a Project code could
    // never be is a checkout it cannot judge.
    //
    // The manifest also declares a code, so the two *can* disagree — see
    // `super::drift`, which reports it. This validator deliberately does not
    // resolve that disagreement. It runs inside one repository's own CI with no
    // access to the live row, and the rule is that the row wins and the
    // repository is corrected; a gate that picked a winner without seeing the
    // row would be guessing, and would fail a repository mid-correction.
    let code = repository_name(root, repository);
    if !store::projects::is_valid_code(&code) {
        errors.push(Finding::at(
            root,
            format!(
                "repository name `{code}` is not a valid Navigator Project code; \
                 the repository name *is* the code"
            ),
        ));
    }

    // The legacy-layout move runs first, so the layout walk below judges the
    // checkout the move leaves rather than the one it found.
    validate_documents_gitignore(root, write_fixes, &mut errors, &mut warnings);
    let manifest_valid = validate_layout(root, &mut errors, &mut warnings);
    validate_codeowners(root, &mut errors);
    validate_automerge_workflow(root, write_fixes, &mut errors);
    validate_tracked_files(root, write_fixes, &mut errors, &mut warnings);
    validate_skills(root, &mut errors);
    validate_documented_cli(root, &mut errors);
    let blueprints = blueprint_selection(root, &mut errors, &mut warnings);
    let templates = match blueprints {
        rules::project_blueprints::BlueprintSelection::Selected(directory) if manifest_valid => {
            validate_templates(root, directory, &mut errors, &mut warnings)
        }
        _ => 0,
    };
    if blueprints == rules::project_blueprints::BlueprintSelection::Absent {
        println!("note: {code} carries no `{DRAFT_DIRECTORY}/` yet");
    }

    for warning in &warnings {
        println!("{}: warning: {}", warning.path.display(), warning.message);
    }
    for error in &errors {
        eprintln!("{}: error: {}", error.path.display(), error.message);
    }
    println!(
        "Validated Project repository `{code}`: {templates} template(s), {} error(s), {} warning(s)",
        errors.len(),
        warnings.len()
    );

    LayoutOutcome {
        errors: errors.len(),
        warnings: warnings.len(),
    }
}

/// The Draft directory a command writes into: `drafts/`, or the
/// `templates/` alias when only that spelling is present. An ambiguous tree
/// gets the canonical spelling, and the gate refuses it.
pub(crate) fn draft_directory(root: &Path) -> &'static str {
    use rules::project_blueprints::{BlueprintDirectory, BlueprintSelection};
    match rules::project_blueprints::select(
        root.join(TEMPLATE_DIRECTORY).is_dir(),
        root.join(DRAFT_DIRECTORY).is_dir(),
    ) {
        BlueprintSelection::Selected(directory) => directory.as_str(),
        BlueprintSelection::Absent | BlueprintSelection::Ambiguous => {
            BlueprintDirectory::Drafts.as_str()
        }
    }
}

/// Choose the Draft directory. `drafts/` is canonical and passes silently;
/// the `templates/` alias is accepted with a rename warning until the fleet
/// has moved; both together are refused, because every reader must resolve
/// one directory.
fn blueprint_selection(
    root: &Path,
    errors: &mut Vec<Finding>,
    warnings: &mut Vec<Finding>,
) -> rules::project_blueprints::BlueprintSelection {
    use rules::project_blueprints::{BlueprintDirectory, BlueprintSelection};
    let selection = rules::project_blueprints::select(
        root.join(TEMPLATE_DIRECTORY).is_dir(),
        root.join(DRAFT_DIRECTORY).is_dir(),
    );
    match selection {
        BlueprintSelection::Ambiguous => errors.push(Finding::at(
            root,
            format!(
                "choose one Project blueprint directory: `{DRAFT_DIRECTORY}/` or `{TEMPLATE_DIRECTORY}/`; both are present"
            ),
        )),
        BlueprintSelection::Selected(BlueprintDirectory::Templates) => warnings.push(Finding::at(
            root.join(TEMPLATE_DIRECTORY),
            format!(
                "`{TEMPLATE_DIRECTORY}/` is the alias spelling of `{DRAFT_DIRECTORY}/`; rename it with \
                 `git mv {TEMPLATE_DIRECTORY} {DRAFT_DIRECTORY}`"
            ),
        )),
        BlueprintSelection::Selected(BlueprintDirectory::Drafts) | BlueprintSelection::Absent => {}
    }
    selection
}

/// A repository still carrying [`RETIRED_WORKFLOW`] or [`RETIRED_CD_WORKFLOW`]
/// is not failed for it — it is warned, once, by name. The one-release
/// transition this documents lives in `docs/gate.md`: this release still
/// reads the retired filename, and the release after
/// [`FINAL_RETIRED_WORKFLOW_RELEASE`] refuses it, so the warning is the
/// operator's whole notice to rename it.
fn retired_workflow_warning(path: &Path, canonical: &str) -> Finding {
    Finding::at(
        path,
        format!(
            "`{}` is the retired filename for `{canonical}`; releases through \
             {FINAL_RETIRED_WORKFLOW_RELEASE} still accept it, but the release after that \
             refuses it — rename the file to `{canonical}`",
            path.display()
        ),
    )
}

/// Whether this binary's own release, `current` (from [`crate::cli_version`]),
/// is a release after [`FINAL_RETIRED_WORKFLOW_RELEASE`] and must therefore
/// refuse [`RETIRED_WORKFLOW`] or [`RETIRED_CD_WORKFLOW`] outright rather than
/// warn about it.
///
/// An unparseable version on either side fails open — refusing a repository's
/// CI over a version string this check cannot read would be a worse failure
/// than accepting one release past the bound, and a plain local build without
/// a baked release tag falls back to `CARGO_PKG_VERSION`, which is exactly
/// [`FINAL_RETIRED_WORKFLOW_RELEASE`] until the next `chore(release)` bump —
/// so a developer's own tree only starts refusing once it actually is a
/// later release.
fn retired_workflow_refused(current: &str) -> bool {
    let Ok(current) = semver::Version::parse(current) else {
        return false;
    };
    let Ok(final_release) = semver::Version::parse(FINAL_RETIRED_WORKFLOW_RELEASE) else {
        return false;
    };
    current > final_release
}

/// The release named by [`FINAL_RETIRED_WORKFLOW_RELEASE`] has passed:
/// `path`'s retired filename is refused outright, not merely warned about.
fn retired_workflow_refusal(path: &Path, canonical: &str) -> Finding {
    Finding::at(
        path,
        format!(
            "`{}` is the retired filename for `{canonical}`; only releases through \
             {FINAL_RETIRED_WORKFLOW_RELEASE} accepted it, and this release refuses it — \
             rename the file to `{canonical}`",
            path.display()
        ),
    )
}

/// `retired` still exists beside its canonical replacement `current`, so a
/// repository can carry a structurally validated `current` and an
/// unvalidated `retired` that GitHub still runs — the coexistence bypass this
/// finding closes. Independent of [`retired_workflow_refused`]: this is
/// refused in every release, not only past the bound, because the retired
/// file is never even inspected once the canonical one is present.
fn retired_workflow_beside_canonical(retired: &Path, current: &Path) -> Finding {
    Finding::at(
        retired,
        format!(
            "`{}` exists alongside its canonical replacement `{}`; GitHub still runs whichever \
             workflows trigger, so a retired file beside the canonical one is a live, unvalidated \
             CI/CD path — delete `{}`",
            retired.display(),
            current.display(),
            retired.display()
        ),
    )
}

/// `expected` (a `.yml` path) is missing, but its `.yaml` sibling exists: name
/// the extension actually on disk and the one Navigator reads, rather than
/// reporting a bare "missing" that sends the operator looking for a typo they
/// already made correctly.
fn yaml_extension_finding(expected: &Path, canonical: &str) -> Option<Finding> {
    let yaml_path = expected.with_extension("yaml");
    if yaml_path.is_file() {
        Some(Finding::at(
            &yaml_path,
            format!(
                "`{}` uses the retired `.yaml` extension; Navigator only reads `{canonical}`",
                yaml_path.display()
            ),
        ))
    } else {
        None
    }
}

/// The fixed facts one workflow file's resolution needs: its current name,
/// its retired name, the canonical name to name in a diagnostic, and the
/// message for a bare "missing" finding.
struct WorkflowSpec<'a> {
    current: &'a Path,
    retired: &'a Path,
    canonical: &'a str,
    missing_message: &'a str,
}

/// Resolve one workflow file against `spec`'s current name, its retired
/// name, and a `.yaml` extension typo of either, then structurally validate
/// whichever content was found with `validate`.
///
/// Shared by both `ci.yml` (with [`validate_workflow`]) and `cd.yml` (with
/// [`validate_cd_workflow`]) in [`validate_layout`], which is what keeps this
/// resolution order — current name, then retired name with a warning, then
/// the precise extension diagnostic, then a bare "missing" — in exactly one
/// place instead of drifting between the two callers.
fn resolve_workflow(
    spec: &WorkflowSpec,
    manifest: Option<&super::manifest::Manifest>,
    errors: &mut Vec<Finding>,
    warnings: &mut Vec<Finding>,
    validate: impl Fn(&Path, &str, Option<&super::manifest::Manifest>, &mut Vec<Finding>),
) {
    match fs::read_to_string(spec.current) {
        Ok(contents) => {
            validate(spec.current, &contents, manifest, errors);
            if spec.retired.is_file() {
                errors.push(retired_workflow_beside_canonical(
                    spec.retired,
                    spec.current,
                ));
            }
        }
        Err(_) => match fs::read_to_string(spec.retired) {
            Ok(contents) => {
                if retired_workflow_refused(crate::cli_version()) {
                    errors.push(retired_workflow_refusal(spec.retired, spec.canonical));
                } else {
                    warnings.push(retired_workflow_warning(spec.retired, spec.canonical));
                }
                validate(spec.retired, &contents, manifest, errors);
            }
            Err(_) => match yaml_extension_finding(spec.current, spec.canonical)
                .or_else(|| yaml_extension_finding(spec.retired, spec.canonical))
            {
                Some(finding) => errors.push(finding),
                None => errors.push(Finding::at(spec.current, spec.missing_message)),
            },
        },
    }
}

fn validate_codeowners(root: &Path, errors: &mut Vec<Finding>) {
    let path = root.join(".github/CODEOWNERS");
    match fs::read_to_string(&path) {
        Ok(contents) if contents == CODEOWNERS => {}
        Ok(_) => errors.push(Finding::at(
            &path,
            "`.github/CODEOWNERS` must contain the canonical single-owner rule",
        )),
        Err(_) => errors.push(Finding::at(&path, "missing required `.github/CODEOWNERS`")),
    }
}

fn validate_documented_cli(root: &Path, errors: &mut Vec<Finding>) {
    let tree = crate::navigator_command();
    for path in super::cli_docs::markdown_paths(root) {
        let Ok(markdown) = fs::read_to_string(&path) else {
            continue;
        };
        for finding in super::cli_docs::unresolved_invocations(&markdown, &tree) {
            errors.push(Finding::at(
                path.clone(),
                format!(
                    "line {}: documented `{}` does not resolve; `{}` is not a subcommand of this navigator",
                    finding.line, finding.command, finding.verb
                ),
            ));
        }
    }
}

fn repository_name(root: &Path, explicit: Option<&str>) -> String {
    if let Some(name) = explicit.map(str::trim).filter(|name| !name.is_empty()) {
        return name.rsplit('/').next().unwrap_or(name).to_string();
    }
    if let Ok(repository) = std::env::var("GITHUB_REPOSITORY") {
        if let Some(name) = repository
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
        {
            return name.to_string();
        }
    }
    if let Ok(contents) = fs::read_to_string(root.join(PROJECT_MANIFEST)) {
        if let Ok(manifest) = super::manifest::parse(&contents) {
            if let Some(code) = manifest
                .project
                .as_deref()
                .map(str::trim)
                .filter(|code| !code.is_empty())
            {
                return code.to_string();
            }
        }
    }
    root.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("<unknown>")
        .to_string()
}

/// Return the files Git considers tracked or stageable, applying every ignore
/// file in the checkout. Git's index is deliberately included even when an
/// ignore rule now matches a tracked path, so a tracked file cannot hide from
/// the source-only layout checks.
fn git_tracked_and_stageable_files(root: &Path) -> io::Result<Vec<PathBuf>> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .output()
        .map_err(|error| {
            io::Error::new(error.kind(), format!("could not run git ls-files: {error}"))
        })?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = if detail.to_ascii_lowercase().contains("not a git repository") {
            "not a Git repository".to_string()
        } else if detail.is_empty() {
            "git ls-files exited unsuccessfully".to_string()
        } else {
            detail
        };
        return Err(io::Error::other(detail));
    }

    let mut files = Vec::new();
    for path in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let relative = std::str::from_utf8(path).map_err(|error| {
            io::Error::other(format!("git ls-files returned a non-UTF-8 path: {error}"))
        })?;
        let path = root.join(relative);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_file() => files.push(path),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(files)
}

fn layout_entries(root: &Path, errors: &mut Vec<Finding>) -> Option<Vec<(PathBuf, bool)>> {
    match git_tracked_and_stageable_files(root) {
        Ok(files) => Some(files.into_iter().map(|path| (path, true)).collect()),
        Err(error) => {
            errors.push(Finding::at(
                root,
                format!("could not enumerate git-tracked and stageable files: {error}"),
            ));
            None
        }
    }
}

/// Report one path that sits under a [`RETIRED_AGENT_MIRRORS`] entry, and say
/// whether it did.
///
/// Matched at any depth, like [`FORBIDDEN_COMPONENTS`]: an
/// `apps/portal/CLAUDE.md` is the same mirror as a root one, and a rule that
/// only reads the root is one an application directory walks around.
///
/// `seen` carries the mirrors already reported, so the finding is one per
/// mirror, anchored at the mirror itself: a `.claude/skills/` holds a file per
/// skill, and a directory that should not exist is one thing to delete, not
/// nine. A `true` return also suppresses the generic unenumerated-root
/// finding, which would otherwise say something vaguer about the same file.
fn retired_mirror(
    root: &Path,
    components: &[String],
    seen: &mut Vec<PathBuf>,
    errors: &mut Vec<Finding>,
) -> bool {
    if components
        .first()
        .is_some_and(|component| component == ".claude")
        && components.len() == 2
        && components[1] == "settings.json"
    {
        return false;
    }
    let Some((depth, message)) = components
        .iter()
        .enumerate()
        .find_map(|(depth, component)| {
            RETIRED_AGENT_MIRRORS
                .iter()
                .copied()
                .find(|(mirror, _)| *mirror == component.as_str())
                .map(|(_, message)| (depth, message))
        })
    else {
        return false;
    };
    let mirror = root.join(components[..=depth].iter().collect::<PathBuf>());
    if !seen.contains(&mirror) {
        seen.push(mirror.clone());
        errors.push(Finding::at(mirror, message));
    }
    true
}

/// Refuse a path whose first component is outside [`ALLOWED_ROOTS`]. A root
/// file is reported where it sits; a file under an unknown top-level
/// directory is reported once, at that directory.
fn unlisted_root(
    root: &Path,
    path: &Path,
    components: &[String],
    seen: &mut Vec<PathBuf>,
    errors: &mut Vec<Finding>,
) {
    if components.len() == 1 {
        errors.push(Finding::at(
            path,
            "path is outside the source-only Project repository layout",
        ));
        return;
    }
    let directory = root.join(&components[0]);
    if !seen.contains(&directory) {
        seen.push(directory.clone());
        errors.push(Finding::at(
            directory,
            format!(
                "`{}/` is outside the source-only Project repository layout; \
                 a Project repository holds only its listed top-level roots",
                components[0]
            ),
        ));
    }
}

/// Close `.github/` to exactly [`CODEOWNERS`], the two thin workflow callers,
/// and their retired filenames during the one-release transition.
///
/// [`ALLOWED_ROOTS`] admits `.github` as a whole first path component, so
/// nothing else in [`validate_layout`]'s walk descends into it — this is that
/// descent. It closes which *paths* may exist; `.github/CODEOWNERS`'s
/// content is [`validate_codeowners`]'s job, and each workflow's content is
/// [`validate_workflow`]'s or [`validate_cd_workflow`]'s.
fn slash_separated_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// The auto-merge workflow every Project repository must carry, admitted to
/// the closed `.github` set alongside [`WORKFLOW`] and [`CD_WORKFLOW`].
///
/// Its content is machine-owned and validated byte-exact against
/// [`AUTOMERGE_WORKFLOW_CONTENTS`], the same shape as [`CODEOWNERS`] — no
/// repository has a legitimate reason to differ from another, so any
/// variation is drift rather than local intent.
pub(crate) const AUTOMERGE_WORKFLOW: &str = ".github/workflows/automerge.yml";
/// The byte-exact content [`AUTOMERGE_WORKFLOW`] must carry.
///
/// Arms as the merge-queue App, never as `GITHUB_TOKEN` — a token minted from
/// the run's own `GITHUB_TOKEN` starts no further workflow on `main`, so a
/// publish armed that way silently never runs (ENG-256) — and skips a draft
/// PR, with `ready_for_review` in the trigger types so a draft marked ready
/// later still fires a run rather than sitting green and unarmed forever.
/// Carries no comments of its own beyond the one pinned-action version, for
/// the same reason [`CODEOWNERS`] carries none: this file is machine-owned,
/// and a comment is the part of a hand-copied file that rots unnoticed in
/// twenty repositories. The rationale above lives here and in ENG-256, not
/// re-typed into the file itself.
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

fn validate_github_path(path: &Path, relative: &Path, errors: &mut Vec<Finding>) {
    let relative = slash_separated_path(relative);
    let allowed = [
        ".github/CODEOWNERS",
        WORKFLOW,
        CD_WORKFLOW,
        AUTOMERGE_WORKFLOW,
        RETIRED_WORKFLOW,
        RETIRED_CD_WORKFLOW,
    ];
    if !allowed.contains(&relative.as_ref()) {
        errors.push(Finding::at(
            path,
            format!(
                "`{relative}` is outside the closed `.github` file set; only CODEOWNERS, \
                 `{WORKFLOW}`, `{CD_WORKFLOW}`, and `{AUTOMERGE_WORKFLOW}` belong here"
            ),
        ));
    }
}

/// Close `.forgejo/` to exactly [`FORGEJO_GATE_WORKFLOW`], held byte-exact
/// to [`forgejo_gate_workflow`]: it is machine-owned, and it is the one
/// workflow path the deployment's Forgejo document door trusts.
fn validate_forgejo_path(path: &Path, relative: &Path, errors: &mut Vec<Finding>) {
    let relative = slash_separated_path(relative);
    if relative != FORGEJO_GATE_WORKFLOW {
        errors.push(Finding::at(
            path,
            format!(
                "`{relative}` is outside the closed `.forgejo` file set; only \
                 `{FORGEJO_GATE_WORKFLOW}` belongs here"
            ),
        ));
    } else if fs::read_to_string(path).ok() != Some(forgejo_gate_workflow()) {
        errors.push(Finding::at(
            path,
            format!(
                "`{FORGEJO_GATE_WORKFLOW}` differs from this release's generated Forgejo gate; \
                 run `navigator project repository sync-skills` to restore it"
            ),
        ));
    }
}

/// Hold [`AUTOMERGE_WORKFLOW`] to [`AUTOMERGE_WORKFLOW_CONTENTS`] byte-exact,
/// the same shape as [`validate_codeowners`] — but unlike that check, this one
/// self-repairs under `write_fixes`, the way [`validate_documents_gitignore`]
/// does: the file is machine-owned, so a drifted or missing copy has exactly
/// one correct byte sequence to converge on, and there is nothing a human
/// judgment call could add.
fn validate_automerge_workflow(root: &Path, write_fixes: bool, errors: &mut Vec<Finding>) {
    let path = root.join(AUTOMERGE_WORKFLOW);
    let current = fs::read_to_string(&path).ok();
    if current.as_deref() == Some(AUTOMERGE_WORKFLOW_CONTENTS) {
        return;
    }
    if write_fixes {
        if let Some(parent) = path.parent() {
            if let Err(error) = fs::create_dir_all(parent) {
                errors.push(Finding::at(
                    &path,
                    format!("could not create {}: {error}", parent.display()),
                ));
                return;
            }
        }
        if let Err(error) = fs::write(&path, AUTOMERGE_WORKFLOW_CONTENTS) {
            errors.push(Finding::at(
                &path,
                format!("could not write canonical `{AUTOMERGE_WORKFLOW}`: {error}"),
            ));
            return;
        }
        println!("fixed {}", path.display());
        return;
    }
    errors.push(Finding::at(
        &path,
        if current.is_some() {
            format!(
                "`{AUTOMERGE_WORKFLOW}` must match the canonical auto-merge workflow byte-exact"
            )
        } else {
            format!("missing required `{AUTOMERGE_WORKFLOW}`")
        },
    ));
}

fn validate_layout(root: &Path, errors: &mut Vec<Finding>, warnings: &mut Vec<Finding>) -> bool {
    if !root.join("README.md").is_file() {
        errors.push(Finding::at(
            root.join("README.md"),
            "missing required repository README",
        ));
    }

    validate_agent_contract(root, errors, warnings);
    validate_claude_settings(root, errors);
    let manifest_valid = validate_manifest(root, errors, warnings);
    let manifest = fs::read_to_string(root.join(PROJECT_MANIFEST))
        .ok()
        .and_then(|contents| super::manifest::parse(&contents).ok());

    resolve_workflow(
        &WorkflowSpec {
            current: &root.join(WORKFLOW),
            retired: &root.join(RETIRED_WORKFLOW),
            canonical: WORKFLOW,
            missing_message: "missing required CI gate",
        },
        manifest.as_ref(),
        errors,
        warnings,
        |path, contents, manifest, errors| {
            validate_workflow(path, contents, manifest, DEFAULT_WORKFLOW_HOME, errors);
        },
    );
    resolve_workflow(
        &WorkflowSpec {
            current: &root.join(CD_WORKFLOW),
            retired: &root.join(RETIRED_CD_WORKFLOW),
            canonical: CD_WORKFLOW,
            missing_message: "missing required CD workflow",
        },
        manifest.as_ref(),
        errors,
        warnings,
        |path, contents, manifest, errors| {
            validate_cd_workflow(path, contents, manifest, DEFAULT_WORKFLOW_HOME, errors);
        },
    );

    let Some(entries) = layout_entries(root, errors) else {
        return manifest_valid;
    };

    let mut seen = LayoutWalk::default();
    for (path, is_file) in entries {
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        if relative.as_os_str().is_empty() {
            continue;
        }
        let components: Vec<String> = relative
            .components()
            .filter_map(|component| component.as_os_str().to_str().map(str::to_string))
            .collect();
        validate_layout_entry(
            root,
            is_file,
            relative,
            &components,
            &mut seen,
            errors,
            warnings,
        );
    }
    manifest_valid
}

/// The directories [`validate_layout`]'s walk has already reported, so a
/// directory that should not exist is one finding, anchored at the
/// directory, rather than one per file inside it.
#[derive(Default)]
struct LayoutWalk {
    retired_mirrors: Vec<PathBuf>,
    unlisted_roots: Vec<PathBuf>,
    tooling_directories: Vec<PathBuf>,
}

/// Roots whose content the repo-local tooling warning leaves alone: synced
/// agent skills and workflow callers, which Navigator owns, and seed YAML,
/// which is data for `navigator site import`.
const TOOLING_EXEMPT_ROOTS: &[&str] = &[".agents", ".github", SEED_DIRECTORY];
/// Node package-manager files, matched by exact filename.
const NODE_TOOLING_FILES: &[&str] = &[
    "package.json",
    "pnpm-lock.yaml",
    "package-lock.json",
    "yarn.lock",
    ".nvmrc",
];
/// Script and Node source extensions.
const SCRIPT_EXTENSIONS: &[&str] = &["ts", "tsx", "js", "mjs", "cjs", "py", "sh"];
/// A directory of repo-local scripts, warned once at the directory.
const SCRIPTS_DIRECTORY: &str = "scripts";
/// What every tooling warning tells the author to do instead.
const TOOLING_REPLACEMENT: &str =
    "a Project repository runs no tooling of its own: matter pages are \
     declared under `pages:` in `navigator.yaml`, and the only CLI it uses is the \
     public `navigator` release";

/// The depth of the Vite application directory `components` sits inside:
/// the legacy root `portal/` or an `apps/<app>/` workspace.
fn application_depth(components: &[String]) -> Option<usize> {
    match components {
        [first, _, ..] if first == PORTAL_DIRECTORY => Some(1),
        [first, _, _, ..] if first == APPLICATIONS_DIRECTORY => Some(2),
        _ => None,
    }
}

/// Warn on Node and repo-local tooling, and say whether the warning claimed
/// the path's whole top-level directory, so the unlisted-root error does not
/// report the same directory a second time.
///
/// A warning while the fleet moves to `pages:`. A Vite application under
/// `portal/` or `apps/<app>/` is one warning at the application; a
/// `scripts/` directory is one warning at the directory; any other
/// `package.json`, lockfile, `.nvmrc`, `tsconfig*.json`, or Node, Python, or
/// shell source file is a warning where it sits. `node_modules/` is a
/// forbidden component and already an error, so it is not warned again.
fn repo_local_tooling(
    root: &Path,
    is_file: bool,
    components: &[String],
    seen: &mut Vec<PathBuf>,
    warnings: &mut Vec<Finding>,
) -> bool {
    let Some((name, parents)) = components.split_last() else {
        return false;
    };
    if TOOLING_EXEMPT_ROOTS.contains(&components[0].as_str())
        || components
            .iter()
            .any(|component| FORBIDDEN_COMPONENTS.contains(&component.as_str()))
    {
        return false;
    }
    let mut warn_directory = |depth: usize, what: &str| {
        let directory = root.join(components[..depth].iter().collect::<PathBuf>());
        if !seen.contains(&directory) {
            seen.push(directory.clone());
            let relative = components[..depth].join("/");
            warnings.push(Finding::at(
                directory,
                format!("`{relative}/` is {what}; {TOOLING_REPLACEMENT}"),
            ));
        }
    };
    if let Some(depth) = application_depth(components) {
        warn_directory(depth, "a Vite application");
        return false;
    }
    if let Some(index) = parents
        .iter()
        .position(|component| component == SCRIPTS_DIRECTORY)
    {
        warn_directory(index + 1, "a directory of repo-local scripts");
        return index == 0;
    }
    let extension = Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let tsconfig = name.starts_with("tsconfig") && extension == "json";
    if is_file
        && (NODE_TOOLING_FILES.contains(&name.as_str())
            || tsconfig
            || SCRIPT_EXTENSIONS.contains(&extension.as_str()))
    {
        warnings.push(Finding::at(
            root.join(components.iter().collect::<PathBuf>()),
            format!(
                "`{}` is repo-local tooling; {TOOLING_REPLACEMENT}",
                components.join("/")
            ),
        ));
    }
    false
}

/// One git-tracked or stageable path's share of [`validate_layout`]'s walk:
/// the retired-mirror check, the closed root-set check, the `.github` closed
/// set, the `documents/` pointer-only check, the forbidden-component check,
/// and the forbidden-extension checks. Split out so `validate_layout` itself
/// stays a short list of passes rather than the loop body that runs them.
///
/// The closed root-set check reads a path's first component at any depth.
/// An unknown top-level directory is refused once, at the directory, however
/// deep its files sit; a `foo/bar.txt` is as far outside the layout as a root
/// `foo.txt`. Each allowed root is then walked by its own rules — `.github/`
/// by its closed file set, `documents/` by its pointer-only rule, and every
/// root by the forbidden-component and forbidden-extension checks — rather
/// than by a second closed list of nested paths. [`repo_local_tooling`]
/// reads every root the same way, and a root `scripts/` is its warning
/// rather than an unlisted-root error.
fn validate_layout_entry(
    root: &Path,
    is_file: bool,
    relative: &Path,
    components: &[String],
    seen: &mut LayoutWalk,
    errors: &mut Vec<Finding>,
    warnings: &mut Vec<Finding>,
) {
    let Some(first) = components.first() else {
        return;
    };
    let path = &root.join(relative);
    let mirrored = retired_mirror(root, components, &mut seen.retired_mirrors, errors);
    let tooling_root = repo_local_tooling(
        root,
        is_file,
        components,
        &mut seen.tooling_directories,
        warnings,
    );
    if !mirrored
        && !tooling_root
        && first != NOTES_DIRECTORY
        && !ALLOWED_ROOTS.contains(&first.as_str())
    {
        unlisted_root(root, path, components, &mut seen.unlisted_roots, errors);
    }
    if first == ".github" && is_file {
        validate_github_path(path, relative, errors);
    }
    if first == ".forgejo" && is_file {
        validate_forgejo_path(path, relative, errors);
    }
    if first == NOTES_DIRECTORY {
        errors.push(Finding::at(
            path,
            "`notes/` must stay git-ignored; a note feeds `navigator project communication create --body-file`, and the Communication's body lives in Navigator's store, never in Git",
        ));
    }
    // A tracked path already gone from the working tree is a removal waiting
    // to be committed — what the legacy-layout move leaves — not a file to
    // delete.
    if first == DOCUMENT_DIRECTORY && is_file && fs::symlink_metadata(path).is_ok() {
        // A pointer or guard the layout before `blobs/` committed is a
        // warning for one release, so an upgraded Project does not go red
        // before its move is committed (LAW-189). Bytes, and a pointer in a
        // matter folder, are refused.
        let below: PathBuf = components[1..].iter().collect();
        let legacy = (components.len() == 2 && components[1] == ".gitignore")
            || (crate::document_sync::is_pointer_path(path)
                && crate::document_layout::matter_folder_of(&below).is_none());
        if legacy {
            warnings.push(Finding::at(
                path,
                "document pointers now live under `blobs/`; run `navigator project sync` or `navigator project repository migrate-documents` and commit the move. The next release refuses it here",
            ));
        } else {
            errors.push(Finding::at(
                path,
                "`documents/` is Git-ignored: legal documents and raw document bytes must not be committed, and each document's `*.yaml` pointer belongs under `blobs/`",
            ));
        }
    }
    if first == POINTER_DIRECTORY && is_file && !crate::document_sync::is_pointer_path(path) {
        errors.push(Finding::at(
            path,
            "`blobs/` holds only `*.yaml` document pointers; the bytes they name belong under the Git-ignored `documents/`",
        ));
    }
    if let Some(component) = components
        .iter()
        .find(|component| FORBIDDEN_COMPONENTS.contains(&component.as_str()))
    {
        errors.push(Finding::at(
            path,
            format!("forbidden `{component}` path; repositories hold source, never client material or build output"),
        ));
    }
    if is_file {
        let name = path
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default();
        if name == ".env" || name.starts_with(".env.") || name.starts_with("answers.") {
            errors.push(Finding::at(
                path,
                "client answers and environment secrets must not be committed",
            ));
        }
        if FORBIDDEN_CREDENTIAL_EXTENSIONS.contains(&extension) {
            errors.push(Finding::at(
                path,
                "credential material must not be committed",
            ));
        }
        if FORBIDDEN_DOCUMENT_EXTENSIONS.contains(&extension) {
            errors.push(Finding::at(
                path,
                "legal documents and rendered output must not be committed",
            ));
        }
    }
}

/// Hold `AGENTS.md` to the one canonical contract every Project repository
/// receives. Project-specific coordinates belong in `navigator.yaml` and the
/// live Project row; checked-in agent instructions must not vary by matter.
fn validate_agent_contract(root: &Path, errors: &mut Vec<Finding>, warnings: &mut Vec<Finding>) {
    let agents_path = root.join("AGENTS.md");
    let Ok(agents) = fs::read_to_string(&agents_path) else {
        errors.push(Finding::at(
            agents_path,
            "missing required AGENTS.md; it is the agent contract for this repository",
        ));
        return;
    };
    if agents != AGENT_CONTRACT_BASE {
        errors.push(Finding::at(
            agents_path,
            format!(
                "AGENTS.md must match the canonical contract embedded in Navigator CLI {}; \
                 restore it with `navigator project repository sync-skills`",
                crate::cli_version()
            ),
        ));
    }
    let _ = warnings;
}

/// Require the checked-in Claude project permissions that deny email delivery
/// tools while leaving draft creation available.
fn validate_claude_settings(root: &Path, errors: &mut Vec<Finding>) {
    let path = root.join(CLAUDE_SETTINGS);
    if fs::read(&path).ok().as_deref() != Some(CLAUDE_SETTINGS_CONTENTS.as_bytes()) {
        errors.push(Finding::at(
            path,
            format!(
                "`{CLAUDE_SETTINGS}` must match Navigator's canonical email-send denial policy"
            ),
        ));
    }
}

/// Hold a Project repository's `navigator.yaml` to the closed key set and
/// value shapes [`super::manifest::lint`] owns. There is no per-repository
/// exemption mechanism.
fn validate_manifest(root: &Path, errors: &mut Vec<Finding>, warnings: &mut Vec<Finding>) -> bool {
    let mut valid = true;
    for finding in super::manifest::lint(root) {
        if !finding.warning {
            valid = false;
        }
        let converted = Finding::at(
            finding.path,
            format!("{}: {}", finding.code, finding.message),
        );
        if finding.warning {
            warnings.push(converted);
        } else {
            errors.push(converted);
        }
    }
    valid
}

/// A synced skill that is missing, or whose content has drifted from the
/// canonical copy.
///
/// Freshness is judged against the copy compiled into *this* binary, not a
/// live `.agents/skills` clone or a fetch of the pinned release. That mirrors
/// [`validate_workflow`]'s own pin check exactly (see ENG-356): CI runs the
/// validate action at the version the gate pins, so the binary performing
/// this comparison already *is* "the canonical copy at the pinned CLI
/// version" in the one place this check runs for real.
///
/// ## `.agents/` is the opt-in, and it is opt-in to the whole catalog
///
/// Absence of `templates/` or `portal/` is silent: not adopted is not broken.
/// That reasoning stops holding once a repository has an `.agents/` directory,
/// because then an agent *is* working in it under whatever skills it finds,
/// and the ones it does not find are the conventions nobody told it about.
///
/// The catalog carries conventions that would otherwise live only in a
/// comment. `deadline` is the worked example: a rule that a date is recorded
/// once through `navigator project deadline`, kept as a header comment, cannot
/// stop the file it sits in from hand-writing a second copy. A convention an
/// agent cannot see drifts.
///
/// So: no `.agents/` directory, no findings — a repository that has not
/// adopted agent tooling is not failed for it. With one, every skill in the
/// catalog is required, copied byte-for-byte from Navigator's own
/// `.agents/skills/`.
///
/// This only reaches a repository when it bumps the validate action's pin, so
/// adoption stays staged rather than turning the fleet red at once.
fn validate_skills(root: &Path, errors: &mut Vec<Finding>) {
    let agent_directory = root.join(".agents");
    if !agent_directory.is_dir() {
        return;
    }
    for (name, canonical) in SYNCED_SKILLS {
        let path = root.join(".agents/skills").join(name).join("SKILL.md");
        match fs::read_to_string(&path) {
            Ok(contents) if contents == *canonical => {}
            Ok(_) => errors.push(Finding::at(
                &path,
                format!("synced skill `{name}` has drifted from the canonical copy"),
            )),
            Err(_) => errors.push(Finding::at(
                &path,
                format!(
                    "this repository has an `.agents/` directory but is missing synced skill \
                     `{name}`"
                ),
            )),
        }
    }
}

/// Where a Project repository's `ci.yml`/`cd.yml` resolve the reusable
/// project-gate/project-publish workflows from today.
///
/// The sample Project repositories are private in `neon-law`, and this source
/// repository grants them Actions access. GitHub Actions does not follow
/// redirects for `uses:` references, so the old source slug is invalid after
/// transfer. [`workflow_for`] and [`cd_workflow_for`] still accept an explicit
/// `workflow_home` for forks with their own workflow host.
pub(crate) const DEFAULT_WORKFLOW_HOME: &str = "neon-law/navigator";

/// The `uses:` prefix a `ci.yml` job calling the reusable Project gate
/// workflow must start with, for an arbitrary `workflow_home`.
///
/// At [`DEFAULT_WORKFLOW_HOME`] this is the transferred repository's literal,
/// `neon-law/navigator/.github/workflows/project-gate.yml@` — a
/// test in this module pins that equality down so the two can never drift
/// apart silently.
pub(crate) fn gate_workflow_prefix(workflow_home: &str) -> String {
    format!("{workflow_home}/.github/workflows/project-gate.yml@")
}

/// Same as [`gate_workflow_prefix`], for the `publish` job's reusable
/// project-publish workflow.
pub(crate) fn publish_workflow_prefix(workflow_home: &str) -> String {
    format!("{workflow_home}/.github/workflows/project-publish.yml@")
}

/// Just enough of a workflow to check its trigger, its top-level
/// permissions, and each job's reusable-workflow call.
///
/// Unlike the old permissive reader this replaces, every field named here is
/// closed against: an unrecognized job, an added trigger, or a widened
/// `permissions` block is exactly what [`validate_workflow`] and
/// [`validate_cd_workflow`] exist to reject. `#[serde(default)]` on each
/// field only lets a workflow that omits it (no `on:`, no `permissions:`)
/// parse at all — the validators still treat the omission as a finding where
/// the canonical generator would have written something.
#[derive(serde::Deserialize)]
struct Workflow {
    #[serde(default, rename = "on")]
    on: Option<serde_yaml::Value>,
    #[serde(default)]
    permissions: Option<BTreeMap<String, String>>,
    #[serde(default)]
    jobs: BTreeMap<String, WorkflowJob>,
}

#[derive(serde::Deserialize)]
struct WorkflowJob {
    #[serde(default)]
    uses: Option<String>,
    #[serde(default)]
    with: BTreeMap<String, serde_yaml::Value>,
    #[serde(default)]
    needs: Option<serde_yaml::Value>,
    #[serde(default)]
    permissions: Option<BTreeMap<String, String>>,
}

/// Every trigger name an `on:` value declares, reading either the mapping
/// shape (`on:\n  pull_request:`) or the shorthand sequence
/// (`on: [pull_request]`) as the same set — GitHub Actions treats them as the
/// same trigger, so a caller that prefers one spelling over the other is a
/// harmless formatting difference, not a structural one.
fn trigger_names(on: &serde_yaml::Value) -> Option<Vec<String>> {
    match on {
        serde_yaml::Value::Mapping(map) => Some(
            map.keys()
                .filter_map(|key| key.as_str().map(str::to_string))
                .collect(),
        ),
        serde_yaml::Value::Sequence(sequence) => Some(
            sequence
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect(),
        ),
        serde_yaml::Value::String(value) => Some(vec![value.clone()]),
        _ => None,
    }
}

/// The CI caller triggers on `pull_request` and nothing else.
///
/// `pull_request_target` is named explicitly because it is not a formatting
/// variant of `pull_request` — it runs with the base repository's token and
/// secrets against a fork's checked-out code, which is exactly the
/// privilege escalation a thin, unreviewed caller must not be able to gain by
/// a one-word edit.
fn validate_ci_trigger(path: &Path, on: Option<&serde_yaml::Value>, errors: &mut Vec<Finding>) {
    let Some(names) = on.and_then(trigger_names) else {
        errors.push(Finding::at(
            path,
            "CI gate must trigger on `pull_request` and nothing else",
        ));
        return;
    };
    if names.iter().any(|name| name == "pull_request_target") {
        errors.push(Finding::at(
            path,
            "CI gate must trigger on `pull_request`, not `pull_request_target`, which runs \
             with the base repository's secrets and write token against a fork's code",
        ));
        return;
    }
    if names != ["pull_request"] {
        errors.push(Finding::at(
            path,
            format!("CI gate must trigger on exactly `pull_request`; found {names:?}"),
        ));
    }
}

/// The CD caller triggers on a push to `main` and `workflow_dispatch`, and
/// nothing else — the shorthand sequence spelling is refused here rather than
/// accepted the way [`validate_ci_trigger`] accepts it, because that shape
/// cannot express `branches: [main]`: a `cd.yml` that triggers on every
/// branch push mints the deployment token far wider than the manifest's one
/// host was ever granted.
fn validate_cd_trigger(path: &Path, on: Option<&serde_yaml::Value>, errors: &mut Vec<Finding>) {
    let Some(serde_yaml::Value::Mapping(map)) = on else {
        errors.push(Finding::at(
            path,
            "CD workflow must trigger on a `push` to `main` and `workflow_dispatch`",
        ));
        return;
    };
    let mut names: Vec<String> = map
        .keys()
        .filter_map(|key| key.as_str().map(str::to_string))
        .collect();
    if names
        .iter()
        .any(|name| name == "pull_request" || name == "pull_request_target")
    {
        errors.push(Finding::at(
            path,
            "CD workflow must not trigger on a pull request event; only a push to `main` and \
             `workflow_dispatch` mint the deployment token",
        ));
        return;
    }
    names.sort();
    if names != ["push".to_string(), "workflow_dispatch".to_string()] {
        errors.push(Finding::at(
            path,
            format!(
                "CD workflow must trigger on exactly `push` and `workflow_dispatch`; found {names:?}"
            ),
        ));
        return;
    }
    let branches: Vec<String> = map
        .get("push")
        .and_then(|push| push.get("branches"))
        .and_then(|value| value.as_sequence())
        .map(|sequence| {
            sequence
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if branches != ["main".to_string()] {
        errors.push(Finding::at(
            path,
            format!(
                "CD workflow's `push` trigger must be scoped to `branches: [main]`; found {branches:?}"
            ),
        ));
    }
}

/// When declared, the CI caller grants only checkout and OIDC token
/// permissions. Older repository fixtures may omit the block; generated
/// callers declare this exact minimum, and no declared write permission is
/// accepted.
const CI_PERMISSIONS: &[(&str, &str)] = &[("contents", "read"), ("id-token", "write")];

fn validate_ci_permissions(
    path: &Path,
    permissions: Option<&BTreeMap<String, String>>,
    errors: &mut Vec<Finding>,
) {
    let Some(found) = permissions else {
        return;
    };
    let expected: BTreeMap<String, String> = CI_PERMISSIONS
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    if *found != expected {
        errors.push(Finding::at(
            path,
            format!(
                "CI gate must declare exactly `permissions: {{contents: read, id-token: write}}`; found {found:?}"
            ),
        ));
    }
}

/// The exact scope the CD caller's token needs: read the checkout and mint
/// the OIDC token the reusable publisher exchanges for a deployment session.
/// Nothing wider — `contents: write` in particular is the privilege
/// escalation a compromised or careless edit would reach for.
const CD_PERMISSIONS: &[(&str, &str)] = &[("contents", "read"), ("id-token", "write")];

fn validate_cd_permissions(
    path: &Path,
    permissions: Option<&BTreeMap<String, String>>,
    errors: &mut Vec<Finding>,
) {
    let found = permissions.cloned().unwrap_or_default();
    let expected: BTreeMap<String, String> = CD_PERMISSIONS
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    if found != expected {
        errors.push(Finding::at(
            path,
            format!(
                "CD workflow must declare exactly `permissions: {{contents: read, id-token: write}}`; found {found:?}"
            ),
        ));
    }
}

/// Every name `needs:` lists, reading either the bare-string shape
/// (`needs: gate`) or the sequence shape (`needs: [gate]`) as the same
/// dependency.
fn needs_list(needs: Option<&serde_yaml::Value>) -> Vec<String> {
    match needs {
        Some(serde_yaml::Value::String(value)) => vec![value.clone()],
        Some(serde_yaml::Value::Sequence(sequence)) => sequence
            .iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

/// One job's reusable-workflow call: it names no `permissions` of its own, no
/// `with:` block of its own, and it calls `expected_prefix` at a pinned
/// release that agrees with the repository's manifest.
///
/// The reusable workflow reads `project`/`host` from the caller's checked-out
/// `navigator.yaml` directly, so a caller has nothing left to repeat there —
/// any `with:` block at all is a duplicate of the manifest and is rejected
/// outright, closing off the same duplicate the one-job rule on `ci.yml`
/// already closes.
///
/// Shared by [`validate_workflow`] (the sole `ci` job, calling
/// [`gate_workflow_prefix`]) and [`validate_cd_workflow`] (the `gate` and
/// `publish` jobs, calling [`gate_workflow_prefix`] and
/// [`publish_workflow_prefix`] respectively).
fn validate_gate_call(
    path: &Path,
    job_name: &str,
    job: &WorkflowJob,
    expected_prefix: &str,
    manifest: Option<&super::manifest::Manifest>,
    errors: &mut Vec<Finding>,
) {
    if job.permissions.as_ref().is_some_and(|map| !map.is_empty()) {
        errors.push(Finding::at(
            path,
            format!("job `{job_name}` must not declare its own `permissions`"),
        ));
    }
    let Some(uses) = job.uses.as_deref().map(str::trim) else {
        errors.push(Finding::at(
            path,
            format!("job `{job_name}` must call `{expected_prefix}` at a pinned release"),
        ));
        return;
    };
    let Some(action_version) = uses.strip_prefix(expected_prefix) else {
        errors.push(Finding::at(
            path,
            format!(
                "job `{job_name}` must call `{expected_prefix}` at a pinned release; found `{uses}`"
            ),
        ));
        return;
    };
    if !job.with.is_empty() {
        errors.push(Finding::at(
            path,
            format!(
                "job `{job_name}` must not declare a `with:` block; the reusable workflow reads `project`/`host` from navigator.yaml"
            ),
        ));
        return;
    }
    if let Some(expected_version) = manifest.and_then(|manifest| manifest.version.as_deref()) {
        if action_version != expected_version {
            errors.push(Finding::at(
                path,
                format!(
                    "job `{job_name}` ref `{action_version}` must equal manifest version `{expected_version}`"
                ),
            ));
        }
    }
    if !is_release_tag(action_version) {
        errors.push(Finding::at(
            path,
            format!("job `{job_name}` ref `{action_version}` must be {RELEASE_TAG_SHAPE}"),
        ));
    }
}

/// Hold the CI gate to exactly one job, named [`REQUIRED_CHECK`], triggered
/// only by `pull_request`, carrying the minimal checkout/OIDC `permissions`, and calling
/// the pinned reusable project-gate workflow — resolved from `workflow_home`
/// — at an exact release tag matching the repository manifest.
fn validate_workflow(
    path: &Path,
    contents: &str,
    manifest: Option<&super::manifest::Manifest>,
    workflow_home: &str,
    errors: &mut Vec<Finding>,
) {
    let workflow: Workflow = match serde_yaml::from_str(contents) {
        Ok(workflow) => workflow,
        Err(error) => {
            errors.push(Finding::at(
                path,
                format!("CI gate is not valid YAML: {error}"),
            ));
            return;
        }
    };

    validate_ci_trigger(path, workflow.on.as_ref(), errors);
    validate_ci_permissions(path, workflow.permissions.as_ref(), errors);

    if workflow.jobs.len() != 1 {
        let mut names: Vec<&String> = workflow.jobs.keys().collect();
        names.sort();
        errors.push(Finding::at(
            path,
            format!(
                "CI gate must define exactly one job, the pinned project-gate caller named \
                 `{REQUIRED_CHECK}`; found {names:?}"
            ),
        ));
        return;
    }
    let Some(job) = workflow.jobs.get(REQUIRED_CHECK) else {
        errors.push(Finding::at(
            path,
            format!("CI gate's one job must be named `{REQUIRED_CHECK}`"),
        ));
        return;
    };
    validate_gate_call(
        path,
        REQUIRED_CHECK,
        job,
        &gate_workflow_prefix(workflow_home),
        manifest,
        errors,
    );
}

/// Hold the CD workflow to exactly the `gate` and `publish` jobs, triggered
/// only by a push to `main` and `workflow_dispatch`, carrying exactly
/// `permissions: {contents: read, id-token: write}`, `publish` needing
/// `gate`, and each job calling its pinned reusable workflow — resolved from
/// `workflow_home` — at an exact release tag matching the repository
/// manifest.
fn validate_cd_workflow(
    path: &Path,
    contents: &str,
    manifest: Option<&super::manifest::Manifest>,
    workflow_home: &str,
    errors: &mut Vec<Finding>,
) {
    let workflow: Workflow = match serde_yaml::from_str(contents) {
        Ok(workflow) => workflow,
        Err(error) => {
            errors.push(Finding::at(
                path,
                format!("CD workflow is not valid YAML: {error}"),
            ));
            return;
        }
    };

    validate_cd_trigger(path, workflow.on.as_ref(), errors);
    validate_cd_permissions(path, workflow.permissions.as_ref(), errors);

    let mut names: Vec<String> = workflow.jobs.keys().cloned().collect();
    names.sort();
    if names != vec!["gate".to_string(), "publish".to_string()] {
        errors.push(Finding::at(
            path,
            format!(
                "CD workflow must define exactly the `gate` and `publish` jobs; found {names:?}"
            ),
        ));
        return;
    }

    if let Some(gate) = workflow.jobs.get("gate") {
        validate_gate_call(
            path,
            "gate",
            gate,
            &gate_workflow_prefix(workflow_home),
            manifest,
            errors,
        );
    }
    if let Some(publish) = workflow.jobs.get("publish") {
        validate_gate_call(
            path,
            "publish",
            publish,
            &publish_workflow_prefix(workflow_home),
            manifest,
            errors,
        );
        let needs = needs_list(publish.needs.as_ref());
        if needs != vec!["gate".to_string()] {
            errors.push(Finding::at(
                path,
                format!("CD workflow's `publish` job must declare `needs: gate`; found {needs:?}"),
            ));
        }
    }
}

fn is_release_tag(version: &str) -> bool {
    crate::devx::registry::is_release_tag(version)
}

/// What `is_release_tag` requires, spelled out once so `scaffold`'s CLI-time
/// refusal, `validate_workflow`'s CI-time finding, and
/// [`super::super::devx::github_setup`]'s own refusal describe the one rule in
/// one sentence rather than three that are free to drift.
pub(crate) const RELEASE_TAG_SHAPE: &str =
    "an exact release tag, such as YY.M.D or YY.M.D-hotfix.N — never `main` or `latest`";

fn validate_templates(
    root: &Path,
    blueprint_directory: rules::project_blueprints::BlueprintDirectory,
    errors: &mut Vec<Finding>,
    warnings: &mut Vec<Finding>,
) -> usize {
    let name = blueprint_directory.as_str();
    let directory = root.join(name);
    let mut paths = Vec::new();
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) => {
            errors.push(Finding::at(directory, format!("read {name}: {error}")));
            return 0;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                errors.push(Finding::at(
                    &directory,
                    format!("read {name} entry: {error}"),
                ));
                continue;
            }
        };
        let path = entry.path();
        if path.is_dir() {
            errors.push(Finding::at(
                path,
                format!("Project templates must be direct `{name}/<code>.md` files"),
            ));
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("md") {
            paths.push(path);
        } else {
            errors.push(Finding::at(
                path,
                format!("only Markdown template blueprints belong in `{name}/`"),
            ));
        }
    }
    paths.sort();
    if paths.is_empty() {
        errors.push(Finding::at(
            directory,
            format!("`{name}/` is present but empty; at least one `{name}/<code>.md` blueprint is required"),
        ));
        return 0;
    }

    let rules = rules::navigator_default_rules_with_codes(&rules::canonical_question_codes());
    let mut declared_codes = BTreeMap::new();
    for path in &paths {
        lint_project_template(path, &rules, &mut declared_codes, errors, warnings);
    }
    paths.len()
}

/// `Y014` — a Project repository's document pointers live under `blobs/`,
/// never beside their bytes under the Git-ignored `documents/`.
///
/// The legacy layout committed `documents/<slug>.yaml` next to the bytes at
/// `documents/<slug>`, guarded by a four-line `documents/.gitignore`. A
/// pointer still there is one the root `.gitignore` now hides from the next
/// `git add`, so it must move.
pub const DOCUMENT_GITIGNORE_CODE: &str = "Y014";

/// When `documents/` still holds pointers or the retired `documents/.gitignore`,
/// local `project gate` moves them to `blobs/`
/// ([`crate::document_sync::migrate_legacy_layout`]); `--ci` reports and
/// leaves the checkout alone.
///
/// For one release (LAW-189) `--ci` reports the legacy layout as a warning,
/// so an upgraded Project stays green until its move is committed.
fn validate_documents_gitignore(
    root: &Path,
    write_fixes: bool,
    errors: &mut Vec<Finding>,
    warnings: &mut Vec<Finding>,
) {
    let documents = root.join(DOCUMENT_DIRECTORY);
    let (legacy, guard) = match crate::document_sync::legacy_layout(root) {
        Ok(found) => found,
        Err(error) => {
            errors.push(Finding::at(
                &documents,
                format!("{DOCUMENT_GITIGNORE_CODE}: could not read `documents/`: {error:#}"),
            ));
            return;
        }
    };
    if legacy.is_empty() && guard.is_none() {
        return;
    }
    if write_fixes {
        match crate::document_sync::migrate_legacy_layout(root) {
            Ok(moved) => println!(
                "fixed {}: moved {moved} pointer(s) to {POINTER_DIRECTORY}/; commit the move",
                documents.display()
            ),
            Err(error) => errors.push(Finding::at(
                &documents,
                format!("{DOCUMENT_GITIGNORE_CODE}: could not move the legacy pointers to `blobs/`: {error:#}"),
            )),
        }
        return;
    }
    warnings.push(Finding::at(
        documents,
        format!(
            "{DOCUMENT_GITIGNORE_CODE}: {} still sit under `documents/`; pointers belong under `blobs/`. Run `navigator project sync` or `navigator project repository migrate-documents` and commit the move; the next release refuses it",
            crate::document_sync::describe_legacy_layout(legacy.len(), guard.is_some())
        ),
    ));
}

/// `Y024` — a Project repository commits only Markdown and YAML (ENG-1136).
///
/// Its pages are declared under `pages:` in `navigator.yaml` and drawn by
/// Navigator; its documents live in Navigator's store. So a tracked or
/// stageable file is `*.md`, `*.yml`, or `*.yaml`, or it is one of
/// [`CANONICAL_TRACKED_FILES`], whose bytes the release itself supplies. No
/// `package.json` or lockfile, no TypeScript, JavaScript, Python, or shell, no
/// `tests/` code, and no binaries. Each offending path is its own finding,
/// so the refusal names every file to delete.
pub const TRACKED_FILE_CODE: &str = "Y024";

/// The extensions a Project repository commits, matched exactly.
const TRACKED_EXTENSIONS: &[&str] = &["md", "yml", "yaml"];

/// Node package-manager files that happen to be YAML, refused by filename
/// at any depth: a lockfile is tooling, whatever its syntax.
const REFUSED_YAML_TOOLING: &[&str] = &["pnpm-lock.yaml", "pnpm-workspace.yaml", ".yarnrc.yml"];

/// The canonical `.gitattributes`: every text file is normalized to LF.
pub(crate) const CANONICAL_GITATTRIBUTES: &str = "* text=auto eol=lf\n";

/// The only tracked files that are neither Markdown nor YAML, each paired
/// with the bytes this release holds it to, or `None` where another pass
/// already owns the byte check and names its own fix: `.github/CODEOWNERS`
/// (`validate_codeowners`) and `.claude/settings.json`
/// (`validate_claude_settings`).
///
/// The three licence files are the ones the scaffold writes, held to
/// Navigator's own texts; a Firm that writes its own licence writes
/// `LICENSE.md`.
const CANONICAL_TRACKED_FILES: &[(&str, Option<&str>)] = &[
    (".gitignore", Some(ROOT_GITIGNORE)),
    (".gitattributes", Some(CANONICAL_GITATTRIBUTES)),
    (".github/CODEOWNERS", None),
    (CLAUDE_SETTINGS, None),
    ("LICENSE", Some(include_str!("../../../LICENSE"))),
    ("LICENSE-MIT", Some(include_str!("../../../LICENSE-MIT"))),
    (
        "LICENSE-APACHE-2",
        Some(include_str!("../../../LICENSE-APACHE-2")),
    ),
];

/// Whether `relative` (slash-separated) may be committed at all.
fn tracked_file_admitted(relative: &str) -> bool {
    if CANONICAL_TRACKED_FILES
        .iter()
        .any(|(canonical, _)| *canonical == relative)
    {
        return true;
    }
    let name = relative.rsplit('/').next().unwrap_or(relative);
    if REFUSED_YAML_TOOLING.contains(&name) {
        return false;
    }
    name.rsplit_once('.').is_some_and(|(stem, extension)| {
        !stem.is_empty() && TRACKED_EXTENSIONS.contains(&extension)
    })
}

/// Hold every git-tracked or stageable file to [`TRACKED_FILE_CODE`], and
/// each present [`CANONICAL_TRACKED_FILES`] entry to its canonical bytes.
/// Local `project gate` writes a drifted `.gitignore` or `.gitattributes`
/// back to canonical; `--ci` reports it.
fn validate_tracked_files(
    root: &Path,
    write_fixes: bool,
    errors: &mut Vec<Finding>,
    warnings: &mut Vec<Finding>,
) {
    let files = match git_tracked_and_stageable_files(root) {
        Ok(files) => files,
        Err(error) => {
            errors.push(Finding::at(
                root,
                format!("{TRACKED_FILE_CODE}: could not enumerate git-tracked and stageable files: {error}"),
            ));
            return;
        }
    };
    for path in files {
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        // Already deleted from the working tree: committing that removes it.
        if fs::symlink_metadata(&path).is_err() {
            continue;
        }
        let relative = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if !tracked_file_admitted(&relative) {
            errors.push(Finding::at(
                &path,
                format!(
                    "{TRACKED_FILE_CODE}: `{relative}` cannot be committed; a Project \
                     repository commits only `*.md`, `*.yml`, and `*.yaml` (no package-manager \
                     lockfile or config, whatever its syntax), plus the canonical \
                     `.gitignore`, `.gitattributes`, \
                     `.github/CODEOWNERS`, and `.claude/settings.json`. Delete it: matter pages \
                     are declared under `pages:` in `navigator.yaml`, documents live in \
                     Navigator (`navigator project sync`), and the only CLI a Project uses is \
                     the public `navigator` release"
                ),
            ));
        }
    }
    for (relative, canonical) in CANONICAL_TRACKED_FILES {
        let Some(canonical) = canonical else {
            continue;
        };
        let path = root.join(relative);
        let Ok(current) = fs::read(&path) else {
            continue;
        };
        if current == canonical.as_bytes() {
            continue;
        }
        // LAW-181: the root `.gitignore` is the Project's own file, so the
        // gate reports it and never rewrites it.
        if *relative == ".gitignore" {
            root_gitignore_drift(&path, &current, errors, warnings);
            continue;
        }
        // A licence is a grant of rights; the gate never rewrites one.
        let licence = relative.starts_with("LICENSE");
        if licence {
            errors.push(Finding::at(
                &path,
                format!(
                    "{TRACKED_FILE_CODE}: `{relative}` must match the text this release ships \
                     byte for byte; a Firm's own licence is `LICENSE.md`"
                ),
            ));
            continue;
        }
        if write_fixes {
            match fs::write(&path, canonical) {
                Ok(()) => println!("fixed {}", path.display()),
                Err(error) => errors.push(Finding::at(
                    &path,
                    format!("{TRACKED_FILE_CODE}: could not write canonical `{relative}`: {error}"),
                )),
            }
            continue;
        }
        errors.push(Finding::at(
            &path,
            format!(
                "{TRACKED_FILE_CODE}: `{relative}` must match this release's canonical copy byte \
                 for byte; run `navigator project gate` locally to write it"
            ),
        ));
    }
}

/// The [`ROOT_GITIGNORE`] lines `contents` lacks, as written there. Comments
/// and blank lines do not count, and neither do lines the Project added.
fn missing_root_gitignore_lines(contents: &str) -> Vec<&'static str> {
    let present: std::collections::BTreeSet<&str> = contents.lines().map(str::trim).collect();
    ROOT_GITIGNORE
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter(|line| !present.contains(line))
        .collect()
}

/// Report a root `.gitignore` that lacks canonical lines: an error when it
/// lacks one of [`ROOT_GITIGNORE_PROTECTIONS`], a warning otherwise. A file
/// that only adds lines of its own is not drift.
fn root_gitignore_drift(
    path: &Path,
    current: &[u8],
    errors: &mut Vec<Finding>,
    warnings: &mut Vec<Finding>,
) {
    let contents = String::from_utf8_lossy(current);
    let missing = missing_root_gitignore_lines(&contents);
    if missing.is_empty() {
        return;
    }
    let quote = |lines: &[&str]| {
        lines
            .iter()
            .map(|line| format!("`{line}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let unprotected: Vec<&str> = missing
        .iter()
        .copied()
        .filter(|line| ROOT_GITIGNORE_PROTECTIONS.contains(line))
        .collect();
    let repair = "run `navigator project repository sync-skills` to restore them; the gate never rewrites this file";
    if unprotected.is_empty() {
        warnings.push(Finding::at(
            path,
            format!(
                "{TRACKED_FILE_CODE}: `.gitignore` has drifted from this release's copy and lacks {}; {repair}",
                quote(&missing)
            ),
        ));
    } else {
        errors.push(Finding::at(
            path,
            format!(
                "{TRACKED_FILE_CODE}: `.gitignore` lacks {}; without {} a document's bytes, a legal file, or a handoff note can be committed; {repair}",
                quote(&missing),
                quote(&unprotected)
            ),
        ));
    }
}

/// Restore the [`ROOT_GITIGNORE`] lines a Project's root `.gitignore` lacks,
/// keeping every line it added after them: dropping an ignore rule is how a
/// legal file reaches Git (LAW-181). A missing file is written whole.
fn repair_root_gitignore(root: &Path, repaired: &mut Vec<String>) -> anyhow::Result<()> {
    let path = root.join(".gitignore");
    let current = match fs::read_to_string(&path) {
        Ok(current) => current,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.into()),
    };
    if !current.is_empty() && missing_root_gitignore_lines(&current).is_empty() {
        return Ok(());
    }
    let canonical: std::collections::BTreeSet<&str> =
        ROOT_GITIGNORE.lines().map(str::trim).collect();
    let kept: Vec<&str> = current
        .lines()
        .filter(|line| {
            let line = line.trim();
            !line.is_empty() && !line.starts_with('#') && !canonical.contains(line)
        })
        .collect();
    let mut contents = ROOT_GITIGNORE.to_string();
    if !kept.is_empty() {
        contents.push_str("\n# Kept from this repository's own .gitignore.\n");
        for line in kept {
            contents.push_str(line);
            contents.push('\n');
        }
    }
    fs::write(&path, contents)?;
    repaired.push(".gitignore".to_string());
    Ok(())
}

/// `navigator project gate --tracked-files`: only [`TRACKED_FILE_CODE`], for
/// the shared workflow's required `ci` job. Nothing is written. Prints each
/// finding and returns how many there were.
pub(crate) fn validate_tracked_files_only(root: &Path) -> usize {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    validate_tracked_files(root, false, &mut errors, &mut warnings);
    for warning in &warnings {
        println!("{}: warning: {}", warning.path.display(), warning.message);
    }
    for error in &errors {
        eprintln!("{}: error: {}", error.path.display(), error.message);
    }
    println!(
        "{TRACKED_FILE_CODE}: {} finding(s) against the Markdown-and-YAML rule",
        errors.len()
    );
    errors.len()
}

/// `Y010` — a Project template names `Neon Law` with a corporate suffix that is
/// not the firm entity of record.
///
/// `Neon Law` is a mark, and a mark may head any sentence in a template. The
/// mark followed by `, Inc.`, `LLC`, `PLLC`, or another corporate suffix is a
/// claim about which legal person the client is engaging, and on an
/// engagement letter that claim sits above a signature block. The one legal
/// person is [`store::seed::FIRM_ENTITY_NAME`]; this compares against that
/// constant and never against a literal of its own, so the gate cannot become
/// a further spelling. `Neon Law IP LLC`, the Licensor, is a different name
/// with a different word after the mark, not a suffix case.
pub const ENTITY_CODE: &str = "Y010";
/// `Y017` — an onboarding or offboarding template must name the Firm by its
/// legal entity, not describe the mark as its trade name.
pub const FIRM_TRADE_NAME_CODE: &str = "Y017";
/// `Y018` — a California onboarding or offboarding template must not name an
/// LLC as the lawyer providing legal services. The firm of record and the
/// individual California lawyer both remain valid parties.
pub const CALIFORNIA_LAW_FIRM_ENTITY_CODE: &str = "Y018";

/// The mark a template may trade under without naming a legal person.
const FIRM_MARK: &str = "Neon Law";

/// Corporate suffixes that turn the mark into an entity claim. Longer
/// spellings precede the shorter spelling they contain, so `Incorporated` is
/// read whole rather than as `Inc` with letters after it.
const CORPORATE_SUFFIXES: &[&str] = &[
    "Incorporated",
    "Inc.",
    "Inc",
    "P.L.L.C.",
    "PLLC",
    "L.L.C.",
    "LLC",
    "L.L.P.",
    "LLP",
    "Corporation",
    "Corp.",
    "Corp",
    "Limited",
    "Ltd.",
    "Ltd",
    "P.C.",
    "PC",
    "Co.",
];

/// Every `(line, spelling)` at which `contents` names the mark with a
/// corporate suffix and the result is not `firm`, the entity of record.
///
/// Lines are one-based, as the finding prints them. The spelling is the mark
/// plus the suffix as the template wrote it, with one space between, so the
/// message names what the file says rather than a normalized form.
fn misnamed_firm_entities(contents: &str, firm: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    for (index, line) in contents.lines().enumerate() {
        for (at, _) in line.match_indices(FIRM_MARK) {
            let rest = &line[at + FIRM_MARK.len()..];
            let Some(suffix) = entity_suffix(rest) else {
                continue;
            };
            let spelled = format!("{FIRM_MARK}{suffix}");
            if spelled != firm {
                found.push((index + 1, spelled));
            }
        }
    }
    found
}

/// Every `(line, spelling)` where an onboarding/offboarding template presents
/// Neon Law as the Firm's trade name. Contact-channel references remain valid.
fn firm_trade_name_claims(contents: &str) -> Vec<(usize, String)> {
    let Some(frontmatter) = rules::frontmatter::extract(contents) else {
        return Vec::new();
    };
    let kind = rules::frontmatter::field(frontmatter, "kind");
    if !matches!(kind.as_deref(), Some("onboarding" | "offboarding")) {
        return Vec::new();
    }

    let mut found = Vec::new();
    for (index, line) in contents.lines().enumerate() {
        let normalized = line
            .chars()
            .filter(|character| !matches!(character, '*' | '_' | '`'))
            .collect::<String>()
            .to_ascii_lowercase();
        let normalized = normalized.split_whitespace().collect::<Vec<_>>().join(" ");
        let claim = normalized.contains("neon law, a practice of shook law pc")
            || normalized.contains("trading as neon law")
            || normalized.contains("doing business as neon law")
            || normalized.contains("d/b/a neon law")
            || normalized.contains("shook law pc (\"neon law\")")
            || normalized.contains("shook law pc ('neon law')")
            || normalized.trim_matches(|character: char| !character.is_ascii_alphanumeric())
                == "neon law";
        if claim {
            found.push((index + 1, "Neon Law".to_string()));
        }
    }
    found
}

/// Find firm-entity claims in California engagement and closing templates.
/// A corporate suffix is reportable only where the line assigns the lawyer's
/// role to that entity; ordinary references to an entity remain allowed.
fn california_law_firm_entity_claims(contents: &str) -> Vec<usize> {
    let Some(frontmatter) = rules::frontmatter::extract(contents) else {
        return Vec::new();
    };
    if rules::frontmatter::field(frontmatter, "jurisdiction").as_deref() != Some("CA")
        || !matches!(
            rules::frontmatter::field(frontmatter, "kind").as_deref(),
            Some("onboarding" | "offboarding")
        )
    {
        return Vec::new();
    }

    // The firm's name without its entity suffix, so a bare signature heading
    // naming the firm as an LLC (`**Shook Law PLLC**`) is a party claim.
    let firm = store::seed::FIRM_ENTITY_NAME.to_ascii_lowercase();
    let firm_stem = firm
        .rsplit_once(' ')
        .map_or(firm.as_str(), |(stem, _)| stem);
    contents
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let normalized = line
                .chars()
                .filter(|character| !matches!(character, '*' | '_' | '`'))
                .collect::<String>()
                .to_ascii_lowercase();
            let is_llc = normalized.contains(" llc") || normalized.contains(" pllc");
            let heading =
                normalized.trim_matches(|character: char| !character.is_ascii_alphanumeric());
            let is_party_claim = normalized.contains("thank you for choosing")
                || normalized.contains("this letter engages")
                || normalized.contains("confirms that")
                    && normalized.contains("completed its work")
                || heading.strip_prefix(firm_stem).is_some_and(|suffix| {
                    matches!(suffix.trim_start_matches(','), " llc" | " pllc")
                });
            (is_llc && is_party_claim).then_some(index + 1)
        })
        .collect()
}

/// The corporate suffix `rest` opens with — an optional comma, at least one
/// whitespace character, and one of [`CORPORATE_SUFFIXES`] ending at a word
/// boundary — rendered as `, Inc.` or ` PLLC`. `None` when the mark ran into
/// more letters (`Neon Lawyers`), ended the sentence (`Neon Law.`), or was
/// followed by any other word (`Neon Law IP LLC`, `Neon Law helps`).
fn entity_suffix(rest: &str) -> Option<String> {
    let (comma, rest) = match rest.strip_prefix(',') {
        Some(rest) => (",", rest),
        None => ("", rest),
    };
    let trimmed = rest.trim_start();
    if trimmed.len() == rest.len() {
        return None;
    }
    CORPORATE_SUFFIXES.iter().find_map(|suffix| {
        let head = trimmed.get(..suffix.len())?;
        if !head.eq_ignore_ascii_case(suffix) {
            return None;
        }
        let boundary = trimmed[suffix.len()..]
            .chars()
            .next()
            .is_none_or(|next| !next.is_alphanumeric());
        boundary.then(|| format!("{comma} {head}"))
    })
}

fn lint_project_template(
    path: &Path,
    rules: &[Box<dyn rules::Rule>],
    declared_codes: &mut BTreeMap<String, PathBuf>,
    errors: &mut Vec<Finding>,
    warnings: &mut Vec<Finding>,
) {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) => {
            errors.push(Finding::at(path, format!("read template: {error}")));
            return;
        }
    };
    let filename = path.file_name().map_or_else(PathBuf::new, PathBuf::from);
    let source = rules::SourceFile {
        path: filename,
        contents: contents.clone(),
    };
    for violation in rules.iter().flat_map(|rule| rule.lint(&source)) {
        let finding = Finding::at(path, format!("{}: {}", violation.code, violation.message));
        if rules::severity_for_code(violation.code) == rules::Severity::Error {
            errors.push(finding);
        } else {
            warnings.push(finding);
        }
    }
    if rules::frontmatter::extract(&contents)
        .and_then(|fm| rules::frontmatter::field(fm, "kind"))
        .is_some_and(|kind| kind == "pleading")
    {
        if let Err(message) = super::matter::caption_for(path, &contents) {
            errors.push(Finding::at(
                path,
                format!("{}: {message}", super::matter::CODE),
            ));
        }
    }
    for (line, spelling) in misnamed_firm_entities(&contents, store::seed::FIRM_ENTITY_NAME) {
        errors.push(Finding::at(
            path,
            format!(
                "{ENTITY_CODE}: line {line} names `{spelling}` as the firm, but the entity of \
                 record is `{}`; a template that gives the mark a corporate suffix must name \
                 the contracting entity exactly",
                store::seed::FIRM_ENTITY_NAME
            ),
        ));
    }
    for (line, spelling) in firm_trade_name_claims(&contents) {
        errors.push(Finding::at(
            path,
            format!(
                "{FIRM_TRADE_NAME_CODE}: line {line} names `{spelling}` as the Firm's trade name; use the Firm's legal entity, `{}`, as the contracting party",
                store::seed::FIRM_ENTITY_NAME
            ),
        ));
    }
    for line in california_law_firm_entity_claims(&contents) {
        errors.push(Finding::at(
            path,
            format!(
                "{CALIFORNIA_LAW_FIRM_ENTITY_CODE}: line {line} names an LLC as the law firm in a California template; name the firm of record, `{}`, or the individual California lawyer",
                store::seed::FIRM_ENTITY_NAME
            ),
        ));
    }
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    if let Some(first) = declared_codes.insert(stem.to_string(), path.to_path_buf()) {
        errors.push(Finding::at(
            path,
            format!(
                "duplicate template filename stem `{stem}`; first declared in {}",
                first.display()
            ),
        ));
    }
}

/// The one `AGENTS.md` every Project repository carries. Project-specific
/// coordinates belong in `navigator.yaml` and Navigator's live Project row,
/// not in checked-in agent instructions.
const AGENT_CONTRACT_BASE: &str = include_str!("agent_contract.md");

/// [`emit_scaffold_for`] a GitHub-backed Project.
#[cfg(test)]
pub(crate) fn emit_scaffold(
    root: &Path,
    code: &str,
    host: &str,
    lawyer_dri: &str,
    client_dri: &str,
) -> anyhow::Result<()> {
    emit_scaffold_for(
        root,
        code,
        host,
        lawyer_dri,
        client_dri,
        cloud::ForgeBackend::GitHub,
    )
}

/// Emit the canonical shell for a newly opened Project using resources
/// embedded in this CLI release. No checkout of Navigator is needed. A
/// Forgejo-backed Project also gets [`FORGEJO_GATE_WORKFLOW`].
pub(crate) fn emit_scaffold_for(
    root: &Path,
    code: &str,
    host: &str,
    lawyer_dri: &str,
    client_dri: &str,
    forge: cloud::ForgeBackend,
) -> anyhow::Result<()> {
    if !store::projects::is_valid_code(code) {
        anyhow::bail!("invalid immutable Project code `{code}`");
    }
    fs::create_dir_all(root)?;
    let files = [
        ("README.md", format!("# Project repository\n\nNavigator Project `{code}`.\n")),
        ("AGENTS.md", AGENT_CONTRACT_BASE.to_string()),
        (CLAUDE_SETTINGS, CLAUDE_SETTINGS_CONTENTS.to_string()),
        (".github/workflows/ci.yml", workflow_for(DEFAULT_WORKFLOW_HOME, crate::cli_version())),
        (".github/workflows/cd.yml", cd_workflow_for(DEFAULT_WORKFLOW_HOME, crate::cli_version())),
        (".github/CODEOWNERS", CODEOWNERS.to_string()),
        ("LICENSE", include_str!("../../../LICENSE").to_string()),
        (
            "LICENSE-MIT",
            include_str!("../../../LICENSE-MIT").to_string(),
        ),
        (
            "LICENSE-APACHE-2",
            include_str!("../../../LICENSE-APACHE-2").to_string(),
        ),
        (".gitignore", ROOT_GITIGNORE.to_string()),
        (
            PROJECT_MANIFEST,
            format!(
                "version: {}\nproject:\n  host: {}\n  name: {}\n  lawyer_dri: {}\n  client_dri: {}\n  private_slack_channel: not provisioned\n  private_notion_page: not provisioned\n",
                crate::cli_version(), host, code, lawyer_dri, client_dri
            ),
        ),
    ];
    let (draft, stub) = super::lifecycle::scaffold_draft(super::lifecycle::ONBOARDING);
    let forgejo_gate = (forge == cloud::ForgeBackend::Forgejo)
        .then(|| (FORGEJO_GATE_WORKFLOW, forgejo_gate_workflow()));
    for (relative, contents) in files
        .into_iter()
        .chain([(draft.as_str(), stub)])
        .chain(forgejo_gate)
    {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, contents)?;
    }
    emit_synced_skills(root)?;
    if !root.join(".git").exists() {
        let status = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(root)
            .status()
            .context("initialize the generated repository")?;
        if !status.success() {
            anyhow::bail!("git init failed in {}", root.display());
        }
    }
    // The gate owns machine-generated files such as the root .gitignore and
    // automerge.yml. Generate those through the same gate used in production.
    let outcome = validate_gate(root, Some(code), true);
    if outcome.errors != 0 {
        anyhow::bail!(
            "generated Project scaffold failed its gate ({} errors)",
            outcome.errors
        );
    }
    Ok(())
}

/// Write Navigator's canonical Agent Skills. The scaffold always writes them;
/// [`repair_skills`] restores a drifted copy and reports what changed.
pub(crate) fn emit_synced_skills(root: &Path) -> anyhow::Result<()> {
    for (name, contents) in SYNCED_SKILLS {
        let path = root.join(".agents/skills").join(name).join("SKILL.md");
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("skill output path has no parent"))?;
        fs::create_dir_all(parent)?;
        fs::write(path, contents)?;
    }
    Ok(())
}

/// Require an existing Project checkout before the skills repair command can
/// write. The Navigator source root and arbitrary folders are refused.
///
/// Restores `AGENTS.md`, Claude permissions, and each canonical skill to the copy embedded in this
/// CLI release, plus [`FORGEJO_GATE_WORKFLOW`] for a Forgejo-backed Project
/// (`NAVIGATOR_FORGE_BACKEND=forgejo`, or a checkout that already carries
/// `.forgejo/`). Returns the repository-relative paths whose bytes changed.
/// Custom skills and every other Project file stay untouched.
pub(crate) fn repair_skills(root: &Path) -> anyhow::Result<Vec<String>> {
    repair_skills_for(root, forge_backend_from_env()?)
}

/// [`repair_skills`] for a named forge.
pub(crate) fn repair_skills_for(
    root: &Path,
    forge: cloud::ForgeBackend,
) -> anyhow::Result<Vec<String>> {
    let root = root.canonicalize()?;
    let navigator_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .and_then(|path| path.canonicalize().ok());
    anyhow::ensure!(
        navigator_root.as_deref() != Some(root.as_path()),
        "refusing to repair skills in the Navigator repository root"
    );
    anyhow::ensure!(
        root.join(".git").exists(),
        "not an existing Project repository: missing .git"
    );
    anyhow::ensure!(
        root.join("README.md").is_file(),
        "not an existing Project repository: missing README.md"
    );
    anyhow::ensure!(
        root.join(PROJECT_MANIFEST).is_file(),
        "not an existing Project repository: missing {PROJECT_MANIFEST}"
    );
    let manifest = super::manifest::read(&root).ok_or_else(|| {
        anyhow::anyhow!("not an existing Project repository: invalid {PROJECT_MANIFEST}")
    })?;
    let project_code = manifest.project.as_deref();
    anyhow::ensure!(
        project_code.is_some_and(store::projects::is_valid_code),
        "not an existing Project repository: missing project.name in {PROJECT_MANIFEST}"
    );
    let mut repaired = Vec::new();
    restore_canonical(&root, "AGENTS.md", AGENT_CONTRACT_BASE, &mut repaired)?;
    repair_root_gitignore(&root, &mut repaired)?;
    restore_canonical(
        &root,
        CLAUDE_SETTINGS,
        CLAUDE_SETTINGS_CONTENTS,
        &mut repaired,
    )?;
    for (name, contents) in SYNCED_SKILLS {
        let relative = format!(".agents/skills/{name}/SKILL.md");
        restore_canonical(&root, &relative, contents, &mut repaired)?;
    }
    if forge == cloud::ForgeBackend::Forgejo || root.join(".forgejo").exists() {
        restore_canonical(
            &root,
            FORGEJO_GATE_WORKFLOW,
            &forgejo_gate_workflow(),
            &mut repaired,
        )?;
    }
    for name in RETIRED_SYNCED_SKILLS {
        remove_retired_skill(&root, name, &mut repaired)?;
    }
    Ok(repaired)
}

/// Skills this catalog once synced and has since retired (ENG-1075: the
/// Portal's `portal-chrome` and `server`). `sync-skills` deletes each from a
/// Project repository, so dropping a name from [`SYNCED_SKILLS`] retires it
/// fleet-wide rather than leaving a stale copy no gate checks.
const RETIRED_SYNCED_SKILLS: &[&str] = &["portal-chrome", "server"];

/// Delete `.agents/skills/<name>/` when its `SKILL.md` is a synced copy: it
/// names itself `name` in its frontmatter and says its canonical copy lives in
/// Navigator. A Project's own skill that happens to share the name is left
/// alone.
fn remove_retired_skill(root: &Path, name: &str, removed: &mut Vec<String>) -> anyhow::Result<()> {
    let relative = format!(".agents/skills/{name}");
    let directory = root.join(&relative);
    let Ok(contents) = fs::read_to_string(directory.join("SKILL.md")) else {
        return Ok(());
    };
    let normalized = contents.split_whitespace().collect::<Vec<_>>().join(" ");
    let names_itself = contents
        .lines()
        .any(|line| line.trim() == format!("name: {name}"));
    if names_itself && normalized.contains("this canonical copy lives in Navigator's own") {
        fs::remove_dir_all(&directory)?;
        removed.push(relative);
    }
    Ok(())
}

/// Write `contents` to `relative` when the file is missing or different.
/// `relative` is a fixed canonical path, the same set [`emit_synced_skills`]
/// writes, plus `AGENTS.md`.
fn restore_canonical(
    root: &Path,
    relative: &str,
    contents: &str,
    repaired: &mut Vec<String>,
) -> anyhow::Result<()> {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        anyhow::ensure!(
            parent.starts_with(root),
            "refusing to repair a path outside the Project repository"
        );
        fs::create_dir_all(parent)?;
    }
    let bytes = contents.as_bytes();
    if fs::read(&path).ok().as_deref() == Some(bytes) {
        return Ok(());
    }
    fs::write(&path, bytes)?;
    repaired.push(relative.to_string());
    Ok(())
}

/// A thin `ci.yml` caller: one required job named [`REQUIRED_CHECK`] that
/// calls the reusable project-gate workflow, resolved from `workflow_home`,
/// at `action_version`.
///
/// The jobs themselves live in `.github/workflows/project-gate.yml` in
/// `workflow_home`. Pinning that file is the thing that scales; a Project
/// repository does not copy them. The job carries no `with:` block: the
/// reusable workflow reads `project`/`host` from this repository's own
/// `navigator.yaml`, so the pinned `version` in `uses:` is the only value a
/// caller repeats.
pub(crate) fn workflow_for(workflow_home: &str, action_version: &str) -> String {
    let gate = gate_workflow_prefix(workflow_home);
    format!(
        r"name: {REQUIRED_CHECK}

on:
  pull_request:

permissions:
  contents: read
  id-token: write

jobs:
  {REQUIRED_CHECK}:
    uses: {gate}{action_version}
"
    )
}

/// [`workflow_for`] at [`DEFAULT_WORKFLOW_HOME`] — a fixture shorthand for
/// the still-real-in-production default. Every production caller now threads
/// its own `workflow_home` explicitly through [`workflow_for`] instead, which
/// is why this convenience wrapper is test-only.
#[cfg(test)]
pub(crate) fn workflow(action_version: &str) -> String {
    workflow_for(DEFAULT_WORKFLOW_HOME, action_version)
}

/// The thin `cd.yml` caller for the reusable publisher, resolved from
/// `workflow_home`.
///
/// `gate` re-invokes `project-gate.yml` on this same push-to-`main` event so
/// its live document verification, live Project gate, and seed import run
/// here — `ci.yml` above calls that file only on `pull_request`, so nothing
/// else triggers those live jobs. `publish` `needs: gate`: a push publishes
/// only after the live checks it depends on have passed. Neither job carries
/// a `with:` block or a token-scoping comment: what the reusable workflow
/// does with the token, and where it reads `project`/`host` from, is
/// documented in the shared workflow, not repeated in every caller.
pub(crate) fn cd_workflow_for(workflow_home: &str, action_version: &str) -> String {
    let gate = gate_workflow_prefix(workflow_home);
    let publish = publish_workflow_prefix(workflow_home);
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
    uses: {gate}{action_version}
  publish:
    needs: gate
    uses: {publish}{action_version}
",
    )
}

/// The gate workflow a Forgejo-backed Project repository carries. Forgejo
/// Actions reads `.forgejo/workflows/` and ignores `.github/workflows/` when
/// it exists, so `ci.yml` and `cd.yml` stay for GitHub and this file is the
/// whole Forgejo gate. The deployment's `NAVIGATOR_FORGEJO_ACTIONS_OIDC_WORKFLOW`
/// names this exact path: the Forgejo document-token door trusts no other.
pub(crate) const FORGEJO_GATE_WORKFLOW: &str = ".forgejo/workflows/gate.yml";

/// The commit-status context [`forgejo_gate_workflow`] posts, which `main`'s
/// branch protection requires: `<workflow name> / <job> (<event>)`.
pub(crate) const FORGEJO_REQUIRED_CHECK: &str = "gate / gate (pull_request)";

/// The Forgejo Actions gate: on every pull request, check out its head,
/// install the `navigator` release `navigator.yaml` pins, and run
/// `navigator project gate --ci`. `enable-openid-connect` makes the runner
/// export `ACTIONS_ID_TOKEN_REQUEST_*`; the CLI requests a token audienced to
/// the manifest's host and exchanges it at `/auth/ci/forgejo/document-token`.
///
/// Every step is plain shell, so the workflow needs no action repository the
/// Forgejo installation would have to mirror. The binary installs outside the
/// checkout, where the gate's closed root list would refuse it.
pub(crate) fn forgejo_gate_workflow() -> String {
    use crate::lsp_download::RELEASE_DOWNLOAD_BASE;
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

/// The forge this process scaffolds for, from `NAVIGATOR_FORGE_BACKEND`
/// (unset is GitHub).
pub(crate) fn forge_backend_from_env() -> anyhow::Result<cloud::ForgeBackend> {
    cloud::ForgeBackend::from_lookup(|name| std::env::var(name).ok())
        .map_err(|error| anyhow::anyhow!("{error}"))
}

/// [`cd_workflow_for`] at [`DEFAULT_WORKFLOW_HOME`] — see [`workflow`]'s
/// comment: test-only, because production now threads `workflow_home`
/// explicitly through [`cd_workflow_for`] instead.
#[cfg(test)]
pub(crate) fn cd_workflow(action_version: &str) -> String {
    cd_workflow_for(DEFAULT_WORKFLOW_HOME, action_version)
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_headless_import_root_is_admitted_by_the_project_gate() {
        for root in rules::source_archive::HEADLESS_ROOTS {
            assert!(
                super::ALLOWED_ROOTS.contains(root),
                "{root} is importable but the gate would refuse it"
            );
        }
    }

    use super::{
        blueprint_selection, california_law_firm_entity_claims, cd_workflow, cd_workflow_for,
        emit_scaffold, firm_trade_name_claims, gate_workflow_prefix, is_release_tag,
        lint_project_template, misnamed_firm_entities, missing_root_gitignore_lines,
        publish_workflow_prefix, repair_root_gitignore, repair_skills, repository_name,
        retired_workflow_refused, validate_agent_contract, validate_cd_workflow,
        validate_documents_gitignore, validate_gate, validate_github_path, validate_layout,
        validate_skills, validate_workflow, workflow, workflow_for, Finding, LayoutOutcome,
        AGENT_CONTRACT_BASE, ALLOWED_ROOTS, AUTOMERGE_WORKFLOW_CONTENTS, CD_WORKFLOW,
        CLAUDE_SETTINGS, CLAUDE_SETTINGS_CONTENTS, DEFAULT_WORKFLOW_HOME, DRAFT_DIRECTORY,
        ENTITY_CODE, FINAL_RETIRED_WORKFLOW_RELEASE, FIRM_TRADE_NAME_CODE, PROJECT_MANIFEST,
        RETIRED_CD_WORKFLOW, RETIRED_SYNCED_SKILLS, RETIRED_WORKFLOW, ROOT_GITIGNORE,
        SYNCED_SKILLS, TEMPLATE_DIRECTORY, WORKFLOW,
    };
    use super::{
        tracked_file_admitted, validate_tracked_files, CANONICAL_GITATTRIBUTES, CODEOWNERS,
    };
    use crate::projects::manifest::Manifest;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    #[cfg(unix)]
    use std::process::Command;

    /// The pin the fixtures below scaffold with.
    ///
    /// A literal, not `crate::published_cli_version()`: a fixture that reads
    /// the running binary's version makes every layout test depend on how
    /// this build was stamped, and an ambient `NAVIGATOR_RELEASE_TAG` would
    /// then decide whether they pass. The one test that must speak about the
    /// real default says so itself.
    const FIXTURE_PIN: &str = "26.8.23";

    /// The messages `validate_workflow` reports for one gate file, resolved
    /// against [`DEFAULT_WORKFLOW_HOME`].
    fn findings(contents: &str) -> Vec<String> {
        let mut errors: Vec<Finding> = Vec::new();
        validate_workflow(
            Path::new("gate.yml"),
            contents,
            None,
            DEFAULT_WORKFLOW_HOME,
            &mut errors,
        );
        errors.into_iter().map(|error| error.message).collect()
    }

    /// The messages `validate_cd_workflow` reports for one CD file, resolved
    /// against [`DEFAULT_WORKFLOW_HOME`].
    fn cd_findings(contents: &str) -> Vec<String> {
        let mut errors: Vec<Finding> = Vec::new();
        validate_cd_workflow(
            Path::new("cd.yml"),
            contents,
            None,
            DEFAULT_WORKFLOW_HOME,
            &mut errors,
        );
        errors.into_iter().map(|error| error.message).collect()
    }

    /// Run the generated GitHub arm step against a synthetic `gh` binary. The
    /// stub models the two successful postconditions: an armed request, or a
    /// PR that merged before the workflow could read it back.
    #[cfg(unix)]
    fn run_automerge_step(
        gh_token: &str,
        merge_exit: &str,
        armed: &str,
        state: &str,
    ) -> (std::process::Output, String) {
        let workflow: serde_yaml::Value = serde_yaml::from_str(AUTOMERGE_WORKFLOW_CONTENTS)
            .expect("canonical auto-merge workflow is valid YAML");
        let steps = workflow["jobs"]["enable-automerge"]["steps"]
            .as_sequence()
            .expect("auto-merge steps");
        let script = steps
            .iter()
            .find(|step| step["name"].as_str() == Some("Arm auto-merge"))
            .and_then(|step| step["run"].as_str())
            .expect("arm step script");

        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let gh = bin.join("gh");
        fs::write(
            &gh,
            r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$CALL_LOG"
case "${1:-} ${2:-}" in
  "pr merge") exit "${GH_MERGE_EXIT:-0}" ;;
  "pr view")
    if [ "$PR_ARMED" = true ] || [ "$PR_STATE" = MERGED ]; then
      printf 'true\n'
    else
      printf 'false\n'
    fi
    ;;
  *) exit 64 ;;
esac
"#,
        )
        .unwrap();
        let mut permissions = fs::metadata(&gh).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&gh, permissions).unwrap();

        let path = std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        )))
        .unwrap();
        let call_log = root.path().join("calls.log");
        let output = Command::new("bash")
            .arg("--noprofile")
            .arg("--norc")
            .arg("-e")
            .arg("-o")
            .arg("pipefail")
            .arg("-c")
            .arg(script)
            .env("PATH", path)
            .env("GH_TOKEN", gh_token)
            .env("GH_MERGE_EXIT", merge_exit)
            .env("PR_ARMED", armed)
            .env("PR_STATE", state)
            .env("PR_URL", "https://example.test/project/pull/1")
            .env("CALL_LOG", call_log.as_os_str())
            .output()
            .unwrap();
        let calls = fs::read_to_string(call_log).unwrap_or_default();
        (output, calls)
    }

    /// The warnings `validate_layout` reports for one checkout.
    fn layout_warnings(root: &Path) -> Vec<String> {
        let mut errors: Vec<Finding> = Vec::new();
        let mut warnings: Vec<Finding> = Vec::new();
        validate_layout(root, &mut errors, &mut warnings);
        warnings
            .into_iter()
            .map(|warning| warning.message)
            .collect()
    }

    /// The smallest checkout `validate_layout` accepts, so a test adding one
    /// file measures that file and nothing else.
    fn scaffold_minimal(root: &Path) {
        std::fs::create_dir_all(root.join(".github/workflows")).unwrap();
        std::fs::write(root.join("README.md"), "# fixture\n").unwrap();
        std::fs::write(root.join(WORKFLOW), workflow(FIXTURE_PIN)).unwrap();
        std::fs::write(root.join(CD_WORKFLOW), cd_workflow(FIXTURE_PIN)).unwrap();
        std::fs::write(root.join("AGENTS.md"), AGENT_CONTRACT_BASE).unwrap();
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        std::fs::write(root.join(CLAUDE_SETTINGS), CLAUDE_SETTINGS_CONTENTS).unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(root)
            .status()
            .unwrap();
        assert!(status.success(), "git init failed in {}", root.display());
    }

    fn layout_findings(root: &Path) -> Vec<String> {
        let mut errors: Vec<Finding> = Vec::new();
        let mut warnings = Vec::new();
        validate_layout(root, &mut errors, &mut warnings);
        errors.into_iter().map(|error| error.message).collect()
    }

    #[test]
    fn repository_name_prefers_the_explicit_coordinate() {
        assert_eq!(
            repository_name(Path::new("/tmp/renamed"), Some("org/example")),
            "example"
        );
    }

    #[test]
    fn repository_name_uses_github_repository_before_the_manifest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(PROJECT_MANIFEST),
            "project: acme\nhost: staging.neonlaw.com\n",
        )
        .unwrap();
        let previous = std::env::var("GITHUB_REPOSITORY").ok();
        std::env::set_var("GITHUB_REPOSITORY", "org/from-ci");
        let name = repository_name(dir.path(), None);
        match previous {
            Some(value) => std::env::set_var("GITHUB_REPOSITORY", value),
            None => std::env::remove_var("GITHUB_REPOSITORY"),
        }
        assert_eq!(name, "from-ci");
    }

    #[test]
    fn repository_name_uses_the_manifest_when_github_is_unset() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(PROJECT_MANIFEST),
            "project: acme\nhost: staging.neonlaw.com\n",
        )
        .unwrap();
        let previous = std::env::var("GITHUB_REPOSITORY").ok();
        std::env::remove_var("GITHUB_REPOSITORY");
        let name = repository_name(dir.path(), None);
        if let Some(value) = previous {
            std::env::set_var("GITHUB_REPOSITORY", value);
        }
        assert_eq!(name, "acme");
    }

    #[test]
    fn generated_template_has_a_stable_code() {
        assert!(is_release_tag("26.7.27"));
        assert!(is_release_tag("26.8.19-hotfix.14"));
        assert!(!is_release_tag("main"));
        assert!(!is_release_tag("26.8.19-hotfix."));
        // The legacy four-component spelling is not a version Cargo can parse,
        // so no release has been able to carry it since the tag started coming
        // from `[workspace.package].version`.
        assert!(!is_release_tag("26.7.27.4"));
    }

    /// The generated gate is one always-running required job.
    ///
    /// A `paths:` filter here would let a required check pass by being skipped,
    /// so the job name matches the one `ops github setup` binds and the gate
    /// carries no filter at all.
    #[test]
    fn a_licence_at_the_root_is_part_of_the_layout() {
        // Every one of these repositories is proprietary. A templates-only
        // Project has no `portal/` to hide a licence inside, so refusing it at
        // the root made the layout unsatisfiable for that shape rather than
        // merely opinionated.
        assert!(ALLOWED_ROOTS.contains(&"LICENSE.md"));
    }

    #[test]
    fn gitattributes_at_the_root_is_part_of_the_layout() {
        assert!(ALLOWED_ROOTS.contains(&".gitattributes"));
    }

    #[test]
    fn a_reusable_workflow_call_with_a_matching_pin_passes() {
        let contents = r"name: ci
on: [pull_request]
jobs:
  ci:
    uses: neon-law/navigator/.github/workflows/project-gate.yml@26.7.27
";
        assert_eq!(findings(contents), Vec::<String>::new());
    }

    #[test]
    fn a_real_version_mismatch_is_still_caught() {
        let contents = r"name: ci
on: [pull_request]
jobs:
  ci:
    uses: neon-law/navigator/.github/workflows/project-gate.yml@26.7.27
";
        let manifest = Manifest {
            version: Some("26.7.26".to_string()),
            project: Some("acme".to_string()),
            ..Manifest::default()
        };
        let mut errors = Vec::new();
        validate_workflow(
            Path::new("ci.yml"),
            contents,
            Some(&manifest),
            DEFAULT_WORKFLOW_HOME,
            &mut errors,
        );
        let found: Vec<String> = errors.into_iter().map(|error| error.message).collect();
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].contains("must equal manifest version"),
            "{found:?}"
        );
    }

    #[test]
    fn a_moving_ref_is_still_refused() {
        let contents = r"name: ci
on: [pull_request]
jobs:
  ci:
    uses: neon-law/navigator/.github/workflows/project-gate.yml@main
";
        let found = findings(contents);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].contains("must be an exact release tag"),
            "{found:?}"
        );
    }

    /// The core of this issue: the reusable workflow reads `project`/`host`
    /// from the caller's own `navigator.yaml`, so a caller has nothing left
    /// to repeat there. A `with:` block of any shape is a duplicate of the
    /// manifest and is rejected outright, not merely checked for agreement.
    #[test]
    fn a_with_block_is_rejected() {
        let contents = r#"name: ci
on: [pull_request]
jobs:
  ci:
    uses: neon-law/navigator/.github/workflows/project-gate.yml@26.7.27
    with:
      project: "acme"
      host: "staging.neonlaw.com"
"#;
        let found = findings(contents);
        assert!(
            found
                .iter()
                .any(|message| message.contains("must not declare a `with:` block")),
            "{found:?}"
        );
    }

    /// [`gate_workflow_prefix`]/[`publish_workflow_prefix`] at
    /// [`DEFAULT_WORKFLOW_HOME`] must keep naming this repository's own
    /// reusable workflows — the historical literal every existing Project
    /// repository is already pinned against. See ENG-931's "Keep Project
    /// CI/CD working".
    #[test]
    fn the_default_workflow_home_resolves_this_repositorys_own_workflows() {
        assert_eq!(
            gate_workflow_prefix(DEFAULT_WORKFLOW_HOME),
            "neon-law/navigator/.github/workflows/project-gate.yml@"
        );
        assert_eq!(
            publish_workflow_prefix(DEFAULT_WORKFLOW_HOME),
            "neon-law/navigator/.github/workflows/project-publish.yml@"
        );

        let legacy = workflow_for("neon-law-source-code/navigator", FIXTURE_PIN);
        let mut errors = Vec::new();
        validate_workflow(
            Path::new("ci.yml"),
            &legacy,
            None,
            DEFAULT_WORKFLOW_HOME,
            &mut errors,
        );
        assert!(
            !errors.is_empty(),
            "Actions does not follow the old slug redirect"
        );

        let legacy_cd = cd_workflow_for("neon-law-source-code/navigator", FIXTURE_PIN);
        let mut errors = Vec::new();
        validate_cd_workflow(
            Path::new("cd.yml"),
            &legacy_cd,
            None,
            DEFAULT_WORKFLOW_HOME,
            &mut errors,
        );
        assert!(
            !errors.is_empty(),
            "Actions does not follow the old slug redirect"
        );
    }

    /// ENG-931's cross-organization Project fixture: a Project repository in
    /// an organization other than `neon-law-source-code` — the same
    /// `acme/navigator`-shaped fixture `cli/src/devx/github_setup.rs` already
    /// uses — reconciles cleanly to a `ci.yml`/`cd.yml` pinned to an
    /// alternate, always-public `workflow_home` rather than this repository.
    /// This is the round trip the reusable-workflow relocation must prove
    /// before any real Project repository is repointed at a new home: the
    /// generator and the validator must agree on the exact same alternate
    /// `uses:` prefix, exactly as they already do for the default one above.
    #[test]
    fn a_cross_organization_project_fixture_validates_against_an_alternate_workflow_home() {
        const ALTERNATE_HOME: &str = "neon-law-source-code/navigator-actions";

        let ci = workflow_for(ALTERNATE_HOME, FIXTURE_PIN);
        let mut errors = Vec::new();
        validate_workflow(Path::new("ci.yml"), &ci, None, ALTERNATE_HOME, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");

        let cd = cd_workflow_for(ALTERNATE_HOME, FIXTURE_PIN);
        let mut errors = Vec::new();
        validate_cd_workflow(Path::new("cd.yml"), &cd, None, ALTERNATE_HOME, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");

        // Validating an alternate-home caller against the *default* home must
        // still fail — the whole point is that the two homes are distinct,
        // not that the validator has stopped checking the prefix at all.
        let mut errors = Vec::new();
        validate_workflow(
            Path::new("ci.yml"),
            &ci,
            None,
            DEFAULT_WORKFLOW_HOME,
            &mut errors,
        );
        assert!(
            !errors.is_empty(),
            "expected the default home to reject an alternate-home caller"
        );
    }

    /// A gate that does not parse is its own failure. Reporting it as a missing
    /// action sends the reader looking for a step that is right there.
    #[test]
    fn an_unparseable_gate_says_so() {
        let found = findings("name: ci\njobs: [oops\n");
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("not valid YAML"), "{found:?}");
    }

    /// The generator and the validator must agree. Nothing asserted this, which
    /// is how three defects lived in one twelve-line function.
    #[test]
    fn the_scaffolded_gate_passes_its_own_validation() {
        assert_eq!(findings(&workflow(FIXTURE_PIN)), Vec::<String>::new());
    }

    /// Six Project repositories declare their Project in a root manifest. The
    /// layout gate refused it, which is why the pinned action had to be pulled
    /// from every one of their gates.
    #[test]
    fn the_project_manifest_is_part_of_the_layout() {
        assert!(ALLOWED_ROOTS.contains(&"navigator.yaml"));
    }

    /// `seeds/` is where a Project repository's `navigator site import` documents
    /// belong. Refusing it left nowhere in the layout for real actors a
    /// matter names, and `fixtures/` is the wrong root: a fixture is invented
    /// or firm-owned, while a seed document is the input to a production
    /// write.
    #[test]
    fn the_seed_directory_is_part_of_the_layout() {
        assert!(ALLOWED_ROOTS.contains(&"seeds"));
    }

    #[test]
    fn a_checkout_carrying_seeds_has_no_layout_finding() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        std::fs::create_dir_all(root.path().join("seeds")).unwrap();
        std::fs::write(
            root.path().join("seeds/Person.yaml"),
            "lookup_fields:\n  - email\nrecords: []\n",
        )
        .unwrap();

        assert_eq!(layout_findings(root.path()), Vec::<String>::new());
    }

    #[test]
    fn a_checkout_carrying_the_manifest_has_no_layout_finding() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        std::fs::write(
            root.path().join("navigator.yaml"),
            "host: staging.neonlaw.com\nproject: acme\n",
        )
        .unwrap();

        assert_eq!(layout_findings(root.path()), Vec::<String>::new());
    }

    #[test]
    fn a_checkout_without_agents_md_is_refused() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        std::fs::remove_file(root.path().join("AGENTS.md")).unwrap();
        let found = layout_findings(root.path());
        assert!(
            found
                .iter()
                .any(|finding| finding.contains("missing required AGENTS.md")),
            "{found:?}"
        );
    }

    /// The generator and the gate have to agree on the bytes, or the check is
    /// theatre: a freshly scaffolded repository must be clean under the very
    /// rule that reads the file `scaffold` just wrote.
    #[test]
    fn the_scaffolded_contract_is_the_canonical_shared_file() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        assert_eq!(
            fs::read_to_string(root.path().join("AGENTS.md")).unwrap(),
            AGENT_CONTRACT_BASE
        );
        let warnings = layout_warnings(root.path());
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    /// The drift this catches is the quiet kind: the contract still *reads*
    /// correctly and still names the feedback destination, so every check that
    /// existed before this one passes. Only the byte comparison notices that
    /// one repository now tells an agent something slightly different from the
    /// other eighteen.
    #[test]
    fn a_reworded_contract_is_reported_as_an_error() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        let reworded = AGENT_CONTRACT_BASE.replace(
            "When Navigator's CLI is missing or wrong, open a Linear issue on the Lawyers team",
            "When Navigator's CLI is missing or wrong, just work around it",
        );
        std::fs::write(root.path().join("AGENTS.md"), reworded).unwrap();

        let mut errors: Vec<Finding> = Vec::new();
        let mut warnings = Vec::new();
        validate_layout(root.path(), &mut errors, &mut warnings);
        assert!(
            errors
                .iter()
                .any(|finding| finding.path.ends_with("AGENTS.md")
                    && finding.message.contains("canonical contract")),
            "{errors:?}"
        );
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    /// Any hand-written contract is drift, including one that omits the
    /// canonical feedback and safety guidance.
    #[test]
    fn a_contract_without_the_canonical_content_is_refused() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        std::fs::write(root.path().join("AGENTS.md"), "# Working in acme\n").unwrap();

        let mut errors: Vec<Finding> = Vec::new();
        let mut warnings = Vec::new();
        validate_layout(root.path(), &mut errors, &mut warnings);
        assert!(
            errors
                .iter()
                .any(|finding| finding.message.contains("canonical contract")),
            "{errors:?}"
        );
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    /// `AGENTS.md` is the contract, so a `CLAUDE.md` beside it is a retired
    /// mirror rather than a second copy to keep in sync. Refusing it by name —
    /// not as one more anonymous unenumerated root — is what tells the person
    /// reading CI which file to delete and which one survives.
    #[test]
    fn a_claude_md_is_refused_as_a_retired_mirror() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        std::fs::write(root.path().join("CLAUDE.md"), "AGENTS.md").unwrap();

        // Asserted on the path as well as the message: the message alone would
        // not say which file carried the finding, and the path alone would keep
        // passing if `CLAUDE.md` fell back to the generic unenumerated-root
        // wording this test exists to replace.
        let mut errors: Vec<Finding> = Vec::new();
        let mut warnings = Vec::new();
        validate_layout(root.path(), &mut errors, &mut warnings);
        assert!(
            errors
                .iter()
                .any(|finding| finding.path.ends_with("CLAUDE.md")
                    && finding.message.contains("retired agent-instruction mirror")
                    && finding
                        .message
                        .contains("`AGENTS.md` is the whole contract")),
            "{errors:?}"
        );
    }

    /// A committed `.claude/` or `.codex/` is the same mirror one directory
    /// deep, and it is the half that used to pass: nothing checked a path below
    /// its first component against [`ALLOWED_ROOTS`], so a mirrored `skills/`
    /// was admitted in full while a root `CLAUDE.md` was refused.
    #[test]
    fn a_committed_mirror_directory_is_refused_once() {
        for mirror in [".claude", ".codex"] {
            let root = tempfile::tempdir().unwrap();
            scaffold_minimal(root.path());
            let path = root.path().join(mirror).join("skills/council");
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("SKILL.md"), "# mirror\n").unwrap();
            let status = std::process::Command::new("git")
                .args(["add", "--all"])
                .current_dir(root.path())
                .status()
                .unwrap();
            assert!(
                status.success(),
                "git add failed in {}",
                root.path().display()
            );

            let mut errors: Vec<Finding> = Vec::new();
            let mut warnings = Vec::new();
            validate_layout(root.path(), &mut errors, &mut warnings);
            let mirrored: Vec<&Finding> = errors
                .iter()
                .filter(|finding| finding.message.contains("retired agent-instruction mirror"))
                .collect();
            assert_eq!(
                mirrored.len(),
                1,
                "one directory to delete, not one finding per skill: {errors:?}"
            );
            assert!(mirrored[0].path.ends_with(mirror), "{mirrored:?}");
            assert!(
                mirrored[0].message.contains(".agents/skills/"),
                "the finding must name the canonical catalog: {mirrored:?}"
            );
        }
    }

    /// A developer's own ignored `.claude/` state remains outside the gate.
    #[test]
    fn an_ignored_claude_directory_is_left_alone() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        std::fs::write(root.path().join(".gitignore"), ".claude/\n").unwrap();
        std::fs::create_dir_all(root.path().join(".claude/worktrees")).unwrap();
        std::fs::write(root.path().join(".claude/settings.json"), "{}\n").unwrap();

        let found = layout_findings(root.path());
        assert!(
            !found
                .iter()
                .any(|message| message.contains("retired agent-instruction mirror")),
            "{found:?}"
        );
    }

    #[test]
    fn only_root_claude_settings_is_allowed_and_it_must_be_canonical() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        fs::create_dir_all(root.path().join(".claude")).unwrap();
        fs::write(
            root.path().join(".claude/settings.json"),
            CLAUDE_SETTINGS_CONTENTS,
        )
        .unwrap();
        let findings = layout_findings(root.path());
        assert!(findings.is_empty(), "{findings:?}");

        fs::write(root.path().join(".claude/settings.json"), "{}\n").unwrap();
        assert!(layout_findings(root.path())
            .iter()
            .any(|finding| finding.contains("canonical email-send denial policy")));

        fs::write(
            root.path().join(".claude/settings.json"),
            CLAUDE_SETTINGS_CONTENTS,
        )
        .unwrap();
        fs::write(root.path().join(".claude/CLAUDE.md"), "instructions\n").unwrap();
        assert!(layout_findings(root.path())
            .iter()
            .any(|finding| finding.contains("retired agent-instruction mirror")));
    }

    /// A mirror inside an application directory is the same mirror. The root
    /// is where one is written by hand today, but `apps/<app>/` is a whole
    /// workspace with its own tooling, and a rule that reads only the root is
    /// one an application walks around.
    #[test]
    fn a_mirror_inside_an_application_is_refused_where_it_sits() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        let application = root.path().join("apps/portal");
        std::fs::create_dir_all(&application).unwrap();
        std::fs::write(application.join("CLAUDE.md"), "AGENTS.md").unwrap();

        let mut errors: Vec<Finding> = Vec::new();
        let mut warnings = Vec::new();
        validate_layout(root.path(), &mut errors, &mut warnings);
        assert!(
            errors
                .iter()
                .any(|finding| finding.path.ends_with("apps/portal/CLAUDE.md")
                    && finding.message.contains("retired agent-instruction mirror")),
            "the finding is anchored where the mirror sits, not at the root: {errors:?}"
        );
    }

    /// The catalog that survives is still accepted: this is a rename of the
    /// mirror check, not a refusal of agent tooling.
    #[test]
    fn the_canonical_agents_directory_is_not_a_retired_mirror() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        let path = root.path().join(".agents/skills/council");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("SKILL.md"), "# council\n").unwrap();

        let found = layout_findings(root.path());
        assert!(
            !found
                .iter()
                .any(|message| message.contains("retired agent-instruction mirror")),
            "{found:?}"
        );
    }

    /// Synced skills are copied into Project repositories that do not carry
    /// Navigator's `docs/` tree. A relative link that only resolves here
    /// becomes `M057` the moment `project gate` scans `.agents/`.
    #[test]
    fn synced_skills_resolve_relative_links_outside_navigator() {
        use rules::{M057RelativeLinkResolves, Rule, SourceFile};

        for (name, contents) in SYNCED_SKILLS {
            let root = tempfile::tempdir().unwrap();
            let path = root
                .path()
                .join(".agents/skills")
                .join(name)
                .join("SKILL.md");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, contents).unwrap();
            let violations = M057RelativeLinkResolves.lint(&SourceFile {
                path: path.clone(),
                contents: (*contents).to_string(),
            });
            assert!(
                violations.is_empty(),
                "{name} still points at a Navigator-only path: {violations:?}"
            );
        }
    }

    /// ENG-870: a synced skill's documented `navigator …` invocation must
    /// resolve against *this build's* live clap tree, checked here — in
    /// Navigator's own test suite — rather than only when `project gate`
    /// happens to run inside a Project repository that carries the synced
    /// copy. That gap is exactly how a retired verb (`project repository
    /// sync-skills`, `project repository deliver`) survived in synced
    /// skill text after the command it named was removed: the caller lived
    /// in a Project repository, "not searchable from the Navigator
    /// checkout" (ENG-870's own words) — until now.
    #[test]
    fn every_synced_skill_documents_only_commands_that_resolve() {
        let tree = crate::navigator_command();
        for (name, contents) in SYNCED_SKILLS {
            assert!(
                contents.contains("Find the term first"),
                "synced skill `{name}` must point Project contributors to glossary guidance"
            );
            let found = crate::projects::cli_docs::unresolved_invocations(contents, &tree);
            assert!(
                found.is_empty(),
                "synced skill `{name}` documents a command that does not resolve: {found:?}"
            );
        }
    }

    /// LAW-126: the fleet's `deadline` skill writes through the Deadline verbs,
    /// keeps reminders in the private channel, and never computes a date.
    #[test]
    fn deadline_is_synced_and_records_through_project_deadline() {
        let (_, canonical) = SYNCED_SKILLS
            .iter()
            .find(|(name, _)| *name == "deadline")
            .expect("deadline is a synced skill");
        for expected in [
            "navigator project deadline list",
            "navigator project deadline create",
            "navigator project deadline update",
            "navigator project deadline complete",
            "navigator project deadline cancel",
            "navigator glossary show \"Deadline\"",
            "private_slack_channel",
            "--client-lead-days",
            "Never compute court days",
            "Court appearance registration is separate",
        ] {
            assert!(canonical.contains(expected), "skill must name `{expected}`");
        }
        assert!(
            !canonical.contains("shared_slack_channel"),
            "a deadline reminder never goes to the client-shared channel"
        );
    }

    /// LAW-133: the fleet's `case-timeline` skill keeps the case record as
    /// Docket Entries through the docket verbs, hands dates to the deadline
    /// skill, and never keeps a memo.
    #[test]
    fn case_timeline_is_synced_and_records_through_project_docket() {
        let (_, canonical) = SYNCED_SKILLS
            .iter()
            .find(|(name, _)| *name == "case-timeline")
            .expect("case-timeline is a synced skill");
        for expected in [
            "navigator project docket list",
            "navigator project docket add",
            "navigator project docket link-document",
            "navigator project docket supersede",
            "navigator project docket ingest",
            "navigator glossary show \"Docket Entry\"",
            "--scheduled-on",
            "--dry-run",
            "A hearing is a Docket Entry",
            "Do not write a memo",
            "navigator project communication create",
            "Communications stay server-only",
            "Entry only when its Document is client-visible",
            "a deadline only when it has a client lead time",
        ] {
            assert!(canonical.contains(expected), "skill must name `{expected}`");
        }
        assert!(
            !canonical.contains("kind: memo"),
            "the timeline is never a memo document"
        );
    }

    /// LAW-94: the fleet's `case-downloader` skill files an Authority through
    /// the one current verb, and a repository carrying a drifted copy is
    /// refused like any other synced skill.
    #[test]
    fn case_downloader_is_synced_and_files_authorities_through_project_sync() {
        let (_, canonical) = SYNCED_SKILLS
            .iter()
            .find(|(name, _)| *name == "case-downloader")
            .expect("case-downloader is a synced skill");
        for expected in [
            "navigator project sync",
            ".authority.yaml",
            "documents/cases/",
            "documents/rules/",
            "SHA256SUMS",
            "analyzeLaw",
            "get_case_pdf",
        ] {
            assert!(canonical.contains(expected), "skill must name `{expected}`");
        }
        for retired in ["navigator site sync", "navigator site pull"] {
            assert!(
                !canonical.contains(retired),
                "skill still names `{retired}`"
            );
        }

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(".agents/skills/case-downloader/SKILL.md");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, format!("{canonical}drift\n")).unwrap();
        let mut findings = Vec::new();
        validate_skills(root.path(), &mut findings);
        assert!(
            findings.iter().any(|f| f.message
                == "synced skill `case-downloader` has drifted from the canonical copy"),
            "{findings:?}"
        );
    }

    #[test]
    fn project_setup_skill_is_synced_and_uses_current_commands() {
        let (_, canonical) = SYNCED_SKILLS
            .iter()
            .find(|(name, _)| *name == "project-setup")
            .expect("project-setup is a synced skill");
        for required in [
            "navigator project scaffold",
            "navigator project setup <code>",
            "navigator project doctor --project <code>",
            "navigator project gate",
        ] {
            assert!(canonical.contains(required), "skill must name `{required}`");
        }
        for retired in [
            "copy them from Navigator",
            "gcloud iam",
            "ops application publisher",
            "WIF_PROVIDER",
            "queued",
        ] {
            assert!(
                !canonical.contains(retired),
                "skill must not direct `{retired}`"
            );
        }
    }

    #[test]
    fn opposing_communication_review_is_synced_and_covers_recipient_readings() {
        let (_, canonical) = SYNCED_SKILLS
            .iter()
            .find(|(name, _)| *name == "opposing-communication-review")
            .expect("opposing-communication-review is a synced skill");
        for required in [
            "recipient's perspective",
            "incoming messages",
            "drafts",
            "already sent",
            "Documented",
            "Inference",
            "Unknown",
            "Extension request",
            "read this as cooperation",
            "question why an extension is needed",
            "unconditional concession",
            "existing records suffice",
            "Conditional concession",
            "Preserve the condition",
            "distinguish the offered change",
            "ask for clarification only",
            "Routine acknowledgment",
            "receipt, not agreement",
            "do not recommend a corrective reply",
            "attorney work product",
            "client-visible record",
            "source Communication",
            "A review request is not",
            "to send, reply",
            "Never invent bad faith",
            "never assign a success probability",
        ] {
            assert!(
                canonical.contains(required),
                "skill must cover `{required}`"
            );
        }

        let root = tempfile::tempdir().unwrap();
        emit_scaffold(
            root.path(),
            "matter-one",
            "staging.neonlaw.com",
            "lawyer@example.com",
            "client@example.com",
        )
        .unwrap();
        let path = root
            .path()
            .join(".agents/skills/opposing-communication-review/SKILL.md");
        assert_eq!(fs::read_to_string(&path).unwrap(), *canonical);
        fs::write(&path, "drift\n").unwrap();
        assert!(validate_gate(root.path(), Some("matter-one"), false).errors > 0);

        let custom = root.path().join(".agents/skills/custom/SKILL.md");
        fs::create_dir_all(custom.parent().unwrap()).unwrap();
        fs::write(&custom, "# custom skill\n").unwrap();
        repair_skills(root.path()).unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), *canonical);
        assert_eq!(fs::read(&custom).unwrap(), b"# custom skill\n");
        assert_eq!(
            validate_gate(root.path(), Some("matter-one"), false).errors,
            0
        );
    }

    #[test]
    fn record_search_is_canonical_and_synced() {
        let (_, canonical) = SYNCED_SKILLS
            .iter()
            .find(|(name, _)| *name == "record-search")
            .expect("record-search is a canonical synced skill");
        assert!(canonical.contains("Kept\n  identical, byte for byte"));
        assert!(canonical.contains("navigator project search"));
        assert!(!canonical.contains("pdftotext"));
        assert!(!canonical.contains("Python's `email` package"));
    }

    #[test]
    fn record_search_is_repaired_and_drift_fails_the_project_gate() {
        let root = tempfile::tempdir().unwrap();
        emit_scaffold(
            root.path(),
            "matter-one",
            "staging.neonlaw.com",
            "lawyer@example.com",
            "client@example.com",
        )
        .unwrap();
        let path = root.path().join(".agents/skills/record-search/SKILL.md");
        let (_, canonical) = SYNCED_SKILLS
            .iter()
            .find(|(name, _)| *name == "record-search")
            .unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), *canonical);

        fs::write(&path, "drift\n").unwrap();
        assert!(validate_gate(root.path(), Some("matter-one"), false).errors > 0);
        repair_skills(root.path()).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), *canonical);
        assert_eq!(
            validate_gate(root.path(), Some("matter-one"), false).errors,
            0
        );
    }

    /// ENG-870 named this doc specifically: it still documented the
    /// retired `navigator site document verify` after that verb moved onto
    /// `navigator project gate --check`.
    #[test]
    fn project_repositories_doc_documents_only_commands_that_resolve() {
        let tree = crate::navigator_command();
        let contents = include_str!("../../../docs/project-repositories.md");
        let found = crate::projects::cli_docs::unresolved_invocations(contents, &tree);
        assert!(
            found.is_empty(),
            "docs/project-repositories.md documents a command that does not resolve: {found:?}"
        );
    }

    /// An older `AGENTS.md` fails the gate, `sync-skills` restores the embedded
    /// contract, and a Project's own skill is left in place.
    #[test]
    fn an_old_contract_fails_the_gate_until_sync_skills_restores_it() {
        let root = tempfile::tempdir().unwrap();
        emit_scaffold(
            root.path(),
            "matter-one",
            "staging.neonlaw.com",
            "lawyer@example.com",
            "client@example.com",
        )
        .unwrap();
        let custom = root.path().join(".agents/skills/custom/SKILL.md");
        fs::create_dir_all(custom.parent().unwrap()).unwrap();
        fs::write(&custom, "# custom skill\n").unwrap();
        fs::write(root.path().join("AGENTS.md"), "# older project contract\n").unwrap();

        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        validate_agent_contract(root.path(), &mut errors, &mut warnings);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].message.contains(&format!(
                "canonical contract embedded in Navigator CLI {}",
                crate::cli_version()
            )),
            "{errors:?}"
        );
        assert!(
            errors[0]
                .message
                .contains("navigator project repository sync-skills"),
            "{errors:?}"
        );
        assert!(validate_gate(root.path(), Some("matter-one"), false).errors > 0);

        let repaired = repair_skills(root.path()).unwrap();
        assert!(repaired.contains(&"AGENTS.md".to_string()), "{repaired:?}");
        assert_eq!(
            fs::read_to_string(root.path().join("AGENTS.md")).unwrap(),
            AGENT_CONTRACT_BASE
        );
        assert_eq!(fs::read(&custom).unwrap(), b"# custom skill\n");
        assert_eq!(
            validate_gate(root.path(), Some("matter-one"), false).errors,
            0
        );
        assert!(repair_skills(root.path()).unwrap().is_empty());
    }

    #[test]
    fn agents_md_must_match_the_canonical_contract() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        std::fs::write(root.path().join("AGENTS.md"), "# Working in acme\n").unwrap();
        let found = layout_findings(root.path());
        assert!(
            found
                .iter()
                .any(|finding| finding.contains("canonical contract")),
            "{found:?}"
        );
    }

    #[test]
    fn canonical_contract_marks_email_draft_only() {
        assert!(AGENT_CONTRACT_BASE.contains("## Critical: email is draft-only"));
        assert!(AGENT_CONTRACT_BASE.contains("Every email must remain a draft"));
        assert!(AGENT_CONTRACT_BASE.contains("never sends, replies to, or forwards email"));
        assert!(AGENT_CONTRACT_BASE.contains("lawyer reviews each draft"));
    }

    #[test]
    fn claude_settings_deny_email_delivery_and_preserve_draft_creation() {
        let settings: serde_json::Value =
            serde_json::from_str(CLAUDE_SETTINGS_CONTENTS).expect("valid Claude settings JSON");
        let denied = settings["permissions"]["deny"]
            .as_array()
            .expect("deny list");
        let denied: Vec<&str> = denied
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect();
        assert_eq!(
            denied,
            [
                "mcp__*__send_message",
                "mcp__*__send_draft",
                "mcp__*__reply",
                "mcp__*__forward",
                "mcp__codex_apps__gmail_send*",
                "mcp__codex_apps__gmail_reply*",
                "mcp__codex_apps__gmail_forward*"
            ]
        );
        // Claude Code globs `*` over any run of characters, including the
        // connector's server segment (a UUID in the desktop app).
        let glob = |rule: &str, tool: &str| {
            let mut parts = rule.split('*');
            let first = parts.next().unwrap_or_default();
            let Some(mut rest) = tool.strip_prefix(first) else {
                return false;
            };
            let parts: Vec<&str> = parts.collect();
            let Some((last, middle)) = parts.split_last() else {
                return rest.is_empty();
            };
            for part in middle {
                match rest.find(part) {
                    Some(at) => rest = &rest[at + part.len()..],
                    None => return false,
                }
            }
            rest.ends_with(last)
        };
        let matches = |tool: &str| denied.iter().any(|rule| glob(rule, tool));
        for blocked in [
            "mcp__claude_ai_Gmail__send_message",
            "mcp__claude_ai_Gmail__send_draft",
            "mcp__claude_ai_Gmail__reply",
            "mcp__claude_ai_Gmail__forward",
            "mcp__749f0d2d-8707-4882-911c-d98a553a3925__send_message",
            "mcp__749f0d2d-8707-4882-911c-d98a553a3925__reply",
            "mcp__749f0d2d-8707-4882-911c-d98a553a3925__forward",
            "mcp__codex_apps__gmail_send_email",
            "mcp__codex_apps__gmail_send_draft",
            "mcp__codex_apps__gmail_reply_email",
            "mcp__codex_apps__gmail_forward_emails",
        ] {
            assert!(matches(blocked), "delivery tool remains enabled: {blocked}");
        }
        for allowed in [
            "mcp__claude_ai_Gmail__create_draft",
            "mcp__claude_ai_Gmail__update_draft",
            "mcp__claude_ai_Gmail__list_drafts",
            "mcp__claude_ai_Gmail__get_thread",
            "mcp__claude_ai_Gmail__search_threads",
            "mcp__749f0d2d-8707-4882-911c-d98a553a3925__create_draft",
            "mcp__claude_ai_Slack__slack_send_message_draft",
            "mcp__codex_apps__gmail_create_draft",
            "mcp__codex_apps__gmail_update_draft",
            "mcp__codex_apps__gmail_list_drafts",
            "mcp__codex_apps__gmail_read_email",
            "mcp__codex_apps__gmail_search_emails",
        ] {
            assert!(!matches(allowed), "read/draft tool is denied: {allowed}");
        }
    }
    #[test]
    fn canonical_contract_requires_deliberate_client_visibility_review() {
        assert!(AGENT_CONTRACT_BASE.contains("## Branches"));
        assert!(AGENT_CONTRACT_BASE.contains("squash-merged"));
        assert!(AGENT_CONTRACT_BASE.contains("deleted on merge"));
        assert!(AGENT_CONTRACT_BASE.contains("Open pull requests ready for review by default"));
        assert!(AGENT_CONTRACT_BASE.contains("visibility: internal"));
        assert!(!AGENT_CONTRACT_BASE.contains("apps/portal/"));
        assert!(AGENT_CONTRACT_BASE.contains("Deadline"));
    }

    #[test]
    #[cfg(unix)]
    fn github_automerge_step_accepts_armed_or_already_merged_and_rejects_unarmed() {
        let (unarmed, unarmed_calls) = run_automerge_step("app-token", "0", "false", "OPEN");
        assert!(!unarmed.status.success());
        assert!(
            String::from_utf8_lossy(&unarmed.stderr).contains("did not arm auto-merge"),
            "{}",
            String::from_utf8_lossy(&unarmed.stderr)
        );
        assert!(unarmed_calls.contains("pr view"), "{unarmed_calls}");

        let (armed, armed_calls) = run_automerge_step("app-token", "0", "true", "OPEN");
        assert!(
            armed.status.success(),
            "{}",
            String::from_utf8_lossy(&armed.stderr)
        );
        assert!(armed_calls.contains("pr merge"), "{armed_calls}");
        assert!(armed_calls.contains("pr view"), "{armed_calls}");
        assert!(
            armed_calls.contains("autoMergeRequest,state"),
            "{armed_calls}"
        );
        assert!(
            armed_calls.contains("(.autoMergeRequest != null) or (.state == \"MERGED\")"),
            "{armed_calls}"
        );

        let (merged, merged_calls) = run_automerge_step("app-token", "0", "false", "MERGED");
        assert!(
            merged.status.success(),
            "{}",
            String::from_utf8_lossy(&merged.stderr)
        );
        assert!(merged_calls.contains("pr view"), "{merged_calls}");
    }

    #[test]
    #[cfg(unix)]
    fn github_automerge_step_keeps_the_missing_credentials_fallback() {
        let (output, calls) = run_automerge_step("", "0", "false", "OPEN");
        assert!(output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("credentials absent"),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(calls.is_empty(), "credential-free step invoked gh: {calls}");
    }

    #[test]
    #[cfg(unix)]
    fn github_automerge_step_stops_when_the_arm_command_fails() {
        let (output, calls) = run_automerge_step("app-token", "2", "false", "OPEN");
        assert!(!output.status.success());
        assert!(calls.contains("pr merge"), "{calls}");
        assert!(
            !calls.contains("pr view"),
            "read-back ran after arm failure: {calls}"
        );
    }

    #[test]
    fn legal_council_citations_ship_in_scaffold_and_skill_repair() {
        let root = tempfile::tempdir().unwrap();
        emit_scaffold(
            root.path(),
            "matter-one",
            "staging.neonlaw.com",
            "lawyer@example.com",
            "client@example.com",
        )
        .unwrap();
        let path = root.path().join(".agents/skills/legal-council/SKILL.md");
        let scaffold = fs::read_to_string(&path).unwrap();
        for case in [
            "Moran",
            "Maples",
            "Samsung",
            "Golden Eagle",
            "Iacangelo",
            "Momentous",
            "Melendres",
            "Electronic Trading",
            "Shadow Traffic",
            "Lennar",
            "Nextdoor",
            "Jenner",
        ] {
            assert!(scaffold.contains(case), "released skill must cite {case}");
        }
        fs::write(&path, "# outdated Legal Council\n").unwrap();
        assert!(validate_gate(root.path(), Some("matter-one"), false).errors > 0);
        repair_skills(root.path()).unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), scaffold);
        assert_eq!(
            validate_gate(root.path(), Some("matter-one"), false).errors,
            0
        );
    }

    #[test]
    fn repair_skills_restores_only_canonical_files_and_is_idempotent() {
        let root = tempfile::tempdir().unwrap();
        emit_scaffold(
            root.path(),
            "matter-one",
            "staging.neonlaw.com",
            "lawyer@example.com",
            "client@example.com",
        )
        .unwrap();
        let custom = root.path().join(".agents/skills/custom/SKILL.md");
        fs::create_dir_all(custom.parent().unwrap()).unwrap();
        fs::write(&custom, "# custom\n").unwrap();
        let (name, expected) = SYNCED_SKILLS[0];
        let canonical = root
            .path()
            .join(".agents/skills")
            .join(name)
            .join("SKILL.md");
        fs::remove_file(&canonical).unwrap();
        let (drifted_name, _) = SYNCED_SKILLS[1];
        fs::write(
            root.path()
                .join(".agents/skills")
                .join(drifted_name)
                .join("SKILL.md"),
            "drift\n",
        )
        .unwrap();
        let manifest_before = fs::read(root.path().join(PROJECT_MANIFEST)).unwrap();

        repair_skills(root.path()).unwrap();
        let first_run = fs::read(&canonical).unwrap();
        repair_skills(root.path()).unwrap();

        assert_eq!(first_run, expected.as_bytes());
        assert_eq!(fs::read(&canonical).unwrap(), first_run);
        assert_eq!(
            fs::read(
                root.path()
                    .join(".agents/skills")
                    .join(drifted_name)
                    .join("SKILL.md")
            )
            .unwrap(),
            SYNCED_SKILLS[1].1.as_bytes()
        );
        assert_eq!(fs::read(&custom).unwrap(), b"# custom\n");
        assert_eq!(
            fs::read(root.path().join(PROJECT_MANIFEST)).unwrap(),
            manifest_before
        );
    }

    /// ENG-1075: `sync-skills` deletes a retired synced skill, keeps a
    /// Project's own skill that shares the name, and no longer scaffolds one.
    #[test]
    fn repair_skills_deletes_retired_synced_skills_only() {
        let root = tempfile::tempdir().unwrap();
        emit_scaffold(
            root.path(),
            "matter-one",
            "staging.neonlaw.com",
            "lawyer@example.com",
            "client@example.com",
        )
        .unwrap();
        let skills = root.path().join(".agents/skills");
        for name in RETIRED_SYNCED_SKILLS {
            assert!(!skills.join(name).exists(), "`{name}` is still scaffolded");
            assert!(SYNCED_SKILLS.iter().all(|(synced, _)| synced != name));
        }
        let synced = skills.join("portal-chrome");
        fs::create_dir_all(&synced).unwrap();
        fs::write(
            synced.join("SKILL.md"),
            "---\nname: portal-chrome\ndescription: >\n  Kept identical; this canonical copy\n  lives in Navigator's own `.agents/skills/`.\n---\n",
        )
        .unwrap();
        let custom = skills.join("server");
        fs::create_dir_all(&custom).unwrap();
        fs::write(custom.join("SKILL.md"), "---\nname: server\n---\n# ours\n").unwrap();

        let removed = repair_skills(root.path()).unwrap();

        assert_eq!(removed, vec![".agents/skills/portal-chrome".to_string()]);
        assert!(!synced.exists());
        assert!(custom.join("SKILL.md").is_file());
        assert!(repair_skills(root.path()).unwrap().is_empty());
    }

    #[test]
    fn repair_skills_refuses_navigator_root_and_non_project_before_writing() {
        let navigator_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap();
        assert!(repair_skills(navigator_root).is_err());

        let arbitrary = tempfile::tempdir().unwrap();
        fs::create_dir_all(arbitrary.path().join(".agents/skills/custom")).unwrap();
        fs::write(
            arbitrary.path().join(".agents/skills/custom/SKILL.md"),
            "custom\n",
        )
        .unwrap();
        assert!(repair_skills(arbitrary.path()).is_err());
        assert!(!arbitrary
            .path()
            .join(".agents/skills/council/SKILL.md")
            .exists());
        assert_eq!(
            fs::read(arbitrary.path().join(".agents/skills/custom/SKILL.md")).unwrap(),
            b"custom\n"
        );
    }

    #[test]
    fn generated_project_is_byte_canonical_and_passes_its_gate() {
        let root = tempfile::tempdir().unwrap();
        emit_scaffold(
            root.path(),
            "matter-one",
            "staging.neonlaw.com",
            "lawyer@example.com",
            "client@example.com",
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(root.path().join(WORKFLOW)).unwrap(),
            workflow_for(DEFAULT_WORKFLOW_HOME, crate::cli_version())
        );
        assert_eq!(
            fs::read_to_string(root.path().join(CD_WORKFLOW)).unwrap(),
            cd_workflow_for(DEFAULT_WORKFLOW_HOME, crate::cli_version())
        );
        assert_eq!(
            fs::read_to_string(root.path().join(PROJECT_MANIFEST)).unwrap(),
            format!(
                "version: {}\nproject:\n  host: staging.neonlaw.com\n  name: matter-one\n  lawyer_dri: lawyer@example.com\n  client_dri: client@example.com\n  private_slack_channel: not provisioned\n  private_notion_page: not provisioned\n",
                crate::cli_version()
            )
        );
        for (name, canonical) in SYNCED_SKILLS {
            assert_eq!(
                fs::read_to_string(
                    root.path()
                        .join(".agents/skills")
                        .join(name)
                        .join("SKILL.md")
                )
                .unwrap(),
                *canonical
            );
        }
        assert_eq!(
            validate_gate(root.path(), Some("matter-one"), false).errors,
            0
        );
        assert_eq!(
            fs::read_to_string(root.path().join(".gitignore")).unwrap(),
            ROOT_GITIGNORE
        );
        let (draft, stub) =
            super::super::lifecycle::scaffold_draft(super::super::lifecycle::ONBOARDING);
        assert!(draft.starts_with("drafts/"), "{draft}");
        assert_eq!(fs::read_to_string(root.path().join(&draft)).unwrap(), stub);
        assert!(!root.path().join(TEMPLATE_DIRECTORY).exists());
        assert!(!root.path().join("notations").exists());
        assert_eq!(
            validate_gate(root.path(), Some("matter-one"), false),
            LayoutOutcome {
                errors: 0,
                warnings: 0
            }
        );
    }

    /// Write one Draft under `directory/` so the blueprint pass has a file.
    fn write_blueprint(root: &Path, directory: &str) {
        let path = root.join(directory).join("engagement__letter.md");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            "---\nkind: letter\ntitle: Engagement letter\n---\n\nBody.\n",
        )
        .unwrap();
    }

    fn blueprint_findings(root: &Path) -> (Vec<String>, Vec<String>) {
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        blueprint_selection(root, &mut errors, &mut warnings);
        let messages = |findings: Vec<Finding>| {
            findings
                .into_iter()
                .map(|finding| finding.message)
                .collect::<Vec<_>>()
        };
        (messages(errors), messages(warnings))
    }

    #[test]
    fn a_drafts_directory_is_canonical_and_clean() {
        let root = tempfile::tempdir().unwrap();
        write_blueprint(root.path(), DRAFT_DIRECTORY);
        assert_eq!(blueprint_findings(root.path()), (vec![], vec![]));
    }

    #[test]
    fn a_templates_directory_warns_with_a_rename_hint() {
        let root = tempfile::tempdir().unwrap();
        write_blueprint(root.path(), TEMPLATE_DIRECTORY);
        let (errors, warnings) = blueprint_findings(root.path());
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("git mv templates drafts"),
            "{warnings:?}"
        );
    }

    #[test]
    fn both_blueprint_directories_are_refused() {
        let root = tempfile::tempdir().unwrap();
        write_blueprint(root.path(), DRAFT_DIRECTORY);
        write_blueprint(root.path(), TEMPLATE_DIRECTORY);
        let (errors, warnings) = blueprint_findings(root.path());
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("both are present"), "{errors:?}");
    }

    #[test]
    fn ignored_notes_stay_out_of_the_layout_and_unignored_notes_are_refused() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        fs::create_dir(root.path().join("notes")).unwrap();
        fs::write(root.path().join("notes/call.md"), "Synthetic call note.\n").unwrap();
        assert_eq!(
            layout_findings(root.path()),
            vec!["`notes/` must stay git-ignored; a note feeds `navigator project communication create --body-file`, and the Communication's body lives in Navigator's store, never in Git".to_string()],
            "an unignored notes/ file must not reach Git"
        );

        fs::write(root.path().join(".gitignore"), ROOT_GITIGNORE).unwrap();
        assert_eq!(layout_findings(root.path()), Vec::<String>::new());
    }

    /// An unknown key, including a retired exemption key, is refused and names
    /// the accepted set. There is no per-repository exemption mechanism.
    #[test]
    fn a_manifest_carrying_an_unknown_key_is_refused() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        std::fs::write(
            root.path().join("navigator.yaml"),
            "host: staging.neonlaw.com\nproject: acme\nexempt_roots:\n  - documents\n  - evidence\n",
        )
        .unwrap();

        let found = layout_findings(root.path());
        assert!(
            found.iter().any(|finding| finding.contains("Y006")
                && finding.contains("exempt_roots")
                && finding.contains("host")),
            "{found:?}"
        );
    }

    #[test]
    fn a_manifest_carrying_an_arbitrary_unknown_key_is_refused() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        std::fs::write(
            root.path().join("navigator.yaml"),
            "host: staging.neonlaw.com\nproject: acme\ntotally_made_up_key: whatever\n",
        )
        .unwrap();

        let found = layout_findings(root.path());
        assert!(
            found
                .iter()
                .any(|finding| finding.contains("Y006") && finding.contains("totally_made_up_key")),
            "{found:?}"
        );
    }

    /// Admitting the manifest must not make the closed list permissive: the
    /// point of `ALLOWED_ROOTS` is that anything unenumerated is refused.
    #[test]
    fn an_unlisted_root_is_still_refused() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        std::fs::write(root.path().join("notes.md"), "scratch\n").unwrap();

        assert_eq!(
            layout_findings(root.path()),
            vec!["path is outside the source-only Project repository layout".to_string()]
        );
    }

    /// An unknown top-level directory is refused however deep its files sit,
    /// once, at the directory: nesting a file one level down must not walk
    /// around the closed root list.
    #[test]
    fn an_unlisted_top_level_directory_is_refused_once_at_any_depth() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        fs::create_dir_all(root.path().join("foo/deeper")).unwrap();
        fs::write(root.path().join("foo/bar.txt"), "scratch\n").unwrap();
        fs::write(root.path().join("foo/deeper/baz.txt"), "scratch\n").unwrap();

        let mut errors: Vec<Finding> = Vec::new();
        let mut warnings = Vec::new();
        validate_layout(root.path(), &mut errors, &mut warnings);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].path, root.path().join("foo"));
        assert!(
            errors[0].message.contains("`foo/` is outside"),
            "{}",
            errors[0].message
        );
    }

    /// Write each of `rels` under `root` with synthetic contents.
    fn write_files(root: &Path, rels: &[&str]) {
        for rel in rels {
            let path = root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "synthetic\n").unwrap();
        }
    }

    /// Every Node and repo-local tooling file kind warns where it sits, and
    /// none of them is an error yet.
    #[test]
    fn node_and_script_files_warn_and_name_pages() {
        let rels = [
            "tests/package.json",
            "tests/pnpm-lock.yaml",
            "tests/package-lock.json",
            "tests/yarn.lock",
            "tests/.nvmrc",
            "tests/tsconfig.json",
            "tests/tsconfig.base.json",
            "tests/check.ts",
            "tests/view.tsx",
            "tests/check.js",
            "tests/check.mjs",
            "tests/check.cjs",
            "tests/render.py",
            "tests/check.sh",
        ];
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        write_files(root.path(), &rels);

        assert_eq!(layout_findings(root.path()), Vec::<String>::new());
        let warnings = layout_warnings(root.path());
        assert_eq!(warnings.len(), rels.len(), "{warnings:?}");
        for warning in &warnings {
            assert!(warning.contains("`pages:`"), "{warning}");
            assert!(warning.contains("public `navigator` release"), "{warning}");
        }
    }

    /// A `scripts/` directory warns once, at the directory, and a root one is
    /// that warning rather than an unlisted-root error.
    #[test]
    fn a_scripts_directory_warns_once_instead_of_being_refused() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        write_files(
            root.path(),
            &[
                "scripts/README.md",
                "scripts/render.py",
                "tests/scripts/x.sh",
            ],
        );

        assert_eq!(layout_findings(root.path()), Vec::<String>::new());
        let mut errors: Vec<Finding> = Vec::new();
        let mut warnings = Vec::new();
        validate_layout(root.path(), &mut errors, &mut warnings);
        let paths: Vec<PathBuf> = warnings.into_iter().map(|w| w.path).collect();
        assert_eq!(
            paths,
            vec![
                root.path().join("scripts"),
                root.path().join("tests/scripts")
            ]
        );
    }

    /// Synced `.agents/` and `.github/` content and `seeds/` data are exempt.
    #[test]
    fn synced_content_and_seeds_are_exempt_from_the_tooling_warning() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        write_files(
            root.path(),
            &[".agents/skills/council/run.sh", "seeds/lookup.js"],
        );
        assert_eq!(layout_warnings(root.path()), Vec::<String>::new());
    }

    /// Every `Y024` finding, as messages, over a tree whose files are all
    /// stageable.
    fn tracked_file_findings(root: &Path, write_fixes: bool) -> Vec<String> {
        let mut errors: Vec<Finding> = Vec::new();
        let mut warnings: Vec<Finding> = Vec::new();
        validate_tracked_files(root, write_fixes, &mut errors, &mut warnings);
        errors
            .into_iter()
            .chain(warnings)
            .map(|finding| finding.message)
            .collect()
    }

    /// The scaffold-shaped minimal tree is Markdown, YAML, and
    /// `.claude/settings.json`, and passes.
    #[test]
    fn markdown_and_yaml_only_tree_passes_y024() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        write_files(
            root.path(),
            &[
                "drafts/onboarding.md",
                "blobs/letter.pdf.yaml",
                "seeds/people.yaml",
                "tests/README.md",
                ".agents/skills/council/SKILL.md",
                ".github/workflows/automerge.yml",
            ],
        );
        fs::write(root.path().join(".gitignore"), ROOT_GITIGNORE).unwrap();
        fs::write(root.path().join(".gitattributes"), CANONICAL_GITATTRIBUTES).unwrap();
        fs::write(root.path().join(".github/CODEOWNERS"), CODEOWNERS).unwrap();
        assert_eq!(
            tracked_file_findings(root.path(), false),
            Vec::<String>::new()
        );
    }

    /// One fixture per refused class: Node package files and lockfiles,
    /// TypeScript and JavaScript, Python, shell, `tests/` code, and a binary.
    /// Each is its own `Y024` error naming the path, wherever it sits —
    /// synced `.agents/` and `seeds/` included.
    #[test]
    fn every_refused_class_is_a_y024_error_naming_its_path() {
        let refused = [
            "package.json",
            "pnpm-lock.yaml",
            "portal/.yarnrc.yml",
            "apps/portal/package-lock.json",
            "portal/yarn.lock",
            "portal/src/main.ts",
            "portal/src/view.tsx",
            "portal/vite.config.js",
            "tests/render.test.mjs",
            "scripts/render.py",
            "scripts/check.sh",
            "tests/a.mjs",
            "tests/conventions.py",
            "documents/exhibit.png",
            ".agents/skills/server/run.sh",
            "seeds/lookup.js",
            "LICENSE.txt",
            "Makefile",
        ];
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        write_files(root.path(), &refused);
        fs::write(root.path().join("logo.bin"), [0_u8, 159, 146, 150]).unwrap();

        let findings = tracked_file_findings(root.path(), false);
        assert_eq!(findings.len(), refused.len() + 1, "{findings:#?}");
        for relative in refused.iter().chain(&["logo.bin"]) {
            assert!(
                findings
                    .iter()
                    .any(|finding| finding.starts_with(&format!("Y024: `{relative}` "))),
                "no finding names `{relative}`: {findings:#?}"
            );
        }
        assert!(findings.iter().all(|finding| finding.contains("`pages:`")));
    }

    /// The canonical non-text files are admitted only by their exact path:
    /// a `.gitignore` anywhere but the root — the retired `documents/.gitignore`
    /// included — or a second `settings.json`, is refused.
    #[test]
    fn canonical_exceptions_are_exact_paths() {
        assert!(tracked_file_admitted(".gitignore"));
        assert!(!tracked_file_admitted("documents/.gitignore"));
        assert!(tracked_file_admitted(".github/CODEOWNERS"));
        assert!(tracked_file_admitted(".claude/settings.json"));
        assert!(tracked_file_admitted("LICENSE"));
        assert!(!tracked_file_admitted("drafts/.gitignore"));
        assert!(!tracked_file_admitted("CODEOWNERS"));
        assert!(!tracked_file_admitted(".agents/settings.json"));
        assert!(!tracked_file_admitted("drafts/.md"));
        assert!(!tracked_file_admitted("README.MD"));
        assert!(tracked_file_admitted("drafts/engagement.letter.md"));
        assert!(!tracked_file_admitted("portal/pnpm-lock.yaml"));
        assert!(!tracked_file_admitted("pnpm-workspace.yaml"));
    }

    /// A drifted `.gitattributes` fails under `--ci` and is rewritten to the
    /// release's bytes locally. A root `.gitignore` without the protections
    /// fails too, and is never rewritten (LAW-181).
    #[test]
    fn drifted_canonical_files_fail_in_ci_and_only_gitattributes_is_fixed_locally() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        fs::write(root.path().join(".gitignore"), "node_modules\n").unwrap();
        fs::write(root.path().join(".gitattributes"), "*.pdf binary\n").unwrap();
        fs::write(root.path().join("LICENSE"), "All rights reserved.\n").unwrap();

        let findings = tracked_file_findings(root.path(), false);
        assert_eq!(findings.len(), 3, "{findings:#?}");
        for relative in [".gitattributes", "LICENSE"] {
            assert!(
                findings
                    .iter()
                    .any(|finding| finding.starts_with(&format!("Y024: `{relative}` must match"))),
                "{findings:#?}"
            );
        }
        assert!(
            findings
                .iter()
                .any(|finding| finding.starts_with("Y024: `.gitignore` lacks")
                    && finding.contains("`/documents/`, `*.pdf`, `*.docx`, `*.local` a document")
                    && finding.contains("sync-skills")),
            "{findings:#?}"
        );

        let local = tracked_file_findings(root.path(), true);
        assert_eq!(local.len(), 2, "{local:#?}");
        assert_eq!(
            fs::read_to_string(root.path().join(".gitignore")).unwrap(),
            "node_modules\n",
            "the gate never rewrites the root .gitignore"
        );
        assert_eq!(
            fs::read_to_string(root.path().join("LICENSE")).unwrap(),
            "All rights reserved.\n"
        );
        assert_eq!(
            fs::read_to_string(root.path().join(".gitattributes")).unwrap(),
            CANONICAL_GITATTRIBUTES
        );
    }

    /// LAW-181: a custom root `.gitignore` that keeps the protections but
    /// lacks another canonical line is left byte-identical, and its drift is
    /// a warning. One that only adds its own lines is not drift at all.
    #[test]
    fn the_gate_reports_root_gitignore_drift_and_leaves_the_file_byte_identical() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        let custom =
            "# Our own ignores.\n/documents/\nnotes/\n*.pdf\n*.docx\n*.local\nnode_modules\n";
        fs::write(root.path().join(".gitignore"), custom).unwrap();

        let mut errors: Vec<Finding> = Vec::new();
        let mut warnings: Vec<Finding> = Vec::new();
        validate_tracked_files(root.path(), true, &mut errors, &mut warnings);
        assert!(errors.is_empty(), "{errors:#?}");
        assert_eq!(warnings.len(), 1, "{warnings:#?}");
        assert!(
            warnings[0].message.contains("has drifted")
                && warnings[0].message.contains("`.DS_Store`")
                && warnings[0].message.contains("never rewrites"),
            "{}",
            warnings[0].message
        );
        assert_eq!(
            fs::read_to_string(root.path().join(".gitignore")).unwrap(),
            custom
        );

        let extended = format!("{ROOT_GITIGNORE}node_modules\n");
        fs::write(root.path().join(".gitignore"), &extended).unwrap();
        assert_eq!(
            tracked_file_findings(root.path(), true),
            Vec::<String>::new()
        );
        assert_eq!(
            fs::read_to_string(root.path().join(".gitignore")).unwrap(),
            extended
        );
    }

    /// `sync-skills` restores the canonical lines and keeps every line the
    /// Project added, so a repair never drops an ignore rule.
    #[test]
    fn repairing_the_root_gitignore_restores_protections_and_keeps_project_lines() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join(".gitignore"),
            "notes/\nnode_modules\ndist\n",
        )
        .unwrap();
        let mut repaired = Vec::new();
        repair_root_gitignore(root.path(), &mut repaired).unwrap();
        assert_eq!(repaired, vec![".gitignore".to_string()]);
        let contents = fs::read_to_string(root.path().join(".gitignore")).unwrap();
        assert!(contents.starts_with(ROOT_GITIGNORE), "{contents}");
        assert!(contents.ends_with("node_modules\ndist\n"), "{contents}");
        assert_eq!(contents.matches("notes/").count(), 1, "{contents}");
        assert!(missing_root_gitignore_lines(&contents).is_empty());

        let mut again = Vec::new();
        repair_root_gitignore(root.path(), &mut again).unwrap();
        assert!(again.is_empty(), "a repaired file is left alone");
    }

    /// LAW-189: a pointer the layout before `blobs/` committed is a warning
    /// for one release; bytes and a pointer inside a matter folder are not.
    #[test]
    fn the_gate_refuses_a_stray_pointer_and_only_warns_on_a_flat_one() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        write_files(
            root.path(),
            &["blobs/complaint.pdf.yaml", "documents/flat.pdf.yaml"],
        );
        assert_eq!(layout_findings(root.path()), Vec::<String>::new());
        let warnings = layout_warnings(root.path());
        assert_eq!(warnings.len(), 1, "{warnings:#?}");
        assert!(warnings[0].contains("migrate-documents"), "{}", warnings[0]);

        write_files(
            root.path(),
            &[
                "documents/Discovery/stray.pdf.yaml",
                "documents/Work Product/memo.txt",
            ],
        );
        let errors = layout_findings(root.path());
        assert_eq!(errors.len(), 2, "{errors:#?}");
        assert!(
            errors
                .iter()
                .all(|error| error.contains("belongs under `blobs/`")),
            "{errors:#?}"
        );
    }

    /// `Y014`: under `--ci` the legacy layout is a warning for one release,
    /// and the checkout is left alone.
    #[test]
    fn the_legacy_layout_warns_in_ci_for_one_release() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("documents")).unwrap();
        fs::write(
            root.path().join("documents/.gitignore"),
            "*\n!*/\n!*.yaml\n!.gitignore\n",
        )
        .unwrap();
        let (mut errors, mut warnings) = (Vec::new(), Vec::new());
        validate_documents_gitignore(root.path(), false, &mut errors, &mut warnings);
        assert!(errors.is_empty(), "{errors:#?}");
        assert_eq!(warnings.len(), 1, "{warnings:#?}");
        assert!(root.path().join("documents/.gitignore").is_file());
    }

    #[test]
    fn a_vite_application_warns_once_with_a_pointer_to_pages() {
        for app in ["portal", "apps/portal"] {
            let root = tempfile::tempdir().unwrap();
            scaffold_minimal(root.path());
            let rels: Vec<String> = [
                "package.json",
                "pnpm-lock.yaml",
                "tsconfig.json",
                "src/main.tsx",
                "scripts/build.sh",
            ]
            .iter()
            .map(|file| format!("{app}/{file}"))
            .collect();
            write_files(
                root.path(),
                &rels.iter().map(String::as_str).collect::<Vec<_>>(),
            );

            let mut errors: Vec<Finding> = Vec::new();
            let mut warnings = Vec::new();
            validate_layout(root.path(), &mut errors, &mut warnings);
            assert_eq!(warnings.len(), 1, "{app}: {warnings:?}");
            assert_eq!(warnings[0].path, root.path().join(app));
            assert!(warnings[0].message.contains("`pages:`"), "{warnings:?}");
        }
    }

    /// The allowed roots are walked by their own rules, not refused for
    /// carrying nested files.
    #[test]
    fn nested_files_under_allowed_roots_are_not_unlisted() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        for rel in ["drafts/will.md", "seeds/Person.yaml", "tests/README.md"] {
            let path = root.path().join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "synthetic\n").unwrap();
        }
        assert_eq!(layout_findings(root.path()), Vec::<String>::new());
    }

    #[test]
    fn a_documented_cli_verb_that_does_not_resolve_is_refused() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        std::fs::write(
            root.path().join("AGENTS.md"),
            "# contract\n\nRun `navigator template render file.md` then `navigator notation preview`.\n",
        )
        .unwrap();
        let mut errors: Vec<Finding> = Vec::new();
        super::validate_documented_cli(root.path(), &mut errors);
        let found: Vec<String> = errors.into_iter().map(|error| error.message).collect();
        assert!(
            found.iter().any(|finding| finding.contains("template")
                && finding.contains("does not resolve")),
            "{found:?}"
        );
        assert!(
            found
                .iter()
                .all(|finding| !finding.contains("notation preview")),
            "{found:?}"
        );
    }

    /// The forbidden-path checks run independently of the allowed-root list, so
    /// admitting a new root cannot open a door for build output beside it.
    #[test]
    fn a_forbidden_component_still_wins() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        std::fs::write(
            root.path().join("navigator.yaml"),
            "host: staging.neonlaw.com\nproject: acme\n",
        )
        .unwrap();
        std::fs::write(root.path().join(".env"), "SECRET=1\n").unwrap();

        let found = layout_findings(root.path());
        assert!(
            found
                .iter()
                .any(|finding| finding.contains("must not be committed")),
            "{found:?}"
        );
    }

    #[test]
    fn the_generated_gate_is_one_unfiltered_required_job() {
        let generated = workflow(FIXTURE_PIN);
        assert!(generated.contains("\n  ci:\n"), "{generated}");
        assert!(
            !generated.contains("paths:"),
            "a path-filtered required check can be satisfied by a skip"
        );
        assert!(generated.contains("project-gate.yml@"));
        assert!(
            !generated.contains("with:"),
            "the caller must carry no `with:` block; the reusable workflow reads project/host from navigator.yaml:\n{generated}"
        );
        assert!(!generated.contains("push:"));
        assert!(generated.contains("permissions:\n  contents: read\n  id-token: write"));
        assert!(!generated.contains("project_repository: true"));
    }

    /// The pin the caller names reaches both places that carry it, and no
    /// literal survives in the generator.
    #[test]
    fn the_generated_gate_pins_the_version_it_was_given() {
        let generated = workflow("26.8.23");
        assert!(
            generated
                .contains("uses: neon-law/navigator/.github/workflows/project-gate.yml@26.8.23"),
            "{generated}"
        );
        assert!(!generated.contains("with:"), "{generated}");
        assert!(!generated.contains("version:"), "{generated}");
        assert!(
            !generated.contains("26.7.27"),
            "a hard-coded literal is back:\n{generated}"
        );
    }

    /// One command gates the repository, so one job runs it. `verify` builds
    /// every application and then gates the whole tree; `documents` and
    /// `seeds` remain because each asks the deployment something the offline
    /// gate cannot.
    #[test]
    fn the_reusable_workflow_fans_two_jobs_into_the_required_check() {
        let generated = include_str!("../../../.github/workflows/project-gate.yml");
        for job in ["verify:", "documents:", "seeds:"] {
            assert!(
                generated.contains(&format!("\n  {job}\n")),
                "missing job `{job}`:\n{generated}"
            );
        }
        for retired in ["\n  lint:\n", "\n  notation:\n", "\n  manifest:\n"] {
            assert!(
                !generated.contains(retired),
                "`{retired}` ground now belongs to verify:\n{generated}"
            );
        }
        assert!(
            generated.contains("\n  ci:\n    needs: [read-manifest, verify, documents, seeds]\n"),
            "{generated}"
        );
    }

    #[test]
    fn the_required_check_asserts_every_dependencys_result() {
        let generated = include_str!("../../../.github/workflows/project-gate.yml");
        assert!(generated.contains("if: always()"), "{generated}");
        for job in ["read-manifest", "verify", "documents", "seeds"] {
            assert!(
                generated.contains(&format!("needs.{job}.result")),
                "the required check does not check `{job}`'s result:\n{generated}"
            );
        }
        for retired in [
            "needs.lint.result",
            "needs.notation.result",
            "needs.manifest.result",
        ] {
            assert!(
                !generated.contains(retired),
                "`{retired}` is retired, so nothing should still check it:\n{generated}"
            );
        }
    }

    /// A Project repository commits only source, so `verify` builds nothing:
    /// it installs the pinned CLI and runs the gate, and no step sets up Node
    /// or builds a portal.
    #[test]
    fn the_verify_job_builds_nothing_before_it_gates() {
        let generated = include_str!("../../../.github/workflows/project-gate.yml");
        let verify = generated
            .split_once("\n  verify:\n")
            .expect("no verify job")
            .1
            .split_once("\n  documents:\n")
            .expect("verify is not followed by documents")
            .0;
        assert!(
            verify.contains("navigator project gate --ci"),
            "verify does not run the gate:\n{verify}"
        );
        assert!(
            verify.contains("/.github/actions/navigator-install@"),
            "verify validates without installing the pinned CLI:\n{verify}"
        );
        for retired in [
            "navigator project portal",
            "navigator project applications",
            "setup-node",
            "pnpm",
        ] {
            assert!(
                !verify.contains(retired),
                "verify still carries `{retired}`:\n{verify}"
            );
        }
    }

    /// LAW-62: `--check` (`run_document_check_gate` in `cli/src/main.rs`)
    /// runs only the live document check. `verify` already ran the whole
    /// offline gate.
    #[test]
    fn the_documents_job_never_builds() {
        let generated = include_str!("../../../.github/workflows/project-gate.yml");
        let documents = generated
            .split_once("\n  documents:\n")
            .expect("no documents job")
            .1
            .split_once("\n  seeds:\n")
            .expect("documents is not followed by seeds")
            .0;
        assert!(
            !documents.contains("navigator project portal"),
            "documents has nothing to build now that `--check` runs only the \
             live document check:\n{documents}"
        );
        assert!(
            documents.contains("navigator project gate --check --ci"),
            "{documents}"
        );
        assert!(
            documents.contains("/.github/actions/navigator-install@"),
            "documents validates without installing the pinned CLI:\n{documents}"
        );
    }

    /// The generated CD caller delegates publication to Navigator's reusable workflow.
    #[test]
    fn cd_workflow_publishes_through_the_pinned_actions() {
        let generated = cd_workflow(FIXTURE_PIN);
        assert!(!generated.contains("TBD"), "{generated}");
        assert!(
            generated.contains("push:\n    branches: [main]"),
            "{generated}"
        );
        assert!(generated.contains("id-token: write"), "{generated}");
        assert!(
            generated.contains("neon-law/navigator/.github/workflows/project-publish.yml@26.8.23"),
            "{generated}"
        );
        assert!(generated.contains("workflow_dispatch:"), "{generated}");
        assert!(
            !generated.contains("with:"),
            "the gate/publish callers must carry no `with:` block; the reusable workflow reads project/host from navigator.yaml:\n{generated}"
        );
        assert!(
            !generated.contains("NAVIGATOR_APPLICATIONS_BUCKET"),
            "{generated}"
        );
    }

    /// The pin reaches the reusable publisher, and no literal survives.
    #[test]
    fn cd_workflow_pins_the_version_it_was_given() {
        let generated = cd_workflow("26.8.23");
        assert!(
            generated.contains("neon-law/navigator/.github/workflows/project-publish.yml@26.8.23"),
            "{generated}"
        );
        assert!(
            !generated.contains("26.7.27"),
            "a hard-coded literal is back:\n{generated}"
        );
    }

    /// Neither generated workflow takes a Project code, and this proves
    /// nothing project-specific leaks in behind that: the only organization
    /// named in either file is Navigator's own, the one exception
    /// `cli/tests/forge_coordinate_retired.rs` allows.
    #[test]
    fn the_generated_workflows_name_no_project() {
        for generated in [workflow(FIXTURE_PIN), cd_workflow(FIXTURE_PIN)] {
            for line in generated.lines() {
                let Some(reference) = line.trim_start().strip_prefix("uses: ") else {
                    continue;
                };
                let owner = reference.split('/').next().unwrap_or_default();
                assert!(
                    matches!(owner, "actions" | "pnpm" | "neon-law"),
                    "unexpected organization `{owner}` in generated workflow:\n{generated}"
                );
            }
            assert!(
                !generated.contains("NAVIGATOR_GCP_PROJECT_ID"),
                "a deployment coordinate leaked into the generated workflow:\n{generated}"
            );
        }
    }

    /// Neither generated workflow ever names Google Drive as a publish
    /// destination.
    ///
    /// ENG-73: object storage is the working-file authority and Drive is a
    /// per-Project ingest source only — CI writes into the documents or
    /// applications bucket, never into Drive. A folder ID committed to a
    /// generated workflow would also be attacker-controlled the same way a
    /// literal bucket name would be, so this guards the same class of
    /// mistake `the_generated_workflows_name_no_project` guards for
    /// organizations. Neither generated workflow requests a Drive OAuth
    /// scope or writes Drive at all, so `contains("drive")` is exact — no
    /// legitimate line needs the word.
    #[test]
    fn the_generated_workflows_never_target_drive() {
        for generated in [workflow(FIXTURE_PIN), cd_workflow(FIXTURE_PIN)] {
            let lowered = generated.to_lowercase();
            assert!(
                !lowered.contains("drive"),
                "a generated workflow names Drive; CI must publish only to \
                 object storage:\n{generated}"
            );
        }
    }

    /// The default `--action-version` pin `ops github setup` falls back to is
    /// empty, or it is a release tag — never a version-shaped string this
    /// build merely happens to carry.
    ///
    /// This is the guard that makes the invariant structural rather than
    /// asserted once: a hard-coded pin cannot be checked by anything, because
    /// it is correct on the day it is typed and nothing revisits it, while
    /// this assertion runs on every build. `cargo test` itself is the "cannot
    /// vouch for it" case — it bakes neither a runtime nor a build-time
    /// `NAVIGATOR_RELEASE_TAG` — so `published_cli_version()` is empty here.
    /// This test is what a release CLI build, or one built with
    /// `NAVIGATOR_RELEASE_TAG` set, has to satisfy instead.
    #[test]
    fn the_default_action_version_pin_is_a_release_tag_or_empty() {
        let default = crate::published_cli_version();
        if default.is_empty() {
            return;
        }
        assert!(
            is_release_tag(default),
            "the default action-version pin would be `{default}`, which is not an exact release tag"
        );
        assert_eq!(findings(&workflow(default)), Vec::<String>::new());
    }

    /// `Y010`: the mark with a corporate suffix is an entity claim, and the
    /// claim is measured against the entity of record the caller passes, so
    /// the spelling that *is* the entity passes and every other one fails.
    #[test]
    fn a_template_naming_the_mark_with_a_corporate_suffix_is_refused() {
        for (body, spelling) in [
            (
                "The Company, Neon Law, Inc., engages the Client.",
                "Neon Law, Inc.",
            ),
            ("**Neon Law PLLC** (the \"Firm\")", "Neon Law PLLC"),
            ("between Neon Law LLC and the Client", "Neon Law LLC"),
            ("Neon Law,   P.L.L.C.", "Neon Law, P.L.L.C."),
            (
                "engaged Neon Law Incorporated today",
                "Neon Law Incorporated",
            ),
            ("Neon Law, inc. signs below", "Neon Law, inc."),
        ] {
            assert_eq!(
                misnamed_firm_entities(body, "Shook Law PC"),
                vec![(1, spelling.to_string())],
                "{body}"
            );
        }
        assert_eq!(
            misnamed_firm_entities("---\ntitle: x\n---\n\nNeon Law, Inc.\n", "Shook Law PC"),
            vec![(5, "Neon Law, Inc.".to_string())]
        );
        // The comparison is against the entity of record, not against every
        // suffix: were the firm itself `Neon Law PLLC`, that spelling passes.
        assert_eq!(
            misnamed_firm_entities("engages Neon Law PLLC", "Neon Law PLLC"),
            Vec::new()
        );
    }

    /// The bare mark, the Licensor's own name, a longer word that merely
    /// starts with the mark, and the entity of record itself are not entity
    /// claims: a template may trade under the mark and may cite
    /// `Neon Law IP LLC` as the Licensor.
    #[test]
    fn the_mark_alone_and_the_licensor_are_not_entity_claims() {
        for body in [
            "Neon Law",
            "Neon Law.",
            "Neon Law helps you.",
            "Neon Law, a practice of Shook Law PC, will",
            "Neon Law IP LLC licenses the mark.",
            "Neon Lawyers Inc",
            "Neon Law Incorporates the terms",
            "Shook Law PC",
        ] {
            assert_eq!(
                misnamed_firm_entities(body, "Shook Law PC"),
                Vec::new(),
                "{body}"
            );
        }
    }

    #[test]
    fn onboarding_and_offboarding_templates_refuse_neon_law_as_a_trade_name() {
        for kind in ["onboarding", "offboarding"] {
            let body = format!(
                "---\nkind: {kind}\ntitle: Letter\n---\n\n\
                 Thank you for choosing Neon Law, a practice of Shook Law PC.\n\
                 Shook Law PC, trading as Neon Law\n\
                 Shook Law PC (\"Neon Law\")\n\
                 Neon Law\n\
                 Write to contact@neonlaw.com or use the Neon Law portal.\n"
            );
            let claims = firm_trade_name_claims(&body);
            assert_eq!(claims.len(), 4, "{body}: {claims:?}");
            assert!(claims.iter().all(|(_, claim)| claim == "Neon Law"));
        }
    }

    #[test]
    fn firm_trade_name_rule_ignores_other_kinds_and_contact_references() {
        for body in [
            "---\nkind: letter\ntitle: Letter\n---\n\nShook Law PC, trading as Neon Law\n",
            "---\nkind: onboarding\ntitle: Letter\n---\n\nWrite to contact@neonlaw.com or use the Neon Law portal.\n",
        ] {
            assert!(firm_trade_name_claims(body).is_empty(), "{body}");
        }
    }

    #[test]
    fn california_templates_refuse_an_llc_as_the_law_firm_but_allow_nonparty_mentions() {
        let california = concat!(
            "---\nkind: onboarding\ntitle: Letter\njurisdiction: CA\n---\n",
            "Thank you for choosing Shook Law PLLC.\n",
            "The attorney's license is held by Nick Shook.\n",
            "The client may have an LLC.\n",
        );
        assert_eq!(california_law_firm_entity_claims(california), vec![6]);

        let nv = california.replace("jurisdiction: CA", "jurisdiction: NV");
        assert!(california_law_firm_entity_claims(&nv).is_empty());

        let client_reference = california.replace(
            "Thank you for choosing Shook Law PLLC.",
            "The client may have an LLC.",
        );
        assert!(california_law_firm_entity_claims(&client_reference).is_empty());
    }

    #[test]
    fn california_templates_accept_the_firm_of_record_and_the_individual_lawyer() {
        let template = |body: &str| {
            format!("---\nkind: onboarding\ntitle: Letter\njurisdiction: CA\n---\n{body}\n")
        };
        for party in [
            "Thank you for choosing Shook Law PC.\n\n**Shook Law PC**",
            "Thank you for choosing Nick Shook, Attorney at Law (State Bar No. 000000), called \"the Firm\" in this letter.\n\n**Nick Shook, Attorney at Law**",
        ] {
            assert!(
                california_law_firm_entity_claims(&template(party)).is_empty(),
                "{party}"
            );
        }
        assert_eq!(
            california_law_firm_entity_claims(&template(
                "Thank you for choosing Shook Law PLLC.\n\n**Shook Law PLLC**"
            )),
            vec![6, 8]
        );
    }

    /// Through the real template lint: the finding carries the rule code, the
    /// line, the spelling the file used, and the entity of record.
    /// `Y019`: a pleading in a Project repository gates on the same matter and
    /// frontmatter fields `notation pdf` and `notation word` compose from.
    #[test]
    fn lint_project_template_names_a_pleading_s_missing_caption_fields() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/synthetic_matter");
        let manifest = std::fs::read_to_string(fixture.join("navigator.yaml")).unwrap();
        std::fs::write(
            dir.path().join("navigator.yaml"),
            manifest.replace("  department: \"12\"\n", ""),
        )
        .unwrap();
        std::fs::create_dir(dir.path().join("templates")).unwrap();
        let template = std::fs::read_to_string(fixture.join("templates/rfa_responses.md"))
            .unwrap()
            .replace(
                "footer_title: PLAINTIFF'S RESPONSES TO DEFENDANT'S REQUESTS FOR ADMISSION, SET ONE\n",
                "",
            );
        let path = dir.path().join("templates/rfa_responses.md");
        std::fs::write(&path, template).unwrap();

        let mut errors: Vec<Finding> = Vec::new();
        let mut warnings: Vec<Finding> = Vec::new();
        let mut declared = std::collections::BTreeMap::new();
        lint_project_template(&path, &[], &mut declared, &mut errors, &mut warnings);
        let message = errors
            .iter()
            .map(|error| error.message.as_str())
            .find(|message| message.starts_with("Y019: "))
            .expect("a Y019 finding");
        assert!(message.contains("matter.department"), "{message}");
        assert!(message.contains("footer_title"), "{message}");
    }

    #[test]
    fn lint_project_template_reports_y010_with_the_line_and_the_spelling() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("onboarding__letter.md");
        let template = concat!(
            "---\n",
            "kind: onboarding\n",
            "title: Onboarding letter\n",
            "respondent_type: entity\n",
            "code: onboarding\n",
            "jurisdiction: NV\n",
            "confidential: true\n",
            "questionnaire:\n",
            "  BEGIN:\n",
            "    _: END\n",
            "  END: {}\n",
            "workflow:\n",
            "  BEGIN:\n",
            "    intake_submitted: lawyer_review\n",
            "  lawyer_review:\n",
            "    approved: END\n",
            "    rejected: END\n",
            "  END: {}\n",
            "---\n",
            "\n",
            "This letter engages Neon Law, Inc. (the \"Firm\").\n",
        )
        .to_string();
        let line = template
            .lines()
            .position(|line| line.contains("Neon Law, Inc."))
            .expect("the body carries the spelling")
            + 1;
        std::fs::write(&path, &template).unwrap();

        let mut errors: Vec<Finding> = Vec::new();
        let mut warnings: Vec<Finding> = Vec::new();
        let mut declared = std::collections::BTreeMap::new();
        lint_project_template(&path, &[], &mut declared, &mut errors, &mut warnings);
        let messages: Vec<String> = errors.into_iter().map(|error| error.message).collect();
        assert!(
            messages.iter().any(|message| {
                message.starts_with(&format!("{ENTITY_CODE}: line {line} "))
                    && message.contains("`Neon Law, Inc.`")
                    && message.contains(store::seed::FIRM_ENTITY_NAME)
            }),
            "{messages:?}"
        );
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn lint_project_template_reports_y017_but_keeps_contact_references() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("onboarding__letter.md");
        let template = concat!(
            "---\n",
            "kind: onboarding\n",
            "title: Onboarding letter\n",
            "respondent_type: entity\n",
            "code: onboarding\n",
            "jurisdiction: NV\n",
            "confidential: true\n",
            "questionnaire:\n",
            "  BEGIN:\n",
            "    _: END\n",
            "  END: {}\n",
            "workflow:\n",
            "  BEGIN:\n",
            "    intake_submitted: lawyer_review\n",
            "  lawyer_review:\n",
            "    approved: END\n",
            "    rejected: END\n",
            "  END: {}\n",
            "---\n",
            "\n",
            "Shook Law PC, trading as Neon Law\n",
            "Write to contact@neonlaw.com or use the Neon Law portal.\n",
        );
        let line = template
            .lines()
            .position(|line| line.contains("trading as Neon Law"))
            .expect("the body carries the trade-name claim")
            + 1;
        std::fs::write(&path, template).unwrap();

        let mut errors: Vec<Finding> = Vec::new();
        let mut warnings: Vec<Finding> = Vec::new();
        let mut declared = std::collections::BTreeMap::new();
        lint_project_template(&path, &[], &mut declared, &mut errors, &mut warnings);
        let messages: Vec<String> = errors.into_iter().map(|error| error.message).collect();
        assert!(
            messages.iter().any(|message| {
                message.starts_with(&format!("{FIRM_TRADE_NAME_CODE}: line {line} "))
                    && message.contains("Neon Law")
                    && message.contains(store::seed::FIRM_ENTITY_NAME)
            }),
            "{messages:?}"
        );
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    /// The generated CD caller passes its own structural validation, the same
    /// invariant `the_scaffolded_gate_passes_its_own_validation` proves for
    /// `ci.yml`: the generator and the validator must agree.
    #[test]
    fn the_scaffolded_cd_workflow_passes_its_own_validation() {
        assert_eq!(cd_findings(&cd_workflow(FIXTURE_PIN)), Vec::<String>::new());
    }

    /// ENG-675 reproduction 4: `pull_request_target` runs with the base
    /// repository's secrets and write token against a fork's checked-out
    /// code, so a caller that swaps it in for `pull_request` must be refused
    /// by name, not merely by shape.
    #[test]
    fn a_ci_gate_triggered_by_pull_request_target_is_refused() {
        let contents = r"name: ci
on:
  pull_request_target:
jobs:
  ci:
    uses: neon-law/navigator/.github/workflows/project-gate.yml@26.7.27
";
        let found = findings(contents);
        assert!(
            found
                .iter()
                .any(|message| message.contains("pull_request_target")),
            "{found:?}"
        );
    }

    /// ENG-675 reproduction 3: an extra job with `contents: write` and
    /// `id-token: write` next to the thin project-gate caller is a second,
    /// unreviewed door into the same required check, so a CI gate must be
    /// exactly one job.
    #[test]
    fn a_ci_gate_carrying_an_extra_privileged_job_is_refused() {
        let contents = r"name: ci
on:
  pull_request:
jobs:
  ci:
    uses: neon-law/navigator/.github/workflows/project-gate.yml@26.7.27
  smuggled:
    permissions:
      contents: write
      id-token: write
    runs-on: ubuntu-latest
    steps:
      - run: echo pwned
";
        let found = findings(contents);
        assert!(
            found
                .iter()
                .any(|message| message.contains("exactly one job")),
            "{found:?}"
        );
    }

    /// A CI gate may declare only the minimal checkout and OIDC permissions;
    /// write-capable or otherwise different grants are refused.
    #[test]
    fn a_ci_gate_declaring_permissions_is_refused() {
        let contents = r"name: ci
on:
  pull_request:
permissions:
  contents: write
  id-token: write
jobs:
  ci:
    uses: neon-law/navigator/.github/workflows/project-gate.yml@26.7.27
";
        let found = findings(contents);
        assert!(
            found
                .iter()
                .any(|message| message.contains("must declare exactly `permissions")),
            "{found:?}"
        );
    }

    /// The `gate` and `publish` jobs on `cd.yml` are the other two callers
    /// this issue names: a `with:` block is rejected there exactly as it is
    /// on `ci.yml`, not merely checked for agreement with the manifest.
    #[test]
    fn a_cd_gate_or_publish_with_block_is_rejected() {
        let contents = r#"name: cd
on:
  push:
    branches: [main]
  workflow_dispatch:
permissions:
  contents: read
  id-token: write
jobs:
  gate:
    uses: neon-law/navigator/.github/workflows/project-gate.yml@26.7.27
    with:
      project: "acme"
  publish:
    needs: gate
    uses: neon-law/navigator/.github/workflows/project-publish.yml@26.7.27
    with:
      host: "staging.neonlaw.com"
"#;
        let found = cd_findings(contents);
        assert!(
            found.iter().any(|message| message.contains("job `gate`")
                && message.contains("must not declare a `with:` block")),
            "{found:?}"
        );
        assert!(
            found.iter().any(|message| message.contains("job `publish`")
                && message.contains("must not declare a `with:` block")),
            "{found:?}"
        );
    }

    /// ENG-675 reproduction 2: an arbitrary replacement `cd.yml` — an
    /// unrelated job with none of the pinned reusable-workflow calls — must
    /// be refused by the same structural check that closes `ci.yml`.
    #[test]
    fn an_arbitrary_cd_workflow_is_refused() {
        let contents = r"name: cd
on:
  push:
    branches: [main]
  workflow_dispatch:
permissions:
  contents: read
  id-token: write
jobs:
  deploy:
    runs-on: ubuntu-latest
    steps:
      - run: ./deploy.sh
";
        let found = cd_findings(contents);
        assert!(
            found
                .iter()
                .any(|message| message.contains("exactly the `gate` and `publish` jobs")),
            "{found:?}"
        );
    }

    /// A `cd.yml` that triggers on every branch push, not only `main`, mints
    /// the deployment token far wider than the manifest's one host was ever
    /// granted.
    #[test]
    fn a_cd_workflow_not_scoped_to_main_is_refused() {
        let contents = r"name: cd
on:
  push:
    branches: [main, staging]
  workflow_dispatch:
permissions:
  contents: read
  id-token: write
jobs:
  gate:
    uses: neon-law/navigator/.github/workflows/project-gate.yml@26.7.27
  publish:
    needs: gate
    uses: neon-law/navigator/.github/workflows/project-publish.yml@26.7.27
";
        let found = cd_findings(contents);
        assert!(
            found
                .iter()
                .any(|message| message.contains("branches: [main]")),
            "{found:?}"
        );
    }

    /// A `cd.yml` widened to `contents: write` is the privilege escalation
    /// the closed permissions set exists to catch.
    #[test]
    fn a_cd_workflow_with_contents_write_is_refused() {
        let contents = r"name: cd
on:
  push:
    branches: [main]
  workflow_dispatch:
permissions:
  contents: write
  id-token: write
jobs:
  gate:
    uses: neon-law/navigator/.github/workflows/project-gate.yml@26.7.27
  publish:
    needs: gate
    uses: neon-law/navigator/.github/workflows/project-publish.yml@26.7.27
";
        let found = cd_findings(contents);
        assert!(
            found
                .iter()
                .any(|message| message.contains("permissions: {contents: read, id-token: write}")),
            "{found:?}"
        );
    }

    /// ENG-675 reproduction 1: an extra file under `.github/` — this
    /// repository holds only CODEOWNERS and the two thin callers, so
    /// anything else is a path the closed set has to name and reject.
    #[test]
    fn an_extra_github_file_is_refused() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        std::fs::write(root.path().join(".github/extra.txt"), "scratch\n").unwrap();

        let found = layout_findings(root.path());
        assert!(
            found
                .iter()
                .any(|message| message.contains(".github/extra.txt")
                    && message.contains("closed `.github` file set")),
            "{found:?}"
        );
    }

    #[test]
    fn canonical_github_paths_are_accepted_regardless_of_separator() {
        let paths = [
            PathBuf::from(".github/workflows/ci.yml"),
            PathBuf::from_iter([".github", "workflows", "ci.yml"]),
        ];
        for relative in paths {
            let mut errors = Vec::new();
            validate_github_path(Path::new("ci.yml"), &relative, &mut errors);
            assert!(errors.is_empty(), "{relative:?}: {errors:?}");
        }

        let mut errors = Vec::new();
        validate_github_path(
            Path::new("other.yml"),
            &PathBuf::from_iter([".github", "other.yml"]),
            &mut errors,
        );
        assert_eq!(errors.len(), 1, "{errors:?}");
    }

    /// This binary is past [`FINAL_RETIRED_WORKFLOW_RELEASE`], so a retired
    /// `gate.yml` is an error, not a warning — the door `docs/gate.md`
    /// promised this release would close.
    #[test]
    fn a_retired_workflow_filename_is_refused() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        let ci_contents = std::fs::read_to_string(root.path().join(WORKFLOW)).unwrap();
        std::fs::remove_file(root.path().join(WORKFLOW)).unwrap();
        std::fs::write(root.path().join(RETIRED_WORKFLOW), &ci_contents).unwrap();

        let found = layout_findings(root.path());
        assert!(
            found
                .iter()
                .any(|message| message.contains(RETIRED_WORKFLOW)
                    && message.contains(WORKFLOW)
                    && message.contains(FINAL_RETIRED_WORKFLOW_RELEASE)
                    && message.contains("this release refuses it")),
            "{found:?}"
        );
        assert_eq!(layout_warnings(root.path()), Vec::<String>::new());
    }

    /// The bound `docs/gate.md` documents: once this binary's own release is
    /// after [`FINAL_RETIRED_WORKFLOW_RELEASE`], the retired filename is
    /// refused outright rather than merely warned about — ENG-815's first
    /// gap, that nothing ever implemented the "one further release" the
    /// warning text promised.
    #[test]
    fn a_retired_workflow_filename_is_refused_past_the_final_accepting_release() {
        assert!(!retired_workflow_refused(FINAL_RETIRED_WORKFLOW_RELEASE));
        assert!(!retired_workflow_refused("0.1.0"));
        assert!(retired_workflow_refused("26.9.24"));
        assert!(retired_workflow_refused("27.1.1"));
        // Fails open on a version string it cannot parse, rather than
        // refusing a repository's CI over a value this check cannot read.
        assert!(!retired_workflow_refused("not-a-version"));
    }

    /// ENG-815's second gap: `resolve_workflow` used to inspect the retired
    /// file only when the canonical one was missing, so a repository could
    /// carry a structurally validated `ci.yml` and an unvalidated `gate.yml`
    /// that GitHub still runs. Both paths must now be named in one finding.
    #[test]
    fn a_retired_workflow_beside_its_canonical_replacement_is_refused() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        let ci_contents = std::fs::read_to_string(root.path().join(WORKFLOW)).unwrap();
        std::fs::write(root.path().join(RETIRED_WORKFLOW), &ci_contents).unwrap();

        let found = layout_findings(root.path());
        assert!(
            found
                .iter()
                .any(|message| message.contains(RETIRED_WORKFLOW)
                    && message.contains(WORKFLOW)
                    && message.contains("alongside")),
            "{found:?}"
        );
        assert_eq!(layout_warnings(root.path()), Vec::<String>::new());
    }

    #[test]
    fn a_retired_cd_workflow_filename_is_refused() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        let cd_contents = std::fs::read_to_string(root.path().join(CD_WORKFLOW)).unwrap();
        std::fs::remove_file(root.path().join(CD_WORKFLOW)).unwrap();
        std::fs::write(root.path().join(RETIRED_CD_WORKFLOW), &cd_contents).unwrap();

        let found = layout_findings(root.path());
        assert!(
            found
                .iter()
                .any(|message| message.contains(RETIRED_CD_WORKFLOW)
                    && message.contains(CD_WORKFLOW)
                    && message.contains("this release refuses it")),
            "{found:?}"
        );
        assert_eq!(layout_warnings(root.path()), Vec::<String>::new());
    }

    /// A `.yaml` spelling of the CI gate reports the extension it actually
    /// found and the one Navigator reads, not a bare "missing" that sends the
    /// operator looking for a typo they already made correctly.
    #[test]
    fn a_yaml_extension_ci_workflow_reports_the_expected_extension() {
        let root = tempfile::tempdir().unwrap();
        scaffold_minimal(root.path());
        let ci_contents = std::fs::read_to_string(root.path().join(WORKFLOW)).unwrap();
        std::fs::remove_file(root.path().join(WORKFLOW)).unwrap();
        std::fs::write(root.path().join(".github/workflows/ci.yaml"), ci_contents).unwrap();

        let found = layout_findings(root.path());
        assert!(
            found
                .iter()
                .any(|message| message.contains(".github/workflows/ci.yaml")
                    && message.contains(WORKFLOW)),
            "{found:?}"
        );
    }
}

#[cfg(test)]
mod forgejo_gate_tests {
    use super::{
        emit_scaffold_for, forgejo_gate_workflow, repair_skills_for, validate_gate, workflow_for,
        DEFAULT_WORKFLOW_HOME, FORGEJO_GATE_WORKFLOW, FORGEJO_REQUIRED_CHECK, WORKFLOW,
    };
    use cloud::ForgeBackend;
    use std::fs;

    fn scaffold(forge: ForgeBackend) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        emit_scaffold_for(
            root.path(),
            "matter-one",
            "staging.neonlaw.com",
            "lawyer@example.com",
            "client@example.com",
            forge,
        )
        .unwrap();
        root
    }

    #[test]
    fn the_forgejo_gate_runs_navigator_project_gate_on_pull_requests_with_oidc() {
        let workflow = forgejo_gate_workflow();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&workflow).unwrap();
        assert_eq!(parsed["name"].as_str(), Some("gate"));
        assert_eq!(
            parsed["on"],
            serde_yaml::from_str::<serde_yaml::Value>("{pull_request: null}").unwrap()
        );
        let jobs = parsed["jobs"].as_mapping().unwrap();
        assert_eq!(jobs.len(), 1, "one job, one context: {workflow}");
        let gate = &parsed["jobs"]["gate"];
        assert!(gate["name"].is_null(), "the job is named by its key");
        assert_eq!(gate["enable-openid-connect"].as_bool(), Some(true));
        let steps = gate["steps"].as_sequence().unwrap();
        assert!(
            steps.iter().all(|step| step["uses"].is_null()),
            "plain shell only, no action repository to mirror"
        );
        let last = steps.last().unwrap();
        assert_eq!(last["run"].as_str(), Some("navigator project gate --ci"));
        let install = steps[1]["run"].as_str().unwrap();
        assert!(install.contains("navigator.yaml"), "{install}");
        assert!(
            install.contains("sha256"),
            "the release is verified: {install}"
        );
        assert!(workflow.contains(FORGEJO_REQUIRED_CHECK));
        // Rendered, not templated: no Rust format braces survive.
        assert!(!workflow.contains("{{{"), "{workflow}");
        assert!(workflow.contains("${{ forgejo.ref }}"), "{workflow}");
        assert!(
            workflow.contains("\"${SERVER_URL%/}/${REPOSITORY}.git\""),
            "{workflow}"
        );
    }

    #[test]
    fn a_forgejo_scaffold_carries_the_gate_and_passes_its_own_validation() {
        let root = scaffold(ForgeBackend::Forgejo);
        assert_eq!(
            fs::read_to_string(root.path().join(FORGEJO_GATE_WORKFLOW)).unwrap(),
            forgejo_gate_workflow()
        );
        // GitHub's thin caller stays byte-for-byte what it was.
        assert_eq!(
            fs::read_to_string(root.path().join(WORKFLOW)).unwrap(),
            workflow_for(DEFAULT_WORKFLOW_HOME, crate::cli_version())
        );
        assert_eq!(
            validate_gate(root.path(), Some("matter-one"), false).errors,
            0
        );
    }

    #[test]
    fn the_gate_closes_forgejo_to_the_generated_workflow() {
        let root = scaffold(ForgeBackend::Forgejo);
        let gate = root.path().join(FORGEJO_GATE_WORKFLOW);
        fs::write(&gate, "name: gate\non: [push]\n").unwrap();
        assert_eq!(
            validate_gate(root.path(), Some("matter-one"), false).errors,
            1
        );
        fs::write(&gate, forgejo_gate_workflow()).unwrap();
        fs::write(
            root.path().join(".forgejo/workflows/extra.yml"),
            "name: extra\n",
        )
        .unwrap();
        assert_eq!(
            validate_gate(root.path(), Some("matter-one"), false).errors,
            1
        );
    }

    #[test]
    fn a_github_scaffold_has_no_forgejo_workflow() {
        let root = scaffold(ForgeBackend::GitHub);
        assert!(!root.path().join(".forgejo").exists());
        assert!(root.path().join(WORKFLOW).is_file());
    }

    #[test]
    fn repair_restores_a_drifted_or_missing_forgejo_gate() {
        let root = scaffold(ForgeBackend::Forgejo);
        let path = root.path().join(FORGEJO_GATE_WORKFLOW);
        fs::write(&path, "name: gate\non: [push]\n").unwrap();
        // A checkout that already carries `.forgejo/` is Forgejo-backed even
        // when this shell does not say so.
        assert_eq!(
            repair_skills_for(root.path(), ForgeBackend::GitHub).unwrap(),
            vec![FORGEJO_GATE_WORKFLOW.to_string()]
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), forgejo_gate_workflow());
        assert!(repair_skills_for(root.path(), ForgeBackend::Forgejo)
            .unwrap()
            .is_empty());

        let github = scaffold(ForgeBackend::GitHub);
        assert!(repair_skills_for(github.path(), ForgeBackend::GitHub)
            .unwrap()
            .is_empty());
        assert!(!github.path().join(".forgejo").exists());
        assert_eq!(
            repair_skills_for(github.path(), ForgeBackend::Forgejo).unwrap(),
            vec![FORGEJO_GATE_WORKFLOW.to_string()]
        );
    }

    #[test]
    fn a_forgejo_scaffold_is_a_valid_headless_source_tree() {
        let root = scaffold(ForgeBackend::Forgejo);
        let tree = crate::projects::source_import::tree_from_checkout(root.path()).unwrap();
        assert!(tree.files.contains_key(FORGEJO_GATE_WORKFLOW));
    }
}

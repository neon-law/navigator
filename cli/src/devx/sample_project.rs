//! `navigator dev sample-project` — clone and stage each sample matter's
//! Project repository for a local boot.
//!
//! A sample repository is headless: its `navigator.yaml` declares the
//! matter's pages and its `drafts/` holds the matter's Drafts. Development
//! boot clones the repository recorded on each sample Project and stages
//! that source — the manifest, the Drafts, and the commit they were cloned
//! at — before writing the environment that `web` reads. The boot seed then
//! stores the manifest's `pages:`, persists each Draft pinned to that commit,
//! and writes the rows each declared section reads
//! (`store::seed`'s sample records).
//!
//! The URLs come from the Projects rather than constants here, so pointing a
//! matter at a different forge is a data change. `--repo` still overrides one
//! for a fork or a local mirror, and `--project` narrows the refresh to a
//! single matter.
//!
//! The clone happens in a **temporary directory** that is removed when the
//! command returns. What survives into `.devx/sample-projects/<code>/` is the
//! staged source. Boot re-reads the staged manifest rather than trusting the
//! directory it was found in, so a source staged under the wrong code is
//! refused rather than read onto another matter. `--keep` retains the temp
//! tree for debugging.
//!
//! Nothing here writes to a deployment: only the local boot path stages it.
//! Nothing here builds anything either: a Project repository commits only
//! source, so there is no Portal bundle to install or build.
//!
//! ## Testing
//!
//! Cloning shells out to the network, so [`run`]'s sequencing is not
//! unit-tested. Everything that *decides* something is extracted so it can be:
//! which matters to refresh ([`choose_matters`]), which repository to clone
//! ([`choose_repo`]), the git arguments, the staged layout ([`stage_source`]),
//! and the tree copy.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

/// Where the projects are staged, relative to the workspace root. Inside
/// `.devx/` because it is generated, per-checkout, and already ignored. Each
/// matter gets a subdirectory named for its Project code.
const STAGE_RELATIVE: [&str; 1] = ["sample-projects"];

/// Where an earlier stage left a built Portal bundle. Nothing builds one any
/// more, so a stage that finds it removes it rather than leave a stale bundle
/// beside the source.
const RETIRED_BUNDLE_DIR: &str = "dist";

/// Build the `git clone` arguments. Always a shallow, single-branch clone: the
/// history is not wanted, only the tree that builds.
///
/// A pinned `git_ref` still clones shallow — `--branch` accepts a tag or a
/// branch name — so the common case stays one round trip.
fn clone_args(repo: &str, git_ref: Option<&str>, dest: &Path) -> Vec<String> {
    let mut args = vec![
        "clone".to_string(),
        "--depth".to_string(),
        "1".to_string(),
        "--single-branch".to_string(),
    ];
    if let Some(reference) = git_ref {
        args.push("--branch".to_string());
        args.push(reference.to_string());
    }
    args.push(repo.to_string());
    args.push(dest.display().to_string());
    args
}

/// The directory every staged project sits under — the one path `.devx/env`
/// names, so `web` finds all of them from a single variable.
fn staged_root(workspace_root: &Path) -> PathBuf {
    let mut path = workspace_root.join(".devx");
    for segment in STAGE_RELATIVE {
        path.push(segment);
    }
    path
}

/// Where one matter is staged for the next `web` boot — the manifest and the
/// built bundle together, under the matter's own Project code.
fn staged_for(workspace_root: &Path, project_code: &str) -> PathBuf {
    staged_root(workspace_root).join(project_code)
}

/// Copy a directory tree, replacing `dst` wholesale.
///
/// Replacing rather than merging is deliberate: a merge would leave assets
/// from a previous build in the staged tree, and boot publishes everything it
/// finds, so stale files would be republished forever.
fn copy_tree(src: &Path, dst: &Path) -> Result<usize> {
    if dst.exists() {
        std::fs::remove_dir_all(dst)
            .with_context(|| format!("clearing the staged bundle at {}", dst.display()))?;
    }
    let mut copied = 0;
    copy_into(src, dst, &mut copied)?;
    Ok(copied)
}

fn copy_into(src: &Path, dst: &Path, copied: &mut usize) -> Result<()> {
    std::fs::create_dir_all(dst).with_context(|| format!("creating {}", dst.display()))?;
    for entry in std::fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let metadata = std::fs::metadata(&from)?;
        if metadata.is_dir() {
            copy_into(&from, &to, copied)?;
        } else if metadata.is_file() {
            std::fs::copy(&from, &to)
                .with_context(|| format!("copying {} to {}", from.display(), to.display()))?;
            *copied += 1;
        }
    }
    Ok(())
}

/// Run one command in `dir`, failing loudly. Output is inherited so a `pnpm`
/// build's own diagnostics reach the operator instead of being swallowed into
/// a captured buffer nobody prints.
fn run_in(dir: &Path, program: &str, args: &[String]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .current_dir(dir)
        .status()
        .with_context(|| format!("running `{program}` — is it installed and on PATH?"))?;
    if !status.success() {
        bail!(
            "`{program} {}` failed in {} ({status})",
            args.join(" "),
            dir.display()
        );
    }
    Ok(())
}

/// Whether the store was even consulted, so the repository choice can say
/// which of the two absences it is looking at.
enum Lookup {
    /// No row for this Project code at all.
    NoProject,
    /// The row exists and carries this `repository_url`.
    Project(Option<String>),
}

/// Which matters this invocation refreshes.
///
/// No `--project` refreshes all of them, which is what a boot wants. Naming
/// one narrows it to that matter, and a name that is not a sample matter is
/// refused with the list rather than silently refreshing nothing — a typo
/// there would otherwise look like a successful no-op.
fn choose_matters<'a>(project: Option<&str>, known: &[&'a str]) -> Result<Vec<&'a str>> {
    let Some(code) = project else {
        return Ok(known.to_vec());
    };
    match known.iter().find(|known| **known == code) {
        Some(found) => Ok(vec![*found]),
        None => bail!(
            "`{code}` is not a sample matter. Known matters: {}.",
            known.join(", ")
        ),
    }
}

/// Choose the repository to clone for one matter, given the flag and what the
/// store holds.
///
/// The pure half of [`resolve_repo`]: every branch a caller can land in is
/// decided here, so the IO wrapper stays a connect-and-read with no decisions
/// of its own.
///
/// `--repo` wins without a lookup. Otherwise the Project row wins, which is
/// what keeps pointing a matter at a different forge a data change. Only when
/// there is no row at all does the compiled-in URL for that matter stand in —
/// that is a store nobody has seeded yet, and the row the seed is about to
/// write carries exactly this URL. A row that exists but records no URL is
/// still an error, because something deliberately cleared it.
fn choose_repo(
    project_code: &str,
    explicit: Option<&str>,
    fallback: Option<&str>,
    lookup: impl FnOnce() -> Result<Lookup>,
) -> Result<String> {
    if let Some(repo) = explicit {
        return Ok(repo.to_string());
    }
    match lookup()? {
        Lookup::Project(Some(url)) => Ok(url),
        Lookup::Project(None) => bail!(
            "Project `{project_code}` records no repository URL. Set one on the matter, or \
             pass `--repo`."
        ),
        Lookup::NoProject => match fallback {
            Some(url) => Ok(url.to_string()),
            None => bail!(
                "no Project `{project_code}` in this store — start `web` once so the seed \
                 runs, or pass `--repo`"
            ),
        },
    }
}

/// The repository to clone for one matter: `--repo` when given, else the URL
/// recorded on the Project, else the compiled-in default for that matter.
///
/// Reading the Project is what keeps one source of truth. The decision lives in
/// [`choose_repo`]; this only supplies the store.
pub(super) fn resolve_repo(project_code: &str, explicit: Option<&str>) -> Result<String> {
    let fallback = store::seed::sample_matter_repository(project_code);
    choose_repo(project_code, explicit, fallback, || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("create tokio runtime")?;
        runtime.block_on(async {
            let surreal = store::surreal::connect_from_env().await.context(
                "connect to SurrealDB to read the Project's repository URL — source \
                 this worktree's `.devx/env` first, or pass `--repo`",
            )?;
            Ok(
                match store::projects::find_by_code(&surreal, project_code)
                    .await
                    .with_context(|| format!("look up Project `{project_code}`"))?
                {
                    None => Lookup::NoProject,
                    Some(project) => Lookup::Project(project.repository_url),
                },
            )
        })
    })
}

/// The manifest text and the Project code it declares.
///
/// Read *before* a build is spent on the checkout: a bundle declaring the wrong
/// Project is refused at boot anyway, so finding out here saves an install. The
/// text is returned alongside the code because it is staged verbatim next to the
/// bundle — boot re-reads it rather than trusting whoever staged it.
fn declared_project(checkout: &Path) -> Result<(String, String)> {
    let manifest_path = checkout.join(store::sample_project::MANIFEST_FILE);
    let manifest = std::fs::read_to_string(&manifest_path).with_context(|| {
        format!(
            "reading {} — a project application declares its Project there",
            manifest_path.display()
        )
    })?;
    let code = store::sample_project::project_code_from_manifest(&manifest)?;
    Ok((manifest, code))
}

/// The commit a checkout is at, or `None` when git cannot say.
fn head_commit(checkout: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(checkout)
        .output()
        .ok()?;
    let commit = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (output.status.success() && !commit.is_empty()).then_some(commit)
}

/// Refresh the sample matters' staged source for a local boot.
pub fn run(
    project: Option<&str>,
    repo: Option<&str>,
    git_ref: Option<&str>,
    keep: bool,
) -> Result<()> {
    super::require_tools(&["git"])?;
    let known = store::seed::sample_matter_codes();
    let matters = choose_matters(project, &known)?;
    let workspace_root = super::orchestrate::workspace_root()?;

    let mut copied = 0;
    for code in &matters {
        let url = resolve_repo(code, repo)?;
        copied += stage_one(code, &url, git_ref, keep, &workspace_root)?;
    }
    print!(
        "{}",
        staging_instructions(copied, matters.len(), &staged_root(&workspace_root))
    );
    Ok(())
}

/// Clone and stage every sample matter's source for `root`,
/// each from the repository the compiled-in fixture records for it.
///
/// The development orchestrator calls this before it renders `.devx/env`, so
/// the following `web` process always reads freshly staged bundles. It reads
/// the compiled-in URLs rather than the store because it runs before the first
/// boot has seeded anything.
///
/// **Best-effort, per matter.** One repository that will not clone or build
/// does not fail the boot: it is reported and skipped, and that matter keeps
/// whatever was staged for it before. Bringing up the whole
/// local tier is not the moment to discover that somebody else's sample
/// repository is having a bad day, and the two other matters have nothing to do
/// with it. `dev sample-project` is the strict path — an operator who asked for
/// a refresh wants the error, not a warning they have to go looking for.
pub(super) fn run_for_root(keep: bool, workspace_root: &Path) -> Result<usize> {
    super::require_tools(&["git"])?;
    let mut copied = 0;
    let mut skipped = Vec::new();
    for code in store::seed::sample_matter_codes() {
        let Some(url) = store::seed::sample_matter_repository(code) else {
            skipped.push(code);
            continue;
        };
        match stage_one(code, url, None, keep, workspace_root) {
            Ok(count) => copied += count,
            Err(error) => {
                eprintln!("navigator: could not stage `{code}` from {url}: {error:#}");
                skipped.push(code);
            }
        }
    }
    if !skipped.is_empty() {
        eprintln!(
            "navigator: {} matter(s) keep their previously staged source: {}. Run `navigator dev sample-project --project <code>` to see the failure \
             on its own.",
            skipped.len(),
            skipped.join(", ")
        );
    }
    Ok(copied)
}

/// A Project repository cloned into a temporary tree.
///
/// The checkout lives only as long as this value: dropping it removes the
/// tree, so a caller copies or uploads what it needs first and then lets it
/// go, or calls [`keep`](Self::keep) to leave the tree behind for debugging.
pub(super) struct ClonedProject {
    temp: tempfile::TempDir,
    /// The cloned repository root.
    pub(super) checkout: PathBuf,
    /// The manifest text, verbatim — staged so boot can re-read the declared
    /// Project rather than trust the directory name.
    pub(super) manifest: String,
    /// The Project code the manifest declares, already proven to be one.
    pub(super) code: String,
    /// The commit the checkout is at.
    pub(super) commit: Option<String>,
}

impl ClonedProject {
    /// Leak the temporary tree so it survives for inspection, printing where.
    pub(super) fn keep(self) {
        let path = self.temp.keep();
        println!("navigator: kept the build tree at {}", path.display());
    }
}

/// Clone `repo` at `git_ref` into a temporary tree and read the Project it
/// declares.
pub(super) fn clone_repository(repo: &str, git_ref: Option<&str>) -> Result<ClonedProject> {
    // The checkout lives in a temp tree; only the staged source survives.
    let temp = tempfile::Builder::new()
        .prefix("navigator-sample-project-")
        .tempdir()
        .context("creating a temporary build directory")?;
    let checkout = temp.path().join("checkout");

    println!("navigator: cloning {repo}");
    let args = clone_args(repo, git_ref, &checkout);
    run_in(temp.path(), "git", &args)?;

    let (manifest, code) = declared_project(&checkout)?;
    println!(
        "navigator: {} declares Project `{code}`",
        repo_basename(repo)
    );
    let commit = head_commit(&checkout);
    Ok(ClonedProject {
        temp,
        checkout,
        manifest,
        code,
        commit,
    })
}

/// The refusal a bundle earns when it declares a different matter than the one
/// it is being staged for.
///
/// One literal with an explicit line continuation, so the operator reads a
/// single space where the sentence wraps, and named so a test can assert on the
/// rendered string without running a clone and a build.
fn project_mismatch_message(repo: &str, declared: &str, expected: &str) -> String {
    format!(
        "{repo} declares Project `{declared}`, but it is being staged for `{expected}`. \
         One matter's application must not mount on another's portal."
    )
}

/// Clone and stage one matter's source, returning how many files
/// landed in its staging directory.
fn stage_one(
    project_code: &str,
    repo: &str,
    git_ref: Option<&str>,
    keep: bool,
    workspace_root: &Path,
) -> Result<usize> {
    let built = clone_repository(repo, git_ref)?;

    // Refuse a bundle that declares a different matter, here rather than at
    // boot. The staging directory is named for the matter it belongs to, so
    // staging one matter's bundle under another's code would be the one
    // mistake that puts a client's application on another client's portal.
    // Boot checks this again against the manifest it re-reads; this is the
    // earlier, clearer failure.
    if built.code != project_code {
        bail!(
            "{}",
            project_mismatch_message(repo, &built.code, project_code)
        );
    }

    let stage = staged_for(workspace_root, project_code);
    let copied = stage_source(&built, &stage)?;

    if keep {
        built.keep();
    }

    Ok(copied)
}

/// Stage one cloned repository at `stage`, returning how many files landed.
///
/// The manifest, the repository's `drafts/`, and the commit. A part the
/// repository no longer carries is removed from the stage, so a Draft deleted
/// upstream is not read back on the next boot, and a bundle an earlier stage
/// built is cleared.
fn stage_source(built: &ClonedProject, stage: &Path) -> Result<usize> {
    std::fs::create_dir_all(stage).with_context(|| format!("creating {}", stage.display()))?;
    std::fs::write(
        stage.join(store::sample_project::MANIFEST_FILE),
        &built.manifest,
    )
    .with_context(|| format!("staging the manifest in {}", stage.display()))?;
    let mut copied = 1;

    let commit_file = stage.join(store::sample_project::SOURCE_COMMIT_FILE);
    match &built.commit {
        Some(commit) => {
            std::fs::write(&commit_file, format!("{commit}\n"))
                .with_context(|| format!("staging the commit in {}", stage.display()))?;
            copied += 1;
        }
        None => remove_if_present(&commit_file)?,
    }

    let drafts = built.checkout.join(store::sample_project::DRAFTS_DIR);
    let staged_drafts = stage.join(store::sample_project::DRAFTS_DIR);
    if drafts.is_dir() {
        copied += copy_tree(&drafts, &staged_drafts)?;
    } else {
        remove_if_present(&staged_drafts)?;
    }

    remove_if_present(&stage.join(RETIRED_BUNDLE_DIR))?;
    Ok(copied)
}

/// Remove a staged file or directory if it is there.
fn remove_if_present(path: &Path) -> Result<()> {
    let removed = if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else if path.exists() {
        std::fs::remove_file(path)
    } else {
        return Ok(());
    };
    removed.with_context(|| format!("removing the stale {}", path.display()))
}

/// What to tell the operator once the bundles are staged.
///
/// Built as a string so the refresh output is covered by a focused test.
fn staging_instructions(copied: usize, matters: usize, stage: &Path) -> String {
    format!(
        "\nnavigator: staged {copied} file(s) across {matters} matter(s) under {}\n\n\
         The next `web` boot reads them from the generated `.devx/env`.\n",
        stage.display(),
    )
}

/// The repository's last path segment, for a readable progress line.
pub(super) fn repo_basename(repo: &str) -> &str {
    repo.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(repo)
        .trim_end_matches(".git")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--repo` wins outright, and does not consult the store at all.
    ///
    /// The lookup panics if called: a flag that still needed a database would
    /// make the command unusable on a cold checkout, which is the one case the
    /// flag exists for.
    #[test]
    fn an_explicit_repo_wins_without_reading_the_store() {
        let chosen = choose_repo(
            "sample-litigation",
            Some("https://example.test/a-fork/x.git"),
            Some("https://example.test/compiled-in.git"),
            || panic!("the store must not be consulted when --repo is given"),
        )
        .expect("a repo");
        assert_eq!(chosen, "https://example.test/a-fork/x.git");
    }

    /// With no flag, the Project's own recorded URL is what gets cloned —
    /// whatever forge it names, and in preference to the compiled-in default.
    /// That preference is what keeps repointing a matter a data change.
    #[test]
    fn the_projects_recorded_url_beats_the_compiled_in_default() {
        let chosen = choose_repo(
            "sample-litigation",
            None,
            Some("https://github.test/neon/compiled-in.git"),
            || {
                Ok(Lookup::Project(Some(
                    "https://gitlab.example/a-group/a-project.git".to_string(),
                )))
            },
        )
        .expect("a repo");
        assert_eq!(chosen, "https://gitlab.example/a-group/a-project.git");
    }

    /// A store nobody has seeded yet falls back to the matter's compiled-in
    /// repository, because that is the URL the seed is about to write. This is
    /// the first-boot path: the orchestrator stages the bundles *before*
    /// anything has created a Project row to read.
    #[test]
    fn an_unseeded_store_falls_back_to_the_matters_own_repository() {
        let chosen = choose_repo(
            "sample-transactional",
            None,
            Some("https://github.test/neon/navigator-sample-project-transactional"),
            || Ok(Lookup::NoProject),
        )
        .expect("a repo");
        assert_eq!(
            chosen,
            "https://github.test/neon/navigator-sample-project-transactional"
        );
    }

    /// The two absences are different problems, so they get different
    /// messages — and a row that exists with an empty URL is still an error.
    /// Something deliberately cleared it, so falling back would overwrite that
    /// decision with a default the operator did not ask for.
    #[test]
    fn each_absence_names_its_own_fix() {
        let no_project = choose_repo("sample-litigation", None, None, || Ok(Lookup::NoProject))
            .expect_err("a store with no such Project and no default is an error");
        let message = no_project.to_string();
        assert!(
            message.contains("start `web` once") && message.contains("--repo"),
            "the no-Project error must name both fixes: {message}"
        );

        let no_url = choose_repo(
            "sample-litigation",
            None,
            Some("https://github.test/neon/compiled-in.git"),
            || Ok(Lookup::Project(None)),
        )
        .expect_err("a Project with no repository URL is an error");
        let message = no_url.to_string();
        assert!(
            message.contains("records no repository URL"),
            "the no-URL error must say the column is empty: {message}"
        );
        assert!(
            !message.contains("compiled-in"),
            "a cleared URL must not be silently refilled from the default: {message}"
        );
    }

    #[test]
    fn project_mismatch_message_has_no_reflow_gap() {
        let message = project_mismatch_message("sample-repo", "declared", "expected");

        assert!(
            !message.contains("  "),
            "the refusal must not contain a run of spaces: {message}"
        );
    }

    /// A failed lookup propagates rather than being read as "no URL recorded".
    ///
    /// Otherwise an unreachable database would produce the *set one on the
    /// matter* advice, sending the reader to fix a row that is probably fine —
    /// or worse, silently build the compiled-in default.
    #[test]
    fn a_failed_lookup_propagates_instead_of_becoming_an_absence() {
        let error = choose_repo("sample-litigation", None, Some("https://x/y.git"), || {
            anyhow::bail!("connection refused")
        })
        .expect_err("a lookup failure is an error");
        assert!(
            error.to_string().contains("connection refused"),
            "the underlying failure must survive: {error}"
        );
    }

    /// No `--project` refreshes everything; naming one narrows to it; a name
    /// that is not a matter is refused with the list rather than refreshing
    /// nothing and reporting success.
    #[test]
    fn choosing_matters_defaults_to_all_and_refuses_an_unknown_name() {
        let known = ["sample-litigation", "sample-transactional", "sample-estate"];

        assert_eq!(choose_matters(None, &known).expect("all"), known.to_vec());
        assert_eq!(
            choose_matters(Some("sample-transactional"), &known).expect("one"),
            vec!["sample-transactional"]
        );

        let error = choose_matters(Some("no-such-matter"), &known)
            .expect_err("a name that is not a matter must fail");
        let message = error.to_string();
        assert!(
            message.contains("not a sample matter") && message.contains("sample-litigation"),
            "the error must list the real matters: {message}"
        );
    }

    /// Every matter the seed carries resolves a compiled-in repository, so the
    /// orchestrator's first-boot staging can never be handed a `None`.
    #[test]
    fn every_sample_matter_records_a_repository() {
        let codes = store::seed::sample_matter_codes();
        assert!(!codes.is_empty(), "the fixture carries at least one matter");
        for code in codes {
            let url = store::seed::sample_matter_repository(code)
                .unwrap_or_else(|| panic!("`{code}` records a repository"));
            assert!(url.starts_with("https://"), "`{code}` -> {url}");
        }
        assert_eq!(store::seed::sample_matter_repository("nope"), None);
    }

    /// A missing or unusable manifest is refused before a build is spent, and
    /// the text is returned verbatim for staging.
    #[test]
    fn the_declared_project_is_read_before_a_build_is_spent() {
        let dir = tempfile::tempdir().expect("tempdir");

        let missing =
            declared_project(dir.path()).expect_err("a checkout with no manifest is refused");
        assert!(
            missing
                .to_string()
                .contains(store::sample_project::MANIFEST_FILE),
            "the refusal must name the file a bundle declares its Project in: {missing}"
        );

        // Present but naming something that is not a Project code.
        std::fs::write(
            dir.path().join(store::sample_project::MANIFEST_FILE),
            b"project: \"../etc\"\n",
        )
        .expect("write");
        declared_project(dir.path()).expect_err("a manifest cannot smuggle a path segment");

        std::fs::write(
            dir.path().join(store::sample_project::MANIFEST_FILE),
            b"project: sample-litigation\n",
        )
        .expect("write");
        let (manifest, code) = declared_project(dir.path()).expect("a valid manifest");
        assert_eq!(code, "sample-litigation");
        assert_eq!(
            manifest, "project: sample-litigation\n",
            "the text is staged verbatim, so it must come back unaltered"
        );
    }

    /// A headless repository stages its manifest, its Drafts, and its commit,
    /// and clears a bundle and Drafts left by an earlier stage, so the next
    /// boot reads exactly what the repository carries now.
    #[test]
    fn a_headless_repository_stages_its_source_and_clears_a_stale_bundle() {
        let temp = tempfile::tempdir().expect("tempdir");
        let checkout = temp.path().join("checkout");
        std::fs::create_dir_all(checkout.join("drafts")).expect("mkdir");
        std::fs::write(
            checkout.join("drafts/engagement.md"),
            b"---\ntitle: T\n---\n",
        )
        .expect("draft");
        let stage = temp.path().join("stage");
        std::fs::create_dir_all(stage.join("dist/assets")).expect("mkdir");
        std::fs::write(stage.join("dist/index.html"), b"old").expect("stale bundle");
        std::fs::create_dir_all(stage.join("drafts")).expect("mkdir");
        std::fs::write(stage.join("drafts/removed.md"), b"old").expect("stale draft");

        let built = ClonedProject {
            temp: tempfile::tempdir().expect("tempdir"),
            checkout,
            manifest: "project:\n  name: sample-estate\n".to_string(),
            code: "sample-estate".to_string(),
            commit: Some("abc123".to_string()),
        };
        let copied = stage_source(&built, &stage).expect("stage");

        assert_eq!(copied, 3, "manifest, commit, and one Draft");
        assert_eq!(
            std::fs::read_to_string(stage.join(store::sample_project::MANIFEST_FILE))
                .expect("manifest"),
            built.manifest
        );
        assert_eq!(
            std::fs::read_to_string(stage.join(store::sample_project::SOURCE_COMMIT_FILE))
                .expect("commit"),
            "abc123\n"
        );
        assert!(stage.join("drafts/engagement.md").is_file());
        assert!(
            !stage.join("drafts/removed.md").exists(),
            "a removed Draft is not read back"
        );
        assert!(
            !stage.join("dist").exists(),
            "a stale Portal bundle is cleared"
        );
    }

    /// The refresh output names the staged path and the generated environment.
    #[test]
    fn the_instructions_name_the_key_boot_reads_and_the_staged_path() {
        let text = staging_instructions(5, 3, Path::new("/w/.devx/sample-projects"));
        assert!(
            text.contains("The next `web` boot reads them from the generated `.devx/env`."),
            "{text}"
        );
        assert!(
            text.contains("staged 5 file(s) across 3 matter(s)"),
            "{text}"
        );
        assert!(text.contains("/w/.devx/sample-projects"), "{text}");
    }

    /// `run_in` reports a failing command and a missing one differently.
    ///
    /// These are the two failures an operator hits — a `pnpm build` that exits
    /// nonzero, and a `pnpm` that is not installed — and they need different
    /// fixes, so the missing-program case carries the "is it installed" hint
    /// rather than an exit status.
    #[test]
    fn a_failing_command_and_a_missing_one_are_reported_differently() {
        let dir = tempfile::tempdir().expect("tempdir");

        let succeeding = ("true", Vec::<String>::new());
        run_in(dir.path(), succeeding.0, &succeeding.1).expect("a succeeding command is Ok");

        let failing = ("false", Vec::<String>::new());
        let failed =
            run_in(dir.path(), failing.0, &failing.1).expect_err("a nonzero exit must fail");
        assert!(
            failed.to_string().contains("failed in"),
            "a nonzero exit must name where it ran: {failed}"
        );

        let missing = run_in(dir.path(), "navigator-no-such-program", &[])
            .expect_err("a missing program must fail");
        assert!(
            missing.to_string().contains("is it installed and on PATH?"),
            "a missing program must say so rather than report an exit code: {missing}"
        );
    }

    #[test]
    fn clone_is_shallow_and_single_branch() {
        let args = clone_args("https://example.com/r.git", None, Path::new("/tmp/x"));
        assert_eq!(
            args,
            vec![
                "clone",
                "--depth",
                "1",
                "--single-branch",
                "https://example.com/r.git",
                "/tmp/x"
            ]
        );
    }

    #[test]
    fn a_pinned_ref_becomes_branch_and_stays_shallow() {
        let args = clone_args("r.git", Some("v1.2.3"), Path::new("/tmp/x"));
        assert!(args.contains(&"--branch".to_string()));
        assert!(args.contains(&"v1.2.3".to_string()));
        assert_eq!(
            args.iter().filter(|a| *a == "--depth").count(),
            1,
            "pinning a ref must not cost a full history"
        );
    }

    /// One staging root, one subdirectory per matter. `web` reads the root
    /// from a single generated variable and finds every bundle under it.
    #[test]
    fn staged_paths_live_under_devx_one_directory_per_matter() {
        assert_eq!(
            staged_root(Path::new("/w")),
            PathBuf::from("/w/.devx/sample-projects")
        );
        assert_eq!(
            staged_for(Path::new("/w"), "sample-estate"),
            PathBuf::from("/w/.devx/sample-projects/sample-estate")
        );
    }

    #[test]
    fn repo_basename_reads_through_the_git_suffix_and_trailing_slash() {
        assert_eq!(
            repo_basename("https://github.com/o/navigator-sample-project-estate.git"),
            "navigator-sample-project-estate"
        );
        // SCP-style remotes split on the same `/` as a URL path.
        assert_eq!(repo_basename("git@github.com:o/r.git"), "r");
        assert_eq!(repo_basename("https://x/y/"), "y");
    }

    #[test]
    fn copy_tree_replaces_rather_than_merges() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        std::fs::create_dir_all(src.join("assets")).expect("mkdir");
        std::fs::write(src.join("index.html"), b"new").expect("write");
        std::fs::write(src.join("assets/app-new.js"), b"new").expect("write");

        // A previous build left an asset that the new one does not have.
        std::fs::create_dir_all(dst.join("assets")).expect("mkdir");
        std::fs::write(dst.join("assets/app-old.js"), b"old").expect("write");

        let copied = copy_tree(&src, &dst).expect("copy");

        assert_eq!(copied, 2);
        assert!(dst.join("assets/app-new.js").is_file());
        assert!(
            !dst.join("assets/app-old.js").exists(),
            "a stale asset would be republished on every boot"
        );
    }

    #[test]
    fn copy_tree_preserves_nested_structure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        std::fs::create_dir_all(src.join("assets/fonts")).expect("mkdir");
        std::fs::write(src.join("index.html"), b"x").expect("write");
        std::fs::write(src.join("assets/fonts/gorp.woff2"), b"x").expect("write");

        copy_tree(&src, &dst).expect("copy");

        assert!(dst.join("assets/fonts/gorp.woff2").is_file());
    }
}

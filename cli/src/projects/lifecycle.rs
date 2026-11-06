//! Project onboarding and offboarding as repository-root workflows.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};

use crate::{projects::manifest, remote};

#[path = "lifecycle_letter.rs"]
mod letter;

/// The lifecycle kinds live in `project-scaffold` beside the Draft stubs
/// they name, so the CLI's lifecycle commands and the shared scaffold
/// cannot disagree on what an unedited stub is.
pub(crate) use project_scaffold::{OFFBOARDING, ONBOARDING};

/// Open the onboarding notation, deriving the matter and client from
/// `navigator.yaml` in `root`.
pub async fn onboard(host: Option<&str>, root: &Path, answers_from: Option<&Path>) -> ExitCode {
    run(root, host, ONBOARDING, answers_from).await
}

/// Open the offboarding notation, or close and archive a matter whose signed
/// offboarding pointer is already filed.
pub async fn offboard(
    host: Option<&str>,
    root: &Path,
    effective_at: Option<DateTime<Utc>>,
) -> ExitCode {
    run_offboard(root, host, effective_at).await
}

async fn run(root: &Path, host: Option<&str>, kind: &str, answers_from: Option<&Path>) -> ExitCode {
    match run_open(root, host, kind, answers_from).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("navigator project {kind}: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run_open(
    root: &Path,
    host: Option<&str>,
    kind: &str,
    answers_from: Option<&Path>,
) -> Result<()> {
    let project = project_coordinates(root)?;
    let template = template_path(root, kind)?;
    let code = template
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| anyhow!("the selected {kind} Draft has no UTF-8 filename stem"))?;
    if !template.is_file() {
        write_template_stub(&template, kind)?;
        println!(
            "wrote {} — draft the letter before opening it",
            template.display()
        );
        return Ok(());
    }
    if std::fs::read_to_string(&template).is_ok_and(|body| is_unedited_stub(&body)) {
        println!(
            "{} is still the unedited stub — draft the letter before opening it",
            template.display()
        );
        return Ok(());
    }
    require_template_matches_main(root, kind)?;

    if has_signed_pointer(root, kind)? {
        println!("{kind} letter is already signed and filed; Project is onboarded/offboarded");
        return Ok(());
    }

    if let Some(row) = remote::notation_rows(host, &project.code)
        .await?
        .into_iter()
        .find(|row| row.template_code == code && row.notation_id.is_some())
    {
        println!(
            "{kind} notation {} is already open in state {}",
            row.notation_id.as_deref().unwrap_or_default(),
            row.current_state.as_deref().unwrap_or("unknown")
        );
        return Ok(());
    }

    let created = remote::notation_create(
        host,
        code,
        &project.client_dri,
        &project.code,
        if kind == ONBOARDING {
            answers_from
        } else {
            None
        },
    )
    .await;
    if created != ExitCode::SUCCESS {
        return Err(anyhow!("could not open the {kind} notation"));
    }
    Ok(())
}

async fn run_offboard(
    root: &Path,
    host: Option<&str>,
    effective_at: Option<DateTime<Utc>>,
) -> ExitCode {
    let result = async {
        let project = project_coordinates(root)?;
        let template = template_path(root, OFFBOARDING)?;
        let code = template
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| anyhow!("the selected offboarding Draft has no UTF-8 filename stem"))?;
        if !template.is_file() {
            write_template_stub(&template, OFFBOARDING)?;
            println!(
                "wrote {} — draft the letter before opening it",
                template.display()
            );
            return Ok(());
        }
        require_template_matches_main(root, OFFBOARDING)?;

        if !has_signed_pointer(root, OFFBOARDING)? {
            if let Some(row) = remote::notation_rows(host, &project.code)
                .await?
                .into_iter()
                .find(|row| row.template_code == code && row.notation_id.is_some())
            {
                println!(
                    "offboarding notation {} is already open in state {}",
                    row.notation_id.as_deref().unwrap_or_default(),
                    row.current_state.as_deref().unwrap_or("unknown")
                );
                return Ok(());
            }
            if remote::notation_create(host, code, &project.client_dri, &project.code, None).await
                != ExitCode::SUCCESS
            {
                return Err(anyhow!("could not open the offboarding notation"));
            }
            return Ok(());
        }

        ensure_archive_ready(root)?;
        let closed = remote::matter_close(
            host,
            &project.code,
            store::projects::ClosureReason::EngagementCompleted,
            effective_at,
        )
        .await;
        if closed != ExitCode::SUCCESS {
            return Err(anyhow!("could not close the Project"));
        }
        if remote::archive_repository(host, &project.code, root).await != ExitCode::SUCCESS {
            return Err(anyhow!("could not file the repository archive"));
        }
        archive_forge_repository(root).await?;
        println!("offboarded {}", project.code);
        Ok(())
    }
    .await;

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("navigator project offboard: {error:#}");
            ExitCode::FAILURE
        }
    }
}

struct ProjectCoordinates {
    code: String,
    client_dri: String,
}

fn project_coordinates(root: &Path) -> Result<ProjectCoordinates> {
    let manifest = manifest::read(root)
        .ok_or_else(|| anyhow!("no valid navigator.yaml in {}", root.display()))?;
    Ok(ProjectCoordinates {
        code: manifest
            .project
            .ok_or_else(|| anyhow!("navigator.yaml has no Project code"))?,
        client_dri: manifest
            .client_dri
            .ok_or_else(|| anyhow!("navigator.yaml has no project.client_dri"))?,
    })
}

fn template_relative(root: &Path, kind: &str) -> Result<String> {
    if kind == ONBOARDING {
        if let Some(relative) = letter::declared(root)? {
            let path = letter::checked_path(root, &relative)?;
            let contents =
                std::fs::read_to_string(path).context("read the declared engagement Draft")?;
            if is_legacy_letter(&contents) {
                return Err(anyhow!("the declared engagement Draft needs `navigator project migrate-letter` before onboarding"));
            }
            let declared_kind = rules::frontmatter::extract(&contents)
                .and_then(|frontmatter| rules::frontmatter::field(frontmatter, "kind"));
            if declared_kind.as_deref() != Some(ONBOARDING) {
                return Err(anyhow!("the declared engagement Draft must have kind: onboarding; only onboarding-derived agreements can be migrated"));
            }
            return Ok(relative);
        }
    }
    let directory = super::repository::draft_directory(root);
    let folder = root.join(directory);
    let mut authored = Vec::new();
    if folder.is_dir() {
        for entry in
            std::fs::read_dir(&folder).with_context(|| format!("read {}", folder.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() || path.extension().is_none_or(|extension| extension != "md") {
                continue;
            }
            let body = std::fs::read_to_string(&path)
                .with_context(|| format!("read {}", path.display()))?;
            if is_unedited_stub(&body) {
                continue;
            }
            let declared_kind = rules::frontmatter::extract(&body)
                .and_then(|frontmatter| rules::frontmatter::field(frontmatter, "kind"));
            if declared_kind.as_deref() == Some(kind) {
                let filename = entry.file_name();
                let filename = filename
                    .to_str()
                    .ok_or_else(|| anyhow!("a {kind} Draft has a non-UTF-8 filename"))?;
                authored.push(format!("{directory}/{filename}"));
            }
        }
    }
    authored.sort();
    match authored.as_slice() {
        [] if kind == ONBOARDING && !migration_candidates(root, directory)?.is_empty() => Err(anyhow!(
            "an engagement agreement needs `navigator project migrate-letter` before onboarding"
        )),
        [] => Ok(format!("{directory}/{}.md", template_code(kind))),
        [relative] => Ok(relative.clone()),
        _ => Err(anyhow!(
            "multiple {kind} Drafts in {directory}: {}; keep one authored {kind} Draft before opening its notation",
            authored.join(", ")
        )),
    }
}

fn template_path(root: &Path, kind: &str) -> Result<PathBuf> {
    Ok(root.join(template_relative(root, kind)?))
}

/// Only tests compare a checkout's letter Draft against the unedited stub
/// the shared scaffold wrote; production commands write through
/// [`write_template_stub`] and never re-derive the path.
#[cfg(test)]
pub(crate) use project_scaffold::scaffold_draft;
/// The repository-relative path and stub bytes of `kind`'s letter Draft, as
/// `repository scaffold` writes it into a new Project repository. The
/// canonical bytes live in `project-scaffold` so the CLI's scaffold and the
/// server-side repository seeding write the same placeholder.
pub(crate) use project_scaffold::{template_code, template_placeholder, template_stub};

fn write_template_stub(path: &Path, kind: &str) -> Result<()> {
    std::fs::create_dir_all(path.parent().unwrap_or(Path::new("."))).with_context(|| {
        format!(
            "create {}",
            path.parent().unwrap_or(Path::new(".")).display()
        )
    })?;
    std::fs::write(path, template_stub(kind)).with_context(|| format!("write {}", path.display()))
}

/// Whether `contents` is a lifecycle letter stub nobody has drafted yet.
/// Such a file is a placeholder, never an attorney's edit.
pub(crate) fn is_unedited_stub(contents: &str) -> bool {
    let body = rules::frontmatter::split(contents).map(|(_, body)| body.trim());
    [ONBOARDING, OFFBOARDING].iter().any(|kind| {
        contents == template_stub(kind)
            || body.is_some_and(|body| body == template_placeholder(kind))
    })
}
/// The `kind:` value a legacy engagement letter declares: a Project-local
/// agreement draft, not a shared-template kind. A letter is *legacy* when it
/// declares this kind together with a [`LINEAGE_FIELD`] naming the shared
/// template it was authored from — together they mark the draft that the
/// migration verb moves onto the canonical onboarding path.
const LEGACY_ONBOARDING_KIND: &str = "agreement";
const LINEAGE_FIELD: &str = "derived_from";

/// Move a legacy engagement letter onto the canonical onboarding path so
/// `navigator project onboard` opens the notation it was drafted for.
///
/// A legacy letter is a non-stub `drafts/` document declaring
/// `kind: {LEGACY_ONBOARDING_KIND}` and a [`LINEAGE_FIELD`] pointing at the
/// onboarding shared template it descended from. Exactly one such letter is expected:
/// the verb rewrites its `kind:` to `onboarding`, moves the file to
/// `{draft_directory}/onboarding__letter.md`, and leaves the `derived_from`
/// lineage and the rest of the document untouched. An unedited stub already
/// sitting at the canonical path is replaced; an authored canonical draft
/// refuses the migration so a lawyer's writing is never overwritten.
///
/// The migration retypes the Draft and rewrites its page references together;
/// `Kind::opens_a_matter` and the rule engine are untouched, and
/// `derived_from` stays documentation-only.
pub(crate) fn migrate_letter(root: &Path) -> Result<()> {
    let directory = super::repository::draft_directory(root);
    let legacy = if let Some(relative) = letter::declared(root)? {
        let path = letter::checked_path(root, &relative)?;
        let contents =
            std::fs::read_to_string(&path).context("read the declared engagement Draft")?;
        if !is_legacy_letter(&contents) {
            return Err(anyhow!(
                "the declared engagement Draft is not an onboarding-derived agreement"
            ));
        }
        vec![path]
    } else {
        migration_candidates(root, directory)?
    };
    match legacy.as_slice() {
        [path] => migrate_letter_file(root, directory, path),
        [] => Err(anyhow!(
            "no legacy letter to migrate: a legacy letter declares `kind: \
             {LEGACY_ONBOARDING_KIND}` and a `{LINEAGE_FIELD}: onboarding__*` lineage in `{directory}/`; \
             nothing there declares both"
        )),
        [first, rest @ ..] => Err(anyhow!(
            "multiple legacy letters to migrate: {} and {}; retire or \
             migrate all but one before running this again",
            first.display(),
            rest.iter()
                .map(|path| format!("`{}`", path.display()))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn migration_candidates(root: &Path, directory: &str) -> Result<Vec<PathBuf>> {
    let drafts = root.join(directory);
    if !drafts.exists() {
        return Ok(Vec::new());
    }
    let mut legacy = Vec::new();
    for entry in std::fs::read_dir(&drafts).context("read the Project Draft directory")? {
        let path = entry?.path();
        if path.extension().is_none_or(|extension| extension != "md") {
            continue;
        }
        let relative = path
            .strip_prefix(root)?
            .to_str()
            .ok_or_else(|| anyhow!("Draft path is not UTF-8"))?;
        letter::checked_path(root, relative)?;
        let contents = std::fs::read_to_string(&path).context("read the engagement Draft")?;
        if !is_unedited_stub(&contents) && is_legacy_letter(&contents) {
            legacy.push(path);
        }
    }
    legacy.sort();
    Ok(legacy)
}

/// Whether the document is a legacy letter: an agreement-kind draft that
/// records an `onboarding__*` shared-template lineage.
fn is_legacy_letter(contents: &str) -> bool {
    let frontmatter = rules::frontmatter::extract(contents);
    frontmatter
        .and_then(|frontmatter| rules::frontmatter::field(frontmatter, "kind"))
        .is_some_and(|kind| kind == LEGACY_ONBOARDING_KIND)
        && frontmatter.is_some_and(|frontmatter| {
            rules::frontmatter::field(frontmatter, LINEAGE_FIELD)
                .is_some_and(|lineage| lineage.starts_with("onboarding__"))
        })
}

/// Move one legacy letter to the canonical onboarding path with its `kind:`
/// retyped, replacing an unedited stub that occupies the destination.
fn migrate_letter_file(root: &Path, directory: &str, source: &Path) -> Result<()> {
    let contents =
        std::fs::read_to_string(source).with_context(|| format!("read {}", source.display()))?;
    let retyped = retyped_kind(&contents, ONBOARDING).ok_or_else(|| {
        anyhow!(
            "{} declares no `kind:` frontmatter field to retype",
            source.display()
        )
    })?;
    let target = root
        .join(directory)
        .join(format!("{}.md", template_code(ONBOARDING)));
    if target
        .symlink_metadata()
        .is_ok_and(|metadata| !metadata.is_file())
    {
        return Err(anyhow!(
            "the canonical letter destination must be a regular file"
        ));
    }
    if source != target && target.is_file() {
        let existing = std::fs::read_to_string(&target)
            .with_context(|| format!("read {}", target.display()))?;
        if !is_unedited_stub(&existing) {
            return Err(anyhow!(
                "refusing to migrate {} onto the already authored {} — retire one of the \
                 two letters first, then run this again",
                source.display(),
                target.display()
            ));
        }
    }
    letter::migrate(root, source, &target, &contents, &retyped)?;
    println!("migrated {} -> {}", source.display(), target.display());
    println!("next: review `git status`, commit the move, then run `navigator project onboard`");
    Ok(())
}

/// The document with its frontmatter `kind:` line rewritten to `new_kind`,
/// or `None` when no `kind:` line is declared. Only the frontmatter block is
/// touched; the body is returned verbatim.
fn retyped_kind(contents: &str, new_kind: &str) -> Option<String> {
    let (frontmatter, body) = rules::frontmatter::split(contents)?;
    let newline = if frontmatter.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut lines: Vec<String> = frontmatter.lines().map(ToString::to_string).collect();
    let mut retyped = false;
    for line in &mut lines {
        if !retyped && line.starts_with("kind:") {
            *line = format!("kind: {new_kind}");
            retyped = true;
        }
    }
    if !retyped {
        return None;
    }
    Some(format!(
        "---{newline}{}{newline}---{newline}{body}",
        lines.join(newline)
    ))
}

fn require_template_matches_main(root: &Path, kind: &str) -> Result<()> {
    git(root, &["fetch", "origin", "main"])
        .context("fetch origin/main before opening the notation")?;
    let relative = template_relative(root, kind)?;
    let exists = git(
        root,
        &["cat-file", "-e", &format!("origin/main:{relative}")],
    )?;
    if !exists.status.success() {
        return Err(anyhow!(
            "origin/main has no {relative}; publish the template before opening its notation"
        ));
    }
    let diff = git(root, &["diff", "--quiet", "origin/main", "--", &relative])?;
    if !diff.status.success() {
        return Err(anyhow!(
            "local {relative} differs from origin/main; publish the template before opening its notation"
        ));
    }
    Ok(())
}

fn has_signed_pointer(root: &Path, kind: &str) -> Result<bool> {
    let folder = root
        .join(crate::document_sync::POINTER_DIRECTORY)
        .join(kind);
    if !folder.is_dir() {
        return Ok(false);
    }
    for entry in std::fs::read_dir(&folder).with_context(|| format!("read {}", folder.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path
            .extension()
            .is_none_or(|extension| extension != "yaml" && extension != "yml")
        {
            continue;
        }
        let pointer = store::document_pointers::DocumentPointer::from_yaml(
            &std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?,
        )?;
        let source_name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default();
        if source_name.ends_with("_signed.pdf") && pointer.docusign_envelope_id.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn ensure_archive_ready(root: &Path) -> Result<()> {
    let status = git(root, &["status", "--porcelain"])?;
    if !status.status.success() {
        return Err(anyhow!("could not read git status in {}", root.display()));
    }
    if !String::from_utf8_lossy(&status.stdout).trim().is_empty() {
        return Err(anyhow!("refusing to archive a dirty Project repository"));
    }
    git(root, &["fetch", "origin", "main"]).context("fetch origin/main before archiving")?;
    let head = git_text(root, &["rev-parse", "HEAD"])?;
    let main = git_text(root, &["rev-parse", "origin/main"])?;
    if head != main {
        return Err(anyhow!(
            "refusing to archive: HEAD does not equal origin/main"
        ));
    }
    Ok(())
}

pub(crate) fn git_text(root: &Path, args: &[&str]) -> Result<String> {
    let output = git(root, args)?;
    if !output.status.success() {
        return Err(anyhow!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub(crate) fn git(root: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .with_context(|| format!("run git {} in {}", args.join(" "), root.display()))
}

async fn archive_forge_repository(root: &Path) -> Result<()> {
    let remote = git_text(root, &["remote", "get-url", "origin"])?;
    let (owner, name) = github_owner_name(&remote)?;
    let token = github_token()?;
    let response = reqwest::Client::new()
        .patch(format!("https://api.github.com/repos/{owner}/{name}"))
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "navigator-cli")
        .json(&serde_json::json!({ "archived": true }))
        .send()
        .await
        .context("archive the GitHub Project repository")?;
    if !response.status().is_success() {
        return Err(anyhow!(
            "GitHub repository archive failed: {}",
            response.status()
        ));
    }
    Ok(())
}

pub(crate) fn github_token() -> Result<String> {
    for key in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(value) = std::env::var(key) {
            if !value.trim().is_empty() {
                return Ok(value);
            }
        }
    }
    let output = Command::new("gh")
        .args(["auth", "token"])
        .output()
        .context("read GitHub credentials with `gh auth token`")?;
    if !output.status.success() {
        return Err(anyhow!(
            "GitHub authentication is unavailable; run `gh auth login`"
        ));
    }
    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if token.is_empty() {
        return Err(anyhow!("`gh auth token` returned no GitHub token"));
    }
    Ok(token)
}

pub(crate) fn github_owner_name(remote: &str) -> Result<(String, String)> {
    let url = remote.trim().trim_end_matches(".git");
    let path = if let Some(rest) = url.strip_prefix("git@github.com:") {
        rest
    } else if let Some(rest) = url.strip_prefix("https://github.com/") {
        rest
    } else if let Some(rest) = url.strip_prefix("ssh://git@github.com/") {
        rest
    } else {
        return Err(anyhow!("origin must be a GitHub repository URL"));
    };
    let (owner, name) = path
        .split_once('/')
        .ok_or_else(|| anyhow!("origin is not an owner/repository GitHub URL"))?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return Err(anyhow!("origin is not an owner/repository GitHub URL"));
    }
    Ok((owner.to_string(), name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::{
        ensure_archive_ready, github_owner_name, has_signed_pointer, project_coordinates,
        require_template_matches_main, run_offboard, run_open, scaffold_draft, template_code,
        template_path, template_relative, write_template_stub, OFFBOARDING, ONBOARDING,
    };
    use std::path::{Path, PathBuf};
    use std::process::{Command, ExitCode};

    fn git(root: &Path, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap()
    }

    fn git_ok(root: &Path, args: &[&str]) {
        let output = git(root, args);
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write_manifest(root: &Path) {
        std::fs::write(
            root.join("navigator.yaml"),
            "project:\n  host: navigator.example.test\n  name: acme\n  client_dri: client@example.com\n",
        )
        .unwrap();
    }

    fn init_project_repository(root: &Path, kind: &str) {
        git_ok(root, &["init", "--quiet", "--initial-branch=main"]);
        git_ok(root, &["config", "user.name", "Test Contributor"]);
        git_ok(root, &["config", "user.email", "contributor@example.com"]);
        git_ok(root, &["config", "commit.gpgsign", "false"]);
        write_manifest(root);
        let template = template_path(root, kind).unwrap();
        std::fs::create_dir_all(template.parent().unwrap()).unwrap();
        std::fs::write(template, "---\nkind: letter\ntitle: Letter\n---\n").unwrap();
        git_ok(root, &["add", "navigator.yaml", "drafts"]);
        git_ok(root, &["commit", "--quiet", "-m", "Initial Project"]);
        git_ok(
            root,
            &[
                "init",
                "--quiet",
                "--bare",
                "--initial-branch=main",
                ".origin.git",
            ],
        );
        std::fs::write(root.join(".git/info/exclude"), ".origin.git/\n").unwrap();
        git_ok(root, &["remote", "add", "origin", ".origin.git"]);
        git_ok(
            root,
            &["push", "--quiet", "--set-upstream", "origin", "main"],
        );
        git_ok(root, &["fetch", "--quiet", "origin", "main"]);
    }

    #[test]
    fn github_origin_supports_ssh_and_https_urls() {
        assert_eq!(
            github_owner_name("git@github.com:firm/matter.git").unwrap(),
            ("firm".to_string(), "matter".to_string())
        );
        assert_eq!(
            github_owner_name("https://github.com/firm/matter").unwrap(),
            ("firm".to_string(), "matter".to_string())
        );
        assert_eq!(
            github_owner_name("ssh://git@github.com/firm/matter.git").unwrap(),
            ("firm".to_string(), "matter".to_string())
        );
        assert!(github_owner_name("https://gitlab.com/firm/matter").is_err());
        assert!(github_owner_name("git@github.com:firm").is_err());
        assert!(github_owner_name("https://github.com//matter").is_err());
        assert!(github_owner_name("https://github.com/firm/nested/matter").is_err());
    }

    #[test]
    fn a_missing_template_gets_a_draft_stub() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("drafts/offboarding__letter.md");
        write_template_stub(&path, "offboarding").unwrap();
        let body = std::fs::read_to_string(path).unwrap();
        assert!(body.contains("kind: offboarding"));
        assert!(body.contains("Draft this letter"));
        assert!(body.contains("navigator project offboard"));
    }

    #[test]
    fn template_codes_map_lifecycle_kinds_to_published_letter_names() {
        assert_eq!(template_code(ONBOARDING), "onboarding__letter");
        assert_eq!(template_code(OFFBOARDING), "offboarding__letter");
        assert_eq!(template_code("engagement__letter"), "engagement__letter");
        assert_eq!(
            template_path(Path::new("project"), ONBOARDING).unwrap(),
            PathBuf::from("project/drafts/onboarding__letter.md")
        );
    }

    #[test]
    fn an_existing_kind_named_lifecycle_draft_is_selected() {
        for directory in ["drafts", "templates"] {
            for kind in [ONBOARDING, OFFBOARDING] {
                let root = tempfile::tempdir().unwrap();
                std::fs::create_dir(root.path().join(directory)).unwrap();
                std::fs::write(
                    root.path().join(format!("{directory}/{kind}.md")),
                    format!("---\nkind: {kind}\n---\nAuthored draft.\n"),
                )
                .unwrap();
                assert_eq!(
                    template_relative(root.path(), kind).unwrap(),
                    format!("{directory}/{kind}.md")
                );
            }
        }
    }

    #[test]
    fn an_authored_kind_named_draft_takes_precedence_over_an_unedited_stub() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("drafts")).unwrap();
        std::fs::write(
            root.path().join("drafts/onboarding.md"),
            "---\nkind: onboarding\n---\nAuthored draft.\n",
        )
        .unwrap();
        std::fs::write(
            root.path().join("drafts/onboarding__letter.md"),
            super::template_stub(ONBOARDING),
        )
        .unwrap();
        assert_eq!(
            template_relative(root.path(), ONBOARDING).unwrap(),
            "drafts/onboarding.md"
        );
        std::fs::write(
            root.path().join("drafts/onboarding__letter.md"),
            "---\nkind: onboarding\n---\nAuthored canonical draft.\n",
        )
        .unwrap();
        assert!(template_relative(root.path(), ONBOARDING)
            .unwrap_err()
            .to_string()
            .contains("multiple onboarding Drafts"));
    }

    #[test]
    fn a_body_only_placeholder_does_not_hide_an_authored_lifecycle_letter() {
        for (kind, command) in [(ONBOARDING, "onboard"), (OFFBOARDING, "offboard")] {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir(root.path().join("drafts")).unwrap();
            let placeholder = format!("---\nkind: {kind}\ntitle: Undrafted letter\nrespondent_type: person_and_entity\nconfidential: true\n---\n\nDraft this letter before running `navigator project {command}` again.\n");
            std::fs::write(
                root.path()
                    .join(format!("drafts/{}.md", template_code(kind))),
                placeholder,
            )
            .unwrap();
            std::fs::write(
                root.path().join(format!("drafts/{kind}.md")),
                format!("---\nkind: {kind}\n---\nAuthored letter.\n"),
            )
            .unwrap();
            assert_eq!(
                template_relative(root.path(), kind).unwrap(),
                format!("drafts/{kind}.md")
            );
        }
    }

    #[tokio::test]
    async fn multiple_authored_onboarding_drafts_are_refused_before_remote_requests() {
        let root = tempfile::tempdir().unwrap();
        write_manifest(root.path());
        std::fs::create_dir(root.path().join("drafts")).unwrap();
        for name in ["onboarding.md", "onboarding__letter.md"] {
            std::fs::write(
                root.path().join("drafts").join(name),
                "---\nkind: onboarding\n---\nAuthored letter.\n",
            )
            .unwrap();
        }
        let error = run_open(root.path(), None, ONBOARDING, None)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("multiple onboarding Drafts"),
            "ambiguous Drafts must be refused before Git or HTTP: {error:#}"
        );
    }

    #[test]
    fn project_coordinates_requires_a_parseable_manifest_with_project_and_client() {
        let root = tempfile::tempdir().unwrap();
        assert!(project_coordinates(root.path()).is_err());

        std::fs::write(root.path().join("navigator.yaml"), "project: []\n").unwrap();
        assert!(project_coordinates(root.path()).is_err());

        std::fs::write(
            root.path().join("navigator.yaml"),
            "project:\n  host: navigator.example.test\n  name: acme\n",
        )
        .unwrap();
        let error = project_coordinates(root.path()).err().unwrap();
        assert!(error.to_string().contains("client_dri"));

        write_manifest(root.path());
        let project = project_coordinates(root.path()).unwrap();
        assert_eq!(project.code, "acme");
        assert_eq!(project.client_dri, "client@example.com");
    }

    #[test]
    fn template_stubs_create_the_requested_onboarding_and_offboarding_drafts() {
        let root = tempfile::tempdir().unwrap();
        for (kind, expected_title, expected_command) in [
            (ONBOARDING, "Onboarding letter", "onboard"),
            (OFFBOARDING, "Offboarding letter", "offboard"),
        ] {
            let path = template_path(root.path(), kind).unwrap();
            write_template_stub(&path, kind).unwrap();
            let body = std::fs::read_to_string(path).unwrap();
            assert!(body.contains(&format!("title: {expected_title}")));
            assert!(body.contains(&format!("navigator project {expected_command}")));
        }
    }

    #[test]
    fn the_onboarding_stub_starts_compliant_with_the_engagement_arbitration_clause() {
        use rules::Rule;
        let (_, stub) = scaffold_draft(ONBOARDING);
        let violations = rules::clauses::ClauseReferences::default().lint(&rules::SourceFile {
            path: "drafts/onboarding__letter.md".into(),
            contents: stub.clone(),
        });
        assert!(violations.is_empty(), "{violations:?}");
        assert!(stub.contains("jurisdiction__governing_law"));
        assert!(stub.contains("jurisdiction__arbitration_seat"));
        assert!(rules::clauses::compile(&stub).is_ok());
    }

    #[test]
    fn published_templates_must_exist_on_main_and_match_the_checkout() {
        let root = tempfile::tempdir().unwrap();
        init_project_repository(root.path(), ONBOARDING);
        require_template_matches_main(root.path(), ONBOARDING).unwrap();

        let missing = require_template_matches_main(root.path(), "unpublished").unwrap_err();
        assert!(missing.to_string().contains("origin/main has no"));

        std::fs::write(
            template_path(root.path(), ONBOARDING).unwrap(),
            "---\nkind: letter\ntitle: Local edit\n---\n",
        )
        .unwrap();
        let changed = require_template_matches_main(root.path(), ONBOARDING).unwrap_err();
        assert!(changed.to_string().contains("differs from origin/main"));
    }

    #[test]
    fn archive_requires_a_clean_checkout_at_origin_main() {
        let root = tempfile::tempdir().unwrap();
        init_project_repository(root.path(), OFFBOARDING);
        ensure_archive_ready(root.path()).unwrap();

        std::fs::write(root.path().join("untracked.txt"), "pending work").unwrap();
        let dirty = ensure_archive_ready(root.path()).unwrap_err();
        assert!(dirty.to_string().contains("dirty Project repository"));
        std::fs::remove_file(root.path().join("untracked.txt")).unwrap();

        std::fs::write(root.path().join("new.txt"), "ahead of main").unwrap();
        git_ok(root.path(), &["add", "new.txt"]);
        git_ok(root.path(), &["commit", "--quiet", "-m", "Local change"]);
        let ahead = ensure_archive_ready(root.path()).unwrap_err();
        assert!(ahead
            .to_string()
            .contains("HEAD does not equal origin/main"));
    }

    fn legacy_letter() -> String {
        "---
kind: agreement
title: Engagement letter
respondent_type: person_and_entity
confidential: true
derived_from: onboarding__letter
questionnaire:
  BEGIN:
    _: jurisdiction__governing_law
  jurisdiction__governing_law:
    _: jurisdiction__arbitration_seat
  jurisdiction__arbitration_seat:
    _: END
clauses:
  arbitration:
    source: engagement_arbitration@1
    inputs:
      law: jurisdiction__governing_law
      seat: jurisdiction__arbitration_seat
workflow:
  BEGIN:
    created: lawyer_review
  lawyer_review:
    approved: END
---

{{> arbitration}}

The Firm will represent the client.
"
        .to_string()
    }

    #[test]
    fn a_legacy_agreement_letter_migrates_to_the_canonical_onboarding_path() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("drafts")).unwrap();
        std::fs::write(
            root.path().join("drafts/engagement__letter.md"),
            legacy_letter(),
        )
        .unwrap();
        super::migrate_letter(root.path()).unwrap();
        assert!(!root.path().join("drafts/engagement__letter.md").exists());
        assert_eq!(
            template_relative(root.path(), ONBOARDING).unwrap(),
            "drafts/onboarding__letter.md"
        );
        let migrated =
            std::fs::read_to_string(root.path().join("drafts/onboarding__letter.md")).unwrap();
        assert!(migrated.contains("kind: onboarding"), "{migrated}");
        assert!(migrated.contains("derived_from: onboarding__letter"));
        assert!(migrated.contains("The Firm will represent the client."));
    }

    fn declared_letter_manifest(path: &str) -> String {
        format!(
            "project:\n  host: navigator.example.test\n  name: sample-litigation\n  client_dri: client@example.test\npages:\n  - slug: engagement\n    kind: engagement_billing_records\n    title: Engagement\n    draft: '{path}'\n"
        )
    }

    #[test]
    fn onboarding_selects_the_declared_engagement_draft_before_other_authored_letters() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("templates")).unwrap();
        for name in ["chosen.md", "other.md"] {
            std::fs::write(
                root.path().join("templates").join(name),
                "---\nkind: onboarding\n---\nAuthored.\n",
            )
            .unwrap();
        }
        std::fs::write(
            root.path().join("navigator.yaml"),
            declared_letter_manifest("templates/chosen.md"),
        )
        .unwrap();
        assert_eq!(
            template_relative(root.path(), ONBOARDING).unwrap(),
            "templates/chosen.md"
        );
    }

    #[tokio::test]
    async fn a_declared_agreement_requires_migration_without_scaffolding_another_letter() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("templates")).unwrap();
        std::fs::write(root.path().join("templates/engagement.md"), legacy_letter()).unwrap();
        std::fs::write(
            root.path().join("navigator.yaml"),
            declared_letter_manifest("templates/engagement.md"),
        )
        .unwrap();
        let error = run_open(root.path(), None, ONBOARDING, None)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("project migrate-letter"),
            "{error:#}"
        );
        assert!(!root.path().join("templates/onboarding__letter.md").exists());
        assert_eq!(
            std::fs::read_to_string(root.path().join("templates/engagement.md")).unwrap(),
            legacy_letter()
        );
    }

    #[test]
    fn migration_rewrites_every_page_reference_and_preserves_other_manifest_bytes() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("templates")).unwrap();
        std::fs::write(root.path().join("templates/engagement.md"), legacy_letter()).unwrap();
        let original = format!("{}  - {{slug: evidence, kind: document_workbench, title: Evidence, draft: \"templates/engagement.md\"}}\nallowed_hosts:\n  example.test: approved\n", declared_letter_manifest("templates/engagement.md"));
        std::fs::write(root.path().join("navigator.yaml"), &original).unwrap();
        super::migrate_letter(root.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join("navigator.yaml")).unwrap(),
            original.replace("templates/engagement.md", "templates/onboarding__letter.md")
        );
        assert_eq!(
            template_relative(root.path(), ONBOARDING).unwrap(),
            "templates/onboarding__letter.md"
        );
        assert!(!root.path().join("templates/engagement.md").exists());
        let migrated =
            std::fs::read_to_string(root.path().join("templates/onboarding__letter.md")).unwrap();
        assert_eq!(
            rules::frontmatter::split(&migrated).unwrap().1,
            rules::frontmatter::split(&legacy_letter()).unwrap().1
        );
        assert!(migrated.contains("derived_from: onboarding__letter"));
    }

    #[test]
    fn a_malformed_manifest_refuses_migration_before_any_letter_changes() {
        for manifest in [
            "pages: [\n",
            "42\n",
            "pages: wrong\n",
            "pages: [42]\n",
            "pages: [{draft: 42}]\n",
            "pages: [{slug: engagement, kind: unknown, title: Engagement, draft: drafts/engagement.md}]\n",
        ] {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir(root.path().join("drafts")).unwrap();
            std::fs::write(root.path().join("drafts/engagement.md"), legacy_letter()).unwrap();
            std::fs::write(root.path().join("navigator.yaml"), manifest).unwrap();
            assert!(super::migrate_letter(root.path()).is_err());
            assert!(!root.path().join("drafts/onboarding__letter.md").exists());
            assert_eq!(
                std::fs::read_to_string(root.path().join("drafts/engagement.md")).unwrap(),
                legacy_letter()
            );
            assert_eq!(
                std::fs::read_to_string(root.path().join("navigator.yaml")).unwrap(),
                manifest
            );
        }
    }

    #[test]
    fn unrelated_agreement_lineage_never_becomes_an_engagement() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("drafts")).unwrap();
        let original = legacy_letter().replace(
            "derived_from: onboarding__letter",
            "derived_from: unrelated__agreement",
        );
        std::fs::write(root.path().join("drafts/agreement.md"), &original).unwrap();
        let error = super::migrate_letter(root.path()).unwrap_err();
        assert!(error.to_string().contains("onboarding__"), "{error:#}");
        assert!(!root.path().join("drafts/onboarding__letter.md").exists());
        assert_eq!(
            std::fs::read_to_string(root.path().join("drafts/agreement.md")).unwrap(),
            original
        );
    }

    #[test]
    fn unsafe_or_missing_declared_drafts_refuse_before_writing_anything() {
        for draft in ["../outside.md", "/outside.md", "templates/missing.md"] {
            let root = tempfile::tempdir().unwrap();
            let original = declared_letter_manifest(draft);
            std::fs::write(root.path().join("navigator.yaml"), &original).unwrap();
            assert!(
                template_relative(root.path(), ONBOARDING).is_err(),
                "{draft}"
            );
            assert!(super::migrate_letter(root.path()).is_err(), "{draft}");
            assert!(!root.path().join("drafts").exists());
            assert_eq!(
                std::fs::read_to_string(root.path().join("navigator.yaml")).unwrap(),
                original
            );
        }
    }

    #[test]
    fn a_declared_symlink_cannot_read_or_migrate_an_outside_letter() {
        for directory_link in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let original = legacy_letter();
            std::fs::write(outside.path().join("letter.md"), &original).unwrap();
            if directory_link {
                std::os::unix::fs::symlink(outside.path(), root.path().join("templates")).unwrap();
            } else {
                std::fs::create_dir(root.path().join("templates")).unwrap();
                std::os::unix::fs::symlink(
                    outside.path().join("letter.md"),
                    root.path().join("templates/letter.md"),
                )
                .unwrap();
            }
            std::fs::write(
                root.path().join("navigator.yaml"),
                declared_letter_manifest("templates/letter.md"),
            )
            .unwrap();
            assert!(template_relative(root.path(), ONBOARDING).is_err());
            assert!(super::migrate_letter(root.path()).is_err());
            assert_eq!(
                std::fs::read_to_string(outside.path().join("letter.md")).unwrap(),
                original
            );
            assert!(!root.path().join("templates/onboarding__letter.md").exists());
        }
    }

    #[test]
    fn conflicting_declared_engagement_letters_require_a_choice_before_changes() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("drafts")).unwrap();
        for name in ["one.md", "two.md"] {
            std::fs::write(root.path().join("drafts").join(name), legacy_letter()).unwrap();
        }
        let original = format!("{}  - slug: alternative\n    kind: engagement_billing_records\n    title: Alternative\n    draft: drafts/two.md\n", declared_letter_manifest("drafts/one.md"));
        std::fs::write(root.path().join("navigator.yaml"), &original).unwrap();
        assert!(template_relative(root.path(), ONBOARDING)
            .unwrap_err()
            .to_string()
            .contains("multiple engagement Drafts"));
        assert!(super::migrate_letter(root.path()).is_err());
        assert!(!root.path().join("drafts/onboarding__letter.md").exists());
        assert_eq!(
            std::fs::read_to_string(root.path().join("navigator.yaml")).unwrap(),
            original
        );
    }

    #[test]
    fn migration_refuses_a_symlink_at_the_canonical_destination() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("drafts")).unwrap();
        std::fs::write(root.path().join("drafts/old.md"), legacy_letter()).unwrap();
        std::fs::write(outside.path().join("letter.md"), "Outside letter.").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("letter.md"),
            root.path().join("drafts/onboarding__letter.md"),
        )
        .unwrap();
        assert!(super::migrate_letter(root.path()).is_err());
        assert_eq!(
            std::fs::read_to_string(outside.path().join("letter.md")).unwrap(),
            "Outside letter."
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("drafts/old.md")).unwrap(),
            legacy_letter()
        );
    }

    #[test]
    fn retyping_a_letter_preserves_nested_kind_fields() {
        let original = "---\nmetadata:\n  kind: letter\nkind: agreement\nderived_from: onboarding__letter\n---\nBody.\n";
        assert_eq!(
            super::retyped_kind(original, ONBOARDING).unwrap(),
            original.replace("\nkind: agreement", "\nkind: onboarding")
        );
    }

    #[test]
    fn a_migrated_letter_passes_the_gate_rules_and_questionnaire_paths() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("drafts")).unwrap();
        std::fs::write(
            root.path().join("drafts/engagement__letter.md"),
            legacy_letter(),
        )
        .unwrap();
        super::migrate_letter(root.path()).unwrap();
        let contents =
            std::fs::read_to_string(root.path().join("drafts/onboarding__letter.md")).unwrap();
        let rule_set =
            rules::navigator_default_rules_with_codes(&rules::canonical_question_codes());
        let source = rules::SourceFile {
            path: "onboarding__letter.md".into(),
            contents,
        };
        let errors: Vec<_> = rule_set
            .iter()
            .flat_map(|rule| rule.lint(&source))
            .filter(|violation| rules::severity_for_code(violation.code) == rules::Severity::Error)
            .collect();
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn migration_replaces_an_unedited_canonical_stub() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("drafts")).unwrap();
        std::fs::write(
            root.path().join("drafts/engagement__letter.md"),
            legacy_letter(),
        )
        .unwrap();
        write_template_stub(
            &root.path().join("drafts/onboarding__letter.md"),
            ONBOARDING,
        )
        .unwrap();
        super::migrate_letter(root.path()).unwrap();
        assert!(!root.path().join("drafts/engagement__letter.md").exists());
        let migrated =
            std::fs::read_to_string(root.path().join("drafts/onboarding__letter.md")).unwrap();
        assert!(migrated.contains("kind: onboarding"));
    }

    #[test]
    fn migration_refuses_to_overwrite_an_authored_canonical_draft() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("drafts")).unwrap();
        std::fs::write(
            root.path().join("drafts/engagement__letter.md"),
            legacy_letter(),
        )
        .unwrap();
        let canonical = root.path().join("drafts/onboarding__letter.md");
        std::fs::write(
            &canonical,
            "---\nkind: onboarding\n---\nAuthored canonical draft.\n",
        )
        .unwrap();
        let error = super::migrate_letter(root.path()).unwrap_err();
        assert!(error.to_string().contains("already authored"), "{error:#}");
        assert_eq!(
            std::fs::read_to_string(&canonical).unwrap(),
            "---\nkind: onboarding\n---\nAuthored canonical draft.\n"
        );
        assert!(root.path().join("drafts/engagement__letter.md").exists());
    }

    #[test]
    fn a_drafted_agreement_without_derived_from_is_not_a_legacy_letter() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("drafts")).unwrap();
        std::fs::write(
            root.path().join("drafts/engagement__letter.md"),
            "---\nkind: agreement\ntitle: Agreement\n---\nAuthored agreement.\n",
        )
        .unwrap();
        let error = super::migrate_letter(root.path()).unwrap_err();
        assert!(error.to_string().contains("no legacy letter"), "{error:#}");
        assert_eq!(
            std::fs::read_to_string(root.path().join("drafts/engagement__letter.md")).unwrap(),
            "---\nkind: agreement\ntitle: Agreement\n---\nAuthored agreement.\n"
        );
    }

    #[tokio::test]
    async fn onboarding_writes_a_stub_before_making_any_remote_request() {
        let root = tempfile::tempdir().unwrap();
        write_manifest(root.path());
        run_open(root.path(), None, ONBOARDING, None).await.unwrap();
        let stub =
            std::fs::read_to_string(template_path(root.path(), ONBOARDING).unwrap()).unwrap();
        assert!(stub.contains("Draft this letter"));
    }

    #[tokio::test]
    async fn signed_onboarding_is_complete_once_the_published_template_matches() {
        let root = tempfile::tempdir().unwrap();
        init_project_repository(root.path(), ONBOARDING);
        let folder = root.path().join("blobs/onboarding");
        std::fs::create_dir_all(&folder).unwrap();
        let pointer = format!(
            "kind: onboarding\nvisibility: internal\ndocusign_envelope_id: 0199b9e4-14b7-7ad0-87a5-71ef24a46d41\ncurrent_version:\n  version: 1\n  created_at: '2026-01-01T00:00:00Z'\n  sha256: {}\n  size_bytes: 1\n  asset_id: 00000000-0000-0000-0000-000000000001\n",
            "a".repeat(64)
        );
        std::fs::write(folder.join("retainer_signed.pdf.yaml"), pointer).unwrap();
        run_open(root.path(), None, ONBOARDING, None).await.unwrap();
    }

    #[tokio::test]
    async fn offboarding_writes_a_stub_or_refuses_to_archive_a_dirty_repository() {
        let root = tempfile::tempdir().unwrap();
        write_manifest(root.path());
        assert_eq!(
            run_offboard(root.path(), None, None).await,
            ExitCode::SUCCESS
        );
        assert!(template_path(root.path(), OFFBOARDING).unwrap().is_file());

        let root = tempfile::tempdir().unwrap();
        init_project_repository(root.path(), OFFBOARDING);
        let folder = root.path().join("blobs/offboarding");
        std::fs::create_dir_all(&folder).unwrap();
        let pointer = format!(
            "kind: offboarding\nvisibility: internal\ndocusign_envelope_id: 0199b9e4-14b7-7ad0-87a5-71ef24a46d41\ncurrent_version:\n  version: 1\n  created_at: '2026-01-01T00:00:00Z'\n  sha256: {}\n  size_bytes: 1\n  asset_id: 00000000-0000-0000-0000-000000000001\n",
            "a".repeat(64)
        );
        std::fs::write(folder.join("letter_signed.pdf.yaml"), pointer).unwrap();
        assert_eq!(
            run_offboard(root.path(), None, None).await,
            ExitCode::FAILURE
        );
    }

    #[test]
    fn signed_pointer_requires_both_signature_name_and_envelope_id() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("blobs/onboarding");
        std::fs::create_dir_all(&folder).unwrap();
        let pointer = |envelope: &str| {
            format!(
                "kind: onboarding\nvisibility: internal\ndocusign_envelope_id: {envelope}\ncurrent_version:\n  version: 1\n  created_at: '2026-01-01T00:00:00Z'\n  sha256: {}\n  size_bytes: 1\n  asset_id: 00000000-0000-0000-0000-000000000001\n",
                "a".repeat(64)
            )
        };
        std::fs::write(
            folder.join("retainer_signed.pdf.yaml"),
            pointer("0199b9e4-14b7-7ad0-87a5-71ef24a46d41"),
        )
        .unwrap();
        assert!(has_signed_pointer(root.path(), "onboarding").unwrap());

        std::fs::write(folder.join("retainer.pdf.yaml"), pointer("null")).unwrap();
        assert!(has_signed_pointer(root.path(), "onboarding").unwrap());

        std::fs::remove_file(folder.join("retainer_signed.pdf.yaml")).unwrap();
        assert!(!has_signed_pointer(root.path(), "onboarding").unwrap());
    }

    #[test]
    fn signed_pointer_ignores_other_extensions_and_malformed_yaml_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        assert!(!has_signed_pointer(root.path(), ONBOARDING).unwrap());
        let folder = root.path().join("blobs/onboarding");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("retainer_signed.pdf.txt"), "not a pointer").unwrap();
        assert!(!has_signed_pointer(root.path(), ONBOARDING).unwrap());
        std::fs::write(folder.join("retainer_signed.pdf.yml"), "not: [valid").unwrap();
        assert!(has_signed_pointer(root.path(), ONBOARDING).is_err());
    }

    #[test]
    fn a_templates_alias_checkout_keeps_its_spelling() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("templates")).unwrap();
        assert_eq!(
            template_path(root.path(), ONBOARDING).unwrap(),
            root.path().join("templates/onboarding__letter.md")
        );
        assert_eq!(
            template_relative(root.path(), OFFBOARDING).unwrap(),
            "templates/offboarding__letter.md"
        );

        std::fs::create_dir(root.path().join("drafts")).unwrap();
        assert!(
            template_relative(root.path(), ONBOARDING)
                .unwrap_err()
                .to_string()
                .contains("both drafts/ and templates/"),
            "ambiguous Draft directories must refuse before scaffolding"
        );
    }

    #[test]
    fn published_drafts_are_staged_and_compared_under_drafts() {
        let root = tempfile::tempdir().unwrap();
        init_project_repository(root.path(), ONBOARDING);
        let tracked = git(root.path(), &["ls-files"]);
        assert!(String::from_utf8_lossy(&tracked.stdout).contains("drafts/onboarding__letter.md"));
        require_template_matches_main(root.path(), ONBOARDING).unwrap();
    }

    #[tokio::test]
    async fn an_unedited_stub_is_never_opened() {
        let root = tempfile::tempdir().unwrap();
        write_manifest(root.path());
        let (relative, stub) = scaffold_draft(ONBOARDING);
        assert_eq!(relative, "drafts/onboarding__letter.md");
        std::fs::create_dir_all(root.path().join("drafts")).unwrap();
        std::fs::write(root.path().join(&relative), &stub).unwrap();
        // No git checkout and no host: reaching either would fail, so success
        // proves the stub stopped the command first.
        run_open(root.path(), None, ONBOARDING, None).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join(relative)).unwrap(),
            stub
        );
    }

    #[test]
    fn template_path_is_under_project_root() {
        let root = Path::new("matter");
        assert_eq!(
            super::template_path(root, "onboarding").unwrap(),
            root.join("drafts/onboarding__letter.md")
        );
    }
}

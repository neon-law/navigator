//! Project onboarding and offboarding as repository-root workflows.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};

use crate::{projects::manifest, remote};

/// The lifecycle kinds live in `project-scaffold` beside the Draft stubs
/// they name, so the CLI's lifecycle commands and the shared scaffold
/// cannot disagree on what an unedited stub is.
pub(crate) use project_scaffold::{OFFBOARDING, ONBOARDING};

/// Open the onboarding notation, deriving the matter and client from
/// `navigator.yaml` in `root`.
pub async fn onboard(host: Option<&str>, root: &Path) -> ExitCode {
    run(root, host, ONBOARDING).await
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

async fn run(root: &Path, host: Option<&str>, kind: &str) -> ExitCode {
    match run_open(root, host, kind).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("navigator project {kind}: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run_open(root: &Path, host: Option<&str>, kind: &str) -> Result<()> {
    let project = project_coordinates(root)?;
    let template = template_path(root, kind);
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
        .find(|row| row.template_code == template_code(kind) && row.notation_id.is_some())
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
        template_code(kind),
        &project.client_dri,
        &project.code,
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
        let template = template_path(root, OFFBOARDING);
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
                .find(|row| {
                    row.template_code == template_code(OFFBOARDING) && row.notation_id.is_some()
                })
            {
                println!(
                    "offboarding notation {} is already open in state {}",
                    row.notation_id.as_deref().unwrap_or_default(),
                    row.current_state.as_deref().unwrap_or("unknown")
                );
                return Ok(());
            }
            if remote::notation_create(
                host,
                template_code(OFFBOARDING),
                &project.client_dri,
                &project.code,
            )
            .await
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

fn template_relative(root: &Path, kind: &str) -> String {
    format!(
        "{}/{}.md",
        super::repository::draft_directory(root),
        template_code(kind)
    )
}

fn template_path(root: &Path, kind: &str) -> PathBuf {
    root.join(template_relative(root, kind))
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
pub(crate) use project_scaffold::{template_code, template_stub};

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
    [ONBOARDING, OFFBOARDING]
        .iter()
        .any(|kind| contents == template_stub(kind))
}

fn require_template_matches_main(root: &Path, kind: &str) -> Result<()> {
    git(root, &["fetch", "origin", "main"])
        .context("fetch origin/main before opening the notation")?;
    let relative = template_relative(root, kind);
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

fn git_text(root: &Path, args: &[&str]) -> Result<String> {
    let output = git(root, args)?;
    if !output.status.success() {
        return Err(anyhow!("git {} failed", args.join(" ")));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git(root: &Path, args: &[&str]) -> Result<std::process::Output> {
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

fn github_token() -> Result<String> {
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

fn github_owner_name(remote: &str) -> Result<(String, String)> {
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
        let template = template_path(root, kind);
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
            template_path(Path::new("project"), ONBOARDING),
            PathBuf::from("project/drafts/onboarding__letter.md")
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
            let path = template_path(root.path(), kind);
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
        let violations = rules::clauses::ClauseReferences.lint(&rules::SourceFile {
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
            template_path(root.path(), ONBOARDING),
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

    #[tokio::test]
    async fn onboarding_writes_a_stub_before_making_any_remote_request() {
        let root = tempfile::tempdir().unwrap();
        write_manifest(root.path());
        run_open(root.path(), None, ONBOARDING).await.unwrap();
        let stub = std::fs::read_to_string(template_path(root.path(), ONBOARDING)).unwrap();
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
        run_open(root.path(), None, ONBOARDING).await.unwrap();
    }

    #[tokio::test]
    async fn offboarding_writes_a_stub_or_refuses_to_archive_a_dirty_repository() {
        let root = tempfile::tempdir().unwrap();
        write_manifest(root.path());
        assert_eq!(
            run_offboard(root.path(), None, None).await,
            ExitCode::SUCCESS
        );
        assert!(template_path(root.path(), OFFBOARDING).is_file());

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
            template_path(root.path(), ONBOARDING),
            root.path().join("templates/onboarding__letter.md")
        );
        assert_eq!(
            template_relative(root.path(), OFFBOARDING),
            "templates/offboarding__letter.md"
        );

        std::fs::create_dir(root.path().join("drafts")).unwrap();
        assert_eq!(
            template_relative(root.path(), ONBOARDING),
            "drafts/onboarding__letter.md",
            "an ambiguous checkout falls back to the canonical spelling; the gate refuses it"
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
        run_open(root.path(), None, ONBOARDING).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join(relative)).unwrap(),
            stub
        );
    }

    #[test]
    fn template_path_is_under_project_root() {
        let root = Path::new("matter");
        assert_eq!(
            super::template_path(root, "onboarding"),
            root.join("drafts/onboarding__letter.md")
        );
    }
}

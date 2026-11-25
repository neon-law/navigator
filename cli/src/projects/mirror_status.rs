//! `navigator project mirror-status`: per Project checkout, GitHub `main`,
//! Forgejo `main`, and how the Forgejo mirror stands against GitHub.
//!
//! Read-only. It reads each repository's `main` with `git ls-remote`, so it
//! needs the credentials git already holds and no Forgejo API token, and it
//! writes nothing to any remote or ref. The relation is the mirror's to
//! GitHub: `equal`, `behind` (a bump can fast-forward it), `ahead`, or
//! `diverged` (preserved, never overwritten).

use std::path::Path;
use std::process::ExitCode;

use std::fmt::Write as _;

use anyhow::{ensure, Context, Result};
use serde::Serialize;

use super::lifecycle::{git_text, github_origin};
use super::mirror::{self, Relation};
use super::{manifest, workspace_root, workspace_skills};

const MAIN: &str = "refs/heads/main";

/// One Project's mirror standing.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Row {
    pub project: String,
    pub github_main: Option<String>,
    pub forgejo_main: Option<String>,
    pub relation: Option<Relation>,
    /// Why there is no relation, or what a reader should know.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Set when the Project's remotes could not be read at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

async fn row(dir: &Path) -> Row {
    let project = dir
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let mut row = Row {
        project,
        github_main: None,
        forgejo_main: None,
        relation: None,
        note: None,
        error: None,
    };
    if let Err(error) = fill(dir, &mut row).await {
        row.error = Some(format!("{error:#}"));
    }
    row
}

async fn fill(dir: &Path, row: &mut Row) -> Result<()> {
    let origin =
        git_text(dir, &["remote", "get-url", "origin"]).context("read the origin remote")?;
    let Ok(forgejo) = git_text(dir, &["remote", "get-url", "forgejo"]) else {
        if github_origin(dir, &origin).await.is_some() {
            row.github_main = mirror::remote_ref(dir, &origin, MAIN)?;
            row.note = Some("GitHub only: no Forgejo remote".into());
        } else {
            row.forgejo_main = mirror::remote_ref(dir, &origin, MAIN)?;
            row.note = Some("Forgejo only: no GitHub repository".into());
        }
        return Ok(());
    };
    ensure!(
        origin != forgejo,
        "the Forgejo remote is the same repository as origin"
    );
    row.github_main = mirror::remote_ref(dir, &origin, MAIN)?;
    row.forgejo_main = mirror::remote_ref(dir, &forgejo, MAIN)?;
    match (&row.github_main, &row.forgejo_main) {
        (None, _) => row.note = Some("GitHub has no main".into()),
        (Some(_), None) => row.note = Some("Forgejo has no main".into()),
        (Some(_), Some(_)) => {
            row.relation = mirror::inspect(dir, &origin, &forgejo, MAIN)?.relation;
        }
    }
    Ok(())
}

/// Every Project checkout under `root`, or `root` itself when it is one.
pub async fn rows(root: &Path) -> Result<Vec<Row>> {
    let directories = if root.join(manifest::FILE).is_file() {
        vec![root.to_path_buf()]
    } else {
        workspace_skills::project_directories(root)
            .with_context(|| format!("read {}", root.display()))?
    };
    let mut rows = Vec::new();
    for directory in directories {
        rows.push(row(&directory).await);
    }
    Ok(rows)
}

fn short(sha: Option<&str>) -> &str {
    sha.map_or("-", |sha| &sha[..sha.len().min(12)])
}

/// The rows as an aligned table.
#[must_use]
pub fn render(rows: &[Row]) -> String {
    let width = rows
        .iter()
        .map(|row| row.project.len())
        .max()
        .unwrap_or(7)
        .max(7);
    let mut out = format!(
        "{:<width$}  {:<12}  {:<12}  RELATION\n",
        "PROJECT", "GITHUB MAIN", "FORGEJO MAIN"
    );
    for row in rows {
        let relation = match (&row.error, row.relation, &row.note) {
            (Some(error), _, _) => format!("error: {error}"),
            (None, Some(relation), _) => relation.as_str().to_owned(),
            (None, None, Some(note)) => note.clone(),
            (None, None, None) => "-".to_owned(),
        };
        let _ = writeln!(
            out,
            "{:<width$}  {:<12}  {:<12}  {relation}",
            row.project,
            short(row.github_main.as_deref()),
            short(row.forgejo_main.as_deref()),
        );
    }
    out
}

pub async fn run(dir: Option<&Path>, json: bool) -> ExitCode {
    let rows = async {
        let root = workspace_root::selected(dir)?;
        ensure!(root.is_dir(), "workspace is not a directory");
        rows(&root).await
    }
    .await;
    match rows {
        Ok(rows) => {
            if json {
                println!("{}", serde_json::json!({ "projects": rows }));
            } else {
                print!("{}", render(&rows));
            }
            if rows.iter().any(|row| row.error.is_some()) {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(error) => {
            eprintln!("navigator project mirror-status: {error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(dir: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(["-c", "commit.gpgsign=false", "-c", "user.name=Test"])
            .args(["-c", "user.email=test@example.test"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn bare(root: &Path, name: &str) -> String {
        let path = root.join(name);
        std::fs::create_dir(&path).unwrap();
        run_git(
            &path,
            &["init", "--quiet", "--bare", "--initial-branch=main"],
        );
        path.to_str().unwrap().to_owned()
    }

    /// A Project checkout whose origin is the "GitHub" repository and whose
    /// `forgejo` remote is the mirror, both starting at one commit.
    fn project(workspace: &Path, name: &str) -> (std::path::PathBuf, String, String) {
        let dir = workspace.join(name);
        std::fs::create_dir(&dir).unwrap();
        run_git(&dir, &["init", "--quiet", "--initial-branch=main"]);
        std::fs::write(dir.join(manifest::FILE), format!("project: {name}\n")).unwrap();
        run_git(&dir, &["add", "."]);
        run_git(&dir, &["commit", "--quiet", "-m", "base"]);
        let remotes = workspace.join(format!("{name}-remotes"));
        std::fs::create_dir(&remotes).unwrap();
        let github = bare(&remotes, "github.git");
        let forgejo = bare(&remotes, "forgejo.git");
        run_git(&dir, &["remote", "add", "origin", &github]);
        run_git(&dir, &["remote", "add", "forgejo", &forgejo]);
        for url in [&github, &forgejo] {
            run_git(&dir, &["push", "--quiet", url, "HEAD:refs/heads/main"]);
        }
        (dir, github, forgejo)
    }

    fn commit(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), name).unwrap();
        run_git(dir, &["add", name]);
        run_git(dir, &["commit", "--quiet", "-m", name]);
    }

    /// ENG-1418 acceptance: one equal, one behind, one diverged.
    #[tokio::test]
    async fn the_fleet_reports_equal_behind_and_diverged_and_changes_nothing() {
        let workspace = tempfile::tempdir().unwrap();
        let root = workspace.path();
        let (_equal, _, _) = project(root, "a-equal");

        let (behind, github, _) = project(root, "b-behind");
        commit(&behind, "release");
        run_git(
            &behind,
            &["push", "--quiet", &github, "HEAD:refs/heads/main"],
        );

        let (diverged, github, forgejo) = project(root, "c-diverged");
        commit(&diverged, "mirror-side");
        run_git(
            &diverged,
            &["push", "--quiet", &forgejo, "HEAD:refs/heads/main"],
        );
        run_git(&diverged, &["reset", "--quiet", "--hard", "HEAD~1"]);
        commit(&diverged, "github-side");
        run_git(
            &diverged,
            &["push", "--quiet", &github, "HEAD:refs/heads/main"],
        );

        let before: Vec<_> = ["b-behind-remotes", "c-diverged-remotes"]
            .iter()
            .map(|name| run_git(&root.join(name).join("forgejo.git"), &["rev-parse", "main"]))
            .collect();
        let rows = rows(root).await.unwrap();
        let relations: Vec<_> = rows
            .iter()
            .map(|row| (row.project.as_str(), row.relation))
            .collect();
        assert_eq!(
            relations,
            [
                ("a-equal", Some(Relation::Equal)),
                ("b-behind", Some(Relation::Behind)),
                ("c-diverged", Some(Relation::Diverged)),
            ]
        );
        assert!(rows
            .iter()
            .all(|row| row.github_main.is_some() && row.forgejo_main.is_some()));
        let after: Vec<_> = ["b-behind-remotes", "c-diverged-remotes"]
            .iter()
            .map(|name| run_git(&root.join(name).join("forgejo.git"), &["rev-parse", "main"]))
            .collect();
        assert_eq!(before, after, "a status read moves no remote ref");

        let table = render(&rows);
        assert!(table.starts_with("PROJECT"), "{table}");
        for word in ["equal", "behind", "diverged"] {
            assert!(table.contains(word), "{table}");
        }
        let json = serde_json::to_value(&rows).unwrap();
        assert_eq!(json[1]["relation"], "behind");
        assert!(json[0].get("note").is_none() && json[0].get("error").is_none());
    }

    #[tokio::test]
    async fn a_checkout_without_a_second_repository_or_a_main_says_which() {
        let workspace = tempfile::tempdir().unwrap();
        let root = workspace.path();
        let (only, _, _) = project(root, "a-origin-only");
        run_git(&only, &["remote", "remove", "forgejo"]);
        let (empty_mirror, _, _) = project(root, "b-empty-mirror");
        let empty = bare(root, "empty.git");
        run_git(&empty_mirror, &["remote", "set-url", "forgejo", &empty]);
        let (broken, _, _) = project(root, "c-no-origin");
        run_git(&broken, &["remote", "remove", "origin"]);

        let rows = rows(root).await.unwrap();
        assert_eq!(
            rows[0].note.as_deref(),
            Some("Forgejo only: no GitHub repository")
        );
        assert!(rows[0].forgejo_main.is_some() && rows[0].relation.is_none());
        assert_eq!(rows[1].note.as_deref(), Some("Forgejo has no main"));
        assert!(
            rows[2].error.as_deref().unwrap().contains("origin"),
            "{:?}",
            rows[2]
        );
        assert!(render(&rows).contains("error:"));
    }

    #[tokio::test]
    async fn a_single_checkout_is_read_as_itself() {
        let workspace = tempfile::tempdir().unwrap();
        let (dir, _, _) = project(workspace.path(), "solo");
        let rows = rows(&dir).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].project, "solo");
        assert_eq!(rows[0].relation, Some(Relation::Equal));
    }
}

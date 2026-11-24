//! Inspect a commit's checked-in files, including its own Git index, independently of local edits.
use super::{
    document_check,
    lifecycle::{git, git_text},
    manifest, push,
};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::{path::Path, process::ExitCode};

// Hooks and GUI clients may export an alternate index or object database.
fn scoped_command(program: impl AsRef<std::ffi::OsStr>, dir: &Path) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    command.current_dir(dir).env_remove("GITHUB_REPOSITORY");
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    ] {
        command.env_remove(key);
    }
    command
}

fn check_snapshot_links(root: &Path) -> Result<()> {
    // Only the private checkout is writable. A tracked symlink can otherwise
    // direct a layout repair into another checkout, even through a directory.
    let canonical = root.canonicalize()?;
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| e.file_name() != ".git")
    {
        let entry = entry?;
        if entry.file_type().is_symlink() {
            let target = entry
                .path()
                .canonicalize()
                .context("resolve a tracked symlink before simulating repairs")?;
            ensure!(
                target.starts_with(&canonical) && !target.starts_with(canonical.join(".git")),
                "tracked symlink leaves the snapshot"
            );
        }
    }
    Ok(())
}

fn simulate_repairs(root: &Path) -> Result<BTreeSet<String>> {
    let simulated = crate::commands::run_repository_gate(root, false, false);
    ensure!(
        simulated == ExitCode::SUCCESS || simulated == ExitCode::FAILURE,
        "repair simulation unavailable"
    );
    let mut paths = BTreeSet::new();
    for args in [
        vec!["diff", "--no-renames", "--name-only", "-z", "HEAD"],
        vec!["ls-files", "--others", "-z"],
    ] {
        let output = git(root, &args)?;
        ensure!(output.status.success(), "could not list simulated repairs");
        paths.extend(
            std::str::from_utf8(&output.stdout)?
                .split_terminator('\0')
                .map(str::to_owned),
        );
    }
    Ok(paths)
}

fn checkout(repo: &Path, sha: &str, root: &Path) -> Result<()> {
    // A local clone keeps history for removed-pointer checks. Its private index
    // also lets the layout gate inspect tracking and ignore rules at this SHA.
    git_text(
        repo,
        &[
            "clone",
            "--quiet",
            "--no-checkout",
            "--local",
            "--",
            ".",
            root.to_str().context("invalid scratch path")?,
        ],
    )?;
    git_text(root, &["checkout", "--quiet", "--detach", sha])?;
    Ok(())
}

/// The child owns its temporary checkout and may change its working directory.
pub(crate) async fn inspect(sha: &str, repair: bool) -> Result<ExitCode> {
    let temp = tempfile::tempdir()?;
    let root = temp.path();
    checkout(Path::new("."), sha, root)?;
    std::env::set_current_dir(root)?;
    let root = Path::new(".");
    check_snapshot_links(root)?;
    let manifest = manifest::read(root).context("revision needs a Project manifest")?;
    ensure!(manifest.project.is_some(), "revision needs a Project code");
    let pin = manifest.version.context("revision needs a release pin")?;
    ensure!(
        pin == crate::cli_version(),
        "run preflight with the Project's pinned Navigator release {pin}"
    );

    if !repair {
        return Ok(crate::commands::run_repository_gate(root, true, false));
    }
    let live = document_check::publish_preflight(root).await;
    let live = match live {
        Ok(mut outcome) => {
            outcome
                .fixes
                .sort_by(|a, b| (&a.path, &a.action).cmp(&(&b.path, &b.action)));
            outcome
                .errors
                .sort_by(|a, b| (&a.location, &a.message).cmp(&(&b.location, &b.message)));
            outcome
                .warnings
                .sort_by(|a, b| (&a.location, &a.message).cmp(&(&b.location, &b.message)));
            let ignored = push::ignored_pointer_paths(root, &outcome.fixes)?;
            let status = if outcome.errors.is_empty() && outcome.fixes.is_empty() {
                "passed"
            } else {
                "failed"
            };
            json!({"status":status,"errors":outcome.errors,"repairs":outcome.fixes,"warnings":outcome.warnings,"ignored_pointers":ignored})
        }
        Err(err) => {
            json!({"status":"unavailable", "detail": format!("{err:#}")})
        }
    };
    let repair_result = simulate_repairs(root);
    let repair_error = repair_result.as_ref().err().map(|err| format!("{err:#}"));
    let repairs = repair_result.unwrap_or_default();
    println!(
        "{}",
        json!({"revision":sha,"release":pin,"passed":repair_error.is_none() && repairs.is_empty() && live["status"] == "passed",
        "live_documents":live,"local_repairs":repairs,"repair_error":repair_error})
    );
    Ok(ExitCode::SUCCESS)
}

fn report(repo: &Path, revision: &str) -> Result<Value> {
    let selected = scoped_command("git", repo)
        .args([
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{revision}^{{commit}}"),
        ])
        .output()?;
    ensure!(
        selected.status.success(),
        "could not resolve the selected Project revision"
    );
    let sha = std::str::from_utf8(&selected.stdout)?.trim();
    let executable = std::env::current_exe()?;
    let inspect = |repair: bool| -> Result<_> {
        let mut command = scoped_command(&executable, repo);
        command.args(["release", "inspect-tree", "--revision", sha]);
        if repair {
            command.arg("--repair");
        }
        Ok(command.output()?)
    };
    let checked = inspect(false)?;
    let status = match checked.status.code() {
        Some(0) => "passed",
        Some(1) => "failed",
        _ => "unavailable",
    };
    let logs = format!(
        "{}{}",
        String::from_utf8_lossy(&checked.stdout),
        String::from_utf8_lossy(&checked.stderr)
    );
    let diagnostics: BTreeSet<_> = logs.lines().collect();
    let live = inspect(true)?;
    let mut report: Value = if live.status.success() {
        // The local gate prints its diagnostics before the worker's final JSON record.
        serde_json::from_str(
            std::str::from_utf8(&live.stdout)?
                .lines()
                .last()
                .context("repair report missing")?,
        )?
    } else {
        json!({"revision":sha, "passed":false, "live_documents":{"status":"unavailable"}, "error":String::from_utf8_lossy(&live.stderr).trim()})
    };
    report["repository"] = json!({"status":status,"diagnostics":diagnostics});
    report["passed"] = json!(checked.status.success() && report["passed"] == true);
    Ok(report)
}

pub(crate) fn run(repo: &Path, revision: &str) -> ExitCode {
    match report(repo, revision) {
        Ok(report) => {
            println!("{report}");
            if report["passed"] == true {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(err) => {
            println!("{}", json!({"passed":false, "error":format!("{err:#}")}));
            ExitCode::FAILURE
        }
    }
}

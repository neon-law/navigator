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

fn repository_check(root: &Path, repair: bool) -> Result<std::process::Output> {
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command.args(["release", "inspect-tree"]);
    if repair {
        command.arg("--repair");
    }
    Ok(command
        .current_dir(root)
        .env_remove("GITHUB_REPOSITORY")
        .output()?)
}

pub(crate) fn simulate_repairs(repo: &Path) -> Result<BTreeSet<String>> {
    let temp = tempfile::tempdir()?;
    let root = temp.path();
    checkout(repo, "HEAD", root)?;
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

pub(super) async fn report(repo: &Path, revision: &str) -> Result<Value> {
    let sha = git_text(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{revision}^{{commit}}"),
        ],
    )?;
    let temp = tempfile::tempdir()?;
    let root = temp.path();
    checkout(repo, &sha, root)?;
    let manifest = manifest::read(root).context("revision needs a Project manifest")?;
    ensure!(manifest.project.is_some(), "revision needs a Project code");
    let pin = manifest.version.context("revision needs a release pin")?;
    ensure!(
        pin == crate::cli_version(),
        "run preflight with the Project's pinned Navigator release {pin}"
    );

    let checked = repository_check(root, false)?;
    let repository_status = match checked.status.code() {
        Some(0) => "passed",
        Some(1) => "failed",
        _ => "unavailable",
    };
    let logs = format!(
        "{}{}",
        String::from_utf8_lossy(&checked.stdout),
        String::from_utf8_lossy(&checked.stderr)
    )
    .replace(&root.canonicalize()?.display().to_string(), ".")
    .replace(&root.display().to_string(), ".");
    let diagnostics: BTreeSet<_> = logs.lines().collect();
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
            json!({"status":"unavailable", "detail": format!("{err:#}").replace(&root.display().to_string(), ".")})
        }
    };
    let repair_result = repository_check(root, true).and_then(|output| {
        ensure!(
            output.status.success(),
            "could not simulate repository repairs: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = std::str::from_utf8(&output.stdout)?;
        Ok(serde_json::from_str::<BTreeSet<String>>(
            stdout.lines().last().context("repair report missing")?,
        )?)
    });
    let repair_error = repair_result.as_ref().err().map(|err| format!("{err:#}"));
    let repairs = repair_result.unwrap_or_default();
    let passed = checked.status.success()
        && repair_error.is_none()
        && repairs.is_empty()
        && live["status"] == "passed";
    Ok(json!({
        "revision":sha, "release":pin, "passed":passed,
        "repository":{"status":repository_status, "diagnostics":diagnostics},
        "live_documents":live, "local_repairs":repairs, "repair_error":repair_error,
    }))
}

pub(crate) async fn run(repo: &Path, revision: &str) -> ExitCode {
    match report(repo, revision).await {
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

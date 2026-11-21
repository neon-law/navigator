//! Build-only Git runtime acceptance. This probe is not copied into the site image.
use std::error::Error;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::{Command, Output};

use repos::{Author, RepoStore};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn check(ok: bool, message: &str) -> Result<()> {
    if !ok {
        return Err(message.into());
    }
    Ok(())
}

fn git(directory: &Path, args: &[&str]) -> Result<Output> {
    Ok(Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .output()?)
}

fn successful(directory: &Path, args: &[&str]) -> Result<String> {
    let output = git(directory, args)?;
    if !output.status.success() {
        return Err(format!(
            "synthetic Git runtime command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn private_owned(path: &Path) -> Result<()> {
    let metadata = path.metadata()?;
    check(
        metadata.uid() == 65532 && metadata.gid() == 65532,
        "runtime does not use UID/GID 65532",
    )?;
    check(
        metadata.permissions().mode() & 0o777 == 0o700,
        "repo is not private",
    )
}

fn roundtrip(store: &RepoStore) -> Result<()> {
    let path = store.ensure_code("synthetic-runtime")?;
    private_owned(store.root())?;
    private_owned(&path)?;
    check(
        store.head_oid_code("synthetic-runtime")?.is_none(),
        "new repo has a head",
    )?;
    let first = store.commit_as_code(
        "synthetic-runtime",
        Author {
            name: "synthetic-one",
            email: "one@example.invalid",
        },
        "synthetic first",
        &[("keep.txt", b"synthetic retained bytes")],
    )?;
    let second = store.commit_as_code(
        "synthetic-runtime",
        Author {
            name: "synthetic-two",
            email: "two@example.invalid",
        },
        "synthetic second",
        &[("removed.txt", b"synthetic removable bytes")],
    )?;
    check(first != second, "two commits did not advance history")?;
    check(
        successful(&path, &["show", "-s", "--format=%an|%ae|%cn|%ce", &first])?
            == "synthetic-one|one@example.invalid|synthetic-one|one@example.invalid",
        "first attribution changed",
    )?;
    check(
        successful(&path, &["rev-parse", "main~1"])? == first,
        "commit parent changed",
    )?;
    check(
        successful(&path, &["log", "-1", "--format=%an|%ae|%cn|%ce"])?
            == "synthetic-two|two@example.invalid|synthetic-two|two@example.invalid",
        "attribution changed",
    )?;
    let files = store.read_head_tree_code("synthetic-runtime")?;
    check(
        files
            == vec![
                ("keep.txt".into(), b"synthetic retained bytes".to_vec()),
                ("removed.txt".into(), b"synthetic removable bytes".to_vec()),
            ],
        "head tree bytes changed",
    )?;
    let outcome = store.expunge_path_code("synthetic-runtime", "removed.txt")?;
    check(
        outcome.head_before.as_deref() == Some(second.as_str()),
        "expunge lost previous head",
    )?;
    check(
        outcome.head_after.is_some() && outcome.head_after != outcome.head_before,
        "expunge did not rewrite history",
    )?;
    check(
        store.read_head_tree_code("synthetic-runtime")?
            == vec![("keep.txt".into(), b"synthetic retained bytes".to_vec())],
        "expunge changed retained bytes",
    )?;
    check(
        successful(&path, &["log", "--all", "--oneline", "--", "removed.txt"])?.is_empty(),
        "removed path remains in history",
    )?;
    check(
        !successful(&path, &["rev-list", "--all", "--objects"])?.contains("removed.txt"),
        "removed path remains reachable",
    )
}

fn push_guards(store: &RepoStore, work: &Path) -> Result<()> {
    let bare = store.ensure_code("synthetic-push")?;
    successful(
        work,
        &["clone", bare.to_str().ok_or("non-UTF8 fixture path")?, "."],
    )?;
    successful(work, &["config", "user.name", "synthetic-writer"])?;
    successful(work, &["config", "user.email", "writer@example.invalid"])?;
    std::fs::write(work.join("keep.txt"), b"synthetic push bytes")?;
    successful(work, &["add", "keep.txt"])?;
    successful(work, &["commit", "-m", "synthetic initial"])?;
    successful(work, &["push", "origin", "main"])?;
    std::fs::write(work.join("keep.txt"), b"synthetic fast-forward bytes")?;
    successful(work, &["commit", "-am", "synthetic fast-forward"])?;
    successful(work, &["push", "origin", "main"])?;
    let branch = git(work, &["push", "origin", "HEAD:refs/heads/feature"])?;
    check(
        !branch.status.success() && String::from_utf8(branch.stderr)?.contains("single-branch"),
        "pre-receive hook allowed a second branch",
    )?;
    std::fs::write(work.join("keep.txt"), b"synthetic rewritten bytes")?;
    successful(work, &["commit", "--amend", "-am", "synthetic rewrite"])?;
    let force = git(work, &["push", "--force", "origin", "main"])?;
    check(
        !force.status.success() && String::from_utf8(force.stderr)?.contains("non-fast-forward"),
        "receive-pack allowed rewriting main",
    )?;
    let delete = git(work, &["push", "origin", "--delete", "main"])?;
    check(
        !delete.status.success()
            && String::from_utf8(delete.stderr)?.contains("deletion prohibited"),
        "receive-pack allowed deleting main",
    )?;
    check(
        store.read_head_tree_code("synthetic-push")?
            == vec![("keep.txt".into(), b"synthetic fast-forward bytes".to_vec())],
        "refused pushes changed stored bytes",
    )
}

fn ownership_guard(foreign: &Path) -> Result<()> {
    let metadata = foreign.metadata()?;
    check(
        metadata.uid() != 65532 && metadata.gid() == 65532,
        "foreign fixture must have another owner and the runtime group",
    )?;
    let output = git(foreign, &["rev-parse", "--is-bare-repository"])?;
    check(
        !output.status.success() && String::from_utf8(output.stderr)?.contains("dubious ownership"),
        "Git foreign-owner protection was bypassed",
    )
}

fn main() -> Result<()> {
    let foreign = std::env::args_os()
        .nth(1)
        .ok_or("missing foreign-owner fixture")?;
    let root = tempfile::tempdir()?;
    let store = RepoStore::new(root.path().join("repos"));
    roundtrip(&store)?;
    let work = tempfile::tempdir()?;
    push_guards(&store, work.path())?;
    ownership_guard(Path::new(&foreign))?;
    std::fs::write(
        "/tmp/navigator-git-runtime-proof",
        b"format_version=1\nuid=65532\ngid=65532\ngit_runtime=passed\n",
    )?;
    Ok(())
}

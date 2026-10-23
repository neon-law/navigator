//! `navigator ops public-snapshot` writes a new local Git repository holding
//! exactly one root commit: the source ref's tree minus the paths listed in
//! that ref's `.public-snapshot-exclude`. No history travels with it.
//!
//! The commit's author, committer, and date come from the source commit (or
//! the identity flags) and the message is fixed, so two runs on one ref
//! produce one commit SHA. The command reads the source repository and writes
//! only the `--out` directory: it never fetches, pushes, adds a remote, or
//! changes a hosted repository. `docs/public-snapshot.md` is the procedure.

use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

use anyhow::{bail, Context, Result};

/// The checked-in exclusion manifest, read from the source ref's own tree.
pub(crate) const EXCLUDE_MANIFEST: &str = ".public-snapshot-exclude";

/// The branch the source ref must already be reachable from.
pub(crate) const PUBLISHED_BASE: &str = "origin/main";

/// The snapshot commit's message. Fixed, because it is part of the SHA.
pub(crate) const MESSAGE: &str = "Navigator public source snapshot\n";

/// Overrides for the snapshot commit's identity. `None` keeps the source
/// commit's committer name, email, or date.
#[derive(Debug, Default, Clone)]
pub(crate) struct Identity {
    pub name: Option<String>,
    pub email: Option<String>,
    pub date: Option<String>,
}

/// One snapshot request.
pub(crate) struct Request<'a> {
    pub repo: &'a Path,
    pub git_ref: &'a str,
    pub out: &'a Path,
    pub identity: Identity,
    pub sign: bool,
}

/// What the snapshot wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub commit: String,
    pub tree: String,
    pub source_tree: String,
    pub excluded_paths: usize,
    /// Manifest entries that matched nothing in the source tree.
    pub unmatched: Vec<String>,
}

/// A precondition the source repository failed. Nothing has been written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    NotARepository,
    DirtyWorktree,
    UnknownRef,
    RefNotCheckedOut,
    MissingBase,
    NotOnBase,
    OutNotEmpty,
    ValidateFailed(String),
    MissingManifest,
    BadManifestEntry(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotARepository => write!(f, "--repo is not a Git worktree"),
            Self::DirtyWorktree => write!(
                f,
                "the worktree has uncommitted or untracked changes; commit or remove them first"
            ),
            Self::UnknownRef => write!(f, "--ref does not name a commit in --repo"),
            Self::RefNotCheckedOut => write!(
                f,
                "--ref is not the checked-out commit; check it out so `navigator validate` \
                 reads exactly the tree being published"
            ),
            Self::MissingBase => write!(
                f,
                "{PUBLISHED_BASE} does not exist in --repo; run `git fetch origin main` first"
            ),
            Self::NotOnBase => write!(
                f,
                "--ref is not reachable from {PUBLISHED_BASE}; only merged history is published"
            ),
            Self::OutNotEmpty => write!(f, "--out must be a new or empty directory"),
            Self::ValidateFailed(detail) => {
                write!(f, "`navigator validate` failed on --ref: {detail}")
            }
            Self::MissingManifest => write!(f, "--ref carries no {EXCLUDE_MANIFEST}"),
            Self::BadManifestEntry(entry) => write!(
                f,
                "{EXCLUDE_MANIFEST} entry `{entry}` must be a relative path inside the repository"
            ),
        }
    }
}

/// Either a precondition refusal or an operational failure.
#[derive(Debug)]
pub(crate) enum Failure {
    Refused(Refusal),
    Failed(anyhow::Error),
}

impl From<Refusal> for Failure {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

impl From<anyhow::Error> for Failure {
    fn from(error: anyhow::Error) -> Self {
        Self::Failed(error)
    }
}

/// Entry point for `ops public-snapshot`. Exits 2 on a refusal or failure.
pub fn run(request: &Request<'_>) -> ExitCode {
    let validate = |root: &Path| -> Result<()> {
        let exe = std::env::current_exe().context("locate the navigator binary")?;
        let status = Command::new(exe)
            .arg("validate")
            .arg(root)
            // stdout carries only the snapshot SHA.
            .stdout(std::io::stderr())
            .status()
            .context("run `navigator validate`")?;
        if status.success() {
            Ok(())
        } else {
            bail!("it exited with {status}")
        }
    };
    match prepare(request, validate) {
        Ok(snapshot) => {
            report(&snapshot, request.sign);
            ExitCode::SUCCESS
        }
        Err(Failure::Refused(refusal)) => {
            eprintln!("navigator: public-snapshot: refused: {refusal}");
            ExitCode::from(2)
        }
        Err(Failure::Failed(error)) => {
            eprintln!("navigator: public-snapshot: {error:#}");
            ExitCode::from(2)
        }
    }
}

fn report(snapshot: &Snapshot, sign: bool) {
    println!("{}", snapshot.commit);
    eprintln!(
        "navigator: public-snapshot: one root commit {} written to the --out repository",
        snapshot.commit
    );
    eprintln!("  snapshot tree: {}", snapshot.tree);
    eprintln!("  source tree:   {}", snapshot.source_tree);
    eprintln!(
        "  excluded:      {} path(s) by {EXCLUDE_MANIFEST}",
        snapshot.excluded_paths
    );
    for entry in &snapshot.unmatched {
        eprintln!("  warning: {EXCLUDE_MANIFEST} entry `{entry}` matched nothing");
    }
    if sign {
        eprintln!(
            "  signed: the signature is part of the commit, so a rehearsal's SHA differs; \
             compare tree hashes instead"
        );
    }
    eprintln!(
        "Nothing was pushed and no remote was added. Next, by hand, per docs/public-snapshot.md:\n  \
         1. Re-run into a second empty directory and confirm the same SHA.\n  \
         2. In the --out repository, confirm `git rev-list --count HEAD` prints 1.\n  \
         3. Archive the private repository.\n  \
         4. Create the public repository, add it as a remote of the --out repository, and push."
    );
}

/// Check every precondition, then build the snapshot repository at
/// `request.out`. `validate` receives the source worktree root and runs only
/// after the cheaper checks pass.
pub(crate) fn prepare(
    request: &Request<'_>,
    validate: impl FnOnce(&Path) -> Result<()>,
) -> Result<Snapshot, Failure> {
    let repo = request.repo;
    let root = git(repo, &["rev-parse", "--show-toplevel"]).map_err(|_| Refusal::NotARepository)?;
    let root = Path::new(root.trim()).to_path_buf();
    if !git(
        &root,
        &["status", "--porcelain", "--untracked-files=normal"],
    )?
    .trim()
    .is_empty()
    {
        return Err(Refusal::DirtyWorktree.into());
    }
    let commit = git(
        &root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{}^{{commit}}", request.git_ref),
        ],
    )
    .map_err(|_| Refusal::UnknownRef)?;
    let commit = commit.trim().to_string();
    if git(&root, &["rev-parse", "HEAD"])?.trim() != commit {
        return Err(Refusal::RefNotCheckedOut.into());
    }
    git(
        &root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{PUBLISHED_BASE}^{{commit}}"),
        ],
    )
    .map_err(|_| Refusal::MissingBase)?;
    if !git_succeeds(
        &root,
        &["merge-base", "--is-ancestor", &commit, PUBLISHED_BASE],
    )? {
        return Err(Refusal::NotOnBase.into());
    }
    if request.out.exists()
        && fs::read_dir(request.out)
            .context("read --out")?
            .next()
            .is_some()
    {
        return Err(Refusal::OutNotEmpty.into());
    }
    validate(&root).map_err(|error| Refusal::ValidateFailed(format!("{error:#}")))?;

    let manifest = git(&root, &["show", &format!("{commit}:{EXCLUDE_MANIFEST}")])
        .map_err(|_| Refusal::MissingManifest)?;
    let entries = parse_manifest(&manifest)?;

    let source_tree = git(&root, &["rev-parse", &format!("{commit}^{{tree}}")])?
        .trim()
        .to_string();
    let (tree, excluded_paths, unmatched) = filtered_tree(&root, &commit, &entries)?;
    let snapshot_commit = commit_tree(&root, &commit, &tree, &request.identity, request.sign)?;
    write_repository(&root, &snapshot_commit, request.out)?;

    Ok(Snapshot {
        commit: snapshot_commit,
        tree,
        source_tree,
        excluded_paths,
        unmatched,
    })
}

/// One repository-relative path per line; `#` starts a comment line. An entry
/// excludes that file or everything under that directory.
pub(crate) fn parse_manifest(text: &str) -> Result<Vec<String>, Refusal> {
    let mut entries = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let entry = line.trim_end_matches('/');
        if entry.is_empty()
            || entry.starts_with('/')
            || entry.contains('\\')
            || entry
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(Refusal::BadManifestEntry(line.to_string()));
        }
        entries.push(entry.to_string());
    }
    Ok(entries)
}

fn excluded(path: &str, entries: &[String]) -> Option<usize> {
    entries.iter().position(|entry| {
        path == entry
            || path
                .strip_prefix(entry.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// Write the source commit's tree minus the excluded paths into the source
/// object database through a throwaway index, and return its tree hash.
fn filtered_tree(
    root: &Path,
    commit: &str,
    entries: &[String],
) -> Result<(String, usize, Vec<String>)> {
    let listing = git_bytes(root, &["ls-tree", "-r", "-z", "--full-tree", commit])?;
    let mut kept = Vec::with_capacity(listing.len());
    let mut matched = vec![false; entries.len()];
    let mut excluded_paths = 0;
    for record in listing.split(|byte| *byte == 0).filter(|r| !r.is_empty()) {
        let text = std::str::from_utf8(record).context("ls-tree printed a non-UTF-8 path")?;
        let (_, path) = text
            .split_once('\t')
            .context("ls-tree record without a path")?;
        if let Some(index) = excluded(path, entries) {
            matched[index] = true;
            excluded_paths += 1;
        } else {
            kept.extend_from_slice(record);
            kept.push(0);
        }
    }
    let scratch = tempfile::tempdir().context("create a scratch index directory")?;
    let index = scratch.path().join("index");
    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["update-index", "-z", "--index-info"])
        .env("GIT_INDEX_FILE", &index)
        .stdin(Stdio::piped())
        .spawn()
        .context("run git update-index")?;
    child
        .stdin
        .take()
        .context("git update-index stdin")?
        .write_all(&kept)
        .context("write the filtered tree listing")?;
    if !child.wait()?.success() {
        bail!("git update-index failed");
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("write-tree")
        .env("GIT_INDEX_FILE", &index)
        .output()
        .context("run git write-tree")?;
    if !output.status.success() {
        bail!("git write-tree failed");
    }
    let tree = String::from_utf8(output.stdout)?.trim().to_string();
    let unmatched = entries
        .iter()
        .zip(matched)
        .filter(|(_, hit)| !hit)
        .map(|(entry, _)| entry.clone())
        .collect();
    Ok((tree, excluded_paths, unmatched))
}

/// Create the parentless snapshot commit with a fixed identity and message.
fn commit_tree(
    root: &Path,
    source: &str,
    tree: &str,
    identity: &Identity,
    sign: bool,
) -> Result<String> {
    let source_identity = git(
        root,
        &[
            "log",
            "-1",
            "--date=raw",
            "--format=%cn%x00%ce%x00%cd",
            source,
        ],
    )?;
    let mut fields = source_identity.trim_end_matches('\n').splitn(3, '\0');
    let (Some(name), Some(email), Some(date)) = (fields.next(), fields.next(), fields.next())
    else {
        bail!("could not read the source commit's committer");
    };
    let name = identity.name.as_deref().unwrap_or(name);
    let email = identity.email.as_deref().unwrap_or(email);
    let date = identity.date.as_deref().unwrap_or(date);
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(["-c", "i18n.commitEncoding=UTF-8", "commit-tree"])
        .arg(if sign { "-S" } else { "--no-gpg-sign" })
        .args([tree, "-F", "-"])
        .env("GIT_AUTHOR_NAME", name)
        .env("GIT_AUTHOR_EMAIL", email)
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_NAME", name)
        .env("GIT_COMMITTER_EMAIL", email)
        .env("GIT_COMMITTER_DATE", date)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let mut child = command.spawn().context("run git commit-tree")?;
    child
        .stdin
        .take()
        .context("git commit-tree stdin")?
        .write_all(MESSAGE.as_bytes())?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!("git commit-tree failed");
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

/// Initialise `out`, copy only the snapshot commit's objects into it, and
/// check `main` out at that commit.
fn write_repository(root: &Path, commit: &str, out: &Path) -> Result<()> {
    fs::create_dir_all(out).context("create --out")?;
    let output = Command::new("git")
        .args(["init", "--quiet", "--initial-branch=main"])
        .arg(out)
        .output()
        .context("run git init")?;
    if !output.status.success() {
        bail!("git init failed");
    }
    let mut pack = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["pack-objects", "--revs", "--stdout", "--quiet"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("run git pack-objects")?;
    pack.stdin
        .take()
        .context("git pack-objects stdin")?
        .write_all(format!("{commit}\n").as_bytes())?;
    let stream = pack.stdout.take().context("git pack-objects stdout")?;
    let index = Command::new("git")
        .arg("-C")
        .arg(out)
        .args(["index-pack", "--stdin"])
        .stdin(Stdio::from(stream))
        .stdout(Stdio::null())
        .status()
        .context("run git index-pack")?;
    if !pack.wait()?.success() || !index.success() {
        bail!("copying the snapshot objects into --out failed");
    }
    git(out, &["update-ref", "refs/heads/main", commit])?;
    git(out, &["reset", "--hard", "--quiet"])?;
    if git(out, &["rev-list", "--count", "HEAD"])?.trim() != "1" {
        bail!("the --out repository does not hold exactly one commit");
    }
    Ok(())
}

fn git_bytes(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .context("run git")?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8(git_bytes(dir, args)?)?)
}

fn git_succeeds(dir: &Path, args: &[&str]) -> Result<bool> {
    Ok(Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .context("run git")?
        .status
        .success())
}

#[cfg(test)]
#[path = "public_snapshot_tests.rs"]
mod tests;

//! Snapshot behaviour against synthetic temporary repositories. No test
//! touches a real remote: `origin/main` is a local ref set by `update-ref`.

use super::*;
use std::cell::Cell;
use std::path::PathBuf;

const MANIFEST: &str = "\
# Paths that never leave the private repository.
deployments/private-row/
secret-notes.md
";

fn sh(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_DATE", "1700000000 -0800")
        .env("GIT_COMMITTER_DATE", "1700000000 -0800")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn write(dir: &Path, path: &str, body: &str) {
    let path = dir.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

/// Two commits of history, the second merged to `origin/main` and checked out.
fn source_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    sh(repo, &["init", "--quiet", "--initial-branch=main"]);
    sh(repo, &["config", "user.email", "snapshot@example.com"]);
    sh(repo, &["config", "user.name", "snapshot-fixture"]);
    sh(repo, &["config", "commit.gpgsign", "false"]);
    write(repo, "secret-notes.md", "private history\n");
    sh(repo, &["add", "."]);
    sh(repo, &["commit", "--quiet", "-m", "private first commit"]);
    write(repo, EXCLUDE_MANIFEST, MANIFEST);
    write(repo, "README.md", "public\n");
    write(repo, "src/lib.rs", "pub fn f() {}\n");
    write(
        repo,
        "deployments/private-row/config.toml",
        "project = \"x\"\n",
    );
    write(
        repo,
        "deployments/example/config.toml",
        "project = \"example\"\n",
    );
    write(repo, "deployments/private-row-two.toml", "kept\n");
    sh(repo, &["add", "."]);
    sh(repo, &["commit", "--quiet", "-m", "second commit"]);
    sh(repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    dir
}

fn request<'a>(repo: &'a Path, out: &'a Path) -> Request<'a> {
    Request {
        repo,
        git_ref: "HEAD",
        out,
        identity: Identity::default(),
        sign: false,
    }
}

#[allow(clippy::unnecessary_wraps)] // the shape `prepare` takes
fn passes(_: &Path) -> Result<()> {
    Ok(())
}

fn refusal(result: Result<Snapshot, Failure>) -> Refusal {
    match result {
        Err(Failure::Refused(refusal)) => refusal,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn files(repo: &Path, rev: &str) -> Vec<String> {
    sh(repo, &["ls-tree", "-r", "--name-only", rev])
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn writes_one_root_commit_of_the_tree_minus_the_manifest() {
    let source = source_repo();
    let scratch = tempfile::tempdir().unwrap();
    let out = scratch.path().join("public");

    let snapshot = prepare(&request(source.path(), &out), passes).expect("snapshot");

    assert_eq!(sh(&out, &["rev-parse", "HEAD"]), snapshot.commit);
    assert_eq!(sh(&out, &["rev-list", "--count", "HEAD"]), "1");
    assert_eq!(
        sh(&out, &["rev-list", "--max-parents=0", "HEAD"]),
        snapshot.commit
    );
    assert_eq!(sh(&out, &["rev-parse", "HEAD^{tree}"]), snapshot.tree);
    assert_eq!(sh(&out, &["symbolic-ref", "HEAD"]), "refs/heads/main");
    assert_eq!(sh(&out, &["remote"]), "", "the snapshot has no remote");
    assert_eq!(sh(&out, &["status", "--porcelain"]), "");
    assert_eq!(
        files(&out, "HEAD"),
        [
            EXCLUDE_MANIFEST,
            "README.md",
            "deployments/example/config.toml",
            "deployments/private-row-two.toml",
            "src/lib.rs",
        ]
    );
    assert!(out.join("README.md").is_file(), "main is checked out");
    assert_eq!(snapshot.excluded_paths, 2);
    assert!(snapshot.unmatched.is_empty());
    assert_eq!(
        snapshot.source_tree,
        sh(source.path(), &["rev-parse", "HEAD^{tree}"])
    );
    assert_ne!(snapshot.tree, snapshot.source_tree);
}

#[test]
fn carries_no_object_from_private_history() {
    let source = source_repo();
    let scratch = tempfile::tempdir().unwrap();
    let out = scratch.path().join("public");
    prepare(&request(source.path(), &out), passes).expect("snapshot");

    let objects = sh(
        &out,
        &[
            "cat-file",
            "--batch-all-objects",
            "--batch-check=%(objecttype) %(objectname)",
        ],
    );
    let commits: Vec<&str> = objects
        .lines()
        .filter(|l| l.starts_with("commit "))
        .collect();
    assert_eq!(commits.len(), 1, "{objects}");
    let private_blob = sh(source.path(), &["rev-parse", "HEAD:secret-notes.md"]);
    let private_row = sh(
        source.path(),
        &["rev-parse", "HEAD:deployments/private-row/config.toml"],
    );
    for hash in [private_blob, private_row] {
        assert!(!objects.contains(&hash), "{hash} leaked: {objects}");
    }
    for source_commit in sh(source.path(), &["rev-list", "HEAD"]).lines() {
        assert!(!objects.contains(source_commit), "source commit leaked");
    }
}

#[test]
fn two_runs_on_one_ref_produce_one_sha() {
    let source = source_repo();
    let scratch = tempfile::tempdir().unwrap();
    let first = prepare(&request(source.path(), &scratch.path().join("a")), passes).unwrap();
    let second = prepare(&request(source.path(), &scratch.path().join("b")), passes).unwrap();
    assert_eq!(first, second);

    let header = sh(&scratch.path().join("a"), &["cat-file", "commit", "HEAD"]);
    assert!(
        header.contains("author snapshot-fixture <snapshot@example.com> 1700000000 -0800"),
        "{header}"
    );
    assert!(header.contains("committer snapshot-fixture <snapshot@example.com> 1700000000 -0800"));
    assert!(!header.contains("parent "), "{header}");
    assert!(!header.contains("gpgsig"), "{header}");
    assert!(header.ends_with(MESSAGE.trim_end()), "{header}");
}

#[test]
fn identity_flags_replace_the_source_identity_deterministically() {
    let source = source_repo();
    let scratch = tempfile::tempdir().unwrap();
    let identity = Identity {
        name: Some("override-fixture".into()),
        email: Some("source@example.com".into()),
        date: Some("1800000000 +0000".into()),
    };
    let (a, b) = (scratch.path().join("a"), scratch.path().join("b"));
    let mut first_request = request(source.path(), &a);
    first_request.identity = identity.clone();
    let mut second_request = request(source.path(), &b);
    second_request.identity = identity;
    let first = prepare(&first_request, passes).unwrap();
    let second = prepare(&second_request, passes).unwrap();
    let default = prepare(&request(source.path(), &scratch.path().join("c")), passes).unwrap();

    assert_eq!(first.commit, second.commit);
    assert_ne!(first.commit, default.commit);
    assert_eq!(first.tree, default.tree);
    let header = sh(&scratch.path().join("a"), &["cat-file", "commit", "HEAD"]);
    assert!(
        header.contains("author override-fixture <source@example.com> 1800000000 +0000"),
        "{header}"
    );
}

#[test]
fn reports_manifest_entries_that_match_nothing() {
    let source = source_repo();
    write(
        source.path(),
        EXCLUDE_MANIFEST,
        "secret-notes.md\nnot/here/\n",
    );
    sh(source.path(), &["commit", "--quiet", "-am", "manifest"]);
    sh(
        source.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    let scratch = tempfile::tempdir().unwrap();
    let snapshot = prepare(&request(source.path(), &scratch.path().join("a")), passes).unwrap();
    assert_eq!(snapshot.unmatched, ["not/here"]);
}

#[test]
fn refuses_a_dirty_worktree_before_validating() {
    let source = source_repo();
    write(source.path(), "untracked.txt", "x\n");
    let scratch = tempfile::tempdir().unwrap();
    let out = scratch.path().join("a");
    let called = Cell::new(false);
    let result = prepare(&request(source.path(), &out), |_| {
        called.set(true);
        Ok(())
    });
    assert_eq!(refusal(result), Refusal::DirtyWorktree);
    assert!(!called.get());
    assert!(!out.exists(), "nothing is written on a refusal");

    fs::remove_file(source.path().join("untracked.txt")).unwrap();
    write(source.path(), "README.md", "edited\n");
    assert_eq!(
        refusal(prepare(&request(source.path(), &out), passes)),
        Refusal::DirtyWorktree
    );
}

#[test]
fn refuses_a_ref_not_reachable_from_origin_main() {
    let source = source_repo();
    sh(source.path(), &["checkout", "--quiet", "-b", "unmerged"]);
    write(source.path(), "README.md", "unmerged\n");
    sh(source.path(), &["commit", "--quiet", "-am", "unmerged"]);
    let scratch = tempfile::tempdir().unwrap();
    let out = scratch.path().join("a");
    assert_eq!(
        refusal(prepare(&request(source.path(), &out), passes)),
        Refusal::NotOnBase
    );
    assert!(!out.exists());
}

#[test]
fn refuses_without_origin_main() {
    let source = source_repo();
    sh(
        source.path(),
        &["update-ref", "-d", "refs/remotes/origin/main"],
    );
    let scratch = tempfile::tempdir().unwrap();
    assert_eq!(
        refusal(prepare(
            &request(source.path(), &scratch.path().join("a")),
            passes
        )),
        Refusal::MissingBase
    );
}

#[test]
fn refuses_a_ref_that_is_not_checked_out() {
    let source = source_repo();
    let scratch = tempfile::tempdir().unwrap();
    let out = scratch.path().join("a");
    let mut req = request(source.path(), &out);
    req.git_ref = "HEAD~1";
    assert_eq!(refusal(prepare(&req, passes)), Refusal::RefNotCheckedOut);
    req.git_ref = "no-such-ref";
    assert_eq!(refusal(prepare(&req, passes)), Refusal::UnknownRef);
}

#[test]
fn refuses_when_validate_fails_and_writes_nothing() {
    let source = source_repo();
    let scratch = tempfile::tempdir().unwrap();
    let out = scratch.path().join("a");
    let seen = std::cell::RefCell::new(PathBuf::new());
    let result = prepare(&request(source.path(), &out), |root| {
        *seen.borrow_mut() = root.to_path_buf();
        bail!("audit finding")
    });
    match refusal(result) {
        Refusal::ValidateFailed(detail) => assert!(detail.contains("audit finding")),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        seen.borrow().canonicalize().unwrap(),
        source.path().canonicalize().unwrap(),
        "validate reads the source worktree root"
    );
    assert!(!out.exists());
}

#[test]
fn refuses_a_non_empty_out_directory() {
    let source = source_repo();
    let scratch = tempfile::tempdir().unwrap();
    write(scratch.path(), "keep.txt", "x\n");
    assert_eq!(
        refusal(prepare(&request(source.path(), scratch.path()), passes)),
        Refusal::OutNotEmpty
    );
    assert!(scratch.path().join("keep.txt").is_file());
}

#[test]
fn refuses_a_ref_without_the_manifest() {
    let source = source_repo();
    sh(source.path(), &["rm", "--quiet", EXCLUDE_MANIFEST]);
    sh(source.path(), &["commit", "--quiet", "-m", "drop manifest"]);
    sh(
        source.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    let scratch = tempfile::tempdir().unwrap();
    assert_eq!(
        refusal(prepare(
            &request(source.path(), &scratch.path().join("a")),
            passes
        )),
        Refusal::MissingManifest
    );
}

#[test]
fn refuses_a_path_outside_the_repository() {
    let source = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    assert_eq!(
        refusal(prepare(
            &request(source.path(), &scratch.path().join("a")),
            passes
        )),
        Refusal::NotARepository
    );
}

#[test]
fn manifest_entries_must_stay_inside_the_repository() {
    assert_eq!(
        parse_manifest("# c\n\n a/b/ \nc.md\n").unwrap(),
        ["a/b", "c.md"]
    );
    for bad in ["/etc", "../x", "a/../b", "a//b", "./a", "a\\b", "/"] {
        assert!(
            matches!(parse_manifest(bad), Err(Refusal::BadManifestEntry(_))),
            "{bad}"
        );
    }
}

#[test]
fn an_entry_excludes_a_file_or_a_whole_directory_but_not_a_sibling_prefix() {
    let entries = vec!["deployments/private-row".to_string()];
    assert!(excluded("deployments/private-row", &entries).is_some());
    assert!(excluded("deployments/private-row/config.toml", &entries).is_some());
    assert!(excluded("deployments/private-row-two.toml", &entries).is_none());
}

#[test]
fn the_checked_in_manifest_parses() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let text = fs::read_to_string(root.join(EXCLUDE_MANIFEST)).expect("manifest is checked in");
    parse_manifest(&text).expect("manifest parses");
}

#[test]
fn every_checked_in_manifest_entry_names_a_tracked_path() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let text = fs::read_to_string(root.join(EXCLUDE_MANIFEST)).expect("manifest is checked in");
    let entries = parse_manifest(&text).expect("manifest parses");
    let tracked = sh(root, &["ls-files"]);
    for (index, entry) in entries.iter().enumerate() {
        assert!(
            tracked
                .lines()
                .any(|path| excluded(path, &entries) == Some(index)),
            "{entry} matches no tracked path; remove it from {EXCLUDE_MANIFEST}"
        );
    }
}

#[test]
fn the_command_never_pushes_or_adds_a_remote() {
    let source = include_str!("public_snapshot.rs");
    let code = source.split("#[cfg(test)]").next().unwrap();
    for forbidden in ["\"push\"", "\"remote\"", "\"fetch\"", "\"clone\""] {
        assert!(!code.contains(forbidden), "{forbidden}");
    }
}

#[test]
fn the_redistribution_audit_states_the_license_of_record() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let manifest = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    let license = manifest
        .lines()
        .find_map(|line| line.trim().strip_prefix("license = "))
        .expect("the workspace declares a license")
        .trim_matches('"');
    let audit = fs::read_to_string(root.join("docs/licensing/redistribution-audit.md")).unwrap();
    let current = audit
        .split("## Current publication-readiness snapshot")
        .nth(1)
        .and_then(|rest| rest.split("\n## ").next())
        .expect("the audit has a current snapshot section");
    assert!(
        current.contains(&format!("`{license}`")),
        "the current snapshot must state `{license}`"
    );
    assert!(
        current.contains("### Unresolved clearance blockers"),
        "the current snapshot must enumerate its blockers"
    );
}

#[test]
fn the_checked_in_manifest_never_excludes_a_license_instrument() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let text = fs::read_to_string(root.join(EXCLUDE_MANIFEST)).expect("manifest is checked in");
    let entries = parse_manifest(&text).expect("manifest parses");
    for instrument in [
        "LICENSE",
        "LICENSE-MIT",
        "LICENSE-APACHE-2",
        "NOTICE",
        "THIRD-PARTY-NOTICES.txt",
        "CONTRIBUTORS.md",
    ] {
        assert!(
            root.join(instrument).is_file(),
            "{instrument} is checked in"
        );
        assert!(
            excluded(instrument, &entries).is_none(),
            "{EXCLUDE_MANIFEST} must not exclude {instrument}"
        );
    }
}

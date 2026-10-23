//! A removed pointer as a request to delete the live document.
//!
//! Deleting `documents/<slug>.yaml` from a Project repository and running
//! `navigator project sync` soft-deletes that document on the live Project for
//! 180 days. This module is the detection half: which live documents had a
//! pointer in the repository's history that is now gone. A live document that
//! never had a pointer is not a deletion; the sync writes its pointer instead.
//!
//! The history is read once per run, not once per document. A deletion the
//! person already confirmed is recorded beside the repository's Git data, so
//! restoring the document brings its pointer back instead of asking again.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use crate::document_sync::{DOCUMENT_DIRECTORY, POINTER_DIRECTORY, POINTER_READ_EXTENSIONS};

/// Days the live Project keeps a soft-deleted document, shown in the prompt.
pub(crate) const RETENTION_DAYS: i64 = 180;

/// Every pointer the repository carried that is now gone, and the commit that
/// carried it last.
#[derive(Debug, Default)]
pub(crate) struct RemovedPointers {
    /// Pointer paths below the checkout root (`documents/a.pdf.yaml`) that
    /// `HEAD` still carries.
    in_head: BTreeSet<String>,
    head: Option<String>,
    /// Pointer path to the newest commit that deleted it.
    deleted: BTreeMap<String, String>,
    /// `(slug, commit)` pairs whose deletion was already carried out.
    actioned: BTreeSet<(String, String)>,
}

/// The file under the repository's Git directory that remembers which
/// deletions were carried out. It is local state, never committed.
fn actioned_path(root: &Path) -> Option<std::path::PathBuf> {
    let path = git(
        root,
        &["rev-parse", "--git-path", "navigator-document-deletions"],
    )?;
    let path = std::path::PathBuf::from(path.trim());
    Some(if path.is_absolute() {
        path
    } else {
        root.join(path)
    })
}

/// Remember that the soft deletion of `slug` for the removal in `commit` was
/// carried out.
pub(crate) fn record_actioned(root: &Path, slug: &str, commit: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let Some(path) = actioned_path(root) else {
        return Ok(());
    };
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{commit} {slug}")
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).ok())
        .flatten()
}

impl RemovedPointers {
    /// Read the pointer history under `root`: `blobs/`, and the legacy
    /// `documents/` pointers committed before it. A checkout that is not a
    /// Git repository, or has no commits, has no removed pointers.
    pub(crate) fn scan(root: &Path) -> Self {
        let Some(head) = git(root, &["rev-parse", "--verify", "HEAD"]) else {
            return Self::default();
        };
        let in_head = git(
            root,
            &[
                "ls-tree",
                "-r",
                "--name-only",
                "HEAD",
                "--",
                POINTER_DIRECTORY,
                DOCUMENT_DIRECTORY,
            ],
        )
        .map(|names| names.lines().map(str::to_string).collect())
        .unwrap_or_default();
        let mut deleted = BTreeMap::new();
        if let Some(log) = git(
            root,
            &[
                "log",
                "--relative",
                "--diff-filter=D",
                "--name-only",
                "--format=%x00%H",
                "--",
                POINTER_DIRECTORY,
                DOCUMENT_DIRECTORY,
            ],
        ) {
            let mut commit = None;
            for line in log.lines().filter(|line| !line.is_empty()) {
                if let Some(sha) = line.strip_prefix('\0') {
                    commit = Some(sha.to_string());
                } else if let Some(commit) = &commit {
                    // Newest first: the first deletion seen is the latest.
                    deleted
                        .entry(line.to_string())
                        .or_insert_with(|| commit.clone());
                }
            }
        }
        let actioned = actioned_path(root)
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| {
                text.lines()
                    .filter_map(|line| line.split_once(' '))
                    .map(|(commit, slug)| (slug.to_string(), commit.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            in_head,
            head: Some(head.trim().to_string()),
            deleted,
            actioned,
        }
    }

    /// The removal of the pointer for `slug` that still asks for a deletion:
    /// the commit that removed it, unless the person already confirmed that
    /// deletion. The caller has already established that no pointer file
    /// exists for the slug now.
    pub(crate) fn pending_removal(&self, slug: &str) -> Option<&str> {
        self.last_removal(slug).filter(|commit| {
            !self
                .actioned
                .contains(&(slug.to_string(), (*commit).to_string()))
        })
    }

    /// The commit that last removed the pointer for `slug`, in either pointer
    /// spelling, whether or not a deletion followed. A pointer `HEAD` still
    /// carries but the working tree lost is removed as of `HEAD`. The Blob
    /// under `blobs/` is read before the legacy pointer under `documents/`.
    pub(crate) fn last_removal(&self, slug: &str) -> Option<&str> {
        let paths = [POINTER_DIRECTORY, DOCUMENT_DIRECTORY]
            .into_iter()
            .flat_map(|root| {
                POINTER_READ_EXTENSIONS
                    .iter()
                    .map(move |extension| format!("{root}/{slug}.{extension}"))
            });
        for path in paths {
            if self.in_head.contains(&path) {
                return self.head.as_deref();
            }
            if let Some(commit) = self.deleted.get(&path) {
                return Some(commit);
            }
        }
        None
    }
}

/// The first seven characters of a commit id.
pub(crate) fn short_commit(commit: &str) -> &str {
    &commit[..commit.len().min(7)]
}

/// The question for one deletion: the slug, kind, visibility, revision count,
/// and the purge date, with a second line when a client stops seeing it.
pub(crate) fn deletion_prompt(
    label: &str,
    slug: &str,
    kind: &str,
    visibility: &str,
    revisions: usize,
    commit: &str,
    purge_on: chrono::NaiveDate,
) -> String {
    let mut lines = vec![
        label.to_string(),
        format!(
            "  slug {slug}  kind {kind}  visibility {visibility}  {revisions} revision(s)"
        ),
        format!("  its pointer was removed in {}", short_commit(commit)),
        format!(
            "Soft-delete `{slug}` on the live Project? It can be restored until {purge_on}, then it is purged."
        ),
    ];
    if visibility == "client" {
        lines.push("The client will stop seeing it.".to_string());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_in(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.test"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(root)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn repo() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        git_in(root.path(), &["init", "--quiet"]);
        root
    }

    fn put(root: &Path, relative: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "kind: memo\n").unwrap();
    }

    fn head(root: &Path) -> String {
        git(root, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string()
    }

    #[test]
    fn a_pointer_deleted_in_a_commit_names_that_commit() {
        let root = repo();
        put(root.path(), "blobs/a/memo.pdf.yaml");
        git_in(root.path(), &["add", "."]);
        git_in(root.path(), &["commit", "--quiet", "-m", "add"]);
        git_in(root.path(), &["rm", "--quiet", "blobs/a/memo.pdf.yaml"]);
        git_in(root.path(), &["commit", "--quiet", "-m", "remove"]);
        let removed_in = head(root.path());
        put(root.path(), "unrelated.txt");
        git_in(root.path(), &["add", "."]);
        git_in(root.path(), &["commit", "--quiet", "-m", "later"]);

        let removed = RemovedPointers::scan(root.path());

        assert_eq!(
            removed.last_removal("a/memo.pdf"),
            Some(removed_in.as_str())
        );
    }

    #[test]
    fn a_pointer_the_working_tree_lost_is_removed_as_of_head() {
        let root = repo();
        put(root.path(), "blobs/a/memo.pdf.yaml");
        git_in(root.path(), &["add", "."]);
        git_in(root.path(), &["commit", "--quiet", "-m", "add"]);
        std::fs::remove_file(root.path().join("blobs/a/memo.pdf.yaml")).unwrap();

        let removed = RemovedPointers::scan(root.path());

        assert_eq!(
            removed.last_removal("a/memo.pdf"),
            Some(head(root.path()).as_str())
        );
    }

    /// A document that never had a pointer is not a deletion, and neither is
    /// a checkout with no history to read.
    #[test]
    fn a_document_that_never_had_a_pointer_is_not_removed() {
        let root = repo();
        assert_eq!(
            RemovedPointers::scan(root.path()).last_removal("a/memo.pdf"),
            None
        );
        put(root.path(), "README.md");
        git_in(root.path(), &["add", "."]);
        git_in(root.path(), &["commit", "--quiet", "-m", "add"]);
        assert_eq!(
            RemovedPointers::scan(root.path()).last_removal("a/memo.pdf"),
            None
        );
        let not_a_repo = tempfile::tempdir().unwrap();
        assert_eq!(
            RemovedPointers::scan(not_a_repo.path()).last_removal("a/memo.pdf"),
            None
        );
    }

    #[test]
    fn the_retired_yml_spelling_counts_and_a_newer_deletion_wins() {
        let root = repo();
        put(root.path(), "blobs/old.pdf.yml");
        git_in(root.path(), &["add", "."]);
        git_in(root.path(), &["commit", "--quiet", "-m", "add"]);
        git_in(root.path(), &["rm", "--quiet", "blobs/old.pdf.yml"]);
        git_in(root.path(), &["commit", "--quiet", "-m", "remove"]);
        let first = head(root.path());
        put(root.path(), "blobs/old.pdf.yaml");
        git_in(root.path(), &["add", "."]);
        git_in(root.path(), &["commit", "--quiet", "-m", "re-add"]);
        git_in(root.path(), &["rm", "--quiet", "blobs/old.pdf.yaml"]);
        git_in(root.path(), &["commit", "--quiet", "-m", "remove again"]);
        let second = head(root.path());

        let removed = RemovedPointers::scan(root.path());

        assert_eq!(removed.last_removal("old.pdf"), Some(second.as_str()));
        assert_ne!(first, second);
    }

    /// A deletion the person already carried out is remembered, so a restored
    /// document is not asked about again; a later removal is a new question.
    #[test]
    fn a_carried_out_deletion_is_not_pending_but_a_later_removal_is() {
        let root = repo();
        put(root.path(), "blobs/a/memo.pdf.yaml");
        git_in(root.path(), &["add", "."]);
        git_in(root.path(), &["commit", "--quiet", "-m", "add"]);
        git_in(root.path(), &["rm", "--quiet", "blobs/a/memo.pdf.yaml"]);
        git_in(root.path(), &["commit", "--quiet", "-m", "remove"]);
        let first = head(root.path());
        assert_eq!(
            RemovedPointers::scan(root.path()).pending_removal("a/memo.pdf"),
            Some(first.as_str())
        );

        record_actioned(root.path(), "a/memo.pdf", &first).unwrap();

        let after = RemovedPointers::scan(root.path());
        assert_eq!(after.pending_removal("a/memo.pdf"), None);
        assert_eq!(
            after.last_removal("a/memo.pdf"),
            Some(first.as_str()),
            "leftover bytes still know their pointer was removed"
        );
        put(root.path(), "blobs/a/memo.pdf.yaml");
        git_in(root.path(), &["add", "."]);
        git_in(
            root.path(),
            &["commit", "--quiet", "-m", "restore the pointer"],
        );
        git_in(root.path(), &["rm", "--quiet", "blobs/a/memo.pdf.yaml"]);
        git_in(root.path(), &["commit", "--quiet", "-m", "remove again"]);
        let second = head(root.path());
        assert_eq!(
            RemovedPointers::scan(root.path()).pending_removal("a/memo.pdf"),
            Some(second.as_str())
        );
    }

    #[test]
    fn the_prompt_names_the_facts_and_warns_about_a_client() {
        let purge_on = chrono::NaiveDate::from_ymd_opt(2027, 3, 31).unwrap();
        let internal = deletion_prompt(
            "documents/a/memo.pdf",
            "a/memo.pdf",
            "memo",
            "internal",
            3,
            "3f87afb5c0ffee",
            purge_on,
        );
        for fact in [
            "documents/a/memo.pdf",
            "kind memo",
            "visibility internal",
            "3 revision(s)",
            "3f87afb",
            "2027-03-31",
        ] {
            assert!(internal.contains(fact), "{fact} missing from:\n{internal}");
        }
        assert!(!internal.contains("client will stop"));
        let client = deletion_prompt("d", "s", "memo", "client", 1, "abc1234", purge_on);
        assert!(client.contains("The client will stop seeing it."));
    }
}

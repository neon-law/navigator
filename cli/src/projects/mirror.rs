//! The Forgejo mirror's `main` against the authoritative repository's.
//!
//! A release lands on the authoritative forge and the mirror follows. The
//! relation of the mirror to the authoritative `main` is one of four words:
//! `equal`, `behind` (the mirror lacks commits the authoritative has, so it
//! can fast-forward), `ahead` (the mirror holds commits the authoritative
//! lacks), or `diverged`. Only `behind` is ever moved, and never by force:
//! an `ahead` or `diverged` mirror is preserved and reported with both SHAs.

use std::path::Path;

use anyhow::{bail, ensure, Context, Result};
use serde::Serialize;

use super::lifecycle::{git, git_text};

/// The mirror's `main` relative to the authoritative `main`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Relation {
    Equal,
    Behind,
    Ahead,
    Diverged,
}

impl Relation {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Equal => "equal",
            Self::Behind => "behind",
            Self::Ahead => "ahead",
            Self::Diverged => "diverged",
        }
    }
}

/// What one pass over a mirror did, and what it read back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Parity {
    pub authoritative: String,
    /// The mirror's `main` before the pass; `None` when it has none.
    pub mirror: Option<String>,
    pub relation: Option<Relation>,
    /// `none`, `fast-forwarded`, `preserved`, or `missing`.
    pub action: &'static str,
    /// The mirror's `main` after the pass, read back from the mirror.
    pub after: Option<String>,
}

impl Parity {
    /// Whether the mirror's `main` now equals the authoritative one.
    #[must_use]
    pub fn in_parity(&self) -> bool {
        self.after.as_deref() == Some(self.authoritative.as_str())
    }

    /// Why the mirror was left as it is, naming both sides, or `None` when it
    /// is in parity.
    #[must_use]
    pub fn explanation(&self) -> Option<String> {
        if self.in_parity() {
            return None;
        }
        let mirror = self.mirror.as_deref().unwrap_or("none");
        Some(match self.relation {
            None => format!(
                "the mirror has no main; authoritative main is {}. Import the source instead of fast-forwarding",
                self.authoritative
            ),
            Some(relation) => format!(
                "mirror main {mirror} is {} authoritative main {}; it was preserved, not overwritten",
                relation.as_str(),
                self.authoritative
            ),
        })
    }
}

/// Whether `ancestor` is an ancestor of `descendant`. Both objects must be in
/// the local object store.
fn is_ancestor(dir: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let output = git(dir, &["merge-base", "--is-ancestor", ancestor, descendant])?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!("could not compare {ancestor} with {descendant}"),
    }
}

/// The mirror's relation to the authoritative `main`. Both commits must be in
/// `dir`'s object store.
pub fn classify(dir: &Path, authoritative: &str, mirror: &str) -> Result<Relation> {
    if authoritative == mirror {
        return Ok(Relation::Equal);
    }
    Ok(if is_ancestor(dir, mirror, authoritative)? {
        Relation::Behind
    } else if is_ancestor(dir, authoritative, mirror)? {
        Relation::Ahead
    } else {
        Relation::Diverged
    })
}

/// The commit `reference` points at on `url`, or `None` when it has none.
pub fn remote_ref(dir: &Path, url: &str, reference: &str) -> Result<Option<String>> {
    let output = git_text(dir, &["ls-remote", "--refs", "--", url, reference])
        .with_context(|| format!("read {reference} on a repository"))?;
    let Some((sha, found)) = output.trim().split_once('\t') else {
        return Ok(None);
    };
    ensure!(
        found == reference
            && matches!(sha.len(), 40 | 64)
            && sha.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "a repository advertised an invalid ref"
    );
    Ok(Some(sha.to_owned()))
}

/// Bring `reference`'s objects from `url` into `dir`, writing no ref.
pub fn fetch_objects(dir: &Path, url: &str, reference: &str) -> Result<()> {
    git_text(
        dir,
        &[
            "fetch",
            "--no-tags",
            "--no-write-fetch-head",
            "--refmap=",
            "--",
            url,
            reference,
        ],
    )
    .with_context(|| format!("fetch {reference} from a repository"))?;
    Ok(())
}

/// Read both mains and say how the mirror stands, moving nothing.
pub fn inspect(
    dir: &Path,
    authoritative_url: &str,
    mirror_url: &str,
    reference: &str,
) -> Result<Parity> {
    let authoritative = remote_ref(dir, authoritative_url, reference)?
        .with_context(|| format!("the authoritative repository has no {reference}"))?;
    let Some(mirror) = remote_ref(dir, mirror_url, reference)? else {
        return Ok(Parity {
            authoritative,
            mirror: None,
            relation: None,
            action: "missing",
            after: None,
        });
    };
    let relation = if authoritative == mirror {
        Relation::Equal
    } else {
        fetch_objects(dir, authoritative_url, reference)?;
        fetch_objects(dir, mirror_url, reference)?;
        classify(dir, &authoritative, &mirror)?
    };
    Ok(Parity {
        after: Some(mirror.clone()),
        authoritative,
        mirror: Some(mirror),
        relation: Some(relation),
        action: "none",
    })
}

/// Fast-forward a mirror that is `behind`, then read both refs back. Never
/// forces: an `ahead` or `diverged` mirror, or one with no `main`, is left
/// untouched and described by the returned [`Parity`].
pub fn fast_forward(
    dir: &Path,
    authoritative_url: &str,
    mirror_url: &str,
    reference: &str,
) -> Result<Parity> {
    let mut parity = inspect(dir, authoritative_url, mirror_url, reference)?;
    match parity.relation {
        Some(Relation::Behind) => {
            let refspec = format!("{}:{reference}", parity.authoritative);
            git_text(
                dir,
                &[
                    "-c",
                    "http.followRedirects=false",
                    "push",
                    "--no-follow-tags",
                    "--no-force",
                    "--",
                    mirror_url,
                    &refspec,
                ],
            )
            .context("the mirror rejected the fast-forward; check its access, branch protection and ref divergence")?;
            let after = remote_ref(dir, mirror_url, reference)?;
            let authoritative_after = remote_ref(dir, authoritative_url, reference)?;
            ensure!(
                after.as_deref() == Some(parity.authoritative.as_str())
                    && authoritative_after.as_deref() == Some(parity.authoritative.as_str()),
                "the mirror's main did not read back as the authoritative merge {}",
                parity.authoritative
            );
            parity.after = after;
            parity.action = "fast-forwarded";
        }
        Some(Relation::Ahead | Relation::Diverged) => parity.action = "preserved",
        Some(Relation::Equal) | None => {}
    }
    Ok(parity)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) -> String {
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

    /// An authoritative and a mirror bare repository that both start at one
    /// commit, and a clone to build further commits in.
    struct Fleet {
        _root: tempfile::TempDir,
        authoritative: String,
        mirror: String,
        work: std::path::PathBuf,
    }

    impl Fleet {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let base = root.path();
            let authoritative = base.join("authoritative.git");
            let mirror = base.join("mirror.git");
            let work = base.join("work");
            for bare in [&authoritative, &mirror] {
                std::fs::create_dir(bare).unwrap();
                run(
                    bare,
                    &["init", "--quiet", "--bare", "--initial-branch=main"],
                );
            }
            std::fs::create_dir(&work).unwrap();
            run(&work, &["init", "--quiet", "--initial-branch=main"]);
            let fleet = Self {
                authoritative: authoritative.to_str().unwrap().to_owned(),
                mirror: mirror.to_str().unwrap().to_owned(),
                work,
                _root: root,
            };
            fleet.commit("one");
            fleet.push(&fleet.authoritative);
            fleet.push(&fleet.mirror);
            fleet
        }

        fn commit(&self, name: &str) -> String {
            std::fs::write(self.work.join(name), name).unwrap();
            run(&self.work, &["add", name]);
            run(&self.work, &["commit", "--quiet", "-m", name]);
            run(&self.work, &["rev-parse", "HEAD"])
        }

        fn push(&self, url: &str) {
            run(
                &self.work,
                &["push", "--quiet", "--force", url, "HEAD:refs/heads/main"],
            );
        }

        fn rewind(&self, sha: &str) {
            run(&self.work, &["reset", "--quiet", "--hard", sha]);
        }

        fn head(&self, url: &str) -> String {
            remote_ref(&self.work, url, "refs/heads/main")
                .unwrap()
                .unwrap()
        }
    }

    #[test]
    fn a_mirror_at_the_same_commit_is_equal_and_untouched() {
        let fleet = Fleet::new();
        let parity = fast_forward(
            &fleet.work,
            &fleet.authoritative,
            &fleet.mirror,
            "refs/heads/main",
        )
        .unwrap();
        assert_eq!(parity.relation, Some(Relation::Equal));
        assert_eq!(parity.action, "none");
        assert!(parity.in_parity());
        assert_eq!(parity.explanation(), None);
    }

    #[test]
    fn a_mirror_behind_is_fast_forwarded_and_both_refs_read_back() {
        let fleet = Fleet::new();
        let older = fleet.head(&fleet.mirror);
        let merge = fleet.commit("two");
        fleet.push(&fleet.authoritative);
        assert_eq!(fleet.head(&fleet.mirror), older);

        let parity = fast_forward(
            &fleet.work,
            &fleet.authoritative,
            &fleet.mirror,
            "refs/heads/main",
        )
        .unwrap();
        assert_eq!(parity.relation, Some(Relation::Behind));
        assert_eq!(parity.action, "fast-forwarded");
        assert_eq!(parity.mirror.as_deref(), Some(older.as_str()));
        assert_eq!(parity.after.as_deref(), Some(merge.as_str()));
        assert!(parity.in_parity());
        assert_eq!(fleet.head(&fleet.mirror), merge);
        assert_eq!(fleet.head(&fleet.authoritative), merge);
    }

    #[test]
    fn an_ahead_or_diverged_mirror_is_preserved_and_reported_with_both_shas() {
        // Ahead: the mirror holds a commit the authoritative lacks.
        let fleet = Fleet::new();
        let shared = fleet.head(&fleet.authoritative);
        let extra = fleet.commit("mirror-only");
        fleet.push(&fleet.mirror);
        let parity = fast_forward(
            &fleet.work,
            &fleet.authoritative,
            &fleet.mirror,
            "refs/heads/main",
        )
        .unwrap();
        assert_eq!(parity.relation, Some(Relation::Ahead));
        assert_eq!(parity.action, "preserved");
        assert_eq!(fleet.head(&fleet.mirror), extra, "never overwritten");
        let why = parity.explanation().unwrap();
        assert!(why.contains(&extra) && why.contains(&shared), "{why}");
        assert!(why.contains("ahead"), "{why}");

        // Diverged: each side has its own commit.
        let fleet = Fleet::new();
        let base = fleet.head(&fleet.authoritative);
        fleet.commit("mirror-side");
        fleet.push(&fleet.mirror);
        fleet.rewind(&base);
        let theirs = fleet.commit("authoritative-side");
        fleet.push(&fleet.authoritative);
        let mirror_before = fleet.head(&fleet.mirror);
        let parity = fast_forward(
            &fleet.work,
            &fleet.authoritative,
            &fleet.mirror,
            "refs/heads/main",
        )
        .unwrap();
        assert_eq!(parity.relation, Some(Relation::Diverged));
        assert_eq!(parity.action, "preserved");
        assert_eq!(
            fleet.head(&fleet.mirror),
            mirror_before,
            "never overwritten"
        );
        assert_eq!(fleet.head(&fleet.authoritative), theirs);
        assert!(!parity.in_parity());
    }

    #[test]
    fn a_mirror_with_no_main_is_reported_not_created() {
        let fleet = Fleet::new();
        let empty = fleet.mirror.replace("mirror.git", "empty.git");
        std::fs::create_dir(&empty).unwrap();
        run(
            Path::new(&empty),
            &["init", "--quiet", "--bare", "--initial-branch=main"],
        );
        let parity =
            fast_forward(&fleet.work, &fleet.authoritative, &empty, "refs/heads/main").unwrap();
        assert_eq!(parity.action, "missing");
        assert_eq!(parity.relation, None);
        assert!(parity.explanation().unwrap().contains("no main"));
        assert!(remote_ref(&fleet.work, &empty, "refs/heads/main")
            .unwrap()
            .is_none());
    }

    #[test]
    fn inspecting_reads_but_never_moves_a_mirror_that_is_behind() {
        let fleet = Fleet::new();
        let older = fleet.head(&fleet.mirror);
        fleet.commit("two");
        fleet.push(&fleet.authoritative);
        let parity = inspect(
            &fleet.work,
            &fleet.authoritative,
            &fleet.mirror,
            "refs/heads/main",
        )
        .unwrap();
        assert_eq!(parity.relation, Some(Relation::Behind));
        assert_eq!(parity.action, "none");
        assert_eq!(fleet.head(&fleet.mirror), older);
    }

    #[test]
    fn a_missing_authoritative_main_is_an_error() {
        let fleet = Fleet::new();
        let nowhere = fleet.mirror.replace("mirror.git", "nowhere.git");
        std::fs::create_dir(&nowhere).unwrap();
        run(Path::new(&nowhere), &["init", "--quiet", "--bare"]);
        let error = inspect(&fleet.work, &nowhere, &fleet.mirror, "refs/heads/main").unwrap_err();
        assert!(error.to_string().contains("no refs/heads/main"), "{error}");
    }
}

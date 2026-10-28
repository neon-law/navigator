//! `navigator ops cut-release` names the next release, compares it with
//! published tags, and writes the workspace version. The name is today's UTC
//! `YY.M.D` until that is published, then the next unpublished date's
//! `YY.M.D-rc.N`; [`crate::release::default_tag`] holds the rule.
//!
//! The clock is sampled once at the CLI boundary. The decision uses that date
//! and the tag list; the writer uses the selected repository's root manifest.
//! `--dry-run` prints the candidate without writing release files. A tag far
//! ahead of the calendar or an operational error exits 2. `ops release-default-tag` is the
//! read-only probe whose covered-date result exits 0.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};

use crate::release_check::{fetch_tags, release_tags};

/// What the next release is called against the published tags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Newer than every published release: today's `YY.M.D`, or a later
    /// date's `YY.M.D-rc.N` once today is published.
    Cut(String),
    /// No date within the lookahead clears the published releases.
    AlreadyCovered {
        today: String,
        published: Option<String>,
    },
}

/// Name the next release, or report that nothing clears the published tags.
#[must_use]
pub(crate) fn decide(now: DateTime<Utc>, tags: &[String]) -> Decision {
    let today = crate::release::today_tag(now);
    match crate::release::default_tag(now, tags) {
        Some(version) => Decision::Cut(version.to_string()),
        None => Decision::AlreadyCovered {
            today,
            published: crate::release::highest_release(tags).map(|version| version.to_string()),
        },
    }
}

/// Entry point for `ops cut-release`.
pub fn run(
    now: DateTime<Utc>,
    repo: &Path,
    fetch: bool,
    manifest_path: &Path,
    no_commit: bool,
    dry_run: bool,
) -> ExitCode {
    match decide_from_repo(now, repo, fetch) {
        Ok(Decision::Cut(tag)) if dry_run => {
            println!("{tag}");
            eprintln!("navigator: cut-release: --dry-run: would write {tag} and commit it");
            ExitCode::SUCCESS
        }
        Ok(Decision::Cut(tag)) => {
            println!("navigator: cut-release: the next release is {tag}");
            match workspace_manifest(repo, manifest_path) {
                Ok(manifest) => crate::release_version::run(&manifest, &tag, no_commit),
                Err(error) => {
                    eprintln!("navigator: cut-release: {error:#}");
                    ExitCode::from(2)
                }
            }
        }
        Ok(Decision::AlreadyCovered { today, published }) => {
            match published {
                Some(published) => eprintln!(
                    "navigator: cut-release: today's UTC date names {today}, which is not a new \
                     release — {published} is already published"
                ),
                None => eprintln!(
                    "navigator: cut-release: today's UTC date names {today}, which is not a new \
                     release"
                ),
            }
            ExitCode::from(2)
        }
        Err(error) => {
            eprintln!("navigator: cut-release: {error:#}");
            ExitCode::from(2)
        }
    }
}

/// The tags, manifest, pins, lockfile, and commit belong to one worktree.
fn workspace_manifest(repo: &Path, manifest_path: &Path) -> Result<PathBuf> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("resolve the release worktree")?;
    if !output.status.success() {
        bail!("could not resolve the release worktree");
    }
    let root = PathBuf::from(String::from_utf8(output.stdout)?.trim())
        .canonicalize()
        .context("resolve the release worktree root")?;
    let manifest = root
        .join(manifest_path)
        .canonicalize()
        .context("resolve the workspace manifest")?;
    if manifest != root.join("Cargo.toml") {
        bail!("--manifest-path must name the root Cargo.toml in --repo");
    }
    Ok(manifest)
}

fn decide_from_repo(now: DateTime<Utc>, repo: &Path, fetch: bool) -> Result<Decision> {
    if fetch {
        fetch_tags(repo).context("fetch the release tags")?;
    }
    let tags = release_tags(repo).context("list the release tags")?;
    Ok(decide(now, &tags))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(year: i32, month: u32, day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, 12, 0, 0)
            .single()
            .expect("a valid calendar date")
    }

    fn tags(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    #[test]
    fn today_is_cuttable_when_nothing_is_published() {
        assert_eq!(
            decide(utc(2026, 9, 24), &[]),
            Decision::Cut("26.9.24".to_string())
        );
    }

    #[test]
    fn today_is_cuttable_when_it_is_newer_than_every_release() {
        assert_eq!(
            decide(utc(2026, 9, 24), &tags(&["26.9.23", "26.9.23-rc.1"])),
            Decision::Cut("26.9.24".to_string())
        );
    }

    /// A published prerelease of today does not consume the ordinary name:
    /// semver ranks `26.9.24-rc.1` below `26.9.24`.
    #[test]
    fn a_prerelease_of_today_does_not_block_the_ordinary_cut() {
        assert_eq!(
            decide(utc(2026, 9, 24), &tags(&["26.9.24-rc.1"])),
            Decision::Cut("26.9.24".to_string())
        );
    }

    /// Once today is published, the cut is tomorrow's next candidate.
    #[test]
    fn cuts_tomorrows_candidate_when_today_is_already_published() {
        assert_eq!(
            decide(utc(2026, 9, 24), &tags(&["26.9.24", "26.9.25-rc.1"])),
            Decision::Cut("26.9.25-rc.2".to_string())
        );
    }

    #[test]
    fn cuts_a_candidate_past_a_later_published_daily() {
        assert_eq!(
            decide(utc(2026, 9, 24), &tags(&["26.9.25"])),
            Decision::Cut("26.9.26-rc.1".to_string())
        );
    }

    #[test]
    fn names_the_highest_published_version_in_the_covered_decision() {
        assert_eq!(
            decide(utc(2026, 9, 24), &tags(&["26.9.23", "99.12.31"])),
            Decision::AlreadyCovered {
                today: "26.9.24".to_string(),
                published: Some("99.12.31".to_string()),
            }
        );
    }
}

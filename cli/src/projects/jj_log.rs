//! `navigator project jj log` — the lawyer-facing view of a checkout's
//! Jujutsu history.
//!
//! The colocated store tracks every edit, but `jj log` itself speaks in
//! templates, graphs, and commit ids. This command renders the same history
//! in lawyer terms: each change's description, its author, when it was
//! made, and an explicit warning when the change is conflicted. No Git
//! concepts and no jj grammar — a conflict never reads as jargon, and a
//! commit SHA is never rendered, because the template never asks for one.

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

/// The machine template behind the lawyer-facing view: one record per
/// change, fields joined by the unit separator, in the order
/// [`ChangeEntry`] parses. It deliberately omits `commit_id` — the Git
/// SHA cannot leak into output that never reads it.
const RECORD_TEMPLATE: &str = r#"change_id.short() ++ "\x1f" ++ description.first_line() ++ "\x1f" ++ author.name() ++ "\x1f" ++ committer.timestamp() ++ "\x1f" ++ if(empty, "empty", "filled") ++ "\x1f" ++ if(conflict, "conflicted", "clean") ++ "\n""#;

/// The wording the view gives a conflicted change. Stated as an action for
/// the reader, never as jj or Git plumbing.
const CONFLICT_WARNING: &str = "⚠ conflicted — resolve before sending";

/// One change as a lawyer reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeEntry {
    pub change_id: String,
    pub description: String,
    pub author: String,
    pub timestamp: String,
    pub empty: bool,
    pub conflicted: bool,
}

/// Print the checkout's history in lawyer terms.
///
/// The checkout must already be tracked by jj (`navigator project
/// repository jj`); this command installs nothing and runs on any host.
pub fn run(dir: &Path) -> Result<()> {
    if !dir.join(".jj").is_dir() {
        anyhow::bail!(
            "{} is not tracked by Jujutsu yet; run `navigator project repository jj` first",
            dir.display()
        );
    }

    let output = Command::new("jj")
        .current_dir(dir)
        .args([
            "log",
            "--no-graph",
            "--no-pager",
            "--template",
            RECORD_TEMPLATE,
        ])
        .output()
        .context("run jj log")?;
    if !output.status.success() {
        anyhow::bail!(
            "jj log failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let entries = parse_log_records(&String::from_utf8_lossy(&output.stdout));
    print!("{}", render_log(&entries));
    Ok(())
}

/// Parse `jj log --template {RECORD_TEMPLATE}` output into change entries.
///
/// Records are one per line with exactly six unit-separated fields — a
/// separator cannot appear in any of them, so a line that does not split
/// into six is stray notice output, not a record, and is skipped. The empty,
/// undescribed changes jj's commit workflow leaves along the way carry no
/// signal for a lawyer and are dropped — but a conflicted change is always
/// kept, whatever its other fields say.
#[must_use]
pub fn parse_log_records(raw: &str) -> Vec<ChangeEntry> {
    raw.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\u{1f}').collect();
            if fields.len() != 6 {
                return None;
            }
            let entry = ChangeEntry {
                change_id: fields[0].trim().to_string(),
                description: fields[1].trim().to_string(),
                author: fields[2].trim().to_string(),
                timestamp: fields[3].trim().to_string(),
                empty: fields[4] == "empty",
                conflicted: fields[5] == "conflicted",
            };
            (entry.conflicted || !(entry.empty && entry.description.is_empty())).then_some(entry)
        })
        .collect()
}

/// Render change entries as the lawyer-facing view: a headline per change,
/// its author and time beneath, and the conflict warning when present.
#[must_use]
pub fn render_log(entries: &[ChangeEntry]) -> String {
    let mut out = String::new();
    for entry in entries {
        let headline = if entry.description.is_empty() {
            "(no description)"
        } else {
            &entry.description
        };
        let author = if entry.author.is_empty() {
            "(unknown)"
        } else {
            &entry.author
        };
        out.push_str(headline);
        out.push('\n');
        let _ = writeln!(
            out,
            "  change {} · {author} · {}",
            entry.change_id,
            brief_timestamp(&entry.timestamp)
        );
        if entry.conflicted {
            let _ = writeln!(out, "  {CONFLICT_WARNING}");
        }
        out.push('\n');
    }
    out
}

/// `committer.timestamp()` renders `2026-11-10 21:40:36.000 +00:00`; the
/// seconds already carry the moment, so the view keeps the local time and
/// drops the fraction and offset. A shorter or differently shaped timestamp
/// passes through untouched.
fn brief_timestamp(raw: &str) -> String {
    match raw.split_once('.') {
        Some((seconds, _)) => seconds.to_string(),
        None => raw.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real `jj log --template {RECORD_TEMPLATE}` output from a colocated
    /// checkout (jj 0.47.1): a merge conflict, two clean changes, and the
    /// empty plumbing changes jj's workflow leaves along the way.
    fn fixture_log() -> String {
        [
            "swwomtxqxqwm\u{1f}merged\u{1f}Ada Lawyer\u{1f}2026-11-10 21:40:36.000 +00:00\u{1f}empty\u{1f}conflicted",
            "qzxyrmqwsxzw\u{1f}right\u{1f}Ada Lawyer\u{1f}2026-11-10 21:40:35.000 +00:00\u{1f}filled\u{1f}clean",
            "nrszvlulpzwv\u{1f}left\u{1f}Ada Lawyer\u{1f}2026-11-10 21:40:35.000 +00:00\u{1f}filled\u{1f}clean",
            "mokmnrmvszqr\u{1f}\u{1f}\u{1f}2026-11-10 21:39:28.000 +00:00\u{1f}empty\u{1f}clean",
            "zzzzzzzzzzzz\u{1f}\u{1f}\u{1f}1970-01-01 00:00:00.000 +00:00\u{1f}empty\u{1f}clean",
        ]
        .join("\n")
    }

    #[test]
    fn fixture_records_parse_into_change_entries() {
        let entries = parse_log_records(&fixture_log());
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].change_id, "swwomtxqxqwm");
        assert_eq!(entries[0].description, "merged");
        assert_eq!(entries[0].author, "Ada Lawyer");
        assert!(entries[0].conflicted);
        assert!(!entries[1].conflicted);
        assert_eq!(entries[2].description, "left");
    }

    #[test]
    fn empty_undescribed_plumbing_changes_are_dropped() {
        let entries = parse_log_records(&fixture_log());
        assert!(
            entries
                .iter()
                .all(|entry| entry.conflicted || !entry.description.is_empty()),
            "every kept entry is described or conflicted"
        );
    }

    #[test]
    fn a_conflicted_change_is_never_dropped_even_when_undescribed() {
        let raw = "abcdefgqwxlm\u{1f}\u{1f}\u{1f}2026-11-10 21:40:36.000 +00:00\u{1f}empty\u{1f}conflicted\n";
        let entries = parse_log_records(raw);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].conflicted);
    }

    #[test]
    fn stray_lines_without_six_fields_are_skipped() {
        let raw = format!("warning: nothing here\n{}\n", fixture_log());
        assert_eq!(parse_log_records(&raw).len(), 3);
    }

    #[test]
    fn the_view_renders_in_lawyer_terms() {
        let rendered = render_log(&parse_log_records(&fixture_log()));
        assert_eq!(
            rendered,
            "merged\n  change swwomtxqxqwm · Ada Lawyer · 2026-11-10 21:40:36\n  \
             ⚠ conflicted — resolve before sending\n\n\
             right\n  change qzxyrmqwsxzw · Ada Lawyer · 2026-11-10 21:40:35\n\n\
             left\n  change nrszvlulpzwv · Ada Lawyer · 2026-11-10 21:40:35\n\n"
        );
    }

    #[test]
    fn the_default_view_names_no_commit_sha() {
        // The fixture's Git commit ids, exactly as jj's own log would show
        // them (4f35bf9b, 390dc8b8, f532581e…). The view renders change ids,
        // so none may appear.
        let rendered = render_log(&parse_log_records(&fixture_log()));
        for sha in ["4f35bf9b", "390dc8b8", "f532581e", "990ecc98", "d2044c8b"] {
            assert!(!rendered.contains(sha), "a Git SHA leaked into the view");
        }
    }

    #[test]
    fn a_change_without_description_or_author_still_reads_clearly() {
        let entry = ChangeEntry {
            change_id: "qzxyrmqwsxzw".into(),
            description: String::new(),
            author: String::new(),
            timestamp: "2026-11-10 21:40:35.000 +00:00".into(),
            empty: false,
            conflicted: false,
        };
        let rendered = render_log(&[entry]);
        assert!(rendered.contains("(no description)"));
        assert!(rendered.contains("(unknown)"));
    }

    #[test]
    fn timestamps_lose_their_fraction_and_offset() {
        assert_eq!(
            brief_timestamp("2026-11-10 21:40:36.000 +00:00"),
            "2026-11-10 21:40:36"
        );
        assert_eq!(brief_timestamp("unknown shape"), "unknown shape");
    }
}

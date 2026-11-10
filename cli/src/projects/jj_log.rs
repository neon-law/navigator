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
pub(crate) struct ChangeEntry {
    pub(crate) change_id: String,
    pub(crate) description: String,
    pub(crate) author: String,
    pub(crate) timestamp: String,
    pub(crate) empty: bool,
    pub(crate) conflicted: bool,
}

/// Print the checkout's history in lawyer terms.
///
/// The checkout must already be tracked by jj (`navigator project
/// repository jj`); this command installs nothing and runs on any host.
pub(crate) fn run(dir: &Path) -> Result<()> {
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
pub(crate) fn parse_log_records(raw: &str) -> Vec<ChangeEntry> {
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
pub(crate) fn render_log(entries: &[ChangeEntry]) -> String {
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

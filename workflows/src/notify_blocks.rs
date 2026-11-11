//! Push analyses rendered as Slack Block Kit blocks.
//!
//! The push pipeline's delivery stage: a pure function from the
//! journal's persisted [`PushAnalysis`]
//! (`cloud::forgejo_push_analysis::PushAnalysis`) to the Block Kit
//! message the Project's Slack channel reads — a header naming the
//! Project and the pushed change, a section per change with its
//! description and diff statistics, the conflict warning when the
//! analysis signaled one, and a context block carrying the author, the
//! push time, and — for operators only — the link to the change's
//! Forgejo commit page. Lawyers consume Navigator's surface, not Slack
//! links, so the link rides the context block and nothing else.
//!
//! An uploaded image, when the delivery stage uploaded one, is
//! referenced by its `slack_file` id — never by a URL — so confidential
//! content stays inside Slack. The blocks degrade to readable plain
//! text through [`cloud::blocks_to_fallback_text`], the fallback
//! `SlackBot::post_blocks` applies when an implementor cannot send
//! blocks; the tests hold that degradation readable rather than
//! reimplementing a second extractor here.

use cloud::forgejo_push_analysis::{FileStats, PushAnalysis};
use serde_json::Value;
/// The warning a conflicted change carries, word for word the warning
/// `cli::projects::jj_log` renders beside the change itself — one
/// conflict reads one way across Navigator's surfaces.
pub const CONFLICT_BADGE: &str = "⚠ conflicted — resolve before sending";

/// One pushed change, with the push-level facts Slack renders but the
/// analysis columns don't persist.
pub struct ChangeNotice<'a> {
    pub analysis: &'a PushAnalysis,
    /// The change's Forgejo commit page, for operators.
    pub commit_url: Option<&'a str>,
    /// The uploaded image's `slack_file` id, when the delivery stage
    /// uploaded one.
    pub image_file_id: Option<&'a str>,
}

/// The push a message renders: the Project it landed on, when it was
/// received, and the pushed changes in push order. The pipeline analyzes
/// one change per receipt today; several render as one section each.
pub struct PushNotice<'a> {
    pub project_code: &'a str,
    pub timestamp: &'a str,
    pub changes: Vec<ChangeNotice<'a>>,
}

/// Render a push as Block Kit blocks: a header naming the Project and
/// its sole change — or the change count when a push carried several —
/// then a section, conflict badge, and context per change.
#[must_use]
pub fn push_blocks(notice: &PushNotice<'_>) -> Vec<Value> {
    let header_text = match notice.changes.as_slice() {
        [sole] => format!(
            "Project {} · change {}",
            notice.project_code, sole.analysis.change_id
        ),
        changes => format!(
            "Project {} · {} changes",
            notice.project_code,
            changes.len()
        ),
    };
    let mut blocks = vec![serde_json::json!({
        "type": "header",
        "text": { "type": "plain_text", "text": header_text },
    })];
    for change in &notice.changes {
        blocks.extend(change_blocks(notice, change));
    }
    blocks
}

/// One change's section, conflict badge, context, and image.
fn change_blocks(notice: &PushNotice<'_>, change: &ChangeNotice<'_>) -> Vec<Value> {
    let analysis = change.analysis;
    let mut blocks = vec![section(analysis)];
    if analysis.conflict_signal {
        blocks.push(serde_json::json!({
            "type": "section",
            "text": { "type": "mrkdwn", "text": CONFLICT_BADGE },
        }));
    }
    blocks.push(context(notice, change));
    if let Some(file_id) = change.image_file_id {
        blocks.push(serde_json::json!({
            "type": "image",
            "slack_file": { "id": file_id },
            "alt_text": format!("Change {} summary", analysis.change_id),
        }));
    }
    blocks
}

/// The change's summary section: the description's first line, and the
/// diff statistics in lawyer terms — lines, not Git vocabulary.
fn section(analysis: &PushAnalysis) -> Value {
    serde_json::json!({
        "type": "section",
        "fields": [
            { "type": "mrkdwn", "text": format!("*{}*", analysis.description_summary) },
            { "type": "mrkdwn", "text": render_stats(analysis.stats) },
        ],
    })
}

/// The diff statistics a lawyer reads: what was added, what was removed,
/// and across how many files.
#[must_use]
pub fn render_stats(stats: FileStats) -> String {
    let files = if stats.files == 1 {
        "1 file".to_owned()
    } else {
        format!("{} files", stats.files)
    };
    format!(
        "{} lines added, {} removed across {}",
        stats.insertions, stats.deletions, files
    )
}

/// Author, push time, and — for operators — the Forgejo commit link.
fn context(notice: &PushNotice<'_>, change: &ChangeNotice<'_>) -> Value {
    let mut elements = vec![serde_json::json!({
        "type": "mrkdwn",
        "text": format!("by {} · {}", change.analysis.author, notice.timestamp),
    })];
    if let Some(url) = change.commit_url {
        elements.push(serde_json::json!({
            "type": "mrkdwn",
            "text": format!("<{url}|Forgejo commit>"),
        }));
    }
    serde_json::json!({ "type": "context", "elements": elements })
}

#[cfg(test)]
mod tests {
    use super::{push_blocks, render_stats, ChangeNotice, PushNotice, CONFLICT_BADGE};
    use cloud::blocks_to_fallback_text;
    use cloud::forgejo_push_analysis::FileStats;

    use serde_json::Value;

    fn stats(insertions: i64, deletions: i64, files: i64) -> FileStats {
        FileStats {
            insertions,
            deletions,
            files,
        }
    }

    fn analysis(
        change_id: &str,
        summary: &str,
        conflicted: bool,
    ) -> cloud::forgejo_push_analysis::PushAnalysis {
        cloud::forgejo_push_analysis::PushAnalysis {
            change_id: change_id.to_owned(),
            author: "T-bot (tbot@neonlaw.org)".to_owned(),
            description_summary: summary.to_owned(),
            stats: stats(12, 3, 1),
            conflict_signal: conflicted,
        }
    }

    fn push(analyses: &[cloud::forgejo_push_analysis::PushAnalysis]) -> PushNotice<'_> {
        let changes = analyses
            .iter()
            .map(|a| ChangeNotice {
                analysis: a,
                commit_url: None,
                image_file_id: None,
            })
            .collect();
        PushNotice {
            project_code: "sample-estate",
            timestamp: "2026-11-11 09:21 UTC",
            changes,
        }
    }

    fn block_types(blocks: &[Value]) -> Vec<&str> {
        blocks.iter().filter_map(|b| b["type"].as_str()).collect()
    }

    #[test]
    fn a_normal_push_renders_the_blueprinted_shape() {
        let a = analysis(
            "qvrpxyq",
            "Amend the engagement letter's fee schedule",
            false,
        );
        let blocks = push_blocks(&push(&[a]));
        assert_eq!(block_types(&blocks), vec!["header", "section", "context"]);

        let header = blocks[0]["text"]["text"].as_str().unwrap();
        assert_eq!(header, "Project sample-estate · change qvrpxyq");

        let fields = blocks[1]["fields"].as_array().unwrap();
        assert!(fields[0]["text"]
            .as_str()
            .unwrap()
            .contains("Amend the engagement letter's fee schedule"));
        assert!(fields[1]["text"]
            .as_str()
            .unwrap()
            .contains("12 lines added, 3 removed across 1 file"));

        let elements = blocks[2]["elements"].as_array().unwrap();
        assert!(elements[0]["text"]
            .as_str()
            .unwrap()
            .contains("T-bot (tbot@neonlaw.org)"));
        assert!(elements[0]["text"]
            .as_str()
            .unwrap()
            .contains("2026-11-11 09:21 UTC"));

        // Every text field carries the type Block Kit requires.
        assert_eq!(blocks[0]["text"]["type"], "plain_text");
        assert_eq!(blocks[1]["fields"][0]["type"], "mrkdwn");
        assert_eq!(blocks[2]["elements"][0]["type"], "mrkdwn");
    }

    #[test]
    fn a_conflicted_push_carries_the_badge() {
        let a = analysis("qvrpxyq", "Merge the fee schedules", true);
        let blocks = push_blocks(&push(&[a]));
        assert_eq!(
            block_types(&blocks),
            vec!["header", "section", "section", "context"]
        );
        assert_eq!(blocks[2]["text"]["text"], CONFLICT_BADGE);
    }

    #[test]
    fn a_clean_push_carries_no_badge() {
        let a = analysis("qvrpxyq", "Amend the fee schedule", false);
        let blocks = push_blocks(&push(&[a]));
        for block in &blocks {
            assert_ne!(block["text"]["text"], CONFLICT_BADGE);
        }
    }

    #[test]
    fn a_multi_commit_push_renders_one_section_per_change() {
        let analyses = [
            analysis("qvrpxyq", "Amend the fee schedule", false),
            analysis("swwomtx", "Merge the engagement letter", true),
        ];
        let blocks = push_blocks(&push(&analyses));
        assert_eq!(
            block_types(&blocks),
            vec!["header", "section", "context", "section", "section", "context"]
        );
        assert_eq!(
            blocks[0]["text"]["text"],
            "Project sample-estate · 2 changes"
        );
        assert!(serde_json::to_string(&blocks[4])
            .unwrap()
            .contains(CONFLICT_BADGE));
        assert!(!serde_json::to_string(&blocks[1])
            .unwrap()
            .contains(CONFLICT_BADGE));
    }

    #[test]
    fn the_image_references_the_slack_file_id_and_never_a_url() {
        let a = analysis("qvrpxyq", "Amend the fee schedule", false);
        let changes = [a];
        let mut notice = push(&changes);
        notice.changes[0].image_file_id = Some("F0123ABC");
        let blocks = push_blocks(&notice);
        let image = blocks.iter().find(|b| b["type"] == "image").unwrap();
        assert_eq!(image["slack_file"]["id"], "F0123ABC");
        assert!(image.get("image_url").is_none());
        assert!(image["alt_text"].as_str().unwrap().contains("qvrpxyq"));
    }

    #[test]
    fn the_commit_link_rides_the_context_block_for_operators() {
        let a = analysis("qvrpxyq", "Amend the fee schedule", false);
        let changes = [a];
        let mut notice = push(&changes);
        notice.changes[0].commit_url =
            Some("https://forgejo.example/acme-law/sample-estate/commit/9f3b");
        let blocks = push_blocks(&notice);
        let context = serde_json::to_string(&blocks[2]).unwrap();
        assert!(context.contains("https://forgejo.example/acme-law/sample-estate/commit/9f3b"));
        assert!(context.contains("Forgejo commit"));

        // Without a URL, no link element — and never a link anywhere else.
        let bare = push_blocks(&push(&[analysis("qvrpxyq", "Amend", false)]));
        assert!(!serde_json::to_string(&bare).unwrap().contains("https://"));
    }

    #[test]
    fn the_fallback_text_stays_readable_through_the_post_blocks_degradation() {
        let a = analysis(
            "qvrpxyq",
            "Amend the engagement letter's fee schedule",
            true,
        );
        let changes = [a];
        let mut notice = push(&changes);
        notice.changes[0].commit_url =
            Some("https://forgejo.example/acme-law/sample-estate/commit/9f3b");
        let fallback = blocks_to_fallback_text(&push_blocks(&notice));
        for expected in [
            "Project sample-estate · change qvrpxyq",
            "Amend the engagement letter's fee schedule",
            "12 lines added, 3 removed across 1 file",
            CONFLICT_BADGE,
            "T-bot (tbot@neonlaw.org)",
            "2026-11-11 09:21 UTC",
            "Forgejo commit",
        ] {
            assert!(
                fallback.contains(expected),
                "fallback lost {expected}: {fallback}"
            );
        }
    }

    #[test]
    fn the_stats_read_in_lawyer_terms() {
        assert_eq!(
            render_stats(stats(12, 3, 1)),
            "12 lines added, 3 removed across 1 file"
        );
        assert_eq!(
            render_stats(stats(0, 2, 4)),
            "0 lines added, 2 removed across 4 files"
        );
    }
}

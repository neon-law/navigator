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


//! Per-change analysis of a journaled Forgejo push receipt.
//!
//! The push pipeline's analysis stage: given the journal's verified
//! receipt (see `store::postgres::forgejo_push_webhook_journal`), read the
//! pushed commit through the Forgejo API with the Firm's own credential —
//! no clone, no `jj` server-side — and reduce it to the facts the firm
//! reads: the jj change ID from the `push/<change-id>` bookmark name
//! ([`change_id_from_ref`], the runtime twin of
//! `cli::projects::jj::PUSH_BOOKMARK_TEMPLATE`), the description's first
//! line, the author classified bot-or-human, the diff statistics, and the
//! conflict signal.
//!
//! A jj conflict is Git-visible content: a conflicted change's commit
//! carries `<<<<<<<` and `>>>>>>>` marker lines, so the signal reads the
//! commit's diff and flags a marker *pair* among its added lines
//! ([`added_conflict_markers`]). Only added lines count — a marker quoted
//! in unchanged context is prose, and one a resolution removed was never
//! this change's conflict.
//!
//! The engine is pure orchestration over the two forge reads. It never
//! persists: the store's consumption wrapper copies the produced
//! [`PushAnalysis`] onto the receipt's journal row, so a redelivery or a
//! post-crash replay answers from the row and never re-reads the API.

use serde::Serialize;
use serde_json::Value as Json;

use crate::forge::ForgeError;
use crate::forgejo_forge::ForgejoForge;

/// A pushed change's diff statistics — the shape the journal persists and
/// the Block Kit message renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct FileStats {
    pub insertions: i64,
    pub deletions: i64,
    pub files: i64,
}

/// The analysis of one pushed change — field for field the analysis
/// columns the journal persists
/// (`store::postgres::forgejo_push_webhook_journal::PushAnalysis`), which
/// that crate's consumer copies across the crate boundary.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PushAnalysis {
    /// The jj change ID the pushed bookmark was named for.
    pub change_id: String,
    /// The classified author line, e.g. `T-bot (tbot@neonlaw.org)`.
    pub author: String,
    /// The change description's first line.
    pub description_summary: String,
    pub stats: FileStats,
    /// Whether the pushed change's added lines introduce Git conflict
    /// markers — how a jj conflict surfaces to the Git backend.
    pub conflict_signal: bool,
}

/// A commit author's kind: the firm's lawyers, or a T-bot working a jj
/// workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorKind {
    Human,
    Bot,
}

/// The T-bot service identity commits are authored under (`tbot` at), the
/// convention the journal's own fixtures record. A named registry of bot
/// identities, when one exists, supersedes this constant.
const TBOT_LOCAL_PART: &str = "tbot";

/// Classify a commit author by email local part.
#[must_use]
pub fn classify_author(email: &str) -> AuthorKind {
    let local = email.split('@').next().unwrap_or_default();
    if local.eq_ignore_ascii_case(TBOT_LOCAL_PART) {
        AuthorKind::Bot
    } else {
        AuthorKind::Human
    }
}

/// The author line the firm reads: the bot's role or the lawyer's name,
/// with the email either way.
#[must_use]
pub fn render_author(name: &str, email: &str, kind: AuthorKind) -> String {
    let display = match kind {
        AuthorKind::Bot => "T-bot",
        AuthorKind::Human => name,
    };
    format!("{display} ({email})")
}

/// The change ID a pushed ref's name carries: the bookmark
/// `jj git push --change` mints, `push/<change-id-short>` — as a ref,
/// `refs/heads/push/<change-id-short>`. `None` for a ref outside the
/// `push/` namespace or a change-less bookmark.
#[must_use]
pub fn change_id_from_ref(ref_name: &str) -> Option<&str> {
    ref_name
        .strip_prefix("refs/heads/")
        .unwrap_or(ref_name)
        .strip_prefix("push/")
        .filter(|id| !id.is_empty())
}

/// The Project code a receipt's `repo_full_name` resolves to under this
/// client's Firm namespace — `None` when the push named a repository
/// outside the Firm, which this client must never read.
#[must_use]
pub fn resolve_repository<'a>(forge: &ForgejoForge, repo_full_name: &'a str) -> Option<&'a str> {
    let (owner, project_code) = repo_full_name.split_once('/')?;
    (owner == forge.owner() && crate::workspace::is_valid_slug(project_code))
        .then_some(project_code)
}

/// The Git conflict-marker pair, each at line starts, per Git's own
/// merge-conflict shape. Requiring the pair — not a lone `<<<<<<<` —
/// keeps a marker quoted in legal prose from flagging a document.
fn marker_pair<'a>(lines: impl Iterator<Item = &'a str>) -> bool {
    let (mut opened, mut closed) = (false, false);
    for line in lines {
        opened |= line.starts_with("<<<<<<<");
        closed |= line.starts_with(">>>>>>>");
        if opened && closed {
            return true;
        }
    }
    false
}

/// Whether any line of `text` opens and closes a Git conflict marker —
/// the shape a jj-conflicted change's committed content has.
#[must_use]
pub fn contains_conflict_markers(text: &str) -> bool {
    marker_pair(text.lines())
}

/// A diff's added lines, without the `+` — and without the diff's own
/// `+++` header, which a single strip would otherwise disguise as an
/// added `++ …` line.
fn added_lines(diff: &str) -> impl Iterator<Item = &str> {
    diff.lines()
        .filter(|line| !line.starts_with("+++"))
        .filter_map(|line| line.strip_prefix('+'))
}

/// Whether the pushed change's *added* lines introduce a conflict-marker
/// pair — how a jj conflict becomes Git-visible. A marker carried in
/// context or removed by a resolution was never this change's conflict.
#[must_use]
pub fn added_conflict_markers(diff: &str) -> bool {
    marker_pair(added_lines(diff))
}

/// A counted iterator as the stats column's `i64` — saturating, since a
/// diff with more than `i64::MAX` lines is not a real thing, but a cast
/// is a lint.
fn line_count<'a>(lines: impl Iterator<Item = &'a str>) -> i64 {
    i64::try_from(lines.count()).unwrap_or(i64::MAX)
}

/// The diff's own statistics: insertions and deletions from the
/// unified-diff body, files from the `diff --git` headers — the fallback
/// when the forge's commit record carries none.
#[must_use]
pub fn diff_stats(diff: &str) -> FileStats {
    FileStats {
        insertions: line_count(added_lines(diff)),
        deletions: line_count(
            diff.lines()
                .filter(|line| line.starts_with('-') && !line.starts_with("---")),
        ),
        files: line_count(diff.lines().filter(|line| line.starts_with("diff --git "))),
    }
}

/// The commit message's first line — a jj change's description, one line
/// long by construction. Empty when the message is blank.
#[must_use]
pub fn description_summary(message: &str) -> &str {
    message
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
}

/// Why a journaled push cannot be analyzed.
#[derive(Debug, thiserror::Error)]
pub enum AnalysisError {
    /// A `push/*` ref update named no destination commit — the payload
    /// promised a push it cannot locate.
    #[error("push ref update for change {change_id} did not name a destination commit")]
    MissingDestination { change_id: String },
    /// The pushed commit named no author.
    #[error("pushed commit {change_id} did not name an author")]
    MissingAuthor { change_id: String },
    /// The receipt names a repository outside this Firm's namespace.
    #[error("push receipt names repository {0}, outside this Firm's namespace")]
    ForeignRepository(String),
    /// A forge read failed.
    #[error(transparent)]
    Forge(#[from] ForgeError),
}

/// Analyze a journaled push: the first `push/*` ref update's change, read
/// through the Firm's Forgejo credential. `None` when the payload names
/// no `push/*` bookmark — the skip case, observable and never analyzed.
///
/// # Errors
/// [`AnalysisError`] when the receipt names a foreign repository, the
/// push's ref update is malformed, or a forge read fails.
pub async fn analyze_push(
    forge: &ForgejoForge,
    repo_full_name: &str,
    ref_updates: &Json,
) -> Result<Option<PushAnalysis>, AnalysisError> {
    let Some(project_code) = resolve_repository(forge, repo_full_name) else {
        return Err(AnalysisError::ForeignRepository(repo_full_name.to_string()));
    };
    let Some((change_id, destination)) =
        ref_updates
            .as_array()
            .into_iter()
            .flatten()
            .find_map(|update| {
                let ref_name = update.get("ref")?.as_str()?;
                let change_id = change_id_from_ref(ref_name)?;
                Some((change_id, update.get("after").and_then(Json::as_str)))
            })
    else {
        return Ok(None);
    };
    let Some(after) = destination else {
        return Err(AnalysisError::MissingDestination {
            change_id: change_id.to_owned(),
        });
    };

    let detail = forge.commit_detail(project_code, after).await?;
    let diff = forge.commit_diff(project_code, after).await?;
    let (Some(name), Some(email)) = (&detail.author_name, &detail.author_email) else {
        return Err(AnalysisError::MissingAuthor {
            change_id: change_id.to_owned(),
        });
    };
    let stats = detail.stats.map_or_else(
        || diff_stats(&diff),
        |stats| FileStats {
            insertions: stats.insertions,
            deletions: stats.deletions,
            files: diff_stats(&diff).files,
        },
    );
    Ok(Some(PushAnalysis {
        change_id: change_id.to_owned(),
        author: render_author(name, email, classify_author(email)),
        description_summary: description_summary(detail.message.as_deref().unwrap_or_default())
            .to_owned(),
        stats,
        conflict_signal: added_conflict_markers(&diff),
    }))
}

#[cfg(test)]
mod tests {
    use super::{
        change_id_from_ref, classify_author, contains_conflict_markers, render_author, AuthorKind,
    };

    #[test]
    fn the_push_namespace_parses_to_change_ids() {
        for (ref_name, expected) in [
            ("refs/heads/push/qvrpxyq", Some("qvrpxyq")),
            ("push/qvrpxyq", Some("qvrpxyq")),
            ("refs/heads/main", None),
            ("refs/heads/push/", None),
            ("", None),
        ] {
            assert_eq!(change_id_from_ref(ref_name), expected, "{ref_name}");
        }
    }

    #[test]
    fn authors_classify_bot_or_human_and_render_for_the_firm() {
        assert_eq!(classify_author("tbot@neonlaw.org"), AuthorKind::Bot);
        assert_eq!(classify_author("Tbot@neonlaw.org"), AuthorKind::Bot);
        assert_eq!(classify_author("nick@neonlaw.com"), AuthorKind::Human);
        assert_eq!(
            render_author("T-bot", "tbot@neonlaw.org", AuthorKind::Bot),
            "T-bot (tbot@neonlaw.org)"
        );
        assert_eq!(
            render_author("Nick Shook", "nick@neonlaw.com", AuthorKind::Human),
            "Nick Shook (nick@neonlaw.com)"
        );
    }

    #[test]
    fn a_lone_marker_is_prose_but_a_pair_is_a_conflict() {
        // The pair at line starts: a conflict.
        assert!(contains_conflict_markers(
            "before\n<<<<<<< theirs\nours\n>>>>>>> theirs\nafter"
        ));
        // A lone marker quoted in prose: not one.
        assert!(!contains_conflict_markers(
            "the log shows <<<<<<< as output"
        ));
        assert!(!contains_conflict_markers("<<<<<<< only"));
        // Markers not at line starts are prose.
        assert!(!contains_conflict_markers(
            "see <<<<<<< and >>>>>>> in the diff"
        ));
    }

    use super::{added_conflict_markers, diff_stats};

    #[test]
    fn only_added_lines_signal_a_conflict() {
        let added = concat!(
            "diff --git a/letter.md b/letter.md\n",
            "index 111..222 100644\n",
            "--- a/letter.md\n",
            "+++ b/letter.md\n",
            "@@ -1,2 +1,6 @@\n",
            " context\n",
            "+<<<<<<< theirs\n",
            "+ours\n",
            "+>>>>>>> theirs\n",
        );
        assert!(added_conflict_markers(added));

        // A resolution removes the markers: never this change's conflict.
        let resolved = concat!(
            "diff --git a/letter.md b/letter.md\n",
            "@@ -1,6 +1,2 @@\n",
            " context\n",
            "-<<<<<<< theirs\n",
            "->>>>>>> theirs\n",
        );
        assert!(!added_conflict_markers(resolved));

        // A marker carried in unchanged context is prose.
        let carried = concat!(
            "diff --git a/letter.md b/letter.md\n",
            "@@ -1,6 +1,6 @@\n",
            " <<<<<<< theirs\n",
            " >>>>>>> theirs\n",
        );
        assert!(!added_conflict_markers(carried));
    }

    #[test]
    fn the_diff_stats_count_its_body_not_its_headers() {
        let diff = concat!(
            "diff --git a/letter.md b/letter.md\n",
            "--- a/letter.md\n",
            "+++ b/letter.md\n",
            "@@ -1,2 +1,3 @@\n",
            " context\n",
            "+first\n",
            "+second\n",
            "-dropped\n",
            "diff --git a/filing.md b/filing.md\n",
            "--- a/filing.md\n",
            "+++ b/filing.md\n",
            "@@ -1,1 +1,1 @@\n",
            "-old\n",
            "+new\n",
        );
        assert_eq!(
            diff_stats(diff),
            super::FileStats {
                insertions: 3,
                deletions: 2,
                files: 2
            }
        );
    }

    mod analyze {
        use super::super::{analyze_push, PushAnalysis};
        use crate::forgejo_forge::ForgejoForge;
        use serde_json::{json, Value};
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        const SHA: &str = "9f3ba2c7114e0d5689c0b1a2f4e5d6c7b8a9f0e1";
        const REPO: &str = "acme-law/sample-estate";
        const COMMIT_PATH: &str =
            "/api/v1/repos/acme-law/sample-estate/git/commits/9f3ba2c7114e0d5689c0b1a2f4e5d6c7b8a9f0e1";

        fn forge(server: &MockServer) -> ForgejoForge {
            ForgejoForge::new(&server.uri(), "acme-law", "synthetic-token").unwrap()
        }

        fn ref_updates() -> Value {
            json!([
                {
                    "ref": "refs/heads/push/qvrpxyq",
                    "before": "0000000000000000000000000000000000000000",
                    "after": SHA,
                }
            ])
        }

        fn commit_detail() -> Value {
            json!({
                "sha": SHA,
                "commit": {
                    "message": "Amend the engagement letter's fee schedule\n\nbody\n",
                    "author": { "name": "T-bot", "email": "tbot@neonlaw.org" },
                },
                "stats": { "additions": 12, "deletions": 3 },
            })
        }

        fn commit_diff() -> &'static str {
            "diff --git a/letter.md b/letter.md\n--- a/letter.md\n+++ b/letter.md\n@@ -1,2 +1,2 @@\n-old\n+new\n"
        }

        async fn mounted(forge_reads: usize) -> (MockServer, ForgejoForge) {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path(COMMIT_PATH))
                .respond_with(ResponseTemplate::new(200).set_body_json(commit_detail()))
                .expect(forge_reads as u64)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path(format!("{COMMIT_PATH}.diff")))
                .respond_with(ResponseTemplate::new(200).set_body_string(commit_diff()))
                .expect(forge_reads as u64)
                .mount(&server)
                .await;
            let client = forge(&server);
            (server, client)
        }

        #[tokio::test]
        async fn a_pushed_change_reduces_to_its_facts() {
            let (_server, client) = mounted(1).await;
            let analysis = analyze_push(&client, REPO, &ref_updates())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                analysis,
                PushAnalysis {
                    change_id: "qvrpxyq".to_owned(),
                    author: "T-bot (tbot@neonlaw.org)".to_owned(),
                    description_summary: "Amend the engagement letter's fee schedule".to_owned(),
                    stats: super::super::FileStats {
                        insertions: 12,
                        deletions: 3,
                        files: 1
                    },
                    conflict_signal: false,
                }
            );
        }

        #[tokio::test]
        async fn a_conflicted_change_signals_conflict_and_falls_back_to_diff_stats() {
            let server = MockServer::start().await;
            let conflicted = json!({
                "sha": SHA,
                "commit": {
                    "message": "Merge the fee schedules",
                    "author": { "name": "Nick Shook", "email": "nick@neonlaw.com" },
                },
            });
            Mock::given(method("GET"))
                .and(path(COMMIT_PATH))
                .respond_with(ResponseTemplate::new(200).set_body_json(conflicted))
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path(format!("{COMMIT_PATH}.diff")))
                .respond_with(ResponseTemplate::new(200).set_body_string(concat!(
                    "diff --git a/letter.md b/letter.md\n",
                    "--- a/letter.md\n",
                    "+++ b/letter.md\n",
                    "@@ -1,2 +1,5 @@\n",
                    "+<<<<<<< theirs\n",
                    "+ours\n",
                    "+>>>>>>> theirs\n",
                )))
                .expect(1)
                .mount(&server)
                .await;
            let client = forge(&server);

            let analysis = analyze_push(&client, REPO, &ref_updates())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(analysis.change_id, "qvrpxyq");
            assert_eq!(analysis.author, "Nick Shook (nick@neonlaw.com)");
            assert_eq!(
                analysis.stats,
                super::super::FileStats {
                    insertions: 3,
                    deletions: 0,
                    files: 1
                }
            );
            assert!(analysis.conflict_signal);
        }

        #[tokio::test]
        async fn a_push_outside_the_push_namespace_is_skipped_without_a_read() {
            // No mocks mounted: any read fails the test.
            let server = MockServer::start().await;
            let client = forge(&server);
            let main = json!([
                { "ref": "refs/heads/main", "before": SHA, "after": SHA }
            ]);
            assert!(analyze_push(&client, REPO, &main).await.unwrap().is_none());
        }

        #[tokio::test]
        async fn a_push_for_another_firms_repository_is_refused() {
            let server = MockServer::start().await;
            let client = forge(&server);
            let error = analyze_push(&client, "other-firm/sample-estate", &ref_updates())
                .await
                .unwrap_err();
            assert!(error.to_string().contains("outside this Firm's namespace"));
        }

        #[tokio::test]
        async fn a_push_update_without_a_destination_is_a_consumption_failure() {
            let server = MockServer::start().await;
            let client = forge(&server);
            let destinationless = json!([
                { "ref": "refs/heads/push/qvrpxyq", "before": SHA }
            ]);
            let error = analyze_push(&client, REPO, &destinationless)
                .await
                .unwrap_err();
            assert!(error.to_string().contains("did not name a destination"));
        }
    }
}

//! One Slack message per Project pull request, edited in place as it moves.
//!
//! A Project repository's CI reports each pull-request run to the Project's
//! private channel. The first report posts a message; every later report for
//! the same pull request rewrites that message through `chat.update`, so the
//! channel shows one line per pull request rather than one per run. A Pointer
//! whose `visibility` becomes `client` is a client-release decision, so each
//! one is called out on its own line.

use std::fmt::Write;

use serde::{Deserialize, Serialize};

use crate::slack::{SlackError, SlackService};

/// The longest title the notice accepts.
const TITLE_LIMIT: usize = 512;
/// How much of the title the notice shows.
const TITLE_SHOWN: usize = 120;
/// The most Pointers one notice lists.
const POINTER_LIMIT: usize = 50;

/// Where a pull request stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestStage {
    ChecksPassed,
    ChecksFailed,
    Merged,
    Closed,
}

impl PullRequestStage {
    const fn label(self) -> &'static str {
        match self {
            Self::ChecksPassed => "checks passed",
            Self::ChecksFailed => "checks failed",
            Self::Merged => "merged",
            Self::Closed => "closed without merging",
        }
    }
}

/// A Pointer the pull request adds or changes: its slug (the path below
/// `blobs/`, without `.yaml`) and the `visibility` it ends with.
/// `client_release` is true when the pull request makes it `client`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PointerChange {
    pub slug: String,
    pub visibility: String,
    #[serde(default)]
    pub client_release: bool,
}

/// One report of a pull request, as the CI run sends it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PullRequestEvent {
    pub number: u32,
    pub title: String,
    pub url: String,
    pub author_login: String,
    pub stage: PullRequestStage,
    #[serde(default)]
    pub pointers: Vec<PointerChange>,
}

fn one_line(value: &str) -> bool {
    !value.chars().any(char::is_control)
}

/// A Pointer slug is a relative path below `blobs/`. File names may hold
/// spaces; they may not hold control characters, or the backtick the notice
/// quotes them in.
fn slug_ok(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 512
        && !slug.starts_with('/')
        && !slug.split('/').any(|part| part.is_empty() || part == "..")
        && !slug.chars().any(|c| c.is_control() || c == '`')
}

impl PullRequestEvent {
    /// Refuse anything that is not a positive number, a one-line title, an
    /// `https` URL, a forge login, and well-formed Pointer slugs, naming the
    /// first bad field.
    ///
    /// # Errors
    /// The message names the refused field.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.number == 0 {
            return Err("number must be at least 1");
        }
        if self.title.trim().is_empty()
            || self.title.chars().count() > TITLE_LIMIT
            || !one_line(&self.title)
        {
            return Err("title must be one line of at most 512 characters");
        }
        // The URL sits inside Slack's `<url|label>` link, so it may not carry
        // whitespace or the link's own delimiters.
        if !self.url.starts_with("https://")
            || self.url.len() > 2048
            || self
                .url
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | '|'))
        {
            return Err("url must be an https URL");
        }
        let login_ok = (1..=40).contains(&self.author_login.len())
            && self
                .author_login
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
        if !login_ok {
            return Err("author_login must be a GitHub or Forgejo login");
        }
        if self.pointers.len() > POINTER_LIMIT {
            return Err("pointers lists at most 50 Pointers");
        }
        for pointer in &self.pointers {
            if !slug_ok(&pointer.slug) {
                return Err("each pointer slug must be a path below blobs/");
            }
            if !matches!(pointer.visibility.as_str(), "internal" | "client") {
                return Err("each pointer visibility must be internal or client");
            }
            if pointer.client_release && pointer.visibility != "client" {
                return Err("a client release must end with visibility client");
            }
        }
        Ok(())
    }
}

/// Free text as Slack shows it: control characters and mrkdwn markers
/// dropped, and `&`, `<`, `>` escaped so nothing can mention or link anyone.
fn plain(text: &str, shown: usize) -> String {
    let kept: String = text
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '*' | '_' | '~' | '`'))
        .collect();
    let kept = kept.trim();
    let mut out: String = kept.chars().take(shown).collect();
    if out.chars().count() < kept.chars().count() {
        out.push('…');
    }
    escape(&out)
}

/// Escape Slack's three control characters so text renders literally.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The notice: the linked number, title, author, and stage, then one line
/// per Pointer, each client release on its own line.
#[must_use]
pub fn render(event: &PullRequestEvent) -> String {
    let mut notice = format!(
        "<{}|PR #{}> {} by {} — {}",
        event.url,
        event.number,
        plain(&event.title, TITLE_SHOWN),
        event.author_login,
        event.stage.label()
    );
    for pointer in &event.pointers {
        notice.push('\n');
        if pointer.client_release {
            let _ = write!(
                notice,
                ":warning: Client release: `{}` becomes visibility client",
                escape(&pointer.slug)
            );
        } else {
            let _ = write!(
                notice,
                "Files `{}` (visibility {})",
                escape(&pointer.slug),
                pointer.visibility
            );
        }
    }
    notice
}

/// What [`publish`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Published {
    /// A new message was posted at this timestamp.
    Posted(String),
    /// The message at this timestamp was rewritten in place.
    Updated(String),
}

impl Published {
    #[must_use]
    pub fn ts(&self) -> &str {
        match self {
            Self::Posted(ts) | Self::Updated(ts) => ts,
        }
    }
}

/// Rewrite the pull request's recorded message, or post one when none is
/// recorded or Slack no longer has it.
///
/// # Errors
/// Slack's refusal or an unreachable Slack.
pub async fn publish<S: SlackService + ?Sized>(
    slack: &S,
    channel_id: &str,
    recorded_ts: Option<&str>,
    event: &PullRequestEvent,
) -> Result<Published, SlackError> {
    let text = render(event);
    if let Some(ts) = recorded_ts {
        if slack.update_message(channel_id, ts, &text).await? {
            return Ok(Published::Updated(ts.to_string()));
        }
    }
    slack
        .post_message_ts(channel_id, &text)
        .await
        .map(Published::Posted)
}

#[cfg(test)]
mod tests {
    use super::{publish, render, PointerChange, Published, PullRequestEvent, PullRequestStage};
    use crate::slack::{ensure_private_channel, FakeSlack};

    fn event(stage: PullRequestStage) -> PullRequestEvent {
        PullRequestEvent {
            number: 12,
            title: "File the <answer> & exhibits".to_string(),
            url: "https://forge.example/acme/sample/pulls/12".to_string(),
            author_login: "lawyer-1".to_string(),
            stage,
            pointers: vec![
                PointerChange {
                    slug: "answer.pdf".to_string(),
                    visibility: "client".to_string(),
                    client_release: true,
                },
                PointerChange {
                    slug: "exhibits/a & b.pdf".to_string(),
                    visibility: "internal".to_string(),
                    client_release: false,
                },
            ],
        }
    }

    #[test]
    fn the_notice_calls_out_a_client_release_on_its_own_line() {
        let notice = render(&event(PullRequestStage::ChecksPassed));
        let lines: Vec<&str> = notice.lines().collect();
        assert_eq!(
            lines,
            [
                "<https://forge.example/acme/sample/pulls/12|PR #12> File the &lt;answer&gt; &amp; exhibits by lawyer-1 — checks passed",
                ":warning: Client release: `answer.pdf` becomes visibility client",
                "Files `exhibits/a &amp; b.pdf` (visibility internal)",
            ]
        );
    }

    #[tokio::test]
    async fn one_pull_request_is_one_message_edited_in_place() {
        let slack = FakeSlack::new();
        let (channel, _) = ensure_private_channel(&slack, "navigator-sample", &[])
            .await
            .unwrap();
        let first = publish(
            &slack,
            &channel.id,
            None,
            &event(PullRequestStage::ChecksFailed),
        )
        .await
        .unwrap();
        let Published::Posted(ts) = first.clone() else {
            panic!("the first report posts: {first:?}");
        };
        let second = publish(
            &slack,
            &channel.id,
            Some(&ts),
            &event(PullRequestStage::Merged),
        )
        .await
        .unwrap();
        assert_eq!(second, Published::Updated(ts.clone()));
        assert_eq!(
            slack.messages().len(),
            1,
            "the merge edits, it does not post"
        );
        let (_, text) = slack.message_at(&ts).expect("the message is still there");
        assert!(text.contains("— merged"), "{text}");
    }

    #[tokio::test]
    async fn a_message_deleted_in_slack_is_posted_again() {
        let slack = FakeSlack::new();
        let (channel, _) = ensure_private_channel(&slack, "navigator-sample", &[])
            .await
            .unwrap();
        let ts = publish(
            &slack,
            &channel.id,
            None,
            &event(PullRequestStage::ChecksPassed),
        )
        .await
        .unwrap()
        .ts()
        .to_string();
        slack.delete_message(&ts);
        let again = publish(
            &slack,
            &channel.id,
            Some(&ts),
            &event(PullRequestStage::Merged),
        )
        .await
        .unwrap();
        assert!(
            matches!(again, Published::Posted(ref new) if new != &ts),
            "{again:?}"
        );
    }

    #[test]
    fn validation_names_the_first_bad_field() {
        let mut bad = event(PullRequestStage::ChecksPassed);
        assert_eq!(bad.validate(), Ok(()));
        bad.title = "two\nlines".to_string();
        assert_eq!(
            bad.validate(),
            Err("title must be one line of at most 512 characters")
        );
        let mut bad = event(PullRequestStage::ChecksPassed);
        bad.pointers[0].slug = "../escape.pdf".to_string();
        assert_eq!(
            bad.validate(),
            Err("each pointer slug must be a path below blobs/")
        );
        let mut bad = event(PullRequestStage::ChecksPassed);
        bad.pointers[1].client_release = true;
        assert_eq!(
            bad.validate(),
            Err("a client release must end with visibility client")
        );
        let mut bad = event(PullRequestStage::ChecksPassed);
        bad.url = "http://forge.example/pulls/12".to_string();
        assert_eq!(bad.validate(), Err("url must be an https URL"));
    }
}

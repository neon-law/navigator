//! Durable polling worker for inbound `YouTube` comments on a Brand's
//! published videos.
//!
//! `YouTube` offers no push surface for comments; the poller reads the
//! `commentThreads` endpoint for every confirmed `YouTube` publication receipt
//! and files each audience comment into the firm inbox on the `youtube`
//! channel through [`store::inbox::ingest_inbound`]. Provider dedup keys on
//! `(firm, channel, provider_account, provider_message_id)`, so repeated polls
//! of the same comment produce one Message, and a fresh, non-deduped ingest
//! starts the same alert and triage workflows every other inbound channel
//! starts after PR #220's seams.
//!
//! Both workflow engines checkpoint one step per video. The polling core
//! shares the same methods with provider tests, and only a production
//! deployment reaches the provider.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use serde::{Deserialize, Serialize};
use store::application_store::ApplicationStore;
use store::config::SocialEgressGate;
use store::social_accounts::SocialProvider;
use uuid::Uuid;

use workflows::social_publish::{self, TokenRefreshConfig};

pub type PollWorkflow = dbos::WorkflowRef<PollRequest, PollReport, PollError>;

#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
pub enum PollError {
    #[error("the YouTube poll targets could not be loaded")]
    Targets,
}

pub fn register_dbos(
    dbos: &dbos::DBOS,
    service: YoutubeCommentsPollerService,
) -> dbos::Result<PollWorkflow> {
    dbos.register_workflow("YoutubeCommentsPoller", move |_: PollRequest| {
        let service = service.clone();
        async move {
            let targets = dbos::step("load-youtube-poll-targets", || {
                let service = service.clone();
                async move {
                    service
                        .poll_targets()
                        .await
                        .map_err(|_| dbos::Error::Application(PollError::Targets))
                }
            })
            .await?;
            let mut report = PollReport {
                videos: targets.len(),
                ..PollReport::default()
            };
            for (index, target) in targets.into_iter().enumerate() {
                let name = format!("poll-youtube-comments-{index}");
                let outcome = dbos::step(&name, || {
                    let (service, target) = (service.clone(), target.clone());
                    async move {
                        let outcome = service.poll_video(&target).await;
                        if outcome.is_err() {
                            tracing::warn!(video_index = index, "YouTube comment poll failed");
                        }
                        Ok::<_, dbos::Error<PollError>>(outcome.ok())
                    }
                })
                .await?;
                if let Some(outcome) = outcome {
                    report.ingested += outcome.ingested;
                    report.deduped += outcome.deduped;
                    report.skipped += outcome.skipped;
                } else {
                    report.failed += 1;
                }
            }
            Ok(report)
        }
    })
}

const YOUTUBE_API: &str = "https://www.googleapis.com/youtube/v3";
/// Page size for commentThreads reads.
const PAGE_LIMIT: &str = "100";

/// The default readback horizon: the 100 most recently confirmed videos.
const VIDEO_LIMIT: usize = 100;

/// One video to poll, resolved from a confirmed publication receipt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollTarget {
    pub video_id: String,
    pub brand_key: String,
    pub account_id: String,
    pub firm_id: Uuid,
}

/// Per-video polling result.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct VideoOutcome {
    /// Comments filed as fresh Messages.
    pub ingested: usize,
    /// Comments already present from an earlier poll.
    pub deduped: usize,
    /// Comments dropped for a parseable reason (Brand's own echo, an
    /// unparseable timestamp). Each drop warns with its reason.
    pub skipped: usize,
}

impl VideoOutcome {
    fn record(&mut self, outcome: IngestOutcome) {
        match outcome {
            IngestOutcome::Fresh => self.ingested += 1,
            IngestOutcome::Duplicate => self.deduped += 1,
            IngestOutcome::Skipped => self.skipped += 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IngestOutcome {
    Fresh,
    Duplicate,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollRequest {
    pub tick: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PollReport {
    pub videos: usize,
    pub ingested: usize,
    pub deduped: usize,
    pub skipped: usize,
    pub failed: usize,
}

/// The polling core. The Restate workflow wraps its methods in journaled
/// steps; tests call them directly against a local store and mock server.
#[derive(Clone)]
pub struct YoutubeCommentsPollerService {
    application_store: ApplicationStore,
    kms: Arc<dyn cloud::RuntimeKms>,
    http: reqwest::Client,
    base: String,
    egress: SocialEgressGate,
    refresh: TokenRefreshConfig,
}

impl YoutubeCommentsPollerService {
    #[must_use]
    pub fn new(application_store: ApplicationStore, kms: Arc<dyn cloud::RuntimeKms>) -> Self {
        Self {
            application_store,
            kms,
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            base: YOUTUBE_API.to_string(),
            egress: SocialEgressGate::from_process_env(),
            refresh: TokenRefreshConfig::from_env(),
        }
    }

    /// Point the readback at a different host (the test harness).
    #[must_use]
    pub fn with_base(mut self, base: String) -> Self {
        self.base = base;
        self
    }

    #[must_use]
    pub fn with_egress(mut self, egress: SocialEgressGate) -> Self {
        self.egress = egress;
        self
    }

    /// Resolve confirmed `YouTube` publication receipts into poll targets.
    ///
    /// # Errors
    ///
    /// A database failure.
    pub async fn poll_targets(&self) -> anyhow::Result<Vec<PollTarget>> {
        let publications = store::social_publications::confirmed_links(
            &self.application_store,
            "youtube",
            VIDEO_LIMIT,
        )
        .await?;
        let mut targets = Vec::new();
        for publication in publications {
            let Some(video_id) = publication.provider_post_id.as_deref() else {
                continue;
            };
            let Some(post) =
                store::posts::find_by_id(&self.application_store, publication.post_id).await?
            else {
                continue;
            };
            let accounts = store::social_accounts::selected_for_brand(
                &self.application_store,
                &post.brand_key,
                &post.selected_social_accounts,
            )
            .await?;
            let Some(account) = accounts
                .into_iter()
                .find(|account| account.provider == SocialProvider::Youtube)
            else {
                continue;
            };
            targets.push(PollTarget {
                video_id: video_id.to_string(),
                brand_key: post.brand_key,
                account_id: account.provider_account_id,
                firm_id: account.firm_id,
            });
        }
        Ok(targets)
    }

    /// Poll one video's comment threads into the inbox.
    ///
    /// # Errors
    ///
    /// A credential, provider, or database failure — the caller journals the
    /// step so a transient failure retries without re-reading other videos.
    pub async fn poll_video(&self, target: &PollTarget) -> anyhow::Result<VideoOutcome> {
        if self.egress.check().is_err() {
            return Ok(VideoOutcome::default());
        }
        let credential = social_publish::send_credential(
            &self.application_store,
            self.kms.as_ref(),
            &self.http,
            &self.refresh,
            &target.brand_key,
            SocialProvider::Youtube,
            &target.account_id,
        )
        .await?;
        let comments = self
            .fetch_comments(&target.video_id, credential.access_token())
            .await?;
        let mut outcome = VideoOutcome::default();
        for comment in comments {
            if comment.author_channel_id == target.account_id {
                // The Brand's own reply echoes back as a thread comment; like
                // the TikTok seam, an owner echo is never an inbound message.
                outcome.record(IngestOutcome::Skipped);
                continue;
            }
            if comment.text.trim().is_empty() {
                outcome.record(IngestOutcome::Skipped);
                continue;
            }
            let Some(occurred) = occurred_at(&comment.published_at) else {
                tracing::warn!(
                    video_id = %target.video_id,
                    comment_id = %comment.id,
                    "YouTube comment carried an unparseable timestamp; skipped"
                );
                outcome.record(IngestOutcome::Skipped);
                continue;
            };
            let received = self.ingest_comment(target, &comment, occurred).await?;
            start_inbound_hooks(received.message_id).await?;
            outcome.record(if received.deduped {
                IngestOutcome::Duplicate
            } else {
                IngestOutcome::Fresh
            });
        }
        Ok(outcome)
    }

    /// Fetch every thread page and every omitted reply page before publishing effects.
    /// Explicit limits fail the poll instead of silently reporting a truncated success.
    async fn fetch_comments(
        &self,
        video_id: &str,
        token: &str,
    ) -> anyhow::Result<Vec<ParsedComment>> {
        let mut output = Vec::new();
        let mut ids = std::collections::BTreeSet::new();
        let mut page_token: Option<String> = None;
        let mut seen = std::collections::BTreeSet::new();
        let mut requests = 0_usize;
        loop {
            requests += 1;
            anyhow::ensure!(requests <= 4096, "YouTube pagination request limit");
            let mut request = self
                .http
                .get(format!("{}/commentThreads", self.base))
                .query(&[
                    ("part", "snippet,replies"),
                    ("videoId", video_id),
                    ("maxResults", PAGE_LIMIT),
                    ("order", "time"),
                    ("textFormat", "plainText"),
                ])
                .bearer_auth(token);
            if let Some(page) = page_token.as_deref() {
                request = request.query(&[("pageToken", page)]);
            }
            let response = request.send().await?;
            let status = response.status();
            anyhow::ensure!(status.is_success(), "commentThreads read returned {status}");
            let page: CommentThreads = response.json().await?;
            for thread in &page.items {
                let parent = &thread.snippet.top_level_comment.id;
                let mut comments = vec![parse_comment(&thread.snippet.top_level_comment, None)];
                let embedded = thread.replies.as_ref().or(thread.snippet.replies.as_ref());
                if let Some(replies) = embedded {
                    comments.extend(
                        replies
                            .comments
                            .iter()
                            .map(|reply| parse_comment(reply, Some(parent.clone()))),
                    );
                }
                if thread.snippet.total_reply_count > embedded.map_or(0, |r| r.comments.len()) {
                    let mut reply_token: Option<String> = None;
                    let mut reply_seen = std::collections::BTreeSet::new();
                    loop {
                        requests += 1;
                        anyhow::ensure!(requests <= 4096, "YouTube pagination request limit");
                        let mut request = self
                            .http
                            .get(format!("{}/comments", self.base))
                            .query(&[
                                ("part", "snippet"),
                                ("parentId", parent.as_str()),
                                ("maxResults", PAGE_LIMIT),
                                ("textFormat", "plainText"),
                            ])
                            .bearer_auth(token);
                        if let Some(page) = reply_token.as_deref() {
                            request = request.query(&[("pageToken", page)]);
                        }
                        let response = request.send().await?;
                        let status = response.status();
                        anyhow::ensure!(status.is_success(), "comments read returned {status}");
                        let replies: CommentPage = response.json().await?;
                        comments.extend(
                            replies
                                .items
                                .iter()
                                .map(|reply| parse_comment(reply, Some(parent.clone()))),
                        );
                        reply_token = replies.next_page_token;
                        match reply_token.as_ref() {
                            None => break,
                            Some(next) => anyhow::ensure!(
                                reply_seen.insert(next.clone()),
                                "YouTube repeated reply page token"
                            ),
                        }
                    }
                }
                for comment in comments {
                    if ids.insert(comment.id.clone()) {
                        output.push(comment);
                    }
                }
                anyhow::ensure!(output.len() <= 100_000, "YouTube comment count limit");
            }
            page_token = page.next_page_token;
            match page_token.as_ref() {
                None => break,
                Some(next) => anyhow::ensure!(
                    seen.insert(next.clone()),
                    "YouTube repeated thread page token"
                ),
            }
        }
        Ok(output)
    }

    async fn ingest_comment(
        &self,
        target: &PollTarget,
        comment: &ParsedComment,
        occurred: DateTime<Utc>,
    ) -> anyhow::Result<store::inbox::IngestedInbound> {
        store::inbox::ingest_inbound(
            &self.application_store,
            &store::inbox::IngestInbound {
                firm_id: target.firm_id,
                brand_key: Some(&target.brand_key),
                interaction_type: Some("comment"),
                provider_post_id: Some(&target.video_id),
                parent_provider_message_id: comment.parent_id.as_deref(),
                visibility: Some("public"),
                channel: store::inbox::channel::YOUTUBE,
                provider_account: &target.account_id,
                provider_message_id: &comment.id,
                provider_thread_ref: Some(&target.video_id),
                external_address: &comment.author_channel_id,
                external_name: comment.author_name.as_deref(),
                subject: None,
                body: &comment.text,
                source_page: Some(&target.video_id),
                source_channel: Some("youtube"),
                matter_consent: false,
                promotional_opt_in: false,
                email_conversation_id: None,
                attachments: Vec::new(),
                project_id: None,
                occurred_at: &occurred.to_rfc3339(),
                raw_storage_key: None,
            },
        )
        .await
        .map_err(anyhow::Error::from)
    }
}

/// Queue the alert and triage workflows for a persisted Message. Both use the Message ID for deduplication.
async fn start_inbound_hooks(message_id: Uuid) -> anyhow::Result<()> {
    let (ingress, token) = workflows::trigger::dbos_ingress_from_env()?;
    workflows::trigger::start_dbos_inbound_message_alert(
        &ingress,
        &token,
        &serde_json::json!({
            "workflow_id": workflows::trigger::inbound_message_alert_workflow_id(message_id),
            "request": workflows::InboundMessageAlertRequest { message_id },
        }),
    )
    .await?;
    workflows::trigger::start_inbound_triage(message_id)
        .await
        .map_err(anyhow::Error::msg)?;
    Ok(())
}

/// One comment flattened from a thread's top-level comment or its replies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedComment {
    pub id: String,
    /// The thread's top-level comment id for a reply, `None` for a top-level
    /// comment.
    pub parent_id: Option<String>,
    pub text: String,
    pub author_channel_id: String,
    pub author_name: Option<String>,
    pub published_at: String,
}

#[derive(Deserialize)]
struct CommentThreads {
    #[serde(rename = "nextPageToken", default)]
    next_page_token: Option<String>,
    #[serde(default)]
    items: Vec<CommentThread>,
}

#[derive(Deserialize)]
struct CommentThread {
    #[serde(default)]
    replies: Option<CommentReplies>,
    snippet: ThreadSnippet,
}

#[derive(Deserialize)]
struct ThreadSnippet {
    #[serde(rename = "totalReplyCount", default)]
    total_reply_count: usize,
    #[serde(rename = "topLevelComment")]
    top_level_comment: CommentResource,
    #[serde(default)]
    replies: Option<CommentReplies>,
}

#[derive(Deserialize)]
struct CommentReplies {
    #[serde(default)]
    comments: Vec<CommentResource>,
}

#[derive(Deserialize)]
struct CommentPage {
    #[serde(default)]
    items: Vec<CommentResource>,
    #[serde(rename = "nextPageToken", default)]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct CommentResource {
    id: String,
    snippet: CommentSnippet,
}

#[derive(Deserialize)]
struct CommentSnippet {
    #[serde(rename = "textDisplay", default)]
    text_display: String,
    #[serde(rename = "textOriginal", default)]
    text_original: String,
    #[serde(rename = "authorDisplayName", default)]
    author_display_name: Option<String>,
    #[serde(rename = "authorChannelId", default)]
    author_channel_id: Option<AuthorChannelId>,
    #[serde(rename = "publishedAt", default)]
    published_at: String,
}

#[derive(Deserialize)]
struct AuthorChannelId {
    #[serde(default)]
    value: String,
}

/// Flatten a `commentThreads` response: top-level comments first, then each
/// thread's replies under their top-level parent.
#[cfg(test)]
#[must_use]
fn parse_threads(threads: &CommentThreads) -> Vec<ParsedComment> {
    let mut parsed = Vec::new();
    for thread in &threads.items {
        parsed.push(parse_comment(&thread.snippet.top_level_comment, None));
        if let Some(replies) = thread.replies.as_ref().or(thread.snippet.replies.as_ref()) {
            for reply in &replies.comments {
                parsed.push(parse_comment(
                    reply,
                    Some(thread.snippet.top_level_comment.id.clone()),
                ));
            }
        }
    }
    parsed
}

fn parse_comment(comment: &CommentResource, parent_id: Option<String>) -> ParsedComment {
    ParsedComment {
        id: comment.id.clone(),
        parent_id,
        text: if comment.snippet.text_original.is_empty() {
            comment.snippet.text_display.clone()
        } else {
            comment.snippet.text_original.clone()
        },
        author_channel_id: comment
            .snippet
            .author_channel_id
            .as_ref()
            .map(|channel| channel.value.clone())
            .unwrap_or_default(),
        author_name: comment.snippet.author_display_name.clone(),
        published_at: comment.snippet.published_at.clone(),
    }
}

/// `YouTube` returns RFC 3339 instants; an unparseable one drops the comment
/// rather than inventing a receipt time.
fn occurred_at(published_at: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(published_at)
        .ok()
        .map(DateTime::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn threads_body() -> serde_json::Value {
        serde_json::json!({
            "kind": "youtube#commentThreadListResponse",
            "items": [
                {"id": "t1", "snippet": {"topLevelComment": {"id": "c1", "snippet": {
                    "textDisplay": "A question", "textOriginal": "A question",
                    "authorDisplayName": "Synthetic Viewer",
                    "authorChannelId": {"value": "UC-viewer"},
                    "publishedAt": "2026-10-30T12:00:00Z"}},
                 "replies": {"comments": [
                    {"id": "r1", "snippet": {
                        "textDisplay": "From another viewer",
                        "authorDisplayName": "Other Viewer",
                        "authorChannelId": {"value": "UC-other"},
                        "publishedAt": "2026-10-30T13:00:00Z"}}]}}},
                {"id": "t2", "snippet": {"topLevelComment": {"id": "c2", "snippet": {
                    "textDisplay": "Our own comment",
                    "authorChannelId": {"value": "synthetic-channel"},
                    "publishedAt": "2026-10-30T12:30:00Z"}}}},
                {"id": "t3", "snippet": {"topLevelComment": {"id": "c3", "snippet": {
                    "textDisplay": "A broken timestamp",
                    "authorChannelId": {"value": "UC-late"},
                    "publishedAt": "not-a-time"}}}}
            ]
        })
    }

    fn parse_fixture() -> Vec<ParsedComment> {
        parse_threads(&serde_json::from_value(threads_body()).unwrap())
    }

    #[test]
    fn parse_threads_flattens_top_level_and_replies() {
        let parsed = parse_fixture();
        assert_eq!(parsed.len(), 4);
        assert_eq!(parsed[0].id, "c1");
        assert_eq!(parsed[0].parent_id, None);
        assert_eq!(parsed[1].id, "r1");
        assert_eq!(parsed[1].parent_id.as_deref(), Some("c1"));
        assert_eq!(parsed[0].author_channel_id, "UC-viewer");
        assert_eq!(parsed[0].author_name.as_deref(), Some("Synthetic Viewer"));
        assert_eq!(
            occurred_at(&parsed[0].published_at),
            occurred_at("2026-10-30T12:00:00Z")
        );
    }

    #[test]
    fn fallback_uses_display_text_when_original_is_absent() {
        let body = serde_json::json!({
            "items": [{"snippet": {"topLevelComment": {"id": "c9", "snippet": {
                "textDisplay": "display only",
                "authorChannelId": {"value": "UC-x"},
                "publishedAt": "2026-10-30T12:00:00Z"}}}}]
        });
        let parsed = parse_threads(&serde_json::from_value(body).unwrap());
        assert_eq!(parsed[0].text, "display only");
    }
}

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
//! The Restate workflow journals one step per video, so a provider outage on
//! one video never re-reads another. Like the social publisher, the workflow
//! body calls the same plain methods a test calls directly, and only a
//! `production` deployment reaches the provider.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use restate_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use store::config::SocialEgressGate;
use store::social_accounts::SocialProvider;
use store::surreal::SurrealDb;
use uuid::Uuid;

use workflows::social_publish::{self, TokenRefreshConfig};

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
    surreal: SurrealDb,
    kms: Arc<dyn cloud::RuntimeKms>,
    http: reqwest::Client,
    base: String,
    egress: SocialEgressGate,
    refresh: TokenRefreshConfig,
}

impl YoutubeCommentsPollerService {
    #[must_use]
    pub fn new(surreal: SurrealDb, kms: Arc<dyn cloud::RuntimeKms>) -> Self {
        Self {
            surreal,
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
        let publications =
            store::social_publications::confirmed_links(&self.surreal, "youtube", VIDEO_LIMIT)
                .await?;
        let mut targets = Vec::new();
        for publication in publications {
            let Some(video_id) = publication.provider_post_id.as_deref() else {
                continue;
            };
            let Some(post) = store::posts::find_by_id(&self.surreal, publication.post_id).await?
            else {
                continue;
            };
            let accounts = store::social_accounts::selected_for_brand(
                &self.surreal,
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
            &self.surreal,
            self.kms.as_ref(),
            &self.http,
            &self.refresh,
            &target.brand_key,
            SocialProvider::Youtube,
            &target.account_id,
        )
        .await?;
        let response = self
            .http
            .get(format!("{}/commentThreads", self.base))
            .query(&[
                ("part", "snippet,replies"),
                ("videoId", target.video_id.as_str()),
                ("maxResults", PAGE_LIMIT),
                ("order", "time"),
            ])
            .bearer_auth(credential.access_token())
            .send()
            .await?;
        let status = response.status();
        let bytes = response.bytes().await?;
        if !status.is_success() {
            anyhow::bail!("commentThreads read returned {status}");
        }
        let threads: CommentThreads = serde_json::from_slice(&bytes)?;
        let mut outcome = VideoOutcome::default();
        for comment in parse_threads(&threads) {
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

    async fn ingest_comment(
        &self,
        target: &PollTarget,
        comment: &ParsedComment,
        occurred: DateTime<Utc>,
    ) -> anyhow::Result<store::inbox::IngestedInbound> {
        store::inbox::ingest_inbound(
            &self.surreal,
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
                attachment_asset_id: None,
                occurred_at: &occurred.to_rfc3339(),
            },
        )
        .await
        .map_err(anyhow::Error::from)
    }
}

/// Start the same alert and triage workflows every inbound channel starts
/// after a fresh ingest. Both are keyed by Message ID, so a repeated start
/// attaches to the same durable workflow instead of duplicating work. A
/// deployment without a broker configured is a no-op, exactly as in the
/// portal's ingest paths.
///
/// # Errors
///
/// A failed durable submission — the caller's journaled step retries it while
/// ingest itself stays untouched.
async fn start_inbound_hooks(message_id: Uuid) -> anyhow::Result<()> {
    let Some(ingress) = std::env::var("RESTATE_BROKER_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(());
    };
    let auth = std::env::var("RESTATE_AUTH_TOKEN").ok();
    workflows::start_workflow(
        &ingress,
        auth.as_deref(),
        "InboundMessageAlert",
        &message_id.to_string(),
        "run",
        &workflows::InboundMessageAlertRequest { message_id },
        true,
    )
    .await?;
    workflows::start_workflow(
        &ingress,
        auth.as_deref(),
        "InboundTriage",
        &message_id.to_string(),
        "run",
        &workflows::InboundTriageRequest { message_id },
        true,
    )
    .await?;
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
    #[serde(default)]
    items: Vec<CommentThread>,
}

#[derive(Deserialize)]
struct CommentThread {
    snippet: ThreadSnippet,
}

#[derive(Deserialize)]
struct ThreadSnippet {
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
#[must_use]
fn parse_threads(threads: &CommentThreads) -> Vec<ParsedComment> {
    let mut parsed = Vec::new();
    for thread in &threads.items {
        parsed.push(parse_comment(&thread.snippet.top_level_comment, None));
        if let Some(replies) = thread.snippet.replies.as_ref() {
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

#[restate_sdk::workflow(name = "YoutubeCommentsPoller")]
impl YoutubeCommentsPollerService {
    /// One polling tick: resolve targets once, then journal one step per
    /// video. A video whose poll fails counts as failed instead of failing
    /// the tick, so one deleted video can never wedge the schedule; the
    /// next tick re-reads it.
    #[restate_sdk::handler]
    async fn run(
        &self,
        ctx: WorkflowContext<'_>,
        Json(_request): Json<PollRequest>,
    ) -> Result<Json<PollReport>, HandlerError> {
        let service = self.clone();
        let targets = ctx
            .run(move || async move {
                service
                    .poll_targets()
                    .await
                    .map(Json)
                    .map_err(|error| HandlerError::from(error.to_string()))
            })
            .name("load-youtube-poll-targets")
            .await?
            .into_inner();
        let mut report = PollReport {
            videos: targets.len(),
            ..PollReport::default()
        };
        for (index, target) in targets.into_iter().enumerate() {
            let service = self.clone();
            match ctx
                .run(move || async move {
                    service
                        .poll_video(&target)
                        .await
                        .map(Json)
                        .map_err(|error| HandlerError::from(error.to_string()))
                })
                .name(format!("poll-youtube-comments-{index}"))
                .await
            {
                Ok(outcome) => {
                    let outcome: VideoOutcome = outcome.into_inner();
                    report.ingested += outcome.ingested;
                    report.deduped += outcome.deduped;
                    report.skipped += outcome.skipped;
                }
                Err(error) => {
                    tracing::warn!(video_index = index, %error, "YouTube comment poll failed");
                    report.failed += 1;
                }
            }
        }
        Ok(Json(report))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use store::posts::{AccountAlias, SourceCaptions, SourcePost};
    use store::social_accounts::ConnectSocialAccount;
    use store::test_support::mem_surreal;
    use store::{
        brands::NewBrand, firms::NewFirm, persons::NewPerson, persons::Role, posts,
        social_publications,
    };
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const VIDEO_ID: &str = "abc123defg";
    const ACCOUNT_ID: &str = "synthetic-channel";

    fn threads_body() -> serde_json::Value {
        serde_json::json!({
            "kind": "youtube#commentThreadListResponse",
            "items": [
                {"id": "t1", "snippet": {"topLevelComment": {"id": "c1", "snippet": {
                    "textDisplay": "A question", "textOriginal": "A question",
                    "authorDisplayName": "Synthetic Viewer",
                    "authorChannelId": {"value": "UC-viewer"},
                    "publishedAt": "2026-10-30T12:00:00Z"},
                 "replies": {"comments": [
                    {"id": "r1", "snippet": {
                        "textDisplay": "From another viewer",
                        "authorDisplayName": "Other Viewer",
                        "authorChannelId": {"value": "UC-other"},
                        "publishedAt": "2026-10-30T13:00:00Z"}}]}}}},
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

    async fn seed_target(db: &SurrealDb) {
        let admin = store::persons::create(
            db,
            &NewPerson::with_role("Synthetic Admin", "youtube-test@example.com", Role::Admin),
        )
        .await
        .unwrap();
        let firm = store::firms::create(
            db,
            &NewFirm {
                name: "Synthetic Firm".into(),
                status: "active".into(),
                entity_id: store::test_support::seed_entity(db).await,
                admin_dri_person_id: admin.id,
            },
        )
        .await
        .unwrap();
        store::brands::create(
            db,
            Role::Admin,
            Some(admin.id),
            &NewBrand {
                name: "Neon Law".into(),
                key: "synthetic-brand".into(),
                firm_id: Some(firm.id),
                primary_color: None,
                accent_color: None,
                typeface: None,
            },
        )
        .await
        .unwrap();
        let scopes = SocialProvider::Youtube
            .required_scopes()
            .iter()
            .map(|scope| (*scope).into())
            .collect::<Vec<String>>();
        store::social_accounts::connect(
            db,
            ConnectSocialAccount {
                actor_role: Role::Admin,
                actor_person_id: admin.id,
                brand_key: "synthetic-brand",
                provider: SocialProvider::Youtube,
                provider_account_id: ACCOUNT_ID,
                display_name: None,
                scopes: &scopes,
                expires_at: None,
                access_token: "synthetic-token",
                refresh_token: None,
            },
            std::sync::Arc::new(cloud::FakeKms::new("youtube-test")).as_ref(),
        )
        .await
        .unwrap();
        posts::seed_post(
            db,
            &SourcePost {
                brand_key: "synthetic-brand",
                slug: "youtube-comments-test",
                title: "A published Post",
                description: "A test Post.",
                body_html: "<p>A test Post.</p>",
                body_markdown: None,
                status: "published",
                publish_at: Utc.timestamp_opt(1_781_388_000, 0).unwrap(),
                video_asset: None,
                vertical_video: None,
                tiktok_ai_generated: None,
                captions: SourceCaptions::default(),
                accounts: &[AccountAlias {
                    provider: "youtube".into(),
                    provider_account_id: ACCOUNT_ID.into(),
                }],
                social_links: &[],
            },
            Utc::now(),
        )
        .await
        .unwrap();
        let post = posts::find_by_brand_slug(db, "synthetic-brand", "youtube-comments-test")
            .await
            .unwrap()
            .unwrap();
        let post_id = post.id;
        assert!(matches!(
            social_publications::admit(db, post_id, "youtube")
                .await
                .unwrap(),
            social_publications::Admission::Admitted(_)
        ));
        social_publications::mark_confirmed(db, post_id, "youtube", VIDEO_ID, None)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn polling_is_idempotent_and_owner_echo_is_ignored() {
        let db = mem_surreal().await;
        seed_target(&db).await;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/commentThreads"))
            .respond_with(ResponseTemplate::new(200).set_body_json(threads_body()))
            .mount(&server)
            .await;
        let service = YoutubeCommentsPollerService::new(
            db.clone(),
            std::sync::Arc::new(cloud::FakeKms::new("youtube-test")),
        )
        .with_base(server.uri())
        .with_egress(SocialEgressGate::new(|| Ok(())));
        let targets = service.poll_targets().await.unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].video_id, VIDEO_ID);
        assert_eq!(targets[0].account_id, ACCOUNT_ID);

        let first = service.poll_video(&targets[0]).await.unwrap();
        assert_eq!(first.ingested, 2, "top-level comment plus its reply");
        assert_eq!(first.deduped, 0);
        assert_eq!(
            first.skipped, 2,
            "the Brand's own echo and the broken timestamp"
        );

        let second = service.poll_video(&targets[0]).await.unwrap();
        assert_eq!(
            second.deduped, 2,
            "the same comments are recognized, never re-filed"
        );
        assert_eq!(second.ingested, 0);

        let mut response = db
            .query("SELECT * FROM inbox_conversation")
            .await
            .unwrap()
            .check()
            .unwrap();
        let conversations: Vec<serde_json::Value> = response.take(0).unwrap();
        assert_eq!(
            conversations.len(),
            1,
            "threading keeps a video's comments together"
        );
        assert_eq!(conversations[0]["channel"], "youtube");
        assert_eq!(conversations[0]["brand_key"], "synthetic-brand");
        assert_eq!(conversations[0]["provider_account"], ACCOUNT_ID);
        assert_eq!(conversations[0]["provider_post_id"], VIDEO_ID);
        assert_eq!(conversations[0]["external_address"], "UC-viewer");

        let mut response = db
            .query("SELECT * FROM message ORDER BY occurred_at")
            .await
            .unwrap()
            .check()
            .unwrap();
        let messages: Vec<serde_json::Value> = response.take(0).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["body"], "A question");
        assert_eq!(messages[1]["body"], "From another viewer");
    }
}
